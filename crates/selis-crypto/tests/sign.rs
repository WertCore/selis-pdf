//! Certificate parsing, chain validation and CMS signing, against fixtures a
//! real CA tool (OpenSSL 3) produced. The fixtures are test-only: the crate
//! never ships a key, and the tools page asks the user for one.
//!
//! The chain tests pin the rule the product states in prose: a certificate is
//! trusted when it walks to an anchor the caller supplied, never because it is
//! self-signed and never because it looks plausible.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects
)]

use rsa::pkcs8::DecodePrivateKey as _;
use selis_crypto::cert::{self, DigestAlg};
use selis_crypto::cms;
use selis_sandbox::{Budget, Surface};

const ROOT: &str = include_str!("fixtures/sign/root.crt");
const LEAF: &str = include_str!("fixtures/sign/leaf.crt");
const LEAF_KEY: &str = include_str!("fixtures/sign/leaf.key");
const ROGUE: &str = include_str!("fixtures/sign/rogue.crt");

/// 2026-11-04T13:46:40Z — inside every fixture's validity window.
const NOW: i64 = 1_793_800_000;

fn guard() -> selis_sandbox::BudgetGuard<'static> {
    Box::leak(Box::new(Budget::profile(Surface::Viewer))).guard()
}

fn der(pem: &str, label: &str) -> Vec<u8> {
    cert::pem_body(pem, label).expect("fixture PEM decodes")
}

fn leaf_der() -> Vec<u8> {
    der(LEAF, "CERTIFICATE")
}

fn root_der() -> Vec<u8> {
    der(ROOT, "CERTIFICATE")
}

fn rogue_der() -> Vec<u8> {
    der(ROGUE, "CERTIFICATE")
}

fn leaf_key() -> rsa::RsaPrivateKey {
    rsa::RsaPrivateKey::from_pkcs8_pem(LEAF_KEY).expect("fixture key decodes")
}

#[test]
fn a_certificate_yields_names_validity_and_usage() {
    let leaf = leaf_der();
    let mut g = guard();
    let parsed = cert::parse(&leaf, &mut g).expect("leaf parses");
    assert_eq!(parsed.subject_common_name(), "Selis Test Signer");
    assert_eq!(parsed.issuer_common_name(), "Selis Test Root CA");
    assert_eq!(parsed.is_ca, Some(false));
    assert!(parsed.allows_digital_signature());
    assert!(!parsed.allows_certificate_signing());
    assert!(parsed.valid_at(NOW));
    assert!(!parsed.valid_at(0), "1970 is before the fixture");
    assert_eq!(parsed.version, 3);
    assert!(parsed.not_after > parsed.not_before);
}

#[test]
fn a_chain_is_trusted_only_through_a_supplied_anchor() {
    let leaf = leaf_der();
    let root = root_der();
    // Leaf -> root, with the root as the anchor.
    let mut g = guard();
    let report = cert::validate_chain(&leaf, &[], &[&root], NOW, &mut g).expect("chain walks");
    assert!(report.trusted, "reason: {:?}", report.reason);
    assert_eq!(report.links.len(), 2);
    assert!(!report.links[0].anchor);
    assert!(report.links[1].anchor);
    assert!(!report.revocation_checked);

    // The same chain with no anchors supplied is not trusted: the root is
    // self-signed, and being self-signed is not a trust decision.
    let mut g = guard();
    let untrusted = cert::validate_chain(&leaf, &[], &[], NOW, &mut g).expect("chain walks");
    assert!(!untrusted.trusted);
    assert!(untrusted
        .reason
        .as_deref()
        .is_some_and(|r| r.contains("no supplied certificate issued")));

    // A different self-signed certificate does not vouch for the leaf.
    let rogue = rogue_der();
    let mut g = guard();
    let wrong = cert::validate_chain(&leaf, &[], &[&rogue], NOW, &mut g).expect("chain walks");
    assert!(!wrong.trusted);
}

#[test]
fn an_expired_certificate_fails_its_window() {
    let leaf = leaf_der();
    let root = root_der();
    let mut g = guard();
    let report =
        cert::validate_chain(&leaf, &[], &[&root], NOW + 400_000_000, &mut g).expect("chain walks");
    assert!(!report.trusted);
    assert!(report
        .reason
        .as_deref()
        .is_some_and(|r| r.contains("not valid")));
}

#[test]
fn a_near_miss_signature_does_not_verify() {
    let leaf = leaf_der();
    let mut g = guard();
    let parsed = cert::parse(&leaf, &mut g).expect("leaf parses");
    let algorithm = parsed.signature_algorithm.expect("known algorithm");
    // The certificate's own signature, checked against the leaf's key rather
    // than its issuer's: same algorithm, wrong key.
    let wrong = cert::verify_with_key(parsed.key, algorithm, parsed.tbs, parsed.signature);
    assert!(wrong.is_err());
}

#[test]
fn signed_data_round_trips_and_detects_a_tampered_digest() {
    let leaf = leaf_der();
    let root = root_der();
    let key = leaf_key();
    let digest = DigestAlg::Sha256;
    let message = b"the pdf byte range goes here";
    let message_digest = digest.digest(message);

    let cms_bytes = cms::build_detached_signed_data(
        &key,
        &[leaf.clone(), root.clone()],
        &message_digest,
        "20261104120000Z",
        digest,
    )
    .expect("CMS builds");

    let mut g = guard();
    let parsed = cms::parse_signed_data(&cms_bytes, &mut g).expect("CMS parses");
    assert_eq!(parsed.certificates.len(), 2);
    assert_eq!(parsed.signers.len(), 1);
    let signer = parsed.signers.first().expect("one signer");
    assert_eq!(signer.digest, Some(DigestAlg::Sha256));
    let attrs = signer.signed_attrs.as_ref().expect("signed attributes");
    assert_eq!(attrs.message_digest.as_deref(), Some(&message_digest[..]));
    assert_eq!(
        attrs.content_type.as_deref(),
        Some(&[1u64, 2, 840, 113_549, 1, 7, 1][..])
    );
    assert_eq!(attrs.signing_time.as_deref(), Some("261104120000Z"));

    cms::verify_signer(signer, &parsed.certificates, &message_digest)
        .expect("the signature verifies");

    // Flip one byte of the digest: the signed attribute no longer matches.
    let mut tampered = message_digest.clone();
    tampered[0] ^= 0xFF;
    assert!(cms::verify_signer(signer, &parsed.certificates, &tampered).is_err());
}

#[test]
fn a_signed_attribute_flip_breaks_the_signature() {
    let key = leaf_key();
    let digest = DigestAlg::Sha256;
    let message_digest = digest.digest(b"payload");
    let mut bytes = cms::build_detached_signed_data(
        &key,
        &[leaf_der()],
        &message_digest,
        "20261104120000Z",
        digest,
    )
    .expect("CMS builds");
    // Find the signing-time string and change a digit; the signature covers
    // the attributes, so this must not verify even though messageDigest is
    // untouched.
    let needle = b"261104120000Z";
    let at = bytes
        .windows(needle.len())
        .position(|w| w == needle)
        .expect("signing time is in the CMS");
    bytes[at + 5] = b'9';
    let mut g = guard();
    let parsed = cms::parse_signed_data(&bytes, &mut g).expect("CMS still parses");
    let signer = parsed.signers.first().expect("one signer");
    assert!(cms::verify_signer(signer, &parsed.certificates, &message_digest).is_err());
}

#[test]
fn digest_spans_hash_the_concatenation() {
    let digest = DigestAlg::Sha256;
    assert_eq!(
        cms::digest_spans(digest, &[b"abcdef"]),
        cms::digest_spans(digest, &[b"abc", b"def"])
    );
    assert_ne!(
        cms::digest_spans(digest, &[b"abc", b"def"]),
        cms::digest_spans(digest, &[b"abd", b"def"])
    );
}

#[test]
fn base64_decodes_padded_and_unpadded_bodies() {
    assert_eq!(
        cert::base64_decode("aGVsbG8=").as_deref(),
        Some(&b"hello"[..])
    );
    // Whitespace and line breaks are ignored, as PEM requires.
    assert_eq!(
        cert::base64_decode("aGVs\r\nbG8=").as_deref(),
        Some(&b"hello"[..])
    );
    assert!(cert::base64_decode("aGVsbG8").is_some());
    assert!(cert::base64_decode("not base64!").is_none());
    assert!(cert::base64_decode("aGVsbG8=x").is_none());
}
