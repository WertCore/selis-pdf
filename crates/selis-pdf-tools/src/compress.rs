//! Compression for the browser and CLI tools.
//!
//! The lossless tier is a full-document rewrite that keeps every rendered
//! pixel identical:
//!
//! * only objects reachable from the catalog (and `/Info`) are kept;
//! * unfiltered streams gain `FlateDecode`, and existing Flate streams are
//!   re-encoded at the highest level when that is smaller;
//! * byte-identical objects are deduplicated and every reference is redirected
//!   at the survivor.
//!
//! The lossy tier ([`CompressOptions::level`]) adds the shared image pass from
//! [`selis_pdf_optimize`]: simple image XObjects are re-encoded as JPEG at a
//! reduced quality and downsampled past a pixel cap. With a target size the
//! search runs up to [`selis_pdf_optimize::MAX_PASSES`] quality-lowering
//! passes, each from the lossless baseline (no generational loss), until the
//! output fits or stops shrinking. Text, annotations, forms and outlines are
//! never touched, and the output is never larger than the input — when no
//! candidate beats the original bytes, the original is returned verbatim.
//!
//! Encrypted documents are refused. Removing encryption is the separate
//! unlock operation, not a side effect of compression.

use selis_pdf_cos::copy::{collect_objects, dedup_objects};
use selis_pdf_cos::doc_writer::write_objects_as_document_with_trailer;
use selis_pdf_cos::{Obj, Ref};
use selis_pdf_optimize::{CompressionLevel, MAX_PASSES};
use selis_sandbox::{Budget, BudgetGuard};

use crate::verify_report::Verification;
use crate::write_gate::{self, PRESERVE_ALL};
use crate::{read_file, CliError, CliResult};

/// The maximum bytes a single stream may expand to while being re-encoded.
const RECODE_LIMIT: u64 = 256 * 1024 * 1024;

/// The zlib level used when re-encoding streams.
const RECODE_LEVEL: u8 = 10;

/// Which tier the Compress tool runs. The default is the lossless-only
/// rewrite; a `level` opts into the shared lossy image pass.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CompressOptions {
    /// The lossy level (`less`, `recommended`, `extreme`); `None` keeps the
    /// lossless-only behaviour.
    pub level: Option<CompressionLevel>,
    /// Optional target size in KiB. With a level set, the search lowers the
    /// quality across bounded passes until the output fits or stops
    /// shrinking. Without a level the target is ignored (lossless cannot
    /// trade quality for size).
    pub target_kib: Option<u64>,
}

/// The measured result of one rewrite.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CompressionReport {
    /// Objects reachable from the root (after GC, before dedup).
    pub objects: usize,
    /// Objects removed by identical-object dedup.
    pub deduplicated: usize,
    /// Streams re-encoded (new Flate, or smaller Flate).
    pub streams_recoded: usize,
    /// Image XObjects the lossy tier re-encoded (0 when lossless only).
    pub images_reencoded: usize,
    /// Lossy passes actually run (0 when lossless only).
    pub lossy_passes: usize,
    /// Input size in bytes.
    pub in_bytes: usize,
    /// Output size in bytes.
    pub out_bytes: usize,
}

impl CompressionReport {
    /// The number of bytes saved (`in - out`, saturating).
    #[must_use]
    pub fn saved(&self) -> usize {
        self.in_bytes.saturating_sub(self.out_bytes)
    }
}

/// Compress `path` into `output` and return the structural verification.
///
/// # Errors
///
/// [`crate::ToolError`] when the input cannot be read, the document is
/// encrypted or damaged, or the rewritten output fails verification.
pub fn compress(path: &str, output: &str) -> CliResult<Verification> {
    let (report, verification) = compress_file(path, output)?;
    eprintln!(
        "compressed {path}: {} -> {} bytes (saved {}), {} objects, {} deduplicated, {} streams re-encoded, {} images re-encoded in {} lossy pass(es) -> {output}",
        report.in_bytes,
        report.out_bytes,
        report.saved(),
        report.objects,
        report.deduplicated,
        report.streams_recoded,
        report.images_reencoded,
        report.lossy_passes
    );
    verification.emit_line();
    Ok(verification)
}

/// Compress `path` and return both the measured report and verification.
///
/// This is the shared entry point for batch mode and the browser protocol.
/// It runs the lossless tier only; [`compress_file_with_options`] adds the
/// lossy level/target search.
pub fn compress_file(path: &str, output: &str) -> CliResult<(CompressionReport, Verification)> {
    compress_file_with_options(path, output, &CompressOptions::default())
}

/// Compress `path` with an explicit tier and return the report and
/// verification.
///
/// The lossless rewrite is always computed first and is the floor every
/// lossy candidate must beat. With a level set, up to
/// [`selis_pdf_optimize::MAX_PASSES`] passes re-encode images at progressively
/// lower quality, each cloned from the lossless baseline so quality never
/// compounds. The smallest candidate wins; when none beats the input bytes
/// the input is returned verbatim, so the output is never larger than the
/// input. Verification ([`PRESERVE_ALL`]) runs once on the chosen bytes.
///
/// # Errors
///
/// As [`compress_file`].
pub fn compress_file_with_options(
    path: &str,
    output: &str,
    options: &CompressOptions,
) -> CliResult<(CompressionReport, Verification)> {
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
            Obj::Ref(reference) => Some(*reference),
            _ => None,
        });

    // Garbage-collect: walk from the catalog root, plus /Info when present so
    // the metadata object survives the rewrite.
    let mut roots = vec![root];
    if let Some(info) = info {
        roots.push(info);
    }
    let mut next_num = 1u32;
    let (mut objects, remap) = collect_objects(&src, &roots, &mut next_num, &budget, &mut g)
        .map_err(|e| CliError(format!("{path}: {e}")))?;
    let object_count = objects.len();
    let new_root = remap.get(&root.num).copied().unwrap_or(root.num);
    // The trailer is outside the object graph, so its `/Info` reference is
    // folded through the collect remap here and the dedup remap per candidate.
    let info_num = info.and_then(|reference| remap.get(&reference.num).copied());

    // Re-encode streams losslessly. This baseline is the floor every lossy
    // candidate must beat, and the output when no level is requested.
    let mut streams_recoded = 0usize;
    for (_num, obj) in objects.iter_mut() {
        if recode_stream(obj, &budget, &mut g) {
            streams_recoded = streams_recoded.saturating_add(1);
        }
    }

    let (mut best, mut best_dedup) =
        render_candidate(objects.clone(), new_root, info_num, path, &budget, &mut g)?;
    let mut images_reencoded = 0usize;
    let mut lossy_passes = 0usize;

    if let Some(level) = options.level {
        let target_bytes = options.target_kib.map(|kib| kib.saturating_mul(1024));
        let mut prev_len: Option<usize> = None;
        for index in 0..MAX_PASSES {
            // Each pass starts from the lossless baseline, not the previous
            // pass's output, so re-encoding never compounds quality loss.
            let mut candidate = objects.clone();
            let image_report =
                selis_pdf_optimize::recompress_images(&mut candidate, level.pass(index), &mut g);
            lossy_passes = lossy_passes.saturating_add(1);
            if image_report.eligible == 0 {
                // Nothing this tier can touch; harder passes cannot help.
                break;
            }
            let (bytes, dedup) =
                render_candidate(candidate, new_root, info_num, path, &budget, &mut g)?;
            let len = bytes.len();
            if len < best.len() {
                best = bytes;
                best_dedup = dedup;
                images_reencoded = image_report.reencoded;
            }
            if target_bytes
                .is_some_and(|target| u64::try_from(best.len()).unwrap_or(u64::MAX) <= target)
            {
                break;
            }
            // No further shrinkage: a harder pass that does not beat the
            // previous one ends the bounded search.
            if prev_len.is_some_and(|previous| len >= previous) {
                break;
            }
            prev_len = Some(len);
        }
        // Never larger than the input: when no candidate beats the original
        // bytes, return the original verbatim.
        if best.len() >= in_bytes {
            best = src.clone();
            best_dedup = 0;
            images_reencoded = 0;
        }
    }

    let out_bytes = best.len();

    // Structural verification: the output must reparse, every reference must
    // resolve, and the input's surveyed counts must hold. Commit is atomic.
    let observed = selis_pdf_cos::verify::survey(&src, &budget, &mut g)
        .map_err(|e| CliError(format!("{path}: input survey failed: {e}")))?;
    let expected = write_gate::expectations_from(&observed, &PRESERVE_ALL);
    let verdict = write_gate::write_verified(&best, output, &expected, &budget, &mut g)?;

    // The output must also expose a usable page tree. A root graph that parses
    // but has no /Pages is not a compressible document; refuse rather than
    // ship a broken file.
    let out_startxref = selis_pdf_cos::xref::find_startxref(&best, 4096).unwrap_or(0);
    let out_doc = selis_pdf_cos::parse_revisions(&best, out_startxref, &budget, &mut g)
        .map_err(|e| CliError(format!("{path}: output does not reparse: {e}")))?;
    crate::tools::page_refs_of(&best, &out_doc, &budget, &mut g)
        .map_err(|e| CliError(format!("{path}: output has no usable page tree: {e}")))?;

    let verification = Verification::from_gate(
        Some(&observed),
        &verdict,
        &expected,
        u64::try_from(in_bytes).unwrap_or(u64::MAX),
        u64::try_from(out_bytes).unwrap_or(u64::MAX),
    );
    Ok((
        CompressionReport {
            objects: object_count,
            deduplicated: best_dedup,
            streams_recoded,
            images_reencoded,
            lossy_passes,
            in_bytes,
            out_bytes,
        },
        verification,
    ))
}

/// Deduplicate a candidate object graph, fold the root and `/Info` references
/// through the dedup remap, and render it to bytes. Returns the bytes and the
/// number of duplicates removed.
fn render_candidate(
    objects: Vec<(u32, Obj)>,
    new_root: u32,
    info_num: Option<u32>,
    path: &str,
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
) -> CliResult<(Vec<u8>, usize)> {
    let (objects, dedup_remap, deduplicated) = dedup_objects(objects, budget, g);
    let root_num = dedup_remap.get(&new_root).copied().unwrap_or(new_root);
    let root_ref = Ref::new(root_num, 0);
    let mut trailer = Vec::new();
    if let Some(num) = info_num {
        let num = dedup_remap.get(&num).copied().unwrap_or(num);
        trailer.push((b"Info".to_vec(), Obj::Ref(Ref::new(num, 0))));
    }
    let bytes = write_objects_as_document_with_trailer(&objects, root_ref, &trailer, budget, g)
        .map_err(|e| CliError(format!("{path}: write: {e}")))?;
    Ok((bytes, deduplicated))
}

/// Re-encode one stream in place when it loses weight. Returns whether the
/// stream was changed. Non-stream objects are untouched.
fn recode_stream(obj: &mut Obj, _budget: &Budget, g: &mut BudgetGuard<'_>) -> bool {
    let Obj::Stream { dict, data } = obj else {
        return false;
    };
    let filter = dict
        .iter()
        .find(|(k, _)| k.as_slice() == b"Filter")
        .map(|(_, v)| v);
    match filter {
        // No filter: add FlateDecode when it shrinks the payload.
        None => {
            if data.is_empty() {
                return false;
            }
            let encoded = selis_pdf_filter::flate_encode(data, RECODE_LEVEL);
            if encoded.len() >= data.len() {
                return false;
            }
            *data = selis_bytes::Bytes::from(encoded);
            set_key(
                dict,
                b"Length",
                Obj::Int(i64::try_from(data.len()).unwrap_or(i64::MAX)),
            );
            set_key(
                dict,
                b"Filter",
                Obj::Name(selis_bytes::Bytes::copy_from_slice(b"FlateDecode")),
            );
            true
        }
        // A single FlateDecode filter: re-encode at the top level if smaller.
        Some(Obj::Name(n)) if n.as_slice() == b"FlateDecode" || n.as_slice() == b"Fl" => {
            let Ok(decoded) = selis_pdf_filter::flate_decode_bounded(data, RECODE_LIMIT, g) else {
                return false;
            };
            let encoded = selis_pdf_filter::flate_encode(&decoded, RECODE_LEVEL);
            if encoded.len() >= data.len() {
                return false;
            }
            *data = selis_bytes::Bytes::from(encoded);
            set_key(
                dict,
                b"Length",
                Obj::Int(i64::try_from(data.len()).unwrap_or(i64::MAX)),
            );
            true
        }
        // Any other filter (DCT, CCITT, LZW, arrays): leave untouched.
        Some(_) => false,
    }
}

/// Insert or replace the pair whose key matches `key`.
fn set_key(dict: &mut Vec<(selis_bytes::Bytes, Obj)>, key: &[u8], value: Obj) {
    let key_b = selis_bytes::Bytes::copy_from_slice(key);
    if let Some(slot) = dict.iter_mut().find(|(k, _)| k.as_slice() == key) {
        slot.1 = value;
    } else {
        dict.push((key_b, value));
    }
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::indexing_slicing,
        clippy::arithmetic_side_effects,
        clippy::cast_possible_truncation,
        clippy::panic
    )]

    use super::*;

    fn temp_dir(name: &str) -> std::path::PathBuf {
        let dir =
            std::env::temp_dir().join(format!("selis-compress-{}-{name}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).expect("create temp dir");
        dir
    }

    /// A single-page PDF with an uncompressed, highly compressible stream and
    /// an Info dictionary.
    fn plain_pdf() -> Vec<u8> {
        let budget = Budget::unlimited();
        let mut g = budget.guard();
        let mut builder = selis_pdf_cos::doc_writer::DocumentBuilder::new();
        let content = b"q 1 0 0 1 0 0 cm Q\n".repeat(200);
        builder.add_page(200.0, 300.0, &content);
        builder.set_info(b"Title", "Kept");
        builder.write(&budget, &mut g).expect("write fixture")
    }

    #[test]
    fn compression_shrinks_a_compressible_stream_and_keeps_metadata() {
        let dir = temp_dir("plain");
        let input = dir.join("in.pdf");
        let output = dir.join("out.pdf");
        std::fs::write(&input, plain_pdf()).expect("write input");
        let (report, verification) =
            compress_file(input.to_str().unwrap(), output.to_str().unwrap()).expect("compress");
        assert!(report.streams_recoded >= 1);
        assert!(report.out_bytes < report.in_bytes);
        assert_eq!(verification.verdict, "ok");
        assert_eq!(verification.pages_out, 1);
        let bytes = std::fs::read(&output).expect("read output");
        let text = String::from_utf8_lossy(&bytes);
        assert!(text.contains("/Info"), "Info reference is carried");
        assert!(text.contains("(Kept)"), "metadata value survived");
        assert!(text.contains("FlateDecode"), "stream was re-encoded");
    }

    /// A single-page PDF whose `/Info` lives only in the trailer, as older
    /// producers write it (newer writers often put `/Info` on the catalog).
    fn trailer_info_pdf() -> Vec<u8> {
        let budget = Budget::unlimited();
        let mut g = budget.guard();
        let name = |value: &[u8]| Obj::Name(selis_bytes::Bytes::copy_from_slice(value));
        let text = |value: &[u8]| Obj::String(selis_bytes::Bytes::copy_from_slice(value));
        let content = b"q 1 0 0 1 0 0 cm Q\n".repeat(200);
        let objects = vec![
            (
                1u32,
                Obj::Dict(vec![
                    (
                        selis_bytes::Bytes::copy_from_slice(b"Type"),
                        name(b"Catalog"),
                    ),
                    (
                        selis_bytes::Bytes::copy_from_slice(b"Pages"),
                        Obj::Ref(Ref::new(2, 0)),
                    ),
                ]),
            ),
            (
                2u32,
                Obj::Dict(vec![
                    (selis_bytes::Bytes::copy_from_slice(b"Type"), name(b"Pages")),
                    (
                        selis_bytes::Bytes::copy_from_slice(b"Kids"),
                        Obj::Array(vec![Obj::Ref(Ref::new(3, 0))]),
                    ),
                    (selis_bytes::Bytes::copy_from_slice(b"Count"), Obj::Int(1)),
                ]),
            ),
            (
                3u32,
                Obj::Dict(vec![
                    (selis_bytes::Bytes::copy_from_slice(b"Type"), name(b"Page")),
                    (
                        selis_bytes::Bytes::copy_from_slice(b"Parent"),
                        Obj::Ref(Ref::new(2, 0)),
                    ),
                    (
                        selis_bytes::Bytes::copy_from_slice(b"MediaBox"),
                        Obj::Array(vec![Obj::Int(0), Obj::Int(0), Obj::Int(200), Obj::Int(300)]),
                    ),
                    (
                        selis_bytes::Bytes::copy_from_slice(b"Contents"),
                        Obj::Ref(Ref::new(4, 0)),
                    ),
                ]),
            ),
            (
                4u32,
                Obj::Stream {
                    dict: vec![(
                        selis_bytes::Bytes::copy_from_slice(b"Length"),
                        Obj::Int(i64::try_from(content.len()).unwrap_or(i64::MAX)),
                    )],
                    data: selis_bytes::Bytes::from(content),
                },
            ),
            (
                5u32,
                Obj::Dict(vec![(
                    selis_bytes::Bytes::copy_from_slice(b"Title"),
                    text(b"Trailer Kept"),
                )]),
            ),
        ];
        selis_pdf_cos::doc_writer::write_objects_as_document_with_trailer(
            &objects,
            Ref::new(1, 0),
            &[(b"Info".to_vec(), Obj::Ref(Ref::new(5, 0)))],
            &budget,
            &mut g,
        )
        .expect("write trailer-info fixture")
    }

    #[test]
    fn trailer_level_info_is_preserved() {
        let dir = temp_dir("trailer-info");
        let input = dir.join("in.pdf");
        let output = dir.join("out.pdf");
        std::fs::write(&input, trailer_info_pdf()).expect("write input");
        let (report, verification) =
            compress_file(input.to_str().unwrap(), output.to_str().unwrap()).expect("compress");
        assert_eq!(verification.verdict, "ok");
        assert!(report.streams_recoded >= 1);

        let bytes = std::fs::read(&output).expect("read output");
        let budget = Budget::unlimited();
        let mut g = budget.guard();
        let sx = selis_pdf_cos::xref::find_startxref(&bytes, 4096).unwrap_or(0);
        let doc = selis_pdf_cos::parse_revisions(&bytes, sx, &budget, &mut g).expect("parse out");
        let rev = doc.revisions().last().expect("revision");
        let info_ref = rev
            .trailer
            .iter()
            .find(|(key, _)| key.as_slice() == b"Info")
            .and_then(|(_, value)| match value {
                Obj::Ref(reference) => Some(*reference),
                _ => None,
            })
            .expect("output trailer keeps /Info");
        let info = selis_pdf_cos::copy::resolve_ref(&bytes, &doc, info_ref, &budget, &mut g)
            .expect("resolve /Info");
        let Obj::Dict(pairs) = info else {
            panic!("/Info should be a dictionary");
        };
        let title = pairs
            .iter()
            .find(|(key, _)| key.as_slice() == b"Title")
            .and_then(|(_, value)| match value {
                Obj::String(value) => Some(value.as_slice()),
                _ => None,
            })
            .expect("/Info keeps its title");
        assert_eq!(title, b"Trailer Kept");
    }

    #[test]
    fn encrypted_documents_are_refused_without_writing() {
        let dir = temp_dir("encrypted");
        let input = dir.join("in.pdf");
        let output = dir.join("out.pdf");
        let mut bytes = plain_pdf();
        // Add an /Encrypt reference to the trailer. The document still parses;
        // compression refuses before touching the output.
        let trailer_at = bytes
            .windows(7)
            .position(|window| window == b"trailer")
            .expect("trailer");
        let insert_at = trailer_at + b"trailer\n<<".len();
        bytes.splice(insert_at..insert_at, b" /Encrypt 99 0 R".iter().copied());
        std::fs::write(&input, &bytes).expect("write input");
        // The xref offsets are now stale; the parser may still recover, but
        // the operation must not silently produce an unencrypted file.
        let result = compress_file(input.to_str().unwrap(), output.to_str().unwrap());
        assert!(result.is_err());
        assert!(!output.exists());
    }

    /// Deterministic photographic-style noise (an LCG over the sample index)
    /// that Flate cannot shrink but a low-quality JPEG can — the scanned-page
    /// case the lossy tier exists for.
    fn noise_rgb(width: usize, height: usize) -> Vec<u8> {
        let count = width * height * 3;
        let mut out = Vec::with_capacity(count);
        let mut state = 0x1234_5678u32;
        for _ in 0..count {
            state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            out.push((state >> 24) as u8);
        }
        out
    }

    /// A single-page scanned-style PDF: a raw RGB noise image XObject, a
    /// content stream that paints it, and one text annotation so the
    /// preservation set is exercised on a non-zero count.
    fn scanned_pdf() -> Vec<u8> {
        let budget = Budget::unlimited();
        let mut g = budget.guard();
        let name = |v: &[u8]| Obj::Name(selis_bytes::Bytes::copy_from_slice(v));
        let key = |v: &[u8]| selis_bytes::Bytes::copy_from_slice(v);
        let samples = noise_rgb(200, 150);
        let content = b"q 200 0 0 150 0 0 cm /Im0 Do Q\n";
        let objects = vec![
            (
                1u32,
                Obj::Dict(vec![
                    (key(b"Type"), name(b"Catalog")),
                    (key(b"Pages"), Obj::Ref(Ref::new(2, 0))),
                ]),
            ),
            (
                2u32,
                Obj::Dict(vec![
                    (key(b"Type"), name(b"Pages")),
                    (key(b"Kids"), Obj::Array(vec![Obj::Ref(Ref::new(3, 0))])),
                    (key(b"Count"), Obj::Int(1)),
                ]),
            ),
            (
                3u32,
                Obj::Dict(vec![
                    (key(b"Type"), name(b"Page")),
                    (key(b"Parent"), Obj::Ref(Ref::new(2, 0))),
                    (
                        key(b"MediaBox"),
                        Obj::Array(vec![Obj::Int(0), Obj::Int(0), Obj::Int(200), Obj::Int(150)]),
                    ),
                    (
                        key(b"Resources"),
                        Obj::Dict(vec![(
                            key(b"XObject"),
                            Obj::Dict(vec![(key(b"Im0"), Obj::Ref(Ref::new(5, 0)))]),
                        )]),
                    ),
                    (key(b"Contents"), Obj::Ref(Ref::new(4, 0))),
                    (key(b"Annots"), Obj::Array(vec![Obj::Ref(Ref::new(6, 0))])),
                ]),
            ),
            (
                4u32,
                Obj::Stream {
                    dict: vec![(
                        key(b"Length"),
                        Obj::Int(i64::try_from(content.len()).unwrap_or(i64::MAX)),
                    )],
                    data: selis_bytes::Bytes::copy_from_slice(content),
                },
            ),
            (
                5u32,
                Obj::Stream {
                    dict: vec![
                        (key(b"Type"), name(b"XObject")),
                        (key(b"Subtype"), name(b"Image")),
                        (key(b"Width"), Obj::Int(200)),
                        (key(b"Height"), Obj::Int(150)),
                        (key(b"ColorSpace"), name(b"DeviceRGB")),
                        (key(b"BitsPerComponent"), Obj::Int(8)),
                        (
                            key(b"Length"),
                            Obj::Int(i64::try_from(samples.len()).unwrap_or(i64::MAX)),
                        ),
                    ],
                    data: selis_bytes::Bytes::from(samples),
                },
            ),
            (
                6u32,
                Obj::Dict(vec![
                    (key(b"Type"), name(b"Annot")),
                    (key(b"Subtype"), name(b"Text")),
                    (
                        key(b"Rect"),
                        Obj::Array(vec![Obj::Int(0), Obj::Int(0), Obj::Int(10), Obj::Int(10)]),
                    ),
                    (
                        key(b"Contents"),
                        Obj::String(selis_bytes::Bytes::copy_from_slice(b"note")),
                    ),
                ]),
            ),
        ];
        selis_pdf_cos::doc_writer::write_objects_as_document_with_trailer(
            &objects,
            Ref::new(1, 0),
            &[],
            &budget,
            &mut g,
        )
        .expect("write scanned fixture")
    }

    /// The lossy tier re-encodes the scanned image as JPEG, shrinks the file,
    /// and preserves the page and its annotation.
    #[test]
    fn lossy_tier_shrinks_a_scanned_style_fixture() {
        let dir = temp_dir("lossy-scan");
        let input = dir.join("in.pdf");
        let output = dir.join("out.pdf");
        std::fs::write(&input, scanned_pdf()).expect("write input");
        let options = CompressOptions {
            level: Some(CompressionLevel::Recommended),
            target_kib: None,
        };
        let (report, verification) =
            compress_file_with_options(input.to_str().unwrap(), output.to_str().unwrap(), &options)
                .expect("compress");
        assert!(report.images_reencoded >= 1, "the image was re-encoded");
        assert!(report.lossy_passes >= 1);
        assert!(
            report.out_bytes < report.in_bytes,
            "lossy output is smaller"
        );
        assert_eq!(verification.verdict, "ok");
        assert_eq!(verification.pages_out, 1);
        assert_eq!(verification.annotations_out, 1, "annotation preserved");
        let bytes = std::fs::read(&output).expect("read output");
        let text = String::from_utf8_lossy(&bytes);
        assert!(text.contains("DCTDecode"), "image became a JPEG stream");
        assert!(text.contains("(note)"), "annotation content survived");
    }

    /// A reachable target is met within the bounded pass count.
    #[test]
    fn lossy_target_is_met_when_reachable() {
        let dir = temp_dir("lossy-target");
        let input = dir.join("in.pdf");
        let output = dir.join("out.pdf");
        std::fs::write(&input, scanned_pdf()).expect("write input");
        let options = CompressOptions {
            level: Some(CompressionLevel::Recommended),
            target_kib: Some(50),
        };
        let (report, _) =
            compress_file_with_options(input.to_str().unwrap(), output.to_str().unwrap(), &options)
                .expect("compress");
        assert!(
            report.out_bytes <= 50 * 1024,
            "output {} fits the 50 KiB target",
            report.out_bytes
        );
        assert!(report.lossy_passes <= MAX_PASSES, "search stayed bounded");
    }

    /// An unreachable target still returns a smaller file (never larger than
    /// the input) and runs the full bounded search rather than looping.
    #[test]
    fn unreachable_target_stays_bounded_and_never_grows() {
        let dir = temp_dir("lossy-unreachable");
        let input = dir.join("in.pdf");
        let output = dir.join("out.pdf");
        std::fs::write(&input, scanned_pdf()).expect("write input");
        let options = CompressOptions {
            level: Some(CompressionLevel::Extreme),
            target_kib: Some(1),
        };
        let (report, verification) =
            compress_file_with_options(input.to_str().unwrap(), output.to_str().unwrap(), &options)
                .expect("compress");
        assert_eq!(report.lossy_passes, MAX_PASSES, "ran the bounded search");
        assert!(report.out_bytes > 1024, "the 1 KiB target was not met");
        assert!(
            report.out_bytes < report.in_bytes,
            "still smaller than input"
        );
        assert_eq!(verification.verdict, "ok");
    }

    /// The never-larger guarantee holds for a text-only document too: with no
    /// eligible image the lossy tier falls back to the lossless floor, and an
    /// already-optimised input is never grown by a second rewrite.
    #[test]
    fn lossy_never_returns_a_larger_file() {
        let dir = temp_dir("lossy-never-larger");
        let input = dir.join("in.pdf");
        let output = dir.join("out.pdf");
        std::fs::write(&input, plain_pdf()).expect("write input");
        let options = CompressOptions {
            level: Some(CompressionLevel::Extreme),
            target_kib: None,
        };
        let (report, verification) =
            compress_file_with_options(input.to_str().unwrap(), output.to_str().unwrap(), &options)
                .expect("compress");
        assert_eq!(report.images_reencoded, 0, "no image to re-encode");
        assert!(
            report.out_bytes <= report.in_bytes,
            "never larger than input"
        );
        assert_eq!(verification.verdict, "ok");

        // Re-compressing the already-optimised output must not grow it.
        let again = dir.join("again.pdf");
        let (report2, _) =
            compress_file_with_options(output.to_str().unwrap(), again.to_str().unwrap(), &options)
                .expect("compress again");
        assert!(
            report2.out_bytes <= report2.in_bytes,
            "a second rewrite never grows the file"
        );
    }
}
