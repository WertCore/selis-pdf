//! The `selis unlock` tool (SL-1A.TOOL.04): remove password protection.
//!
//! Opens an encrypted document, decrypts every stream and string via the
//! engine's decryption pipeline, and writes an unencrypted clean copy. This is
//! a full rewrite (WRITE.01), not an incremental append — the encrypted bytes
//! must not survive in the output. Aspose.PDF, Adobe Acrobat, and Foxit all
//! follow the same pattern: authenticate the password, derive the file key,
//! decrypt every stream/string with that key, drop `/Encrypt` from the trailer,
//! and emit a fresh single-revision document.

use std::collections::BTreeSet;

use selis_pdf_cos::doc_writer::write_objects_as_document_with_trailer;
use selis_pdf_cos::encrypt::{self, DecryptPolicy};
use selis_pdf_cos::{parse_revisions, xref, Obj, Ref};
use selis_pdf_doc::Resolver;
use selis_sandbox::{Budget, Surface};

use crate::{read_file, CliError, CliResult};

/// Run the unlock tool: decrypt `path` and write the result to `output`.
///
/// `password` defaults to the empty string (the user password of many
/// encrypted documents that require no password to open). A wrong password
/// returns a clean `WRONG_PASSWORD` typed error.
///
/// # Errors
///
/// `IO_READ_FAILED` when the input cannot be read, a typed parse error when
/// the document is damaged, and `WRONG_PASSWORD` when the password does not
/// match the document's user or owner password.
pub(crate) fn run(path: &str, output: &str, password: Option<&str>) -> CliResult<()> {
    let report = unlock_file(path, output, password)?;
    if report.encrypted {
        eprintln!("unlocked {path} -> {output}");
    } else {
        eprintln!("{path}: not encrypted; copied as-is to {output}");
    }
    Ok(())
}

/// The outcome of unlocking a file.
struct UnlockReport {
    /// Whether the original file was encrypted.
    encrypted: bool,
}

/// Core unlock logic: read, decrypt, write, verify.
fn unlock_file(path: &str, output: &str, password: Option<&str>) -> CliResult<UnlockReport> {
    let src = read_file(path)?;
    let budget = Budget::unlimited();
    let mut g = budget.guard();

    let startxref = xref::find_startxref(&src, 4096).unwrap_or(0);
    let doc = parse_revisions(&src, startxref, &budget, &mut g)
        .map_err(|e| CliError(format!("{path}: {e}")))?;
    let rev = doc
        .revisions()
        .last()
        .ok_or_else(|| CliError(format!("{path}: no revisions")))?;

    let policy = match rev.encrypt {
        None => {
            // Not encrypted: copy as-is (verbatim, atomic — the bytes are the
            // user's original file, not writer output to verify).
            crate::write_gate::atomic_write(&src, output)?;
            return Ok(UnlockReport { encrypted: false });
        }
        Some(r) => {
            let info = encrypt::parse_encrypt(&src, Some(r), &budget, &mut g)
                .map_err(|e| CliError(format!("{path}: {e}")))?
                .ok_or_else(|| {
                    CliError(format!(
                        "{path}: unsupported or unreadable /Encrypt dictionary"
                    ))
                })?;
            let id = encrypt::document_id(&rev.trailer);
            let key = encrypt::authenticate(&info, &id, password.unwrap_or("").as_bytes())
                .ok_or_else(|| CliError(format!("{path}: wrong password (WRONG_PASSWORD)")))?;
            DecryptPolicy::from_encrypt(&info, key)
        }
    };

    let root = rev
        .root
        .ok_or_else(|| CliError(format!("{path}: no /Root in trailer")))?;

    // TOOL.04 engineering note: a full rewrite invalidates digital
    // signatures — detect before running and tell the user.
    crate::tools::warn_if_digital_signature(&src, &doc, &budget, &mut g, path);

    // Walk the object graph from /Root (and /Info, so metadata survives),
    // resolving with automatic decryption.
    let mut resolver = Resolver::new(&doc, &src, &budget);
    resolver.set_key(policy);

    let mut objects: Vec<(u32, Obj)> = Vec::new();
    let mut visited = BTreeSet::new();
    walk(&mut resolver, root, &mut visited, &mut objects, &mut g)
        .map_err(|e| CliError(format!("{path}: {e}")))?;

    // /Info is not reachable from /Root (it is a trailer entry), so walk it
    // separately and add an /Info trailer entry.
    let mut extra_trailer: Vec<(Vec<u8>, Obj)> = Vec::new();
    if let Some((_, Obj::Ref(info))) = rev.trailer.iter().find(|(k, _)| k.as_slice() == b"Info") {
        if !visited.contains(&info.num) {
            walk(&mut resolver, *info, &mut visited, &mut objects, &mut g)
                .map_err(|e| CliError(format!("{path}: {e}")))?;
        }
        extra_trailer.push((b"Info".to_vec(), Obj::Ref(*info)));
    }
    // /ID: the first element is the permanent identifier (carried over); the
    // second must change when a file is modified (ISO 32000-1 §14.4), so a
    // fresh deterministic value is derived from the source contents — the
    // same FNV-2x64 pattern the merge tool uses (WRITE.04).
    let old_id: Option<Vec<u8>> = rev
        .trailer
        .iter()
        .find(|(k, _)| k.as_slice() == b"ID")
        .and_then(|(_, v)| match v {
            Obj::Array(items) => items.first().and_then(|x| match x {
                Obj::String(s) => Some(s.as_slice().to_vec()),
                _ => None,
            }),
            _ => None,
        });
    let fresh = fresh_file_id(&src);
    match old_id {
        Some(id0) => extra_trailer.push((
            b"ID".to_vec(),
            Obj::Array(vec![
                Obj::String(selis_bytes::Bytes::copy_from_slice(&id0)),
                Obj::String(selis_bytes::Bytes::copy_from_slice(&fresh)),
            ]),
        )),
        None => extra_trailer.push((
            b"ID".to_vec(),
            Obj::Array(vec![
                Obj::String(selis_bytes::Bytes::copy_from_slice(&fresh)),
                Obj::String(selis_bytes::Bytes::copy_from_slice(&fresh)),
            ]),
        )),
    }

    // The /Encrypt entry is omitted from the new trailer because
    // write_objects_as_document_with_trailer only emits /Size, /Root, + extra.
    let bytes =
        write_objects_as_document_with_trailer(&objects, root, &extra_trailer, &budget, &mut g)
            .map_err(|e| CliError(format!("{path}: write: {e}")))?;

    // Structural verification (WRITE.05, via the shared gate): the output
    // must reparse, every reference must resolve, and the page count must
    // match the input's. Commit is atomic (temp + fsync + rename).
    let observed = selis_pdf_cos::verify::survey(&src, &budget, &mut g)
        .map_err(|e| CliError(format!("{path}: input survey failed: {e}")))?;
    crate::write_gate::write_verified(
        &bytes,
        output,
        &selis_pdf_cos::verify::Expectations {
            pages: Some(observed.pages),
            ..selis_pdf_cos::verify::Expectations::none()
        },
        &budget,
        &mut g,
    )?;
    // The decrypted output must carry no /Encrypt and must build a usable
    // document model (WRITE.07).
    let sx = xref::find_startxref(&bytes, 4096).unwrap_or(0);
    let parsed = parse_revisions(&bytes, sx, &budget, &mut g)
        .map_err(|e| CliError(format!("{path}: output failed verification: {e}")))?;
    let out_rev = parsed
        .revisions()
        .last()
        .ok_or_else(|| CliError(format!("{path}: output has no revisions")))?;
    if out_rev.encrypt.is_some() {
        return Err(CliError(format!(
            "{path}: output failed verification (/Encrypt still present)"
        )));
    }
    // The output must also build a usable document model (WRITE.07).
    let doc_budget = Budget::profile(Surface::Viewer);
    let clock = crate::shell_clock();
    if selis_pdf_engine::Session::open(bytes.clone(), &doc_budget, &clock).is_err() {
        return Err(CliError(format!(
            "{path}: output failed verification (no usable document model)"
        )));
    }

    Ok(UnlockReport { encrypted: true })
}

/// Walk the object graph from `root`, resolving with decryption, collecting
/// (num, Obj) pairs with original object numbers preserved.
fn walk(
    resolver: &mut Resolver<'_>,
    root: Ref,
    visited: &mut BTreeSet<u32>,
    objects: &mut Vec<(u32, Obj)>,
    g: &mut selis_sandbox::BudgetGuard<'_>,
) -> Result<(), selis_error::Error> {
    if !visited.insert(root.num) {
        return Ok(());
    }
    let obj = resolver.resolve(root, g)?;
    let mut pending = Vec::new();
    collect_refs(&obj, &mut pending);
    objects.push((root.num, obj));
    for next in pending {
        walk(resolver, next, visited, objects, g)?;
    }
    Ok(())
}

/// Collect all `Ref` values from an object (used to walk the object graph).
fn collect_refs(obj: &Obj, out: &mut Vec<Ref>) {
    match obj {
        Obj::Ref(r) => out.push(*r),
        Obj::Array(items) => {
            for item in items {
                collect_refs(item, out);
            }
        }
        Obj::Dict(pairs) => {
            for (_, v) in pairs {
                collect_refs(v, out);
            }
        }
        Obj::Stream { dict, data: _ } => {
            for (_, v) in dict {
                collect_refs(v, out);
            }
        }
        _ => {}
    }
}

/// A deterministic 16-byte file identifier from the source contents (the
/// same FNV-2x64 construction the merge tool uses for a fresh `/ID`).
fn fresh_file_id(src: &[u8]) -> Vec<u8> {
    fn fnv(data: &[u8], seed: u64) -> u64 {
        let mut h = seed ^ 0xcbf2_9ce4_8422_2325;
        for &b in data {
            h ^= u64::from(b);
            h = h.wrapping_mul(0x0000_0100_0000_01b3);
        }
        h
    }
    let blob = src.get(..src.len().min(1 << 20)).unwrap_or(src);
    let mut id = vec![0u8; 16];
    if let Some(slot) = id.get_mut(..8) {
        slot.copy_from_slice(&fnv(blob, 1).to_le_bytes());
    }
    if let Some(slot) = id.get_mut(8..) {
        slot.copy_from_slice(&fnv(blob, 2).to_le_bytes());
    }
    id
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::indexing_slicing)]

    /// Build a minimal single-page PDF. When `encrypt` is true the trailer
    /// carries an `/Encrypt` reference to object 4. The xref offsets are
    /// computed so the bytes reparse cleanly.
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

    /// An encrypted document with an unsupported handler is refused.
    #[test]
    fn encrypted_with_unsupported_handler_is_refused() {
        let dir = std::env::temp_dir().join("selis-unlock-test");
        std::fs::create_dir_all(&dir).unwrap();
        let in_path = dir.join("unsupported.pdf");
        let out_path = dir.join("unsupported.out.pdf");
        std::fs::write(&in_path, build_pdf(true)).unwrap();
        let err = super::run(in_path.to_str().unwrap(), out_path.to_str().unwrap(), None)
            .expect_err("unsupported handler refused");
        assert!(
            err.to_string().contains("unsupported"),
            "message mentions unsupported: {err}"
        );
    }

    /// An unencrypted document is copied as-is.
    #[test]
    fn unencrypted_document_is_copied_as_is() {
        let dir = std::env::temp_dir().join("selis-unlock-test");
        std::fs::create_dir_all(&dir).unwrap();
        let in_path = dir.join("plain.pdf");
        let out_path = dir.join("plain.out.pdf");
        let src = build_pdf(false);
        std::fs::write(&in_path, &src).unwrap();
        super::run(in_path.to_str().unwrap(), out_path.to_str().unwrap(), None)
            .expect("copy as-is");
        let copied = std::fs::read(&out_path).unwrap();
        assert_eq!(copied, src, "bytes must be identical for unencrypted input");
    }

    /// Real encrypted document with empty password is unlocked successfully.
    #[test]
    fn real_encrypted_document_is_unlocked() {
        let corpus = "D:\\selis\\corpus\\pdfs\\bug900822.pdf";
        if !std::path::Path::new(corpus).exists() {
            eprintln!("skipping: {corpus} not found");
            return;
        }
        let dir = std::env::temp_dir().join("selis-unlock-test");
        std::fs::create_dir_all(&dir).unwrap();
        let out_path = dir.join("bug900822_unlocked.pdf");
        super::run(corpus, out_path.to_str().unwrap(), None).expect("unlock");

        let out_bytes = std::fs::read(&out_path).unwrap();
        let tail = String::from_utf8_lossy(
            &out_bytes[out_bytes.len().saturating_sub(4096).min(out_bytes.len())..],
        );
        assert!(
            !tail.contains("/Encrypt"),
            "output must not have /Encrypt trailer entry"
        );
        assert!(
            out_bytes.starts_with(b"%PDF-"),
            "output must start with PDF header"
        );
        // Verify the output opens with the engine.
        let budget = selis_sandbox::Budget::profile(selis_sandbox::Surface::Viewer);
        assert!(
            selis_pdf_engine::Session::open(out_bytes, &budget, &crate::shell_clock()).is_ok(),
            "output must open in the engine"
        );
    }

    /// A non-conformant/broken encrypted file (V=4 + RC4 + /Length 40, which
    /// is invalid per spec) must produce a clean error, not a crash.
    #[test]
    fn broken_encrypted_file_errors_cleanly() {
        let corpus = "D:\\selis\\corpus\\pdfs\\issue19484_1.pdf";
        if !std::path::Path::new(corpus).exists() {
            eprintln!("skipping: {corpus} not found");
            return;
        }
        let dir = std::env::temp_dir().join("selis-unlock-test");
        std::fs::create_dir_all(&dir).unwrap();
        let out_path = dir.join("issue19484_unlocked.pdf");
        let err = super::run(corpus, out_path.to_str().unwrap(), None)
            .expect_err("broken encrypted file must error");
        // The error should mention "damaged object container" or similar
        // (the engine also reports 0 pages for this file).
        let msg = err.to_string();
        assert!(
            !msg.contains("panicked") && !msg.contains("unreachable"),
            "error is clean, not a crash: {msg}"
        );
    }
}
