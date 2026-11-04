//! Reading the signatures a document carries, and saying exactly what was
//! checked.
//!
//! A report is not a badge. Each entry distinguishes the three claims that are
//! easy to conflate:
//!
//! * **cryptographic** — the CMS signature verifies over the bytes its
//!   `/ByteRange` names, and those bytes are the document's own;
//! * **coverage** — the `/ByteRange` leaves out nothing except the `/Contents`
//!   string itself, so a byte appended after signing is visible as such;
//! * **trust** — the signer's certificate chains to an anchor the caller
//!   supplied. With no anchors, nothing is trusted, and the report says so.
//!
//! Revocation is never checked: OCSP needs a network and a CRL needs a URL
//! this crate will not fetch. [`SignaturesReport::revocation_checked`] is
//! always `false`, so no shell can present a green check that implies
//! otherwise.

use selis_pdf_cos::copy::resolve_ref;
use selis_pdf_cos::{Doc, Obj};
use selis_sandbox::{Budget, BudgetGuard};
use serde::{Deserialize, Serialize};

use crate::{dict_get, malformed, number, resolve, trailer, SignError};

/// The most signature fields a walk collects. A hostile form cannot turn a
/// report into unbounded work.
pub const MAX_SIGNATURES: usize = 512;
/// How deep a field tree may nest before the walk stops.
const MAX_FIELD_DEPTH: usize = 32;

/// One link of a certificate chain, as a report shows it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChainLinkView {
    /// Subject common name.
    pub subject: String,
    /// Issuer common name.
    pub issuer: String,
    /// `notBefore`, Unix seconds.
    pub not_before: i64,
    /// `notAfter`, Unix seconds.
    pub not_after: i64,
    /// Whether the issuer's signature over this certificate verified.
    pub signature_ok: bool,
    /// Whether the certificate was inside its validity window.
    pub time_ok: bool,
    /// Whether the caller supplied this certificate as an anchor.
    pub anchor: bool,
}

/// The trust half of a signature report.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TrustReport {
    /// The signer certificate's subject common name.
    pub subject: String,
    /// The signer certificate's issuer common name.
    pub issuer: String,
    /// Every link verified and the walk ended on a supplied anchor.
    pub trusted: bool,
    /// Why the walk did not end on an anchor, when it did not.
    pub reason: Option<String>,
    /// Always `false`: no OCSP or CRL is consulted.
    pub revocation_checked: bool,
    /// Leaf first, anchor last.
    pub chain: Vec<ChainLinkView>,
}

/// One signature dictionary, checked.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SignatureReport {
    /// The field's fully qualified name.
    pub field: String,
    /// The 1-based page its widget paints on, when the widget names one.
    pub page: Option<u64>,
    /// `/SubFilter`, e.g. `adbe.pkcs7.detached`.
    pub sub_filter: Option<String>,
    /// The `/Name` the signer asserted, when present.
    pub signer: Option<String>,
    /// `/Reason`, when present.
    pub reason: Option<String>,
    /// `/Location`, when present.
    pub location: Option<String>,
    /// `/M`, the signing time the signer wrote, verbatim.
    pub signing_time: Option<String>,
    /// The four `/ByteRange` values, when the array is well-formed.
    pub byte_range: Option<[u64; 4]>,
    /// The range leaves out nothing but the `/Contents` string.
    pub covers_whole_document: bool,
    /// The CMS signature verifies over the bytes the range names.
    pub verified: bool,
    /// The digest algorithm the signer used, when this build knows it.
    pub digest: Option<String>,
    /// Why the signature did not verify, when it did not.
    pub error: Option<String>,
    /// The certificate chain verdict, when a certificate was found.
    pub trust: Option<TrustReport>,
}

/// Every signature in the newest revision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SignaturesReport {
    /// Always `"ok"`: an unverified signature is a report, not a failure.
    pub status: String,
    /// Signed fields, in field-tree order.
    pub signatures: Vec<SignatureReport>,
    /// Signature fields present but not yet signed.
    pub unsigned_fields: usize,
    /// Always `false` (see the module docs).
    pub revocation_checked: bool,
}

/// The compact JSON the tools chunk and the CLI print.
///
/// # Errors
///
/// [`SignError::Malformed`] if serialisation fails, which cannot happen for
/// this shape but is not a reason to unwrap.
pub fn signature_report_json(report: &SignaturesReport) -> Result<Vec<u8>, SignError> {
    serde_json::to_vec(report).map_err(|e| malformed(format!("cannot encode the report: {e}")))
}

/// Walk every signature field and check it against the bytes it names.
///
/// `anchors` are DER certificates the caller trusts; an empty slice means
/// "nothing is trusted", which is reported rather than assumed. `now` is Unix
/// seconds, supplied by the caller because this crate has no clock.
///
/// # Errors
///
/// Only a document that cannot be opened at all is an error. A malformed
/// signature dictionary is a report entry with `verified: false` and an
/// [`SignatureReport::error`] — a user must see "this did not check", not lose
/// the rest of the report.
pub fn signatures(
    src: &[u8],
    anchors: &[Vec<u8>],
    now: i64,
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
) -> Result<SignaturesReport, SignError> {
    let doc = crate::open_doc(src, budget, g)?;
    if crate::is_encrypted(&doc) {
        return Err(SignError::Encrypted);
    }
    let mut fields = Vec::new();
    collect_signature_fields(src, &doc, budget, g, &mut fields)?;

    let mut signatures = Vec::new();
    let mut unsigned_fields = 0usize;
    for field in fields.iter().take(MAX_SIGNATURES) {
        let Some(value) = field.value.as_ref() else {
            unsigned_fields = unsigned_fields.saturating_add(1);
            continue;
        };
        signatures.push(check_one(src, &doc, value, field, anchors, now, budget, g));
    }
    Ok(SignaturesReport {
        status: "ok".to_string(),
        signatures,
        unsigned_fields,
        revocation_checked: false,
    })
}

struct SignatureField {
    name: String,
    page: Option<u64>,
    value: Option<selis_pdf_cos::Ref>,
}

fn collect_signature_fields(
    src: &[u8],
    doc: &Doc,
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
    out: &mut Vec<SignatureField>,
) -> Result<(), SignError> {
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
    let Obj::Dict(catalog_pairs) = catalog else {
        return Err(malformed("the catalog is not a dictionary"));
    };
    let Some(form) = dict_get(&catalog_pairs, b"AcroForm").cloned() else {
        return Ok(());
    };
    let form = resolve(src, doc, form, budget, g)?;
    let Obj::Dict(form_pairs) = form else {
        return Ok(());
    };
    let Some(fields) = dict_get(&form_pairs, b"Fields").cloned() else {
        return Ok(());
    };
    let fields = resolve(src, doc, fields, budget, g)?;
    let Obj::Array(fields) = fields else {
        return Ok(());
    };
    let page_refs = crate::pages(src, doc, budget, g)
        .map(|pages| pages.iter().map(|page| page.reference).collect::<Vec<_>>())
        .unwrap_or_default();
    for field in &fields {
        walk_field(src, doc, field, "", None, &page_refs, budget, g, out, 0)?;
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn walk_field(
    src: &[u8],
    doc: &Doc,
    value: &Obj,
    parent_name: &str,
    inherited_ft: Option<&[u8]>,
    page_refs: &[selis_pdf_cos::Ref],
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
    out: &mut Vec<SignatureField>,
    depth: usize,
) -> Result<(), SignError> {
    if depth > MAX_FIELD_DEPTH || out.len() >= MAX_SIGNATURES {
        return Ok(());
    }
    let resolved = match value {
        Obj::Ref(reference) => match resolve_ref(src, doc, *reference, budget, g) {
            Ok(obj) => obj,
            Err(_) => return Ok(()),
        },
        other => other.clone(),
    };
    let Obj::Dict(pairs) = resolved else {
        return Ok(());
    };
    let own_name = dict_get(&pairs, b"T")
        .and_then(string_bytes)
        .map(decode_text)
        .unwrap_or_default();
    let name = if parent_name.is_empty() {
        own_name
    } else if own_name.is_empty() {
        parent_name.to_string()
    } else {
        format!("{parent_name}.{own_name}")
    };
    let field_type = dict_get(&pairs, b"FT")
        .and_then(name_bytes)
        .or(inherited_ft);
    if field_type == Some(b"Sig") {
        let page = dict_get(&pairs, b"P").and_then(|value| match value {
            Obj::Ref(reference) => page_refs
                .iter()
                .position(|candidate| candidate == reference)
                .and_then(|index| u64::try_from(index).ok())
                .map(|index| index.saturating_add(1)),
            _ => None,
        });
        let signed = dict_get(&pairs, b"V").and_then(|value| match value {
            Obj::Ref(reference) => Some(*reference),
            _ => None,
        });
        out.push(SignatureField {
            name,
            page,
            value: signed,
        });
        return Ok(());
    }
    if let Some(Obj::Array(kids)) = dict_get(&pairs, b"Kids") {
        for kid in kids {
            walk_field(
                src,
                doc,
                kid,
                &name,
                field_type,
                page_refs,
                budget,
                g,
                out,
                depth.saturating_add(1),
            )?;
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn check_one(
    src: &[u8],
    doc: &Doc,
    value: &selis_pdf_cos::Ref,
    field: &SignatureField,
    anchors: &[Vec<u8>],
    now: i64,
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
) -> SignatureReport {
    let mut report = SignatureReport {
        field: field.name.clone(),
        page: field.page,
        sub_filter: None,
        signer: None,
        reason: None,
        location: None,
        signing_time: None,
        byte_range: None,
        covers_whole_document: false,
        verified: false,
        digest: None,
        error: None,
        trust: None,
    };
    let Ok(dict) = resolve(src, doc, Obj::Ref(*value), budget, g) else {
        report.error = Some("the signature dictionary could not be resolved".to_string());
        return report;
    };
    let Obj::Dict(pairs) = dict else {
        report.error = Some("the /V entry is not a signature dictionary".to_string());
        return report;
    };
    report.sub_filter = dict_get(&pairs, b"SubFilter")
        .and_then(name_bytes)
        .map(|bytes| String::from_utf8_lossy(bytes).into_owned());
    report.signer = dict_get(&pairs, b"Name")
        .and_then(string_bytes)
        .map(decode_text);
    report.reason = dict_get(&pairs, b"Reason")
        .and_then(string_bytes)
        .map(decode_text);
    report.location = dict_get(&pairs, b"Location")
        .and_then(string_bytes)
        .map(decode_text);
    report.signing_time = dict_get(&pairs, b"M")
        .and_then(string_bytes)
        .map(decode_text);

    let contents = match dict_get(&pairs, b"Contents").and_then(string_bytes) {
        Some(bytes) => trim_trailing_zeros(bytes),
        None => {
            report.error = Some("the signature dictionary has no /Contents".to_string());
            return report;
        }
    };
    let ranges = match read_byte_range(src, doc, &pairs, budget, g) {
        Some(ranges) => ranges,
        None => {
            report.error = Some("the signature has no well-formed /ByteRange".to_string());
            return report;
        }
    };
    report.byte_range = Some(ranges);
    report.covers_whole_document = covers_document(src, ranges, contents);
    if !report.covers_whole_document {
        report.error = Some(
            "the /ByteRange does not cover the whole document; bytes were changed or added \
             after signing"
                .to_string(),
        );
    }
    let spans = match spans_for(src, ranges) {
        Some(spans) => spans,
        None => {
            report.error = Some("the /ByteRange points outside the file".to_string());
            return report;
        }
    };

    let sub_filter_ok = matches!(
        report.sub_filter.as_deref(),
        Some("adbe.pkcs7.detached" | "ETSI.CAdES.detached")
    );
    if !sub_filter_ok {
        report.error = Some(format!(
            "the sub-filter {} is not one this build verifies",
            report.sub_filter.as_deref().unwrap_or("(missing)")
        ));
        return report;
    }

    let cms_budget = selis_sandbox::Budget::profile(selis_sandbox::Surface::Fuzz);
    let mut cms_guard = cms_budget.guard();
    let Ok(cms) = selis_crypto::cms::parse_signed_data(contents, &mut cms_guard) else {
        report.error = Some("the /Contents blob is not a readable CMS signature".to_string());
        return report;
    };
    let Some(signer) = cms.signers.first() else {
        report.error = Some("the CMS signature carries no signer".to_string());
        return report;
    };
    let Some(digest) = signer.digest else {
        report.error =
            Some("the signer's digest algorithm is not one this build knows".to_string());
        return report;
    };
    report.digest = Some(digest.name().to_string());
    let document_digest = selis_crypto::cms::digest_spans(digest, &spans);
    match selis_crypto::cms::verify_signer(signer, &cms.certificates, &document_digest) {
        Ok(()) => {
            report.verified = true;
            report.error = None;
        }
        Err(error) => {
            if report.error.is_none() {
                report.error = Some(format!("the signature does not verify: {error}"));
            }
        }
    }

    // Trust is reported even when the signature fails, because a user who sees
    // a broken signature still needs to know whose certificate claimed it.
    if let Some(leaf) = selis_crypto::cms::signer_certificates(signer, &cms.certificates)
        .first()
        .copied()
    {
        report.trust = trust_report(leaf, &cms.certificates, anchors, now);
    }
    report
}

fn trust_report(
    leaf: &[u8],
    certificates: &[&[u8]],
    anchors: &[Vec<u8>],
    now: i64,
) -> Option<TrustReport> {
    let intermediates: Vec<&[u8]> = certificates
        .iter()
        .copied()
        .filter(|candidate| *candidate != leaf)
        .collect();
    let anchor_refs: Vec<&[u8]> = anchors.iter().map(Vec::as_slice).collect();
    let budget = selis_sandbox::Budget::profile(selis_sandbox::Surface::Fuzz);
    let mut g = budget.guard();
    let report =
        selis_crypto::cert::validate_chain(leaf, &intermediates, &anchor_refs, now, &mut g).ok()?;
    let subject = report
        .links
        .first()
        .map(|link| link.subject.clone())
        .unwrap_or_default();
    let issuer = report
        .links
        .first()
        .map(|link| link.issuer.clone())
        .unwrap_or_default();
    Some(TrustReport {
        subject,
        issuer,
        trusted: report.trusted,
        reason: report.reason,
        revocation_checked: false,
        chain: report
            .links
            .into_iter()
            .map(|link| ChainLinkView {
                subject: link.subject,
                issuer: link.issuer,
                not_before: link.not_before,
                not_after: link.not_after,
                signature_ok: link.signature_ok,
                time_ok: link.time_ok,
                anchor: link.anchor,
            })
            .collect(),
    })
}

fn read_byte_range(
    src: &[u8],
    doc: &Doc,
    pairs: &[(selis_bytes::Bytes, Obj)],
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
) -> Option<[u64; 4]> {
    let value = resolve(src, doc, dict_get(pairs, b"ByteRange")?.clone(), budget, g).ok()?;
    let Obj::Array(items) = value else {
        return None;
    };
    if items.len() != 4 {
        return None;
    }
    let mut out = [0u64; 4];
    for (slot, item) in out.iter_mut().zip(items.iter()) {
        let number = number(item)?;
        if !(0.0..1.0e18).contains(&number) {
            return None;
        }
        *slot = number as u64;
    }
    Some(out)
}

fn covers_document(src: &[u8], ranges: [u64; 4], contents: &[u8]) -> bool {
    let [start, length, second_start, second_length] = ranges;
    if start != 0 {
        return false;
    }
    let first_end = start.saturating_add(length);
    let second_end = second_start.saturating_add(second_length);
    if first_end > second_start || second_end != u64::try_from(src.len()).unwrap_or(u64::MAX) {
        return false;
    }
    // The gap is exactly the `/Contents` hex literal: `<` + two hex digits per
    // byte + `>`.
    let gap = second_start.saturating_sub(first_end);
    let expected = u64::try_from(contents.len())
        .unwrap_or(u64::MAX)
        .saturating_mul(2)
        .saturating_add(2);
    if gap != expected {
        return false;
    }
    let at = usize::try_from(first_end).unwrap_or(usize::MAX);
    let end = usize::try_from(second_start).unwrap_or(usize::MAX);
    let Some(slice) = src.get(at..end) else {
        return false;
    };
    slice.first() == Some(&b'<') && slice.last() == Some(&b'>')
}

fn spans_for(src: &[u8], ranges: [u64; 4]) -> Option<[&[u8]; 2]> {
    let [start, length, second_offset, second_length] = ranges;
    let first_start = usize::try_from(start).ok()?;
    let first_end = usize::try_from(start.checked_add(length)?).ok()?;
    let second_start = usize::try_from(second_offset).ok()?;
    let second_end = usize::try_from(second_offset.checked_add(second_length)?).ok()?;
    Some([
        src.get(first_start..first_end)?,
        src.get(second_start..second_end)?,
    ])
}

/// Hex decode, used by the signer to check what it wrote reads back.
#[cfg(test)]
fn hex_decode(text: &[u8]) -> Option<Vec<u8>> {
    if text.len() % 2 != 0 {
        return None;
    }
    let mut out = Vec::with_capacity(text.len() / 2);
    let mut index = 0usize;
    while index < text.len() {
        let hi = hex_value(*text.get(index)?)?;
        let lo = hex_value(*text.get(index.saturating_add(1))?)?;
        out.push(hi.checked_shl(4)?.checked_add(lo)?);
        index = index.saturating_add(2);
    }
    Some(out)
}

#[cfg(test)]
fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

fn trim_trailing_zeros(bytes: &[u8]) -> &[u8] {
    let end = bytes
        .iter()
        .rposition(|byte| *byte != 0)
        .map_or(0, |index| index.saturating_add(1));
    bytes.get(..end).unwrap_or(&[])
}

fn name_bytes(value: &Obj) -> Option<&[u8]> {
    match value {
        Obj::Name(name) => Some(name.as_slice()),
        _ => None,
    }
}

fn string_bytes(value: &Obj) -> Option<&[u8]> {
    match value {
        Obj::String(text) => Some(text.as_slice()),
        Obj::HexString(text) => Some(text.as_slice()),
        _ => None,
    }
}

/// Decode a PDF text string: UTF-16BE when it carries a BOM, Latin-1
/// otherwise. Display only — signatures are compared as bytes.
fn decode_text(raw: &[u8]) -> String {
    if raw.starts_with(&[0xFE, 0xFF]) {
        let units: Vec<u16> = raw
            .get(2..)
            .unwrap_or(&[])
            .chunks_exact(2)
            .map(|pair| {
                u16::from_be_bytes([
                    pair.first().copied().unwrap_or(0),
                    pair.get(1).copied().unwrap_or(0),
                ])
            })
            .collect();
        String::from_utf16_lossy(&units)
    } else if raw.starts_with(&[0xFF, 0xFE]) {
        let units: Vec<u16> = raw
            .get(2..)
            .unwrap_or(&[])
            .chunks_exact(2)
            .map(|pair| {
                u16::from_le_bytes([
                    pair.first().copied().unwrap_or(0),
                    pair.get(1).copied().unwrap_or(0),
                ])
            })
            .collect();
        String::from_utf16_lossy(&units)
    } else {
        raw.iter().map(|byte| char::from(*byte)).collect()
    }
}

/// Encode a report field name that came from the document, for shells.
#[must_use]
pub fn display_name(field: &SignatureReport) -> String {
    if field.field.is_empty() {
        "unnamed signature field".to_string()
    } else {
        field.field.clone()
    }
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

    #[test]
    fn a_hex_decode_rejects_odd_lengths_and_junk() {
        assert_eq!(hex_decode(b"00FF").as_deref(), Some(&[0u8, 255][..]));
        assert!(hex_decode(b"0F0").is_none());
        assert!(hex_decode(b"ZZ").is_none());
    }

    #[test]
    fn trailing_zero_padding_is_trimmed() {
        assert_eq!(trim_trailing_zeros(&[1, 2, 0, 0]), &[1, 2]);
        assert_eq!(trim_trailing_zeros(&[0, 0]), &[] as &[u8]);
    }

    #[test]
    fn byte_range_must_name_the_contents_gap() {
        let mut src = b"<00>tail".to_vec();
        // The first range stops before the /Contents literal, the second starts
        // after its closing '>', so the gap is exactly "<00>".
        let ranges = [0u64, 0, 4, 4];
        assert!(covers_document(&src, ranges, &[0]));
        // A range that stops short leaves bytes uncovered.
        src.push(b'!');
        assert!(!covers_document(&src, ranges, &[0]));
    }

    #[test]
    fn latin1_and_utf16_text_decode() {
        assert_eq!(decode_text(b"Ada"), "Ada");
        assert_eq!(
            decode_text(&[0xFE, 0xFF, 0x00, 0x41, 0x00, 0x64, 0x00, 0x61]),
            "Ada"
        );
    }
}
