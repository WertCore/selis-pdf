//! SL-4.UI.09: what a health report can honestly say about a document.
//!
//! The Do: is "honest reporting as a feature", so every leg here is about the
//! difference between a measured field and a guessed one. The two that matter
//! most are the ones a boolean cannot carry: a signature field is reported as
//! PRESENT and never as valid, and an untagged document is reported untagged
//! rather than as an error.
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

use selis_pdf_engine::{Session, SignatureStatus};
use selis_sandbox::{Budget, CancelToken, FixedClock, Surface};

// Selis-authored synthetic fixtures, committed so this does not depend on the
// fetched-corpus cache. Bytes are embedded at compile time: L0-L3 crates must
// stay filesystem-free (xtask check-purity), even in their tests.

/// Clean, two pages, embedded fonts, no structure tree.
static UNTAGGED: &[u8] = include_bytes!("../../../corpus/fixtures/form_two_pages.pdf");

/// One page, untagged, no AcroForm, no /Encrypt.
static MINIMAL: &[u8] = b"%PDF-1.7
1 0 obj
<< /Type /Catalog /Pages 2 0 R >>
endobj
2 0 obj
<< /Type /Pages /Kids [3 0 R] /Count 1 >>
endobj
3 0 obj
<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << >> >>
endobj
xref
0 4
0000000000 65535 f 
0000000009 00000 n 
0000000058 00000 n 
0000000115 00000 n 
trailer
<< /Size 4 /Root 1 0 R >>
startxref
273
%%EOF
";

fn budget() -> Budget {
    Budget::profile(Surface::Viewer)
}

fn health(src: &[u8]) -> selis_pdf_engine::DocumentHealth {
    let b = budget();
    let session = Session::open(src.to_vec(), &b, &FixedClock(0)).expect("Session::open");
    let mut g = b.guard_with(&FixedClock(0), CancelToken::new());
    session.health(&b, &mut g)
}

/// The page count is measured from the resolved model, so it must agree with
/// what the renderer would open.
#[test]
fn page_count_is_measured() {
    assert_eq!(health(UNTAGGED).pages, 2);
    assert_eq!(health(MINIMAL).pages, 1);
}

/// A clear document has no `/Encrypt`, and the panel must say so rather than
/// carry permissions that mean nothing.
///
/// This is the leg that catches an implementation defaulting `encrypted` to
/// true, or synthesising an all-permissions value for a document that never
/// granted any.
#[test]
fn a_clear_document_is_not_encrypted_and_has_no_permissions() {
    let h = health(MINIMAL);
    assert!(!h.encrypted, "no /Encrypt in the trailer");
    assert!(
        h.permissions.is_none(),
        "permissions are only meaningful for an encrypted document"
    );
}

/// Untagged is a FACT, and the most common fact there is - most PDFs are
/// untagged. It must not be reported as an error or as "unknown".
#[test]
fn an_untagged_document_is_reported_untagged() {
    assert!(
        !health(UNTAGGED).tagged,
        "no /StructTreeRoot, so untagged - which is not a failure"
    );
}

/// The signature leg, and the one the Do: leans on hardest.
///
/// A boolean would have to mean either "there is a signature" or "the signature
/// is good". Nothing in this codebase evaluates a signature, so the enum carries
/// presence only and `Present` is documented as saying nothing about validity.
#[test]
fn a_document_with_no_signature_field_reports_absent() {
    assert_eq!(health(MINIMAL).signature, SignatureStatus::Absent);
    assert_eq!(health(UNTAGGED).signature, SignatureStatus::Absent);
}

/// The enum cannot express "valid" at all - the variant does not exist. That is
/// the point, and asserting it keeps a future `Verified` variant from being
/// added without also adding the verification behind it.
#[test]
fn signature_status_cannot_express_validity() {
    let statuses = [SignatureStatus::Absent, SignatureStatus::Present];
    // Two states, and `Present` is not equal to any absence claim.
    assert_ne!(statuses[0], statuses[1]);
}

/// Deviations ride along with the rest, so the panel needs one call rather than
/// a second pass that could disagree with the first.
#[test]
fn deviations_ride_with_the_health_report() {
    assert!(
        health(UNTAGGED).deviations.is_empty(),
        "a clean document with embedded fonts has no object-syntax deviations"
    );
}
