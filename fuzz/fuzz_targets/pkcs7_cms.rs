//! Fuzz target for the PKCS#7/CMS parse entry point (SL-1.ENC.03) and the
//! certificate-identity recipient selection layered on it (SL-1.ENC.07),
//! with the permission-block decode asserted by the sibling `pkcs7_perms`
//! target (SL-1.ENC.09) and the legacy CMS *content* ciphers (RC4 40/128,
//! TDEA-CBC, RC2-CBC) plus the RSAES-OAEP key transport exercised since
//! SL-1.ENC.08.
//!
//! Exercises `parse_enveloped_data` on fuzzed data — no panic, no OOM,
//! terminates within budget, every structural deviation is a typed error —
//! and, since SL-1.ENC.07, feeds the same bytes through
//! `selis_crypto::x509::parse_identity` and a full `authenticate_public_key`
//! selection with a fixed credential: the committed "Alice" recipient key
//! presented with each certificate of the fixture chain family (its own —
//! an identity match — and Bob's — the wrong-serial case). The selection
//! contract: a `Matched::By*` result must be *re-verifiable* against the
//! blob's own identifiers (the fuzz can never observe selection of a
//! recipient the chain does not address), every failure is typed, and under
//! `MatchBy::Certificate` no structural fall-through may ever occur.
//! SL-1.ENC.08 adds the *content-shape* half of that contract, since the
//! legacy ciphers differ in one important way from AES-CBC: RC4 carries **no
//! padding at all**, so the only wrong-CEK gate is the transport unwrap that
//! already ran (`unwrap_cek`) — which the seeds below pin:
//!
//! * a parsed `Rc4` content must be exactly the 24-byte payload — `decrypt_payload`
//!   accepts nothing else, so a blob that would hand a garbage keystream is a
//!   parse error, not a silent open;
//! * a *successful* selection's file key must be `/Length`-long with `/Length`
//!   honoured as the derivation parameter (an unwrap that never produced a
//!   legal 24-byte payload could not reach this point);
//! * every legacy/wrong-CEK blob in the corpus must fail typed.
//!
//! Seeds (from `cargo xtask pubkey-fuzz-seeds`): the ENC.03 CMS corpus plus
//! valid X.509 chain blobs, a wrong-serial and a duplicate-identifier CMS
//! input, corrupted certificates, and ENC.08's legacy-content + OAEP inputs
//! (RC4-128/40, TDEA, RC2-128, RC2-40, OAEP SHA-1/256 — plus the *wrong-CEK*
//! shapes, where the wrapped key cannot be the content key's length).

#![no_main]

use libfuzzer_sys::fuzz_target;
use selis_crypto::pkcs7::{
    authenticate_public_key, identifier_matches, parse_enveloped_data, CekAlgorithm, MatchBy,
    Matched, PubKeyCredential,
};
use selis_crypto::x509::parse_identity;
use selis_sandbox::{Budget, Surface};

/// The fixture "Alice" RSA private key and her certificate (test-only
/// throwaways, committed as `enc03-*.der` engine fixtures).
static ALICE_KEY: &[u8] =
    include_bytes!("../../crates/selis-pdf-engine/tests/fixtures/enc03-recipient-rsa2048.pk8.der");
static ALICE_CERT: &[u8] =
    include_bytes!("../../crates/selis-pdf-engine/tests/fixtures/enc03-recipient-rsa2048.x509.der");
/// Bob's certificate — the wrong-identity chain: with it, `Certificate`
/// mode must never open anything Alice's blobs address (and must never
/// silently *pick* a differently-addressed recipient).
static BOB_CERT: &[u8] =
    include_bytes!("../../crates/selis-pdf-engine/tests/fixtures/enc03-wrong-rsa2048.x509.der");

fn budget() -> Budget {
    Budget::profile(Surface::Fuzz)
}

fuzz_target!(|data: &[u8]| {
    // --- parse contract (ENC.03) ------------------------------------------
    let mut g = budget().guard();
    // A typed error or BudgetBytes is fine; a panic would abort the harness.
    let parsed = parse_enveloped_data(data, &mut g);
    if let Ok(enveloped) = &parsed {
        // ENC.08's content-shape contract: the ciphers that carry no padding
        // of their own (RC4) or run on an 8-byte block (TDEA, RC2) may not
        // present a body the reader would happily trim or mis-unwrap.
        if let Some(content) = enveloped.encrypted_content {
            match enveloped.cek_algorithm {
                CekAlgorithm::Rc4 => {
                    assert_eq!(content.len(), 24, "RC4 seals exactly the 24-byte payload");
                }
                CekAlgorithm::TdeaCbc { .. } | CekAlgorithm::Rc2Cbc { .. } => {
                    assert!(
                        content.len() >= 16 && content.len() % 8 == 0,
                        "8-byte-block legacy content is ≥ 2 blocks, block-multiple"
                    );
                }
                CekAlgorithm::Aes128Cbc { .. }
                | CekAlgorithm::Aes192Cbc { .. }
                | CekAlgorithm::Aes256Cbc { .. } => {
                    assert!(
                        content.len() >= 32 && content.len() % 16 == 0,
                        "AES-CBC content is ≥ 2 blocks, block-multiple"
                    );
                }
            }
        }
    }

    // --- certificate branch (ENC.07): the same bytes through the reader ---
    let mut g = budget().guard();
    match parse_identity(data, &mut g) {
        Ok(identity) => {
            assert!(identity.issuer.len() <= data.len());
            assert!(!identity.serial.is_empty());
        }
        Err(e) => assert!(
            e.code() == selis_error::Code::EncryptMalformed
                || e.code() == selis_error::Code::EncryptUnsupported
                || e.is_budget()
                || e.is_cancelled(),
            "untyped certificate-parse failure: {:?}",
            e.code()
        ),
    }

    // --- selection branch (ENC.07): fixed credential, fuzzed blob ----------
    for chain in [ALICE_CERT, BOB_CERT] {
        for mode in [MatchBy::Auto, MatchBy::Certificate] {
            let mut g = budget().guard();
            match authenticate_public_key(
                &[data],
                &PubKeyCredential::rsa(ALICE_KEY.to_vec())
                    .with_certificate(chain.to_vec())
                    .matching(mode),
                256,
                true,
                true,
                &mut g,
            ) {
                Ok(auth) => {
                    verify_identity_selection(data, chain, auth.matched_by);
                    assert_eq!(auth.recipient_index, 0);
                    // The derivation honours the length parameter the caller
                    // passed, and a selection that *opened* did so through a
                    // transport whose unwrap yielded a legal content key (the
                    // RC4 path has no padding to fail later; the parse-side
                    // contract above is what bounds its input).
                    assert_eq!(auth.key.len(), 32, "the derived file key is /Length bytes");
                    assert!(
                        mode != MatchBy::Certificate
                            || !matches!(auth.matched_by, Matched::Structurally),
                        "Certificate mode opened structurally — a fall-through it must never take"
                    );
                }
                Err(e) => assert!(
                    matches!(
                        e.code(),
                        selis_error::Code::EncryptMalformed
                            | selis_error::Code::EncryptUnsupported
                            | selis_error::Code::RecipientNoMatch
                    ) || e.is_budget()
                        || e.is_cancelled(),
                    "untyped selection failure: {:?}",
                    e.code()
                ),
            }
        }
    }
});

/// A `Matched::By*` result must rest on an identifier in the blob that the
/// chain really addresses — re-derived here through the parse + match
/// primitives rather than trusted from the selection loop (this is the
/// "never select the wrong recipient" fuzz invariant).
fn verify_identity_selection(blob: &[u8], chain: &[u8], matched: Matched) {
    if !matches!(matched, Matched::ByIssuerAndSerialNumber | Matched::BySubjectKeyIdentifier) {
        return;
    }
    let mut g = budget().guard();
    let Ok(enveloped) = parse_enveloped_data(blob, &mut g) else {
        panic!("selection Ok on an unparsable CMS blob");
    };
    let Ok(identity) = parse_identity(chain, &mut g) else {
        panic!("selection Ok against an unparsable chain certificate");
    };
    let addressed = enveloped
        .recipients
        .iter()
        .any(|transport| identifier_matches(&identity, transport.identifier()).is_some());
    assert!(
        addressed,
        "selected a recipient the chain does not address (matched: {matched:?})"
    );
}
