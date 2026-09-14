//! Tests for the X.509 identity subset (SL-1.ENC.07).

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects
)]

use super::*;
use crate::test_fixtures::{certificate, integer, tlv};
use selis_error::Code;
use selis_sandbox::{Budget, Surface};

fn guard() -> BudgetGuard<'static> {
    Budget::profile(Surface::Fuzz).guard()
}

/// The happy path: issuer, serial, and SKI extension all come back as the
/// exact DER bytes the CMS identifier forms must quote.
#[test]
fn identity_reads_issuer_serial_and_ski() {
    let mut g = guard();
    let ski = [0xABu8; 20];
    let cert = certificate(
        &[0x30, 0x82, 0x01, 0x22],
        77,
        "selis-enc07",
        false,
        Some(&ski),
    );
    let ident = parse_identity(&cert, &mut g).expect("parse");
    assert_eq!(ident.serial, &[0x4Du8][..]); // 77 = 0x4D
    assert_eq!(ident.extension_ski, Some(&ski[..]));
    assert_eq!(ident.computed_ski, None); // RSA: no method-1 fallback
                                          // The issuer is the same RDN bytes the writer put in the Name (self-
                                          // issued here; a matching CMS identifier must quote these exact bytes).
    assert!(!ident.issuer.is_empty());

    // A certificate without the extension yields no extension SKI.
    let mut g2 = guard();
    let probe = certificate(&[0x00], 77, "selis-enc07", false, None);
    let p = parse_identity(&probe, &mut g2).expect("probe");
    assert_eq!(ident.issuer, p.issuer);
    assert!(p.extension_ski.is_none());
}

/// An EC certificate without the extension still yields the method-1 SKI
/// (SHA-1 of the public key octets).
#[test]
fn ec_identity_computes_the_method1_ski() {
    let mut g = guard();
    let point = [0x04u8, 1, 2, 3];
    let cert = certificate(&point, 5, "ec", true, None);
    let ident = parse_identity(&cert, &mut g).expect("parse");
    assert_eq!(
        ident.computed_ski,
        Some(<[u8; 20]>::try_from(sha1::Sha1::digest(&point).as_slice()).expect("20"))
    );
    assert_eq!(ident.extension_ski, None);
}

#[test]
fn malformed_certificates_are_typed_errors() {
    let cases: Vec<(&str, Vec<u8>)> = vec![
        ("not a sequence", tlv(0x31, &[])),
        ("trailing bytes", {
            let mut c = certificate(&[0x00], 1, "a", false, None);
            c.push(0xFF);
            c
        }),
        ("no TBS", tlv(0x30, &tlv(0x30, &[0x05, 0x00]))),
        ("empty serial", {
            // TBSCertificate { version, serial INTEGER (empty) }.
            let mut tbs = tlv(0xA0, &integer(2));
            tbs.extend_from_slice(&tlv(0x02, &[]));
            let inner = tlv(0x30, &tbs);
            tlv(0x30, &inner)
        }),
        ("negative version", {
            let mut tbs = tlv(0xA0, &tlv(0x02, &[0x82]));
            tbs.extend_from_slice(&integer(1));
            let inner = tlv(0x30, &tbs);
            tlv(0x30, &inner)
        }),
    ];
    for (label, cert) in cases {
        let mut g = guard();
        let e = parse_identity(&cert, &mut g)
            .err()
            .unwrap_or_else(|| panic!("{label}: must be a typed error"));
        assert_eq!(e.code(), Code::EncryptMalformed, "{label}");
    }
}

/// Budget exhaustion during the certificate walk is a typed budget error.
#[test]
fn identity_parse_respects_the_budget() {
    let cert = certificate(
        &[0x30, 0x82, 0x01, 0x22],
        77,
        "selis-enc07-long-name",
        true,
        Some(&[0x02; 20]),
    );
    let mut g = Budget {
        bytes: 24,
        ..Budget::profile(Surface::Fuzz)
    }
    .guard();
    let e = parse_identity(&cert, &mut g).expect_err("over budget");
    assert!(e.is_budget());
}

/// A BIT STRING with unused bits is malformed key material.
#[test]
fn unused_bits_in_the_public_key_are_malformed() {
    let mut g = guard();
    // SPKI key content is one byte, so the BIT STRING is exactly
    // 03 02 00 AA — flip the unused-bits octet.
    let mut cert = certificate(&[0xAA], 1, "a", false, None);
    let pos = cert
        .windows(3)
        .position(|w| w == [0x03, 0x02, 0x00])
        .expect("spki bit string");
    cert[pos + 2] = 0x03;
    let e = parse_identity(&cert, &mut g).expect_err("unused bits");
    assert_eq!(e.code(), Code::EncryptMalformed);
}
