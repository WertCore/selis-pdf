//! The `selis compress` tool (SL-1A.TOOL.07): lossless optimisation.
//!
//! Rewrites a document through the full-document writer, keeping every
//! rendered pixel identical while shrinking the file:
//! * **garbage collection** — only objects reachable from `/Root` (and
//!   `/Info`, so metadata survives) are kept;
//! * **stream re-encode** — unfiltered streams gain `FlateDecode`; existing
//!   Flate streams are re-encoded at the highest level when smaller;
//! * **identical-object dedup** — byte-identical objects collapse to one and
//!   every reference is redirected at the survivor.
//!
//! Encrypted documents are refused: removing `/Encrypt` is `SL-1A.TOOL.04`
//! (unlock), a distinct, human-owned operation. The lossless tier never
//! downsamples images or subsets fonts — those land at G2/G3.

use selis_pdf_cos::copy::collect_objects;
use selis_pdf_cos::doc_writer::write_objects_as_document;
use selis_pdf_cos::{parse_revisions, xref, Obj, Ref};
use selis_sandbox::{Budget, BudgetGuard, Surface};

use crate::{read_file, CliError, CliResult};

/// The maximum bytes a single stream may expand to while being re-encoded.
const RECODE_LIMIT: u64 = 256 * 1024 * 1024;

/// The zlib level used when re-encoding streams.
const RECODE_LEVEL: u8 = 10;

/// The optimisation report, printed to stderr.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Report {
    /// Objects reachable from the root (after GC, before dedup).
    pub objects: usize,
    /// Objects removed by identical-object dedup.
    pub deduplicated: usize,
    /// Streams re-encoded (new Flate, or smaller Flate).
    pub streams_recoded: usize,
    /// Input size in bytes.
    pub in_bytes: usize,
    /// Output size in bytes.
    pub out_bytes: usize,
}

impl Report {
    /// The number of bytes saved (`in - out`, saturating).
    #[must_use]
    pub(crate) fn saved(&self) -> usize {
        self.in_bytes.saturating_sub(self.out_bytes)
    }
}

/// Compress `path` into `output` and report the result.
///
/// # Errors
///
/// `IO_READ_FAILED` when the input cannot be read, a typed parse error when
/// the document is damaged, and a clean refusal for encrypted documents.
pub(crate) fn run(path: &str, output: &str) -> CliResult<()> {
    let report = optimise_file(path, output)?;
    eprintln!(
        "compressed {path}: {} -> {} bytes (saved {}), {} objects, {} deduplicated, {} streams re-encoded -> {output}",
        report.in_bytes,
        report.out_bytes,
        report.saved(),
        report.objects,
        report.deduplicated,
        report.streams_recoded
    );
    Ok(())
}

/// Optimise the document at `path`, writing the result to `output`, and
/// return the size/structure report. Batch mode (SL-1A.TOOL.11) calls this
/// per file.
pub(crate) fn optimise_file(path: &str, output: &str) -> CliResult<Report> {
    let src = read_file(path)?;
    let in_bytes = src.len();
    let budget = Budget::unlimited();
    let mut g = budget.guard();

    let startxref = xref::find_startxref(&src, 4096).unwrap_or(0);
    let doc = parse_revisions(&src, startxref, &budget, &mut g)?;
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

    // Garbage-collect: walk from the catalog root (plus /Info when present).
    // `collect_objects` renumbers every reachable object to fresh numbers.
    let mut roots = vec![root];
    if let Some((_, Obj::Ref(info))) = rev.trailer.iter().find(|(k, _)| k.as_slice() == b"Info") {
        roots.push(*info);
    }
    let mut next_num = 1u32;
    let (mut objects, remap) = collect_objects(&src, &roots, &mut next_num, &budget, &mut g)
        .map_err(|e| CliError(format!("{path}: {e}")))?;
    let object_count = objects.len();
    let new_root = remap.get(&root.num).copied().unwrap_or(root.num);

    // Re-encode streams losslessly.
    let mut streams_recoded = 0usize;
    for (_num, obj) in objects.iter_mut() {
        if recode_stream(obj, &budget, &mut g) {
            streams_recoded = streams_recoded.saturating_add(1);
        }
    }

    // Deduplicate byte-identical objects, redirecting refs at the survivor.
    let (objects, deduplicated) = dedup(objects, &budget, &mut g);
    let root_ref = Ref::new(new_root, 0);

    let bytes = write_objects_as_document(&objects, root_ref, &budget, &mut g)
        .map_err(|e| CliError(format!("{path}: write: {e}")))?;
    let out_bytes = bytes.len();

    // Structural verification (WRITE.05 obligation): the output must reparse
    // with the same root reachable.
    let sx = xref::find_startxref(&bytes, 4096).unwrap_or(0);
    let parsed = parse_revisions(&bytes, sx, &budget, &mut g)
        .map_err(|e| CliError(format!("{path}: output failed verification: {e}")))?;
    if parsed.revisions().last().and_then(|r| r.root) != Some(root_ref) {
        return Err(CliError(format!(
            "{path}: output failed verification (/Root not preserved)"
        )));
    }
    // The output must also build a document model — a catalog with a usable
    // page tree. A root graph that parses but has no /Pages (e.g. the input
    // only opens through scan-based recovery) is not a compressible document;
    // refuse rather than ship a broken file (WRITE.07).
    let doc_budget = Budget::profile(selis_sandbox::Surface::Viewer);
    if selis_pdf_engine::Session::open(bytes.clone(), &doc_budget).is_err() {
        return Err(CliError(format!(
            "{path}: output failed verification (no usable document model)"
        )));
    }

    std::fs::write(output, &bytes).map_err(|e| CliError(format!("cannot write {output}: {e}")))?;
    Ok(Report {
        objects: object_count,
        deduplicated,
        streams_recoded,
        in_bytes,
        out_bytes,
    })
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

/// Collapse byte-identical objects to a single representative and redirect
/// every reference at it. Returns the surviving objects (original numbers)
/// and how many duplicates were removed. Delegates to the shared WRITE.03
/// primitive [`selis_pdf_cos::copy::dedup_objects`], which runs to a fixpoint
/// so identical parents whose children merged also collapse.
fn dedup(
    objects: Vec<(u32, Obj)>,
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
) -> (Vec<(u32, Obj)>, usize) {
    let (survivors, _remap, removed) = selis_pdf_cos::copy::dedup_objects(objects, budget, g);
    (survivors, removed)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::indexing_slicing)]

    use super::*;

    /// Re-encoding an unfiltered stream adds FlateDecode and shrinks it.
    #[test]
    fn unfiltered_stream_gains_flate() {
        let budget = Budget::unlimited();
        let mut g = budget.guard();
        let raw = b"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".repeat(32);
        let mut obj = Obj::Stream {
            dict: vec![],
            data: selis_bytes::Bytes::copy_from_slice(&raw),
        };
        assert!(recode_stream(&mut obj, &budget, &mut g));
        let Obj::Stream { dict, data } = &obj else {
            panic!("still a stream");
        };
        assert!(dict.iter().any(|(k, _)| k.as_slice() == b"Filter"));
        assert!(data.len() < raw.len(), "re-encoded stream is smaller");
        // The payload decodes back to the exact original bytes (lossless).
        let decoded = selis_pdf_filter::flate_decode(data).expect("decode");
        assert_eq!(decoded, raw);
    }

    /// A DCT (JPEG) filter is never touched by the lossless tier.
    #[test]
    fn dct_streams_are_left_alone() {
        let budget = Budget::unlimited();
        let mut g = budget.guard();
        let orig = vec![0xFFu8, 0xD8, 0xFF, 0xD9];
        let mut obj = Obj::Stream {
            dict: vec![(
                selis_bytes::Bytes::copy_from_slice(b"Filter"),
                Obj::Name(selis_bytes::Bytes::copy_from_slice(b"DCTDecode")),
            )],
            data: selis_bytes::Bytes::from(orig.clone()),
        };
        assert!(!recode_stream(&mut obj, &budget, &mut g));
        if let Obj::Stream { data, .. } = &obj {
            assert_eq!(data.as_slice(), orig.as_slice(), "payload untouched");
        }
    }

    /// Byte-identical objects collapse to one; refs redirect at the survivor.
    #[test]
    fn identical_objects_deduplicate() {
        let budget = Budget::unlimited();
        let mut g = budget.guard();
        let font = || {
            Obj::Dict(vec![(
                selis_bytes::Bytes::copy_from_slice(b"BaseFont"),
                Obj::Name(selis_bytes::Bytes::copy_from_slice(b"Helvetica")),
            )])
        };
        let objects = vec![
            (3u32, font()),
            (4u32, font()),
            (
                5u32,
                Obj::Dict(vec![
                    (
                        selis_bytes::Bytes::copy_from_slice(b"F1"),
                        Obj::Ref(Ref::new(3, 0)),
                    ),
                    (
                        selis_bytes::Bytes::copy_from_slice(b"F2"),
                        Obj::Ref(Ref::new(4, 0)),
                    ),
                ]),
            ),
        ];
        let (survivors, removed) = dedup(objects, &budget, &mut g);
        assert_eq!(removed, 1, "one duplicate removed");
        assert_eq!(survivors.len(), 2);
        // The resource dict's F2 now points at the surviving font (3).
        let resources = survivors
            .iter()
            .find(|(num, _)| *num == 5)
            .map(|(_, o)| o)
            .expect("resources survive");
        let Obj::Dict(pairs) = resources else {
            panic!("dict");
        };
        for (_, v) in pairs {
            assert_eq!(v, &Obj::Ref(Ref::new(3, 0)), "refs redirect at survivor");
        }
    }

    /// Build a minimal single-page PDF. When `encrypt` is true the trailer
    /// carries an `/Encrypt` reference. The xref offsets are computed so the
    /// bytes reparse cleanly.
    fn build_pdf(encrypt: bool) -> Vec<u8> {
        let objs = [
            (1u32, b"<< /Type /Catalog /Pages 2 0 R >>" as &[u8]),
            (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
            (
                3,
                b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] >>",
            ),
            (4, b"<< /Filter /Standard /V 1 /R 2 /Length 40 >>"),
        ];
        let mut out = Vec::new();
        out.extend_from_slice(b"%PDF-1.4\n");
        let mut offsets = std::collections::HashMap::new();
        for (num, body) in &objs {
            offsets.insert(*num, out.len());
            out.extend_from_slice(format!("{num} 0 obj\n").as_bytes());
            out.extend_from_slice(body);
            out.extend_from_slice(b"\nendobj\n");
        }
        let xref_at = out.len();
        let count = objs.len() + 1;
        out.extend_from_slice(format!("xref\n0 {count}\n").as_bytes());
        out.extend_from_slice(b"0000000000 65535 f \n");
        for i in 1..u32::try_from(count).expect("fits u32") {
            let off = offsets[&i];
            out.extend_from_slice(format!("{off:010} 00000 n \n").as_bytes());
        }
        out.extend_from_slice(b"trailer\n");
        if encrypt {
            out.extend_from_slice(b"<< /Size 5 /Root 1 0 R /Encrypt 4 0 R >>\n");
        } else {
            out.extend_from_slice(b"<< /Size 5 /Root 1 0 R >>\n");
        }
        out.extend_from_slice(format!("startxref\n{xref_at}\n%%EOF\n").as_bytes());
        out
    }

    /// An encrypted document is refused by the lossless tier (unlock is the
    /// separate human-owned SL-1A.TOOL.04).
    #[test]
    fn encrypted_document_is_refused() {
        let dir = std::env::temp_dir().join("selis-compress-test");
        std::fs::create_dir_all(&dir).unwrap();
        let in_path = dir.join("enc.pdf");
        let out_path = dir.join("enc.out.pdf");
        std::fs::write(&in_path, build_pdf(true)).unwrap();
        let err = super::optimise_file(in_path.to_str().unwrap(), out_path.to_str().unwrap())
            .expect_err("encrypted refused");
        assert!(
            err.to_string().contains("encrypted"),
            "message names encryption: {err}"
        );
        assert!(!out_path.exists(), "no output written on refusal");
    }

    /// An unencrypted document round-trips and keeps its single page.
    #[test]
    fn unencrypted_document_compresses() {
        let dir = std::env::temp_dir().join("selis-compress-test");
        std::fs::create_dir_all(&dir).unwrap();
        let in_path = dir.join("plain.pdf");
        let out_path = dir.join("plain.out.pdf");
        std::fs::write(&in_path, build_pdf(false)).unwrap();
        let report = super::optimise_file(in_path.to_str().unwrap(), out_path.to_str().unwrap())
            .expect("compress");
        assert!(report.objects >= 3, "kept the reachable objects");
        assert!(out_path.exists(), "output written");
    }
}
