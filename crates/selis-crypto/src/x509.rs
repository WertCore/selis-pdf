//! The X.509 certificate facts the CMS recipient-identifier match needs
//! (SL-1.ENC.07, RFC 5280 §4.1; RFC 5652 §6).
//!
//! PDF public-key recipients are addressed by a CMS `RecipientIdentifier` —
//! either `issuerAndSerialNumber` (the issuer `Name` plus the certificate
//! serial) or `subjectKeyIdentifier` (RFC 5652 §6.2, ISO 32000-2 §7.6.6).
//! To resolve such an identifier against a supplied certificate the reader
//! only needs four facts from the certificate, and it never needs a trust
//! decision: which key the recipient's private key material can open is
//! proven by the unwrap itself (design note §4). This module therefore parses
//! a deliberately narrow, **signature-not-verifying** subset of
//! `Certificate`: it extracts the identity fields and skips the rest — the
//! same posture as PDFBox's `CertId` and qpdf's recipient matching.
//!
//! The DER reader is [`crate::der`]; the parse is flat (no recursion) and
//! every TLV is charged to the budget, so untrusted certificate bytes are
//! bounded by their own wire size.

use selis_error::Result;
use selis_sandbox::BudgetGuard;
use sha1::Digest as _;

use crate::der::{self, Der, Tag};

/// `subjectKeyIdentifier` (RFC 5280 §4.2.1.2).
const EXT_SUBJECT_KEY_IDENTIFIER: &[u64] = &[2, 5, 29, 14];
/// `id-ecPublicKey` (RFC 5480 §2.1.1.1) — the only key type whose
/// method-1 SKI fallback we compute.
const OID_EC_PUBLIC_KEY: &[u64] = &[1, 2, 840, 10_045, 2, 1];

/// The recipient-identity facts of one X.509 certificate: the raw DER the
/// CMS `RecipientIdentifier` forms are matched against.
///
/// `issuer` is the DER content of the TBSCertificate `issuer Name` TLV and
/// `serial` the content of its `serialNumber` INTEGER — `issuerAndSerialNumber`
/// recipients are matched by byte equality of both (canonical DER, the same
/// rule as PDFBox `CertId` and OpenSSL `X509_NAME_cmp`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CertIdentity<'a> {
    /// DER content of the `issuer` `Name` (the `RDNSequence` body).
    pub issuer: &'a [u8],
    /// DER content of the `serialNumber` INTEGER.
    pub serial: &'a [u8],
    /// The `subjectKeyIdentifier` extension value, when the certificate
    /// carries one (RFC 5280 §4.2.1.2, the authoritative form).
    pub extension_ski: Option<&'a [u8]>,
    /// The method-1 SKI derived from the subject public key (SHA-1 over the
    /// `subjectPublicKey` BIT STRING content octets, excluding the
    /// unused-bits count) — the fallback mainstream viewers apply when the
    /// extension is absent. Only computed for `id-ecPublicKey` keys.
    pub computed_ski: Option<[u8; 20]>,
}

/// Extract the recipient-identity fields of one DER certificate.
///
/// # Budget
///
/// The walk consumes `data` once, charging every TLV's exact wire size to
/// `g` ([`selis_sandbox::Resource::Bytes`]); there is no recursion and no
/// allocation beyond fixed-size outputs. Exhaustion fails the parse typed.
///
/// # Malformed Input
///
/// Certificates are untrusted bytes. Structural deviations — wrong outer
/// tags, a non-minimal or lying length, the high-tag-number form, a
/// `Validity` holding fewer than two time TLVs, an empty BIT STRING,
/// unexpected trailing TBSCertificate fields — are a typed
/// [`selis_error::Code::EncryptMalformed`] error, never a panic. Fields the
/// CMS match does not use (signature algorithm, validity dates, subject
/// name) are consumed structurally but never interpreted. The signature is
/// **not verified**: an unverifiable signature still yields its identity
/// fields, exactly as the recipient-matching of Acrobat/PDFium/qpdf/PDFBox
/// behaves (the recipient key, not the signature, is what opens the
/// document).
pub fn parse_identity<'a>(data: &'a [u8], g: &mut BudgetGuard<'_>) -> Result<CertIdentity<'a>> {
    let mut outer = Der::new(data);
    let cert = outer.next_expect(Tag::SEQUENCE, g)?;
    if !outer.is_empty() {
        return Err(der::malformed(
            outer.offset(),
            "trailing bytes after Certificate",
        ));
    }

    let mut c = Der::new(cert.content);
    let tbs = c.next_expect(Tag::SEQUENCE, g)?;

    let mut t = Der::new(tbs.content);
    // version [0] EXPLICIT INTEGER DEFAULT v1 — when present, one INTEGER
    // inside a constructed [0].
    if t.rest().first() == Some(&Tag::CTX_0_CONSTRUCTED.0) {
        let version = t.next_expect(Tag::CTX_0_CONSTRUCTED, g)?;
        let mut inner = Der::new(version.content);
        let number = inner.next_expect(Tag::INTEGER, g)?;
        if number.content.first().is_some_and(|b| b & 0x80 != 0) {
            return Err(der::malformed(t.offset(), "negative certificate version"));
        }
        if !inner.is_empty() {
            return Err(der::malformed(inner.offset(), "trailing bytes in version"));
        }
    }
    let serial = t.next_expect(Tag::INTEGER, g)?;
    if serial.content.is_empty() {
        return Err(der::malformed(t.offset(), "empty serialNumber"));
    }
    // signature AlgorithmIdentifier (consumed structurally, never read).
    let _sig_alg = t.next_expect(Tag::SEQUENCE, g)?;
    let issuer = t.next_expect(Tag::SEQUENCE, g)?;
    skip_validity(&mut t, g)?;
    let _subject = t.next_expect(Tag::SEQUENCE, g)?;
    let (algorithm_oid, public_key_bits) = parse_subject_public_key_info(&mut t, g)?;

    // issuerUniqueID [1] IMPLICIT / subjectUniqueID [2] IMPLICIT (v2/v3
    // leftovers): both are primitive-tagged, skip wholesale if present.
    if t.rest().first() == Some(&0x81) {
        t.next(g)?;
    }
    if t.rest().first() == Some(&0x82) {
        t.next(g)?;
    }

    // extensions [3] EXPLICIT EXTENSIONS OPTIONAL.
    let mut extension_ski = None;
    while !t.is_empty() {
        let at = t.offset();
        let field = t.next(g)?;
        if field.tag != Tag::CTX_3_CONSTRUCTED {
            return Err(der::malformed(
                at,
                "unexpected trailing TBSCertificate field",
            ));
        }
        extension_ski = scan_extensions(field.content, g)?;
        if !t.is_empty() {
            return Err(der::malformed(
                t.offset(),
                "trailing bytes in TBSCertificate",
            ));
        }
    }

    let mut ident = CertIdentity {
        issuer: issuer.content,
        serial: serial.content,
        extension_ski,
        computed_ski: None,
    };
    // The method-1 SKI fallback only for EC keys (the RSA SKI of real
    // writers always comes from the extension).
    if algorithm_oid.as_slice() == OID_EC_PUBLIC_KEY {
        ident.computed_ski = Some(sha1_pubkey(public_key_bits));
    }
    Ok(ident)
}

/// `Validity ::= SEQUENCE { notBefore Time, notAfter Time }` (RFC 5280 §4.1.2.5).
fn skip_validity(t: &mut Der<'_>, g: &mut BudgetGuard<'_>) -> Result<()> {
    let at = t.offset();
    let validity = t.next_expect(Tag::SEQUENCE, g)?;
    let mut v = Der::new(validity.content);
    v.next(g)?; // notBefore: UTCTime or GeneralizedTime — either is skipped
    v.next(g)?; // notAfter
    if !v.is_empty() {
        return Err(der::malformed(at, "trailing bytes in Validity"));
    }
    Ok(())
}

/// `SubjectPublicKeyInfo ::= SEQUENCE { algorithm, subjectPublicKey BIT STRING }`.
fn parse_subject_public_key_info<'a>(
    t: &mut Der<'a>,
    g: &mut BudgetGuard<'_>,
) -> Result<(Vec<u64>, &'a [u8])> {
    let spki = t.next_expect(Tag::SEQUENCE, g)?;
    let mut s = Der::new(spki.content);
    let algorithm = s.next_expect(Tag::SEQUENCE, g)?;
    let mut a = Der::new(algorithm.content);
    let oid = a.next_expect(Tag::OID, g)?;
    let arcs = der::oid_arcs(oid.content).ok_or_else(|| der::malformed(0, "bad OID"))?;
    // Parameters (curve OID / NULL / an explicit choice): consume one TLV
    // if present, then the SPKI must be exhausted at the top.
    if !a.is_empty() {
        a.next(g)?;
        if !a.is_empty() {
            return Err(der::malformed(
                a.offset(),
                "trailing bytes in AlgorithmIdentifier",
            ));
        }
    }
    let bits = s.next_expect(Tag::BIT_STRING, g)?;
    if !s.is_empty() {
        return Err(der::malformed(
            s.offset(),
            "trailing bytes in SubjectPublicKeyInfo",
        ));
    }
    // BIT STRING: one unused-bits octet, then the key. A non-zero tail of
    // unused bits is malformed for key material.
    let unused = *bits
        .content
        .first()
        .ok_or_else(|| der::malformed(s.offset(), "empty subjectPublicKey BIT STRING"))?;
    if unused != 0 {
        return Err(der::malformed(
            s.offset(),
            "unused bits in subjectPublicKey",
        ));
    }
    let key = bits.content.get(1..).unwrap_or(&[]);
    Ok((arcs, key))
}

/// Scan the `extensions [3]` arm for the `subjectKeyIdentifier` value,
/// returning the inner OCTET STRING's content (the identifier bytes).
fn scan_extensions<'a>(data: &'a [u8], g: &mut BudgetGuard<'_>) -> Result<Option<&'a [u8]>> {
    // EXPLICIT wrapper: one SEQUENCE holding the Extension list.
    let mut top = Der::new(data);
    let list = top.next_expect(Tag::SEQUENCE, g)?;
    if !top.is_empty() {
        return Err(der::malformed(top.offset(), "trailing bytes in extensions"));
    }
    let mut found = None;
    let mut exts = Der::new(list.content);
    while !exts.is_empty() {
        let ext = exts.next_expect(Tag::SEQUENCE, g)?;
        let mut e = Der::new(ext.content);
        let oid = e.next_expect(Tag::OID, g)?;
        let arcs = der::oid_arcs(oid.content).ok_or_else(|| der::malformed(0, "bad OID"))?;
        let is_target = arcs.as_slice() == EXT_SUBJECT_KEY_IDENTIFIER;
        // critical BOOLEAN DEFAULT FALSE is optional.
        if e.rest().first() == Some(&Tag::BOOLEAN.0) {
            e.next(g)?;
        }
        let value = e.next_expect(Tag::OCTET_STRING, g)?;
        if !e.is_empty() {
            return Err(der::malformed(e.offset(), "trailing bytes in Extension"));
        }
        if is_target {
            // extnValue: the DER encoding of SubjectKeyIdentifier ::= OCTET STRING.
            let mut inner = Der::new(value.content);
            let ski = inner.next_expect(Tag::OCTET_STRING, g)?;
            if !inner.is_empty() {
                return Err(der::malformed(
                    inner.offset(),
                    "trailing bytes in SubjectKeyIdentifier",
                ));
            }
            if found.is_some() {
                return Err(der::malformed(0, "duplicate subjectKeyIdentifier"));
            }
            found = Some(ski.content);
        }
    }
    Ok(found)
}

/// RFC 5280 method-1 SKI: SHA-1 over the subject public key octets.
fn sha1_pubkey(public_key: &[u8]) -> [u8; 20] {
    let digest = sha1::Sha1::digest(public_key);
    let mut out = [0u8; 20];
    out.copy_from_slice(digest.as_slice());
    out
}

#[cfg(test)]
mod tests;
