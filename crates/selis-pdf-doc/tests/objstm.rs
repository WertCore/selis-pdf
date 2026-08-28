//! End-to-end integration test: a PDF 1.5 whose page tree, font, and catalog
//! live inside an object stream, parsed via an xref stream.
//!
//! This exercises the full compressed-object resolution path:
//! `parse_revisions` (detects xref stream) → `Document::resolve` →
//! `resolve_ref` → `resolve_compressed` → `parse_value_at`.

#![allow(
    clippy::panic,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing
)]

use selis_pdf_cos::Obj;
use selis_pdf_doc::{Document, Resolver};
use selis_sandbox::{Budget, BudgetGuard, CancelToken, FixedClock};

fn guard() -> BudgetGuard<'static> {
    Budget::unlimited().guard_with(&FixedClock(0), CancelToken::new())
}

#[test]
fn xref_stream_objstm_resolves_end_to_end() {
    let src = include_bytes!("fixtures/pdf15_objstm.pdf");
    let budget = Budget::unlimited();
    let mut g = guard();

    let startxref = selis_pdf_cos::xref::find_startxref(src, 2048).expect("startxref");
    let doc =
        selis_pdf_cos::parse_revisions(src, startxref, &budget, &mut g).expect("parse revisions");
    assert_eq!(doc.len(), 1, "one revision");

    // Resolve the document model (catalog, page tree). Objects 1, 2, 4 are
    // compressed inside ObjStm 6.
    let document = Document::resolve(&doc, src, &budget, &mut g, None).expect("resolve document");
    assert_eq!(document.len(), 1, "one page");

    // The page's content stream (object 5, direct) resolves to a stream.
    let page = document.pages.first().expect("page");
    let content_refs = page.contents.clone().expect("page has /Contents");
    let mut resolver = Resolver::new(&doc, src, &budget);
    let content = resolver
        .resolve(content_refs[0], &mut g)
        .expect("resolve content stream");
    match &content {
        Obj::Stream { data, .. } => {
            let text = String::from_utf8_lossy(data.as_slice());
            assert!(
                text.contains("BT"),
                "content stream must contain the text program, got: {text}"
            );
        }
        other => panic!("content is not a stream: {other:?}"),
    }

    // The font (object 4, compressed in ObjStm 6 index 2) resolves to a dict.
    let font = resolver
        .resolve(selis_pdf_cos::Ref::new(4, 0), &mut g)
        .expect("resolve font");
    let Obj::Dict(pairs) = &font else {
        panic!("font is not a dict: {font:?}");
    };
    assert!(
        pairs.iter().any(|(k, _)| k.as_slice() == b"BaseFont"),
        "font dict must contain /BaseFont, got: {pairs:?}"
    );
}
