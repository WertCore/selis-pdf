//! SL-1.ENC.02 write side: a corpus file re-encrypted with the R6 handler
//! opens via `Session::open` with the empty user password and keeps its page
//! count (round-trip to the engine path).
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

use selis_pdf_cos::{Obj, Ref, XrefEntry};
use selis_pdf_engine::Session;
use selis_sandbox::{Budget, BudgetGuard, CancelToken, FixedClock, Surface};

const CORPUS: &str = "D:\\selis\\corpus\\pdfs\\90ms_rksj_h_sample.pdf";

fn guard() -> BudgetGuard<'static> {
    Budget::unlimited().guard_with(&FixedClock(0), CancelToken::new())
}

/// Collect every in-use object of `src` into `(object_number, Obj)`.
fn collect_objects(src: &[u8]) -> (Vec<(u32, Obj)>, Ref) {
    let budget = Budget::profile(Surface::Viewer);
    let mut g = budget.guard_with(&FixedClock(0), CancelToken::new());
    let startxref = selis_pdf_cos::xref::find_startxref(src, 2048).expect("startxref");
    let doc = selis_pdf_cos::parse_revisions(src, startxref, &budget, &mut g).expect("parse");
    let rev = &doc.revisions()[0];
    let mut objects = Vec::new();
    for (num, entry) in &rev.entries {
        if let XrefEntry::InUse { offset, .. } = entry {
            let obj = selis_pdf_cos::resolve_object(src, *offset, &budget, &mut g).expect("resolve");
            objects.push((*num, obj));
        }
    }
    let root = rev.root.unwrap_or(Ref::new(1, 0));
    (objects, root)
}

#[test]
fn encrypted_corpus_roundtrip_through_session() {
    let src = std::fs::read(CORPUS).expect("read corpus file");
    let budget = Budget::profile(Surface::Viewer);
    let plain = Session::open(src.clone(), &budget).expect("open plaintext");
    let plain_pages = plain.len();
    assert!(plain_pages > 0, "corpus must have pages");

    let (objects, root) = collect_objects(&src);
    let (info, file_key) =
        selis_pdf_cos::encrypt::EncryptInfo::new_r6(b"", b"owner", 0xFFFFF0C0, &[0u8; 16]);
    let mut g = guard();
    let encrypted = selis_pdf_cos::doc_writer::write_objects_as_document_encrypted(
        &objects,
        root,
        &info,
        &file_key,
        &budget,
        &mut g,
    )
    .expect("write encrypted");

    // Session::open with the empty user password must decrypt and keep pages.
    let session = Session::open(encrypted, &budget).expect("open encrypted");
    assert_eq!(
        session.len(),
        plain_pages,
        "page count preserved through encryption"
    );
    for page in 0..session.len() {
        assert!(
            session.page_size(page).is_some(),
            "page {page} has a media box"
        );
    }
}
