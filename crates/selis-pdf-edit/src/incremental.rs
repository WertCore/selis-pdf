//! Object-level incremental editing and saving (SL-5.EDIT.03, 23-EDIT-MODEL-SPEC.md §6).
//!
//! The higher-level text edit in this crate rewrites one content stream. This
//! module is the general object-level layer the spec calls for: a typed
//! mutation taxonomy whose every variant lowers to a small set of COS object
//! changes, an incremental writer that appends ONLY those changed objects
//! (keeping the original bytes verbatim as a prefix), and read-back
//! verification against the expected post-mutation model.
//!
//! Invariants this module upholds (23-EDIT-MODEL-SPEC.md):
//! - **I3** (prior revisions): `output[..original.len()] == original`.
//! - **I7** (non-interference): only the objects a mutation declares are
//!   changed; every other object keeps its number, generation and bytes.
//! - **I5** (prefix reachability): `startxref`/`%%EOF` are emitted last.
//!
//! # Budget
//!
//! Every parse, resolve and write is charged to the caller's [`Budget`]; the
//! property suite runs 100 000 sequences under a bounded budget each.
//!
//! # Malformed Input
//!
//! A document with no `startxref`, a leaf object that cannot be resolved, or a
//! mutation naming a nonexistent object is a typed error — never a partial
//! file. An encrypted document is refused by [`save_incremental`] when no key
//! is supplied: use [`save_incremental_encrypted`] instead (EDIT.06).
//!
//! # Encryption (SL-5.EDIT.06)
//!
//! An incremental save cannot change a document's security handler — the
//! appended objects must decrypt under the *existing* file key to stay
//! readable together with the untouched prefix. Per ADR-P0019 we therefore
//! reuse that handler (even RC4/AESV2), encrypt every changed string/stream
//! with the correct per-object key, carry `/Encrypt` and the refreshed `/ID`
//! in the appended trailer, and verify the result by reopening it with the
//! password. [`EncryptionPolicy::KeepExisting`] is that append path.
//!
//! With explicit consent, [`EncryptionPolicy::UpgradeToAesV3`] is the one
//! deliberate exception: [`save_incremental_encrypted`] detects the upgrade
//! intent on the key and **fully rewrites** the document under a fresh AESV3
//! (revision 6) handler with new user/owner passwords, because an append can
//! never replace the old `/Encrypt` dictionary whose bytes stay authoritative
//! in the preserved prefix. The rewrite reuses the authenticated source key
//! only to decrypt the old objects, then verifies the result reopens with the
//! NEW password (and produces no bytes on any refusal). See
//! [`EncryptionPolicy`] for the consent gate.
//!
//! # Signatures and DocMDP (SL-5.EDIT.07)
//!
//! Because the source is a verbatim prefix, an existing signature's
//! `/ByteRange` keeps naming the same bytes and stays valid by construction.
//! [`crate::signature_guard`] makes that guarantee checked rather than
//! assumed: it refuses redefinitions of signature objects, enforces the
//! certification signature's DocMDP `/P` level *before* writing, and re-runs
//! the real signature report on the saved bytes. A refusal produces no output.

use std::collections::BTreeMap;

use selis_bytes::Bytes;
use selis_error::{err, Code, Result};
use selis_pdf_cos::doc_writer::write_incremental_update_full;
use selis_pdf_cos::encrypt::{self, DecryptPolicy, EncryptInfo, Handler, R6Material};
use selis_pdf_cos::{Obj, Ref};
use selis_pdf_doc::Resolver;
use selis_sandbox::{Budget, BudgetGuard};

/// Whether the newest revision declares `/Encrypt`. A key-less save refuses
/// such input rather than emitting a plaintext object; the encrypted path
/// supplies a key and reuses the existing handler.
fn is_encrypted(doc: &selis_pdf_cos::Doc) -> bool {
    doc.revisions().last().is_some_and(|r| r.encrypt.is_some())
}

/// One object-level mutation. This is the invertible core of the spec's
/// taxonomy (§2): the variants that change COS objects directly. Higher-level
/// page/content/annotation operations lower to these before reaching the
/// writer, so the incremental diff is always a set of object (re)definitions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ObjectMutation {
    /// Set (or redefine) an indirect object to a value.
    SetObject {
        /// Object number.
        num: u32,
        /// New value.
        value: Obj,
    },
    /// Free an indirect object (emitted as a free xref entry, generation bumped).
    DeleteObject {
        /// Object number.
        num: u32,
    },
}

impl ObjectMutation {
    /// The object number this mutation touches (its blast radius, I7).
    #[must_use]
    pub const fn object_number(&self) -> u32 {
        match self {
            Self::SetObject { num, .. } | Self::DeleteObject { num } => *num,
        }
    }
}

/// What a save produced, for the report and the property suite.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IncrementalSaveReport {
    /// Bytes before the save.
    pub bytes_in: u64,
    /// Bytes after the save.
    pub bytes_out: u64,
    /// Number of objects (re)defined or freed in the appended revision.
    pub objects_changed: usize,
    /// Number of revisions in the output (original plus appended updates).
    pub revisions: usize,
    /// Whether the original bytes are a verbatim prefix of the output (I3).
    pub prefix_preserved: bool,
}

/// The current, materialised object model: object number → value. This is
/// `materialise(source, journal)` for the object-level subset — the newest
/// definition of every object across all revisions.
pub type ObjectModel = BTreeMap<u32, Obj>;

/// Materialise a document's live object model by resolving every in-use object
/// in the merged xref view. Compressed objects are resolved to their values
/// (they will be PROMOTED to top-level if a later mutation changes them).
///
/// # Budget / Malformed Input
///
/// Charges the parse and each resolve. An unresolvable referenced object is a
/// typed `Unresolvable` error.
pub fn materialise(src: &[u8], budget: &Budget, g: &mut BudgetGuard<'_>) -> Result<ObjectModel> {
    let startxref_kw = selis_pdf_cos::xref::find_startxref(src, 4096).ok_or_else(|| {
        err!(
            Code::ObjUnexpected,
            during = "incremental",
            detail = "no startxref"
        )
    })?;
    let doc = selis_pdf_cos::parse_revisions(src, startxref_kw, budget, g)?;
    let last = doc.len().checked_sub(1).ok_or_else(|| {
        err!(
            Code::ObjUnexpected,
            during = "incremental",
            detail = "no revisions"
        )
    })?;
    let view = doc.at_revision(last).ok_or_else(|| {
        err!(
            Code::ObjUnexpected,
            during = "incremental",
            detail = "no view"
        )
    })?;
    let mut model = ObjectModel::new();
    for (num, entry) in view.xref {
        if matches!(entry, selis_pdf_cos::XrefEntry::InUse { .. })
            || matches!(entry, selis_pdf_cos::XrefEntry::Compressed { .. })
        {
            let r = Ref::new(num, entry.gen());
            let value = selis_pdf_cos::copy::resolve_ref(src, &doc, r, budget, g)?;
            model.insert(num, value);
        }
    }
    Ok(model)
}

/// Apply a sequence of object mutations to a model, returning the expected
/// post-mutation model. Pure: no bytes, no I/O — the property suite uses this
/// as its oracle for what the saved file must materialise to.
///
/// # Malformed Input
///
/// `SetObject` on a new number is allowed (object allocation); `DeleteObject`
/// on an absent number is a no-op, mirroring a free entry that was already free.
pub fn apply(model: &ObjectModel, mutations: &[ObjectMutation]) -> ObjectModel {
    let mut out = model.clone();
    for m in mutations {
        match m {
            ObjectMutation::SetObject { num, value } => {
                out.insert(*num, value.clone());
            }
            ObjectMutation::DeleteObject { num } => {
                out.remove(num);
            }
        }
    }
    out
}

/// Compute the net changed-object set from a mutation sequence applied on top
/// of a base model. Only the FINAL value of each touched object is emitted — an
/// object set then deleted needs no xref entry if it did not exist in the base.
fn changes(
    base: &ObjectModel,
    expected: &ObjectModel,
    mutations: &[ObjectMutation],
) -> Vec<(u32, Option<Obj>)> {
    let touched: BTreeMap<u32, ()> = mutations.iter().map(|m| (m.object_number(), ())).collect();
    let mut out = Vec::new();
    for (num, ()) in touched {
        let before = base.get(&num);
        let after = expected.get(&num);
        if before == after {
            continue; // net no-op: a change then its inverse
        }
        out.push((num, after.cloned())); // None = free/delete
    }
    out
}

/// Update the trailer `/ID` per ISO 32000-2 §14.4: preserve the first element,
/// regenerate the second from the appended revision so readers can tell
/// revisions apart. Returns a fresh `ID` pair (or `None` if the source had no
/// `/ID`).
fn updated_id(
    rev: &selis_pdf_cos::revision::Revision,
    objects: &[(u32, Option<Obj>)],
) -> Option<Obj> {
    let first = rev
        .trailer
        .iter()
        .find(|(k, _)| k.as_slice() == b"ID")
        .and_then(|(_, v)| match v {
            Obj::Array(items) => items.first().cloned(),
            _ => None,
        })?;
    use md5::Digest;
    let mut h = md5::Md5::new();
    for (num, value) in objects {
        h.update(num.to_be_bytes());
        // Values are hashed via their debug form: stable within a build and
        // sufficient to distinguish revisions, without leaking document text.
        h.update(format!("{value:?}"));
    }
    let second = Bytes::copy_from_slice(&h.finalize());
    Some(Obj::Array(vec![first, Obj::HexString(second)]))
}

/// How an encrypted source document is protected when it is saved.
///
/// An incremental save cannot change a document's security handler: the
/// untouched prefix was encrypted with its file key, so an object appended
/// under a *different* key would be unreadable in the same document. Changing
/// handler therefore requires a full rewrite, which
/// [`save_incremental_encrypted`] performs on the caller's behalf only when
/// the caller opts in with [`EncryptionPolicy::UpgradeToAesV3`] and consent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EncryptionPolicy {
    /// Reuse the source document's handler and per-object key (ADR-P0019).
    /// This is the default and keeps the original bytes as a verbatim prefix.
    KeepExisting,
    /// Ask for an explicit AESV3 (revision 6) upgrade.
    ///
    /// `user_consent` is mandatory: without it the request is refused with
    /// `PERMISSION_DENIED` before any password work. With consent, the source
    /// is authenticated with the *old* password and the save performs a full
    /// rewrite under a fresh AESV3 handler using the new passwords below. We
    /// never silently upgrade, and an append is never attempted under the new
    /// key (the old `/Encrypt` bytes would stay authoritative in the prefix).
    UpgradeToAesV3 {
        /// Whether the caller confirmed the destructive re-encryption.
        user_consent: bool,
        /// The new user (opening) password for the rewritten AESV3 document.
        new_user_password: Vec<u8>,
        /// The new owner password for the rewritten AESV3 document.
        new_owner_password: Vec<u8>,
    },
}

/// The credentials a consented AESV3 upgrade will install. Carried on the
/// authenticated key so the policy chosen at open time reaches the save, where
/// the full rewrite actually happens.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AesV3Upgrade {
    pub(crate) new_user_password: Vec<u8>,
    pub(crate) new_owner_password: Vec<u8>,
}

/// The authenticated key material for an encrypted incremental save.
///
/// Opaque on purpose: the file key is a secret, so it is not a public field.
/// The handler parameters are exposed read-only for diagnostics and tests.
#[derive(Debug, Clone)]
pub struct EncryptedSaveKey {
    pub(crate) info: EncryptInfo,
    pub(crate) file_key: Vec<u8>,
    /// `Some` when this key was opened with a consented AESV3 upgrade policy;
    /// the save then rewrites instead of appending.
    pub(crate) upgrade: Option<AesV3Upgrade>,
}

impl EncryptedSaveKey {
    /// The source handler's revision (`/R`): 2/3 = RC4, 4 = AESV2, 6 = AESV3.
    #[must_use]
    pub fn handler_revision(&self) -> u8 {
        self.info.r
    }

    /// Whether the reused handler is AES (AESV2/AESV3) rather than RC4.
    #[must_use]
    pub fn is_aes(&self) -> bool {
        self.info.aes
    }

    /// A short label for diagnostics and test evidence.
    #[must_use]
    pub fn handler_name(&self) -> &'static str {
        if self.info.handler == Handler::PubKey {
            return match self.info.v {
                4 => "Public-key AESV2 (PKCS#7 s4)",
                5 => "Public-key AESV3 (PKCS#7 s5)",
                _ => "other public-key handler",
            };
        }
        match (self.info.v, self.info.r, self.info.aes) {
            (1, 2, false) => "RC4-40 (R2)",
            (2, 3, false) => "RC4-128 (R3)",
            (4, 4, true) => "AESV2 (R4)",
            (5, 6, true) => "AESV3 (R6)",
            _ => "other standard handler",
        }
    }

    /// The decryption policy used to verify the saved bytes.
    fn decrypt_policy(&self) -> DecryptPolicy {
        DecryptPolicy::from_encrypt(&self.info, self.file_key.clone())
    }
}

/// Wrap an already-authenticated file key for callers (for example an open
/// engine session) that do not want to re-derive it from the password.
///
/// # Budget / Malformed Input
///
/// No budget: both arguments are already-resident values. Returns `None` for
/// a public-key `/Encrypt` dictionary or an empty key — neither can be reused
/// by the standard-handler append path.
#[must_use]
pub fn encrypted_save_key_from_parts(
    info: EncryptInfo,
    file_key: Vec<u8>,
) -> Option<EncryptedSaveKey> {
    if info.handler != Handler::Standard || file_key.is_empty() {
        return None;
    }
    Some(EncryptedSaveKey {
        info,
        file_key,
        upgrade: None,
    })
}

/// Authenticate `src` with `password` and prepare the key material for an
/// encrypted incremental save.
///
/// Returns `Ok(None)` when the document is not encrypted. [`EncryptionPolicy`]
/// is applied *before* key derivation, so a disallowed upgrade fails even if
/// the password is correct. A consented upgrade authenticates with the *old*
/// password and returns a key that carries the new credentials; the full
/// rewrite happens in [`save_incremental_encrypted`].
///
/// # Budget / Malformed Input
///
/// Charges the parse of the `/Encrypt` dictionary. A wrong password is
/// `WRONG_PASSWORD`; an unreadable dictionary is `ENCRYPT_MALFORMED` and an
/// unsupported handler is `ENCRYPT_UNSUPPORTED`. Never returns a key that did
/// not authenticate.
pub fn open_encrypted(
    src: &[u8],
    password: &[u8],
    policy: EncryptionPolicy,
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
) -> Result<Option<EncryptedSaveKey>> {
    let startxref_kw = selis_pdf_cos::xref::find_startxref(src, 4096).ok_or_else(|| {
        err!(
            Code::ObjUnexpected,
            during = "incremental-encrypted",
            detail = "no startxref"
        )
    })?;
    let doc = selis_pdf_cos::parse_revisions(src, startxref_kw, budget, g)?;
    let last = doc.len().checked_sub(1).ok_or_else(|| {
        err!(
            Code::ObjUnexpected,
            during = "incremental-encrypted",
            detail = "no revisions"
        )
    })?;
    let revision = doc.revisions().get(last).ok_or_else(|| {
        err!(
            Code::ObjUnexpected,
            during = "incremental-encrypted",
            detail = "no revision"
        )
    })?;

    // Capture the requested upgrade before the policy is consumed by the
    // consent gate below. The source handler is always authenticated with the
    // OLD password; the new credentials are only installed at save time.
    let upgrade = match &policy {
        EncryptionPolicy::KeepExisting => None,
        EncryptionPolicy::UpgradeToAesV3 {
            new_user_password,
            new_owner_password,
            ..
        } => Some(AesV3Upgrade {
            new_user_password: new_user_password.clone(),
            new_owner_password: new_owner_password.clone(),
        }),
    };

    let Some(encrypt_ref) = revision.encrypt else {
        if upgrade.is_some() {
            return Err(err!(
                Code::EncryptUnsupported,
                during = "incremental-encrypted",
                detail = "adding AESV3 protection to an unencrypted document requires a full rewrite; use save_rewritten with a fresh AESV3 handler"
            ));
        }
        return Ok(None);
    };

    // Consent gate before any password work: knowing the password is not the
    // same as consenting to a handler change (ADR-P0019). With consent the
    // intent is carried on the returned key; the rewrite itself is deferred to
    // save_incremental_encrypted.
    match policy {
        EncryptionPolicy::KeepExisting => {}
        EncryptionPolicy::UpgradeToAesV3 {
            user_consent: false,
            ..
        } => {
            return Err(err!(
                Code::PermissionDenied,
                during = "incremental-encrypted",
                detail = "AESV3 upgrade requires explicit user consent; the existing handler is reused by default"
            ));
        }
        EncryptionPolicy::UpgradeToAesV3 {
            user_consent: true, ..
        } => {}
    }

    let info = encrypt::parse_encrypt(src, Some(encrypt_ref), budget, g)?.ok_or_else(|| {
        err!(
            Code::EncryptMalformed,
            during = "incremental-encrypted",
            detail = "unreadable /Encrypt dictionary"
        )
    })?;
    if info.handler != Handler::Standard || !info.supports() {
        return Err(err!(
            Code::EncryptUnsupported,
            during = "incremental-encrypted",
            detail =
                "only a supported standard security handler can be reused by an incremental save"
        ));
    }
    let id0 = encrypt::document_id(&revision.trailer);
    let file_key = encrypt::authenticate(&info, &id0, password).ok_or_else(|| {
        err!(
            Code::WrongPassword,
            during = "incremental-encrypted",
            detail = "password did not authenticate against the existing handler"
        )
    })?;
    Ok(Some(EncryptedSaveKey {
        info,
        file_key,
        upgrade,
    }))
}

/// Authenticate a public-key-encrypted document with the recipient's private
/// key and prepare the material for an encrypted incremental save.
///
/// The COS layer's public-key reader (SL-1.ENC.03) parses the `/Adobe.PPKLite`
/// / `/Adobe.PubSec` `/Encrypt` dictionary and unwraps one `/Recipients` CMS
/// blob with the credential. The returned key reuses that handler and its file
/// key: a changed object is encrypted with the same per-object algorithm
/// (AESV2 for `/V 4`, direct-key AESV3 for `/V 5`), so every recipient of the
/// unchanged `/Recipients` array can still read the saved file. No new CMS
/// envelopes are minted — the PKCS#7 layer is read-only — so this is an edit
/// of an existing public-key document, never creation of a new one.
///
/// The recipient's [`selis_crypto::pkcs7::PubKeyAuth`] is returned alongside
/// the key so the policy layer can enforce the CMS permission bits
/// (SL-1.ENC.09); this low-level save does not override them.
///
/// # Budget / Malformed Input
///
/// Charges the parse of `/Encrypt` and the CMS blob parses (see
/// [`encrypt::authenticate_public_key`]). Returns `Ok(None)` for a clear
/// document. A standard-handler `/Encrypt` is `ENCRYPT_UNSUPPORTED` here (use
/// [`open_encrypted`] with a password); a wrong credential is
/// `RECIPIENT_NO_MATCH`, and structural CMS damage is `ENCRYPT_MALFORMED`.
pub fn open_encrypted_public_key(
    src: &[u8],
    credential: &selis_crypto::pkcs7::PubKeyCredential,
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
) -> Result<Option<(EncryptedSaveKey, selis_crypto::pkcs7::PubKeyAuth)>> {
    let startxref = selis_pdf_cos::xref::find_startxref(src, 4096).ok_or_else(|| {
        err!(
            Code::ObjUnexpected,
            during = "incremental-encrypted",
            detail = "public-key open: no startxref"
        )
    })?;
    let doc = selis_pdf_cos::parse_revisions(src, startxref, budget, g)?;
    let last = doc.len().checked_sub(1).ok_or_else(|| {
        err!(
            Code::ObjUnexpected,
            during = "incremental-encrypted",
            detail = "public-key open: no revisions"
        )
    })?;
    let revision = doc.revisions().get(last).ok_or_else(|| {
        err!(
            Code::ObjUnexpected,
            during = "incremental-encrypted",
            detail = "public-key open: no revision"
        )
    })?;
    let Some(encrypt_ref) = revision.encrypt else {
        return Ok(None);
    };
    let info = encrypt::parse_encrypt(src, Some(encrypt_ref), budget, g)?.ok_or_else(|| {
        err!(
            Code::EncryptMalformed,
            during = "incremental-encrypted",
            detail = "public-key open: unreadable /Encrypt dictionary"
        )
    })?;
    if info.handler != Handler::PubKey {
        return Err(err!(
            Code::EncryptUnsupported,
            during = "incremental-encrypted",
            detail = "open_encrypted_public_key needs a /Adobe.PPKLite or /Adobe.PubSec /Encrypt dictionary; this document uses the standard handler (use open_encrypted with a password)"
        ));
    }
    let auth = encrypt::authenticate_public_key(&info, credential, g)?;
    let key = EncryptedSaveKey {
        info,
        file_key: auth.key.clone(),
        upgrade: None,
    };
    Ok(Some((key, auth)))
}

/// Save into an encrypted document.
///
/// With a key from [`EncryptionPolicy::KeepExisting`] every changed
/// string/stream is encrypted with the source file key and the appended
/// trailer carries `/Encrypt` plus a refreshed `/ID`, so the output is one
/// coherent document under the *same* key as the original bytes (the I3
/// prefix stays intact).
///
/// With a key from a consented [`EncryptionPolicy::UpgradeToAesV3`], an
/// incremental append cannot replace the old `/Encrypt` dictionary — the old
/// bytes stay authoritative in the preserved prefix — so this instead
/// **fully rewrites** the document under a fresh AESV3/R6 handler with the new
/// passwords. The reported `prefix_preserved` is `false` and `revisions` is
/// `1` for that path; no bytes are produced unless the new file reopens with
/// the new password and materialises to the expected model.
///
/// # Budget / Malformed Input
///
/// As [`save_incremental`], plus: the key must authenticate (see
/// [`open_encrypted`]); a wrong key fails verification with `VERIFY_FAILED`
/// rather than producing a document with plaintext objects. On the upgrade
/// path an unavailable OS CSPRNG is `ENCRYPT_UNSUPPORTED`, and any mismatch in
/// the rewritten model is `VERIFY_FAILED`; in both cases the source is intact.
pub fn save_incremental_encrypted(
    src: &[u8],
    model: &ObjectModel,
    mutations: &[ObjectMutation],
    key: &EncryptedSaveKey,
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
) -> Result<(Vec<u8>, IncrementalSaveReport)> {
    if key.upgrade.is_some() {
        return rewrite_to_aesv3(src, model, mutations, key, budget, g);
    }
    save_incremental_inner(src, model, mutations, Some(key), budget, g)
}

/// Perform a consented AESV3 upgrade as a full, verified rewrite.
///
/// The source was authenticated with its OLD password; `key.upgrade` carries
/// the NEW credentials. Every source object is already plaintext in `model`,
/// so it is written afresh under a brand-new file key with the new R6 handler.
/// The old `/Encrypt` dictionary is dropped (it cannot appear in the new
/// single-revision file) and a fresh one is allocated by the encrypted writer.
///
/// # Budget / Malformed Input
///
/// As [`save_incremental_encrypted`]. The rewrite is verification-gated: the
/// output must be a single revision that reopens with the new password and
/// materialises to the expected model, otherwise `VERIFY_FAILED` is returned
/// and no bytes reach the caller. A missing OS entropy source is
/// `ENCRYPT_UNSUPPORTED` — the old key is never reused as a stand-in.
fn rewrite_to_aesv3(
    src: &[u8],
    model: &ObjectModel,
    mutations: &[ObjectMutation],
    key: &EncryptedSaveKey,
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
) -> Result<(Vec<u8>, IncrementalSaveReport)> {
    let upgrade = key.upgrade.as_ref().ok_or_else(|| {
        err!(
            Code::InternalInvariant,
            during = "incremental-encrypted",
            detail = "AESV3 rewrite called without upgrade credentials"
        )
    })?;

    // A full rewrite must not be a way around a signature/MDP refusal.
    crate::signature_guard::check_mutation_allowed(model, mutations, budget, g)?;
    let expected = apply(model, mutations);
    let changed = changes(model, &expected, mutations);

    // Locate the roots and the OLD /Encrypt object in the source.
    let start = selis_pdf_cos::xref::find_startxref(src, 4096).ok_or_else(|| {
        err!(
            Code::ObjUnexpected,
            during = "incremental-encrypted",
            detail = "AESV3 upgrade: no startxref"
        )
    })?;
    let doc = selis_pdf_cos::parse_revisions(src, start, budget, g)?;
    let last = doc.len().checked_sub(1).ok_or_else(|| {
        err!(
            Code::ObjUnexpected,
            during = "incremental-encrypted",
            detail = "AESV3 upgrade: no revisions"
        )
    })?;
    let revision = doc.revisions().get(last).ok_or_else(|| {
        err!(
            Code::ObjUnexpected,
            during = "incremental-encrypted",
            detail = "AESV3 upgrade: no revision"
        )
    })?;
    let root = revision.root.ok_or_else(|| {
        err!(
            Code::ObjUnexpected,
            during = "incremental-encrypted",
            detail = "AESV3 upgrade: no /Root"
        )
    })?;
    let info_ref = revision
        .trailer
        .iter()
        .find(|(k, _)| k.as_slice() == b"Info")
        .and_then(|(_, v)| match v {
            Obj::Ref(r) => Some(*r),
            _ => None,
        });
    let old_encrypt_num = revision.encrypt.map(|r| r.num);

    // `expected` is the decrypted source with the caller's mutations applied.
    // Drop the old /Encrypt dictionary: the encrypted writer allocates a fresh
    // one (it is never re-encrypted, and must not survive the rewrite).
    let objects: Vec<(u32, Obj)> = expected
        .iter()
        .filter(|(num, _)| Some(**num) != old_encrypt_num)
        .map(|(num, obj)| (*num, obj.clone()))
        .collect();

    // A fresh file key and four salts — never the old key, never a fixed seed.
    let material = R6Material::generate().ok_or_else(|| {
        err!(
            Code::EncryptUnsupported,
            during = "incremental-encrypted",
            detail = "no OS entropy source for a fresh AESV3 key; refusing to reuse the old key"
        )
    })?;
    let (new_info, new_file_key) = EncryptInfo::new_r6(
        &upgrade.new_user_password,
        &upgrade.new_owner_password,
        key.info.p,
        &material,
    );

    let mut extra_trailer: Vec<(Vec<u8>, Obj)> = Vec::new();
    if let Some(info) = info_ref {
        extra_trailer.push((b"Info".to_vec(), Obj::Ref(info)));
    }
    let out = selis_pdf_cos::doc_writer::write_objects_as_document_encrypted_with_trailer(
        &objects,
        root,
        &extra_trailer,
        &new_info,
        &new_file_key,
        budget,
        g,
    )?;

    // Verification: single revision, AESV3 handler, opens with the NEW
    // password, and materialises to exactly the expected plaintext model.
    let out_start = selis_pdf_cos::xref::find_startxref(&out, 4096).ok_or_else(|| {
        err!(
            Code::VerifyFailed,
            during = "incremental-encrypted",
            detail = "AESV3 upgrade: rewritten document has no startxref"
        )
    })?;
    let out_doc = selis_pdf_cos::parse_revisions(&out, out_start, budget, g)?;
    if out_doc.len() != 1 {
        return Err(err!(
            Code::VerifyFailed,
            during = "incremental-encrypted",
            detail = format!(
                "AESV3 upgrade must yield one revision, got {}",
                out_doc.len()
            )
        ));
    }
    let out_revision = out_doc.revisions().last().ok_or_else(|| {
        err!(
            Code::VerifyFailed,
            during = "incremental-encrypted",
            detail = "AESV3 upgrade: no rewritten revision"
        )
    })?;
    let encrypt_ref = out_revision.encrypt.ok_or_else(|| {
        err!(
            Code::VerifyFailed,
            during = "incremental-encrypted",
            detail = "AESV3 upgrade: rewritten document has no /Encrypt"
        )
    })?;
    let parsed_info =
        encrypt::parse_encrypt(&out, Some(encrypt_ref), budget, g)?.ok_or_else(|| {
            err!(
                Code::VerifyFailed,
                during = "incremental-encrypted",
                detail = "AESV3 upgrade: unreadable rewritten /Encrypt dictionary"
            )
        })?;
    if parsed_info.r != 6 || parsed_info.v != 5 || !parsed_info.aes {
        return Err(err!(
            Code::VerifyFailed,
            during = "incremental-encrypted",
            detail = format!(
                "AESV3 upgrade installed r={} v={} aes={}, expected r=6 v=5 aes=true",
                parsed_info.r, parsed_info.v, parsed_info.aes
            )
        ));
    }
    let id0 = encrypt::document_id(&out_revision.trailer);
    let new_key_material = encrypt::authenticate(&parsed_info, &id0, &upgrade.new_user_password)
        .ok_or_else(|| {
            err!(
                Code::VerifyFailed,
                during = "incremental-encrypted",
                detail =
                    "AESV3 upgrade: rewritten file did not authenticate with the new user password"
            )
        })?;
    let reopened_key =
        encrypted_save_key_from_parts(parsed_info, new_key_material).ok_or_else(|| {
            err!(
                Code::VerifyFailed,
                during = "incremental-encrypted",
                detail = "AESV3 upgrade: rewritten handler was not a standard handler"
            )
        })?;

    let mut expected_cmp = expected.clone();
    if let Some(num) = old_encrypt_num {
        expected_cmp.remove(&num);
    }
    let mut reopened = materialise_encrypted(&out, &reopened_key, budget, g)?;
    reopened.remove(&encrypt_ref.num);
    // The writer rewrites stream /Length to the new ciphertext length; the
    // read path leaves it alone. Normalise both sides to the plaintext length
    // before comparing, exactly as the append path does.
    let reopened = normalise_model_lengths(&reopened);
    let expected_cmp = normalise_model_lengths(&expected_cmp);
    if reopened != expected_cmp {
        let missing: Vec<u32> = expected_cmp
            .keys()
            .copied()
            .filter(|n| !reopened.contains_key(n))
            .collect();
        let extra: Vec<u32> = reopened
            .keys()
            .copied()
            .filter(|n| !expected_cmp.contains_key(n))
            .collect();
        let differing: Vec<u32> = expected_cmp
            .iter()
            .filter_map(|(n, v)| reopened.get(n).filter(|other| *other != v).map(|_| *n))
            .collect();
        return Err(err!(
            Code::VerifyFailed,
            during = "incremental-encrypted",
            detail = format!(
                "AESV3 upgrade: rewritten model differs (missing {missing:?}, extra {extra:?}, differing {differing:?})"
            )
        ));
    }

    let bytes_in = u64::try_from(src.len()).unwrap_or(u64::MAX);
    let bytes_out = u64::try_from(out.len()).unwrap_or(u64::MAX);
    Ok((
        out,
        IncrementalSaveReport {
            bytes_in,
            bytes_out,
            objects_changed: changed.len(),
            revisions: 1,
            prefix_preserved: false,
        },
    ))
}

/// Re-materialise a document with its decryption policy applied, so every
/// resolved string/stream is plaintext. This is the encrypted twin of
/// [`materialise`] and the read-back oracle for the encrypted writer.
///
/// # Budget / Malformed Input
///
/// Charges the parse and each resolve, like [`materialise`]. An object that
/// cannot be resolved or decrypted is a typed error, never partial output.
fn materialise_encrypted(
    src: &[u8],
    key: &EncryptedSaveKey,
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
) -> Result<ObjectModel> {
    let startxref_kw = selis_pdf_cos::xref::find_startxref(src, 4096).ok_or_else(|| {
        err!(
            Code::ObjUnexpected,
            during = "incremental-encrypted",
            detail = "no startxref"
        )
    })?;
    let doc = selis_pdf_cos::parse_revisions(src, startxref_kw, budget, g)?;
    let last = doc.len().checked_sub(1).ok_or_else(|| {
        err!(
            Code::ObjUnexpected,
            during = "incremental-encrypted",
            detail = "no revisions"
        )
    })?;
    let view = doc.at_revision(last).ok_or_else(|| {
        err!(
            Code::ObjUnexpected,
            during = "incremental-encrypted",
            detail = "no view"
        )
    })?;
    let mut resolver = Resolver::new(&doc, src, budget);
    resolver.set_key(key.decrypt_policy());
    let mut model = ObjectModel::new();
    for (num, entry) in view.xref {
        if matches!(entry, selis_pdf_cos::XrefEntry::InUse { .. })
            || matches!(entry, selis_pdf_cos::XrefEntry::Compressed { .. })
        {
            let r = Ref::new(num, entry.gen());
            let value = resolver.resolve(r, g)?;
            model.insert(num, value);
        }
    }
    Ok(model)
}

/// [`encrypt_object`](encrypt::encrypt_object) rewrites a stream's `/Length`
/// to the ciphertext length; the read path deliberately leaves it alone. Make
/// both sides of the verification comparison agree on that one field.
fn normalise_stream_lengths(obj: &Obj) -> Obj {
    match obj {
        Obj::Stream { dict, data } => {
            let mut d = dict.clone();
            let len = Obj::Int(i64::try_from(data.len()).unwrap_or(i64::MAX));
            if let Some((_, v)) = d.iter_mut().find(|(k, _)| k.as_slice() == b"Length") {
                *v = len;
            } else {
                d.push((Bytes::copy_from_slice(b"Length"), len));
            }
            Obj::Stream {
                dict: d,
                data: data.clone(),
            }
        }
        Obj::Array(items) => Obj::Array(items.iter().map(normalise_stream_lengths).collect()),
        Obj::Dict(pairs) => Obj::Dict(
            pairs
                .iter()
                .map(|(k, v)| (k.clone(), normalise_stream_lengths(v)))
                .collect(),
        ),
        other => other.clone(),
    }
}

fn normalise_model_lengths(model: &ObjectModel) -> ObjectModel {
    model
        .iter()
        .map(|(num, obj)| (*num, normalise_stream_lengths(obj)))
        .collect()
}

/// Save an object mutation sequence as an incremental update. The original
/// bytes are preserved verbatim; only changed objects, a new xref section and
/// a trailer are appended. The result is re-parsed and its materialised model
/// compared against [`apply`]'s expected state before it is returned.
///
/// Encrypted input is refused here: use [`save_incremental_encrypted`] with a
/// key derived by [`open_encrypted`].
///
/// # Budget / Malformed Input
///
/// See the module docs. A refusal produces no output (the caller still owns
/// `src`).
pub fn save_incremental(
    src: &[u8],
    model: &ObjectModel,
    mutations: &[ObjectMutation],
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
) -> Result<(Vec<u8>, IncrementalSaveReport)> {
    save_incremental_inner(src, model, mutations, None, budget, g)
}

/// The shared implementation behind both save entry points. `key` is `None`
/// for a clear document and `Some` when the caller has authenticated an
/// encrypted one; in the latter case every emitted object is re-encrypted
/// with the source file key before it reaches the writer.
fn save_incremental_inner(
    src: &[u8],
    model: &ObjectModel,
    mutations: &[ObjectMutation],
    key: Option<&EncryptedSaveKey>,
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
) -> Result<(Vec<u8>, IncrementalSaveReport)> {
    if src.is_empty() {
        return Err(err!(
            Code::ObjUnexpected,
            during = "incremental",
            detail = "empty source"
        ));
    }

    // SL-5.EDIT.07: refuse signature-breaking and DocMDP-forbidden mutations
    // BEFORE writing anything. On refusal no bytes are produced (the caller
    // still owns `src`), and the error names the level and refused operation.
    crate::signature_guard::check_mutation_allowed(model, mutations, budget, g)?;

    let expected = apply(model, mutations);
    let changed = changes(model, &expected, mutations);
    if changed.is_empty() {
        // Nothing to do: return the source unchanged, still a valid save.
        let len = u64::try_from(src.len()).unwrap_or(u64::MAX);
        return Ok((
            src.to_vec(),
            IncrementalSaveReport {
                bytes_in: len,
                bytes_out: len,
                objects_changed: 0,
                revisions: 1,
                prefix_preserved: true,
            },
        ));
    }

    let startxref_kw = selis_pdf_cos::xref::find_startxref(src, 4096).ok_or_else(|| {
        err!(
            Code::ObjUnexpected,
            during = "incremental",
            detail = "no startxref"
        )
    })?;
    let doc = selis_pdf_cos::parse_revisions(src, startxref_kw, budget, g)?;
    if is_encrypted(&doc) && key.is_none() {
        return Err(err!(
            Code::MutationInvalid,
            during = "incremental",
            detail = "encrypted; supply a key via open_encrypted + save_incremental_encrypted"
        ));
    }
    if !is_encrypted(&doc) && key.is_some() {
        return Err(err!(
            Code::MutationInvalid,
            during = "incremental",
            detail = "an encryption key was supplied for an unencrypted document"
        ));
    }
    let last = doc.len().checked_sub(1).unwrap_or(0);
    let revision = doc.revisions().get(last).ok_or_else(|| {
        err!(
            Code::ObjUnexpected,
            during = "incremental",
            detail = "no revision"
        )
    })?;

    // Carry /Root, /Info and /Encrypt forward; update /ID. /Size and /Prev
    // are filled by the low-level writer. The /Encrypt dictionary itself is
    // NEVER re-encrypted — it is the key to the whole file.
    let mut trailer: Vec<(Vec<u8>, Obj)> = Vec::new();
    if let Some(root) = revision.root {
        trailer.push((b"Root".to_vec(), Obj::Ref(root)));
    }
    if let Some((_, info)) = revision
        .trailer
        .iter()
        .find(|(k, _)| k.as_slice() == b"Info")
    {
        trailer.push((b"Info".to_vec(), info.clone()));
    }
    if let Some(enc_ref) = revision.encrypt {
        trailer.push((b"Encrypt".to_vec(), Obj::Ref(enc_ref)));
    }

    // Deletions become free xref entries with a generation bump; the full
    // low-level writer handles both redefinitions and frees. On the encrypted
    // path, encrypt each redefined object with the SOURCE file key and the
    // object's own number (generation 0 is what the writer emits). ADR-P0019:
    // no handler change, no plaintext object in an encrypted document.
    let objects: Vec<(u32, Option<Obj>)> = changed
        .iter()
        .map(|(num, value)| match (value, key) {
            (Some(value), Some(k)) => (
                *num,
                Some(encrypt::encrypt_object(
                    value,
                    &k.file_key,
                    *num,
                    0,
                    k.info.r,
                    k.info.aes,
                )),
            ),
            (value, _) => (*num, value.clone()),
        })
        .collect();

    // Update /ID per ISO 32000-2 §14.4: preserve the first element, regenerate
    // the second from the changed objects (deterministic, no circularity with
    // the file bytes). Carry it in the trailer so the low-level writer emits it.
    if let Some(id) = updated_id(revision, &changed) {
        trailer.push((b"ID".to_vec(), id));
    }

    let out = write_incremental_update_full(src, &objects, &trailer, budget, g)?;

    let bytes_in = u64::try_from(src.len()).unwrap_or(u64::MAX);
    let bytes_out = u64::try_from(out.len()).unwrap_or(u64::MAX);

    // Read-back verification (EDIT.03 DoD b/c/d, EDIT.06 DoD): output parses,
    // prefix intact, and every object decrypts back to the expected plaintext
    // model. On the encrypted path the resolver applies the decryption policy
    // so this also proves no object was written in the clear.
    let prefix_preserved = out.get(..src.len()) == Some(src);
    let (reopened, expected_cmp) = match key {
        Some(k) => (
            normalise_model_lengths(&materialise_encrypted(&out, k, budget, g)?),
            normalise_model_lengths(&expected),
        ),
        None => (materialise(&out, budget, g)?, expected),
    };
    if reopened != expected_cmp {
        return Err(err!(
            Code::VerifyFailed,
            during = "incremental",
            detail = "reopened model does not match the expected post-mutation state"
        ));
    }
    let new_doc = selis_pdf_cos::parse_revisions(
        &out,
        selis_pdf_cos::xref::find_startxref(&out, 4096).unwrap_or(0),
        budget,
        g,
    )?;

    // SL-5.EDIT.07: re-run the signature report on the saved bytes. Existing
    // signatures must still verify over the unchanged, prefix-resident byte
    // ranges. The encrypted path is skipped here: `signatures()` refuses
    // ciphertext (there is no decryption policy in the report), and those
    // bytes were already verified plaintext-equivalent above.
    if key.is_none() {
        let _preservation =
            crate::signature_guard::verify_signatures_preserved(src, &out, budget, g)?;
    }

    Ok((
        out,
        IncrementalSaveReport {
            bytes_in,
            bytes_out,
            objects_changed: changed.len(),
            revisions: new_doc.len(),
            prefix_preserved,
        },
    ))
}
