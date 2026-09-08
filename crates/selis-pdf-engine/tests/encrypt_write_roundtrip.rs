//! SL-1.ENC.02 write side: a document built with DocumentBuilder, encrypted
//! with the R6 handler, then opened via `Session::open` with the empty user
//! password — the engine-path round-trip.  The corpus-based round-trip (COS
//! layer) is in `selis-pdf-cos`'s `corpus_file_encrypt_roundtrip` test.
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

use selis_pdf_cos::doc_writer::{ContentBuilder, DocumentBuilder};
use selis_pdf_engine::Session;
use selis_sandbox::BudgetGuard;

fn guard() -> BudgetGuard<'static> {
    use selis_sandbox::{Budget, CancelToken, FixedClock};
    Budget::unlimited().guard_with(&FixedClock(0), CancelToken::new())
}

#[test]
fn encrypted_document_roundtrip_through_session() {
    let mut b = DocumentBuilder::new();
    let mut c = ContentBuilder::new();
    c.set_fill(1.0, 0.0, 0.0).fill_rect(0.0, 0.0, 100.0, 100.0);
    c.begin_text()
        .set_font("Helvetica", 12.0)
        .text_at(10.0, 50.0)
        .show_text("Session round trip")
        .end_text();
    b.add_page(100.0, 100.0, c.to_bytes().as_slice());

    let (info, file_key) =
        selis_pdf_cos::encrypt::EncryptInfo::new_r6(b"", b"owner", 0xFFFFF0C0, &[0u8; 16]);
    b.set_encrypt(info, file_key);
    let mut g = guard();
    let budget = selis_sandbox::Budget::profile(selis_sandbox::Surface::Viewer);
    let bytes = b.write(&budget, &mut g).expect("write encrypted");

    // Session::open with the empty user password must decrypt the content.
    let session = Session::open(bytes, &budget).expect("Session::open encrypted");
    assert_eq!(session.len(), 1, "one page");
    assert!(session.page_size(0).is_some(), "page has a media box");
}
