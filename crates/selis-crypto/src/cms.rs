//! CMS `SignedData`: the structure a PDF signature carries, written and
//! checked (RFC 5652 §5, ISO 32000-2 §12.8.3.4).
//!
//! [`crate::pkcs7`] reads the *other* CMS shape a PDF uses — an
//! `EnvelopedData` recipient blob for public-key encryption. A signature is a
//! `SignedData` with the content detached: the signer hashes the document's
//! byte ranges, puts that digest in a `messageDigest` signed attribute, and
//! signs the DER of the attributes. This module builds exactly that shape
//! (`adbe.pkcs7.detached`, sub-filter `ETSI.CAdES.detached` compatible for the
//! signing side) and parses any conforming one back for verification.
//!
//! The writer signs RSA keys only. ECDSA signing needs an entropy source for
//! the nonce in every mode this tree would ship, and the tools chunk is
//! deliberately import-free; verification of ECDSA *signatures* is supported
//! through [`crate::cert::verify_with_key`], so a document signed elsewhere
//! still reads. An EC private key is refused by name rather than downgraded.
//!
//! The DER encoder is [`crate::der_write`], deliberately separate from the
//! reader in [`crate::der`]: the verifier checks what the writer produced, and
//! two independent encoders cannot share a bug.

use rsa::pkcs1v15::SigningKey;
use rsa::signature::{SignatureEncoding, Signer};
use selis_error::{err, Code, Result};
use selis_sandbox::BudgetGuard;
use sha2::Digest as _;

use crate::cert::{self, DigestAlg, SignatureAlgorithm};
use crate::der::{self, Der, Tag};
use crate::der_write as w;

/// `signedData` (RFC 5652 §5.1).
const OID_SIGNED_DATA: &[u64] = &[1, 2, 840, 113_549, 1, 7, 2];
/// `data` (RFC 5652 §4).
const OID_DATA: &[u64] = &[1, 2, 840, 113_549, 1, 7, 1];
/// `contentType` signed attribute (RFC 5652 §11.1).
const OID_ATTR_CONTENT_TYPE: &[u64] = &[1, 2, 840, 113_549, 1, 9, 3];
/// `messageDigest` signed attribute (RFC 5652 §11.1).
const OID_ATTR_MESSAGE_DIGEST: &[u64] = &[1, 2, 840, 113_549, 1, 9, 4];
/// `signingTime` signed attribute (RFC 5652 §11.3).
const OID_ATTR_SIGNING_TIME: &[u64] = &[1, 2, 840, 113_549, 1, 9, 5];

/// The signed attributes of one signer, as the verifier needs them.
#[derive(Debug, Clone)]
pub struct SignedAttributes {
    /// The `contentType` attribute's OID arcs, when present.
    pub content_type: Option<Vec<u64>>,
    /// The `messageDigest` attribute's octets, when present.
    pub message_digest: Option<Vec<u8>>,
    /// The `signingTime` string as encoded (`YYMMDDHHMMSSZ` or
    /// `YYYYMMDDHHMMSSZ`), when present.
    pub signing_time: Option<String>,
    /// The attributes re-encoded as a DER `SET OF` — the bytes a signature
    /// covers (RFC 5652 §5.4: the `[0] IMPLICIT` tag is replaced by `SET OF`
    /// for signing and verification).
    pub signed_bytes: Vec<u8>,
}

/// One `SignerInfo`, borrowing the CMS it came from.
#[derive(Debug, Clone)]
pub struct SignerInfo<'a> {
    /// `issuerAndSerialNumber.issuer` content, when that identifier form is
    /// used.
    pub issuer: Option<&'a [u8]>,
    /// `issuerAndSerialNumber.serialNumber` content.
    pub serial: Option<&'a [u8]>,
    /// `subjectKeyIdentifier` (`[0]` identifier form).
    pub subject_key_id: Option<&'a [u8]>,
    /// The digest algorithm the signer declares.
    pub digest: Option<DigestAlg>,
    /// The signed attributes, when the signature is over attributes rather
    /// than over the content directly.
    pub signed_attrs: Option<SignedAttributes>,
    /// The signature algorithm.
    pub signature_algorithm: Option<SignatureAlgorithm>,
    /// The signature value.
    pub signature: &'a [u8],
}

/// A parsed `SignedData`.
#[derive(Debug, Clone)]
pub struct SignedData<'a> {
    /// The certificates the CMS carries, as their DER.
    pub certificates: Vec<&'a [u8]>,
    /// The signers (a PDF signature normally has exactly one).
    pub signers: Vec<SignerInfo<'a>>,
    /// The encapsulated content, when the signature is not detached.
    pub encapsulated: Option<&'a [u8]>,
}

/// Parse a CMS `ContentInfo` holding `SignedData`.
///
/// # Budget
///
/// Every TLV charges its wire size, so a hostile blob is bounded by its own
/// length.
///
/// # Malformed Input
///
/// Wrong tags, a non-`signedData` content type, a malformed signer list or
/// trailing bytes are typed [`Code::SignatureInvalid`] errors.
pub fn parse_signed_data<'a>(data: &'a [u8], g: &mut BudgetGuard<'_>) -> Result<SignedData<'a>> {
    let mut outer = Der::new(data);
    let info = outer.next_expect(Tag::SEQUENCE, g)?;
    if !outer.is_empty() {
        return Err(invalid("trailing bytes after ContentInfo"));
    }
    let mut c = Der::new(info.content);
    let oid = c.next_expect(Tag::OID, g)?;
    let arcs = der::oid_arcs(oid.content).ok_or_else(|| invalid("bad content OID"))?;
    if arcs != OID_SIGNED_DATA {
        return Err(invalid("CMS content type is not signedData"));
    }
    let explicit = c.next_expect(Tag::CTX_0_CONSTRUCTED, g)?;
    if !c.is_empty() {
        return Err(invalid("trailing bytes in ContentInfo"));
    }
    let mut e = Der::new(explicit.content);
    let signed = e.next_expect(Tag::SEQUENCE, g)?;
    if !e.is_empty() {
        return Err(invalid("trailing bytes in SignedData wrapper"));
    }

    let mut s = Der::new(signed.content);
    let _version = s.next_expect(Tag::INTEGER, g)?;
    let _digest_algorithms = s.next_expect(Tag::SET, g)?;
    let encap = s.next_expect(Tag::SEQUENCE, g)?;
    let encapsulated = parse_encapsulated(encap, g)?;

    let mut certificates: Vec<&'a [u8]> = Vec::new();
    if s.rest().first() == Some(&Tag::CTX_0_CONSTRUCTED.0) {
        let certs = s.next(g)?;
        let mut set = Der::new(certs.content);
        while !set.is_empty() {
            let cert = set.next(g)?;
            if cert.tag == Tag::SEQUENCE {
                certificates.push(cert.full);
            }
        }
    }
    if s.rest().first() == Some(&Tag::CTX_1_CONSTRUCTED.0) {
        // CRLs: consumed but never used. Revocation is reported as unchecked.
        let _crls = s.next(g)?;
    }

    let signer_set = s.next_expect(Tag::SET, g)?;
    if !s.is_empty() {
        return Err(invalid("trailing bytes in SignedData"));
    }
    let mut signers = Vec::new();
    let mut signer_infos = Der::new(signer_set.content);
    while !signer_infos.is_empty() {
        signers.push(parse_signer(
            signer_infos.next_expect(Tag::SEQUENCE, g)?,
            g,
        )?);
    }
    if signers.is_empty() {
        return Err(invalid("SignedData carries no signer"));
    }
    Ok(SignedData {
        certificates,
        signers,
        encapsulated,
    })
}

fn parse_encapsulated<'a>(tlv: der::Tlv<'a>, g: &mut BudgetGuard<'_>) -> Result<Option<&'a [u8]>> {
    let mut e = Der::new(tlv.content);
    let _content_type = e.next_expect(Tag::OID, g)?;
    let mut content = None;
    if e.rest().first() == Some(&Tag::CTX_0_CONSTRUCTED.0) {
        let explicit = e.next(g)?;
        let mut inner = Der::new(explicit.content);
        let octets = inner.next_expect(Tag::OCTET_STRING, g)?;
        content = Some(octets.content);
    }
    if !e.is_empty() {
        return Err(invalid("trailing bytes in EncapsulatedContentInfo"));
    }
    Ok(content)
}

fn parse_signer<'a>(tlv: der::Tlv<'a>, g: &mut BudgetGuard<'_>) -> Result<SignerInfo<'a>> {
    let mut s = Der::new(tlv.content);
    let _version = s.next_expect(Tag::INTEGER, g)?;
    let sid = s.next(g)?;
    let (issuer, serial, subject_key_id) = match sid.tag {
        Tag::SEQUENCE => {
            let mut id = Der::new(sid.content);
            let issuer = id.next_expect(Tag::SEQUENCE, g)?;
            let serial = id.next_expect(Tag::INTEGER, g)?;
            if !id.is_empty() {
                return Err(invalid("trailing bytes in issuerAndSerialNumber"));
            }
            (Some(issuer.content), Some(serial.content), None)
        }
        // subjectKeyIdentifier [0] IMPLICIT.
        tag if tag.0 == 0x80 => (None, None, Some(sid.content)),
        _ => return Err(invalid("unknown SignerIdentifier form")),
    };

    let digest_alg = s.next_expect(Tag::SEQUENCE, g)?;
    let digest = digest_from_algorithm(digest_alg, g)?;

    let signed_attrs = if s.rest().first() == Some(&Tag::CTX_0_CONSTRUCTED.0) {
        let attrs = s.next(g)?;
        Some(parse_attributes(attrs, g)?)
    } else {
        None
    };
    let sig_alg = s.next_expect(Tag::SEQUENCE, g)?;
    let signature_algorithm = cert::algorithm_from_sequence(sig_alg, digest, g)?;
    let signature = s.next_expect(Tag::OCTET_STRING, g)?;
    if s.rest().first() == Some(&Tag::CTX_1_CONSTRUCTED.0) {
        let _unsigned = s.next(g)?;
    }
    if !s.is_empty() {
        return Err(invalid("trailing bytes in SignerInfo"));
    }
    Ok(SignerInfo {
        issuer,
        serial,
        subject_key_id,
        digest,
        signed_attrs,
        signature_algorithm,
        signature: signature.content,
    })
}

fn digest_from_algorithm(tlv: der::Tlv<'_>, g: &mut BudgetGuard<'_>) -> Result<Option<DigestAlg>> {
    let mut a = Der::new(tlv.content);
    let oid = a.next_expect(Tag::OID, g)?;
    let arcs = der::oid_arcs(oid.content).ok_or_else(|| invalid("bad digest OID"))?;
    Ok(DigestAlg::from_arcs(&arcs))
}

fn parse_attributes(tlv: der::Tlv<'_>, g: &mut BudgetGuard<'_>) -> Result<SignedAttributes> {
    // The tag is `[0] IMPLICIT`; the signature covers a `SET OF` of the same
    // content (RFC 5652 §5.4), so re-tagging is not a re-encoding of values.
    let signed_bytes = w::tlv(w::SET, tlv.content);
    let mut attrs = Der::new(tlv.content);
    let mut out = SignedAttributes {
        content_type: None,
        message_digest: None,
        signing_time: None,
        signed_bytes,
    };
    while !attrs.is_empty() {
        let attr = attrs.next_expect(Tag::SEQUENCE, g)?;
        let mut a = Der::new(attr.content);
        let oid = a.next_expect(Tag::OID, g)?;
        let arcs = der::oid_arcs(oid.content).ok_or_else(|| invalid("bad attribute OID"))?;
        let values = a.next_expect(Tag::SET, g)?;
        if !a.is_empty() {
            return Err(invalid("trailing bytes in Attribute"));
        }
        if arcs == OID_ATTR_CONTENT_TYPE {
            let mut v = Der::new(values.content);
            let value = v.next_expect(Tag::OID, g)?;
            out.content_type = der::oid_arcs(value.content);
        } else if arcs == OID_ATTR_MESSAGE_DIGEST {
            let mut v = Der::new(values.content);
            let value = v.next_expect(Tag::OCTET_STRING, g)?;
            out.message_digest = Some(value.content.to_vec());
        } else if arcs == OID_ATTR_SIGNING_TIME {
            let mut v = Der::new(values.content);
            let value = v.next(g)?;
            out.signing_time = std::str::from_utf8(value.content).ok().map(str::to_string);
        }
    }
    Ok(out)
}

/// The first signer whose certificate the CMS carries, or the first signer.
#[must_use]
pub fn primary<'a>(data: &'a SignedData<'a>) -> Option<&'a SignerInfo<'a>> {
    data.signers.first()
}

/// The certificate a signer identifies, by issuer and serial or by subject
/// key identifier. Falls back to any certificate the CMS carries when the
/// identifier does not match (some writers use an empty issuer), which the
/// signature check then proves or rejects.
#[must_use]
pub fn signer_certificates<'a>(
    signer: &SignerInfo<'_>,
    certificates: &[&'a [u8]],
) -> Vec<&'a [u8]> {
    let mut preferred: Vec<&'a [u8]> = Vec::new();
    for cert in certificates {
        let budget = selis_sandbox::Budget::profile(selis_sandbox::Surface::Fuzz);
        let mut g = budget.guard();
        let Ok(parsed) = cert::parse(cert, &mut g) else {
            continue;
        };
        let matches_serial = signer.serial.is_some_and(|serial| parsed.serial == serial);
        let matches_issuer = signer
            .issuer
            .is_some_and(|issuer| parsed.issuer == issuer && matches_serial);
        let matches_ski = signer
            .subject_key_id
            .is_some_and(|ski| parsed.subject_key_id == Some(ski));
        if matches_issuer || matches_ski {
            preferred.push(*cert);
        }
    }
    if preferred.is_empty() {
        return certificates.to_vec();
    }
    preferred
}

/// Verify one signer against the digest the caller computed over the document
/// bytes the signature names.
///
/// The digest is compared with the `messageDigest` signed attribute, and the
/// signature is verified over the signed attributes (or over the content when
/// there are none). A mismatch is [`Code::SignatureInvalid`].
pub fn verify_signer(
    signer: &SignerInfo<'_>,
    certificates: &[&[u8]],
    document_digest: &[u8],
) -> Result<()> {
    let digest = signer.digest.ok_or_else(|| {
        err!(
            Code::SignatureUnsupported,
            during = "signature",
            detail = "unknown digest algorithm"
        )
    })?;
    if document_digest.len() != digest.output_len() {
        return Err(invalid(
            "signature digest length does not match the algorithm",
        ));
    }
    let mut verified = false;
    let mut last_error: Option<selis_error::Error> = None;
    for cert_der in signer_certificates(signer, certificates) {
        let budget = selis_sandbox::Budget::profile(selis_sandbox::Surface::Fuzz);
        let mut g = budget.guard();
        let Ok(certificate) = cert::parse(cert_der, &mut g) else {
            continue;
        };
        let Some(algorithm) = signer.signature_algorithm else {
            return Err(err!(
                Code::SignatureUnsupported,
                during = "signature",
                detail = "unknown signature algorithm"
            ));
        };
        let (message, expected) = match &signer.signed_attrs {
            Some(attrs) => {
                let Some(expected) = attrs.message_digest.as_ref() else {
                    return Err(invalid("signed attributes carry no messageDigest"));
                };
                (&attrs.signed_bytes[..], expected.as_slice())
            }
            None => (document_digest, document_digest),
        };
        if expected != document_digest {
            last_error = Some(invalid("the signed digest does not match the document"));
            continue;
        }
        match cert::verify_with_key(certificate.key, algorithm, message, signer.signature) {
            Ok(()) => {
                verified = true;
                break;
            }
            Err(error) => last_error = Some(error),
        }
    }
    if verified {
        return Ok(());
    }
    Err(last_error.unwrap_or_else(|| invalid("no certificate in the CMS verified the signature")))
}

/// Build a detached `SignedData` over `message_digest` with an RSA key.
///
/// `signing_time` is `YYYYMMDDHHMMSSZ`; the core has no clock, so the caller
/// asserts the moment (and a caller that cannot name one must refuse rather
/// than invent it).
///
/// # Errors
///
/// A key or certificate that does not parse, a signing time in the wrong
/// shape, and an RSA key too small to sign with the chosen digest (under 768
/// bits) are typed errors.
pub fn build_detached_signed_data(
    key: &rsa::RsaPrivateKey,
    certificate_chain: &[Vec<u8>],
    message_digest: &[u8],
    signing_time: &str,
    digest: DigestAlg,
) -> Result<Vec<u8>> {
    let leaf = certificate_chain
        .first()
        .ok_or_else(|| invalid("no signing certificate"))?;
    let budget = selis_sandbox::Budget::profile(selis_sandbox::Surface::Fuzz);
    let mut g = budget.guard();
    let leaf_cert = cert::parse(leaf, &mut g)?;
    if message_digest.len() != digest.output_len() {
        return Err(invalid(
            "message digest length does not match the algorithm",
        ));
    }
    let time = w::signing_time(signing_time)
        .ok_or_else(|| invalid("signing time is not YYYYMMDDHHMMSSZ"))?;

    let attributes = signed_attributes(message_digest, &time);
    let signed_bytes = w::tlv(w::SET, &w::concat(&attributes));
    let signature = sign_rsa(key, digest, &signed_bytes)?;

    let algorithm = SignatureAlgorithm {
        scheme: cert::Scheme::RsaPkcs1,
        digest,
    };
    let sig_alg = w::sequence(&[w::oid(algorithm.oid()), w::null()]);
    let issuer = w::tlv(w::SEQUENCE, leaf_cert.issuer);
    let serial = w::tlv(w::INTEGER, leaf_cert.serial);
    let signer_info = w::sequence(&[
        w::integer(1),
        w::sequence(&[issuer, serial]),
        w::sequence(&[w::oid(digest.oid()), w::null()]),
        w::implicit(0, true, &w::concat(&attributes)),
        sig_alg,
        w::octet_string(&signature),
    ]);

    let mut certificates = Vec::with_capacity(certificate_chain.len().saturating_mul(2));
    for cert in certificate_chain {
        certificates.extend_from_slice(cert);
    }
    let mut signed_data = vec![
        w::integer(1),
        w::set(&[w::sequence(&[w::oid(digest.oid()), w::null()])]),
        w::sequence(&[w::oid(OID_DATA)]),
        w::implicit(0, true, &certificates),
        w::set(&[signer_info]),
    ];
    signed_data.shrink_to_fit();
    let content_info = w::sequence(&[
        w::oid(OID_SIGNED_DATA),
        w::explicit(0, &w::sequence(&signed_data)),
    ]);
    Ok(content_info)
}

fn signed_attributes(message_digest: &[u8], time: &[u8]) -> Vec<Vec<u8>> {
    let content_type = w::sequence(&[w::oid(OID_ATTR_CONTENT_TYPE), w::set(&[w::oid(OID_DATA)])]);
    let digest_attr = w::sequence(&[
        w::oid(OID_ATTR_MESSAGE_DIGEST),
        w::set(&[w::octet_string(message_digest)]),
    ]);
    let time_attr = w::sequence(&[w::oid(OID_ATTR_SIGNING_TIME), w::set(&[time.to_vec()])]);
    // DER SET OF sorts its encodings octet-wise.
    let mut attrs = vec![content_type, digest_attr, time_attr];
    attrs.sort();
    attrs
}

fn sign_rsa(key: &rsa::RsaPrivateKey, digest: DigestAlg, message: &[u8]) -> Result<Vec<u8>> {
    macro_rules! sign_with {
        ($d:ty) => {{
            let signing = SigningKey::<$d>::new(key.clone());
            signing.sign(message).to_vec()
        }};
    }
    Ok(match digest {
        DigestAlg::Sha1 => sign_with!(sha1::Sha1),
        DigestAlg::Sha256 => sign_with!(sha2::Sha256),
        DigestAlg::Sha384 => sign_with!(sha2::Sha384),
        DigestAlg::Sha512 => sign_with!(sha2::Sha512),
    })
}

/// A detached signature's `messageDigest` is over the document's byte ranges;
/// this helper computes it in one place so the writer and the verifier cannot
/// disagree about what was hashed.
#[must_use]
pub fn digest_spans(digest: DigestAlg, spans: &[&[u8]]) -> Vec<u8> {
    match digest {
        DigestAlg::Sha1 => {
            let mut h = sha1::Sha1::new();
            for span in spans {
                h.update(span);
            }
            h.finalize().to_vec()
        }
        DigestAlg::Sha256 => {
            let mut h = sha2::Sha256::new();
            for span in spans {
                h.update(span);
            }
            h.finalize().to_vec()
        }
        DigestAlg::Sha384 => {
            let mut h = sha2::Sha384::new();
            for span in spans {
                h.update(span);
            }
            h.finalize().to_vec()
        }
        DigestAlg::Sha512 => {
            let mut h = sha2::Sha512::new();
            for span in spans {
                h.update(span);
            }
            h.finalize().to_vec()
        }
    }
}

fn invalid(detail: &'static str) -> selis_error::Error {
    err!(
        Code::SignatureInvalid,
        during = "signature",
        detail = detail
    )
}
