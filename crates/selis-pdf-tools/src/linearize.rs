//! The Linearize ("fast web view") tool operation.
//!
//! Rewrites a PDF so its first page can be rendered after fetching only the
//! leading bytes. The byte-level writer lives in `selis-pdf-optimize`; this
//! module owns the tool boundary: open, refuse encrypted inputs, collect the
//! reachable object graph, render, verify by re-opening the output, and commit
//! atomically through the shared write gate.
//!
//! Encrypted documents are refused — removing encryption is the separate
//! unlock operation, never a side effect of linearisation.
//!
//! The cross-reference encoding is selectable: [`linearize_file`] keeps the
//! classic PDF 1.4 xref tables (the compatibility default), while
//! [`linearize_file_with_mode`] can emit PDF 1.5 `/Type /XRef` streams and
//! `/ObjStm` object streams with the same hint tables.

use selis_pdf_cos::copy::{collect_objects, resolve_ref};
use selis_pdf_cos::{Obj, Ref};
use selis_sandbox::{Budget, BudgetGuard};

use crate::verify_report::Verification;
use crate::write_gate::{self, PRESERVE_ALL};
use crate::{read_file, CliError, CliResult};

/// What one linearisation rewrite produced.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LinearizeReport {
    /// Objects in the collected, rewritten graph.
    pub objects: usize,
    /// Input size in bytes.
    pub in_bytes: usize,
    /// Output size in bytes.
    pub out_bytes: usize,
    /// The declared file length from the output's `/Linearized` dictionary.
    pub declared_length: u64,
    /// The declared page count (`/N`) from the output's lin dictionary.
    pub declared_pages: u32,
}

/// Linearize `path` into `output`, returning the measured report and the
/// structural verification of the committed bytes.
///
/// # Errors
///
/// [`crate::ToolError`] when the input cannot be read, is encrypted, has no
/// usable page tree, or the rewritten output fails re-opening / verification.
pub fn linearize(path: &str, output: &str) -> CliResult<(LinearizeReport, Verification)> {
    let (report, verification) = linearize_file_with_mode(path, output, Default::default())?;
    eprintln!(
        "linearized {path}: {in} -> {out} bytes, {objects} objects, /L {declared}, /N {pages} -> {output}",
        in = report.in_bytes,
        out = report.out_bytes,
        objects = report.objects,
        declared = report.declared_length,
        pages = report.declared_pages,
    );
    verification.emit_line();
    Ok((report, verification))
}

/// Linearize `path` and return both the report and verification. This is the
/// shared entry point for the CLI, batch mode and the browser protocol.
///
/// # Errors
///
/// As [`linearize`].
pub fn linearize_file(path: &str, output: &str) -> CliResult<(LinearizeReport, Verification)> {
    linearize_file_with_mode(path, output, Default::default())
}

/// As [`linearize_file`], with an explicit cross-reference encoding mode.
///
/// `Classic` keeps the PDF 1.4 `xref` tables used by every previous release.
/// `Compressed` writes both xref sections as `/Type /XRef` streams and packs
/// eligible non-stream objects into `/ObjStm` streams, while keeping the same
/// Annex F hint tables and `/Linearized` dictionary.
///
/// # Errors
///
/// As [`linearize_file`]; the compressed mode also refuses (with a typed
/// error) a graph it cannot legally lay out in object streams.
pub fn linearize_file_with_mode(
    path: &str,
    output: &str,
    mode: selis_pdf_optimize::LinearizeMode,
) -> CliResult<(LinearizeReport, Verification)> {
    let src = read_file(path)?;
    let in_bytes = src.len();
    let budget = Budget::unlimited();
    let clock = crate::shell_clock();
    let mut g = crate::runtime::cli_guard(&budget, &clock);

    let startxref = selis_pdf_cos::xref::find_startxref(&src, 4096).unwrap_or(0);
    let doc = selis_pdf_cos::parse_revisions(&src, startxref, &budget, &mut g)
        .map_err(|e| CliError(format!("{path}: {e}")))?;
    let rev = doc
        .revisions()
        .last()
        .ok_or_else(|| CliError(format!("{path}: no revisions")))?;
    if rev.encrypt.is_some() {
        return Err(CliError(format!(
            "{path}: document is encrypted; remove the password with unlock first"
        )));
    }
    let root = rev
        .root
        .ok_or_else(|| CliError(format!("{path}: no /Root in trailer")))?;

    let info = rev
        .trailer
        .iter()
        .find(|(key, _)| key.as_slice() == b"Info")
        .and_then(|(_, value)| match value {
            Obj::Ref(r) => Some(*r),
            _ => None,
        });
    let id = rev
        .trailer
        .iter()
        .find(|(key, _)| key.as_slice() == b"ID")
        .and_then(|(_, value)| match value {
            Obj::Array(items) if items.len() >= 2 => {
                let first = as_bytes(items.first()?)?;
                let second = as_bytes(items.get(1)?)?;
                Some((first, second))
            }
            _ => None,
        });

    // Reserve object 1 for the linearisation dictionary. Collect from the
    // catalog and (when present) /Info so trailer-only metadata survives.
    let mut roots = vec![root];
    if let Some(info) = info {
        roots.push(info);
    }
    let mut next_num = 2u32;
    let (objects, remap) = collect_objects(&src, &roots, &mut next_num, &budget, &mut g)
        .map_err(|e| CliError(format!("{path}: collect: {e}")))?;
    if objects.is_empty() {
        return Err(CliError(format!("{path}: no reachable objects")));
    }

    let new_root_num = remap.get(&root.num).copied().unwrap_or(root.num);
    let new_root = Ref::new(new_root_num, 0);

    // Trailer entries live outside the object graph, so fold their numbers
    // through the collect remap here.
    let mut trailer_extra: Vec<(Vec<u8>, Obj)> = Vec::new();
    if let Some(info) = info {
        let num = remap.get(&info.num).copied().unwrap_or(info.num);
        trailer_extra.push((b"Info".to_vec(), Obj::Ref(Ref::new(num, 0))));
    }
    if let Some((first, second)) = id {
        trailer_extra.push((
            b"ID".to_vec(),
            Obj::Array(vec![Obj::String(first), Obj::String(second)]),
        ));
    }

    let page_count = crate::tools::page_refs_of(&src, &doc, &budget, &mut g)
        .map_err(|e| CliError(format!("{path}: page tree: {e}")))?
        .len();
    let page_count =
        u32::try_from(page_count).map_err(|_| CliError(format!("{path}: too many pages")))?;

    let input = selis_pdf_optimize::LinearizeInput {
        objects: &objects,
        root: new_root,
        trailer_extra: &trailer_extra,
        page_count,
    };
    let bytes = selis_pdf_optimize::write_linearized_with_mode(&input, &budget, &mut g, mode)
        .map_err(|e| CliError(format!("{path}: linearize: {e}")))?;
    let (declared_length, declared_pages) = read_lin_header(&bytes, &budget, &mut g)
        .map_err(|e| CliError(format!("{path}: output lin dictionary: {e}")))?;

    // Structural verification: the output must reparse from its first-page
    // xref, every reference must resolve, and the input's surveyed counts must
    // hold. Commit is atomic — a failure leaves no output file behind.
    let observed = selis_pdf_cos::verify::survey(&src, &budget, &mut g)
        .map_err(|e| CliError(format!("{path}: input survey failed: {e}")))?;
    let expected = write_gate::expectations_from(&observed, &PRESERVE_ALL);
    let verdict = write_gate::write_verified(&bytes, output, &expected, &budget, &mut g)?;

    // Read-back: re-open the byte buffer and assert the linearisation header
    // is consistent and that page contents extract in the original order.
    verify_output(
        &bytes,
        &src,
        &doc,
        declared_length,
        declared_pages,
        &budget,
        &mut g,
    )
    .map_err(|e| CliError(format!("{path}: read-back: {e}")))?;

    let report = LinearizeReport {
        objects: objects.len(),
        in_bytes,
        out_bytes: bytes.len(),
        declared_length,
        declared_pages,
    };
    let verification = Verification::from_gate(
        Some(&observed),
        &verdict,
        &expected,
        u64::try_from(in_bytes).unwrap_or(u64::MAX),
        u64::try_from(bytes.len()).unwrap_or(u64::MAX),
    );
    Ok((report, verification))
}

/// Read `/L` and `/N` from the output's object-1 linearisation dictionary via
/// the engine's own parser, and assert `/L` equals the real byte length.
fn read_lin_header(
    bytes: &[u8],
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
) -> Result<(u64, u32), String> {
    let startxref = selis_pdf_cos::xref::find_startxref(bytes, 4096).ok_or("no startxref")?;
    let doc = selis_pdf_cos::parse_revisions(bytes, startxref, budget, g)
        .map_err(|e| format!("output does not reparse: {e}"))?;
    let lin = resolve_ref(bytes, &doc, Ref::new(1, 0), budget, g)
        .map_err(|e| format!("resolve object 1: {e}"))?;
    let parsed = selis_pdf_cos::Linearisation::from_obj(&lin, g)
        .map_err(|e| format!("parse lin dictionary: {e}"))?;
    if !parsed.is_linearised() {
        return Err("output is not linearised".to_string());
    }
    let length = parsed.file_length.ok_or("no /L")?;
    if length != bytes.len() as u64 {
        return Err(format!("/L {length} != actual {}", bytes.len()));
    }
    let pages = parsed.page_count.ok_or("no /N")?;
    Ok((length, pages))
}

/// Re-open the finished bytes and assert page count/order and that each page's
/// content stream still extracts byte-for-byte relative to the source.
fn verify_output(
    out: &[u8],
    src: &[u8],
    src_doc: &selis_pdf_cos::Doc,
    declared_length: u64,
    declared_pages: u32,
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
) -> Result<(), String> {
    let startxref = selis_pdf_cos::xref::find_startxref(out, 4096).ok_or("no startxref")?;
    let out_doc = selis_pdf_cos::parse_revisions(out, startxref, budget, g)
        .map_err(|e| format!("output does not reparse: {e}"))?;

    let out_pages = crate::tools::page_refs_of(out, &out_doc, budget, g)
        .map_err(|e| format!("output page tree: {e}"))?;
    let src_pages = crate::tools::page_refs_of(src, src_doc, budget, g)
        .map_err(|e| format!("source page tree: {e}"))?;
    if out_pages.len() != src_pages.len()
        || u32::try_from(out_pages.len()).map_err(|_| "count")? != declared_pages
    {
        return Err(format!(
            "page count changed: {} -> {} (/N {declared_pages})",
            src_pages.len(),
            out_pages.len()
        ));
    }
    for (i, (src_page, out_page)) in src_pages.iter().zip(out_pages.iter()).enumerate() {
        let a = page_content(src, src_doc, *src_page, budget, g)?;
        let b = page_content(out, &out_doc, *out_page, budget, g)?;
        if a != b {
            return Err(format!("page {} content changed", i.saturating_add(1)));
        }
    }

    // The declared length and page count must match the bytes on disk.
    if declared_length != out.len() as u64 {
        return Err(format!("/L {declared_length} != {}", out.len()));
    }
    Ok(())
}

/// Concatenate the (raw, still-encoded) content streams of one page.
fn page_content(
    src: &[u8],
    doc: &selis_pdf_cos::Doc,
    page: Ref,
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
) -> Result<Vec<u8>, String> {
    let obj = resolve_ref(src, doc, page, budget, g).map_err(|e| e.to_string())?;
    let contents = match &obj {
        Obj::Dict(pairs) => pairs
            .iter()
            .find(|(k, _)| k.as_slice() == b"Contents")
            .map(|(_, v)| v.clone()),
        _ => None,
    };
    let Some(contents) = contents else {
        return Ok(Vec::new());
    };
    let refs: Vec<Ref> = match contents {
        Obj::Ref(r) => vec![r],
        Obj::Array(items) => items.iter().filter_map(as_ref).collect(),
        _ => return Ok(Vec::new()),
    };
    let mut out = Vec::new();
    for r in refs {
        let stream = resolve_ref(src, doc, r, budget, g).map_err(|e| e.to_string())?;
        if let Obj::Stream { data, .. } = stream {
            out.extend_from_slice(data.as_slice());
        }
    }
    Ok(out)
}

fn as_ref(obj: &Obj) -> Option<Ref> {
    match obj {
        Obj::Ref(r) => Some(*r),
        _ => None,
    }
}

fn as_bytes(obj: &Obj) -> Option<selis_bytes::Bytes> {
    match obj {
        Obj::String(b) | Obj::HexString(b) => Some(b.clone()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::indexing_slicing,
        clippy::cast_possible_truncation,
        clippy::arithmetic_side_effects,
        clippy::integer_division,
        clippy::panic
    )]

    use super::*;

    fn parse_h(text: &str) -> Vec<u64> {
        let marker = "/H [";
        let start = text.find(marker).expect("/H") + marker.len();
        let rest = &text[start..];
        let end = rest.find(']').expect("/H close");
        rest[..end]
            .split_whitespace()
            .map(|v| v.parse().expect("int"))
            .collect()
    }

    fn parse_int_after(text: &str, marker: &str) -> usize {
        let start = text.find(marker).expect(marker) + marker.len();
        let rest = &text[start..];
        let end = rest
            .find(|c: char| !c.is_ascii_digit())
            .unwrap_or(rest.len());
        rest[..end].parse().expect("integer")
    }

    fn temp_dir(name: &str) -> std::path::PathBuf {
        let dir =
            std::env::temp_dir().join(format!("selis-linearize-{}-{name}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).expect("create temp dir");
        dir
    }

    /// A three-page PDF built with the engine's own document writer. Each page
    /// has distinct content; page 1 also carries an annotation.
    fn three_page_pdf() -> Vec<u8> {
        let budget = Budget::unlimited();
        let mut g = budget.guard();
        let mut b = selis_pdf_cos::doc_writer::DocumentBuilder::new();
        b.add_page(200.0, 300.0, b"BT (PAGE-ONE) Tj ET\n");
        b.add_page(200.0, 300.0, b"BT (PAGE-TWO) Tj ET\n");
        b.add_page(200.0, 300.0, b"BT (PAGE-THREE) Tj ET\n");
        b.set_info(b"Title", "linearize fixture");
        b.write(&budget, &mut g).expect("write fixture")
    }

    #[test]
    fn linearizes_and_reopens_preserving_pages_and_content() {
        let dir = temp_dir("three-page");
        let input = dir.join("in.pdf");
        let output = dir.join("out.pdf");
        let src = three_page_pdf();
        std::fs::write(&input, &src).expect("write input");

        let (report, verification) =
            linearize_file(input.to_str().unwrap(), output.to_str().unwrap()).expect("linearize");
        assert_eq!(verification.verdict, "ok");
        assert_eq!(report.declared_pages, 3);
        assert_eq!(report.declared_length, report.out_bytes as u64);

        let out = std::fs::read(&output).expect("read output");
        let text = String::from_utf8_lossy(&out);
        assert!(text.contains("/Linearized 1"));
        // Object 1 is the lin dictionary and must be first.
        assert!(text.starts_with("%PDF-1.4"));
        assert!(text.find("1 0 obj").unwrap() < text.find("2 0 obj").unwrap());
        // Two xref sections and a /Prev chain.
        assert!(text.contains("/Prev "));
        assert!(text.matches("\nxref\n").count() >= 2);
        // /L in the dictionary equals the true length.
        assert!(
            text.contains(&format!("/L {}", out.len())),
            "/L {} not found",
            out.len()
        );
        // /H points at a real hint stream carrying /S (the shared-object
        // table offset) and both hint tables in its payload.
        let h = parse_h(&text);
        assert_eq!(h.len(), 2, "/H is a 2-element array");
        let hint_at = h[0] as usize;
        let hint_head = String::from_utf8_lossy(&out[hint_at..hint_at + 40]);
        assert!(
            hint_head.contains(" obj"),
            "/H points at an object: {hint_head:?}"
        );
        let stream_at = hint_at
            + out[hint_at..]
                .windows(7)
                .position(|w| w == b"stream\n")
                .expect("hint stream marker");
        let dict = String::from_utf8_lossy(&out[hint_at..stream_at]);
        assert!(dict.contains("/S "), "hint stream has /S");
        let s = parse_int_after(&dict, "/S ");
        let data_start = stream_at + 7;
        let data_end = data_start
            + out[data_start..]
                .windows(10)
                .position(|w| w == b"\nendstream")
                .expect("hint endstream");
        assert!(s < data_end - data_start, "/S within hint payload");
        assert_eq!(
            h[1] as usize,
            data_end - data_start,
            "/H length == payload length"
        );
        // All page contents survived; verify *logical* order via the engine,
        // not raw byte position (linearization deliberately reorders objects).
        for tag in ["PAGE-ONE", "PAGE-TWO", "PAGE-THREE"] {
            assert!(text.contains(tag), "missing {tag}");
        }
        let budget = Budget::unlimited();
        let mut g = budget.guard();
        let sx = selis_pdf_cos::xref::find_startxref(&out, 4096).expect("startxref");
        let doc = selis_pdf_cos::parse_revisions(&out, sx, &budget, &mut g).expect("reopen");
        let pages = crate::tools::page_refs_of(&out, &doc, &budget, &mut g).expect("pages");
        assert_eq!(pages.len(), 3);
        let contents: Vec<String> = pages
            .iter()
            .map(|p| {
                String::from_utf8_lossy(&page_content(&out, &doc, *p, &budget, &mut g).unwrap())
                    .to_string()
            })
            .collect();
        assert!(contents[0].contains("PAGE-ONE"));
        assert!(contents[1].contains("PAGE-TWO"));
        assert!(contents[2].contains("PAGE-THREE"));
    }

    /// A two-page PDF whose resource dictionaries and annotations are
    /// non-stream objects, so the compressed mode actually builds /ObjStm.
    fn resource_pdf() -> Vec<u8> {
        fn nm(s: &[u8]) -> selis_bytes::Bytes {
            selis_bytes::Bytes::copy_from_slice(s)
        }
        let budget = Budget::unlimited();
        let mut g = budget.guard();
        let mut b = selis_pdf_cos::doc_writer::DocumentBuilder::new();
        let res = b.allocate();
        let font = b.allocate();
        let annot1 = b.allocate();
        let annot2 = b.allocate();
        b.add_object(
            res,
            Obj::Dict(vec![(
                nm(b"Font"),
                Obj::Dict(vec![(
                    nm(b"F1"),
                    Obj::Ref(selis_pdf_cos::Ref::new(font, 0)),
                )]),
            )]),
        );
        b.add_object(
            font,
            Obj::Dict(vec![
                (nm(b"Type"), Obj::Name(nm(b"Font"))),
                (nm(b"Subtype"), Obj::Name(nm(b"Type1"))),
            ]),
        );
        let annotation = || {
            Obj::Dict(vec![
                (nm(b"Type"), Obj::Name(nm(b"Annot"))),
                (nm(b"Subtype"), Obj::Name(nm(b"Text"))),
                (
                    nm(b"Rect"),
                    Obj::Array(vec![Obj::Int(1), Obj::Int(2), Obj::Int(3), Obj::Int(4)]),
                ),
            ])
        };
        b.add_object(annot1, annotation());
        b.add_object(annot2, annotation());
        for (tag, annot) in [
            (b"PAGE-ONE".as_slice(), annot1),
            (b"PAGE-TWO".as_slice(), annot2),
        ] {
            let content = b.allocate();
            b.add_object(
                content,
                Obj::Stream {
                    dict: vec![],
                    data: nm(format!("BT ({}) Tj ET\n", String::from_utf8_lossy(tag)).as_bytes()),
                },
            );
            b.add_page_with_extra(
                200.0,
                300.0,
                &[selis_pdf_cos::Ref::new(content, 0)],
                Some(selis_pdf_cos::Ref::new(res, 0)),
                vec![(
                    b"Annots".to_vec(),
                    Obj::Array(vec![Obj::Ref(selis_pdf_cos::Ref::new(annot, 0))]),
                )],
            );
        }
        b.write(&budget, &mut g).expect("write fixture")
    }

    #[test]
    fn compressed_mode_linearizes_and_reopens_with_all_objects() {
        let dir = temp_dir("compressed");
        let input = dir.join("in.pdf");
        let output = dir.join("out.pdf");
        std::fs::write(&input, resource_pdf()).expect("write input");

        let (report, verification) = linearize_file_with_mode(
            input.to_str().unwrap(),
            output.to_str().unwrap(),
            crate::LinearizeMode::Compressed,
        )
        .expect("linearize");
        assert_eq!(
            verification.verdict, "ok",
            "write gate preserved all structures"
        );
        assert_eq!(report.declared_pages, 2);
        assert_eq!(report.declared_length, report.out_bytes as u64);

        let out = std::fs::read(&output).expect("read output");
        let text = String::from_utf8_lossy(&out);
        assert_eq!(text.matches("/Type /XRef").count(), 2, "two xref streams");
        assert!(text.contains("/ObjStm"), "resources and annotations packed");
        assert!(
            text.matches("/Root ").count() >= 2,
            "both xref streams carry /Root"
        );
        assert!(
            text.contains(&format!("/L {}", out.len())),
            "/L matches real length"
        );

        let budget = Budget::unlimited();
        let mut g = budget.guard();
        let sx = selis_pdf_cos::xref::find_startxref(&out, 4096).expect("startxref");
        let doc = selis_pdf_cos::parse_revisions(&out, sx, &budget, &mut g).expect("reopen");
        assert_eq!(doc.revisions().len(), 2);
        let pages = crate::tools::page_refs_of(&out, &doc, &budget, &mut g).expect("pages");
        assert_eq!(pages.len(), 2, "both pages survive");
        let contents: Vec<String> = pages
            .iter()
            .map(|p| {
                String::from_utf8_lossy(
                    &page_content(&out, &doc, *p, &budget, &mut g).expect("content"),
                )
                .to_string()
            })
            .collect();
        assert!(contents[0].contains("PAGE-ONE"));
        assert!(contents[1].contains("PAGE-TWO"));
    }

    #[test]
    fn refuses_encrypted_input_and_writes_nothing() {
        let budget = Budget::unlimited();
        let mut g = budget.guard();
        let mut b = selis_pdf_cos::doc_writer::DocumentBuilder::new();
        b.add_page(100.0, 100.0, b"BT (secret) Tj ET\n");
        let material = selis_pdf_cos::encrypt::R6Material::from_bytes(&[0x5A; 64])
            .expect("a 64-byte credential blob");
        let (info, file_key) =
            selis_pdf_cos::encrypt::EncryptInfo::new_r6(b"", b"owner", 0xFFFFF0C0, &material);
        b.set_encrypt(info, file_key);
        let bytes = b.write(&budget, &mut g).expect("encrypted fixture");

        let dir = temp_dir("encrypted");
        let input = dir.join("in.pdf");
        let output = dir.join("out.pdf");
        std::fs::write(&input, &bytes).expect("write input");
        let err = linearize_file(input.to_str().unwrap(), output.to_str().unwrap())
            .expect_err("encrypted input must be refused");
        assert!(err.0.contains("encrypted"));
        assert!(!output.exists(), "no output file on refusal");
    }

    #[test]
    fn writer_refuses_encrypted_flag_via_error_type() {
        let e = selis_pdf_optimize::LinearizeError::Encrypted;
        assert_eq!(e.code(), selis_error::Code::AlreadyEncrypted);
    }
}
