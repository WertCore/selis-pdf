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
}
