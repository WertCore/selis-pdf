//! Structural fingerprint report: the identity a document carries in its own
//! bytes, as JSON. Nothing here rewrites the input.
//!
//! Where [`crate::metadata`] answers "what does this document say about
//! itself", the fingerprint answers "which document is this, and is it the one
//! I saw before". It pairs the authoring trails (producer, creator, the two
//! `/Info` timestamps) with the two identifiers the trailer carries, the
//! revision count, and the set of fonts and image streams the pages reference.
//! Two files built from the same source share a fingerprint; a re-save that
//! changes nothing a reader sees still moves the trailer's changing `/ID` and
//! the revision count, and the report shows that.
//!
//! The image hashes are of the *stored* (still-encoded) stream bytes: this is a
//! fast content fingerprint for change detection, not a cryptographic digest
//! and not perceptual. A visually identical image re-encoded by another writer
//! will not match — the perceptual/raster part is deferred to the render
//! ladder and is not claimed here.
//!
//! An encrypted document reports `status: "encrypted"` with only the facts that
//! survive without the file key: the header version, the byte count, the
//! revision count, and the trailer `/ID` (which is never encrypted). Its
//! dictionaries are ciphertext, so no font, image, or `/Info` field is invented.

use serde::Serialize;

use selis_bytes::Bytes;
use selis_pdf_cos::copy::resolve_ref;
use selis_pdf_cos::verify::survey;
use selis_pdf_cos::Obj;
use selis_sandbox::{Budget, BudgetGuard};

use crate::{read_file, CliError, CliResult, ToolResult};

/// The fingerprint of one document.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct FingerprintReport {
    /// The versioned schema tag (`selis-fingerprint/1`), so a reader can tell
    /// this report apart from the metadata report and the Phase 3 reports.
    pub schema: String,
    /// `ok` when the document was readable, `encrypted` when it was not.
    pub status: String,
    /// The header version, e.g. `1.7`.
    pub pdf_version: String,
    /// Input size in bytes.
    pub bytes: u64,
    /// How many incremental revisions the file carries.
    pub revisions: u64,
    /// Whether the file was saved more than once (revisions &gt; 1).
    pub incrementally_updated: bool,
    /// The trailer's `/ID` pair.
    pub ids: FileIds,
    /// `/Info /Producer`, when present and non-empty.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub producer: Option<String>,
    /// `/Info /Creator`, when present and non-empty.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub creator: Option<String>,
    /// `/Info /CreationDate`, as the file holds it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub creation_date: Option<String>,
    /// `/Info /ModDate`, as the file holds it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mod_date: Option<String>,
    /// Leaf page count (`None` for an encrypted document).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pages: Option<u64>,
    /// Every distinct font the pages reference, deduplicated.
    pub fonts: Vec<FontFact>,
    /// Every distinct image stream the pages reference, deduplicated by hash.
    pub images: Vec<ImageFact>,
}

/// The trailer `/ID`: a permanent identifier and a changing one.
///
/// ISO 32000 keeps `/ID[0]` constant across a document's life and rewrites
/// `/ID[1]` on every save, so `changed` being true means the file has been
/// updated since it was created.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct FileIds {
    /// `/ID[0]` as lowercase hex, when present.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub original: Option<String>,
    /// `/ID[1]` as lowercase hex, when present.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub current: Option<String>,
    /// Whether the two identifiers differ.
    pub changed: bool,
}

/// One distinct font referenced by the pages.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FontFact {
    /// The `/BaseFont` name, empty when the font omits one.
    pub base_font: String,
    /// The `/Subtype` (`Type1`, `TrueType`, `Type0`, ...), empty when omitted.
    pub subtype: String,
    /// Whether an embedded font program (`/FontFile`, `/FontFile2`, or
    /// `/FontFile3`) is present, following a `Type0` font to its descendant.
    pub embedded: bool,
    /// How many page-resource slots reference a font with this identity.
    pub references: u64,
}

/// One distinct image stream referenced by the pages.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ImageFact {
    /// A 64-bit FNV-1a hash of the stored stream bytes, as 16 lowercase hex
    /// digits. Fast and deterministic, not cryptographic, not perceptual.
    pub hash: String,
    /// `/Width` in samples, when it is a direct integer.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub width: Option<u64>,
    /// `/Height` in samples, when it is a direct integer.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub height: Option<u64>,
    /// The `/Filter` name (or the first of a filter array), when present.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub filter: Option<String>,
    /// The stored stream length in bytes.
    pub stored_bytes: u64,
    /// How many page-resource slots reference an image with this hash.
    pub references: u64,
}

/// The most fonts or images the walk will record before it stops, so a hostile
/// resource tree cannot grow the report without bound.
const MAX_FINGERPRINT_ITEMS: usize = 4096;

/// The most `/ID` bytes rendered per identifier. A conforming identifier is 16
/// bytes; the cap keeps a hostile trailer from turning the report into a spike.
const MAX_ID_BYTES: usize = 64;

/// Read the document's structural fingerprint without touching its bytes.
///
/// # Errors
///
/// A typed [`CliError`] when the file cannot be read or parsed.
pub fn fingerprint_report(path: &str) -> ToolResult<FingerprintReport> {
    let src = read_file(path)?;
    let budget = Budget::unlimited();
    let clock = crate::shell_clock();
    let mut g = crate::runtime::cli_guard(&budget, &clock);
    let startxref = selis_pdf_cos::xref::find_startxref(&src, 4096).unwrap_or(0);
    let doc = selis_pdf_cos::parse_revisions(&src, startxref, &budget, &mut g)
        .map_err(|e| CliError(format!("{path}: cannot open: {e}")))?;
    let newest = doc
        .revisions()
        .last()
        .ok_or_else(|| CliError(format!("{path}: no revisions")))?;

    let encrypted = newest.encrypt.is_some();
    // The trailer /ID is never encrypted, so it is read for every document.
    let ids = match newest.trailer.iter().find(|(k, _)| k.as_slice() == b"ID") {
        Some((_, Obj::Array(items))) => {
            let hex = |obj: &Obj| match obj {
                Obj::String(bytes) | Obj::HexString(bytes) => Some(to_hex(bytes.as_slice())),
                _ => None,
            };
            let original = items.first().and_then(hex);
            let current = items.get(1).and_then(hex);
            let changed = matches!((&original, &current), (Some(a), Some(b)) if a != b);
            FileIds {
                original,
                current,
                changed,
            }
        }
        _ => FileIds::default(),
    };

    let base = FingerprintReport {
        schema: "selis-fingerprint/1".to_string(),
        status: if encrypted { "encrypted" } else { "ok" }.to_string(),
        pdf_version: crate::metadata::header_version(&src),
        bytes: u64::try_from(src.len()).unwrap_or(u64::MAX),
        revisions: u64::try_from(doc.revisions().len()).unwrap_or(u64::MAX),
        incrementally_updated: doc.revisions().len() > 1,
        ids,
        producer: None,
        creator: None,
        creation_date: None,
        mod_date: None,
        pages: None,
        fonts: Vec::new(),
        images: Vec::new(),
    };
    if encrypted {
        // Objects are ciphertext without the file key: the honest report is the
        // envelope and the /ID, with no font, image, or /Info field invented.
        return Ok(base);
    }

    let observed = survey(&src, &budget, &mut g)
        .map_err(|e| CliError(format!("{path}: cannot survey: {e}")))?;
    let info = crate::metadata::info_fields(&src, &doc, &budget, &mut g)?;
    let info_field = |name: &str| {
        info.iter()
            .find(|field| field.key == name)
            .map(|field| field.value.clone())
            .filter(|value| !value.is_empty())
    };
    let (fonts, images) = resources_fingerprint(&src, &doc, &budget, &mut g)?;

    Ok(FingerprintReport {
        producer: info_field("Producer"),
        creator: info_field("Creator"),
        creation_date: info_field("CreationDate"),
        mod_date: info_field("ModDate"),
        pages: Some(observed.pages),
        fonts,
        images,
        ..base
    })
}

/// Walk every page's (inherited) `/Resources`, collecting the distinct fonts and
/// image streams the document references.
fn resources_fingerprint(
    src: &[u8],
    doc: &selis_pdf_cos::Doc,
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
) -> CliResult<(Vec<FontFact>, Vec<ImageFact>)> {
    let pages = crate::tools::page_refs_of(src, doc, budget, g).map_err(CliError)?;
    let mut fonts: Vec<FontFact> = Vec::new();
    let mut images: Vec<ImageFact> = Vec::new();
    for page in pages {
        let (_, _, resources, _, _) =
            crate::tools::page_info(src, doc, page, budget, g).map_err(CliError)?;
        let Some(resource_obj) = resources else {
            continue;
        };
        let Obj::Dict(resource_dict) = resolve_inline(src, doc, resource_obj, budget, g) else {
            continue;
        };

        if let Some((_, value)) = resource_dict.iter().find(|(k, _)| k.as_slice() == b"Font") {
            if let Obj::Dict(font_dict) = resolve_inline(src, doc, value.clone(), budget, g) {
                for (_, font_value) in font_dict {
                    if fonts.len() >= MAX_FINGERPRINT_ITEMS {
                        break;
                    }
                    if let Obj::Dict(font) = resolve_inline(src, doc, font_value, budget, g) {
                        accumulate_font(&mut fonts, font_fact(src, doc, &font, budget, g));
                    }
                }
            }
        }

        if let Some((_, value)) = resource_dict
            .iter()
            .find(|(k, _)| k.as_slice() == b"XObject")
        {
            if let Obj::Dict(xobjects) = resolve_inline(src, doc, value.clone(), budget, g) {
                for (_, xobject) in xobjects {
                    if images.len() >= MAX_FINGERPRINT_ITEMS {
                        break;
                    }
                    if let Obj::Stream { dict, data } = resolve_inline(src, doc, xobject, budget, g)
                    {
                        if is_image(&dict) {
                            accumulate_image(&mut images, image_fact(&dict, data.as_slice()));
                        }
                    }
                }
            }
        }
    }
    fonts.sort_by(|a, b| {
        a.base_font
            .cmp(&b.base_font)
            .then_with(|| a.subtype.cmp(&b.subtype))
            .then_with(|| a.embedded.cmp(&b.embedded))
    });
    images.sort_by(|a, b| a.hash.cmp(&b.hash));
    Ok((fonts, images))
}

/// Resolve an indirect reference, returning [`Obj::Null`] on a dangling one so
/// a damaged resource entry is skipped rather than failing the whole report.
fn resolve_inline(
    src: &[u8],
    doc: &selis_pdf_cos::Doc,
    obj: Obj,
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
) -> Obj {
    match obj {
        Obj::Ref(r) => resolve_ref(src, doc, r, budget, g).unwrap_or(Obj::Null),
        other => other,
    }
}

/// One font's identity, counted once per resource slot by [`accumulate_font`].
fn font_fact(
    src: &[u8],
    doc: &selis_pdf_cos::Doc,
    font: &[(Bytes, Obj)],
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
) -> FontFact {
    FontFact {
        base_font: find_name(font, b"BaseFont").unwrap_or_default(),
        subtype: find_name(font, b"Subtype").unwrap_or_default(),
        embedded: font_embedded(src, doc, font, budget, g),
        references: 1,
    }
}

/// Whether the font (or, for a `Type0`, its descendant CIDFont) carries an
/// embedded font program.
fn font_embedded(
    src: &[u8],
    doc: &selis_pdf_cos::Doc,
    font: &[(Bytes, Obj)],
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
) -> bool {
    if descriptor_has_file(src, doc, font, budget, g) {
        return true;
    }
    if let Some((_, value)) = font
        .iter()
        .find(|(k, _)| k.as_slice() == b"DescendantFonts")
    {
        if let Obj::Array(items) = resolve_inline(src, doc, value.clone(), budget, g) {
            for item in items {
                if let Obj::Dict(cid) = resolve_inline(src, doc, item, budget, g) {
                    if descriptor_has_file(src, doc, &cid, budget, g) {
                        return true;
                    }
                }
            }
        }
    }
    false
}

/// Whether a font dictionary's `/FontDescriptor` holds a font program.
fn descriptor_has_file(
    src: &[u8],
    doc: &selis_pdf_cos::Doc,
    font: &[(Bytes, Obj)],
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
) -> bool {
    let Some((_, value)) = font.iter().find(|(k, _)| k.as_slice() == b"FontDescriptor") else {
        return false;
    };
    let Obj::Dict(descriptor) = resolve_inline(src, doc, value.clone(), budget, g) else {
        return false;
    };
    descriptor
        .iter()
        .any(|(k, _)| matches!(k.as_slice(), b"FontFile" | b"FontFile2" | b"FontFile3"))
}

/// One image's identity, counted once per resource slot by [`accumulate_image`].
fn image_fact(dict: &[(Bytes, Obj)], data: &[u8]) -> ImageFact {
    ImageFact {
        hash: fnv1a_hex(data),
        width: find_int(dict, b"Width"),
        height: find_int(dict, b"Height"),
        filter: find_filter(dict),
        stored_bytes: u64::try_from(data.len()).unwrap_or(u64::MAX),
        references: 1,
    }
}

/// Add a font, merging into an existing identity and bumping its count.
fn accumulate_font(fonts: &mut Vec<FontFact>, fact: FontFact) {
    if let Some(existing) = fonts.iter_mut().find(|existing| {
        existing.base_font == fact.base_font
            && existing.subtype == fact.subtype
            && existing.embedded == fact.embedded
    }) {
        existing.references = existing.references.saturating_add(1);
    } else {
        fonts.push(fact);
    }
}

/// Add an image, merging into an existing hash and bumping its count.
fn accumulate_image(images: &mut Vec<ImageFact>, fact: ImageFact) {
    if let Some(existing) = images
        .iter_mut()
        .find(|existing| existing.hash == fact.hash)
    {
        existing.references = existing.references.saturating_add(1);
    } else {
        images.push(fact);
    }
}

/// Whether a stream dictionary is an image XObject.
fn is_image(dict: &[(Bytes, Obj)]) -> bool {
    dict.iter().any(|(k, v)| {
        k.as_slice() == b"Subtype" && matches!(v, Obj::Name(n) if n.as_slice() == b"Image")
    })
}

/// A `/Name` value by key, as text.
fn find_name(dict: &[(Bytes, Obj)], key: &[u8]) -> Option<String> {
    dict.iter()
        .find(|(k, _)| k.as_slice() == key)
        .and_then(|(_, v)| match v {
            Obj::Name(bytes) => Some(String::from_utf8_lossy(bytes.as_slice()).into_owned()),
            _ => None,
        })
}

/// A direct integer value by key.
fn find_int(dict: &[(Bytes, Obj)], key: &[u8]) -> Option<u64> {
    dict.iter()
        .find(|(k, _)| k.as_slice() == key)
        .and_then(|(_, v)| match v {
            Obj::Int(n) => u64::try_from(*n).ok(),
            _ => None,
        })
}

/// The `/Filter` name, or the first name of a filter array.
fn find_filter(dict: &[(Bytes, Obj)]) -> Option<String> {
    let (_, value) = dict.iter().find(|(k, _)| k.as_slice() == b"Filter")?;
    let name_text = |bytes: &Bytes| String::from_utf8_lossy(bytes.as_slice()).into_owned();
    match value {
        Obj::Name(bytes) => Some(name_text(bytes)),
        Obj::Array(items) => items.iter().find_map(|item| match item {
            Obj::Name(bytes) => Some(name_text(bytes)),
            _ => None,
        }),
        _ => None,
    }
}

/// Lowercase hex of up to [`MAX_ID_BYTES`] bytes.
fn to_hex(bytes: &[u8]) -> String {
    let capped = bytes.get(..MAX_ID_BYTES).unwrap_or(bytes);
    let mut out = String::with_capacity(capped.len().saturating_mul(2));
    for byte in capped {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

/// A 64-bit FNV-1a hash as 16 lowercase hex digits.
fn fnv1a_hex(bytes: &[u8]) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{hash:016x}")
}

impl FingerprintReport {
    /// The report as indented JSON: the one export shape, owned here so a reader
    /// sees the same fields whether a CLI or the browser asked.
    ///
    /// # Errors
    ///
    /// [`CliError`] when serialization fails.
    pub fn to_json(&self) -> ToolResult<Vec<u8>> {
        serde_json::to_vec_pretty(self)
            .map_err(|e| CliError(format!("cannot encode the fingerprint report: {e}")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{insert_input, MemorySession};

    fn stream_object(number: usize, dict: &str, data: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(format!("{number} 0 obj\n{dict}\nstream\n").as_bytes());
        out.extend_from_slice(data);
        out.extend_from_slice(b"\nendstream\nendobj\n");
        out
    }

    fn build_pdf(objects: Vec<Vec<u8>>, trailer_extra: &str) -> Vec<u8> {
        let mut out = b"%PDF-1.7\n".to_vec();
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
            format!(
                "trailer\n<< /Size {size} /Root 1 0 R {trailer_extra} >>\nstartxref\n{xref}\n%%EOF\n"
            )
            .as_bytes(),
        );
        out
    }

    const IMAGE_BYTES: &[u8] = b"RGBrgbRGBrgb";

    /// A one-page document with a non-embedded font, an image XObject, an
    /// `/Info` dictionary and a trailer `/ID`.
    fn document(embedded: bool) -> Vec<u8> {
        let font = if embedded {
            b"5 0 obj\n<< /Type /Font /Subtype /TrueType /BaseFont /CustomFont /FontDescriptor 8 0 R >>\nendobj\n".to_vec()
        } else {
            b"5 0 obj\n<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>\nendobj\n".to_vec()
        };
        let image_dict = format!(
            "<< /Type /XObject /Subtype /Image /Width 2 /Height 2 /BitsPerComponent 8 /ColorSpace /DeviceRGB /Length {} >>",
            IMAGE_BYTES.len()
        );
        let mut objects = vec![
            b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n".to_vec(),
            b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n".to_vec(),
            b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 100] /Resources 4 0 R >>\nendobj\n".to_vec(),
            b"4 0 obj\n<< /Font << /F1 5 0 R >> /XObject << /Im1 6 0 R >> >>\nendobj\n".to_vec(),
            font,
            stream_object(6, image_dict.as_str(), IMAGE_BYTES),
            b"7 0 obj\n<< /Producer (Selis Test) /Creator (Unit Fixture) /CreationDate (D:20240101000000Z) /ModDate (D:20240102000000Z) >>\nendobj\n".to_vec(),
        ];
        if embedded {
            objects.push(b"8 0 obj\n<< /Type /FontDescriptor /FontName /CustomFont /FontFile2 9 0 R >>\nendobj\n".to_vec());
            objects.push(stream_object(9, "<< /Length 4 >>", b"fake"));
        }
        build_pdf(
            objects,
            "/Info 7 0 R /ID [<0011223344556677> <8899aabbccddeeff>]",
        )
    }

    /// A two-page document where both pages share one `/Resources` object.
    fn two_page_shared() -> Vec<u8> {
        let image_dict = format!(
            "<< /Type /XObject /Subtype /Image /Width 2 /Height 2 /BitsPerComponent 8 /ColorSpace /DeviceRGB /Length {} >>",
            IMAGE_BYTES.len()
        );
        let objects = vec![
            b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n".to_vec(),
            b"2 0 obj\n<< /Type /Pages /Kids [3 0 R 7 0 R] /Count 2 >>\nendobj\n".to_vec(),
            b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 100] /Resources 4 0 R >>\nendobj\n".to_vec(),
            b"4 0 obj\n<< /Font << /F1 5 0 R >> /XObject << /Im1 6 0 R >> >>\nendobj\n".to_vec(),
            b"5 0 obj\n<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>\nendobj\n".to_vec(),
            stream_object(6, image_dict.as_str(), IMAGE_BYTES),
            b"7 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 100] /Resources 4 0 R >>\nendobj\n".to_vec(),
        ];
        build_pdf(objects, "/ID [<abcd> <abcd>]")
    }

    #[test]
    fn reads_ids_info_fonts_and_images() {
        let _session = MemorySession::activate();
        insert_input("fingerprint/plain/in.pdf", document(false));

        let report = fingerprint_report("fingerprint/plain/in.pdf").expect("report");
        assert_eq!(report.status, "ok");
        assert_eq!(report.pdf_version, "1.7");
        assert_eq!(report.revisions, 1);
        assert!(!report.incrementally_updated);
        assert_eq!(report.ids.original.as_deref(), Some("0011223344556677"));
        assert_eq!(report.ids.current.as_deref(), Some("8899aabbccddeeff"));
        assert!(report.ids.changed);
        assert_eq!(report.producer.as_deref(), Some("Selis Test"));
        assert_eq!(report.creator.as_deref(), Some("Unit Fixture"));
        assert_eq!(report.creation_date.as_deref(), Some("D:20240101000000Z"));
        assert_eq!(report.mod_date.as_deref(), Some("D:20240102000000Z"));
        assert_eq!(report.pages, Some(1));

        assert_eq!(report.fonts.len(), 1);
        assert_eq!(
            report.fonts[0],
            FontFact {
                base_font: "Helvetica".to_string(),
                subtype: "Type1".to_string(),
                embedded: false,
                references: 1,
            }
        );

        assert_eq!(report.images.len(), 1);
        let image = &report.images[0];
        assert_eq!(image.width, Some(2));
        assert_eq!(image.height, Some(2));
        assert_eq!(image.filter, None);
        assert_eq!(image.stored_bytes, IMAGE_BYTES.len() as u64);
        assert_eq!(image.references, 1);
        assert_eq!(image.hash.len(), 16);
        assert!(image.hash.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn an_embedded_font_is_detected_through_its_descendant() {
        let _session = MemorySession::activate();
        insert_input("fingerprint/embedded/in.pdf", document(true));

        let report = fingerprint_report("fingerprint/embedded/in.pdf").expect("report");
        assert_eq!(report.fonts.len(), 1);
        assert_eq!(report.fonts[0].base_font, "CustomFont");
        assert_eq!(report.fonts[0].subtype, "TrueType");
        assert!(report.fonts[0].embedded, "the /FontFile2 is found");
    }

    #[test]
    fn a_shared_resource_is_deduplicated_and_counted() {
        let _session = MemorySession::activate();
        insert_input("fingerprint/shared/in.pdf", two_page_shared());

        let report = fingerprint_report("fingerprint/shared/in.pdf").expect("report");
        assert_eq!(report.pages, Some(2));
        assert_eq!(report.fonts.len(), 1, "one font across two pages");
        assert_eq!(report.fonts[0].references, 2);
        assert_eq!(report.images.len(), 1, "one image across two pages");
        assert_eq!(report.images[0].references, 2);
        assert!(!report.ids.changed, "matching /ID halves are not a change");
    }

    #[test]
    fn the_report_is_deterministic_and_serializes() {
        let _session = MemorySession::activate();
        insert_input("fingerprint/det/in.pdf", document(false));

        let first = fingerprint_report("fingerprint/det/in.pdf").expect("report");
        let second = fingerprint_report("fingerprint/det/in.pdf").expect("report");
        assert_eq!(first, second);

        let json = String::from_utf8(first.to_json().expect("json")).expect("UTF-8");
        assert_eq!(first.schema, "selis-fingerprint/1");
        assert!(
            json.contains("selis-fingerprint/1"),
            "the schema tag is in the export: {json}"
        );
        assert!(
            json.contains("Helvetica"),
            "fonts are in the export: {json}"
        );
        assert!(
            json.contains(&first.images[0].hash),
            "the image hash is in the export: {json}"
        );
    }

    #[test]
    fn an_encrypted_document_reports_only_the_envelope() {
        let _session = MemorySession::activate();
        let mut bytes = document(false);
        let trailer_at = bytes
            .windows(b"trailer".len())
            .position(|window| window == b"trailer")
            .expect("trailer");
        let insert_at = trailer_at + b"trailer\n<<".len();
        bytes.splice(insert_at..insert_at, b" /Encrypt 99 0 R".iter().copied());
        insert_input("fingerprint/encrypted/in.pdf", bytes);

        let report = fingerprint_report("fingerprint/encrypted/in.pdf").expect("report");
        assert_eq!(report.status, "encrypted");
        assert_eq!(report.pages, None);
        assert!(report.fonts.is_empty(), "ciphertext is not a font list");
        assert!(report.images.is_empty());
        assert_eq!(report.producer, None);
        // The trailer /ID survives without the file key.
        assert_eq!(report.ids.current.as_deref(), Some("8899aabbccddeeff"));
    }
}
