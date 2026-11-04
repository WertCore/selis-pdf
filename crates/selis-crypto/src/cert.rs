//! X.509 certificates for signing: parse, verify, and chain to a trust anchor.
//!
//! [`crate::x509`] deliberately reads only the four identity fields a CMS
//! recipient needs and never makes a trust decision. A signature is the
//! opposite problem: the whole point of a certificate there is the claim that
//! some issuer vouches for a public key, so this module parses enough of
//! RFC 5280 to check that claim — names, validity, key usage, basic
//! constraints and the signature itself — and refuses to call anything valid
//! without a chain that ends at a caller-supplied anchor.
//!
//! The trust store is a caller argument, not a built-in: the browser has no
//! OS trust store to borrow, and a hard-coded root list would be a silent
//! claim about the world. The desktop app and the CLI pass their own anchors;
//! the tools page passes whatever the user selected. An empty anchor list
//! means "nothing is trusted", which is reported as such rather than assumed
//! to be fine.
//!
//! No revocation is checked. OCSP and CRL both need a network or a
//! caller-supplied CRL, and a signature tool that cannot reach one must say
//! "revocation not checked" rather than imply it was. That is what
//! [`ChainReport::revocation_checked`] is for.
//!
//! The DER reader in [`crate::der`] is shared with the CMS code; every TLV is
//! charged to the budget, so hostile certificate bytes are bounded by their
//! own wire size.

use rsa::pkcs8::DecodePublicKey as _;
use selis_error::{err, Code, Result};
use selis_sandbox::{BudgetGuard, Resource};
use sha2::{Digest, Sha256, Sha384, Sha512};

use crate::der::{self, Der, Tag};

/// `id-at-commonName` (RFC 5280 §4.1.2.4).
const OID_COMMON_NAME: &[u64] = &[2, 5, 4, 3];
/// `id-ce-basicConstraints` (RFC 5280 §4.2.1.9).
const OID_BASIC_CONSTRAINTS: &[u64] = &[2, 5, 29, 19];
/// `id-ce-keyUsage` (RFC 5280 §4.2.1.3).
const OID_KEY_USAGE: &[u64] = &[2, 5, 29, 15];
/// `id-ce-subjectKeyIdentifier` (RFC 5280 §4.2.1.2).
const OID_SUBJECT_KEY_ID: &[u64] = &[2, 5, 29, 14];
/// `id-ce-authorityKeyIdentifier` (RFC 5280 §4.2.1.1).
const OID_AUTHORITY_KEY_ID: &[u64] = &[2, 5, 29, 35];
/// `id-ecPublicKey` (RFC 5480 §2.1.1.1).
const OID_EC_PUBLIC_KEY: &[u64] = &[1, 2, 840, 10_045, 2, 1];
/// `prime256v1` / `secp256r1` (RFC 5480 §2.1.1.1).
const OID_CURVE_P256: &[u64] = &[1, 2, 840, 10_045, 3, 1, 7];

/// `sha1WithRSAEncryption` (RFC 8017 A.2.4).
const OID_SHA1_RSA: &[u64] = &[1, 2, 840, 113_549, 1, 1, 5];
/// `sha256WithRSAEncryption`.
const OID_SHA256_RSA: &[u64] = &[1, 2, 840, 113_549, 1, 1, 11];
/// `sha384WithRSAEncryption`.
const OID_SHA384_RSA: &[u64] = &[1, 2, 840, 113_549, 1, 1, 12];
/// `sha512WithRSAEncryption`.
const OID_SHA512_RSA: &[u64] = &[1, 2, 840, 113_549, 1, 1, 13];
/// `ecdsa-with-SHA1` (RFC 5758 §3.2).
const OID_ECDSA_SHA1: &[u64] = &[1, 2, 840, 10_045, 4, 1];
/// `ecdsa-with-SHA256`.
const OID_ECDSA_SHA256: &[u64] = &[1, 2, 840, 10_045, 4, 3, 2];
/// `ecdsa-with-SHA384`.
const OID_ECDSA_SHA384: &[u64] = &[1, 2, 840, 10_045, 4, 3, 3];
/// `ecdsa-with-SHA512`.
const OID_ECDSA_SHA512: &[u64] = &[1, 2, 840, 10_045, 4, 3, 4];
/// `rsaEncryption` (RFC 8017 A.2.2).
const OID_RSA_ENCRYPTION: &[u64] = &[1, 2, 840, 113_549, 1, 1, 1];

/// A digest this build can compute and check.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DigestAlg {
    /// SHA-1. Accepted for legacy certificates, never used to sign.
    Sha1,
    /// SHA-256.
    Sha256,
    /// SHA-384.
    Sha384,
    /// SHA-512.
    Sha512,
}

impl DigestAlg {
    /// The digest OID's arc list.
    #[must_use]
    pub fn oid(self) -> &'static [u64] {
        match self {
            Self::Sha1 => &[1, 3, 14, 3, 2, 26],
            Self::Sha256 => &[2, 16, 840, 1, 101, 3, 4, 2, 1],
            Self::Sha384 => &[2, 16, 840, 1, 101, 3, 4, 2, 2],
            Self::Sha512 => &[2, 16, 840, 1, 101, 3, 4, 2, 3],
        }
    }

    /// The wire name a report shows.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Sha1 => "SHA-1",
            Self::Sha256 => "SHA-256",
            Self::Sha384 => "SHA-384",
            Self::Sha512 => "SHA-512",
        }
    }

    /// The digest output length in bytes.
    #[must_use]
    pub fn output_len(self) -> usize {
        match self {
            Self::Sha1 => 20,
            Self::Sha256 => 32,
            Self::Sha384 => 48,
            Self::Sha512 => 64,
        }
    }

    /// Decode a digest OID's arcs.
    #[must_use]
    pub fn from_arcs(arcs: &[u64]) -> Option<Self> {
        [Self::Sha1, Self::Sha256, Self::Sha384, Self::Sha512]
            .into_iter()
            .find(|alg| alg.oid() == arcs)
    }

    /// Digest `data`.
    #[must_use]
    pub fn digest(self, data: &[u8]) -> Vec<u8> {
        match self {
            Self::Sha1 => sha1::Sha1::digest(data).to_vec(),
            Self::Sha256 => Sha256::digest(data).to_vec(),
            Self::Sha384 => Sha384::digest(data).to_vec(),
            Self::Sha512 => Sha512::digest(data).to_vec(),
        }
    }
}

/// The key type a signature algorithm names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scheme {
    /// RSASSA-PKCS1-v1_5 (RFC 8017 §8.2).
    RsaPkcs1,
    /// ECDSA over NIST P-256 (RFC 6979 / SEC1).
    EcdsaP256,
}

/// A signature algorithm, as a scheme plus a digest.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SignatureAlgorithm {
    /// The key type.
    pub scheme: Scheme,
    /// The digest the scheme hashes with.
    pub digest: DigestAlg,
}

impl SignatureAlgorithm {
    /// Decode a `SignatureAlgorithmIdentifier` OID. `default_digest` supplies
    /// the digest for `rsaEncryption`, which CMS signer infos use when the
    /// digest is carried in a separate field.
    #[must_use]
    pub fn from_oid(arcs: &[u64], default_digest: Option<DigestAlg>) -> Option<Self> {
        let rsa = |digest| {
            Some(Self {
                scheme: Scheme::RsaPkcs1,
                digest,
            })
        };
        let ecdsa = |digest| {
            Some(Self {
                scheme: Scheme::EcdsaP256,
                digest,
            })
        };
        match arcs {
            a if a == OID_SHA1_RSA => rsa(DigestAlg::Sha1),
            a if a == OID_SHA256_RSA => rsa(DigestAlg::Sha256),
            a if a == OID_SHA384_RSA => rsa(DigestAlg::Sha384),
            a if a == OID_SHA512_RSA => rsa(DigestAlg::Sha512),
            a if a == OID_ECDSA_SHA1 => ecdsa(DigestAlg::Sha1),
            a if a == OID_ECDSA_SHA256 => ecdsa(DigestAlg::Sha256),
            a if a == OID_ECDSA_SHA384 => ecdsa(DigestAlg::Sha384),
            a if a == OID_ECDSA_SHA512 => ecdsa(DigestAlg::Sha512),
            a if a == OID_RSA_ENCRYPTION => default_digest.map(|digest| Self {
                scheme: Scheme::RsaPkcs1,
                digest,
            }),
            _ => None,
        }
    }

    /// The algorithm OID arcs.
    #[must_use]
    pub fn oid(self) -> &'static [u64] {
        match (self.scheme, self.digest) {
            (Scheme::RsaPkcs1, DigestAlg::Sha1) => OID_SHA1_RSA,
            (Scheme::RsaPkcs1, DigestAlg::Sha256) => OID_SHA256_RSA,
            (Scheme::RsaPkcs1, DigestAlg::Sha384) => OID_SHA384_RSA,
            (Scheme::RsaPkcs1, DigestAlg::Sha512) => OID_SHA512_RSA,
            (Scheme::EcdsaP256, DigestAlg::Sha1) => OID_ECDSA_SHA1,
            (Scheme::EcdsaP256, DigestAlg::Sha256) => OID_ECDSA_SHA256,
            (Scheme::EcdsaP256, DigestAlg::Sha384) => OID_ECDSA_SHA384,
            (Scheme::EcdsaP256, DigestAlg::Sha512) => OID_ECDSA_SHA512,
        }
    }

    /// The name a report shows (`sha256WithRSAEncryption`, `ecdsa-with-SHA256`).
    #[must_use]
    pub fn name(self) -> &'static str {
        match self.scheme {
            Scheme::RsaPkcs1 => match self.digest {
                DigestAlg::Sha1 => "sha1WithRSAEncryption",
                DigestAlg::Sha256 => "sha256WithRSAEncryption",
                DigestAlg::Sha384 => "sha384WithRSAEncryption",
                DigestAlg::Sha512 => "sha512WithRSAEncryption",
            },
            Scheme::EcdsaP256 => match self.digest {
                DigestAlg::Sha1 => "ecdsa-with-SHA1",
                DigestAlg::Sha256 => "ecdsa-with-SHA256",
                DigestAlg::Sha384 => "ecdsa-with-SHA384",
                DigestAlg::Sha512 => "ecdsa-with-SHA512",
            },
        }
    }
}

/// The subject public key of a certificate, kept as the wire bytes the
/// verifier's own parser reads rather than as decoded RSA integers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PublicKey<'a> {
    /// An RSA key: the whole `SubjectPublicKeyInfo` DER.
    Rsa(&'a [u8]),
    /// A NIST P-256 key: the uncompressed SEC1 point from the BIT STRING.
    P256(&'a [u8]),
}

/// One parsed certificate, borrowing the DER it came from.
#[derive(Debug, Clone)]
pub struct Certificate<'a> {
    /// The whole `Certificate` DER, the value a chain compares for identity.
    pub raw: &'a [u8],
    /// The `TBSCertificate` TLV: the bytes the issuer's signature covers.
    pub tbs: &'a [u8],
    /// Version: 1, 2 or 3 (default v1 when the field is absent).
    pub version: u8,
    /// The serial number's INTEGER content.
    pub serial: &'a [u8],
    /// The full `issuer` Name TLV (compared byte-for-byte).
    pub issuer: &'a [u8],
    /// The full `subject` Name TLV.
    pub subject: &'a [u8],
    /// `notBefore`, in Unix seconds.
    pub not_before: i64,
    /// `notAfter`, in Unix seconds.
    pub not_after: i64,
    /// The subject public key.
    pub key: PublicKey<'a>,
    /// The algorithm the issuer signed this certificate with.
    pub signature_algorithm: Option<SignatureAlgorithm>,
    /// The signature BIT STRING content, without the unused-bits octet.
    pub signature: &'a [u8],
    /// `basicConstraints.cA`, when present.
    pub is_ca: Option<bool>,
    /// `basicConstraints.pathLenConstraint`, when present.
    pub path_len: Option<u32>,
    /// `keyUsage` bits, when present (bit 0 = digitalSignature, bit 5 =
    /// keyCertSign, RFC 5280 §4.2.1.3).
    pub key_usage: Option<u16>,
    /// `subjectKeyIdentifier`, when present.
    pub subject_key_id: Option<&'a [u8]>,
    /// `authorityKeyIdentifier`, when present.
    pub authority_key_id: Option<&'a [u8]>,
}

impl<'a> Certificate<'a> {
    /// Whether `now` falls inside the validity window.
    #[must_use]
    pub fn valid_at(&self, now: i64) -> bool {
        now >= self.not_before && now <= self.not_after
    }

    /// Whether the certificate asserts `keyUsage.digitalSignature`.
    #[must_use]
    pub fn allows_digital_signature(&self) -> bool {
        self.key_usage.is_none_or(|bits| bits & 0x8000 != 0)
    }

    /// Whether the certificate asserts `keyUsage.keyCertSign`.
    #[must_use]
    pub fn allows_certificate_signing(&self) -> bool {
        self.key_usage.is_none_or(|bits| bits & 0x0400 != 0)
    }

    /// The subject's common name, decoded for display.
    #[must_use]
    pub fn subject_common_name(&self) -> String {
        common_name(self.subject).unwrap_or_else(|| "unnamed certificate".to_string())
    }

    /// The issuer's common name, decoded for display.
    #[must_use]
    pub fn issuer_common_name(&self) -> String {
        common_name(self.issuer).unwrap_or_else(|| "unnamed issuer".to_string())
    }
}

/// Parse one DER certificate.
///
/// # Budget
///
/// Every TLV charges its wire size; a hostile nesting of zero-length fields
/// still pays for its headers, so the parse is bounded by the input.
///
/// # Malformed Input
///
/// Wrong outer tags, trailing bytes, empty required fields, a bad time string
/// and an unsupported key type are typed errors — never a panic.
pub fn parse<'a>(data: &'a [u8], g: &mut BudgetGuard<'_>) -> Result<Certificate<'a>> {
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
    let sig_alg = c.next_expect(Tag::SEQUENCE, g)?;
    let signature = c.next_expect(Tag::BIT_STRING, g)?;
    if !c.is_empty() {
        return Err(der::malformed(c.offset(), "trailing bytes in Certificate"));
    }

    let mut t = Der::new(tbs.content);
    let mut version = 1u8;
    if t.rest().first() == Some(&Tag::CTX_0_CONSTRUCTED.0) {
        let outer_version = t.next(g)?;
        let mut v = Der::new(outer_version.content);
        let number = v.next_expect(Tag::INTEGER, g)?;
        let raw = number.content.first().copied().unwrap_or(0);
        version = raw.checked_add(1).unwrap_or(0);
        if !v.is_empty() {
            return Err(der::malformed(v.offset(), "trailing bytes in version"));
        }
    }
    let serial = t.next_expect(Tag::INTEGER, g)?;
    let _tbs_sig_alg = t.next_expect(Tag::SEQUENCE, g)?;
    let issuer = t.next_expect(Tag::SEQUENCE, g)?;
    let validity = t.next_expect(Tag::SEQUENCE, g)?;
    let subject = t.next_expect(Tag::SEQUENCE, g)?;
    let spki = t.next_expect(Tag::SEQUENCE, g)?;
    let key = parse_spki(spki, g)?;

    // issuerUniqueID [1] / subjectUniqueID [2] IMPLICIT, v2/v3 only.
    for tag in [0x81u8, 0x82u8] {
        if t.rest().first() == Some(&tag) {
            t.next(g)?;
        }
    }

    let mut cert_out = Certificate {
        raw: data,
        tbs: tbs.full,
        version,
        serial: serial.content,
        issuer: issuer.content,
        subject: subject.content,
        not_before: 0,
        not_after: 0,
        key,
        signature_algorithm: algorithm_from_sequence(sig_alg, None, g)?,
        signature: bit_string_bytes(signature)?,
        is_ca: None,
        path_len: None,
        key_usage: None,
        subject_key_id: None,
        authority_key_id: None,
    };
    let (not_before, not_after) = parse_validity(validity, g)?;
    cert_out.not_before = not_before;
    cert_out.not_after = not_after;

    if t.rest().first() == Some(&Tag::CTX_3_CONSTRUCTED.0) {
        let extensions = t.next(g)?;
        scan_extensions(extensions.content, &mut cert_out, g)?;
    }
    if !t.is_empty() {
        return Err(der::malformed(
            t.offset(),
            "trailing bytes in TBSCertificate",
        ));
    }
    Ok(cert_out)
}

/// Rebuild the full `TBSCertificate` TLV — handled by [`der::Tlv::full`] on
/// the reader side; kept out of this module so there is one encoding authority.
fn bit_string_bytes(tlv: der::Tlv<'_>) -> Result<&[u8]> {
    let unused = *tlv
        .content
        .first()
        .ok_or_else(|| der::malformed(0, "empty signature BIT STRING"))?;
    if unused != 0 {
        return Err(der::malformed(0, "unused bits in signature"));
    }
    tlv.content
        .get(1..)
        .ok_or_else(|| der::malformed(0, "empty signature"))
}

fn parse_spki<'a>(spki: der::Tlv<'a>, g: &mut BudgetGuard<'_>) -> Result<PublicKey<'a>> {
    let mut s = Der::new(spki.content);
    let algorithm = s.next_expect(Tag::SEQUENCE, g)?;
    let key_bits = s.next_expect(Tag::BIT_STRING, g)?;
    if !s.is_empty() {
        return Err(der::malformed(
            s.offset(),
            "trailing bytes in SubjectPublicKeyInfo",
        ));
    }
    let mut a = Der::new(algorithm.content);
    let oid = a.next_expect(Tag::OID, g)?;
    let arcs = der::oid_arcs(oid.content).ok_or_else(|| der::malformed(0, "bad key OID"))?;
    let mut params: Option<Vec<u64>> = None;
    if !a.is_empty() {
        let param = a.next(g)?;
        if param.tag == Tag::OID {
            params = der::oid_arcs(param.content);
        }
    }
    let unused = key_bits.content.first().copied().unwrap_or(0);
    if unused != 0 {
        return Err(der::malformed(0, "unused bits in subjectPublicKey"));
    }
    let key = key_bits.content.get(1..).unwrap_or(&[]);
    if arcs == OID_RSA_ENCRYPTION {
        return Ok(PublicKey::Rsa(spki.full));
    }
    if arcs == OID_EC_PUBLIC_KEY && params.as_deref() == Some(OID_CURVE_P256) {
        return Ok(PublicKey::P256(key));
    }
    Err(err!(
        Code::SignatureUnsupported,
        during = "certificate",
        detail = "certificate key type is not RSA or P-256"
    ))
}

pub(crate) fn algorithm_from_sequence(
    alg: der::Tlv<'_>,
    default_digest: Option<DigestAlg>,
    g: &mut BudgetGuard<'_>,
) -> Result<Option<SignatureAlgorithm>> {
    let mut a = Der::new(alg.content);
    let oid = a.next_expect(Tag::OID, g)?;
    let arcs = der::oid_arcs(oid.content).ok_or_else(|| der::malformed(0, "bad signature OID"))?;
    Ok(SignatureAlgorithm::from_oid(&arcs, default_digest))
}

fn parse_validity(validity: der::Tlv<'_>, g: &mut BudgetGuard<'_>) -> Result<(i64, i64)> {
    let mut v = Der::new(validity.content);
    let not_before = v.next(g)?;
    let not_after = v.next(g)?;
    if !v.is_empty() {
        return Err(der::malformed(v.offset(), "trailing bytes in Validity"));
    }
    let from = parse_time(not_before)?;
    let until = parse_time(not_after)?;
    Ok((from, until))
}

fn scan_extensions<'a>(
    data: &'a [u8],
    out: &mut Certificate<'a>,
    g: &mut BudgetGuard<'_>,
) -> Result<()> {
    let mut top = Der::new(data);
    let list = top.next_expect(Tag::SEQUENCE, g)?;
    if !top.is_empty() {
        return Err(der::malformed(top.offset(), "trailing bytes in extensions"));
    }
    let mut exts = Der::new(list.content);
    while !exts.is_empty() {
        let ext = exts.next_expect(Tag::SEQUENCE, g)?;
        let mut e = Der::new(ext.content);
        let oid = e.next_expect(Tag::OID, g)?;
        let arcs = der::oid_arcs(oid.content).ok_or_else(|| der::malformed(0, "bad OID"))?;
        if e.rest().first() == Some(&Tag::BOOLEAN.0) {
            e.next(g)?;
        }
        let value = e.next_expect(Tag::OCTET_STRING, g)?;
        if !e.is_empty() {
            return Err(der::malformed(e.offset(), "trailing bytes in Extension"));
        }
        if arcs == OID_BASIC_CONSTRAINTS {
            let mut inner = Der::new(value.content);
            let bc = inner.next_expect(Tag::SEQUENCE, g)?;
            if !inner.is_empty() {
                return Err(der::malformed(
                    inner.offset(),
                    "trailing bytes in BasicConstraints",
                ));
            }
            let mut b = Der::new(bc.content);
            let mut is_ca = false;
            if b.rest().first() == Some(&Tag::BOOLEAN.0) {
                let flag = b.next(g)?;
                is_ca = flag.content.first().is_some_and(|v| *v != 0);
            }
            out.is_ca = Some(is_ca);
            if !b.is_empty() {
                let path = b.next_expect(Tag::INTEGER, g)?;
                out.path_len = decode_u32(path.content);
            }
        } else if arcs == OID_KEY_USAGE {
            let mut inner = Der::new(value.content);
            let bits = inner.next_expect(Tag::BIT_STRING, g)?;
            out.key_usage = Some(decode_key_usage(bits.content));
        } else if arcs == OID_SUBJECT_KEY_ID {
            let mut inner = Der::new(value.content);
            let ski = inner.next_expect(Tag::OCTET_STRING, g)?;
            out.subject_key_id = Some(ski.content);
        } else if arcs == OID_AUTHORITY_KEY_ID {
            out.authority_key_id = authority_key_id(value.content, g)?;
        }
    }
    Ok(())
}

fn authority_key_id<'a>(data: &'a [u8], g: &mut BudgetGuard<'_>) -> Result<Option<&'a [u8]>> {
    let mut d = Der::new(data);
    let seq = d.next_expect(Tag::SEQUENCE, g)?;
    if !d.is_empty() {
        return Err(der::malformed(
            d.offset(),
            "trailing bytes in AuthorityKeyIdentifier",
        ));
    }
    let mut s = Der::new(seq.content);
    if s.rest().first() == Some(&0x80) {
        let key_id = s.next(g)?;
        return Ok(Some(key_id.content));
    }
    Ok(None)
}

fn decode_key_usage(content: &[u8]) -> u16 {
    // The first octet counts unused trailing bits; the bits follow MSB-first,
    // so bit 0 (digitalSignature) is 0x8000 in this 16-bit view.
    let bits = content.get(1..).unwrap_or(&[]);
    let high = u16::from(bits.first().copied().unwrap_or(0)) << 8;
    let low = u16::from(bits.get(1).copied().unwrap_or(0));
    high | low
}

fn decode_u32(content: &[u8]) -> Option<u32> {
    let mut value = 0u32;
    for byte in content.iter().take(4) {
        value = value.checked_shl(8)?.checked_add(u32::from(*byte))?;
    }
    Some(value)
}

/// Parse one DER time value into Unix seconds.
pub(crate) fn parse_time(tlv: der::Tlv<'_>) -> Result<i64> {
    let text = std::str::from_utf8(tlv.content).map_err(|_| der::malformed(0, "non-ASCII time"))?;
    match tlv.tag {
        Tag(0x17) => {
            // YYMMDDHHMMSSZ — the two-digit year is 1950..=2049.
            if text.len() != 13 || !text.ends_with('Z') {
                return Err(der::malformed(0, "bad UTCTime"));
            }
            let yy: i32 = text
                .get(0..2)
                .and_then(|s| s.parse().ok())
                .ok_or_else(|| der::malformed(0, "bad UTCTime year"))?;
            let year = if yy >= 50 { 1900 + yy } else { 2000 + yy };
            civil_to_unix(year, text.get(2..).unwrap_or(""))
        }
        Tag(0x18) => {
            // YYYYMMDDHHMMSSZ
            if text.len() != 15 || !text.ends_with('Z') {
                return Err(der::malformed(0, "bad GeneralizedTime"));
            }
            let year: i32 = text
                .get(0..4)
                .and_then(|s| s.parse().ok())
                .ok_or_else(|| der::malformed(0, "bad GeneralizedTime year"))?;
            civil_to_unix(year, text.get(4..).unwrap_or(""))
        }
        _ => Err(der::malformed(0, "unexpected time tag")),
    }
}

fn civil_to_unix(year: i32, rest: &str) -> Result<i64> {
    let digits = rest.strip_suffix('Z').unwrap_or(rest);
    let month: i64 = digits
        .get(0..2)
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| der::malformed(0, "bad month"))?;
    let day: i64 = digits
        .get(2..4)
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| der::malformed(0, "bad day"))?;
    let hour: i64 = digits
        .get(4..6)
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| der::malformed(0, "bad hour"))?;
    let minute: i64 = digits
        .get(6..8)
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| der::malformed(0, "bad minute"))?;
    let second: i64 = digits
        .get(8..10)
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| der::malformed(0, "bad second"))?;
    if !(1..=12).contains(&month)
        || !(1..=31).contains(&day)
        || hour > 23
        || minute > 59
        || second > 60
    {
        return Err(der::malformed(0, "time out of range"));
    }
    let days = days_from_civil(i64::from(year), month, day);
    Ok(days * 86_400 + hour * 3_600 + minute * 60 + second)
}

/// Howard Hinnant's `days_from_civil`: days since 1970-01-01.
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (month + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

fn common_name(name_content: &[u8]) -> Option<String> {
    // `name_content` is the `RDNSequence` body (a `Name`'s content), so the
    // read starts *inside* the SEQUENCE the caller already unwrapped.
    let budget = selis_sandbox::Budget::profile(selis_sandbox::Surface::Fuzz);
    let mut g = budget.guard();
    let mut rdns = Der::new(name_content);
    while !rdns.is_empty() {
        let rdn = rdns.next_expect(Tag::SET, &mut g).ok()?;
        let mut set = Der::new(rdn.content);
        while !set.is_empty() {
            let atv = set.next_expect(Tag::SEQUENCE, &mut g).ok()?;
            let mut a = Der::new(atv.content);
            let oid = a.next_expect(Tag::OID, &mut g).ok()?;
            if der::oid_arcs(oid.content).as_deref() != Some(OID_COMMON_NAME) {
                continue;
            }
            let value = a.next(&mut g).ok()?;
            return decode_directory_string(value.tag, value.content);
        }
    }
    None
}

fn decode_directory_string(tag: Tag, content: &[u8]) -> Option<String> {
    match tag.0 {
        // UTF8String, PrintableString, IA5String, T61String.
        0x0C | 0x13 | 0x16 | 0x14 => Some(String::from_utf8_lossy(content).into_owned()),
        // BMPString: UCS-2 big-endian.
        0x1E => {
            let units: Vec<u16> = content
                .chunks_exact(2)
                .map(|pair| u16::from_be_bytes([pair[0], pair[1]]))
                .collect();
            Some(String::from_utf16_lossy(&units))
        }
        _ => None,
    }
}

/// Verify `signature` over `message` with the certificate's own key.
///
/// # Errors
///
/// A wrong signature is [`Code::SignatureInvalid`]; a scheme this build does
/// not implement is [`Code::SignatureUnsupported`] rather than a silent pass.
pub fn verify_with_key(
    key: PublicKey<'_>,
    algorithm: SignatureAlgorithm,
    message: &[u8],
    signature: &[u8],
) -> Result<()> {
    match (key, algorithm.scheme) {
        (PublicKey::Rsa(spki), Scheme::RsaPkcs1) => {
            let key = rsa::RsaPublicKey::from_public_key_der(spki).map_err(|_| {
                err!(
                    Code::SignatureInvalid,
                    during = "certificate",
                    detail = "certificate RSA key did not decode"
                )
            })?;
            let verified = match algorithm.digest {
                DigestAlg::Sha1 => verify_rsa_digest::<sha1::Sha1>(&key, message, signature),
                DigestAlg::Sha256 => verify_rsa_digest::<Sha256>(&key, message, signature),
                DigestAlg::Sha384 => verify_rsa_digest::<Sha384>(&key, message, signature),
                DigestAlg::Sha512 => verify_rsa_digest::<Sha512>(&key, message, signature),
            };
            if verified {
                Ok(())
            } else {
                Err(invalid("signature"))
            }
        }
        (PublicKey::P256(point), Scheme::EcdsaP256) => {
            use p256::ecdsa::signature::Verifier as _;
            let key = p256::ecdsa::VerifyingKey::from_sec1_bytes(point).map_err(|_| {
                err!(
                    Code::SignatureInvalid,
                    during = "certificate",
                    detail = "certificate P-256 key did not decode"
                )
            })?;
            let sig = p256::ecdsa::Signature::from_der(signature)
                .map_err(|_| invalid("ECDSA signature encoding"))?;
            match algorithm.digest {
                DigestAlg::Sha256 => key.verify(message, &sig).map_err(|_| invalid("signature")),
                DigestAlg::Sha384 | DigestAlg::Sha512 | DigestAlg::Sha1 => Err(err!(
                    Code::SignatureUnsupported,
                    during = "certificate",
                    detail = "ECDSA certificate digest is not P-256 SHA-256"
                )),
            }
        }
        _ => Err(err!(
            Code::SignatureUnsupported,
            during = "certificate",
            detail = "signature scheme does not match the key type"
        )),
    }
}

fn verify_rsa_digest<D>(key: &rsa::RsaPublicKey, message: &[u8], signature: &[u8]) -> bool
where
    D: Digest + sha2::digest::const_oid::AssociatedOid + rsa::signature::digest::FixedOutputReset,
{
    use rsa::signature::Verifier as _;
    let Ok(sig) = rsa::pkcs1v15::Signature::try_from(signature) else {
        return false;
    };
    let verifying = rsa::pkcs1v15::VerifyingKey::<D>::new(key.clone());
    verifying.verify(message, &sig).is_ok()
}

fn invalid(what: &'static str) -> selis_error::Error {
    err!(
        Code::SignatureInvalid,
        during = "certificate",
        detail = what
    )
}

/// One link of a chain, as a report shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChainLink {
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
    /// Whether the certificate was inside its validity window at `now`.
    pub time_ok: bool,
    /// Whether this certificate is one of the caller's anchors.
    pub anchor: bool,
}

/// The outcome of walking a chain.
#[derive(Debug, Clone)]
pub struct ChainReport {
    /// Leaf first, anchor last.
    pub links: Vec<ChainLink>,
    /// Every link verified and the walk ended on an anchor.
    pub trusted: bool,
    /// A short reason when `trusted` is false.
    pub reason: Option<String>,
    /// Always false: no OCSP or CRL is consulted (see the module docs).
    pub revocation_checked: bool,
    /// The leaf's certificate serial, for the report.
    pub leaf_serial: Vec<u8>,
}

/// Walk from `leaf` toward one of `anchors`, verifying every signature.
///
/// # Rules
///
/// * A certificate is trusted only when the walk ends on an anchor the caller
///   supplied — never because it is self-signed.
/// * Every link's signature must verify, and every certificate must be inside
///   its validity window at `now`.
/// * An issuing certificate must assert `basicConstraints.cA` and, when the
///   extension is present, `keyUsage.keyCertSign`.
/// * A `pathLenConstraint` limits how many intermediates may follow it.
/// * Iteration is bounded, so a hostile loop terminates.
///
/// # Errors
///
/// Malformed DER is a typed error. An unverified or unreachable chain is a
/// *report* with `trusted = false`, because "I could not verify this" is a
/// result a user must see, not an operation that failed.
pub fn validate_chain(
    leaf: &[u8],
    intermediates: &[&[u8]],
    anchors: &[&[u8]],
    now: i64,
    g: &mut BudgetGuard<'_>,
) -> Result<ChainReport> {
    let leaf_cert = parse(leaf, g)?;
    let leaf_serial = leaf_cert.serial.to_vec();
    let mut pool: Vec<Certificate<'_>> = Vec::with_capacity(intermediates.len());
    for der in intermediates {
        pool.push(parse(der, g)?);
    }
    let mut anchor_certs: Vec<Certificate<'_>> = Vec::with_capacity(anchors.len());
    for der in anchors {
        anchor_certs.push(parse(der, g)?);
    }

    let mut links: Vec<ChainLink> = Vec::new();
    let mut current = leaf_cert;
    let mut reason: Option<String> = None;
    let mut trusted = false;

    for depth in 0..=8usize {
        let is_anchor = anchor_certs.iter().any(|a| a.raw == current.raw);
        let time_ok = current.valid_at(now);
        let mut link = ChainLink {
            subject: current.subject_common_name(),
            issuer: current.issuer_common_name(),
            not_before: current.not_before,
            not_after: current.not_after,
            signature_ok: true,
            time_ok,
            anchor: is_anchor,
        };
        if !time_ok {
            reason = Some(format!("{} is not valid at the signing time", link.subject));
            links.push(link);
            break;
        }
        if is_anchor {
            links.push(link);
            trusted = true;
            break;
        }
        // Find an issuer by name, then prove it by signature.
        let mut found: Option<(Certificate<'_>, bool)> = None;
        for candidate in pool.iter().chain(anchor_certs.iter()) {
            let name_matches = candidate.subject == current.issuer;
            let is_anchor_candidate = anchor_certs.iter().any(|a| a.raw == candidate.raw);
            if !name_matches {
                continue;
            }
            if !is_anchor_candidate
                && (candidate.is_ca != Some(true) || !candidate.allows_certificate_signing())
            {
                continue;
            }
            let Some(algorithm) = current.signature_algorithm else {
                continue;
            };
            if verify_with_key(candidate.key, algorithm, current.tbs, current.signature).is_ok() {
                found = Some((candidate.clone(), is_anchor_candidate));
                break;
            }
        }
        let Some((issuer, issuer_is_anchor)) = found else {
            if current.subject == current.issuer {
                reason = Some(format!(
                    "{} is self-signed and is not one of the supplied anchors",
                    link.subject
                ));
            } else {
                reason = Some(format!("no supplied certificate issued {}", link.subject));
            }
            link.signature_ok = false;
            links.push(link);
            break;
        };
        if let Some(limit) = issuer.path_len {
            let intermediates_below = links.len();
            if intermediates_below > usize::try_from(limit).unwrap_or(usize::MAX) {
                reason = Some(format!(
                    "{} exceeded its path length",
                    issuer.subject_common_name()
                ));
                links.push(link);
                break;
            }
        }
        links.push(link);
        current = issuer;
        let _ = depth;
        if issuer_is_anchor {
            continue; // the next iteration records the anchor link and stops.
        }
    }
    if !trusted && reason.is_none() {
        reason = Some("the chain is longer than eight certificates".to_string());
    }
    Ok(ChainReport {
        links,
        trusted,
        reason,
        revocation_checked: false,
        leaf_serial,
    })
}

/// The key type a PEM private key names, read without decoding the key.
///
/// The tools page accepts any PEM the user selects, and the signing chunk
/// signs RSA only. Identifying an EC key by name — rather than falling through
/// to "this is not an RSA key" — lets the UI say why it cannot be used and
/// what to export instead.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrivateKeyKind {
    /// A `RSAPrivateKey` (PKCS#1) or a PKCS#8 key with `rsaEncryption`.
    Rsa,
    /// An `ECPrivateKey` (SEC1) or a PKCS#8 key with `id-ecPublicKey`.
    Ec,
    /// Neither shape could be recognised.
    Unknown,
}

/// Classify a PEM private key by its label, or by the algorithm OID of a
/// PKCS#8 `PrivateKeyInfo`.
#[must_use]
pub fn private_key_kind(pem: &str) -> PrivateKeyKind {
    if pem.contains("BEGIN RSA PRIVATE KEY") {
        return PrivateKeyKind::Rsa;
    }
    if pem.contains("BEGIN EC PRIVATE KEY") {
        return PrivateKeyKind::Ec;
    }
    let Some(der) = pem_body(pem, "PRIVATE KEY") else {
        return PrivateKeyKind::Unknown;
    };
    pkcs8_kind(&der).unwrap_or(PrivateKeyKind::Unknown)
}

fn pkcs8_kind(der: &[u8]) -> Option<PrivateKeyKind> {
    let budget = selis_sandbox::Budget::profile(selis_sandbox::Surface::Fuzz);
    let mut g = budget.guard();
    let mut outer = Der::new(der);
    let sequence = outer.next_expect(Tag::SEQUENCE, &mut g).ok()?;
    if !outer.is_empty() {
        return None;
    }
    let mut info = Der::new(sequence.content);
    let _version = info.next_expect(Tag::INTEGER, &mut g).ok()?;
    let algorithm = info.next_expect(Tag::SEQUENCE, &mut g).ok()?;
    let mut a = Der::new(algorithm.content);
    let oid = a.next_expect(Tag::OID, &mut g).ok()?;
    let arcs = der::oid_arcs(oid.content)?;
    if arcs == OID_RSA_ENCRYPTION {
        Some(PrivateKeyKind::Rsa)
    } else if arcs == OID_EC_PUBLIC_KEY {
        Some(PrivateKeyKind::Ec)
    } else {
        Some(PrivateKeyKind::Unknown)
    }
}

/// Decode the bytes of a PEM block body (base64, whitespace ignored).
#[must_use]
pub fn pem_body(text: &str, label: &str) -> Option<Vec<u8>> {
    let begin = format!("-----BEGIN {label}-----");
    let end = format!("-----END {label}-----");
    let start = text.find(&begin)?.saturating_add(begin.len());
    let stop = text.get(start..)?.find(&end)?.saturating_add(start);
    let body = text.get(start..stop)?;
    base64_decode(body)
}

/// Minimal base64 decoder (standard alphabet, `=` padding, whitespace ignored).
///
/// A dependency would be a whole crate for forty lines; the input is a file
/// the user chose, and every failure is `None` rather than a panic.
#[must_use]
pub fn base64_decode(text: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(text.len() / 4 * 3);
    let mut acc = 0u32;
    let mut bits = 0u32;
    let mut padding = 0usize;
    for byte in text.bytes() {
        let value = match byte {
            b'A'..=b'Z' => u32::from(byte - b'A'),
            b'a'..=b'z' => u32::from(byte - b'a') + 26,
            b'0'..=b'9' => u32::from(byte - b'0') + 52,
            b'+' => 62,
            b'/' => 63,
            b'=' => {
                padding = padding.saturating_add(1);
                continue;
            }
            b' ' | b'\t' | b'\r' | b'\n' => continue,
            _ => return None,
        };
        if padding > 0 {
            return None; // data after padding
        }
        acc = (acc << 6) | value;
        bits = bits.saturating_add(6);
        if bits >= 8 {
            bits = bits.saturating_sub(8);
            out.push(u8::try_from((acc >> bits) & 0xFF).unwrap_or(0));
        }
    }
    if bits >= 6 || padding > 2 {
        return None;
    }
    Some(out)
}

/// Charge `bytes` of PEM or DER to a guard, so a huge caller file is refused
/// by the same budget as the parse.
pub fn charge(g: &mut BudgetGuard<'_>, bytes: usize) -> Result<()> {
    g.charge(Resource::Bytes, u64::try_from(bytes).unwrap_or(u64::MAX))
}
