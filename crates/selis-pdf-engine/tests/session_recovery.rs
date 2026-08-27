//! `Session::open` accepts the A3 recovery corpus files (TASK A3).
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

use selis_pdf_engine::Session;
use selis_sandbox::{Budget, Surface};

// Corpus bytes are embedded at compile time: L0–L3 crates must stay
// filesystem-free (xtask check-purity), even in their tests.
static FORM_TWO_PAGES: &[u8] = include_bytes!("../../../corpus/pdfs/form_two_pages.pdf");
static OUTLINES_FOR_EDITOR: &[u8] = include_bytes!("../../../corpus/pdfs/outlines_for_editor.pdf");

fn open(src: &[u8]) -> Session {
    let budget = Budget::profile(Surface::Viewer);
    Session::open(src.to_vec(), &budget).expect("Session::open")
}

#[test]
fn session_opens_form_two_pages() {
    let session = open(FORM_TWO_PAGES);
    assert_eq!(session.len(), 2, "two pages");
    for page in 0..session.len() {
        assert!(
            session.page_size(page).is_some(),
            "page {page} has a media box"
        );
    }
}

#[test]
fn session_opens_outlines_for_editor() {
    let session = open(OUTLINES_FOR_EDITOR);
    assert!(session.len() >= 4, "at least four pages: {}", session.len());
    for page in 0..session.len() {
        assert!(
            session.page_size(page).is_some(),
            "page {page} has a media box"
        );
    }
}
