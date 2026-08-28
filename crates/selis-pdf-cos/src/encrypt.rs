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
}

/// The decryption policy of an authenticated encrypted document: the file
/// key plus which objects are actually encrypted (ISO 32000-1 §7.4.10).
///
/// Streams use `/StmF`, strings use `/StrF`; either may be `/Identity`
/// (not encrypted). The metadata stream is not encrypted when
/// `/EncryptMetadata` is false.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecryptPolicy {
    /// The file encryption key.
    pub key: Vec<u8>,
    /// The handler revision.
    pub rev: u8,
    /// Whether the standard stream filter is AES (`/AESV2`/`/AESV3`).
    pub aes: bool,
    /// Whether streams use the standard crypt filter (are encrypted).
    pub stream_encrypted: bool,
    /// Whether strings use the standard crypt filter (are encrypted).
    pub string_encrypted: bool,
    /// Whether the metadata stream is encrypted (`/EncryptMetadata`).
    pub encrypt_metadata: bool,
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
        }
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
        .unwrap_or(40);
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
    // AES is used when the standard stream crypt filter is AESV2/AESV3.
    let aes = stmf == "StdCF"
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
            .is_some_and(|cfm| cfm == "AESV2" || cfm == "AESV3");
    let ue = match get(b"UE") {
        Some(Obj::String(b)) => b.as_slice().to_vec(),
        _ => Vec::new(),
    };
    let oe = match get(b"OE") {
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
