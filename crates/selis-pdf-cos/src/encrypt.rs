//! PDF standard security handler — `/Encrypt` parsing and key setup
//! (SL-1.ENC.01/02).
//!
//! Parses the `/Encrypt` dictionary, authenticates a password (user/owner
//! passwords, revisions 2–4, RC4 and AES-128), and exposes the encryption key
//! the resolver uses to decrypt streams and strings.

use selis_error::{err, Code, Result};
use selis_sandbox::{Budget, BudgetGuard};

use crate::obj::{Obj, Ref};
use crate::resolve_object;

/// The parsed `/Encrypt` dictionary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EncryptInfo {
    /// The handler revision (`/R`).
    pub r: u8,
    /// The algorithm (`/V`): 1 = RC4-40, 2 = RC4-128, 4 = AES-128.
    pub v: u8,
    /// The key length in bits (`/Length`; 40 for `/V 1`).
    pub length: usize,
    /// `/O`.
    pub o: Vec<u8>,
    /// `/U`.
    pub u: Vec<u8>,
    /// `/P` (permission flags).
    pub p: u32,
    /// Whether the stream/string crypt filters are AES.
    pub aes: bool,
}

impl EncryptInfo {
    /// The stream crypt filter (`/StmF`) — currently only `/Identity` and
    /// `/StdCF` are supported.
    pub fn supports(&self) -> bool {
        self.v <= 4 && self.r <= 4
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
    let obj = resolve_object(src, offset, budget, g)?;
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
    let p = int(b"P").and_then(|v| u32::try_from(v).ok()).unwrap_or(0);
    // AES is used when /CF /StdCF /CFM is /AESV2 and /StmF is /StdCF.
    let aes = {
        let stmf = match get(b"StmF") {
            Some(Obj::Name(n)) => String::from_utf8_lossy(n.as_slice()).to_string(),
            _ => "Identity".to_string(),
        };
        let cf = match get(b"CF") {
            Some(Obj::Dict(pairs)) => pairs.clone(),
            _ => Vec::new(),
        };
        let std_cfm = cf
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
            .unwrap_or_default();
        stmf == "StdCF" && std_cfm == "AESV2"
    };
    Ok(Some(EncryptInfo {
        r,
        v,
        length,
        o,
        u,
        p,
        aes,
    }))
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
    for view in doc.revisions() {
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
