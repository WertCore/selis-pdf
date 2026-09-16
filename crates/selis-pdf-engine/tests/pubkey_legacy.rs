//! SL-1.ENC.08: the legacy CMS content algorithms (RC4 40/128, TDEA-CBC,
//! RC2-CBC) and the RSAES-OAEP key transport, over the `adbe.pkcs7.s3`-era
//! fixtures. Every fixture is deterministic in-repo material from
//! `cargo xtask pubkey-fixtures` (whose `xtask` generator is deliberately
//! independent of the reader: the DER envelope, the Algorithm-1 file key and
//! the per-object streams are re-derived from the spec, while the block
//! ciphers' tables match `openssl enc` output pinned in
//! `selis-crypto/src/legacy.rs`). The DoD's "three 2010-era RC4-CMS cases
//! open with the right key and refuse with a typed error under the wrong one"
//! is therefore checked on bytes whose provenance is independent of the code
//! under test.
//!
//! The wild corpus is not involved (corpus policy forbids committing
//! public-key documents): `xtask pubkey-fixtures --check` re-derives every
//! byte on every CI run.

#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing,
        clippy::arithmetic_side_effects,
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        clippy::integer_division
    )
)]

use selis_error::Code;
use selis_pdf_engine::Session;
use selis_sandbox::{Budget, FixedClock, Surface};

/// The recipient RSA-2048 key + the fixture certificate (identity mode).
static RECIPIENT_RSA: &[u8] = include_bytes!("fixtures/enc03-recipient-rsa2048.pk8.der");
static CERT_RSA: &[u8] = include_bytes!("fixtures/enc03-recipient-rsa2048.x509.der");
/// A different RSA key that opens none of them.
static WRONG_RSA: &[u8] = include_bytes!("fixtures/enc03-wrong-rsa2048.pk8.der");
static CERT_WRONG: &[u8] = include_bytes!("fixtures/enc03-wrong-rsa2048.x509.der");

fn viewer() -> Budget {
    Budget::profile(Surface::Viewer)
}

/// One legacy/OAEP fixture and the `adbe.pkcs7.sN`/`/V` shape it exercises.
const LEGACY_FILES: [(&str, &[u8]); 6] = [
    (
        "rc4-128 (/V 3, salted RC4 streams)",
        include_bytes!("fixtures/pubkey-rc4-s3.pdf"),
    ),
    (
        "rc4-40 export (/V 3, /Length 40)",
        include_bytes!("fixtures/pubkey-rc4-40-s3.pdf"),
    ),
    (
        "3DES-CBC content (/V 3)",
        include_bytes!("fixtures/pubkey-tdea-s3.pdf"),
    ),
    (
        "RC2-CBC content (/V 3)",
        include_bytes!("fixtures/pubkey-rc2-s3.pdf"),
    ),
    (
        "RSAES-OAEP(SHA-1) transport (/V 5)",
        include_bytes!("fixtures/pubkey-oaep-s5.pdf"),
    ),
    (
        "RSAES-OAEP(SHA-256) transport (/V 5)",
        include_bytes!("fixtures/pubkey-oaep-256-s5.pdf"),
    ),
];

/// The DoD line: the s3-era RC4/TDEA/RC2 documents open with the matching key
/// and the page content decrypts to drawing operators.
#[test]
fn legacy_cms_fixtures_open_with_the_recipient_key() {
    for (label, bytes) in LEGACY_FILES {
        let budget = viewer();
        let credential = selis_crypto::pkcs7::PubKeyCredential::rsa(RECIPIENT_RSA.to_vec())
            .with_certificate(CERT_RSA.to_vec());
        let session =
            Session::open_public_key(bytes.to_vec(), &credential, &budget, &FixedClock(0))
                .unwrap_or_else(|e| {
                    panic!("{label} must open with the recipient key: {:?}", e.code())
                });
        assert_eq!(session.len(), 1, "{label}: one page");
        let receipt = session
            .recipient_receipt()
            .unwrap_or_else(|| panic!("{label}: recipient receipt"));
        assert_eq!(receipt.recipient_index, 0, "{label}: single recipient");
        assert_eq!(
            receipt.matched_by,
            selis_crypto::pkcs7::Matched::ByIssuerAndSerialNumber,
            "{label}: the blob addresses the chain"
        );
        assert_eq!(
            receipt.cms_permissions, 0xFFFF_F0C0,
            "{label}: the fixture's grant bits"
        );
        let mut g = budget.guard();
        let dl = session
            .page_display_list(0, &budget, &mut g)
            .unwrap_or_else(|e| panic!("{label}: display list: {:?}", e.code()));
        assert!(!dl.ops.is_empty(), "{label}: the stream decrypted to ops");
    }
}

/// Every legacy/OAEP fixture refuses a credential whose key matches no
/// recipient with `RECIPIENT_NO_MATCH` — RC4's lack of padding is exactly the
/// case the design note §5 says must still fail typed, because the RSA/OAEP
/// transport unwrap is the gate (see `pkcs7::unwrap_cek`).
#[test]
fn legacy_cms_fixtures_refuse_the_wrong_key() {
    for (label, bytes) in LEGACY_FILES {
        let budget = viewer();
        for mode in [
            selis_crypto::pkcs7::MatchBy::Auto,
            selis_crypto::pkcs7::MatchBy::Certificate,
        ] {
            let credential = selis_crypto::pkcs7::PubKeyCredential::rsa(WRONG_RSA.to_vec())
                .with_certificate(CERT_WRONG.to_vec())
                .matching(mode);
            match Session::open_public_key(bytes.to_vec(), &credential, &budget, &FixedClock(0)) {
                Err(e) => assert_eq!(
                    e.code(),
                    Code::RecipientNoMatch,
                    "{label} ({mode:?}) with the wrong key"
                ),
                Ok(_) => panic!("{label} ({mode:?}) must not open with the wrong key"),
            }
        }
    }
}

/// The legacy fixtures open the *tolerant* way without a credential (the
/// ENC.03 posture: page tree resolves, content does not decrypt, no receipt).
#[test]
fn legacy_cms_fixtures_open_tolerantly_without_a_credential() {
    for (label, bytes) in LEGACY_FILES {
        let budget = viewer();
        let session = Session::open(bytes.to_vec(), &budget, &FixedClock(0))
            .unwrap_or_else(|e| panic!("{label}: tolerant open: {:?}", e.code()));
        assert!(
            session.recipient_receipt().is_none(),
            "{label}: no receipt without a credential"
        );
    }
}
