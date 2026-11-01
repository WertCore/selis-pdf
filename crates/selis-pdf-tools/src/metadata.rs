//! Read-only metadata report: what a document says about itself, as JSON.
//!
//! Nothing here rewrites the input. The operation opens the newest revision,
//! reads the `/Info` dictionary and the catalog's XMP `/Metadata` packet, and
//! pairs them with the structural counts the verification gate already walks.
//! A reader can therefore see the authoring trails the `strip_metadata`
//! operation would remove before deciding whether to remove them.
//!
//! An encrypted document reports `status: "encrypted"` and nothing else: with
//! no file key in this chunk every dictionary it would read is still
//! ciphertext, and printing ciphertext as a title would be a lie.

use serde::Serialize;

use selis_bytes::Bytes;
use selis_pdf_cos::reconcile as rec;
use selis_pdf_cos::verify::survey;
use selis_pdf_cos::{Obj, Ref};
use selis_sandbox::Budget;

use crate::{read_file, CliError, CliResult, ToolResult};

/// The report of one document.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct MetadataReport {
    /// `ok` when the document was readable, `encrypted` when it was not.
    pub status: String,
    /// The header version, e.g. `1.7`.
    pub pdf_version: String,
    /// Whether the newest revision declares an `/Encrypt` dictionary.
    pub encrypted: bool,
    /// Input size in bytes.
    pub bytes_in: u64,
    /// How many incremental revisions the file carries.
    pub revisions: u64,
    /// Leaf page count (`None` for an encrypted document).
    pub pages: Option<u64>,
    /// The first page's `/MediaBox` in points.
    pub page_size: Option<PageSize>,
    /// The `/Info` dictionary, one entry per key.
    pub info: Vec<InfoField>,
    /// The catalog's XMP packet.
    pub xmp: XmpPacket,
    /// The structural counts.
    pub counts: Counts,
}

/// One page, in points.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct PageSize {
    /// Box width.
    pub width: f64,
    /// Box height.
    pub height: f64,
}

/// One `/Info` key with its value rendered as text.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct InfoField {
    /// The dictionary key without its leading slash.
    pub key: String,
    /// The value as text: a decoded string, a name, or the object kind for
    /// anything a reader cannot show as words.
    pub value: String,
}

/// The XMP `/Metadata` stream.
#[derive(Debug, Clone, PartialEq, Default, Serialize)]
pub struct XmpPacket {
    /// Whether the catalog points at a metadata stream at all.
    pub present: bool,
    /// Whether its bytes were decoded into text here.
    pub decoded: bool,
    /// The stream length as the file holds it.
    pub bytes: u64,
    /// The packet text, capped at [`MAX_XMP_TEXT`].
    pub text: Option<String>,
    /// Whether `text` was cut short by the cap.
    pub truncated: bool,
    /// The stream's `/Filter` name when the packet could not be decoded.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub filter: Option<String>,
}

/// The structural counts a reader cares about alongside the metadata.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize)]
pub struct Counts {
    /// Total `/Annots` items across the pages.
    pub annotations: u64,
    /// Form fields under `/AcroForm`.
    pub form_fields: u64,
    /// Optional-content groups.
    pub layers: u64,
    /// Reachable outline entries.
    pub outline_entries: u64,
    /// Names in the `/EmbeddedFiles` name tree.
    pub embedded_files: u64,
}

/// The XMP text cap. A packet is normally a few KiB; the limit keeps a hostile
/// stream from turning a report into a memory spike.
const MAX_XMP_TEXT: usize = 65_536;

/// Read the document's metadata and structure without touching its bytes.
///
/// # Errors
///
/// A typed [`CliError`] when the file cannot be read or parsed, or when the
/// report itself cannot be serialized.
pub fn metadata_report(path: &str) -> ToolResult<MetadataReport> {
    let src = read_file(path)?;
    let budget = Budget::unlimited();
    let clock = crate::shell_clock();
    let mut g = crate::runtime::cli_guard(&budget, &clock);
    let startxref = selis_pdf_cos::xref::find_startxref(&src, 4096).unwrap_or(0);
    let doc = selis_pdf_cos::parse_revisions(&src, startxref, &budget, &mut g)
        .map_err(|e| CliError(format!("{path}: cannot open: {e}")))?;
    let revision = doc
        .revisions()
        .last()
        .ok_or_else(|| CliError(format!("{path}: no revisions")))?;

    let report = MetadataReport {
        status: "ok".to_string(),
        pdf_version: header_version(&src),
        encrypted: revision.encrypt.is_some(),
        bytes_in: u64::try_from(src.len()).unwrap_or(u64::MAX),
        revisions: u64::try_from(doc.revisions().len()).unwrap_or(u64::MAX),
        pages: None,
        page_size: None,
        info: Vec::new(),
        xmp: XmpPacket::default(),
        counts: Counts::default(),
    };
    if report.encrypted {
        // Objects are still ciphertext without the file key, so the honest
        // report is the encryption itself and nothing invented on top of it.
        return Ok(MetadataReport {
            status: "encrypted".to_string(),
            ..report
        });
    }

    let observed = survey(&src, &budget, &mut g)
        .map_err(|e| CliError(format!("{path}: cannot survey: {e}")))?;
    let page_refs = crate::tools::page_refs_of(&src, &doc, &budget, &mut g)
        .map_err(|e| CliError(format!("{path}: {e}")))?;
    let page_size = page_refs
        .first()
        .and_then(|page_ref| media_box(&src, &doc, *page_ref, &budget, &mut g).ok());

    Ok(MetadataReport {
        pages: Some(observed.pages),
        page_size,
        info: info_fields(&src, &doc, &budget, &mut g)?,
        xmp: xmp_packet(&src, &doc, &budget, &mut g)?,
        counts: Counts {
            annotations: observed.annotations,
            form_fields: observed.fields,
            layers: observed.ocgs,
            outline_entries: observed.outlines,
            embedded_files: observed.embedded_files,
        },
        ..report
    })
}

impl MetadataReport {
    /// The report as indented JSON: the one export shape, owned here so a
    /// reader sees the same fields whether a CLI or the browser asked.
    ///
    /// # Errors
    ///
    /// [`CliError`] when serialization fails.
    pub fn to_json(&self) -> ToolResult<Vec<u8>> {
        serde_json::to_vec_pretty(self)
            .map_err(|e| CliError(format!("cannot encode the metadata report: {e}")))
    }
}

/// The header version from the `%PDF-x.y` marker, empty when the file has none.
fn header_version(src: &[u8]) -> String {
    let Some(rest) = src.strip_prefix(b"%PDF-".as_slice()) else {
        return String::new();
    };
    let digits: Vec<u8> = rest
        .iter()
        .copied()
        .take_while(|byte| byte.is_ascii_digit() || *byte == b'.')
        .collect();
    String::from_utf8_lossy(&digits).into_owned()
}

/// The `/Info` dictionary: the newest revision's trailer entry, falling back
/// to a catalog `/Info` the way the writer's carry check does.
fn info_fields(
    src: &[u8],
    doc: &selis_pdf_cos::Doc,
    budget: &Budget,
    g: &mut selis_sandbox::BudgetGuard<'_>,
) -> CliResult<Vec<InfoField>> {
    use selis_pdf_cos::copy::resolve_ref;

    let mut pairs: Option<Vec<(Bytes, Obj)>> = None;
    for view in doc.revisions().iter().rev() {
        if let Some((_, Obj::Ref(info_ref))) = view
            .trailer
            .iter()
            .find(|(key, _)| key.as_slice() == b"Info")
        {
            if let Ok(Obj::Dict(entries)) = resolve_ref(src, doc, *info_ref, budget, g) {
                pairs = Some(entries);
                break;
            }
        }
    }
    if pairs.is_none() {
        if let Some(Obj::Ref(info_ref)) = rec::catalog_entry(src, doc, b"Info", budget, g)
            .map_err(|e| CliError(format!("{e}")))?
        {
            if let Ok(Obj::Dict(entries)) = resolve_ref(src, doc, info_ref, budget, g) {
                pairs = Some(entries);
            }
        }
    }
    let Some(entries) = pairs else {
        return Ok(Vec::new());
    };

    let mut fields: Vec<InfoField> = entries
        .iter()
        .map(|(key, value)| InfoField {
            key: String::from_utf8_lossy(key.as_slice()).into_owned(),
            value: field_text(value),
        })
        .collect();
    fields.sort_by(|left, right| left.key.cmp(&right.key));
    Ok(fields)
}

/// A PDF real as decimal text. The parser keeps a real exact as a scaled
/// integer, so the digits are placed rather than the value being converted.
/// The sign is taken off before the point is placed, so `-0.05` stays `-0.05`
/// instead of `0.-5`.
fn real_text(scaled: i64, scale: u8) -> String {
    let digits = usize::from(scale);
    let sign = if scaled < 0 { "-" } else { "" };
    let text = scaled.unsigned_abs().to_string();
    if digits == 0 {
        return format!("{sign}{text}");
    }
    let padded = if text.len() <= digits {
        format!("0{}{text}", "0".repeat(digits.saturating_sub(text.len())))
    } else {
        text
    };
    let (head, tail) = padded.split_at(padded.len().saturating_sub(digits));
    format!("{sign}{head}.{tail}")
}

/// One dictionary value as text.
fn field_text(value: &Obj) -> String {
    match value {
        Obj::String(bytes) | Obj::HexString(bytes) => crate::attachments::pdf_text(bytes),
        Obj::Name(bytes) => String::from_utf8_lossy(bytes.as_slice()).into_owned(),
        Obj::Int(number) => number.to_string(),
        Obj::Bool(flag) => flag.to_string(),
        Obj::Real { scaled, scale } => real_text(*scaled, *scale),
        Obj::Null => String::new(),
        other => other.type_name().to_string(),
    }
}

/// The first page's `/MediaBox` width and height.
fn media_box(
    src: &[u8],
    doc: &selis_pdf_cos::Doc,
    page_ref: Ref,
    budget: &Budget,
    g: &mut selis_sandbox::BudgetGuard<'_>,
) -> CliResult<PageSize> {
    use selis_pdf_cos::copy::resolve_ref;

    let Obj::Dict(entries) =
        resolve_ref(src, doc, page_ref, budget, g).map_err(|e| CliError(format!("{e}")))?
    else {
        return Err(CliError("page is not a dictionary".to_string()));
    };
    let Some((_, Obj::Array(values))) = entries
        .iter()
        .find(|(key, _)| key.as_slice() == b"MediaBox")
    else {
        return Err(CliError("page has no /MediaBox".to_string()));
    };
    let number = |value: &Obj| -> CliResult<f64> {
        let text = match value {
            Obj::Int(scale) => scale.to_string(),
            Obj::Real { scaled, scale } => real_text(*scaled, *scale),
            _ => return Err(CliError("MediaBox holds a non-number".to_string())),
        };
        text.parse::<f64>()
            .map_err(|_| CliError("MediaBox number is unreadable".to_string()))
    };
    let [_, _, high_x, high_y] = values.as_slice() else {
        return Err(CliError("/MediaBox is not four numbers".to_string()));
    };
    Ok(PageSize {
        width: number(high_x)?,
        height: number(high_y)?,
    })
}

/// Read and decode the catalog's XMP packet.
fn xmp_packet(
    src: &[u8],
    doc: &selis_pdf_cos::Doc,
    budget: &Budget,
    g: &mut selis_sandbox::BudgetGuard<'_>,
) -> CliResult<XmpPacket> {
    use selis_pdf_cos::copy::resolve_ref;

    let Some(Obj::Ref(metadata_ref)) = rec::catalog_entry(src, doc, b"Metadata", budget, g)
        .map_err(|e| CliError(format!("{e}")))?
    else {
        return Ok(XmpPacket::default());
    };
    let Obj::Stream { dict, data } =
        resolve_ref(src, doc, metadata_ref, budget, g).map_err(|e| CliError(format!("{e}")))?
    else {
        return Ok(XmpPacket {
            present: true,
            ..XmpPacket::default()
        });
    };
    let held = u64::try_from(data.len()).unwrap_or(u64::MAX);
    let filter = dict
        .iter()
        .find(|(key, _)| key.as_slice() == b"Filter")
        .map(|(_, value)| field_text(value));

    // XMP is conventionally stored uncompressed. A Flate stream is decoded
    // here; any other filter is reported by name rather than guessed at.
    let cap = u64::try_from(MAX_XMP_TEXT).unwrap_or(u64::MAX);
    let kept = |bytes: &[u8]| bytes.get(..MAX_XMP_TEXT).unwrap_or(bytes).to_vec();
    let decoded = match filter.as_deref() {
        None => Ok(kept(data.as_slice())),
        Some("FlateDecode") | Some("Fl") => {
            selis_pdf_filter::flate_decode_bounded(data.as_slice(), cap, g)
                .map_err(|_| CliError("the XMP stream could not be decoded".to_string()))
        }
        Some(other) => Err(CliError(format!("unsupported XMP filter: {other}"))),
    };
    let Ok(body) = decoded else {
        return Ok(XmpPacket {
            present: true,
            decoded: false,
            bytes: held,
            text: None,
            truncated: false,
            filter,
        });
    };

    let text = String::from_utf8_lossy(body.as_slice()).into_owned();
    // Either path can stop at the cap: the stored packet may be longer than
    // what was kept, and an inflate that reached the limit is cut by definition.
    let truncated = held > cap || u64::try_from(body.len()).unwrap_or(u64::MAX) >= cap;
    let shown = if truncated {
        // Cut on a character boundary, never through the middle of one.
        let mut end = MAX_XMP_TEXT;
        while end > 0 && !text.is_char_boundary(end) {
            end = end.saturating_sub(1);
        }
        text.get(..end).unwrap_or("").to_string()
    } else {
        text
    };
    Ok(XmpPacket {
        present: true,
        decoded: true,
        bytes: held,
        text: Some(shown),
        truncated,
        filter,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{insert_input, MemorySession};

    const XMP: &[u8] = b"<?xpacket begin=\"\"?><x:xmpmeta xmlns:x=\"adobe:ns:meta/\">\
                         <rdf:RDF><rdf:Description/></rdf:RDF></x:xmpmeta><?xpacket end=\"w\"?>";

    fn stream_object(number: usize, dict: &str, data: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(format!("{number} 0 obj\n{dict}\nstream\n").as_bytes());
        out.extend_from_slice(data);
        out.extend_from_slice(b"\nendstream\nendobj\n");
        out
    }

    fn build_pdf(objects: Vec<Vec<u8>>) -> Vec<u8> {
        let mut out = b"%PDF-1.4\n".to_vec();
        let mut offsets = Vec::with_capacity(objects.len());
        for object in objects {
            offsets.push(out.len());
            out.extend_from_slice(&object);
        }
        let xref = out.len();
        let size = offsets.len().saturating_add(1);
        out.extend_from_slice(format!("xref\n0 {size}\n").as_bytes());
        out.extend_from_slice(b"0000000000 65535 f \n");
        for offset in offsets {
            out.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
        }
        out.extend_from_slice(
            format!("trailer\n<< /Size {size} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n")
                .as_bytes(),
        );
        out
    }

    /// A one-page document with an `/Info` dictionary and, when `packet` is
    /// given, a catalog `/Metadata` stream holding that packet.
    fn document(packet: Option<(&str, &[u8])>) -> Vec<u8> {
        let catalog = match packet {
            Some(_) => {
                b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R /Info 4 0 R /Metadata 5 0 R >>\nendobj\n"
                    .to_vec()
            }
            None => b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R /Info 4 0 R >>\nendobj\n".to_vec(),
        };
        let mut objects = vec![
            catalog,
            b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n".to_vec(),
            b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 100] >>\nendobj\n".to_vec(),
            b"4 0 obj\n<< /Title (A fixture) /Author (Selis) /ModDate [1 2] >>\nendobj\n".to_vec(),
        ];
        if let Some((dict, body)) = packet {
            objects.push(stream_object(5, dict, body));
        }
        build_pdf(objects)
    }

    /// A stream dictionary with the right `/Length` for `body`.
    fn xml_dict(body: &[u8]) -> String {
        format!("<< /Subtype /XML /Length {} >>", body.len())
    }

    #[test]
    fn reads_the_info_dictionary_and_the_xmp_packet() {
        let _session = MemorySession::activate();
        let dict = xml_dict(XMP);
        insert_input(
            "metadata-report/plain/in.pdf",
            document(Some((dict.as_str(), XMP))),
        );

        let report = metadata_report("metadata-report/plain/in.pdf").expect("report");
        assert_eq!(report.status, "ok");
        assert_eq!(report.pdf_version, "1.4");
        assert!(!report.encrypted);
        assert_eq!(report.revisions, 1);
        assert_eq!(report.pages, Some(1));
        assert_eq!(
            report.page_size,
            Some(PageSize {
                width: 200.0,
                height: 100.0
            })
        );
        assert_eq!(
            report.info,
            vec![
                InfoField {
                    key: "Author".to_string(),
                    value: "Selis".to_string()
                },
                InfoField {
                    key: "ModDate".to_string(),
                    value: "array".to_string()
                },
                InfoField {
                    key: "Title".to_string(),
                    value: "A fixture".to_string()
                },
            ]
        );
        assert!(report.xmp.present);
        assert!(report.xmp.decoded);
        assert!(!report.xmp.truncated);
        assert!(report.xmp.text.is_some());
        assert_eq!(report.counts, Counts::default());

        let json = String::from_utf8(report.to_json().expect("json")).expect("UTF-8");
        assert!(
            json.contains("xmpmeta"),
            "the packet is in the export: {json}"
        );
    }

    #[test]
    fn a_document_without_metadata_reports_nothing_invented() {
        let _session = MemorySession::activate();
        insert_input("metadata-report/empty/in.pdf", document(None));

        let report = metadata_report("metadata-report/empty/in.pdf").expect("report");
        assert_eq!(report.xmp, XmpPacket::default());
        assert_eq!(report.info.len(), 3);
        assert_eq!(report.pages, Some(1));
    }

    #[test]
    fn a_flate_packet_is_decoded() {
        let _session = MemorySession::activate();
        let packed = selis_pdf_filter::flate_encode(XMP, 6);
        let dict = format!(
            "<< /Subtype /XML /Filter /FlateDecode /Length {} >>",
            packed.len()
        );
        insert_input(
            "metadata-report/flate/in.pdf",
            document(Some((dict.as_str(), packed.as_slice()))),
        );

        let report = metadata_report("metadata-report/flate/in.pdf").expect("report");
        assert!(report.xmp.decoded, "the Flate packet is read here");
        assert!(report.xmp.text.unwrap_or_default().contains("x:xmpmeta"));
    }

    #[test]
    fn an_undecodable_filter_is_named_rather_than_guessed() {
        let _session = MemorySession::activate();
        let dict = format!(
            "<< /Subtype /XML /Filter /DCTDecode /Length {} >>",
            XMP.len()
        );
        insert_input(
            "metadata-report/filter/in.pdf",
            document(Some((dict.as_str(), XMP))),
        );

        let report = metadata_report("metadata-report/filter/in.pdf").expect("report");
        assert!(report.xmp.present);
        assert!(!report.xmp.decoded);
        assert_eq!(report.xmp.text, None);
        assert_eq!(report.xmp.filter.as_deref(), Some("DCTDecode"));
        assert_eq!(report.xmp.bytes, u64::try_from(XMP.len()).unwrap_or(0));
    }

    #[test]
    fn an_oversized_packet_is_capped_and_marked() {
        let _session = MemorySession::activate();
        let mut body = Vec::with_capacity(MAX_XMP_TEXT.saturating_add(4096));
        body.extend_from_slice(b"<x:xmpmeta>");
        while body.len() < MAX_XMP_TEXT.saturating_add(4096) {
            body.extend_from_slice(b"padding padding padding\n");
        }
        let dict = xml_dict(body.as_slice());
        insert_input(
            "metadata-report/cap/in.pdf",
            document(Some((dict.as_str(), body.as_slice()))),
        );

        let report = metadata_report("metadata-report/cap/in.pdf").expect("report");
        assert!(report.xmp.truncated, "the cap is reported, not hidden");
        let text = report.xmp.text.expect("text is still shown");
        assert_eq!(text.len(), MAX_XMP_TEXT);
    }

    #[test]
    fn an_encrypted_document_reports_only_that_it_is_encrypted() {
        let _session = MemorySession::activate();
        let dict = xml_dict(XMP);
        let mut bytes = document(Some((dict.as_str(), XMP)));
        let trailer_at = bytes
            .windows(b"trailer".len())
            .position(|window| window == b"trailer")
            .expect("trailer");
        let insert_at = trailer_at + b"trailer\n<<".len();
        bytes.splice(insert_at..insert_at, b" /Encrypt 99 0 R".iter().copied());
        insert_input("metadata-report/encrypted/in.pdf", bytes);

        let report = metadata_report("metadata-report/encrypted/in.pdf").expect("report");
        assert_eq!(report.status, "encrypted");
        assert!(report.encrypted);
        assert_eq!(report.pages, None);
        assert!(report.info.is_empty(), "ciphertext is not a title");
        assert_eq!(report.xmp, XmpPacket::default());
    }

    #[test]
    fn a_negative_fraction_keeps_its_sign_outside_the_point() {
        assert_eq!(real_text(-5, 2), "-0.05");
        assert_eq!(real_text(5, 2), "0.05");
        assert_eq!(real_text(-2000, 2), "-20.00");
        assert_eq!(real_text(-7, 0), "-7");
        assert_eq!(real_text(0, 1), "0.0");
    }
}
