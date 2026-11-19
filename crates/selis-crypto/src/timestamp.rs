//! Minimal RFC 3161 timestamp-token structure reader (SL-5.SIGN.04).
//!
//! The engine never fetches a token; a caller supplies DER bytes. This module
//! parses just enough of the RFC 3161 `TimeStampToken` (a CMS `SignedData`
//! carrying `id-ct-TSTInfo`) to **honestly** report what was timestamped:
//!
//! * the `messageImprint.hashedMessage` (the digest the TSA attested to),
//! * `genTime`, `serialNumber` and the timestamp policy OID.
//!
//! It deliberately does **not** verify the TSA's signature or its trust chain:
//! that needs an anchor the caller supplies and is reported as unverified. A
//! token that does not parse is an error, so an embedder can never claim a
//! token is valid merely because it is opaque bytes.
//!
//! # Budget / Malformed Input
//!
//! The existing [`crate::der`] reader charges every TLV to the guard and
//! rejects indefinite lengths, long lengths over four octets, high tag numbers
//! and trailing bytes. A malformed token is a typed [`Code::SignatureInvalid`]
//! error, never a panic.

use selis_error::{err, Code, Result};
use selis_sandbox::BudgetGuard;

use crate::der::{self, Der, Tag};

/// `id-signedData` — 1.2.840.113549.1.7.2.
const OID_SIGNED_DATA: &[u64] = &[1, 2, 840, 113_549, 1, 7, 2];
/// `id-ct-TSTInfo` — 1.2.840.113549.1.9.16.1.4 (RFC 3161 §2.4.2).
const OID_TST_INFO: &[u64] = &[1, 2, 840, 113_549, 1, 9, 16, 1, 4];
/// `id-sha256` — 2.16.840.1.101.3.4.2.1, the imprint digest this build sends.
pub const OID_SHA256: &[u64] = &[2, 16, 840, 1, 101, 3, 4, 2, 1];

/// The parts of a parsed RFC 3161 token this build relies on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TimestampToken {
    /// `messageImprint.hashedMessage`: the bytes the TSA hashed. For this
    /// build that is the SHA-256 of the signature's `/Contents` CMS blob.
    pub imprint: Vec<u8>,
    /// `genTime` as an ASCII GeneralizedTime string (`YYYYMMDDHHMMSSZ` form).
    pub gen_time: String,
    /// `serialNumber` as its DER integer content (big-endian, may be signed).
    pub serial: Vec<u8>,
    /// The timestamp policy OID's DER content.
    pub policy: Vec<u8>,
}

/// Parse an RFC 3161 `TimeStampToken` from DER.
///
/// # Errors
///
/// A typed `SignatureInvalid` error if any required field is missing, a tag is
/// wrong, an OID does not identify `signedData`/`TSTInfo`, a length is invalid,
/// or trailing bytes remain in any wrapper.
pub fn parse_rfc3161_token(der_bytes: &[u8], g: &mut BudgetGuard<'_>) -> Result<TimestampToken> {
    let mut outer = Der::new(der_bytes);
    let info = outer.next_expect(Tag::SEQUENCE, g)?;
    if !outer.is_empty() {
        return Err(invalid("trailing bytes after timestamp ContentInfo"));
    }
    let mut c = Der::new(info.content);
    let content_type = c.next_expect(Tag::OID, g)?;
    if der::oid_arcs(content_type.content).as_deref() != Some(OID_SIGNED_DATA) {
        return Err(invalid("timestamp token content type is not signedData"));
    }
    let explicit = c.next_expect(Tag::CTX_0_CONSTRUCTED, g)?;
    if !c.is_empty() {
        return Err(invalid("trailing bytes in timestamp ContentInfo"));
    }

    let mut wrapper = Der::new(explicit.content);
    let signed = wrapper.next_expect(Tag::SEQUENCE, g)?;
    if !wrapper.is_empty() {
        return Err(invalid("trailing bytes in timestamp SignedData wrapper"));
    }
    let mut s = Der::new(signed.content);
    let _version = s.next_expect(Tag::INTEGER, g)?;
    let _digest_algorithms = s.next_expect(Tag::SET, g)?;
    let encap = s.next_expect(Tag::SEQUENCE, g)?;

    let mut e = Der::new(encap.content);
    let econtent_type = e.next_expect(Tag::OID, g)?;
    if der::oid_arcs(econtent_type.content).as_deref() != Some(OID_TST_INFO) {
        return Err(invalid("timestamp SignedData does not carry id-ct-TSTInfo"));
    }
    let econtent = e.next_expect(Tag::CTX_0_CONSTRUCTED, g)?;
    let octets = Der::new(econtent.content).next_expect(Tag::OCTET_STRING, g)?;

    let mut t = Der::new(octets.content);
    let tst = t.next_expect(Tag::SEQUENCE, g)?;
    if !t.is_empty() {
        return Err(invalid("trailing bytes after TSTInfo"));
    }
    let mut tst_info = Der::new(tst.content);
    let _version = tst_info.next_expect(Tag::INTEGER, g)?;
    let policy = tst_info.next_expect(Tag::OID, g)?;
    let imprint_seq = tst_info.next_expect(Tag::SEQUENCE, g)?;
    let mut imprint_fields = Der::new(imprint_seq.content);
    let hash_algorithm = imprint_fields.next_expect(Tag::SEQUENCE, g)?;
    let mut alg = Der::new(hash_algorithm.content);
    let alg_oid = alg.next_expect(Tag::OID, g)?;
    if der::oid_arcs(alg_oid.content).as_deref() != Some(OID_SHA256) {
        return Err(invalid("timestamp messageImprint is not SHA-256"));
    }
    let imprint = imprint_fields.next_expect(Tag::OCTET_STRING, g)?;
    let serial = tst_info.next_expect(Tag::INTEGER, g)?;
    let gen_time = tst_info.next_expect(Tag::GENERALIZED_TIME, g)?;
    let time_text = core::str::from_utf8(gen_time.content)
        .map_err(|_| invalid("TSTInfo.genTime is not ASCII"))?;

    Ok(TimestampToken {
        imprint: imprint.content.to_vec(),
        gen_time: time_text.to_string(),
        serial: serial.content.to_vec(),
        policy: policy.content.to_vec(),
    })
}

/// The trust verdict for an RFC 3161 timestamp token's own certificate.
///
/// SL-5.SIGN.04's honest gap was that tokens were structurally parsed and their
/// `messageImprint` checked, but the TSA's own signature and chain were not.
/// This closes that gap: the token is a CMS `signedData`, so its encapsulated
/// `TSTInfo`, embedded signing certificate and `SignerInfo` are verified with
/// the same CMS verifier a document signature uses, and the signer's chain is
/// walked to the caller-supplied TSA anchors. Nothing here contacts a network.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TsaTrustReport {
    /// The TSA signing certificate's subject common name.
    pub signer_subject: String,
    /// The token's CMS signature over `TSTInfo` verified with its own cert.
    pub signature_ok: bool,
    /// The signing certificate chained to one of the caller's TSA anchors.
    pub chain_trusted: bool,
    /// The chain walk, when a signer certificate was found and parsed.
    pub chain: Option<crate::cert::ChainReport>,
    /// Why the verdict was not fully trusted, when it was not.
    pub reason: Option<String>,
    /// Always `false`: no OCSP/CRL is consulted for the TSA certificate.
    pub revocation_checked: bool,
}

/// Verify a timestamp token's own CMS signature and its signer's trust chain.
///
/// `tsa_anchors` are DER certificates the caller trusts as time authorities;
/// an empty slice means "no TSA is trusted", which is reported rather than
/// assumed. `now` is Unix seconds supplied by the caller (this crate has no
/// clock).
///
/// # Errors
///
/// A typed `SignatureInvalid` error if the token is not a readable CMS
/// `signedData`, carries no signer or no encapsulated `TSTInfo`, or the signer
/// names an unknown digest. An untrusted chain or a bad signature is *not* an
/// error — it is a report the caller must show.
pub fn verify_tsa_token(
    token_der: &[u8],
    tsa_anchors: &[&[u8]],
    now: i64,
    g: &mut BudgetGuard<'_>,
) -> Result<TsaTrustReport> {
    let signed = crate::cms::parse_signed_data(token_der, g)?;
    let signer = signed
        .signers
        .first()
        .ok_or_else(|| invalid("timestamp token carries no signer"))?;
    let tst_info = signed
        .encapsulated
        .ok_or_else(|| invalid("timestamp token has no encapsulated TSTInfo"))?;
    let digest = signer
        .digest
        .ok_or_else(|| invalid("timestamp signer names an unknown digest"))?;
    let content_digest = digest.digest(tst_info);

    // The CMS verifier checks the `messageDigest` signed attribute against the
    // TSTInfo bytes and the signature over the signed attributes with the
    // embedded signing certificate's key.
    let signature_ok =
        crate::cms::verify_signer(signer, &signed.certificates, &content_digest).is_ok();

    let signer_cert = crate::cms::signer_certificates(signer, &signed.certificates)
        .into_iter()
        .next();
    let mut chain = None;
    let mut signer_subject = String::new();
    let mut chain_trusted = false;
    let mut reason = None;

    if let Some(leaf) = signer_cert {
        let intermediates: Vec<&[u8]> = signed
            .certificates
            .iter()
            .copied()
            .filter(|candidate| *candidate != leaf)
            .collect();
        match crate::cert::validate_chain(leaf, &intermediates, tsa_anchors, now, g) {
            Ok(report) => {
                signer_subject = report
                    .links
                    .first()
                    .map(|link| link.subject.clone())
                    .unwrap_or_default();
                chain_trusted = report.trusted;
                if !report.trusted {
                    reason = report.reason.clone();
                }
                chain = Some(report);
            }
            Err(e) => reason = Some(format!("the TSA certificate chain is unreadable: {e}")),
        }
    } else {
        reason = Some("the timestamp token carries no usable signing certificate".to_string());
    }
    if !signature_ok && reason.is_none() {
        reason = Some("the timestamp token's own CMS signature did not verify".to_string());
    }

    Ok(TsaTrustReport {
        signer_subject,
        signature_ok,
        chain_trusted,
        chain,
        reason,
        revocation_checked: false,
    })
}

fn invalid(detail: &'static str) -> selis_error::Error {
    err!(
        Code::SignatureInvalid,
        during = "timestamp-token",
        detail = detail
    )
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::{parse_rfc3161_token, OID_SHA256, OID_SIGNED_DATA, OID_TST_INFO};
    use crate::der_write::{
        explicit, integer, octet_string, oid, sequence, set, tlv, GENERALIZED_TIME,
    };
    use selis_sandbox::{Budget, Surface};

    fn token(imprint: &[u8], time: &[u8]) -> Vec<u8> {
        let alg = sequence(&[oid(OID_SHA256)]);
        let mi = sequence(&[alg, octet_string(imprint)]);
        let tst = sequence(&[
            integer(1),
            oid(&[1, 3, 6, 1, 4, 1, 99_999, 1]),
            mi,
            integer(42),
            tlv(GENERALIZED_TIME, time),
        ]);
        let encap = sequence(&[oid(OID_TST_INFO), explicit(0, &octet_string(&tst))]);
        let signed = sequence(&[integer(3), set(&[]), encap]);
        sequence(&[oid(OID_SIGNED_DATA), explicit(0, &signed)])
    }

    #[test]
    fn a_well_formed_token_parses_its_imprint_and_time() {
        let budget = Budget::profile(Surface::Fuzz);
        let mut g = budget.guard();
        let der = token(&[0xAA; 32], b"20261104120000Z");
        let parsed = parse_rfc3161_token(&der, &mut g).expect("a valid token");
        assert_eq!(parsed.imprint, vec![0xAA; 32]);
        assert_eq!(parsed.gen_time, "20261104120000Z");
        assert_eq!(parsed.serial, vec![42]);
    }

    #[test]
    fn a_truncated_or_wrong_type_token_is_refused() {
        let budget = Budget::profile(Surface::Fuzz);
        let mut g = budget.guard();
        assert!(parse_rfc3161_token(&[0x30, 0x00], &mut g).is_err());
        let mut wrong = token(&[1; 32], b"20261104120000Z");
        // Byte 12 is the last content byte of the outer contentType OID
        // (1.2.840.113549.1.7.2). Flipping it makes it a different OID.
        if let Some(byte) = wrong.get_mut(12) {
            *byte ^= 0x01;
        }
        assert!(parse_rfc3161_token(&wrong, &mut g).is_err());
    }

    // ---- TSA certificate + signature verification ------------------------

    use crate::der_write as w;
    use crate::identity::{generate_local_identity, IdentityParams};

    /// Build a complete, correctly signed RFC 3161 token around `imprint`,
    /// carrying `cert_der` as the TSA's signing certificate.
    fn signed_token(cert_der: &[u8], key: &rsa::RsaPrivateKey, imprint: &[u8]) -> Vec<u8> {
        use rsa::pkcs1v15::SigningKey;
        use rsa::signature::{SignatureEncoding as _, Signer as _};
        use sha2::{Digest, Sha256};

        let tst = w::sequence(&[
            w::integer(1),
            w::oid(&[1, 3, 6, 1, 4, 1, 99_999, 1]),
            w::sequence(&[w::sequence(&[w::oid(OID_SHA256)]), w::octet_string(imprint)]),
            w::integer(42),
            w::tlv(w::GENERALIZED_TIME, b"20261104120000Z"),
        ]);
        let digest = Sha256::digest(&tst);
        let digest_alg = w::sequence(&[w::oid(OID_SHA256), w::null()]);
        let mut attrs = vec![
            w::sequence(&[
                w::oid(&[1, 2, 840, 113549, 1, 9, 4]),
                w::set(&[w::octet_string(&digest)]),
            ]),
            w::sequence(&[
                w::oid(&[1, 2, 840, 113549, 1, 9, 5]),
                w::set(&[w::signing_time("20261104120000Z").unwrap()]),
            ]),
        ];
        attrs.sort();
        let signed_bytes = w::tlv(w::SET, &w::concat(&attrs));
        let signature = SigningKey::<Sha256>::new(key.clone())
            .sign(&signed_bytes)
            .to_vec();
        let sig_alg = w::sequence(&[w::oid(&[1, 2, 840, 113549, 1, 1, 11]), w::null()]);
        let signer_info = w::sequence(&[
            w::integer(1),
            w::implicit(0, false, &[0x01, 0x02, 0x03]), // SKI (falls back to the one cert)
            digest_alg.clone(),
            w::implicit(0, true, &w::concat(&attrs)),
            sig_alg,
            w::octet_string(&signature),
        ]);
        let encap = w::sequence(&[w::oid(OID_TST_INFO), w::explicit(0, &w::octet_string(&tst))]);
        let signed = w::sequence(&[
            w::integer(3),
            w::set(&[digest_alg]),
            encap,
            w::implicit(0, true, cert_der),
            w::set(&[signer_info]),
        ]);
        w::sequence(&[w::oid(OID_SIGNED_DATA), w::explicit(0, &signed)])
    }

    fn tsa_identity(cn: &str) -> crate::identity::SelfSignedIdentity {
        generate_local_identity(&IdentityParams {
            common_name: cn,
            organization: None,
            key_bits: 2048,
            validity_days: 365,
            not_before_unix: 1_793_000_000,
            serial: [7; 16],
            is_ca: false,
        })
        .expect("a TSA identity")
    }

    #[test]
    fn tsa_signature_and_chain_verify_against_the_supplied_anchor() {
        let identity = tsa_identity("Selis Test TSA");
        let key = rsa::pkcs8::DecodePrivateKey::from_pkcs8_der(&identity.key_der)
            .expect("the generated key parses");
        let der = signed_token(&identity.cert_der, &key, &[0xAA; 32]);
        let budget = Budget::profile(Surface::Fuzz);
        let mut g = budget.guard();
        let report = crate::timestamp::verify_tsa_token(
            &der,
            &[identity.cert_der.as_slice()],
            1_793_793_600,
            &mut g,
        )
        .expect("verifies");
        assert!(report.signature_ok, "the token's own signature verifies");
        assert!(report.chain_trusted, "chains to the supplied TSA anchor");
        assert!(report.signer_subject.contains("Selis Test TSA"));
        assert!(!report.revocation_checked);
    }

    #[test]
    fn tsa_chain_is_untrusted_with_no_or_a_wrong_anchor() {
        let identity = tsa_identity("Selis Test TSA");
        let other = tsa_identity("Another TSA");
        let key = rsa::pkcs8::DecodePrivateKey::from_pkcs8_der(&identity.key_der)
            .expect("the generated key parses");
        let der = signed_token(&identity.cert_der, &key, &[0xAA; 32]);
        let budget = Budget::profile(Surface::Fuzz);
        let mut g = budget.guard();
        let none =
            crate::timestamp::verify_tsa_token(&der, &[], 1_793_793_600, &mut g).expect("reports");
        assert!(none.signature_ok, "the signature still verifies");
        assert!(!none.chain_trusted, "no anchor means no trust");
        let wrong = crate::timestamp::verify_tsa_token(
            &der,
            &[other.cert_der.as_slice()],
            1_793_793_600,
            &mut g,
        )
        .expect("reports");
        assert!(!wrong.chain_trusted, "a different CA cannot issue the TSA");
    }

    #[test]
    fn a_token_with_a_forged_tst_info_fails_its_signature() {
        let identity = tsa_identity("Selis Test TSA");
        let key = rsa::pkcs8::DecodePrivateKey::from_pkcs8_der(&identity.key_der)
            .expect("the generated key parses");
        let der = signed_token(&identity.cert_der, &key, &[0xAA; 32]);
        // Corrupt one byte of the encapsulated imprint; the messageDigest
        // signed attribute no longer matches, so the signature cannot verify.
        let mut forged = der.clone();
        let idx = forged.len().saturating_sub(90);
        if let Some(byte) = forged.get_mut(idx) {
            *byte ^= 0x01;
        }
        let budget = Budget::profile(Surface::Fuzz);
        let mut g = budget.guard();
        let report = crate::timestamp::verify_tsa_token(
            &forged,
            &[identity.cert_der.as_slice()],
            1_793_793_600,
            &mut g,
        )
        .expect("still parses as a CMS");
        assert!(
            !report.signature_ok,
            "a forged TSTInfo breaks the signature"
        );
    }
}
