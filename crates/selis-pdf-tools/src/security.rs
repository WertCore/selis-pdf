//! The password and permission operations (SL-1A.TOOL.04, .05, .06), shared
//! by the CLI and the browser security chunk.
//!
//! Three operations, one implementation each:
//!
//! * [`unlock`] authenticates a password, decrypts every stream and string
//!   through the engine's resolver, and rewrites the document with no
//!   `/Encrypt` entry;
//! * [`protect`] adds AES-256 / revision 6 encryption (ADR-P0019 — the only
//!   handler Selis writes) and the owner's permission bits;
//! * [`clear_permissions`] lifts the permission bits, which only the owner
//!   password can authorise (SL-1.ENC.04).
//!
//! # Where the randomness comes from
//!
//! `protect` needs a 256-bit file key and four 8-byte salts. It does not draw
//! them: the caller hands over one 64-byte [`R6Material`] blob. The CLI fills
//! it from the operating system; a web page fills it from
//! `crypto.getRandomValues`. Nothing in this module can silently substitute a
//! predictable value, and the rest of the format's random-looking fields — the
//! per-object IVs, the `/Perms` fill bytes, the document `/ID` — are derived
//! from the file key instead of drawn, which is what lets a guest with no
//! entropy source at all write a conformant encrypted file.
//!
//! # Why the walk uses the engine's resolver
//!
//! Decrypting on the way out is the one step that has to agree with every other
//! PDF reader on earth, byte for byte. It runs through `selis_pdf_doc`'s
//! resolver — the same code path the viewer uses — rather than a second
//! decryption implementation written for this module. The oracle evidence
//! recorded in `pdf-plan/11A-PHASE-1A-tools.md` (qpdf, PDFium, poppler) is
//! measured against exactly this path.

use std::collections::BTreeSet;

use selis_pdf_cos::doc_writer::{write_incremental_update, write_objects_as_document_with_trailer};
use selis_pdf_cos::encrypt::{self, DecryptPolicy, EncryptInfo};
use selis_pdf_cos::{parse_revisions, xref, Obj, Ref};
use selis_pdf_doc::Resolver;
use selis_sandbox::{Budget, Surface};

/// Re-exported so a binding can name the key material it must supply without
/// depending on `selis-pdf-cos`.
pub use selis_pdf_cos::encrypt::R6Material;

use crate::verify_report::Verification;
use crate::{read_file, CliError, CliResult};

/// All permission bits granted, restricted only by the spec's reserved-bit
/// rules: bits 1–2 shall be 0, bits 7–8 and 13–32 shall be 1 (ISO 32000-1
/// Table 22). Denying a permission clears its bit from this baseline.
const P_BASELINE: u32 = 0xFFFF_FFFC;

/// Bit 3 — print (ISO 32000-1 Table 22).
pub const BIT_PRINT: u32 = 0x0000_0004;
/// Bit 4 — modify the document contents.
pub const BIT_MODIFY: u32 = 0x0000_0008;
/// Bit 5 — copy / extract text and graphics.
pub const BIT_COPY: u32 = 0x0000_0010;
/// Bit 6 — add or modify annotations and form fields.
pub const BIT_ANNOTATE: u32 = 0x0000_0020;

/// The bits the public surface exposes, with their names. Everything else in
/// the baseline stays granted — including accessibility extraction (bit 10),
/// which is deliberately not restrictable.
const EXPOSED: &[(&str, u32)] = &[
    ("print", BIT_PRINT),
    ("modify", BIT_MODIFY),
    ("copy", BIT_COPY),
    ("annotate", BIT_ANNOTATE),
];

/// The grant mask for a document that restricts nothing.
pub const ALL_GRANTED: u32 = BIT_PRINT | BIT_MODIFY | BIT_COPY | BIT_ANNOTATE;

/// All permissions granted (R2–4): bits 0–1 cleared, all permission and
/// reserved bits set.
const ALL_PERMS_R2_4: u32 = 0xFFFF_FFFC;

/// All permissions granted (R5/6): every bit set.
const ALL_PERMS_R5_6: u32 = 0xFFFF_FFFF;

/// Resolve a permission spec into the exposed-bit grant mask.
///
/// The spec is a comma-separated list of permissions to **grant** — `print`,
/// `modify`, `copy`, `annotate` — or the token `none`. Anything not listed is
/// denied. Bits the surface does not expose (fill forms, accessibility,
/// assemble, high-resolution print) are always granted.
///
/// # Errors
///
/// A plain message naming the valid vocabulary on an unknown token or an empty
/// spec — a caller mistake, not document damage.
pub fn parse_permissions(spec: &str) -> Result<u32, String> {
    let spec = spec.trim();
    if spec.is_empty() {
        return Err("empty permission spec (expected e.g. `print,copy` or `none`)".to_string());
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
pub fn compute_p(granted: u32) -> u32 {
    let exposed_bits: u32 = EXPOSED.iter().map(|(_, bit)| bit).fold(0, |a, b| a | b);
    P_BASELINE & !(exposed_bits & !granted)
}

/// The exposed names a grant mask actually grants, in `EXPOSED` order.
///
/// Both bindings use this for the audit line and the on-page summary, so the
/// words a user reads are derived from the bits that were written rather than
/// from what the caller asked for.
#[must_use]
pub fn granted_names(granted: u32) -> Vec<&'static str> {
    EXPOSED
        .iter()
        .filter(|(_, bit)| (granted & bit) != 0)
        .map(|(name, _)| *name)
        .collect()
}

/// The exposed permissions a written `/P` value denies, in `EXPOSED` order.
///
/// A `/P` bit of 0 means "not granted" (ISO 32000-1 Table 22), so this reads
/// the flag back out of a document rather than trusting what its owner asked
/// for. [`clear_permissions`] reports these as the restriction it lifted.
#[must_use]
pub fn denied_permissions(p: u32) -> Vec<&'static str> {
    EXPOSED
        .iter()
        .filter(|(_, bit)| (p & bit) == 0)
        .map(|(name, _)| *name)
        .collect()
}

/// Algorithm 2.A step (a): a UTF-8 password is truncated to 127 bytes before
/// hashing. Truncation happens here so the written credentials and any later
/// authentication agree byte-for-byte.
fn clamp_password(pw: &str) -> &[u8] {
    let end = pw.len().min(127);
    pw.as_bytes().get(..end).unwrap_or(pw.as_bytes())
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

/// A deterministic 16-byte file identifier from the source contents (the same
/// FNV-2x64 construction the merge tool uses for a fresh `/ID`).
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

/// Open the latest revision of `src`, or a typed error naming the path.
fn open_last_revision(
    src: &[u8],
    path: &str,
    budget: &Budget,
    g: &mut selis_sandbox::BudgetGuard<'_>,
) -> CliResult<selis_pdf_cos::Doc> {
    let startxref = xref::find_startxref(src, 4096).unwrap_or(0);
    parse_revisions(src, startxref, budget, g).map_err(|e| CliError(format!("{path}: {e}")))
}

// ── unlock ────────────────────────────────────────────────────────────────

/// What an [`unlock`] call did to the document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnlockOutcome {
    /// False when the input carried no `/Encrypt`: the bytes were copied
    /// verbatim and nothing was rewritten.
    pub encrypted: bool,
    /// The verification display (SL-1A.UI.02).
    pub verification: Verification,
}

/// Remove password protection from `path` and write the result to `output`.
///
/// `password` defaults to the empty string — the user password of documents
/// that open without one. This is a full rewrite (WRITE.01), not an incremental
/// append: the encrypted bytes must not survive in the output.
///
/// # Errors
///
/// A typed parse error when the document is damaged, `WRONG_PASSWORD` when the
/// password matches neither the user nor the owner password, and a verification
/// failure if the decrypted output keeps an `/Encrypt`. No output is committed
/// on any of them.
pub fn unlock(path: &str, output: &str, password: Option<&str>) -> CliResult<UnlockOutcome> {
    let src = read_file(path)?;
    let in_bytes = u64::try_from(src.len()).unwrap_or(u64::MAX);
    let budget = Budget::unlimited();
    let clock = crate::shell_clock();
    let mut g = crate::runtime::cli_guard(&budget, &clock);

    let doc = open_last_revision(&src, path, &budget, &mut g)?;
    let rev = doc
        .revisions()
        .last()
        .ok_or_else(|| CliError(format!("{path}: no revisions")))?;

    let policy = match rev.encrypt {
        None => {
            // Not encrypted: copy as-is (verbatim, atomic — the bytes are the
            // user's original file, not writer output to verify). The display
            // says so honestly instead of inventing a verdict.
            crate::write_gate::atomic_write(&src, output, &crate::runtime::token())?;
            return Ok(UnlockOutcome {
                encrypted: false,
                verification: Verification::copied(in_bytes),
            });
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
    // signatures — detect before running and say so.
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
    // fresh deterministic value is derived from the source contents.
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

    // Structural verification (WRITE.05, via the shared gate): the output must
    // reparse, every reference must resolve, and the input's surveyed counts
    // must hold — decryption rewrites no structure, so everything is asserted.
    let observed = selis_pdf_cos::verify::survey(&src, &budget, &mut g)
        .map_err(|e| CliError(format!("{path}: input survey failed: {e}")))?;
    let expected =
        crate::write_gate::expectations_from(&observed, &crate::write_gate::PRESERVE_ALL);
    let verdict = crate::write_gate::write_verified(&bytes, output, &expected, &budget, &mut g)?;

    // The decrypted output must carry no /Encrypt (WRITE.07).
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
    open_model(&bytes, path)?;

    let verification = Verification::from_gate(
        Some(&observed),
        &verdict,
        &expected,
        in_bytes,
        u64::try_from(bytes.len()).unwrap_or(u64::MAX),
    );
    Ok(UnlockOutcome {
        encrypted: true,
        verification,
    })
}

// ── protect ───────────────────────────────────────────────────────────────

/// The credentials and policy for one [`protect`] call.
///
/// `owner_password` defaults to `user_password` at the call site, as Acrobat
/// does; the field stays explicit so no binding can forget that a document
/// whose permissions can be rewritten by anyone is not permission-restricted.
///
/// There is deliberately no `Default`: a stand-in `material` would be a fixed
/// file key, which is the failure this type exists to make impossible.
#[derive(Debug, Clone)]
pub struct ProtectOptions {
    /// The password needed to open the document (empty = opens without one).
    pub user_password: String,
    /// The password needed to change permissions later.
    pub owner_password: String,
    /// The exposed permission bits to grant ([`parse_permissions`]).
    pub granted: u32,
    /// Whether the document-level XMP stream is encrypted.
    pub encrypt_metadata: bool,
    /// The 64 bytes of key material the caller supplied.
    pub material: R6Material,
}

/// The permission bits and credentials an accepted [`protect`] wrote.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProtectOutcome {
    /// The exposed names actually granted, as written into `/P`.
    pub granted: Vec<&'static str>,
    /// The verification display (SL-1A.UI.02).
    pub verification: Verification,
}

/// Add AES-256 / revision 6 password protection to `path`, writing to `output`.
///
/// A full rewrite (WRITE.01): encrypting changes every stream and string body,
/// so the output cannot share bytes with the source. An already-encrypted input
/// is refused with the typed `ALREADY_ENCRYPTED` — removing a password is
/// [`unlock`].
///
/// # Errors
///
/// `ALREADY_ENCRYPTED`, a typed parse error on damaged input, a plain usage
/// error when both passwords are empty or the metadata-exemption request needs
/// a direct `/Metadata` stream, and a verification failure if the written file
/// does not re-read with the requested `/P`, `/EncryptMetadata`, and both
/// passwords. Nothing is committed on any of them.
pub fn protect(path: &str, output: &str, options: &ProtectOptions) -> CliResult<ProtectOutcome> {
    if options.user_password.is_empty() && options.owner_password.is_empty() {
        return Err(CliError(
            "no passwords given: a document with neither a user nor an owner \
             password is not protected"
                .to_string(),
        ));
    }
    let p = compute_p(options.granted);

    let src = read_file(path)?;
    let in_bytes = u64::try_from(src.len()).unwrap_or(u64::MAX);
    let budget = Budget::unlimited();
    let clock = crate::shell_clock();
    let mut g = crate::runtime::cli_guard(&budget, &clock);

    let doc = open_last_revision(&src, path, &budget, &mut g)?;
    let rev = doc
        .revisions()
        .last()
        .ok_or_else(|| CliError(format!("{path}: no revisions")))?;

    if rev.encrypt.is_some() {
        let mut ctx = selis_error::Ctx::new();
        ctx.detail = Some(format!(
            "{path}: remove the existing password before protecting it again"
        ));
        return Err(CliError::from(selis_error::Error::with(
            selis_error::Code::AlreadyEncrypted,
            ctx,
        )));
    }
    let root = rev
        .root
        .ok_or_else(|| CliError(format!("{path}: no /Root in trailer")))?;

    // TOOL.04 engineering note: a full rewrite invalidates digital
    // signatures — detect before running and say so.
    crate::tools::warn_if_digital_signature(&src, &doc, &budget, &mut g, path);

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
    if !options.encrypt_metadata {
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
                    "{path}: /Metadata is a direct stream; unencrypted metadata \
                     needs an indirect /Metadata reference"
                )));
            }
            None => {}
        }
    }

    // The R6 credentials come from the caller. The document /ID follows from
    // the file key (ISO 32000-1 §14.4 lets a writer pick it; revision 6 never
    // hashes it into the key).
    let id = options.material.document_id();
    let (mut info, file_key) = EncryptInfo::new_r6(
        clamp_password(&options.user_password),
        clamp_password(&options.owner_password),
        p,
        &options.material,
    );
    info.encrypt_metadata = options.encrypt_metadata;
    // new_r6 bakes the /EncryptMetadata marker into /Perms; recompute the
    // blob so the marker matches the caller's flag.
    info.perms = selis_crypto::compute_perms_r6(p, &file_key, options.encrypt_metadata);

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

    let observed = selis_pdf_cos::verify::survey(&src, &budget, &mut g)
        .map_err(|e| CliError(format!("{path}: input survey failed: {e}")))?;
    let expected =
        crate::write_gate::expectations_from(&observed, &crate::write_gate::PRESERVE_ALL);

    // WRITE.05 (protect-specific): the re-parsed output must carry a
    // revision-6 /Encrypt whose /P and /EncryptMetadata match the request, both
    // passwords must authenticate, /Perms must verify against the file key, the
    // exempted metadata (if any) must still be plaintext, and the document
    // model must open.
    verify_protect_output(
        &bytes,
        &ProtectCheck {
            root,
            p,
            encrypt_metadata: options.encrypt_metadata,
            user_pw: &options.user_password,
            owner_pw: &options.owner_password,
            metadata_exempt,
        },
        path,
    )?;

    // Commit through the shared gate: structural verification + atomic write.
    let verdict = crate::write_gate::write_verified(&bytes, output, &expected, &budget, &mut g)?;

    Ok(ProtectOutcome {
        granted: granted_names(options.granted),
        verification: Verification::from_gate(
            Some(&observed),
            &verdict,
            &expected,
            in_bytes,
            u64::try_from(bytes.len()).unwrap_or(u64::MAX),
        ),
    })
}

/// The expectations the WRITE.05 verification checks protect's output against.
struct ProtectCheck<'a> {
    root: Ref,
    p: u32,
    encrypt_metadata: bool,
    user_pw: &'a str,
    owner_pw: &'a str,
    metadata_exempt: Option<u32>,
}

/// WRITE.05 structural verification of the encrypted output.
///
/// `bytes` is engine-written, not document-derived, so every failure here is a
/// writer bug rather than a document problem: each check fails the tool with an
/// `output failed verification` error and nothing is committed.
fn verify_protect_output(bytes: &[u8], expect: &ProtectCheck<'_>, path: &str) -> CliResult<()> {
    let fail = |what: String| CliError(format!("{path}: output failed verification ({what})"));
    let budget = Budget::unlimited();
    let clock = crate::shell_clock();
    let mut g = crate::runtime::cli_guard(&budget, &clock);
    let sx = xref::find_startxref(bytes, 4096).unwrap_or(0);
    let parsed =
        parse_revisions(bytes, sx, &budget, &mut g).map_err(|e| fail(format!("re-parse: {e}")))?;
    let rev = parsed
        .revisions()
        .last()
        .ok_or_else(|| fail("no revisions".to_string()))?;
    if rev.root != Some(expect.root) {
        return Err(fail("/Root not preserved".to_string()));
    }
    let enc_ref = rev
        .encrypt
        .ok_or_else(|| fail("/Encrypt missing".to_string()))?;
    let info = encrypt::parse_encrypt(bytes, Some(enc_ref), &budget, &mut g)
        .map_err(|e| fail(format!("/Encrypt unreadable: {e}")))?
        .ok_or_else(|| fail("/Encrypt unreadable".to_string()))?;
    if info.r != 6 || info.v != 5 {
        return Err(fail(format!(
            "expected revision 6 / AES-256, got R{} V{}",
            info.r, info.v
        )));
    }
    if info.p != expect.p {
        return Err(fail("/P mismatch".to_string()));
    }
    if info.encrypt_metadata != expect.encrypt_metadata {
        return Err(fail("/EncryptMetadata mismatch".to_string()));
    }
    let id0 = encrypt::document_id(&rev.trailer);
    if id0.is_empty() {
        return Err(fail("/ID missing".to_string()));
    }
    let key = encrypt::authenticate(&info, &id0, expect.user_pw.as_bytes())
        .ok_or_else(|| fail("user password does not authenticate".to_string()))?;
    if !selis_crypto::verify_perms_r6(info.p, &key, &info.perms, info.encrypt_metadata) {
        return Err(fail("/Perms does not verify".to_string()));
    }
    encrypt::authenticate(&info, &id0, expect.owner_pw.as_bytes())
        .ok_or_else(|| fail("owner password does not authenticate".to_string()))?;
    if let Some(num) = expect.metadata_exempt {
        let ok = matches!(
            resolve_number(bytes, num, &budget, &mut g),
            Ok(Obj::Stream { .. })
        );
        if !ok {
            return Err(fail("exempted /Metadata stream unreadable".to_string()));
        }
    }
    // With a non-empty user password the session opens tolerantly without the
    // key — this check is structural reachability, not decryption (the decrypt
    // path is proven by the authenticate and /Perms checks above).
    open_model(bytes, path)
}

/// Resolve one object by number across revisions (used for the metadata
/// exemption check on the re-parsed output).
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

/// WRITE.07: the output must build a usable document model. The typed error is
/// propagated, not swallowed (SL-1A.UI.06).
fn open_model(bytes: &[u8], path: &str) -> CliResult<()> {
    let doc_budget = Budget::profile(Surface::Viewer);
    let clock = crate::shell_clock();
    selis_pdf_engine::Session::open(bytes.to_vec(), &doc_budget, &clock)
        .map_err(|e| CliError(format!("{path}: {e}")))?;
    Ok(())
}

// ── clear permissions ─────────────────────────────────────────────────────

/// What an accepted [`clear_permissions`] call did, in the words the audit line
/// and the on-page summary both use.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClearOutcome {
    /// How the rewrite was performed, naming the revision it applied to.
    pub summary: String,
    /// The exposed permissions the document denied before the rewrite — what
    /// actually changed. Empty means the flag already granted everything the
    /// surface exposes, and the rewrite normalised `/P` and `/Perms` only.
    pub previously_denied: Vec<&'static str>,
    /// The verification display (SL-1A.UI.02).
    pub verification: Verification,
}

/// Lift the permission restrictions on `path`, writing to `output`.
///
/// Only the owner password authorises this (SL-1.ENC.04) — it is an explicit
/// override, not a way to open a document. A document that opens without a
/// password but restricts printing, copying, or editing is the case this tool
/// exists for.
///
/// For R5/6 the file key does not depend on `/P`, so the content stays
/// encrypted at rest and only `/P` and `/Perms` change, appended as a new
/// revision (WRITE.02). For R2–4 `/P` feeds the key derivation, so a new key is
/// derived from the cleared `/P` and every stream and string is re-encrypted.
///
/// # Errors
///
/// `WRONG_OWNER_PASSWORD` when the password does not authenticate as the owner,
/// `WRONG_PASSWORD` when it cannot be recovered to a file key, an error on an
/// unencrypted or unreadable input, and a verification failure if the output
/// loses `/Root`. Nothing is committed on any of them.
pub fn clear_permissions(
    path: &str,
    output: &str,
    password: Option<&str>,
) -> CliResult<ClearOutcome> {
    let pw = password.unwrap_or("");
    let src = read_file(path)?;
    let budget = Budget::unlimited();
    let clock = crate::shell_clock();
    let mut g = crate::runtime::cli_guard(&budget, &clock);

    let doc = open_last_revision(&src, path, &budget, &mut g)?;
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

    if info.r >= 5 {
        clear_r56(
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
        clear_r24(
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
    }
}

/// R5/6: `/P` does not feed the key derivation, so the content stays encrypted
/// at rest under the same file key. Only `/P` and `/Perms` change, and the new
/// `/Encrypt` object is appended as a new revision (WRITE.02).
// A permission rewrite needs the whole encryption record end to end: the source
// bytes, the `/Encrypt` dictionary, the file key, the root, the two paths, the
// trailer pairs, and the budget handles (SL-0.WS.11).
#[allow(clippy::too_many_arguments)]
fn clear_r56(
    src: &[u8],
    info: &encrypt::EncryptInfo,
    key: &[u8],
    root: Ref,
    path: &str,
    output: &str,
    trailer: &[(selis_bytes::Bytes, Obj)],
    budget: &Budget,
    g: &mut selis_sandbox::BudgetGuard<'_>,
) -> CliResult<ClearOutcome> {
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

    let new_trailer = build_appended_trailer(trailer, enc_num, src);

    let updated = write_incremental_update(src, &[(enc_num, new_encrypt)], &new_trailer, budget, g)
        .map_err(|e| CliError(format!("{path}: write: {e}")))?;

    verify_cleared_output(&updated, root, path)?;

    // WRITE.05 (via the shared gate): the appended revision changes only the
    // /Encrypt dictionary, so every surveyed count must hold.
    let verification = crate::write_gate::verify_all_preserved(
        &updated,
        output,
        src,
        u64::try_from(src.len()).unwrap_or(u64::MAX),
        budget,
        g,
    )?;
    Ok(ClearOutcome {
        summary: "R5/6 incremental: /P and /Perms updated; the content stays \
                  encrypted under the unchanged file key"
            .to_string(),
        previously_denied: denied_permissions(info.p),
        verification,
    })
}

/// R2–4: `/P` feeds the key derivation. Decrypt all content with the old key,
/// derive a new key from the cleared `/P`, recompute `/U`, and re-encrypt every
/// stream and string.
// See `clear_r56`: the R2–4 half needs the same record plus the user password
// it recovers and the old file key it decrypts with (SL-0.WS.11).
#[allow(clippy::too_many_arguments)]
fn clear_r24(
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
) -> CliResult<ClearOutcome> {
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

    // Build the new /Encrypt dict via the canonical builder: mutate the parsed
    // info with the cleared /P and the recomputed /U, then serialize. Per ISO
    // 32000-1 Table 15 the trailer's /Encrypt is an indirect reference, so the
    // dict becomes a fresh object after the walked content.
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

    verify_cleared_output(&bytes, root, path)?;

    // WRITE.05 (via the shared gate): the re-encryption changes no structure,
    // so every surveyed count must hold.
    let verification = crate::write_gate::verify_all_preserved(
        &bytes,
        output,
        src,
        u64::try_from(src.len()).unwrap_or(u64::MAX),
        budget,
        g,
    )?;
    Ok(ClearOutcome {
        summary: format!(
            "R{} full rewrite: content re-encrypted under the /P-cleared key",
            info.r
        ),
        previously_denied: denied_permissions(info.p),
        verification,
    })
}

/// Build the new revision's trailer entries for the R5/6 incremental update.
fn build_appended_trailer(
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

/// WRITE.05: verify the cleared output reparses with the same `/Root` and
/// builds a usable document model.
fn verify_cleared_output(bytes: &[u8], expected_root: Ref, path: &str) -> CliResult<()> {
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
    open_model(bytes, path)
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::arithmetic_side_effects,
        clippy::expect_used,
        clippy::indexing_slicing,
        clippy::panic,
        clippy::unwrap_used
    )]

    use super::*;

    /// A plain (unencrypted) `pages`-page source, written by the same builder
    /// the tools use.
    fn plain_source(pages: usize) -> Vec<u8> {
        let budget = Budget::unlimited();
        let mut g = budget.guard();
        let mut builder = selis_pdf_cos::doc_writer::DocumentBuilder::new();
        for _ in 0..pages {
            builder.add_page(100.0, 100.0, b"q Q");
        }
        builder.write(&budget, &mut g).expect("write plain source")
    }

    /// Key material for the tests. A fixed blob is a test fixture, not a
    /// policy: production callers draw these bytes from the host (see the
    /// module header), and `from_bytes` is the injection point the browser
    /// chunk uses — so exercising it here is the point.
    fn test_material() -> R6Material {
        R6Material::from_bytes(&[0x5A; R6Material::LEN]).expect("64 bytes")
    }

    /// The spec vocabulary is the four exposed bits, `none`, or a refusal.
    #[test]
    fn permission_spec_grants_only_what_it_names() {
        assert_eq!(
            parse_permissions("print,copy").unwrap(),
            BIT_PRINT | BIT_COPY
        );
        assert_eq!(parse_permissions(" NONE ").unwrap(), 0);
        assert_eq!(parse_permissions("Annotate").unwrap(), BIT_ANNOTATE);
        assert_eq!(parse_permissions("modify").unwrap(), BIT_MODIFY);
        assert!(parse_permissions("").is_err());
        assert!(parse_permissions("print,assemble").is_err());
        let message = parse_permissions("fax").expect_err("unknown token");
        assert!(
            message.contains("print"),
            "message lists the vocabulary: {message}"
        );
    }

    /// `/P` keeps the spec's reserved bits in place whatever is denied: bits
    /// 1–2 zero, bits 7–8 and 13–32 set.
    #[test]
    fn compute_p_keeps_the_reserved_bits() {
        for granted in [ALL_GRANTED, 0, BIT_PRINT, BIT_COPY | BIT_ANNOTATE] {
            let p = compute_p(granted);
            assert_eq!(p & 0b11, 0, "bits 1-2 shall be 0");
            assert_eq!(p & 0b1100_0000, 0b1100_0000, "bits 7-8 shall be 1");
            assert_eq!(p >> 12, 0x000F_FFFF, "bits 13-32 shall be 1");
            assert_eq!(
                compute_p(granted) & 0x3C,
                granted & 0x3C,
                "exposed bits follow the grant"
            );
        }
        assert_eq!(compute_p(ALL_GRANTED), P_BASELINE);
        assert_eq!(compute_p(0), P_BASELINE & !0x3C);
    }

    /// The words both bindings report are read back out of the flag, so an
    /// asked-for grant that the writer did not apply cannot be claimed.
    #[test]
    fn granted_and_denied_names_read_the_flag() {
        assert_eq!(
            granted_names(ALL_GRANTED),
            vec!["print", "modify", "copy", "annotate"]
        );
        assert_eq!(granted_names(0), Vec::<&str>::new());
        assert_eq!(
            denied_permissions(compute_p(ALL_GRANTED)),
            Vec::<&str>::new()
        );
        assert_eq!(
            denied_permissions(compute_p(0)),
            vec!["print", "modify", "copy", "annotate"]
        );
        assert_eq!(
            denied_permissions(compute_p(BIT_PRINT | BIT_COPY)),
            vec!["modify", "annotate"]
        );
        // Accessibility (bit 10) and assembly (bit 11) are not restrictable on
        // this surface, so they never appear as denied.
        assert!(!denied_permissions(compute_p(0)).contains(&"accessibility"));
    }

    /// A password longer than the Algorithm 2.A limit is truncated before it
    /// reaches the credential math, so writing and authenticating agree.
    #[test]
    fn clamp_password_stops_at_127_bytes() {
        let short = clamp_password("secret");
        assert_eq!(short, b"secret");
        let long = "u".repeat(200);
        assert_eq!(clamp_password(&long).len(), 127);
        assert_eq!(clamp_password(""), b"");
    }

    /// The browser boundary: an in-memory input protects, and its in-memory
    /// output unlocks again — no filesystem is named on the way.
    ///
    /// The session table is process-wide and Rust runs tests in parallel, so
    /// every test here keeps its own keys and none of them calls `clear()`.
    #[test]
    fn protect_then_unlock_round_trips_in_the_memory_backend() {
        let _session = crate::memory::MemorySession::activate();
        crate::memory::insert_input("rt/in/0", plain_source(3));

        let outcome = protect(
            "rt/in/0",
            "rt/out-protected.pdf",
            &ProtectOptions {
                user_password: "reader".to_string(),
                owner_password: "owner".to_string(),
                granted: parse_permissions("print,copy").unwrap(),
                encrypt_metadata: true,
                material: test_material(),
            },
        )
        .expect("protect in memory");
        assert_eq!(outcome.granted, vec!["print", "copy"]);
        assert_eq!(
            outcome.verification.pages_out, 3,
            "protect keeps every page"
        );
        let protected =
            crate::memory::take_output("rt/out-protected.pdf").expect("protected output");
        assert!(String::from_utf8_lossy(&protected).contains("/Encrypt"));

        crate::memory::insert_input("rt/in/1", protected.clone());
        let unlocked =
            unlock("rt/in/1", "rt/out-unlocked.pdf", Some("reader")).expect("unlock in memory");
        assert!(unlocked.encrypted, "the input carried an /Encrypt");
        assert_eq!(
            unlocked.verification.pages_out, 3,
            "pages survive the round trip"
        );
        let bytes = crate::memory::take_output("rt/out-unlocked.pdf").expect("unlocked output");
        assert!(
            !String::from_utf8_lossy(&bytes).contains("/Encrypt"),
            "the decrypted rewrite drops /Encrypt"
        );
        assert_ne!(bytes, protected, "unlock rewrote the document");
    }

    /// A wrong password is a refusal with no output committed, and a document
    /// that is already encrypted is refused by name rather than re-encrypted.
    #[test]
    fn wrong_password_writes_nothing_and_encrypted_input_is_refused() {
        let _session = crate::memory::MemorySession::activate();
        let options = ProtectOptions {
            user_password: "reader".to_string(),
            owner_password: String::new(),
            granted: ALL_GRANTED,
            encrypt_metadata: true,
            material: test_material(),
        };
        crate::memory::insert_input("rp/in/0", plain_source(1));
        protect("rp/in/0", "rp/out-protected.pdf", &options).expect("protect in memory");
        let protected =
            crate::memory::take_output("rp/out-protected.pdf").expect("protected output");
        crate::memory::insert_input("rp/in/1", protected);

        let err =
            unlock("rp/in/1", "rp/out-unlocked.pdf", Some("wrong")).expect_err("wrong password");
        assert!(err.to_string().contains("WRONG_PASSWORD"), "{err}");
        assert!(
            crate::memory::take_output("rp/out-unlocked.pdf").is_none(),
            "a refused unlock commits nothing"
        );

        let err = protect("rp/in/1", "rp/out-again.pdf", &options).expect_err("already encrypted");
        assert!(
            err.to_string().contains("already password-protected"),
            "the refusal names the existing password: {err}"
        );
        assert!(crate::memory::take_output("rp/out-again.pdf").is_none());
    }

    /// An unencrypted input to `unlock` is a verbatim copy, and the display
    /// says `copied` rather than claiming a rewrite it did not perform.
    #[test]
    fn unlock_of_a_plain_document_copies_verbatim() {
        let _session = crate::memory::MemorySession::activate();
        let src = plain_source(2);
        crate::memory::insert_input("pl/in/0", src.clone());
        let outcome = unlock("pl/in/0", "pl/out.pdf", None).expect("copy as-is");
        assert!(!outcome.encrypted, "nothing was encrypted");
        assert_eq!(outcome.verification.verdict, "copied");
        assert_eq!(
            crate::memory::take_output("pl/out.pdf").expect("copy"),
            src,
            "the bytes of an unencrypted document are copied unchanged"
        );
    }
}
