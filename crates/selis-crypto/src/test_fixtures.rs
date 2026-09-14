//! Deterministic DER builders shared by the unit tests of the CMS/X.509
//! readers (test-only; the shipped code is decrypt-only per ADR-P0019).

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects
)]

/// Encode a TLV with DER length octets.
pub(crate) fn tlv(tag: u8, content: &[u8]) -> Vec<u8> {
    let mut out = vec![tag];
    let n = content.len();
    if n < 0x80 {
        out.push(n as u8);
    } else if n <= 0xFF {
        out.extend_from_slice(&[0x81, n as u8]);
    } else {
        out.extend_from_slice(&[0x82, (n >> 8) as u8, (n & 0xFF) as u8]);
    }
    out.extend_from_slice(content);
    out
}

/// Encode an OID from its arcs (base-128, most significant group first).
pub(crate) fn oid(arcs: &[u64]) -> Vec<u8> {
    let mut body = vec![(40 * arcs[0] + arcs[1]) as u8];
    for &arc in &arcs[2..] {
        let mut groups = Vec::new();
        let mut v = arc;
        loop {
            groups.push(v & 0x7F);
            v >>= 7;
            if v == 0 {
                break;
            }
        }
        for (i, &group) in groups.iter().enumerate().rev() {
            if i == 0 {
                body.push(group as u8);
            } else {
                body.push(0x80 | group as u8);
            }
        }
    }
    tlv(0x06, &body)
}

/// Encode a non-negative INTEGER.
pub(crate) fn integer(v: u64) -> Vec<u8> {
    let be = v.to_be_bytes();
    let first = be.iter().position(|&b| b != 0).unwrap_or(7);
    let mut body = be[first..].to_vec();
    if body.first().is_some_and(|b| b & 0x80 != 0) {
        body.insert(0, 0);
    }
    if body.is_empty() {
        body.push(0);
    }
    tlv(0x02, &body)
}

/// `Cn=<label>` as one RDN, for the distinguished issuer/subject `Name`.
pub(crate) fn name_cn(label: &str) -> Vec<u8> {
    let atv = {
        let mut b = oid(&[2, 5, 4, 3]);
        b.extend_from_slice(&tlv(0x13, label.as_bytes()));
        tlv(0x30, &b)
    };
    tlv(0x30, &tlv(0x31, &atv))
}

fn utctime(s: &str) -> Vec<u8> {
    tlv(0x17, s.as_bytes())
}

/// The DER content of the serial `INTEGER` for `v` (what a matching
/// `RecipientIdentifier` must quote).
pub(crate) fn serial_content(v: u64) -> Vec<u8> {
    let enc = integer(v);
    enc[2..].to_vec()
}

/// A v3 self-shape X.509 certificate: `cn=<label>` issuer/subject, chosen
/// serial, RSA-shaped or EC-shaped SPKI, and the optional
/// `subjectKeyIdentifier` extension. The signature value is fixed filler —
/// the identity reader never verifies signatures.
pub(crate) fn certificate(
    spki_bits: &[u8],
    serial: u64,
    label: &str,
    ec: bool,
    ski: Option<&[u8]>,
) -> Vec<u8> {
    let key_alg = if ec {
        // id-ecPublicKey + P-256 curve parameter.
        let mut a = oid(&[1, 2, 840, 10_045, 2, 1]);
        a.extend_from_slice(&oid(&[1, 2, 840, 10_045, 3, 1, 7]));
        tlv(0x30, &a)
    } else {
        // rsaEncryption + NULL.
        let mut a = oid(&[1, 2, 840, 113_549, 1, 1, 1]);
        a.extend_from_slice(&[0x05, 0x00]);
        tlv(0x30, &a)
    };
    let spki = {
        let mut b = key_alg.clone();
        let mut bit = vec![0u8];
        bit.extend_from_slice(spki_bits);
        b.extend_from_slice(&tlv(0x03, &bit));
        tlv(0x30, &b)
    };
    let validity = tlv(
        0x30,
        &[utctime("260101000000Z"), utctime("991231235959Z")].concat(),
    );
    let subject = name_cn(label);
    let issuer = subject.clone();
    let sig_alg = {
        let mut a = if ec {
            oid(&[1, 2, 840, 10_045, 4, 3, 2]) // ecdsa-with-SHA256
        } else {
            oid(&[1, 2, 840, 113_549, 1, 1, 11]) // sha256WithRSAEncryption
        };
        a.extend_from_slice(&[0x05, 0x00]);
        tlv(0x30, &a)
    };

    let mut tbs = tlv(0xA0, &integer(2)); // version v3
    tbs.extend_from_slice(&integer(serial));
    tbs.extend_from_slice(&sig_alg);
    tbs.extend_from_slice(&issuer);
    tbs.extend_from_slice(&validity);
    tbs.extend_from_slice(&subject);
    tbs.extend_from_slice(&spki);
    if let Some(ski) = ski {
        let ext = {
            let mut b = oid(&[2, 5, 29, 14]);
            b.extend_from_slice(&tlv(0x04, &tlv(0x04, ski)));
            tlv(0x30, &b)
        };
        tbs.extend_from_slice(&tlv(0xA3, &tlv(0x30, &ext)));
    }
    let tbs = tlv(0x30, &tbs);

    let mut cert = tbs;
    cert.extend_from_slice(&sig_alg);
    let mut sig = vec![0u8]; // BIT STRING: no unused bits
    sig.extend_from_slice(&[0x22u8; 32]); // filler — never verified
    cert.extend_from_slice(&tlv(0x03, &sig));
    tlv(0x30, &cert)
}
