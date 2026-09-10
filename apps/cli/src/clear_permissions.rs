//! The `selis clear-permissions` tool (SL-1A.TOOL.05): remove permission
//! restrictions from an encrypted document.
//!
//! Interprets the owner password as the explicit override SL-1.ENC.04
//! requires: the tool only proceeds when the password authenticates as the
//! document owner. For R5/6 (AES-256) the file key is independent of `/P`,
//! so the content stays encrypted at rest — only `/P` and `/Perms` are
//! updated via an incremental append (WRITE.02). For R2–4 (RC4/AES-128) the
//! `/P` value feeds the key derivation, so a new key is derived from the
//! cleared `/P` and every stream/string is re-encrypted.

use std::collections::BTreeSet;

use selis_log::oplog::{ActorId, OpLog, OpRecord, Outcome};
use selis_pdf_cos::doc_writer::{write_incremental_update, write_objects_as_document_with_trailer};
use selis_pdf_cos::encrypt::{self, DecryptPolicy};
use selis_pdf_cos::{parse_revisions, xref, Obj, Ref};
use selis_pdf_doc::Resolver;
use selis_sandbox::{Budget, Surface};

use crate::verify_report::Verification;
use crate::{read_file, CliError, CliResult};

/// All permissions granted (R2–4): bits 0–1 cleared, all permission and
/// reserved bits set.
const ALL_PERMS_R2_4: u32 = 0xFFFF_FFFC;

/// All permissions granted (R5/6): every bit set.
const ALL_PERMS_R5_6: u32 = 0xFFFF_FFFF;

/// The oplog vocabulary for the SL-1.ENC.04 override (closed, greppable).
const OP_CLEAR_PERMISSIONS: &str = "clear-permissions";
const OVERRIDE_OWNER_PASSWORD: &str = "owner-password-permission-bits";

/// Run the clear-permissions tool. Returns the operation's verification
/// display (SL-1A.UI.02).
pub(crate) fn run(path: &str, output: &str, password: Option<&str>) -> CliResult<Verification> {
    let pw = password.unwrap_or("");
    let src = read_file(path)?;
    let budget = Budget::unlimited();
    let clock = crate::shell_clock();
    let mut g = crate::runtime::cli_guard(&budget, &clock);

    let startxref = xref::find_startxref(&src, 4096).unwrap_or(0);
    let doc = parse_revisions(&src, startxref, &budget, &mut g)
        .map_err(|e| CliError(format!("{path}: {e}")))?;
    let rev = doc
        .revisions()
        .last()
        .ok_or_else(|| CliError(format!("{path}: no revisions")))?;

    let enc_ref = rev
        .encrypt
        .ok_or_else(|| CliError(format!("{path}: not encrypted; nothing to clear")))?;

    let info = encrypt::parse_encrypt(&src, Some(enc_ref), &budget, &mut g)
        .map_err(|e| CliError(format!("{path}: {e}")))?
        .ok_or_else(|| CliError(format!("{path}: unsupported /Encrypt dictionary")))?;
    let id0 = encrypt::document_id(&rev.trailer);

    // Require owner access (SL-1.ENC.04): clearing permissions is an explicit
    // override that only the owner password legitimises.
    if !selis_crypto::is_owner_password(
        &info.o,
        &info.u,
        info.p,
        &id0,
        info.r,
        info.length,
        info.encrypt_metadata,
        &info.ue,
        &info.oe,
        pw.as_bytes(),
    ) {
        return Err(CliError(format!(
            "{path}: owner password required (WRONG_OWNER_PASSWORD)"
        )));
    }

    let key = encrypt::authenticate(&info, &id0, pw.as_bytes())
        .ok_or_else(|| CliError(format!("{path}: wrong password (WRONG_PASSWORD)")))?;

    let root = rev
        .root
        .ok_or_else(|| CliError(format!("{path}: no /Root in trailer")))?;

    let verification = if info.r >= 5 {
        clear_permissions_r56(
            &src,
            &info,
            &key,
            root,
            path,
            output,
            &rev.trailer,
            &budget,
            &mut g,
        )
    } else {
        clear_permissions_r24(
            &src,
            &doc,
            &info,
            &key,
            pw.as_bytes(),
            &id0,
            root,
            path,
            output,
            &rev.trailer,
            &budget,
            &mut g,
        )
    }?;
    verification.emit_line();
    Ok(verification)
}
/// R5/6: `/P` does not feed the key derivation, so the content stays
/// encrypted at rest under the same file key.  Only `/P` and `/Perms` change,
/// and the new `/Encrypt` object is appended as a new revision (WRITE.02).
fn clear_permissions_r56(
    src: &[u8],
    info: &encrypt::EncryptInfo,
    key: &[u8],
    root: Ref,
    path: &str,
    output: &str,
    trailer: &[(selis_bytes::Bytes, Obj)],
    budget: &Budget,
    g: &mut selis_sandbox::BudgetGuard<'_>,
) -> CliResult<Verification> {
    let new_p = ALL_PERMS_R5_6;
    let perms = selis_crypto::compute_perms_r6(new_p, key, info.encrypt_metadata);

    // The /Encrypt object number from the trailer.
    let enc_num = trailer
        .iter()
        .find(|(k, _)| k.as_slice() == b"Encrypt")
        .and_then(|(_, v)| match v {
            Obj::Ref(r) => Some(r.num),
            _ => None,
        })
        .ok_or_else(|| CliError(format!("{path}: /Encrypt trailer entry not a ref")))?;

    let mut new_info = info.clone();
    new_info.p = new_p;
    new_info.perms = perms;
    let new_encrypt = encrypt::encrypt_dict(&new_info);

    // New revision trailer: /Root, /ID, /Encrypt.
    let new_trailer = build_new_trailer(trailer, enc_num, src);

    let updated = write_incremental_update(src, &[(enc_num, new_encrypt)], &new_trailer, budget, g)
        .map_err(|e| CliError(format!("{path}: write: {e}")))?;

    verify_output(&updated, root, path)?;

    // WRITE.05 (via the shared gate): the appended revision changes only the
    // /Encrypt dictionary, so every surveyed count must hold on the updated
    // bytes. Commit is atomic.
    let verification = crate::write_gate::verify_all_preserved(
        &updated,
        output,
        src,
        u64::try_from(src.len()).unwrap_or(u64::MAX),
        budget,
        g,
    )?;
    record_override(
        "R5/6 incremental: /P and /Perms updated, content re-encrypted under the unchanged file key",
    );
    eprintln!("cleared permissions on {path} -> {output}");
    Ok(verification)
}

/// R2–4: `/P` feeds the key derivation.  Decrypt all content with the old
/// key, derive a new key from the cleared `/P`, recompute `/U`, and
/// re-encrypt every stream/string.
fn clear_permissions_r24(
    src: &[u8],
    doc: &selis_pdf_cos::Doc,
    info: &encrypt::EncryptInfo,
    old_key: &[u8],
    owner_password: &[u8],
    id0: &[u8],
    root: Ref,
    path: &str,
    output: &str,
    trailer: &[(selis_bytes::Bytes, Obj)],
    budget: &Budget,
    g: &mut selis_sandbox::BudgetGuard<'_>,
) -> CliResult<Verification> {
    // Recover the padded user password from /O (Algorithm 3), so the new key
    // can be derived from the same user password with the cleared /P.
    let user_pw = selis_crypto::recover_user_password(&info.o, info.r, info.length, owner_password)
        .ok_or_else(|| CliError(format!("{path}: cannot recover user password from /O")))?;

    let new_p = ALL_PERMS_R2_4;
    let new_key = selis_crypto::encryption_key(
        &user_pw,
        &info.o,
        new_p,
        id0,
        info.r,
        info.length,
        info.encrypt_metadata,
    );
    let new_u = selis_crypto::compute_u(&new_key, info.r, id0);

    // Walk the object graph with the OLD key (auto-decrypt via Resolver).
    let old_policy = DecryptPolicy::from_encrypt(info, old_key.to_vec());
    let new_policy = DecryptPolicy::from_encrypt(info, new_key.clone());

    let mut resolver = Resolver::new(doc, src, budget);
    resolver.set_key(old_policy);

    let mut objects: Vec<(u32, Obj)> = Vec::new();
    let mut visited = BTreeSet::new();
    walk(&mut resolver, root, &mut visited, &mut objects, g)
        .map_err(|e| CliError(format!("{path}: {e}")))?;

    // /Info as a separate root.
    if let Some((_, Obj::Ref(info_r))) = trailer.iter().find(|(k, _)| k.as_slice() == b"Info") {
        if !visited.contains(&info_r.num) {
            walk(&mut resolver, *info_r, &mut visited, &mut objects, g)
                .map_err(|e| CliError(format!("{path}: {e}")))?;
        }
    }

    // Re-encrypt each object with the new key.
    for (num, obj) in &mut objects {
        let r = Ref::new(*num, 0);
        let taken = std::mem::replace(obj, Obj::Int(0));
        *obj = selis_pdf_doc::encrypt_obj(taken, r, &new_policy);
    }

    // Build the new /Encrypt dict via the canonical builder: mutate the
    // parsed info with the cleared /P and the recomputed /U, then serialize.
    // Per ISO 32000-1 Table 15 the trailer's /Encrypt is an indirect
    // reference, so the dict becomes a fresh object after the walked content.
    let mut new_info = info.clone();
    new_info.p = new_p;
    new_info.u = new_u.clone();
    let new_encrypt = encrypt::encrypt_dict(&new_info);

    // Trailer: /Root, /Info, /ID, /Encrypt (indirect).
    let mut extra_trailer: Vec<(Vec<u8>, Obj)> = Vec::new();
    if let Some((_, Obj::Ref(info_r))) = trailer.iter().find(|(k, _)| k.as_slice() == b"Info") {
        extra_trailer.push((b"Info".to_vec(), Obj::Ref(*info_r)));
    }
    let fresh = fresh_file_id(src);
    extra_trailer.push((
        b"ID".to_vec(),
        Obj::Array(vec![
            Obj::String(selis_bytes::Bytes::copy_from_slice(id0)),
            Obj::String(selis_bytes::Bytes::copy_from_slice(&fresh)),
        ]),
    ));

    // The /Encrypt object gets the first free object number above the walk.
    let enc_num = objects
        .iter()
        .map(|(num, _)| *num)
        .max()
        .unwrap_or(0)
        .saturating_add(1);
    objects.push((enc_num, new_encrypt));
    extra_trailer.push((b"Encrypt".to_vec(), Obj::Ref(Ref::new(enc_num, 0))));

    let bytes = write_objects_as_document_with_trailer(&objects, root, &extra_trailer, budget, g)
        .map_err(|e| CliError(format!("{path}: write: {e}")))?;

    verify_output(&bytes, root, path)?;

    // WRITE.05 (via the shared gate): the re-encryption changes no structure,
    // so every surveyed count must hold. Commit is atomic.
    let verification = crate::write_gate::verify_all_preserved(
        &bytes,
        output,
        src,
        u64::try_from(src.len()).unwrap_or(u64::MAX),
        budget,
        g,
    )?;
    record_override(&format!(
        "R{} full rewrite: content re-encrypted under the /P-cleared key",
        info.r
    ));
    eprintln!("cleared permissions on {path} -> {output}");
    Ok(verification)
}

/// Record the SL-1.ENC.04 override in the oplog vocabulary and echo the
/// audit record (the CLI's one-shot oplog surface until the edit model
/// (SL-5) carries per-document logs).
fn record_override(summary: &str) {
    let mut log = OpLog::new();
    let seq = log.record_override(
        OP_CLEAR_PERMISSIONS,
        summary,
        0,
        ActorId::local(),
        Outcome::Ok { revision: None },
        OVERRIDE_OWNER_PASSWORD,
    );
    if let Some(OpRecord {
        seq: s,
        op,
        summary: sm,
        override_used,
        ..
    }) = log.records().iter().find(|r| r.seq == seq)
    {
        eprintln!(
            "oplog: seq={s} op={op} override={:?} summary={sm}",
            override_used.unwrap_or("")
        );
    }
}

/// Build the new revision's trailer entries for the R5/6 incremental update.
fn build_new_trailer(
    trailer: &[(selis_bytes::Bytes, Obj)],
    enc_num: u32,
    src: &[u8],
) -> Vec<(Vec<u8>, Obj)> {
    let mut t = Vec::new();
    if let Some((_, Obj::Ref(r))) = trailer.iter().find(|(k, _)| k.as_slice() == b"Root") {
        t.push((b"Root".to_vec(), Obj::Ref(*r)));
    }
    let id0 = encrypt::document_id(trailer);
    let fresh = fresh_file_id(src);
    t.push((
        b"ID".to_vec(),
        Obj::Array(vec![
            Obj::String(selis_bytes::Bytes::copy_from_slice(&id0)),
            Obj::String(selis_bytes::Bytes::copy_from_slice(&fresh)),
        ]),
    ));
    t.push((b"Encrypt".to_vec(), Obj::Ref(Ref::new(enc_num, 0))));
    t
}

/// WRITE.05: verify the output reparses with the same /Root and builds a
/// usable document model.
fn verify_output(bytes: &[u8], expected_root: Ref, path: &str) -> CliResult<()> {
    let verify_budget = Budget::unlimited();
    let mut vg = verify_budget.guard();
    let sx = xref::find_startxref(bytes, 4096).unwrap_or(0);
    let parsed = parse_revisions(bytes, sx, &verify_budget, &mut vg)
        .map_err(|e| CliError(format!("{path}: output failed verification: {e}")))?;
    let out_rev = parsed
        .revisions()
        .last()
        .ok_or_else(|| CliError(format!("{path}: output has no revisions")))?;
    if out_rev.root != Some(expected_root) {
        return Err(CliError(format!(
            "{path}: output failed verification (/Root mismatch)"
        )));
    }
    let doc_budget = Budget::profile(Surface::Viewer);
    let clock = crate::shell_clock();
    // The typed error is propagated, not swallowed (SL-1A.UI.06).
    selis_pdf_engine::Session::open(bytes.to_vec(), &doc_budget, &clock)
        .map_err(|e| CliError(format!("{path}: {e}")))?;
    Ok(())
}

/// Walk the object graph from `root`, resolving with decryption.
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

/// Collect all `Ref` values from an object (used to walk the graph).
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

/// A deterministic 16-byte file identifier (same construction as the unlock
/// tool's `fresh_file_id`).
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

    /// Build a minimal single-page PDF with no encryption: clear-permissions
    /// must refuse it cleanly.
    fn build_plain_pdf() -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(b"%PDF-1.4\n");
        let mut offsets = std::collections::HashMap::new();
        let objs: [(&[u8], &[u8]); 2] = [
            (b"1", b"<< /Type /Catalog /Pages 2 0 R >>"),
            (b"2", b"<< /Type /Pages /Kids [] /Count 0 >>"),
        ];
        for (num, body) in &objs {
            offsets.insert(*num, out.len());
            let n = std::str::from_utf8(num).unwrap_or("");
            out.extend_from_slice(format!("{n} 0 obj\n").as_bytes());
            out.extend_from_slice(body);
            out.extend_from_slice(b"\nendobj\n");
        }
        let xref_at = out.len();
        out.extend_from_slice(b"xref\n0 3\n");
        out.extend_from_slice(b"0000000000 65535 f \n");
        for i in 1..3u32 {
            let num = format!("{i}");
            let off = offsets[num.as_bytes()];
            out.extend_from_slice(format!("{off:010} 00000 n \n").as_bytes());
        }
        out.extend_from_slice(b"trailer\n");
        out.extend_from_slice(b"<< /Size 3 /Root 1 0 R >>\n");
        out.extend_from_slice(format!("startxref\n{xref_at}\n%%EOF\n").as_bytes());
        out
    }

    /// A plain document is refused (nothing to clear).
    #[test]
    fn unencrypted_document_is_refused() {
        let dir = std::env::temp_dir().join("selis-clear-perms-test");
        std::fs::create_dir_all(&dir).unwrap();
        let in_path = dir.join("plain.pdf");
        let out_path = dir.join("plain.out.pdf");
        std::fs::write(&in_path, build_plain_pdf()).unwrap();
        let err = super::run(in_path.to_str().unwrap(), out_path.to_str().unwrap(), None)
            .expect_err("plain document refused");
        assert!(
            err.to_string().contains("not encrypted"),
            "message mentions not encrypted: {err}"
        );
    }

    /// Build a minimal R2 (RC4-40) encrypted PDF: empty user password,
    /// owner password `"owner"`, restricted `/P`. Object 3 is the catalog,
    /// object 6 a stream carrying `BT /F1 12 Tf (ok) Tj ET`.
    fn build_r2_encrypted_pdf() -> Vec<u8> {
        let id0 = [0x11u8; 16];
        let restricted_p: u32 = 0xFFFF_FFC4; // print/modify/copy forbidden
        let owner_pw = b"owner";
        let user_pw = b"";

        let o = selis_crypto::compute_o(
            &selis_crypto::compute_owner_key(owner_pw, 2, 40),
            &selis_crypto::pad_password(user_pw),
            2,
        );
        let key = selis_crypto::encryption_key(user_pw, &o, restricted_p, &id0, 2, 40, true);
        let u = selis_crypto::compute_u(&key, 2, &id0);

        // Object bodies (only object 6 is a stream; strings stay unencrypted
        // except the stream body which we encrypt).
        let content = b"BT /F1 12 Tf (ok) Tj ET".to_vec();
        let enc_content = selis_crypto::encrypt_data(&key, 6, 0, &content, 2, false);

        let mut out = Vec::new();
        out.extend_from_slice(b"%PDF-1.4\n");
        let mut offsets = std::collections::HashMap::new();
        let mut push_obj = |num: u32, body: &[u8]| {
            offsets.insert(num, out.len());
            out.extend_from_slice(format!("{num} 0 obj\n").as_bytes());
            out.extend_from_slice(body);
            out.extend_from_slice(b"\nendobj\n");
        };
        push_obj(1, b"<< /Type /Catalog /Pages 2 0 R >>");
        push_obj(2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
        push_obj(
            3,
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] /Contents 6 0 R /Resources << /Font << /F1 4 0 R >> >> >>",
        );
        push_obj(4, b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>");
        // /Encrypt dict (not itself encrypted). /P is a signed 32-bit value.
        let enc_body = format!(
            "<< /Filter /Standard /V 1 /R 2 /Length 40 /O <{}> /U <{}> /P {} >>",
            hex(&o),
            hex(&u),
            restricted_p as i32
        );
        push_obj(5, enc_body.as_bytes());
        // The content stream, RC4-encrypted under object 6.
        let stream_body = format!("<< /Length {} >>\nstream\n", enc_content.len());
        offsets.insert(6, out.len());
        out.extend_from_slice(format!("6 0 obj\n").as_bytes());
        out.extend_from_slice(stream_body.as_bytes());
        out.extend_from_slice(&enc_content);
        out.extend_from_slice(b"\nendstream\nendobj\n");

        let xref_at = out.len();
        out.extend_from_slice(b"xref\n0 7\n");
        out.extend_from_slice(b"0000000000 65535 f \n");
        for i in 1..7u32 {
            let off = offsets[&i];
            out.extend_from_slice(format!("{off:010} 00000 n \n").as_bytes());
        }
        out.extend_from_slice(b"trailer\n");
        let id_hex = format!("{}", hex(&id0));
        out.extend_from_slice(
            format!("<< /Size 7 /Root 1 0 R /Encrypt 5 0 R /ID [<{id_hex}><{id_hex}>] >>\n")
                .as_bytes(),
        );
        out.extend_from_slice(format!("startxref\n{xref_at}\n%%EOF\n").as_bytes());
        out
    }

    fn hex(bytes: &[u8]) -> String {
        let mut s = String::with_capacity(bytes.len() * 2);
        for b in bytes {
            s.push_str(&format!("{b:02X}"));
        }
        s
    }

    /// Build a minimal R6 (AES-256) encrypted PDF: empty user password,
    /// owner password `"owner"`, restricted `/P`. Content is AES-256-CBC
    /// encrypted under the file key (no per-object salting for R6).
    fn build_r6_encrypted_pdf() -> Vec<u8> {
        let id0 = [0x22u8; 16];
        let restricted_p: u32 = 0xFFFF_FBE4;
        let owner_pw = b"owner";
        let user_pw = b"";
        let file_key = [0x5Au8; 32];

        // Random-but-fixed salts keep the synthetic file deterministic; the
        // credential values themselves follow the R6 Algorithms 2.A/2.B.
        let v_salt = [0xA1u8; 8];
        let k_salt = [0xB2u8; 8];
        let ov_salt = [0xC3u8; 8];
        let ok_salt = [0xD4u8; 8];
        let u = selis_crypto::compute_u_r6(user_pw, &v_salt, &k_salt, 6);
        let ue = selis_crypto::compute_ue_r6(user_pw, &k_salt, &file_key, 6);
        let o = selis_crypto::compute_o_r6(owner_pw, &ov_salt, &ok_salt, &u, 6);
        let oe = selis_crypto::compute_oe_r6(owner_pw, &ok_salt, &u, &file_key, 6);
        let perms = selis_crypto::compute_perms_r6(restricted_p, &file_key, true);
        let content = b"BT /F1 12 Tf (ok) Tj ET".to_vec();
        let enc_content = selis_crypto::encrypt_data(&file_key, 6, 0, &content, 6, true);

        let mut out = Vec::new();
        out.extend_from_slice(b"%PDF-2.0\n");
        let mut offsets = std::collections::HashMap::new();
        let mut push_obj = |num: u32, body: &[u8]| {
            offsets.insert(num, out.len());
            out.extend_from_slice(format!("{num} 0 obj\n").as_bytes());
            out.extend_from_slice(body);
            out.extend_from_slice(b"\nendobj\n");
        };
        push_obj(1, b"<< /Type /Catalog /Pages 2 0 R >>");
        push_obj(2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
        push_obj(
            3,
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] /Contents 6 0 R /Resources << /Font << /F1 4 0 R >> >> >>",
        );
        push_obj(4, b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>");
        let enc_body = format!(
            "<< /Filter /Standard /V 5 /R 6 /Length 256 /O <{}> /U <{}> /OE <{}> /UE <{}> /P {} /Perms <{}> >>",
            hex(&o),
            hex(&u),
            hex(&oe),
            hex(&ue),
            restricted_p as i32,
            hex(&perms)
        );
        push_obj(5, enc_body.as_bytes());
        let stream_body = format!("<< /Length {} >>\nstream\n", enc_content.len());
        offsets.insert(6, out.len());
        out.extend_from_slice(b"6 0 obj\n");
        out.extend_from_slice(stream_body.as_bytes());
        out.extend_from_slice(&enc_content);
        out.extend_from_slice(b"\nendstream\nendobj\n");

        let xref_at = out.len();
        out.extend_from_slice(b"xref\n0 7\n");
        out.extend_from_slice(b"0000000000 65535 f \n");
        for i in 1..7u32 {
            let off = offsets[&i];
            out.extend_from_slice(format!("{off:010} 00000 n \n").as_bytes());
        }
        out.extend_from_slice(b"trailer\n");
        let id_hex = hex(&id0);
        out.extend_from_slice(
            format!("<< /Size 7 /Root 1 0 R /Encrypt 5 0 R /ID [<{id_hex}><{id_hex}>] >>\n")
                .as_bytes(),
        );
        out.extend_from_slice(format!("startxref\n{xref_at}\n%%EOF\n").as_bytes());
        out
    }

    /// A real R6 (AES-256) document with known passwords round-trips via the
    /// incremental path: content bytes are untouched, /P is all permissions,
    /// and the output still opens under the same (empty) user password.
    #[test]
    fn r6_encrypted_round_trips_with_bits_cleared() {
        let dir = std::env::temp_dir().join("selis-clear-perms-test");
        std::fs::create_dir_all(&dir).unwrap();
        let in_path = dir.join("r6_encrypted.pdf");
        let out_path = dir.join("r6_cleared.pdf");
        let src = build_r6_encrypted_pdf();
        std::fs::write(&in_path, &src).unwrap();

        super::run(
            in_path.to_str().unwrap(),
            out_path.to_str().unwrap(),
            Some("owner"),
        )
        .expect("clear permissions on R6");

        let out = std::fs::read(&out_path).unwrap();
        // Original bytes are a prefix (incremental append, WRITE.02).
        assert!(out.starts_with(&src), "R6 path must append incrementally");
        let s = String::from_utf8_lossy(&out);
        let tail = &s[s.len().saturating_sub(2000)..];
        assert!(
            tail.contains("/P 4294967295") || tail.contains("/P -1"),
            "output /P is all permissions: {tail}"
        );
        // The output still opens in the engine with the empty user password,
        // and the content stream still decrypts (same file key).
        let budget = selis_sandbox::Budget::profile(selis_sandbox::Surface::Viewer);
        let session = selis_pdf_engine::Session::open(out, &budget, &crate::shell_clock())
            .expect("output opens in the engine");
        assert_eq!(session.len(), 1, "one page");
    }

    /// A real R2 (RC4-40) document with known passwords round-trips with bits
    /// cleared: the output opens with the empty user password and `/P` is all
    /// permissions.
    #[test]
    fn r2_encrypted_round_trips_with_bits_cleared() {
        let dir = std::env::temp_dir().join("selis-clear-perms-test");
        std::fs::create_dir_all(&dir).unwrap();
        let in_path = dir.join("r2_encrypted.pdf");
        let out_path = dir.join("r2_cleared.pdf");
        std::fs::write(&in_path, build_r2_encrypted_pdf()).unwrap();

        super::run(
            in_path.to_str().unwrap(),
            out_path.to_str().unwrap(),
            Some("owner"),
        )
        .expect("clear permissions on R2");

        let out = std::fs::read(&out_path).unwrap();
        let s = String::from_utf8_lossy(&out);
        // /P in the output must be all-permissions (0xFFFFFFFC).
        assert!(
            s.contains("/P 4294967292") || s.contains("/P -4"),
            "output /P is all permissions: {s}"
        );
        // The output opens in the engine with the (empty) user password.
        let budget = selis_sandbox::Budget::profile(selis_sandbox::Surface::Viewer);
        let session = selis_pdf_engine::Session::open(out, &budget, &crate::shell_clock())
            .expect("output opens in the engine");
        assert_eq!(session.len(), 1, "one page");
    }
}
