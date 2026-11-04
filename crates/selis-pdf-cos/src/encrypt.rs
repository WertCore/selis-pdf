//! PDF standard security handler — `/Encrypt` parsing and key setup
//! (SL-1.ENC.01/02).
//!
//! Parses the `/Encrypt` dictionary, authenticates a password (user/owner
//! passwords, revisions 2–6: RC4, AES-128, and AES-256), and exposes the
//! encryption key the resolver uses to decrypt streams and strings.

use selis_error::{err, Code, Result};
use selis_sandbox::{Budget, BudgetGuard};

use crate::obj::{Obj, Ref};
use crate::resolve::resolve_object_numbered;

/// Which security handler a parsed `/Encrypt` dictionary belongs to
/// (ISO 32000-2 §7.6.3–§7.6.6).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Handler {
    /// The standard (password) security handler.
    Standard,
    /// The public-key (PKCS#7/CMS) handler, read side (SL-1.ENC.03).
    PubKey,
}

/// The parsed `/Encrypt` dictionary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EncryptInfo {
    /// Which security handler this dictionary describes.
    pub handler: Handler,
    /// The handler revision (`/R`). For the public-key handler the
    /// per-object cipher revision equals `/V` (4 = AESV2-salted, 5 =
    /// direct-key AESV3); the standard handler's `/R` algorithms do not
    /// apply (no `/O`/`/U`), and [`authenticate`] refuses public-key infos.
    pub r: u8,
    /// The algorithm (`/V`): 1 = RC4-40, 2 = RC4-128, 4 = AES-128, 5 = AES-256.
    pub v: u8,
    /// The key length in bits (`/Length`; 40 for `/V 1`).
    pub length: usize,
    /// `/O`.
    pub o: Vec<u8>,
    /// `/U`.
    pub u: Vec<u8>,
    /// `/P` (permission flags).
    pub p: u32,
    /// `/StmF` — the name of the stream crypt filter (ISO 32000-1 §7.4.10).
    pub stmf: String,
    /// `/StrF` — the name of the string crypt filter.
    pub strf: String,
    /// Whether the standard crypt filter (`/StdCF`) uses AES (`/AESV2`/`/AESV3`).
    pub aes: bool,
    /// `/EncryptMetadata` (default true).
    pub encrypt_metadata: bool,
    /// `/UE` — the wrapped file key for revisions 5–6.
    pub ue: Vec<u8>,
    /// `/OE` — the owner-wrapped file key for revisions 5–6.
    pub oe: Vec<u8>,
    /// `/Perms` — the AES-256-CBC encrypted permission-flags blob (rev 6).
    pub perms: Vec<u8>,
    /// `/CF` — the crypt filter definitions, resolved when indirect. Used to
    /// select a per-stream `/Crypt` filter's algorithm (SL-1.FILT.09).
    pub cf: Vec<(selis_bytes::Bytes, Obj)>,
    /// `/SubFilter` of the public-key handler (`adbe.pkcs7.s4`/`s5`).
    /// `None` for the standard handler.
    pub subfilter: Option<String>,
    /// `/Recipients` — the raw PKCS#7/CMS blobs (public-key handler only),
    /// from the `/Encrypt` dictionary or its default crypt filter.
    pub recipients: Vec<Vec<u8>>,
}

/// The 64 bytes a revision-6 writer must not guess: the 256-bit file key and
/// the four 8-byte salts that `/U`, `/O`, `/UE` and `/OE` are built around.
///
/// Two ways to fill it, and only two:
///
/// * [`R6Material::generate`] draws from the OS CSPRNG and returns `None`
///   where there is no OS entropy source. A native caller that gets `None`
///   refuses to encrypt — there is no substitute for a key, and this module's
///   history includes a fixed-seed stand-in that would have quietly given every
///   protected document the same one.
/// * [`R6Material::from_bytes`] takes bytes a host with entropy produced for
///   it. The web page supplies them from `crypto.getRandomValues`, uses them
///   for one operation, and stores nothing: they are the only secret the page
///   hands the guest that the guest cannot recompute.
///
/// Stream and string IVs are not here — `selis_crypto::derive_iv` makes them
/// from the file key and each object's number — which is why one 64-byte blob
/// covers a document of any size.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct R6Material {
    /// The file encryption key every stream and string is encrypted under.
    pub file_key: [u8; 32],
    /// The user-password validation salt.
    pub u_v_salt: [u8; 8],
    /// The user-password key salt.
    pub u_k_salt: [u8; 8],
    /// The owner-password validation salt.
    pub o_v_salt: [u8; 8],
    /// The owner-password key salt.
    pub o_k_salt: [u8; 8],
}

impl R6Material {
    /// The exact byte length [`R6Material::from_bytes`] takes.
    pub const LEN: usize = 64;

    /// Draw the whole blob from the OS CSPRNG.
    pub fn generate() -> Option<Self> {
        let bytes = selis_crypto::random_bytes(Self::LEN)?;
        Self::from_bytes(&bytes)
    }

    /// Read a host-supplied blob. `None` unless it is exactly [`Self::LEN`]
    /// bytes — a short or over-long injection is a caller bug, and guessing
    /// which half to drop would be a weaker key without saying so.
    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        if bytes.len() != Self::LEN {
            return None;
        }
        let file_key: [u8; 32] = bytes.get(..32)?.try_into().ok()?;
        let u_v_salt: [u8; 8] = bytes.get(32..40)?.try_into().ok()?;
        let u_k_salt: [u8; 8] = bytes.get(40..48)?.try_into().ok()?;
        let o_v_salt: [u8; 8] = bytes.get(48..56)?.try_into().ok()?;
        let o_k_salt: [u8; 8] = bytes.get(56..)?.try_into().ok()?;
        Some(Self {
            file_key,
            u_v_salt,
            u_k_salt,
            o_v_salt,
            o_k_salt,
        })
    }

    /// The document's `/ID`, derived from the file key.
    #[must_use]
    pub fn document_id(&self) -> [u8; 16] {
        selis_crypto::document_id_from_key(&self.file_key)
    }
}

impl EncryptInfo {
    /// Whether the stream crypt filter is a real handler filter (streams are
    /// encrypted). `/Identity` streams are not. Beyond the standard
    /// handler's `/StdCF`, a named filter (the public-key handler's
    /// `/DefaultCryptFilter`) is encrypted unless its `/CFM` is `/None`.
    #[must_use]
    pub fn stream_encrypted(&self) -> bool {
        self.filter_encrypted(&self.stmf)
    }

    /// Whether the string crypt filter is a real handler filter (strings are
    /// encrypted). `/Identity` strings are not.
    #[must_use]
    pub fn string_encrypted(&self) -> bool {
        self.filter_encrypted(&self.strf)
    }

    /// Whether the named crypt filter encrypts its targets: `/Identity`
    /// never, a filter missing from `/CF` defaults to the standard handler
    /// (encrypted), a present filter is encrypted unless its `/CFM` is
    /// `/None` (ISO 32000-1 §7.4.10 / §7.6.6.2).
    fn filter_encrypted(&self, name: &str) -> bool {
        if name == "Identity" {
            return false;
        }
        match self.cfm_for(name) {
            Some(cfm) => cfm != "None",
            // No /CF entry: the standard handler's /StdCF shortcut, and the
            // sub-filter-era default (everything encrypted).
            None => true,
        }
    }

    /// The `/CFM` of a named crypt filter, when `/CF` defines it.
    fn cfm_for(&self, name: &str) -> Option<String> {
        self.cf
            .iter()
            .find(|(k, _)| k.as_slice() == name.as_bytes())
            .and_then(|(_, v)| match v {
                Obj::Dict(p) => p.iter().find(|(k, _)| k.as_slice() == b"CFM"),
                _ => None,
            })
            .and_then(|(_, v)| match v {
                Obj::Name(n) => Some(String::from_utf8_lossy(n.as_slice()).to_string()),
                _ => None,
            })
    }

    /// The stream crypt filter (`/StmF`) — the standard handler's `/StdCF`
    /// (the public-key handler names its default filter
    /// `/DefaultCryptFilter`).
    pub fn supports(&self) -> bool {
        self.v <= 5 && self.r >= 2 && self.r <= 6
    }

    /// Build a revision-6 `/Encrypt` dictionary from caller-supplied
    /// credential material.
    ///
    /// Returns the [`EncryptInfo`] (with `/O`, `/U`, `/UE`, `/OE`, `/Perms` all
    /// computed) and the 32-byte file key. Revision 6 does not hash the
    /// document ID into the key (ISO 32000-2 §7.6.4.3.4), so `id0` is not an
    /// input here — the caller derives it from the material and writes it into
    /// the trailer.
    #[must_use]
    pub fn new_r6(
        user_password: &[u8],
        owner_password: &[u8],
        p: u32,
        material: &R6Material,
    ) -> (Self, Vec<u8>) {
        let file_key = material.file_key.to_vec();
        let u =
            selis_crypto::compute_u_r6(user_password, &material.u_v_salt, &material.u_k_salt, 6);
        let o = selis_crypto::compute_o_r6(
            owner_password,
            &material.o_v_salt,
            &material.o_k_salt,
            &u,
            6,
        );
        let ue = selis_crypto::compute_ue_r6(user_password, &material.u_k_salt, &file_key, 6);
        let oe = selis_crypto::compute_oe_r6(owner_password, &material.o_k_salt, &u, &file_key, 6);
        let perms = selis_crypto::compute_perms_r6(p, &file_key, true);
        let cf_std_cf = Obj::Dict(vec![
            (bytes(b"CFM"), Obj::Name(bytes(b"AESV3"))),
            (bytes(b"Length"), Obj::Int(32)),
            (bytes(b"AuthEvent"), Obj::Name(bytes(b"DocOpen"))),
        ]);
        let info = Self {
            handler: Handler::Standard,
            r: 6,
            v: 5,
            length: 256,
            o,
            u,
            p,
            stmf: "StdCF".to_string(),
            strf: "StdCF".to_string(),
            aes: true,
            encrypt_metadata: true,
            ue,
            oe,
            perms,
            cf: vec![(bytes(b"StdCF"), cf_std_cf)],
            subfilter: None,
            recipients: Vec::new(),
        };
        (info, file_key)
    }

    /// Verify the stored `/Perms` blob against this info's permission flags,
    /// the file key, and the `/EncryptMetadata` flag (revision 6, read side).
    /// Returns `false` when `/Perms` is absent, truncated, or does not carry
    /// the expected Algorithm-10 block — a damaged or re-keyed document.
    ///
    /// # Budget
    ///
    /// No document bytes are consumed: the argument is the authenticated file
    /// key, not document-origin input. Constant work over the fixed 16-byte
    /// `/Perms` blob; no budget interaction.
    ///
    /// # Malformed Input
    ///
    /// A truncated or forged `/Perms` blob fails verification and returns
    /// `false`; the caller treats the document's permission flags as
    /// untrustworthy and falls back to the strictest interpretation.
    #[must_use]
    pub fn verify_perms(&self, file_key: &[u8]) -> bool {
        if self.r < 6 || self.perms.is_empty() {
            return false;
        }
        selis_crypto::verify_perms_r6(self.p, file_key, &self.perms, self.encrypt_metadata)
    }
}

/// The decryption policy of an authenticated encrypted document: the file
/// key plus which objects are actually encrypted (ISO 32000-1 §7.4.10).
///
/// Streams use `/StmF`, strings use `/StrF`; either may be `/Identity`
/// (not encrypted). The metadata stream is not encrypted when
/// `/EncryptMetadata` is false. Per-stream `/Crypt` filters (SL-1.FILT.09)
/// are resolved from the `/CF` dictionary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecryptPolicy {
    /// The file encryption key.
    pub key: Vec<u8>,
    /// The handler revision.
    pub rev: u8,
    /// Whether the default stream filter (`/StmF`) is AES (`/AESV2`/`/AESV3`).
    pub aes: bool,
    /// Whether streams use the standard crypt filter (are encrypted).
    pub stream_encrypted: bool,
    /// Whether strings use the standard crypt filter (are encrypted).
    pub string_encrypted: bool,
    /// Whether the metadata stream is encrypted (`/EncryptMetadata`).
    pub encrypt_metadata: bool,
    /// The `/CF` dictionary entries, for per-stream `/Crypt` filter lookup.
    pub cf: Vec<(selis_bytes::Bytes, Obj)>,
}

impl DecryptPolicy {
    /// Build the policy for an authenticated document.
    #[must_use]
    pub fn from_encrypt(info: &EncryptInfo, key: Vec<u8>) -> Self {
        Self {
            key,
            rev: info.r,
            aes: info.aes,
            stream_encrypted: info.stream_encrypted(),
            string_encrypted: info.string_encrypted(),
            encrypt_metadata: info.encrypt_metadata,
            cf: info.cf.clone(),
        }
    }

    /// Whether the named crypt filter (from `/CF/<name>/CFM`) uses AES
    /// (`/AESV2`/`/AESV3`). Names absent from `/CF` are not AES.
    #[must_use]
    pub fn aes_for(&self, name: &str) -> bool {
        self.cf
            .iter()
            .find(|(k, _)| k.as_slice() == name.as_bytes())
            .and_then(|(_, v)| match v {
                Obj::Dict(p) => p.iter().find(|(k, _)| k.as_slice() == b"CFM"),
                _ => None,
            })
            .and_then(|(_, v)| match v {
                Obj::Name(n) => Some(String::from_utf8_lossy(n.as_slice()).to_string()),
                _ => None,
            })
            .is_some_and(|cfm| cfm == "AESV2" || cfm == "AESV3")
    }
}

/// Resolve and parse the `/Encrypt` dictionary from a trailer.
///
/// Returns `None` when the document is not encrypted.
pub fn parse_encrypt(
    src: &[u8],
    encrypt_ref: Option<Ref>,
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
) -> Result<Option<EncryptInfo>> {
    let Some(r) = encrypt_ref else {
        return Ok(None);
    };
    let offset = offset_of(src, r, budget, g)?;
    let obj = resolve_object_numbered(src, offset, r.num, budget, g)?;
    let Obj::Dict(pairs) = &obj else {
        return Ok(None);
    };
    let get = |key: &[u8]| -> Option<&Obj> {
        pairs
            .iter()
            .find(|(k, _)| k.as_slice() == key)
            .map(|(_, v)| v)
    };
    let filter = match get(b"Filter") {
        Some(Obj::Name(n)) => String::from_utf8_lossy(n.as_slice()).to_string(),
        _ => return Ok(None),
    };
    // /CF may be a direct dict or an indirect reference; resolve it when
    // it's a Ref so per-stream filter selection works (SL-1.FILT.09).
    let cf = resolve_cf_dict(src, budget, g, get(b"CF"));
    // Dispatch: the standard security handler, or the public-key handler
    // (SL-1.ENC.03). Acrobat writes /Adobe.PPKLite; PDFBox writes the
    // /Adobe.PubSec alias with the same dictionary shape.
    match filter.as_str() {
        "Standard" => parse_standard_encrypt(pairs, cf),
        "Adobe.PPKLite" | "Adobe.PubSec" => parse_pubkey_encrypt(pairs, cf),
        // An unknown security handler: reported as "no encryption info"
        // (the established tolerant-open posture; the document opens
        // without a key).
        _ => Ok(None),
    }
}

/// Parse the standard security handler's `/Encrypt` dictionary fields from
/// the already-resolved dict pairs.
fn parse_standard_encrypt(
    pairs: &[(selis_bytes::Bytes, Obj)],
    cf: Vec<(selis_bytes::Bytes, Obj)>,
) -> Result<Option<EncryptInfo>> {
    let get = |key: &[u8]| -> Option<&Obj> {
        pairs
            .iter()
            .find(|(k, _)| k.as_slice() == key)
            .map(|(_, v)| v)
    };
    let int = |key: &[u8]| -> Option<i64> {
        match get(key) {
            Some(Obj::Int(n)) => Some(*n),
            _ => None,
        }
    };
    let r = int(b"R").and_then(|v| u8::try_from(v).ok()).unwrap_or(0);
    let v = int(b"V").and_then(|v| u8::try_from(v).ok()).unwrap_or(0);
    let length = int(b"Length")
        .and_then(|v| usize::try_from(v).ok())
        .unwrap_or(if v >= 4 { 128 } else { 40 });
    let o = match get(b"O") {
        Some(Obj::String(b)) => b.as_slice().to_vec(),
        _ => return Ok(None),
    };
    let u = match get(b"U") {
        Some(Obj::String(b)) => b.as_slice().to_vec(),
        _ => return Ok(None),
    };
    // /P is a signed 32-bit integer (permission flags); the algorithms use
    // its two's-complement bit pattern as an unsigned little-endian value.
    let p = int(b"P")
        .and_then(|v| i32::try_from(v).ok())
        .map(|v| u32::from_ne_bytes(v.to_ne_bytes()))
        .unwrap_or(0);
    let encrypt_metadata = match get(b"EncryptMetadata") {
        Some(Obj::Name(n)) => n.as_slice() != b"false",
        Some(Obj::Bool(b)) => *b,
        _ => true,
    };
    // `/StmF` and `/StrF` exist only from `/V 4` on. Before that every stream
    // and string uses the standard RC4 handler, and a real `/V 2 /R 3` file has
    // no such entries at all — defaulting them to `/Identity` (Table 20's
    // default, which speaks to `/V 4+`) leaves the ciphertext in place, so a
    // rewrite that drops `/Encrypt` produces a document whose pages, strings
    // and Flate streams are still encrypted. A missing entry therefore means
    // "the standard handler" for revisions 2–3 and `/Identity` only for later
    // versions.
    let default_filter = if v <= 3 { "StdCF" } else { "Identity" };
    let stmf = match get(b"StmF") {
        Some(Obj::Name(n)) => String::from_utf8_lossy(n.as_slice()).to_string(),
        _ => default_filter.to_string(),
    };
    let strf = match get(b"StrF") {
        Some(Obj::Name(n)) => String::from_utf8_lossy(n.as_slice()).to_string(),
        _ => default_filter.to_string(),
    };
    // AES is used when the handler revision requires it (`/V` 4 or 5 are
    // AES-128/AES-256 by definition, ISO 32000-1 §7.6.3.2), or when the
    // standard crypt filter declares AESV2/AESV3 in `/CF/StdCF/CFM`. The
    // `/CF` check alone is not sufficient: a damaged or non-conformant writer
    // can set `/V 4` while `/CFM` names `/V2` (RC4) — the file key still
    // derives per `/V`, so the cipher must follow `/V`.
    let aes = v >= 4
        || (stmf == "StdCF"
            && cf
                .iter()
                .find(|(k, _)| k.as_slice() == b"StdCF")
                .and_then(|(_, v)| match v {
                    Obj::Dict(p) => p.iter().find(|(k, _)| k.as_slice() == b"CFM"),
                    _ => None,
                })
                .and_then(|(_, v)| match v {
                    Obj::Name(n) => Some(String::from_utf8_lossy(n.as_slice()).to_string()),
                    _ => None,
                })
                .is_some_and(|cfm| cfm == "AESV2" || cfm == "AESV3"));
    let ue = match get(b"UE") {
        Some(Obj::String(b)) => b.as_slice().to_vec(),
        _ => Vec::new(),
    };
    let oe = match get(b"OE") {
        Some(Obj::String(b)) => b.as_slice().to_vec(),
        _ => Vec::new(),
    };
    let perms = match get(b"Perms") {
        Some(Obj::String(b)) => b.as_slice().to_vec(),
        _ => Vec::new(),
    };
    Ok(Some(EncryptInfo {
        handler: Handler::Standard,
        r,
        v,
        length,
        o,
        u,
        p,
        stmf,
        strf,
        aes,
        encrypt_metadata,
        ue,
        oe,
        perms,
        cf,
        subfilter: None,
        recipients: Vec::new(),
    }))
}

/// Parse the public-key security handler's `/Encrypt` dictionary
/// (ISO 32000-2 §7.6.6.2, SL-1.ENC.03): `/V` (4 = AESV2, 5 = AESV3),
/// `/SubFilter` (`adbe.pkcs7.s4`/`s5`), and the `/Recipients` CMS blobs —
/// either directly on the dictionary (the s4 shape) or inside the crypt
/// filter named by `/StmF` (Acrobat's `/DefaultCryptFilter`, the s5 shape).
fn parse_pubkey_encrypt(
    pairs: &[(selis_bytes::Bytes, Obj)],
    cf: Vec<(selis_bytes::Bytes, Obj)>,
) -> Result<Option<EncryptInfo>> {
    let get = |key: &[u8]| -> Option<&Obj> {
        pairs
            .iter()
            .find(|(k, _)| k.as_slice() == key)
            .map(|(_, v)| v)
    };
    let int = |key: &[u8]| -> Option<i64> {
        match get(key) {
            Some(Obj::Int(n)) => Some(*n),
            _ => None,
        }
    };
    let v = int(b"V").and_then(|v| u8::try_from(v).ok()).unwrap_or(0);
    // The per-object cipher revision follows /V: 4 → salted AES-128
    // (Algorithm 1), 5 → direct-key AES-256 (Algorithm 1a); anything below 4
    // is the RC4-era `s3` shape (per-object RC4 key derivation, ISO 32000-1
    // §7.6.2) whose *sealed CMS payload* still decrypts through the same
    // Algorithm-1 seed hash (SL-1.ENC.08: RC4 / TDEA-CBC / RC2-CBC content keys
    // are accepted read-only; see `selis_crypto::pkcs7::decrypt_payload`).
    let r = v;
    let length = int(b"Length")
        .and_then(|v| usize::try_from(v).ok())
        .unwrap_or(if v >= 5 { 256 } else { 128 });
    // The public-key dictionary may carry `/P` (real writers do; the spec
    // defines per-recipient permissions *inside* the CMS payload, so the
    // table-23 entry is advisory here). SL-1.ENC.09 intersects the two: a
    // document without `/P` grants the recipient's CMS bits alone, so the
    // default when the key is absent is "everything the CMS allows".
    let p = int(b"P")
        .and_then(|v| i32::try_from(v).ok())
        .map(|v| u32::from_ne_bytes(v.to_ne_bytes()))
        .unwrap_or(0xFFFF_FFFF);
    let subfilter = match get(b"SubFilter") {
        Some(Obj::Name(n)) => Some(String::from_utf8_lossy(n.as_slice()).to_string()),
        _ => None,
    };
    let encrypt_metadata = match get(b"EncryptMetadata") {
        Some(Obj::Name(n)) => n.as_slice() != b"false",
        Some(Obj::Bool(b)) => *b,
        _ => true,
    };
    let stmf_owned = match get(b"StmF") {
        Some(Obj::Name(n)) => String::from_utf8_lossy(n.as_slice()).to_string(),
        // The public-key handler's default crypt filter name (ISO 32000-1
        // Table 25; PDFBox's COSName.DEFAULT_CRYPT_FILTER).
        _ => "DefaultCryptFilter".to_string(),
    };
    let strf = match get(b"StrF") {
        Some(Obj::Name(n)) => String::from_utf8_lossy(n.as_slice()).to_string(),
        _ => stmf_owned.clone(),
    };
    // /Recipients: directly on the dictionary (s4/PDFBox shape) or inside
    // the crypt filter /StmF names (Acrobat's /DefaultCryptFilter shape).
    let mut recipients = strings_of(get(b"Recipients"));
    if recipients.is_empty() {
        recipients = cf
            .iter()
            .find(|(k, _)| k.as_slice() == stmf_owned.as_bytes())
            .and_then(|(_, v)| match v {
                Obj::Dict(p) => p.iter().find(|(k, _)| k.as_slice() == b"Recipients"),
                _ => None,
            })
            .map(|(_, v)| strings_of(Some(v)))
            .unwrap_or_default();
    }
    // AES per the named crypt filter's /CFM (AESV2/AESV3); /V ≥ 4 public-key
    // documents are AES by definition.
    let aes = v >= 4;
    Ok(Some(EncryptInfo {
        handler: Handler::PubKey,
        r,
        v,
        length,
        o: Vec::new(),
        u: Vec::new(),
        p,
        stmf: stmf_owned,
        strf,
        aes,
        encrypt_metadata,
        ue: Vec::new(),
        oe: Vec::new(),
        perms: Vec::new(),
        cf,
        subfilter,
        recipients,
    }))
}

/// The raw byte strings of a COS string or array-of-strings object.
fn strings_of(obj: Option<&Obj>) -> Vec<Vec<u8>> {
    match obj {
        Some(Obj::String(b)) => vec![b.as_slice().to_vec()],
        Some(Obj::Array(items)) => items
            .iter()
            .filter_map(|o| match o {
                Obj::String(b) => Some(b.as_slice().to_vec()),
                _ => None,
            })
            .collect(),
        _ => Vec::new(),
    }
}

/// Resolve the `/CF` dictionary, which may be a direct `<<...>>` or an
/// indirect reference (`N G R`). Falls back to an empty dict on error.
fn resolve_cf_dict(
    src: &[u8],
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
    cf_obj: Option<&Obj>,
) -> Vec<(selis_bytes::Bytes, Obj)> {
    match cf_obj {
        Some(Obj::Dict(pairs)) => pairs.clone(),
        Some(Obj::Ref(r)) => {
            match offset_of(src, *r, budget, g)
                .and_then(|off| resolve_object_numbered(src, off, r.num, budget, g))
            {
                Ok(Obj::Dict(pairs)) => pairs,
                _ => Vec::new(),
            }
        }
        _ => Vec::new(),
    }
}

/// Extract `/ID[0]` from the trailer (used in key derivation).
pub fn document_id(trailer: &[(selis_bytes::Bytes, Obj)]) -> Vec<u8> {
    trailer
        .iter()
        .find(|(k, _)| k.as_slice() == b"ID")
        .and_then(|(_, v)| match v {
            Obj::Array(items) => items.first().and_then(|o| match o {
                Obj::String(b) => Some(b.as_slice().to_vec()),
                _ => None,
            }),
            _ => None,
        })
        .unwrap_or_default()
}

/// Authenticate the user password and return the encryption key.
///
/// # Budget
///
/// No budget: fixed-cost hash work over caller-supplied values.
///
/// # Malformed Input
///
/// A wrong password or damaged `/O`/`/U` values yield `None`, never an error
/// and never a partial key. A public-key `/Encrypt` dictionary (which has no
/// `/O`/`/U`) yields `None` — authenticate it with
/// [`authenticate_public_key`].
pub fn authenticate(info: &EncryptInfo, id0: &[u8], password: &[u8]) -> Option<Vec<u8>> {
    if info.handler != Handler::Standard {
        return None;
    }
    selis_crypto::authenticate_user(
        &info.o,
        &info.u,
        info.p,
        id0,
        info.r,
        info.length,
        info.aes,
        info.encrypt_metadata,
        &info.ue,
        &info.oe,
        password,
    )
}

/// Authenticate a public-key document with the recipient's private key and
/// return the derived file encryption key plus the recipient's permission
/// bits (SL-1.ENC.03, ISO 32000-2 §7.6.6.4 Algorithm 1; SL-1.ENC.07
/// certificate-identity selection).
///
/// Selection follows the credential's [`selis_crypto::pkcs7::MatchBy`]
/// policy: with a certificate chain the recipients are first addressed by
/// their CMS `RecipientIdentifier` (issuer/serial or
/// subjectKeyIdentifier) and only those are unwrapped; without a chain (or
/// with no identifier present in the blob list under `auto`) the draft's
/// structural first-try-in-array-order scan applies (design note §4).
///
/// # Budget
///
/// The CMS parses charge each blob's wire bytes to `g`; the seed-hash
/// concatenation is charged before allocation. A hostile blob set
/// terminates within its own byte budget.///
/// # Malformed Input
///
/// Structural CMS damage is `ENCRYPT_MALFORMED` (a damaged certificate in
/// the credential's chain likewise — never silently dropped); a recognised
/// but unimplemented algorithm (AES-192-wrapped on ECDH, PKCS#12 transports,
/// detached CMS content) is `ENCRYPT_UNSUPPORTED`; a credential that opens no
/// recipient is `RECIPIENT_NO_MATCH` (the public-key wrong-key error — typed,
/// never a partial decrypt; under `MatchBy::Certificate` a credential whose
/// chain matches no recipient fails with it even if the key material would
/// have opened a differently-addressed recipient). The `/V ≤ 3` s3-era RC4/TDEA/
/// RC2 content reads (SL-1.ENC.08) go through the same typed path: the key
/// transport's unwrap gates them, not the content cipher's padding.
pub fn authenticate_public_key(
    info: &EncryptInfo,
    credential: &selis_crypto::pkcs7::PubKeyCredential,
    g: &mut BudgetGuard<'_>,
) -> Result<selis_crypto::pkcs7::PubKeyAuth> {
    if info.handler != Handler::PubKey {
        return Err(err!(
            Code::EncryptMalformed,
            during = "encrypt",
            detail = "authenticate_public_key on a non-public-key handler"
        ));
    }
    // The `adbe.pkcs7.s3` shape (/V ≤ 3, RC4-era) *is* accepted (SL-1.ENC.08):
    // its RC4 content keys have *no padding*, so their gate is the
    // RSAES-PKCS1-v1_5 (or RSAES-OAEP) key transport having unwrapped before
    // any content key exists — see `selis_crypto::pkcs7::decrypt_payload` and
    // design note §5. A wrong key therefore still ends `RECIPIENT_NO_MATCH`,
    // never a silently-wrong file key.
    let blobs: Vec<&[u8]> = info.recipients.iter().map(|r| r.as_slice()).collect();
    selis_crypto::pkcs7::authenticate_public_key(
        &blobs,
        credential,
        info.length,
        info.v >= 5,
        info.encrypt_metadata,
        g,
    )
}

/// Decrypt a stream/string value for a specific object.
///
/// # Budget
///
/// No budget: the crypto layer walks the caller-supplied buffer once.
///
/// # Malformed Input
///
/// Truncated ciphertext yields the partial plaintext the block mode allows;
/// callers re-validate the result when decoding it (stream filters, string
/// parsing) rather than trusting length.
pub fn decrypt_data(info: &EncryptInfo, key: &[u8], objnum: u32, gen: u16, data: &[u8]) -> Vec<u8> {
    selis_crypto::decrypt_data(key, objnum, gen, data, info.r, info.aes)
}

/// Build the `/Encrypt` dictionary as a COS object, ready to write as an
/// indirect object.
#[must_use]
pub fn encrypt_dict(info: &EncryptInfo) -> Obj {
    let mut pairs: Vec<(selis_bytes::Bytes, Obj)> = vec![
        (bytes(b"Filter"), Obj::Name(bytes(b"Standard"))),
        (bytes(b"V"), Obj::Int(i64::from(info.v))),
        (bytes(b"R"), Obj::Int(i64::from(info.r))),
        (
            bytes(b"Length"),
            Obj::Int(i64::try_from(info.length).unwrap_or(256)),
        ),
        (bytes(b"O"), Obj::HexString(bytes(&info.o))),
        (bytes(b"U"), Obj::HexString(bytes(&info.u))),
        // /P is a signed 32-bit integer: write the two's-complement bit
        // pattern so values with the high bit set round-trip through the
        // i32 parser.
        (
            bytes(b"P"),
            Obj::Int(i64::from(i32::from_ne_bytes(info.p.to_ne_bytes()))),
        ),
        (bytes(b"StmF"), Obj::Name(bytes(info.stmf.as_bytes()))),
        (bytes(b"StrF"), Obj::Name(bytes(info.strf.as_bytes()))),
    ];
    if !info.ue.is_empty() {
        pairs.push((bytes(b"UE"), Obj::HexString(bytes(&info.ue))));
    }
    if !info.oe.is_empty() {
        pairs.push((bytes(b"OE"), Obj::HexString(bytes(&info.oe))));
    }
    if !info.perms.is_empty() {
        pairs.push((bytes(b"Perms"), Obj::HexString(bytes(&info.perms))));
    }
    if !info.encrypt_metadata {
        pairs.push((bytes(b"EncryptMetadata"), Obj::Bool(false)));
    }
    // The /CF dictionary.
    if !info.cf.is_empty() {
        let cf_dict = Obj::Dict(
            info.cf
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect(),
        );
        pairs.push((bytes(b"CF"), cf_dict));
    }
    Obj::Dict(pairs)
}

/// Encrypt all strings and stream bodies in an object for the given object
/// number, following Algorithm 1 (per-object salted key for rev ≤ 4, direct
/// file key for rev ≥ 5).
///
/// The `/Encrypt` dictionary itself must NOT be passed through this function.
///
/// # Budget
///
/// No budget is consumed: the input is an already-resolved in-memory `Obj`
/// graph plus the file key, not document bytes. Output grows only by the
/// cipher's block padding over the input's own size.
///
/// # Malformed Input
///
/// The object graph is trusted (it was built by the writer, not parsed from
/// a document). Names, references, and numbers pass through unchanged;
/// strings and streams are encrypted in place. `/Length` is rewritten to the
/// ciphertext length, so a wrong pre-existing `/Length` cannot corrupt the
/// output.
#[must_use]
pub fn encrypt_object(obj: &Obj, key: &[u8], objnum: u32, gen: u16, rev: u8, aes: bool) -> Obj {
    match obj {
        Obj::String(b) => {
            let ct = selis_crypto::encrypt_data(key, objnum, gen, b.as_slice(), rev, aes);
            Obj::String(bytes(&ct))
        }
        Obj::Array(items) => Obj::Array(
            items
                .iter()
                .map(|o| encrypt_object(o, key, objnum, gen, rev, aes))
                .collect(),
        ),
        Obj::Dict(pairs) => Obj::Dict(
            pairs
                .iter()
                .map(|(k, v)| (k.clone(), encrypt_object(v, key, objnum, gen, rev, aes)))
                .collect(),
        ),
        Obj::Stream { dict, data } => {
            let ct = selis_crypto::encrypt_data(key, objnum, gen, data.as_slice(), rev, aes);
            let mut new_dict = dict.clone();
            let ct_len = i64::try_from(ct.len()).unwrap_or(i64::MAX);
            if let Some((_, v)) = new_dict.iter_mut().find(|(k, _)| k.as_slice() == b"Length") {
                *v = Obj::Int(ct_len);
            } else {
                new_dict.push((bytes(b"Length"), Obj::Int(ct_len)));
            }
            Obj::Stream {
                dict: new_dict,
                data: bytes(&ct),
            }
        }
        other => other.clone(),
    }
}

/// Find the byte offset of a reference via the xref index.
fn offset_of(src: &[u8], r: Ref, budget: &Budget, g: &mut BudgetGuard<'_>) -> Result<u64> {
    let startxref = crate::xref::find_startxref(src, 4096).unwrap_or(0);
    let doc = crate::parse_revisions(src, startxref, budget, g)?;
    // Newest revision wins: an incremental update redefining the object
    // supersedes the older entry (ISO 32000-1 §7.5.8.3).
    for view in doc.revisions().iter().rev() {
        if let Some(crate::XrefEntry::InUse { offset, .. }) = view.entries.get(&r.num) {
            return Ok(*offset);
        }
    }
    Err(err!(
        Code::ObjUnexpected,
        during = "encrypt",
        object = r.num
    ))
}

fn bytes(v: &[u8]) -> selis_bytes::Bytes {
    selis_bytes::Bytes::copy_from_slice(v)
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing,
        clippy::arithmetic_side_effects
    )]

    use super::*;

    /// A fixed 64-byte credential blob for the tests below. A test that
    /// asserts what `/U`, `/UE` and `/Perms` contain has to pin the key and
    /// salts, which is exactly what the host-injected path lets it do.
    fn fixed_material() -> R6Material {
        R6Material::from_bytes(&[0x5Au8; R6Material::LEN]).expect("a 64-byte credential blob")
    }

    /// One `/Encrypt` byte string by the name a reviewer would ask for it.
    fn entry_of<'a>(info: &'a EncryptInfo, name: &str) -> &'a [u8] {
        let bytes = match name {
            "/U" => Some(info.u.as_slice()),
            "/O" => Some(info.o.as_slice()),
            "/UE" => Some(info.ue.as_slice()),
            "/OE" => Some(info.oe.as_slice()),
            "/Perms" => Some(info.perms.as_slice()),
            _ => None,
        };
        bytes.unwrap_or_else(|| panic!("no /Encrypt entry named {name}"))
    }

    /// `/StmF` and `/StrF` only exist from `/V 4` on, so a revision-3 handler
    /// that omits them encrypts everything with the standard handler. Reading
    /// their absence as Table 20's `/Identity` default (which speaks to
    /// `/V 4+`) leaves the ciphertext in place: a real UIDAI revision-3 file
    /// unlocked to a document whose Flate streams were still encrypted and
    /// whose page content was noise.
    #[test]
    fn revision_3_defaults_to_the_standard_handler_not_identity() {
        let pairs = [
            (selis_bytes::Bytes::copy_from_slice(b"V"), Obj::Int(2)),
            (selis_bytes::Bytes::copy_from_slice(b"R"), Obj::Int(3)),
            (
                selis_bytes::Bytes::copy_from_slice(b"Length"),
                Obj::Int(128),
            ),
            (
                selis_bytes::Bytes::copy_from_slice(b"O"),
                Obj::String(selis_bytes::Bytes::from(vec![0u8; 32])),
            ),
            (
                selis_bytes::Bytes::copy_from_slice(b"U"),
                Obj::String(selis_bytes::Bytes::from(vec![0u8; 32])),
            ),
            (selis_bytes::Bytes::copy_from_slice(b"P"), Obj::Int(-4)),
        ];
        let info = parse_standard_encrypt(&pairs, Vec::new())
            .unwrap()
            .expect("a standard /Encrypt dictionary");
        assert!(
            info.stream_encrypted() && info.string_encrypted(),
            "revision 3 without /StmF or /StrF encrypts streams and strings"
        );
        assert!(!info.aes, "revision 3 is RC4, never AES");

        // From /V 4 on, an absent /StmF really does mean Identity (Table 20).
        let modern = [
            (selis_bytes::Bytes::copy_from_slice(b"V"), Obj::Int(5)),
            (selis_bytes::Bytes::copy_from_slice(b"R"), Obj::Int(6)),
            (
                selis_bytes::Bytes::copy_from_slice(b"Length"),
                Obj::Int(256),
            ),
            (
                selis_bytes::Bytes::copy_from_slice(b"O"),
                Obj::String(selis_bytes::Bytes::from(vec![0u8; 48])),
            ),
            (
                selis_bytes::Bytes::copy_from_slice(b"U"),
                Obj::String(selis_bytes::Bytes::from(vec![0u8; 48])),
            ),
            (selis_bytes::Bytes::copy_from_slice(b"P"), Obj::Int(-4)),
        ];
        let info = parse_standard_encrypt(&modern, Vec::new())
            .unwrap()
            .expect("a standard /Encrypt dictionary");
        assert!(
            !info.stream_encrypted() && !info.string_encrypted(),
            "/V 5 keeps Table 20's /Identity default when the entries are absent"
        );
    }

    /// A document encrypted by the browser path is reproducible: same
    /// credentials, same passwords, same `/P` — the same bytes out. Nothing in
    /// the write path reaches for an entropy source any more, which is what
    /// makes a guest with no CSPRNG able to write this format at all.
    #[test]
    fn injected_credentials_reproduce_the_dictionary() {
        let first = EncryptInfo::new_r6(b"user", b"owner", 0xFFFF_FFC4, &fixed_material()).0;
        let second = EncryptInfo::new_r6(b"user", b"owner", 0xFFFF_FFC4, &fixed_material()).0;
        assert_eq!(first, second, "one credential blob gives one dictionary");
        assert_eq!(first.r, 6);
        assert_eq!(first.v, 5);
        assert_eq!(first.u.len(), 48);
        assert_eq!(first.o.len(), 48);
        assert_eq!(first.ue.len(), 32);
        assert_eq!(first.oe.len(), 32);
        assert_eq!(first.perms.len(), 16);
    }

    /// Which credential byte reaches which `/Encrypt` entry.
    ///
    /// This is the map Algorithms 2.A/2.B/10 imply, written down as an
    /// executable claim: the file key only appears where the key is wrapped or
    /// the permissions are sealed; the user salts sit inside `/U` (and so
    /// change `/O` and `/OE`, which hash `/U`); the owner salts only reach the
    /// owner's own entries. A test that asserted "every byte moves `/U`" would
    /// have been wrong, and it was — until it said so.
    #[test]
    fn each_credential_byte_reaches_exactly_the_entries_it_should() {
        let base = EncryptInfo::new_r6(b"user", b"owner", 0xFFFF_FFC4, &fixed_material()).0;
        // (offset, what it is, the entries that must move, the ones that must not)
        let table: [(usize, &str, &[&str], &[&str]); 5] = [
            (0, "file key", &["/UE", "/OE", "/Perms"], &["/U", "/O"]),
            (
                32,
                "user validation salt",
                &["/U", "/O", "/OE"],
                &["/UE", "/Perms"],
            ),
            (
                40,
                "user key salt",
                &["/U", "/UE", "/O", "/OE"],
                &["/Perms"],
            ),
            (
                48,
                "owner validation salt",
                &["/O"],
                &["/U", "/UE", "/OE", "/Perms"],
            ),
            (
                56,
                "owner key salt",
                &["/O", "/OE"],
                &["/U", "/UE", "/Perms"],
            ),
        ];
        for (offset, what, moves, holds) in table {
            let mut blob = [0x5Au8; R6Material::LEN];
            blob[offset] ^= 0x01;
            let changed = EncryptInfo::new_r6(
                b"user",
                b"owner",
                0xFFFF_FFC4,
                &R6Material::from_bytes(&blob).expect("a 64-byte credential blob"),
            )
            .0;
            for entry in moves {
                assert_ne!(
                    entry_of(&base, entry),
                    entry_of(&changed, entry),
                    "the {what} must move {entry}"
                );
            }
            for entry in holds {
                assert_eq!(
                    entry_of(&base, entry),
                    entry_of(&changed, entry),
                    "the {what} must not move {entry}"
                );
            }
        }
    }

    /// A wrong length is a refusal, not a truncation: silently dropping the
    /// tail would shrink the salt the owner password is keyed by.
    #[test]
    fn a_credential_blob_is_all_sixty_four_bytes_or_nothing() {
        assert!(R6Material::from_bytes(&[0u8; 63]).is_none());
        assert!(R6Material::from_bytes(&[0u8; 65]).is_none());
        assert!(R6Material::from_bytes(&[]).is_none());
        assert!(R6Material::from_bytes(&[0u8; 64]).is_some());
    }

    /// `/ID` comes from the file key, so a fresh save (fresh key) is a fresh
    /// identifier, and the same save reproduces the same one.
    #[test]
    fn document_id_follows_the_file_key() {
        let one = fixed_material();
        let mut other_bytes = [0x5Au8; R6Material::LEN];
        other_bytes[0] ^= 0x80;
        let other = R6Material::from_bytes(&other_bytes).expect("a 64-byte credential blob");
        assert_eq!(one.document_id().len(), 16);
        assert_eq!(one.document_id(), fixed_material().document_id());
        assert_ne!(one.document_id(), other.document_id());
    }

    /// The native writer refuses rather than invents: `generate` is the only
    /// path that can produce a key without a host supplying one, and on this
    /// host it succeeds.
    #[test]
    fn the_native_draw_is_random_or_absent() {
        let a = R6Material::generate().expect("this host has an OS CSPRNG");
        let b = R6Material::generate().expect("this host has an OS CSPRNG");
        assert_ne!(a.file_key, b.file_key, "two saves must not share a key");
        assert_ne!(a.u_v_salt, b.u_v_salt);
    }

    /// Build the dict pairs `parse_pubkey_encrypt` expects (the same shape
    /// the resolver hands `parse_encrypt`).
    macro_rules! dict_pairs {
        ($($name:literal => $value:expr),+ $(,)?) => {{
            vec![$((bytes($name.as_bytes()), $value)),+]
        }};
    }

    /// `/Adobe.PPKLite` s5 shape: recipients inside /CF/DefaultCryptFilter.
    #[test]
    fn pubkey_s5_dispatch_reads_crypt_filter_recipients() {
        let blob = Obj::String(bytes(vec![0x30u8; 40].as_slice()));
        let default_cf = Obj::Dict(vec![
            (bytes(b"CFM"), Obj::Name(bytes(b"AESV3"))),
            (bytes(b"Length"), Obj::Int(256)),
            (bytes(b"Recipients"), Obj::Array(vec![blob])),
        ]);
        let pairs = dict_pairs! {
            "Filter" => Obj::Name(bytes(b"Adobe.PPKLite")),
            "V" => Obj::Int(5),
            "Length" => Obj::Int(256),
            "SubFilter" => Obj::Name(bytes(b"adbe.pkcs7.s5")),
            "StmF" => Obj::Name(bytes(b"DefaultCryptFilter")),
            "StrF" => Obj::Name(bytes(b"DefaultCryptFilter")),
        };
        let cf = vec![(bytes(b"DefaultCryptFilter"), default_cf)];
        let info = parse_pubkey_encrypt(&pairs, cf)
            .expect("parse")
            .expect("info");
        assert_eq!(info.handler, Handler::PubKey);
        assert_eq!(info.v, 5);
        assert_eq!(info.r, 5);
        assert_eq!(info.subfilter.as_deref(), Some("adbe.pkcs7.s5"));
        assert!(info.aes, "AESV3 crypt filter means AES");
        assert_eq!(info.recipients.len(), 1);
        assert!(
            info.stream_encrypted(),
            "DefaultCryptFilter encrypts streams"
        );
        assert!(info.string_encrypted());
        // The standard-handler authentication refuses a public-key info.
        assert!(authenticate(&info, b"", b"").is_none());
    }

    /// `/Adobe.PubSec` s4 shape: /Recipients directly on the dictionary.
    #[test]
    fn pubkey_s4_dispatch_reads_dict_recipients() {
        let blob = Obj::String(bytes(vec![0x31u8; 40].as_slice()));
        let pairs = dict_pairs! {
            "Filter" => Obj::Name(bytes(b"Adobe.PubSec")),
            "V" => Obj::Int(4),
            "Length" => Obj::Int(128),
            "SubFilter" => Obj::Name(bytes(b"adbe.pkcs7.s4")),
            "Recipients" => Obj::Array(vec![blob]),
        };
        let info = parse_pubkey_encrypt(&pairs, Vec::new())
            .expect("parse")
            .expect("info");
        assert_eq!(info.handler, Handler::PubKey);
        assert_eq!(info.v, 4);
        assert!(info.aes);
        assert_eq!(info.recipients.len(), 1);
        // No /CF, no /StmF: the default crypt filter still encrypts.
        assert!(info.stream_encrypted());
    }

    /// The s3-era (RC4/TDEA/RC2, /V ≤ 3) public-key shape parses and is no
    /// longer refused at authentication (SL-1.ENC.08); its RC4 content keys are
    /// gated by the key transport's unwrap, so a *malformed* blob is a typed
    /// CMS error from the crypto layer, not a cos-layer `ENCRYPT_UNSUPPORTED`.
    #[test]
    fn pubkey_s3_is_accepted_and_gated_by_the_cms_parse() {
        let pairs = dict_pairs! {
            "Filter" => Obj::Name(bytes(b"Adobe.PPKLite")),
            "V" => Obj::Int(2),
            "Length" => Obj::Int(128),
            "SubFilter" => Obj::Name(bytes(b"adbe.pkcs7.s3")),
            "Recipients" => Obj::Array(vec![Obj::String(bytes(vec![0x30u8; 40].as_slice()))]),
        };
        let info = parse_pubkey_encrypt(&pairs, Vec::new())
            .expect("parse")
            .expect("info");
        assert_eq!(info.v, 2);
        let credential = selis_crypto::pkcs7::PubKeyCredential::rsa([0u8; 8].to_vec());
        let budget = Budget::unlimited();
        let mut g = budget.guard();
        let e = authenticate_public_key(&info, &credential, &mut g).expect_err("corrupt blob");
        assert_eq!(e.code(), Code::EncryptMalformed);
    }

    /// `authenticate_public_key` refuses standard-handler infos.
    #[test]
    fn pubkey_authentication_refuses_standard_infos() {
        let (info, _) = EncryptInfo::new_r6(b"", b"", 0xFFFF_F0C0, &fixed_material());
        let credential = selis_crypto::pkcs7::PubKeyCredential::rsa([0u8; 8].to_vec());
        let budget = Budget::unlimited();
        let mut g = budget.guard();
        let e = authenticate_public_key(&info, &credential, &mut g).expect_err("wrong handler");
        assert_eq!(e.code(), Code::EncryptMalformed);
    }

    /// `stream_encrypted` keeps treating a named /CFM /None filter as
    /// unencrypted (the SL-1.FILT.09 identity posture).
    #[test]
    fn named_crypt_filter_with_none_cfm_is_not_encrypted() {
        let none_cf = Obj::Dict(vec![(bytes(b"CFM"), Obj::Name(bytes(b"None")))]);
        let pairs = dict_pairs! {
            "Filter" => Obj::Name(bytes(b"Adobe.PPKLite")),
            "V" => Obj::Int(5),
            "StmF" => Obj::Name(bytes(b"DefaultCryptFilter")),
            "StrF" => Obj::Name(bytes(b"DefaultCryptFilter")),
        };
        let cf = vec![(bytes(b"DefaultCryptFilter"), none_cf)];
        let info = parse_pubkey_encrypt(&pairs, cf)
            .expect("parse")
            .expect("info");
        assert!(!info.stream_encrypted());
        assert!(!info.string_encrypted());
    }
}
