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

/// The parsed `/Encrypt` dictionary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EncryptInfo {
    /// The handler revision (`/R`).
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
}

impl EncryptInfo {
    /// Whether the stream crypt filter is the standard handler (streams are
    /// encrypted). `/Identity` streams are not.
    #[must_use]
    pub fn stream_encrypted(&self) -> bool {
        self.stmf == "StdCF"
    }

    /// Whether the string crypt filter is the standard handler (strings are
    /// encrypted). `/Identity` strings are not.
    #[must_use]
    pub fn string_encrypted(&self) -> bool {
        self.strf == "StdCF"
    }

    /// The stream crypt filter (`/StmF`) — currently only `/Identity` and
    /// `/StdCF` are supported.
    pub fn supports(&self) -> bool {
        self.v <= 5 && self.r >= 2 && self.r <= 6
    }

    /// Build a revision-6 `/Encrypt` dictionary and the file encryption key.
    ///
    /// Returns the [`EncryptInfo`] (with `/O`, `/U`, `/UE`, `/OE`, `/Perms` all
    /// computed) and the 32-byte file key. The document ID `id0` (from
    /// [`document_id`]) is used for key derivation.
    #[must_use]
    pub fn new_r6(
        user_password: &[u8],
        owner_password: &[u8],
        p: u32,
        _id0: &[u8],
    ) -> (Self, Vec<u8>) {
        let file_key = selis_crypto::random_bytes(32);
        let u_v_salt = selis_crypto::random_bytes(8);
        let u_k_salt = selis_crypto::random_bytes(8);
        let o_v_salt = selis_crypto::random_bytes(8);
        let o_k_salt = selis_crypto::random_bytes(8);
        let u = selis_crypto::compute_u_r6(user_password, &u_v_salt, &u_k_salt, 6);
        let o = selis_crypto::compute_o_r6(owner_password, &o_v_salt, &o_k_salt, &u, 6);
        let ue = selis_crypto::compute_ue_r6(user_password, &u_k_salt, &file_key, 6);
        let oe = selis_crypto::compute_oe_r6(owner_password, &o_k_salt, &u, &file_key, 6);
        let perms = selis_crypto::compute_perms_r6(p, &file_key);
        let cf_std_cf = Obj::Dict(vec![
            (bytes(b"CFM"), Obj::Name(bytes(b"AESV3"))),
            (bytes(b"Length"), Obj::Int(32)),
            (bytes(b"AuthEvent"), Obj::Name(bytes(b"DocOpen"))),
        ]);
        let info = Self {
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
        };
        (info, file_key)
    }

    /// Verify the stored `/Perms` blob against this info's permission flags
    /// and the file key (revision 6, read side). Returns `false` when `/Perms`
    /// is absent, truncated, or decrypts to different flags/bytes — a damaged
    /// or re-keyed document.
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
        selis_crypto::verify_perms_r6(self.p, file_key, &self.perms)
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
    let int = |key: &[u8]| -> Option<i64> {
        match get(key) {
            Some(Obj::Int(n)) => Some(*n),
            _ => None,
        }
    };
    let filter = match get(b"Filter") {
        Some(Obj::Name(n)) => String::from_utf8_lossy(n.as_slice()).to_string(),
        _ => return Ok(None),
    };
    if filter != "Standard" {
        return Ok(None); // only the standard security handler
    }
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
    let stmf = match get(b"StmF") {
        Some(Obj::Name(n)) => String::from_utf8_lossy(n.as_slice()).to_string(),
        _ => "Identity".to_string(),
    };
    let strf = match get(b"StrF") {
        Some(Obj::Name(n)) => String::from_utf8_lossy(n.as_slice()).to_string(),
        _ => "Identity".to_string(),
    };
    // /CF may be a direct dict or an indirect reference. Resolve it when
    // it's a Ref so per-stream filter selection works (SL-1.FILT.09).
    let cf = resolve_cf_dict(src, budget, g, get(b"CF"));
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
    }))
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
/// and never a partial key.
pub fn authenticate(info: &EncryptInfo, id0: &[u8], password: &[u8]) -> Option<Vec<u8>> {
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
