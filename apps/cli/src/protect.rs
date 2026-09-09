//! The `selis protect` tool (SL-1A.TOOL.06): add password protection and
//! permission bits.
//!
//! Encrypts with AESV3 / revision 6 only (ADR-P0019 — the only handler Selis
//! writes), via the SL-1.ENC.02 write path: `EncryptInfo::new_r6` derives the
//! credentials from separate user and owner passwords with CSPRNG salts and a
//! CSPRNG file key, and every stream/string is encrypted with the file key
//! (`encrypt::encrypt_object`, the mirror of the resolver's decrypt).
//!
//! This is a full rewrite (WRITE.01), not an incremental append: encrypting
//! changes every stream and string body, so the output cannot share bytes
//! with the source. The source bytes stay immutable (they are read once into
//! memory); the rewritten output is verified structurally (WRITE.05) before
//! any file is replaced (ADR-P0007). Encrypting an already-encrypted file is
//! refused with the typed `ALREADY_ENCRYPTED` error — removing a password is
//! the separate `selis unlock` tool.
//!
//! Permission bits (`/P`) are a *convention, not enforcement* (SL-1.ENC.04):
//! the tool writes the owner's intent honestly and the long-help says so in
//! plain language. The v1 surface exposes the four bits users actually ask
//! for — print, modify, copy, annotate — and always grants accessibility
//! extraction (screen readers). `/EncryptMetadata` is honoured: when the
//! caller opts out, the document-level XMP stream is left in the clear.
//!
//! Passwords arrive via arguments or environment variables only. They are
//! never logged and never written anywhere except the `/Encrypt` dictionary.

use std::collections::BTreeSet;

use selis_log::oplog::{ActorId, OpLog, Outcome};
use selis_pdf_cos::doc_writer::write_objects_as_document_with_trailer;
use selis_pdf_cos::encrypt::{self, EncryptInfo};
use selis_pdf_cos::{parse_revisions, xref, Obj, Ref};
use selis_pdf_doc::Resolver;
use selis_sandbox::{Budget, Surface};

use crate::{read_file, CliError, CliResult};

/// All permission bits granted, restricted only by the spec's reserved-bit
/// rules: bits 1–2 shall be 0, bits 7–8 and 13–32 shall be 1 (ISO 32000-1
/// Table 22). Denying a permission clears its bit from this baseline.
const P_BASELINE: u32 = 0xFFFF_FFFC;

/// Bit 3 — print (ISO 32000-1 Table 22).
const BIT_PRINT: u32 = 0x0000_0004;
/// Bit 4 — modify the document contents.
const BIT_MODIFY: u32 = 0x0000_0008;
/// Bit 5 — copy / extract text and graphics.
const BIT_COPY: u32 = 0x0000_0010;
/// Bit 6 — add or modify annotations and form fields.
const BIT_ANNOTATE: u32 = 0x0000_0020;

/// The bits the v1 surface exposes, with their CLI names. Everything else in
/// the baseline stays granted — including accessibility extraction (bit 10),
/// which is deliberately not restrictable — and `/P` bits not exposed here
/// are documented as such rather than hidden behind extra flags.
const EXPOSED: &[(&str, u32)] = &[
    ("print", BIT_PRINT),
    ("modify", BIT_MODIFY),
    ("copy", BIT_COPY),
    ("annotate", BIT_ANNOTATE),
];

/// The oplog vocabulary (`&'static str` keeps the set closed and greppable).
const OP_PROTECT: &str = "protect";

/// Resolve a permission spec into the exposed-bit grant mask.
///
/// The spec is a comma-separated list of permissions to **grant** — `print`,
/// `modify`, `copy`, `annotate` — or the token `none`. Anything not listed is
/// denied; the default (no `--permissions`) grants all four. Bits the surface
/// does not expose (fill-forms, accessibility, assemble, high-res print) are
/// always granted and are not reachable from this mask.
///
/// # Errors
///
/// A plain message naming the valid vocabulary on an unknown token — this is
/// a caller mistake, not document damage.
pub(crate) fn parse_permissions(spec: &str) -> Result<u32, String> {
    let spec = spec.trim();
    if spec.is_empty() {
        return Err("empty --permissions spec (expected e.g. `print,copy` or `none`)".to_string());
    }
    if spec.eq_ignore_ascii_case("none") {
        return Ok(0);
    }
    let mut granted = 0u32;
    for token in spec.split(',') {
        let token = token.trim();
        let (_, bit) = EXPOSED
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case(token))
            .ok_or_else(|| {
                format!(
                    "unknown permission `{token}` (valid: {})",
                    EXPOSED
                        .iter()
                        .map(|(n, _)| *n)
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            })?;
        granted |= bit;
    }
    Ok(granted)
}

/// Map a grant mask onto the conformant `/P` value: start from the
/// all-granted baseline and clear the exposed bits that were denied.
/// Bits 1–2 stay 0 and bits 7–8 / 13–32 stay 1 in every outcome.
#[must_use]
pub(crate) fn compute_p(granted: u32) -> u32 {
    let exposed_bits: u32 = EXPOSED.iter().map(|(_, bit)| bit).fold(0, |a, b| a | b);
    P_BASELINE & !(exposed_bits & !granted)
}

/// Run the protect tool.
///
/// `user_password` opens the document (empty/absent = the document opens
/// without a password); `owner_password` guards permission changes and
/// defaults to the user password, as Acrobat does. Both fall back to
/// `SELIS_USER_PASSWORD` / `SELIS_OWNER_PASSWORD` when their argument is
/// absent. `permissions` is the optional grant spec ([`parse_permissions`]);
/// `encrypt_metadata` selects whether the document-level XMP stream is
/// encrypted (the spec default is true).
///
/// # Errors
///
/// `IO_READ_FAILED` when the input cannot be read, a typed parse error when
/// the document is damaged, the typed `ALREADY_ENCRYPTED` error when the
/// input already carries an `/Encrypt` dictionary, and a plain usage error
/// for empty credentials or an unknown permission name.
///
/// # Budget
///
/// Parsing and the object walk run under an unlimited budget (the CLI's
/// standing choice, matching the sibling tools); the writer charges the
/// serialised output bytes per object. A surface-level budget slots in at the
/// same call sites.
///
/// # Malformed Input
///
/// A damaged document fails at `parse_revisions` and nothing is written. An
/// already-encrypted document fails with `ALREADY_ENCRYPTED` before any work.
/// Verification re-parses the output and re-authenticates both passwords
/// before any file is replaced, so a failure anywhere leaves the source bytes
/// untouched and no partial output on disk.
pub(crate) fn run(
    path: &str,
    output: &str,
    user_password: Option<&str>,
    owner_password: Option<&str>,
    permissions: Option<&str>,
    encrypt_metadata: bool,
) -> CliResult<()> {
    // Passwords arrive via arguments or environment variables only, are
    // truncated to the Algorithm 2.A limit, and are never logged or echoed.
    let user_pw = resolve_password(user_password, "SELIS_USER_PASSWORD").unwrap_or_default();
    let owner_pw = match resolve_password(owner_password, "SELIS_OWNER_PASSWORD") {
        Some(pw) => pw,
        None => user_pw.clone(),
    };
    if user_pw.is_empty() && owner_pw.is_empty() {
        return Err(CliError(
            "no passwords given: --user-password / --owner-password or \
             SELIS_USER_PASSWORD / SELIS_OWNER_PASSWORD"
                .to_string(),
        ));
    }
    let granted = match permissions {
        Some(spec) => parse_permissions(spec).map_err(CliError)?,
        None => parse_permissions("print,modify,copy,annotate").map_err(CliError)?,
    };
    let p = compute_p(granted);

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

    if rev.encrypt.is_some() {
        let mut ctx = selis_error::Ctx::new();
        ctx.detail = Some(format!(
            "{path}: remove the existing password with `selis unlock` first"
        ));
        return Err(CliError::from(selis_error::Error::with(
            selis_error::Code::AlreadyEncrypted,
            ctx,
        )));
    }
    let root = rev
        .root
        .ok_or_else(|| CliError(format!("{path}: no /Root in trailer")))?;

    // Walk the object graph from /Root (and /Info, so metadata survives),
    // keeping the original object numbers. The input is unencrypted, so the
    // resolver needs no key.
    let mut resolver = Resolver::new(&doc, &src, &budget);
    let mut objects: Vec<(u32, Obj)> = Vec::new();
    let mut visited = BTreeSet::new();
    walk(&mut resolver, root, &mut visited, &mut objects, &mut g)
        .map_err(|e| CliError(format!("{path}: {e}")))?;
    if let Some((_, Obj::Ref(info_ref))) = rev.trailer.iter().find(|(k, _)| k.as_slice() == b"Info")
    {
        walk(&mut resolver, *info_ref, &mut visited, &mut objects, &mut g)
            .map_err(|e| CliError(format!("{path}: {e}")))?;
    }

    // The document-level XMP stream is exempt from encryption when
    // /EncryptMetadata is false. Only an indirect /Metadata can be exempted;
    // a stream stored directly inside the catalog would have to be rewritten
    // into its own object, so the tool refuses rather than mis-handle it.
    let mut metadata_exempt: Option<u32> = None;
    if !encrypt_metadata {
        let metadata_obj = objects
            .iter()
            .find(|(num, _)| *num == root.num)
            .and_then(|(_, obj)| match obj {
                Obj::Dict(pairs) => pairs.iter().find(|(k, _)| k.as_slice() == b"Metadata"),
                _ => None,
            })
            .map(|(_, v)| v);
        match metadata_obj {
            Some(Obj::Ref(r)) => metadata_exempt = Some(r.num),
            Some(_) => {
                return Err(CliError(format!(
                    "{path}: /Metadata is a direct stream; --encrypt-metadata false \
                     needs an indirect /Metadata reference"
                )));
            }
            None => {}
        }
    }

    // The R6 credentials: CSPRNG salts + file key inside `new_r6`; the
    // document /ID is generated from the CSPRNG as well (ISO 32000-1 §14.4).
    // Revision 6 does not feed /ID into key derivation, so a fresh random ID
    // is safe and unguessable.
    let id = selis_crypto::random_bytes(16);
    let (mut info, file_key) =
        EncryptInfo::new_r6(clamp_password(&user_pw), clamp_password(&owner_pw), p, &id);
    info.encrypt_metadata = encrypt_metadata;
    // new_r6 bakes the /EncryptMetadata marker into /Perms; recompute the
    // blob so the marker matches the caller's flag.
    info.perms = selis_crypto::compute_perms_r6(p, &file_key, encrypt_metadata);

    // Encrypt every object except the /Encrypt dictionary itself (written
    // separately below) and the exempted metadata stream.
    for (num, obj) in objects.iter_mut() {
        if Some(*num) == metadata_exempt {
            continue;
        }
        let taken = std::mem::replace(obj, Obj::Null);
        *obj = encrypt::encrypt_object(&taken, &file_key, *num, 0, info.r, info.aes);
    }

    // The /Encrypt dictionary as its own (plaintext) object after the walked
    // content, referenced from the trailer (ISO 32000-1 Table 15).
    let enc_num = objects
        .iter()
        .map(|(num, _)| *num)
        .max()
        .unwrap_or(0)
        .saturating_add(1);
    objects.push((enc_num, encrypt::encrypt_dict(&info)));

    // Trailer: /Root, /Info, /ID, /Encrypt (the writer emits /Size + /Root).
    let mut extra_trailer: Vec<(Vec<u8>, Obj)> = Vec::new();
    if let Some((_, Obj::Ref(info_ref))) = rev.trailer.iter().find(|(k, _)| k.as_slice() == b"Info")
    {
        extra_trailer.push((b"Info".to_vec(), Obj::Ref(*info_ref)));
    }
    extra_trailer.push((
        b"ID".to_vec(),
        Obj::Array(vec![
            Obj::HexString(selis_bytes::Bytes::copy_from_slice(&id)),
            Obj::HexString(selis_bytes::Bytes::copy_from_slice(&id)),
        ]),
    ));
    extra_trailer.push((b"Encrypt".to_vec(), Obj::Ref(Ref::new(enc_num, 0))));

    let bytes =
        write_objects_as_document_with_trailer(&objects, root, &extra_trailer, &budget, &mut g)
            .map_err(|e| CliError(format!("{path}: write: {e}")))?;

    // WRITE.05: verify before replacing. The re-parsed output must carry a
    // revision-6 /Encrypt whose /P and /EncryptMetadata match the request,
    // both passwords must authenticate, /Perms must verify against the file
    // key, the exempted metadata (if any) must still be plaintext, and the
    // document model must open.
    verify_output(
        &bytes,
        VerifyExpectations {
            root,
            p,
            encrypt_metadata,
            user_pw,
            owner_pw,
            metadata_exempt,
        },
        path,
    )?;

    std::fs::write(output, &bytes).map_err(|e| CliError(format!("cannot write {output}: {e}")))?;

    record_operation(&granted);
    eprintln!("protected {path} -> {output} (AES-256, revision 6)");
    Ok(())
}

/// Resolve a password: the argument wins; otherwise the named environment
/// variable; otherwise `None` (the caller applies its own default — an absent
/// user password means the document opens without one).
///
/// Command-line arguments can be visible in shell history and process
/// listings; environment variables in the process environment. Both are
/// local-machine exposures — the tradeoff is documented in the command's
/// long help. Neither channel is ever logged, and neither value is written
/// anywhere except the `/Encrypt` dictionary.
fn resolve_password(arg: Option<&str>, env_var: &str) -> Option<String> {
    if let Some(pw) = arg {
        return Some(pw.to_string());
    }
    std::env::var(env_var).ok().filter(|pw| !pw.is_empty())
}

/// Algorithm 2.A step (a): a UTF-8 password is truncated to 127 bytes before
/// hashing. Truncation happens here so the written credentials and any later
/// authentication agree byte-for-byte.
fn clamp_password(pw: &str) -> &[u8] {
    let end = pw.len().min(127);
    pw.as_bytes().get(..end).unwrap_or(pw.as_bytes())
}

/// Record the operation in the oplog vocabulary and echo the audit line (the
/// CLI's one-shot oplog surface until SL-5 carries per-document logs). The
/// record names the granted permission set — never the passwords.
fn record_operation(granted: &u32) {
    let granted_names: Vec<&str> = EXPOSED
        .iter()
        .filter(|(_, bit)| (granted & bit) != 0)
        .map(|(name, _)| *name)
        .collect();
    let mut log = OpLog::new();
    let seq = log.record(
        OP_PROTECT,
        format!(
            "AES-256/revision 6 encryption applied; permissions granted: {}",
            if granted_names.is_empty() {
                "none".to_string()
            } else {
                granted_names.join(",")
            }
        ),
        0,
        ActorId::local(),
        Outcome::Ok { revision: None },
    );
    if let Some(rec) = log.records().iter().find(|r| r.seq == seq) {
        eprintln!(
            "oplog: seq={} op={} summary={}",
            rec.seq, rec.op, rec.summary
        );
    }
}

/// WRITE.05 structural verification of the encrypted output.
///
/// # Budget
///
/// Runs under its own unlimited budget (the CLI's standing choice): a full
/// re-parse, two authentications (hardened-hash runs), and a document-model
/// open over the output bytes. Nothing is charged against the caller.
///
/// # Malformed Input
///
/// `bytes` is engine-written, not document-derived, so every failure here is
/// a writer bug, not a document problem: each check fails the tool with an
/// `output failed verification` error and no file is written.
/// The expectations the WRITE.05 verification checks the output against.
struct VerifyExpectations {
    /// The catalog reference from the input.
    root: Ref,
    /// The requested `/P` value.
    p: u32,
    /// The requested `/EncryptMetadata` flag.
    encrypt_metadata: bool,
    /// The user password that must authenticate.
    user_pw: String,
    /// The owner password that must authenticate.
    owner_pw: String,
    /// The object number of the plaintext-exempt `/Metadata` stream, if any.
    metadata_exempt: Option<u32>,
}

/// WRITE.05 structural verification of the encrypted output.
///
/// # Budget
///
/// Runs under its own unlimited budget (the CLI's standing choice): a full
/// re-parse, two authentications (hardened-hash runs), and a document-model
/// open over the output bytes. Nothing is charged against the caller.
///
/// # Malformed Input
///
/// `bytes` is engine-written, not document-derived, so every failure here is
/// a writer bug, not a document problem: each check fails the tool with an
/// `output failed verification` error and no file is written.
fn verify_output(bytes: &[u8], expect: VerifyExpectations, path: &str) -> CliResult<()> {
    let budget = Budget::unlimited();
    let mut g = budget.guard();
    let sx = xref::find_startxref(bytes, 4096).unwrap_or(0);
    let parsed = parse_revisions(bytes, sx, &budget, &mut g)
        .map_err(|e| CliError(format!("{path}: output failed verification: {e}")))?;
    let rev = parsed
        .revisions()
        .last()
        .ok_or_else(|| CliError(format!("{path}: output failed verification (no revisions)")))?;
    if rev.root != Some(expect.root) {
        return Err(CliError(format!(
            "{path}: output failed verification (/Root not preserved)"
        )));
    }
    let enc_ref = rev.encrypt.ok_or_else(|| {
        CliError(format!(
            "{path}: output failed verification (/Encrypt missing)"
        ))
    })?;
    let info = encrypt::parse_encrypt(bytes, Some(enc_ref), &budget, &mut g)
        .map_err(|e| CliError(format!("{path}: output failed verification: {e}")))?
        .ok_or_else(|| {
            CliError(format!(
                "{path}: output failed verification (/Encrypt unreadable)"
            ))
        })?;
    if info.r != 6 || info.v != 5 {
        return Err(CliError(format!(
            "{path}: output failed verification (expected revision 6 / AES-256, got R{} V{})",
            info.r, info.v
        )));
    }
    if info.p != expect.p {
        return Err(CliError(format!(
            "{path}: output failed verification (/P mismatch)"
        )));
    }
    if info.encrypt_metadata != expect.encrypt_metadata {
        return Err(CliError(format!(
            "{path}: output failed verification (/EncryptMetadata mismatch)"
        )));
    }
    let id0 = encrypt::document_id(&rev.trailer);
    if id0.is_empty() {
        return Err(CliError(format!(
            "{path}: output failed verification (/ID missing)"
        )));
    }
    let key = encrypt::authenticate(&info, &id0, expect.user_pw.as_bytes()).ok_or_else(|| {
        CliError(format!(
            "{path}: output failed verification (user password does not authenticate)"
        ))
    })?;
    if !selis_crypto::verify_perms_r6(info.p, &key, &info.perms, info.encrypt_metadata) {
        return Err(CliError(format!(
            "{path}: output failed verification (/Perms does not verify)"
        )));
    }
    encrypt::authenticate(&info, &id0, expect.owner_pw.as_bytes()).ok_or_else(|| {
        CliError(format!(
            "{path}: output failed verification (owner password does not authenticate)"
        ))
    })?;
    if let Some(num) = expect.metadata_exempt {
        let ok = matches!(
            resolve_number(bytes, num, &budget, &mut g),
            Ok(Obj::Stream { .. })
        );
        if !ok {
            return Err(CliError(format!(
                "{path}: output failed verification (exempted /Metadata stream unreadable)"
            )));
        }
    }
    // The output must also build a usable document model (WRITE.07). With a
    // non-empty user password the session opens tolerantly without the key —
    // this check is structural reachability, not decryption (the decrypt path
    // is proven by the authenticate + /Perms checks above and the round-trip
    // tests).
    let doc_budget = Budget::profile(Surface::Viewer);
    if selis_pdf_engine::Session::open(bytes.to_vec(), &doc_budget).is_err() {
        return Err(CliError(format!(
            "{path}: output failed verification (no usable document model)"
        )));
    }
    Ok(())
}

/// Resolve one object by number across revisions (used for the metadata
/// exemption check on the re-parsed output).
///
/// # Budget
///
/// Charged to the caller's guard like every parse: object bytes and one
/// object unit per resolution.
///
/// # Malformed Input
///
/// A number absent from every revision's xref, or an unparseable body,
/// returns the typed parse error instead of a partial object.
fn resolve_number(
    src: &[u8],
    num: u32,
    budget: &Budget,
    g: &mut selis_sandbox::BudgetGuard<'_>,
) -> Result<Obj, selis_error::Error> {
    let startxref = xref::find_startxref(src, 4096).unwrap_or(0);
    let doc = parse_revisions(src, startxref, budget, g)?;
    for view in doc.revisions().iter().rev() {
        if let Some(selis_pdf_cos::XrefEntry::InUse { offset, .. }) = view.entries.get(&num) {
            return selis_pdf_cos::resolve_object(src, *offset, budget, g);
        }
    }
    Err(selis_error::Error::new(selis_error::Code::ObjUnexpected))
}

/// Walk the object graph from `root`, collecting (num, Obj) pairs with the
/// original object numbers preserved. The visited set makes cyclic graphs
/// terminate; depth and object counts are charged to the budget guard.
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

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::indexing_slicing)]

    use super::*;

    /// Build a minimal single-page PDF with a content stream, an indirect
    /// catalog /Metadata stream (XMP), and an /Info dictionary — every string
    /// and stream class the encryption must cover. Object 5 is the content
    /// stream, 6 the metadata stream, 7 the Info dict.
    fn build_plain_pdf(content: &[u8]) -> Vec<u8> {
        // One object per entry: (number, body). Stream objects carry their
        // own `stream...endstream` section inside the body.
        let objs: Vec<(u32, Vec<u8>)> = vec![
            (
                1,
                b"<< /Type /Catalog /Pages 2 0 R /Metadata 6 0 R >>".to_vec(),
            ),
            (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec()),
            (
                3,
                b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] /Contents 5 0 R /Resources << /Font << /F1 4 0 R >> >> >>".to_vec(),
            ),
            (
                4,
                b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_vec(),
            ),
            (
                5,
                [
                    format!("<< /Length {} >>\nstream\n", content.len()).into_bytes(),
                    content.to_vec(),
                    b"\nendstream".to_vec(),
                ]
                .concat(),
            ),
            (
                6,
                [
                    format!(
                        "<< /Length {} /Type /Metadata /Subtype /XML >>\nstream\n",
                        XMP.len()
                    )
                    .into_bytes(),
                    XMP.to_vec(),
                    b"\nendstream".to_vec(),
                ]
                .concat(),
            ),
            (
                7,
                b"<< /Title (secret-title) /Producer (selis-test) >>".to_vec(),
            ),
        ];

        let mut out = Vec::new();
        out.extend_from_slice(b"%PDF-1.7\n");
        let mut offsets = std::collections::HashMap::new();
        for (num, body) in &objs {
            offsets.insert(*num, out.len());
            out.extend_from_slice(format!("{num} 0 obj\n").as_bytes());
            out.extend_from_slice(body);
            out.extend_from_slice(b"\nendobj\n");
        }

        let xref_at = out.len();
        out.extend_from_slice(b"xref\n0 8\n");
        out.extend_from_slice(b"0000000000 65535 f \n");
        for i in 1..8u32 {
            let off = offsets[&i];
            out.extend_from_slice(format!("{off:010} 00000 n \n").as_bytes());
        }
        out.extend_from_slice(b"trailer\n");
        out.extend_from_slice(b"<< /Size 8 /Root 1 0 R /Info 7 0 R >>\n");
        out.extend_from_slice(format!("startxref\n{xref_at}\n%%EOF\n").as_bytes());
        out
    }

    /// The XMP packet carried by the synthetic metadata stream.
    const XMP: &[u8] = b"<?xpacket begin=''?><x:xmpmeta selis-test='1'></x:xmpmeta><?xpacket end?>";

    /// The synthetic content stream (plaintext form).
    fn content_of(tag: &str) -> Vec<u8> {
        format!("BT /F1 12 Tf ({tag}) Tj ET").into_bytes()
    }

    struct TempPaths {
        input: std::path::PathBuf,
        output: std::path::PathBuf,
    }

    fn temp_paths(tag: &str) -> TempPaths {
        let dir = std::env::temp_dir().join("selis-protect-test");
        std::fs::create_dir_all(&dir).unwrap();
        let unique = format!("{tag}-{}", std::process::id());
        TempPaths {
            input: dir.join(format!("{unique}.in.pdf")),
            output: dir.join(format!("{unique}.out.pdf")),
        }
    }

    /// Write `src` to the temp input and protect it. Panics on harness I/O
    /// errors; `run`'s outcome flows through the return value.
    fn protect(paths: &TempPaths, src: &[u8], args: ProtectArgs) -> Result<(), CliError> {
        if std::fs::write(&paths.input, src).is_err() {
            panic!("write temp input");
        }
        super::run(
            paths.input.to_str().unwrap(),
            paths.output.to_str().unwrap(),
            args.user_password,
            args.owner_password,
            args.permissions,
            args.encrypt_metadata,
        )
    }

    /// protect() argument bundle with sensible defaults.
    struct ProtectArgs {
        user_password: Option<&'static str>,
        owner_password: Option<&'static str>,
        permissions: Option<&'static str>,
        encrypt_metadata: bool,
    }

    impl Default for ProtectArgs {
        fn default() -> Self {
            Self {
                user_password: Some("secret"),
                owner_password: Some("owner"),
                permissions: None,
                encrypt_metadata: true,
            }
        }
    }

    /// The /Encrypt dictionary of a protected file, plus the file key that
    /// its user password authenticates to.
    fn parse_output(bytes: &[u8]) -> (selis_pdf_cos::encrypt::EncryptInfo, Vec<u8>) {
        let budget = Budget::unlimited();
        let mut g = budget.guard();
        let sx = xref::find_startxref(bytes, 4096).unwrap_or(0);
        let doc = parse_revisions(bytes, sx, &budget, &mut g).unwrap();
        let rev = doc.revisions().last().unwrap();
        let info = encrypt::parse_encrypt(bytes, rev.encrypt, &budget, &mut g)
            .unwrap()
            .expect("/Encrypt present");
        let id0 = encrypt::document_id(&rev.trailer);
        let key = encrypt::authenticate(&info, &id0, b"secret").expect("user password");
        (info, key)
    }

    /// The raw stream body of object `num` in `bytes` (an unfiltered lookup —
    /// the object is written with classic xref, so a direct resolve works).
    fn stream_data(bytes: &[u8], num: u32) -> Option<Vec<u8>> {
        let budget = Budget::unlimited();
        let mut g = budget.guard();
        match super::resolve_number(bytes, num, &budget, &mut g) {
            Ok(Obj::Stream { data, .. }) => Some(data.to_vec()),
            _ => None,
        }
    }

    #[test]
    fn parse_permissions_grants_named_bits() {
        assert_eq!(
            super::parse_permissions("print,copy").unwrap(),
            BIT_PRINT | BIT_COPY
        );
        assert_eq!(
            super::parse_permissions("all-of-nothing").unwrap_err(),
            "unknown permission `all-of-nothing` (valid: print, modify, copy, annotate)"
        );
        assert_eq!(super::parse_permissions("none").unwrap(), 0);
        assert_eq!(
            super::parse_permissions("print,modify,copy,annotate").unwrap(),
            BIT_PRINT | BIT_MODIFY | BIT_COPY | BIT_ANNOTATE
        );
        assert!(super::parse_permissions("").is_err());
    }

    #[test]
    fn compute_p_keeps_reserved_bits_conformant() {
        // All granted: bits 1-2 zero, everything else set (Table 22 baseline).
        assert_eq!(
            super::compute_p(BIT_PRINT | BIT_MODIFY | BIT_COPY | BIT_ANNOTATE),
            0xFFFF_FFFC
        );
        // Deny copy + annotate: their bits clear, reserved bits untouched.
        assert_eq!(
            super::compute_p(BIT_PRINT | BIT_MODIFY),
            0xFFFF_FFFC & !(BIT_COPY | BIT_ANNOTATE)
        );
        // None granted: only the four exposed bits clear.
        assert_eq!(super::compute_p(0), 0xFFFF_FFC0);
    }

    #[test]
    fn round_trip_user_password_decrypts_and_perms_verify() {
        let paths = temp_paths("round-trip");
        let content = content_of("round-trip");
        protect(&paths, &build_plain_pdf(&content), ProtectArgs::default()).expect("protect");
        let out = std::fs::read(&paths.output).unwrap();

        // The plaintext markers must all be gone from the raw bytes: the
        // content stream, the Info strings, and the XMP packet are encrypted.
        let raw = String::from_utf8_lossy(&out);
        assert!(!raw.contains("(round-trip)"), "content stream encrypted");
        assert!(!raw.contains("secret-title"), "Info strings encrypted");
        assert!(!raw.contains("x:xmpmeta"), "XMP encrypted by default");

        // Wrong password: no key, ever (never a partial decrypt).
        let info = {
            let budget = Budget::unlimited();
            let mut g = budget.guard();
            let sx = xref::find_startxref(&out, 4096).unwrap_or(0);
            let doc = parse_revisions(&out, sx, &budget, &mut g).unwrap();
            let rev = doc.revisions().last().unwrap();
            encrypt::parse_encrypt(&out, rev.encrypt, &budget, &mut g)
                .unwrap()
                .expect("/Encrypt present")
        };
        assert!(
            encrypt::authenticate(&info, &document_id_of(&out), b"wrong-pw").is_none(),
            "wrong password must not authenticate"
        );

        // Correct password: authenticates and the content decrypts to the
        // exact plaintext.
        let (info, key) = parse_output(&out);
        assert_eq!(info.r, 6, "revision 6");
        assert_eq!(info.v, 5, "AES-256");
        let ct = stream_data(&out, 5).expect("content stream");
        let plain = selis_crypto::decrypt_data(&key, 5, 0, &ct, 6, true);
        assert_eq!(plain, content, "content round-trips byte-exactly");

        // /Perms verifies against the file key and the requested /P; a
        // different /P is rejected.
        assert!(selis_crypto::verify_perms_r6(
            info.p,
            &key,
            &info.perms,
            true
        ));
        assert!(!selis_crypto::verify_perms_r6(
            info.p & !BIT_COPY,
            &key,
            &info.perms,
            true
        ));
    }

    /// The /ID[0] of a document's bytes.
    fn document_id_of(bytes: &[u8]) -> Vec<u8> {
        let budget = Budget::unlimited();
        let mut g = budget.guard();
        let sx = xref::find_startxref(bytes, 4096).unwrap_or(0);
        let doc = parse_revisions(bytes, sx, &budget, &mut g).unwrap();
        encrypt::document_id(&doc.revisions().last().unwrap().trailer)
    }

    #[test]
    fn wrong_password_unlock_fails_cleanly_and_writes_nothing() {
        let paths = temp_paths("wrong-pw");
        protect(
            &paths,
            &build_plain_pdf(&content_of("wrong")),
            ProtectArgs::default(),
        )
        .expect("protect");
        let out_path = paths.output.parent().unwrap().join("wrong-pw-unlocked.pdf");
        let _ = std::fs::remove_file(&out_path);
        let err = crate::unlock::run(
            paths.output.to_str().unwrap(),
            out_path.to_str().unwrap(),
            Some("not-the-password"),
        )
        .expect_err("wrong password refused");
        assert!(
            err.to_string().contains("WRONG_PASSWORD"),
            "typed WRONG_PASSWORD expected: {err}"
        );
        assert!(
            !out_path.exists(),
            "a wrong password must never produce output"
        );
    }

    #[test]
    fn unlock_with_user_password_round_trips() {
        let paths = temp_paths("unlock-rt");
        protect(
            &paths,
            &build_plain_pdf(&content_of("unlock-rt")),
            ProtectArgs::default(),
        )
        .expect("protect");
        let out_path = paths
            .output
            .parent()
            .unwrap()
            .join("unlock-rt-unlocked.pdf");
        crate::unlock::run(
            paths.output.to_str().unwrap(),
            out_path.to_str().unwrap(),
            Some("secret"),
        )
        .expect("unlock with the user password");
        let unlocked = std::fs::read(&out_path).unwrap();
        assert_eq!(
            stream_data(&unlocked, 5).unwrap(),
            content_of("unlock-rt"),
            "decrypted content matches"
        );
        let budget = Budget::profile(Surface::Viewer);
        assert!(
            selis_pdf_engine::Session::open(unlocked, &budget).is_ok(),
            "unlocked output opens"
        );
    }

    #[test]
    fn already_encrypted_is_refused_typed() {
        let paths = temp_paths("already");
        protect(
            &paths,
            &build_plain_pdf(&content_of("already")),
            ProtectArgs::default(),
        )
        .expect("first protect");
        let err = super::run(
            paths.output.to_str().unwrap(),
            paths
                .output
                .parent()
                .unwrap()
                .join("already-second.pdf")
                .to_str()
                .unwrap(),
            Some("secret"),
            Some("owner"),
            None,
            true,
        )
        .expect_err("second protect refused");
        let msg = err.to_string();
        assert!(
            msg.contains("E1806") || msg.contains("ALREADY_ENCRYPTED"),
            "typed ALREADY_ENCRYPTED expected: {msg}"
        );
    }

    #[test]
    fn encrypt_metadata_false_leaves_xmp_in_the_clear() {
        let paths = temp_paths("encmeta");
        protect(
            &paths,
            &build_plain_pdf(&content_of("encmeta")),
            ProtectArgs {
                encrypt_metadata: false,
                ..ProtectArgs::default()
            },
        )
        .expect("protect");
        let out = std::fs::read(&paths.output).unwrap();
        let raw = String::from_utf8_lossy(&out);
        assert!(
            raw.contains("x:xmpmeta"),
            "the XMP stream stays plaintext under /EncryptMetadata false"
        );
        assert!(
            !raw.contains("(encmeta)"),
            "the content stream is still encrypted"
        );
        // The /Encrypt dict carries the flag.
        let budget = Budget::unlimited();
        let mut g = budget.guard();
        let sx = xref::find_startxref(&out, 4096).unwrap_or(0);
        let doc = parse_revisions(&out, sx, &budget, &mut g).unwrap();
        let rev = doc.revisions().last().unwrap();
        let info = encrypt::parse_encrypt(&out, rev.encrypt, &budget, &mut g)
            .unwrap()
            .unwrap();
        assert!(!info.encrypt_metadata, "flag recorded in /Encrypt");
    }

    #[test]
    fn encrypt_metadata_true_encrypts_xmp() {
        let paths = temp_paths("encmeta-true");
        protect(
            &paths,
            &build_plain_pdf(&content_of("encmeta-true")),
            ProtectArgs {
                encrypt_metadata: true,
                ..ProtectArgs::default()
            },
        )
        .expect("protect");
        let out = std::fs::read(&paths.output).unwrap();
        let raw = String::from_utf8_lossy(&out);
        assert!(!raw.contains("x:xmpmeta"), "XMP encrypted by default");
    }

    #[test]
    fn permission_bits_land_in_p_and_perms() {
        let paths = temp_paths("bits");
        protect(
            &paths,
            &build_plain_pdf(&content_of("bits")),
            ProtectArgs {
                permissions: Some("print,annotate"),
                ..ProtectArgs::default()
            },
        )
        .expect("protect");
        let out = std::fs::read(&paths.output).unwrap();
        let (info, key) = parse_output(&out);
        let expected = super::compute_p(BIT_PRINT | BIT_ANNOTATE);
        assert_eq!(info.p, expected, "/P matches the requested bits");
        assert!(
            selis_crypto::verify_perms_r6(info.p, &key, &info.perms, info.encrypt_metadata),
            "/Perms verifies against the file key"
        );
        // The trailer /P is a signed 32-bit integer with the same bit pattern.
        let raw = String::from_utf8_lossy(&out);
        let signed = i32::from_ne_bytes(expected.to_ne_bytes());
        let tail = &raw[raw.len().saturating_sub(2000)..];
        assert!(
            tail.contains(&format!("/P {signed}")),
            "signed /P written: {tail}"
        );
    }

    #[test]
    fn owner_password_authenticates_and_defaults_to_user() {
        // Separate owner password works.
        let paths = temp_paths("owner");
        protect(
            &paths,
            &build_plain_pdf(&content_of("owner")),
            ProtectArgs::default(),
        )
        .expect("protect");
        let out = std::fs::read(&paths.output).unwrap();
        let info = {
            let budget = Budget::unlimited();
            let mut g = budget.guard();
            let sx = xref::find_startxref(&out, 4096).unwrap_or(0);
            let doc = parse_revisions(&out, sx, &budget, &mut g).unwrap();
            encrypt::parse_encrypt(
                &out,
                doc.revisions().last().unwrap().encrypt,
                &budget,
                &mut g,
            )
            .unwrap()
            .unwrap()
        };
        let id0 = document_id_of(&out);
        assert!(
            encrypt::authenticate(&info, &id0, b"owner").is_some(),
            "owner password authenticates"
        );
        assert!(
            encrypt::authenticate(&info, &id0, b"secret").is_some(),
            "user password authenticates"
        );

        // Owner defaults to the user password when omitted.
        let paths2 = temp_paths("owner-default");
        protect(
            &paths2,
            &build_plain_pdf(&content_of("owner-default")),
            ProtectArgs {
                owner_password: None,
                ..ProtectArgs::default()
            },
        )
        .expect("protect");
        let out2 = std::fs::read(&paths2.output).unwrap();
        let info2 = {
            let budget = Budget::unlimited();
            let mut g = budget.guard();
            let sx = xref::find_startxref(&out2, 4096).unwrap_or(0);
            let doc = parse_revisions(&out2, sx, &budget, &mut g).unwrap();
            encrypt::parse_encrypt(
                &out2,
                doc.revisions().last().unwrap().encrypt,
                &budget,
                &mut g,
            )
            .unwrap()
            .unwrap()
        };
        assert!(
            encrypt::authenticate(&info2, &document_id_of(&out2), b"secret").is_some(),
            "the user password also opens as the owner"
        );
    }

    #[test]
    fn empty_user_password_opens_in_the_engine() {
        // Owner-password-only documents (the "restrict editing" flow) open
        // with no password and decrypt fully in the engine.
        let paths = temp_paths("empty-user");
        protect(
            &paths,
            &build_plain_pdf(&content_of("empty-user")),
            ProtectArgs {
                user_password: None,
                owner_password: Some("owner-only"),
                permissions: Some("print"),
                encrypt_metadata: true,
            },
        )
        .expect("protect");
        let out = std::fs::read(&paths.output).unwrap();
        let budget = Budget::profile(Surface::Viewer);
        let session = selis_pdf_engine::Session::open(out, &budget).expect("opens");
        assert_eq!(session.len(), 1, "one page");
    }

    #[test]
    fn long_passwords_are_truncated_per_algorithm_2a() {
        let long_pw = "p".repeat(300);
        let paths = temp_paths("long-pw");
        let src = build_plain_pdf(&content_of("long-pw"));
        std::fs::write(&paths.input, &src).unwrap();
        super::run(
            paths.input.to_str().unwrap(),
            paths.output.to_str().unwrap(),
            Some(&long_pw),
            None,
            None,
            true,
        )
        .expect("protect with a 300-byte password");
        let out = std::fs::read(&paths.output).unwrap();
        // Authentication with the full 300-byte password must succeed: the
        // credential was truncated to 127 bytes at write time (Algorithm 2.A).
        let info = {
            let budget = Budget::unlimited();
            let mut g = budget.guard();
            let sx = xref::find_startxref(&out, 4096).unwrap_or(0);
            let doc = parse_revisions(&out, sx, &budget, &mut g).unwrap();
            encrypt::parse_encrypt(
                &out,
                doc.revisions().last().unwrap().encrypt,
                &budget,
                &mut g,
            )
            .unwrap()
            .unwrap()
        };
        assert!(
            encrypt::authenticate(&info, &document_id_of(&out), long_pw.as_bytes()).is_some(),
            "a >127-byte password still authenticates"
        );
    }

    #[test]
    fn id_is_fresh_per_document() {
        let paths_a = temp_paths("id-a");
        let paths_b = temp_paths("id-b");
        let src = build_plain_pdf(&content_of("id"));
        protect(&paths_a, &src, ProtectArgs::default()).expect("protect a");
        protect(&paths_b, &src, ProtectArgs::default()).expect("protect b");
        let id_a = document_id_of(&std::fs::read(&paths_a.output).unwrap());
        let id_b = document_id_of(&std::fs::read(&paths_b.output).unwrap());
        assert_eq!(id_a.len(), 16, "/ID is 16 bytes");
        assert_ne!(id_a, id_b, "CSPRNG /ID differs per document");
    }

    // ── Property test (DoD template) ─────────────────────────────────────
    //
    // Over generated passwords, permission subsets, and page counts: the
    // output parses, authenticates the user password, rejects a wrong one,
    // verifies /Perms, and decrypts the content stream byte-exactly.
    //
    // The case count is capped because the revision-6 KDF (Algorithm 2.B) is
    // deliberately expensive — a handful of hardened-hash runs per protect.

    proptest::proptest! {
        #![proptest_config(proptest::prelude::ProptestConfig::with_cases(48))]

        #[test]
        fn protect_round_trips_generated_inputs(
            user_pw in proptest::string::string_regex("[a-zA-Z0-9]{0,12}").unwrap(),
            owner_pw in proptest::string::string_regex("[a-zA-Z0-9]{1,12}").unwrap(),
            grant_print in proptest::bool::ANY,
            grant_copy in proptest::bool::ANY,
            tag in proptest::string::string_regex("[a-z]{4,8}").unwrap(),
        ) {
            let dir = std::env::temp_dir().join("selis-protect-prop");
            std::fs::create_dir_all(&dir).unwrap();
            let stem = format!("prop-{}-{}", tag, std::process::id());
            let input = dir.join(format!("{stem}.in.pdf"));
            let output = dir.join(format!("{stem}.out.pdf"));
            let content = content_of(&tag);

            let spec = match (grant_print, grant_copy) {
                (true, true) => None,
                (true, false) => Some("print,modify,annotate"),
                (false, true) => Some("copy,modify,annotate"),
                (false, false) => Some("modify,annotate"),
            };
            std::fs::write(&input, build_plain_pdf(&content)).unwrap();
            super::run(
                input.to_str().unwrap(),
                output.to_str().unwrap(),
                Some(&user_pw),
                Some(&owner_pw),
                spec,
                true,
            )
            .unwrap_or_else(|e| panic!("protect failed: {e}"));
            let out = std::fs::read(&output).unwrap();

            // Parse the /Encrypt dict and the content ciphertext.
            let budget = Budget::unlimited();
            let mut g = budget.guard();
            let sx = xref::find_startxref(&out, 4096).unwrap_or(0);
            let doc = parse_revisions(&out, sx, &budget, &mut g).unwrap();
            let rev = doc.revisions().last().unwrap();
            let info = encrypt::parse_encrypt(&out, rev.encrypt, &budget, &mut g)
                .unwrap()
                .unwrap();
            let id0 = encrypt::document_id(&rev.trailer);

            // /P matches the requested grant set.
            let mut granted = BIT_MODIFY | BIT_ANNOTATE;
            if grant_print { granted |= BIT_PRINT; }
            if grant_copy { granted |= BIT_COPY; }
            assert_eq!(info.p, super::compute_p(granted), "/P matches the spec");

            // The user password authenticates; a definitely-wrong one does not.
            let key = encrypt::authenticate(&info, &id0, user_pw.as_bytes())
                .unwrap_or_else(|| panic!("user password must authenticate"));
            let wrong = format!("zz-{}-wrong", tag);
            if wrong != user_pw && wrong != owner_pw {
                assert!(encrypt::authenticate(&info, &id0, wrong.as_bytes()).is_none());
            }

            // /Perms verifies against the file key.
            assert!(selis_crypto::verify_perms_r6(info.p, &key, &info.perms, true));

            // The content stream decrypts byte-exactly under the file key.
            let ct = match super::resolve_number(&out, 5, &budget, &mut g).unwrap() {
                Obj::Stream { data, .. } => data.to_vec(),
                other => panic!("object 5 not a stream: {other:?}"),
            };
            assert_eq!(selis_crypto::decrypt_data(&key, 5, 0, &ct, 6, true), content);
        }
    }
}
