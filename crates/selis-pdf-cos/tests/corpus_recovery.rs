//! Regression tests for the A3 parser-robustness corpus files.
//!
//! Both files open via the strict xref path after recovery — no input
//! rewriting, no reconstruction fallback:
//!
//! * `form_two_pages.pdf` — a 1.6 file whose xref *stream* is
//!   `/FlateDecode` + `/DecodeParms << /Predictor 12 >>`; the PNG predictor
//!   must be undone before the entry fields are read (previously the entries
//!   decoded to garbage and `/Root` did not resolve).
//! * `outlines_for_editor.pdf` — `startxref` points 8 bytes short of the
//!   `xref` keyword (at the `endobj` before it), and every object offset is
//!   off by 8 bytes (at the EOL before each `N G obj` header).
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

use selis_sandbox::Budget;

// These two files are Selis-authored synthetic fixtures (corpus/fixtures/):
// they exercise the strict-xref recovery paths and are committed so the test
// does not depend on the fetched-corpus cache (SL-0.LEGAL.04 keeps fetched
// third-party PDFs out of git; our own generated files may live in git).
static FORM_TWO_PAGES: &[u8] = include_bytes!("../../../corpus/fixtures/form_two_pages.pdf");
static OUTLINES_FOR_EDITOR: &[u8] =
    include_bytes!("../../../corpus/fixtures/outlines_for_editor.pdf");

fn open(src: &[u8]) -> selis_pdf_cos::Doc {
    let budget = Budget::unlimited();
    let mut g = budget.guard();
    let startxref = selis_pdf_cos::xref::find_startxref(src, 4096).expect("startxref");
    selis_pdf_cos::parse_revisions(src, startxref, &budget, &mut g).expect("parse revisions")
}

/// The xref stream's `/DecodeParms` PNG predictor is undone: all entries are
/// usable, and `/Root` (object 1) resolves to the catalog.
#[test]
fn form_two_pages_opens_and_root_resolves() {
    let src = FORM_TWO_PAGES;
    let doc = open(&src);
    let rev = doc.revisions().last().expect("one revision");
    assert_eq!(rev.root, Some(selis_pdf_cos::Ref::new(1, 0)));
    assert_eq!(
        rev.entries.len(),
        60,
        "/Size 60 entries, not a partial decode"
    );
    // No entry may point past the end of the file (garbage-decoded offsets do).
    let len = u64::try_from(src.len()).unwrap_or(u64::MAX);
    for (num, entry) in &rev.entries {
        if let selis_pdf_cos::XrefEntry::InUse { offset, .. } = entry {
            assert!(
                *offset < len,
                "object {num} offset {offset} is past end of file"
            );
        }
    }
    // /Root resolves and is the catalog.
    let budget = Budget::unlimited();
    let mut g = budget.guard();
    let root = selis_pdf_cos::copy::resolve_ref(
        &src,
        &doc,
        selis_pdf_cos::Ref::new(1, 0),
        &budget,
        &mut g,
    )
    .expect("resolve /Root");
    let selis_pdf_cos::Obj::Dict(pairs) = &root else {
        panic!("catalog is not a dict");
    };
    assert!(pairs.iter().any(|(k, v)| k.as_slice() == b"Type"
        && matches!(v, selis_pdf_cos::Obj::Name(n) if n.as_slice() == b"Catalog")));
    assert!(pairs.iter().any(|(k, _)| k.as_slice() == b"Pages"));
}

/// The mis-aimed `startxref` (points at the `endobj` before `xref`) recovers,
/// and the off-by-8 object offsets resolve to the right objects.
#[test]
fn outlines_for_editor_opens_and_root_resolves() {
    let src = OUTLINES_FOR_EDITOR;
    let doc = open(&src);
    let rev = doc.revisions().last().expect("one revision");
    assert_eq!(rev.root, Some(selis_pdf_cos::Ref::new(1, 0)));
    assert_eq!(rev.entries.len(), 35, "the full xref table, not a fallback");
    // The recovered revision end is the real table offset, not u64::MAX.
    assert!(
        rev.byte_range.end < u64::MAX,
        "revision end must be a real offset"
    );
    // /Root resolves and is the catalog, despite every offset being 8 short.
    let budget = Budget::unlimited();
    let mut g = budget.guard();
    let root = selis_pdf_cos::copy::resolve_ref(
        &src,
        &doc,
        selis_pdf_cos::Ref::new(1, 0),
        &budget,
        &mut g,
    )
    .expect("resolve /Root");
    let selis_pdf_cos::Obj::Dict(pairs) = &root else {
        panic!("catalog is not a dict");
    };
    assert!(pairs.iter().any(|(k, v)| k.as_slice() == b"Type"
        && matches!(v, selis_pdf_cos::Obj::Name(n) if n.as_slice() == b"Catalog")));
}
