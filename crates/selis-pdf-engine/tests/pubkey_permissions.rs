//! SL-1.ENC.09 — per-recipient PKCS#7 permission bits are *enforced*
//! through the selis-policy layer, not just surfaced as metadata.
//!
//! The `pubkey-perms-s5.pdf` fixture (regenerated on drift by
//! `cargo xtask pubkey-fixtures --check`) carries three recipients with
//! three different 4-byte CMS permission blocks and an all-allowing
//! `/P -4` (bits 1-2 are clear "always" bits):
//!
//! * **blob 0** — Alice (the recipient RSA key): print/modify/copy/annotate/
//!   fill (`GRANT_ALL`).
//! * **blob 1** — Bob (the second RSA key): **extract only** (`GRANT_EXTRACT`).
//! * **blob 2** — the EC recipient: **view only** (`GRANT_VIEW`).
//!
//! All three open the *same* file key (the shared 20-byte seed), so every
//! recipient can see the document; the permission block decides which
//! operations Selis's API runs. That per-recipient binding is the
//! differentiation: a weaker recipient never inherits a stronger one's
//! grant, even though the PDF `/P` would allow more (design note §6.2).
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
use selis_policy::ContentOp;
use selis_sandbox::{Budget, FixedClock, Surface};

static RECIPIENT_RSA: &[u8] = include_bytes!("fixtures/enc03-recipient-rsa2048.pk8.der");
static BOB_RSA: &[u8] = include_bytes!("fixtures/enc03-wrong-rsa2048.pk8.der");
static RECIPIENT_EC: &[u8] = include_bytes!("fixtures/enc03-recipient-ecp256.pk8.der");
static CERT_RSA: &[u8] = include_bytes!("fixtures/enc03-recipient-rsa2048.x509.der");
static CERT_BOB: &[u8] = include_bytes!("fixtures/enc03-wrong-rsa2048.x509.der");
static CERT_EC: &[u8] = include_bytes!("fixtures/enc03-recipient-ecp256.x509.der");

/// The permission blocks the generator encodes (keep in sync with
/// `xtask pkcs7_fixtures.rs` — the engine tests only read them).

fn viewer() -> Budget {
    Budget::profile(Surface::Viewer)
}

fn open(credential: &selis_crypto::pkcs7::PubKeyCredential) -> selis_error::Result<Session> {
    let budget = viewer();
    Session::open_as_recipient(
        include_bytes!("fixtures/pubkey-perms-s5.pdf").to_vec(),
        credential,
        &budget,
        &FixedClock(0),
    )
}

/// Alice's key gives Alice's (stronger) bits: every operation she holds is
/// allowed. Bob's key gives Bob's (extract-only) bits, even though the same
/// `/P` allows more, and even though opening the file as Alice would —
/// Bob cannot annotate or modify. This is the "not Alice's stronger ones"
/// case from DoD 4/7.
#[test]
fn each_recipient_is_bound_to_its_own_cms_grant() {
    let alice = open(
        &selis_crypto::pkcs7::PubKeyCredential::rsa(RECIPIENT_RSA.to_vec())
            .with_certificate(CERT_RSA.to_vec())
            .matching(selis_crypto::pkcs7::MatchBy::Certificate),
    )
    .expect("Alice opens");
    let a = alice.recipient_receipt().expect("alice receipt");
    assert_eq!(a.recipient_index, 0, "Alice is blob 0");
    assert_eq!(
        a.matched_by,
        selis_crypto::pkcs7::Matched::ByIssuerAndSerialNumber
    );
    // Alice: print, copy, annotate, edit, redact, fill are all allowed.
    for op in [
        ContentOp::Print,
        ContentOp::CopyText,
        ContentOp::Annotate,
        ContentOp::Redact,
        ContentOp::Edit,
        ContentOp::FillForm,
    ] {
        assert!(
            alice.check_permissions(op).is_ok(),
            "Alice must allow {op:?}: {:08x}",
            a.cms_permissions
        );
    }

    let bob = open(
        &selis_crypto::pkcs7::PubKeyCredential::rsa(BOB_RSA.to_vec())
            .with_certificate(CERT_BOB.to_vec())
            .matching(selis_crypto::pkcs7::MatchBy::Certificate),
    )
    .expect("Bob opens");
    let b = bob.recipient_receipt().expect("bob receipt");
    assert_eq!(b.recipient_index, 1, "Bob is blob 1");
    assert_eq!(
        b.matched_by,
        selis_crypto::pkcs7::Matched::BySubjectKeyIdentifier
    );
    // Bob: extract only — copy allowed, annotate/redact/edit/print/fill denied.
    assert!(bob.check_permissions(ContentOp::CopyText).is_ok());
    for op in [
        ContentOp::Annotate,
        ContentOp::Redact,
        ContentOp::Edit,
        ContentOp::Print,
    ] {
        let e = bob
            .check_permissions(op)
            .err()
            .unwrap_or_else(|| panic!("Bob must not allow {op:?}"));
        assert_eq!(e.code(), Code::PermissionDeniedByCms);
    }
    // Bob's effective grant is strictly weaker than the document /P (which
    // would allow more) and weaker than Alice's. A CMS grant never raises.
    assert_ne!(b.cms_permissions, a.cms_permissions);
    assert!(!b.effective().annotate());
    assert!(!b.effective().print());
}

/// A view-only recipient can see the structure (open succeeded; `len()` is
/// known) but none of the *content-touching* operations pass.
#[test]
fn view_only_recipient_cannot_extract_annotate_edit() {
    let session = open(
        &selis_crypto::pkcs7::PubKeyCredential::ec_p256(RECIPIENT_EC.to_vec())
            .with_certificate(CERT_EC.to_vec())
            .matching(selis_crypto::pkcs7::MatchBy::Certificate),
    )
    .expect("the recipient opens");
    assert_eq!(session.len(), 1, "page count is always viewable");
    let r = session.recipient_receipt().expect("receipt");
    assert_eq!(r.recipient_index, 2, "view-only is blob 2");

    // Viewing/render-to-screen is allowed (page_size, page_view — structural).
    assert!(session.page_size(0).is_some());

    // extract/copy, annotate, redact/edit, form-fill, print: all refused.
    for op in [
        ContentOp::CopyText,
        ContentOp::Annotate,
        ContentOp::Redact,
        ContentOp::Edit,
        ContentOp::FillForm,
        ContentOp::Print,
    ] {
        assert_eq!(
            session.check_permissions(op).err().map(|e| e.code()),
            Some(Code::PermissionDeniedByCms),
            "a view-only grant never allows {op:?}",
        );
    }
}

/// A *bare* RSA credential (no certificate chain) — no fall-through to
/// permission bits the wrong blob holds: the structural selector must still
/// resolve to the correct blob's CMS block (first blob whose key decrypts),
/// and Bob's bits are never silently replaced by Alice's just because one
/// decrypts. This is the fuzz property ("never select wrong recipient")
/// applied to the *permission* block specifically.
#[test]
fn permission_bits_follow_the_opened_recipient_not_the_first_blob() {
    // Bob's bare key (no chain): the structural pass decrypts blob 1's
    // content — so the receipt carries Bob's grant, not Alice's (blob 0).
    let session = open(
        &selis_crypto::pkcs7::PubKeyCredential::rsa(BOB_RSA.to_vec())
            .matching(selis_crypto::pkcs7::MatchBy::Auto),
    )
    .expect("auto fall-through opens Bob structurally");
    let r = session.recipient_receipt().expect("receipt");
    assert_eq!(r.recipient_index, 1, "Bob decrypted blob 1, not blob 0");
    assert_eq!(r.matched_by, selis_crypto::pkcs7::Matched::Structurally);
    assert_eq!(r.cms_permissions, 0xFFFF_F020);
    assert!(!r.effective().annotate(), "Bob still cannot annotate");
    // But he *can* extract (his own grant).
    assert!(session.check_permissions(ContentOp::CopyText).is_ok());
}

/// The standard (non-public-key, tolerant) open keeps the `/P`-only path:
/// with no receipt, `check_permissions` is always allowed (it never
/// consults the PDF `/P` here — the owner/standard override posture,
/// SL-1.ENC.04). This is the "owner password path keeps standard /P-only"
/// case.
#[test]
fn owner_path_keeps_pdf_only_semantics() {
    let budget = viewer();
    let session = Session::open(
        include_bytes!("fixtures/pubkey-rsa-s4.pdf").to_vec(),
        &budget,
        &FixedClock(0),
    )
    .expect("tolerant owner-like open");
    assert!(session.recipient_receipt().is_none());
    // Without a receipt the CMS bits bind no-one; policy /P-only posture
    // allows (the override is the caller's, per SL-1.ENC.04).
    assert!(session.check_permissions(ContentOp::Print).is_ok());
    assert!(session.check_permissions(ContentOp::Redact).is_ok());
}

/// Every content-touching gate funnels through `check_permissions`, so the
/// single choke point covers extract-copy ops. A view-only recipient is
/// denied the copy gate (which in the Phase-5 redact/annotate ops will also
/// refuse); a copy-granted recipient passes it even though `/P` is not even
/// consulted because it is all-allowing here — the CMS grant binds.
#[test]
fn copy_gate_intersects_cms_for_every_recipient() {
    let view = open(
        &selis_crypto::pkcs7::PubKeyCredential::ec_p256(RECIPIENT_EC.to_vec())
            .with_certificate(CERT_EC.to_vec())
            .matching(selis_crypto::pkcs7::MatchBy::Certificate),
    )
    .expect("view recipient opens");
    // View-only blocks content-copy: the copy gate fails.
    assert_eq!(
        view.check_permissions(ContentOp::CopyText)
            .err()
            .map(|e| e.code()),
        Some(Code::PermissionDeniedByCms)
    );

    let bob = open(
        &selis_crypto::pkcs7::PubKeyCredential::rsa(BOB_RSA.to_vec())
            .with_certificate(CERT_BOB.to_vec())
            .matching(selis_crypto::pkcs7::MatchBy::Certificate),
    )
    .expect("Bob opens (extract-only: copy bit set)");
    // Bob's CMS block has copy set → CopyText passes.
    assert!(bob.check_permissions(ContentOp::CopyText).is_ok());

    // The wired content egress (`embedded_file_data`) consults the same
    // gate the copy bit feeds: view-only fails typed even for a key that
    // does not exist (the gate runs before resolution); Bob's copy grant
    // reaches the (empty) inventory.
    let budget = viewer();
    let mut g = budget.guard();
    assert_eq!(
        view.embedded_file_data("nope", &budget, &mut g)
            .err()
            .map(|e| e.code()),
        Some(Code::PermissionDeniedByCms)
    );
    assert_eq!(
        bob.embedded_file_data("nope", &budget, &mut g)
            .expect("copy-granted recipient gets past the gate"),
        None
    );
}
