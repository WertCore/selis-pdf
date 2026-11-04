//! Signing for the Selis PDF engine (SL-5.SIGN.01, Phase 4).
//!
//! Three things live here, and they are deliberately separate:
//!
//! * a **visual stamp** ([`sign_visual`]) — typed text and freehand strokes
//!   appended to one page's content as an incremental revision, so nothing
//!   already in the file is rewritten;
//! * a **detached CMS signature** ([`sign_digital`]) — a signature field and
//!   widget whose `/V` carries an `adbe.pkcs7.detached` blob over the file's
//!   own `/ByteRange`, with the visible block as its appearance;
//! * a **report** ([`signatures`]) — every signature dictionary in the newest
//!   revision, each checked against the bytes it names, with a chain verdict
//!   that ends at anchors the caller supplied.
//!
//! The honesty rules this crate is built around:
//!
//! * every write is an **incremental revision**: the bytes before the edit are
//!   preserved verbatim, so a signature over an earlier revision stays
//!   verifiable and a viewer can report "signed, then modified" rather than a
//!   silent break;
//! * a signature is *verified* only when the CMS digest matches the byte
//!   ranges **and** the signer's key verifies the signed attributes. A
//!   document whose CMS parses but does not verify reports `verified: false`
//!   with the reason, never a green check;
//! * a certificate is *trusted* only through an anchor the caller passes in.
//!   There is no built-in root store, and self-signed is not a verdict;
//! * revocation is **not checked** — [`SignatureReport::revocation_checked`]
//!   is always `false`, and the UI says so;
//! * signing keys never leave the process: the crate takes PEM text, parses
//!   it, uses it, and never writes it anywhere.
//!
//! Signing is RSA only (PKCS#1 v1.5, SHA-256). An EC key is refused by name:
//! ECDSA signing needs entropy for the nonce, and the browser chunk that
//! calls this crate is deliberately import-free. Verification of ECDSA
//! signatures still works through [`selis_crypto::cert`], so a document signed
//! elsewhere reads.
//!
//! This crate never touches the filesystem or the network; it reads the bytes
//! the caller hands it and returns the bytes the caller hands out.

#![forbid(unsafe_code)]
#![allow(
    // Geometry is a chain of small float operations on validated values; the
    // crate still refuses a non-finite input at every entry point.
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::integer_division
)]

use std::fmt;

use selis_bytes::Bytes;
use selis_pdf_cos::obj::{Obj, Ref};
use selis_pdf_cos::Doc;
use selis_sandbox::{Budget, BudgetGuard};

pub mod digital;
pub mod report;
pub mod visual;

#[cfg(test)]
mod tests;

pub use digital::{sign_digital, DigitalOptions, DigitalReport};
pub use report::{
    signature_report_json, signatures, SignatureReport, SignaturesReport, TrustReport,
};
pub use visual::{sign_visual, VisualLine, VisualOptions, VisualReport, VisualStroke};

/// The most pages a visual stamp may walk to find its target.
pub(crate) const MAX_PAGES: usize = 100_000;
/// The most freehand points a stamp accepts.
pub(crate) const MAX_STROKE_POINTS: usize = 5_000;
/// The most bytes a typed line may carry.
pub(crate) const MAX_TEXT_BYTES: usize = 512;
/// The most bytes of PEM key material the caller may pass.
pub(crate) const MAX_KEY_BYTES: usize = 64 * 1024;

/// A refusal or failure from this crate.
///
/// The variants are the categories a shell has to tell apart: the document is
/// not readable, the request is wrong, the document class refuses the edit, or
/// the bytes produced did not verify.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SignError {
    /// The bytes are not a PDF this engine can read.
    Malformed(String),
    /// An object the walk needs could not be resolved.
    Unresolvable(String),
    /// The document carries `/Encrypt`; its dictionaries are ciphertext here.
    Encrypted,
    /// The request itself is wrong (page out of range, empty name, bad rect).
    Invalid(String),
    /// The request exceeds a documented limit.
    Limit(String),
    /// The document class or its existing signatures refuse the edit.
    Refused(String),
    /// The produced bytes did not read back as the requested signature.
    ReadBack(String),
    /// The key, certificate or algorithm is not something this build signs.
    Unsupported(String),
}

impl fmt::Display for SignError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Malformed(detail) => write!(f, "this PDF cannot be read: {detail}"),
            Self::Unresolvable(detail) => write!(f, "an object could not be resolved: {detail}"),
            Self::Encrypted => write!(
                f,
                "this document is encrypted; remove the password before signing"
            ),
            Self::Invalid(detail) => write!(f, "{detail}"),
            Self::Limit(detail) => write!(f, "{detail}"),
            Self::Refused(detail) => write!(f, "{detail}"),
            Self::ReadBack(detail) => write!(
                f,
                "the signature did not read back from the bytes produced: {detail}"
            ),
            Self::Unsupported(detail) => write!(f, "{detail}"),
        }
    }
}

impl std::error::Error for SignError {}

pub(crate) fn malformed(detail: impl Into<String>) -> SignError {
    SignError::Malformed(detail.into())
}

pub(crate) fn invalid(detail: impl Into<String>) -> SignError {
    SignError::Invalid(detail.into())
}

/// # Budget
///
/// Copies exactly `value.len()` bytes; the caller has already charged the
/// buffer this is stored in.
///
/// # Malformed Input
///
/// None: any byte string is a valid PDF name or string body.
pub(crate) fn bytes(value: &[u8]) -> Bytes {
    Bytes::copy_from_slice(value)
}

/// Open the document and keep every revision addressable.
pub(crate) fn open_doc(
    src: &[u8],
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
) -> Result<Doc, SignError> {
    let startxref = selis_pdf_cos::xref::find_startxref(src, 4096)
        .ok_or_else(|| malformed("no startxref was found"))?;
    selis_pdf_cos::parse_revisions(src, startxref, budget, g).map_err(|e| malformed(e.to_string()))
}

/// Whether the newest revision declares an encrypted document.
pub(crate) fn is_encrypted(doc: &Doc) -> bool {
    doc.revisions()
        .last()
        .is_some_and(|revision| revision.encrypt.is_some())
}

/// The newest revision's trailer.
pub(crate) fn trailer(doc: &Doc) -> Result<&selis_pdf_cos::revision::Revision, SignError> {
    doc.revisions()
        .last()
        .ok_or_else(|| malformed("the document has no revision"))
}

/// Resolve an indirect reference (or pass an inline value through).
pub(crate) fn resolve(
    src: &[u8],
    doc: &Doc,
    obj: Obj,
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
) -> Result<Obj, SignError> {
    match obj {
        Obj::Ref(r) => selis_pdf_cos::copy::resolve_ref(src, doc, r, budget, g)
            .map_err(|e| SignError::Unresolvable(e.to_string())),
        other => Ok(other),
    }
}

/// A page the walk found: its object reference and geometry.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Page {
    pub reference: Ref,
    /// Unrotated media-box size in points.
    pub width: f64,
    pub height: f64,
    /// Normalized `/Rotate` (0, 90, 180, 270).
    pub rotation: i64,
}

impl Page {
    pub(crate) fn display_size(&self) -> (f64, f64) {
        if matches!(self.rotation, 90 | 270) {
            (self.height, self.width)
        } else {
            (self.width, self.height)
        }
    }

    /// Convert a display-space point (bottom-left origin) into user space.
    pub(crate) fn display_to_user(&self, u: f64, v: f64) -> (f64, f64) {
        match self.rotation {
            90 => (self.width - v, u),
            180 => (self.width - u, self.height - v),
            270 => (v, self.height - u),
            _ => (u, v),
        }
    }

    /// Map a display-space direction into user space.
    pub(crate) fn display_dir_to_user(&self, du: f64, dv: f64) -> (f64, f64) {
        match self.rotation {
            90 => (-dv, du),
            180 => (-du, -dv),
            270 => (dv, -du),
            _ => (du, dv),
        }
    }
}

/// Every page, in reading order.
pub(crate) fn pages(
    src: &[u8],
    doc: &Doc,
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
) -> Result<Vec<Page>, SignError> {
    let revision = trailer(doc)?;
    let catalog = resolve(
        src,
        doc,
        Obj::Ref(
            revision
                .root
                .ok_or_else(|| malformed("the trailer has no /Root"))?,
        ),
        budget,
        g,
    )?;
    let Obj::Dict(entries) = catalog else {
        return Err(malformed("the catalog is not a dictionary"));
    };
    let pages_ref = dict_get(&entries, b"Pages")
        .and_then(|value| match value {
            Obj::Ref(r) => Some(*r),
            _ => None,
        })
        .ok_or_else(|| malformed("the catalog has no /Pages"))?;
    let mut out = Vec::new();
    walk_pages(src, doc, pages_ref, None, 0, budget, g, &mut out)?;
    Ok(out)
}

#[allow(clippy::too_many_arguments)]
fn walk_pages(
    src: &[u8],
    doc: &Doc,
    node: Ref,
    inherited_media: Option<(f64, f64)>,
    depth: usize,
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
    out: &mut Vec<Page>,
) -> Result<(), SignError> {
    if depth > 64 || out.len() > MAX_PAGES {
        return Err(SignError::Limit(
            "the page tree is deeper or larger than this build walks".to_string(),
        ));
    }
    let node_obj = resolve(src, doc, Obj::Ref(node), budget, g)?;
    let Obj::Dict(entries) = node_obj else {
        return Err(malformed("a page-tree node is not a dictionary"));
    };
    let media = media_box(src, doc, &entries, budget, g).or(inherited_media);
    match dict_get(&entries, b"Kids") {
        Some(Obj::Array(kids)) => {
            for kid in kids {
                let Obj::Ref(kid_ref) = kid else { continue };
                walk_pages(src, doc, *kid_ref, media, depth + 1, budget, g, out)?;
            }
            Ok(())
        }
        _ => {
            let (width, height) =
                media.ok_or_else(|| malformed("a page has no media box to inherit"))?;
            if !(width.is_finite() && height.is_finite() && width > 0.0 && height > 0.0) {
                return Err(malformed("a page has an invalid media box"));
            }
            let rotation = dict_get(&entries, b"Rotate")
                .and_then(|value| match value {
                    Obj::Int(n) => Some(*n),
                    _ => None,
                })
                .map(|n| n.rem_euclid(360))
                .unwrap_or(0);
            if !matches!(rotation, 0 | 90 | 180 | 270) {
                return Err(malformed("a page rotation is not a multiple of 90"));
            }
            out.push(Page {
                reference: node,
                width,
                height,
                rotation,
            });
            Ok(())
        }
    }
}

fn media_box(
    src: &[u8],
    doc: &Doc,
    entries: &[(Bytes, Obj)],
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
) -> Option<(f64, f64)> {
    let value = dict_get(entries, b"MediaBox")?;
    let resolved = resolve(src, doc, value.clone(), budget, g).ok()?;
    let Obj::Array(items) = resolved else {
        return None;
    };
    let numbers: Vec<f64> = items.iter().filter_map(number).collect();
    if numbers.len() != 4 {
        return None;
    }
    let width = (numbers[2] - numbers[0]).abs();
    let height = (numbers[3] - numbers[1]).abs();
    Some((width, height))
}

pub(crate) fn number(value: &Obj) -> Option<f64> {
    match value {
        Obj::Int(n) => Some(*n as f64),
        Obj::Real { scaled, scale } => Some(*scaled as f64 / 10f64.powi(i32::from(*scale))),
        _ => None,
    }
}

/// Look a key up in a dictionary.
///
/// # Budget
///
/// One comparison per entry; no allocation.
///
/// # Malformed Input
///
/// A key no entry carries is simply absent. No input is refused here.
pub(crate) fn dict_get<'a>(entries: &'a [(Bytes, Obj)], key: &[u8]) -> Option<&'a Obj> {
    entries
        .iter()
        .find(|(k, _)| k.as_slice() == key)
        .map(|(_, v)| v)
}

/// Replace or insert a key in a dictionary, returning the map.
///
/// # Budget
///
/// At most one append; the entry list is caller-owned and already bounded by
/// the dictionary it was read from.
///
/// # Malformed Input
///
/// A repeated key overwrites in place rather than adding a second entry, so a
/// caller cannot grow the dictionary by passing the same key twice.
pub(crate) fn dict_set(entries: &mut Vec<(Bytes, Obj)>, key: &[u8], value: Obj) {
    if let Some(slot) = entries.iter_mut().find(|(k, _)| k.as_slice() == key) {
        slot.1 = value;
    } else {
        entries.push((bytes(key), value));
    }
}

/// Format a real the way the rest of the engine does: short, no exponent,
/// no trailing zeros.
pub(crate) fn real(value: f64) -> String {
    let rounded = (value * 1000.0).round() / 1000.0;
    if (rounded - rounded.trunc()).abs() < f64::EPSILON {
        format!("{}", rounded.trunc() as i64)
    } else {
        let mut text = format!("{rounded:.3}");
        while text.ends_with('0') {
            text.pop();
        }
        if text.ends_with('.') {
            text.pop();
        }
        text
    }
}

/// Encode a line as a PDF literal string body (WinAnsi).
///
/// # Errors
///
/// A character outside WinAnsi is refused rather than replaced with `?`: a
/// signature block that silently mangles the signer's name is worse than one
/// that asks them to type it another way.
pub(crate) fn encode_winansi(text: &str) -> Result<Vec<u8>, SignError> {
    let mut out = Vec::with_capacity(text.len());
    for ch in text.chars() {
        let code = ch as u32;
        if code <= 0xFF {
            out.push(u8::try_from(code).unwrap_or(b'?'));
        } else {
            return Err(SignError::Invalid(format!(
                "{ch:?} cannot be drawn in the built-in WinAnsi font; use Latin-1 characters"
            )));
        }
    }
    Ok(out)
}

/// Escape a WinAnsi byte string for a PDF literal string.
///
/// # Budget
///
/// One pass over the input and a buffer at most twice its length.
///
/// # Malformed Input
///
/// Every byte is accepted: escaping only adds a backslash before the three
/// delimiters, so arbitrary bytes cannot break out of the literal.
pub(crate) fn escape_text(bytes: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(bytes.len());
    for byte in bytes {
        match byte {
            b'(' | b')' | b'\\' => {
                out.push(b'\\');
                out.push(*byte);
            }
            _ => out.push(*byte),
        }
    }
    out
}

/// A text width estimate for the built-in Helvetica metrics (1/1000 em).
///
/// Only used to centre a typed line inside its block; the exact widths of the
/// standard 14 fonts are a viewer detail, and a signature block that is a
/// point off centre is not a correctness claim.
///
/// # Budget
///
/// One pass over the input; no allocation.
///
/// # Malformed Input
///
/// Any byte outside the table takes the default width, so the result is always
/// finite for a finite `size`.
pub(crate) fn text_width(bytes: &[u8], size: f64) -> f64 {
    let units: u32 = bytes
        .iter()
        .map(|byte| match byte {
            b' ' => 278u32,
            b'i' | b'l' | b'.' | b',' | b'\'' | b'!' => 278,
            b'm' | b'w' => 833,
            b'M' | b'W' => 944,
            b'0'..=b'9' => 556,
            b'A'..=b'Z' => 667,
            _ => 556,
        })
        .sum();
    f64::from(units) * size / 1000.0
}

/// The trailer entries the incremental revision has to carry forward.
pub(crate) fn carried_trailer(carried: &selis_pdf_cos::revision::Revision) -> Vec<(Vec<u8>, Obj)> {
    let mut out = Vec::new();
    for key in [b"Root".as_slice(), b"Info", b"ID"] {
        if let Some(value) = dict_get(&carried.trailer, key) {
            out.push((key.to_vec(), value.clone()));
        }
    }
    out
}

/// The next free object number in the newest revision.
pub(crate) fn next_object_number(doc: &Doc) -> u32 {
    doc.revisions()
        .last()
        .map(|revision| {
            revision
                .entries
                .keys()
                .copied()
                .max()
                .unwrap_or(0)
                .saturating_add(1)
        })
        .unwrap_or(1)
}
