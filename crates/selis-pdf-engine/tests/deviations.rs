//! SL-4.UI.09 groundwork: the deviations an open document tolerated.
//!
//! The health panel has to be able to say what the parser worked around, and the
//! honest version of that sentence is only available if the list survives the
//! open AND means something. It survives via `Session::deviations`, which runs
//! `selis_pdf_cos::object_deviations` - object syntax only, stream bodies
//! skipped. See that function for why a whole-file lex is the wrong instrument.
//!
//! Every leg here is written to fail if the pass stops reporting, because a
//! `deviations()` that always returns an empty vec is indistinguishable from a
//! clean document - and "this document has no deviations" is exactly what the
//! panel would tell a user.
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

use selis_pdf_engine::Session;
use selis_sandbox::{Budget, CancelToken, FixedClock, Surface};

// Selis-authored synthetic fixtures, committed so this does not depend on the
// fetched-corpus cache. Bytes are embedded at compile time: L0-L3 crates must
// stay filesystem-free (xtask check-purity), even in their tests.

/// An ordinary, clean document WITH TEXT - two pages, embedded fonts, real
/// content streams.
static WITH_FONTS: &[u8] = include_bytes!("../../../corpus/fixtures/form_two_pages.pdf");

/// A minimal, well-formed one-page document, byte-exact xref, no streams of
/// consequence.
static MINIMAL: &[u8] = b"%PDF-1.7
1 0 obj
<< /Type /Catalog /Pages 2 0 R >>
endobj
2 0 obj
<< /Type /Pages /Kids [3 0 R] /Count 1 >>
endobj
3 0 obj
<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << >> /Contents 4 0 R >>
endobj
4 0 obj
<< /Length 44 >>
stream
0 0 1 rg 10 10 100 100 re f
endstream
endobj
xref
0 5
0000000000 65535 f 
0000000009 00000 n 
0000000058 00000 n 
0000000115 00000 n 
0000000222 00000 n 
trailer
<< /Size 5 /Root 1 0 R >>
startxref
439
%%EOF
";

fn session(src: &[u8]) -> Session {
    let budget = Budget::profile(Surface::Viewer);
    Session::open(src.to_vec(), &budget, &FixedClock(0)).expect("Session::open")
}

/// The names the engine reports for `src`.
fn names(src: &[u8]) -> Vec<String> {
    let budget = Budget::profile(Surface::Viewer);
    let mut g = budget.guard_with(&FixedClock(0), CancelToken::new());
    session(src)
        .deviations(&mut g)
        .iter()
        .map(|d| d.name().to_owned())
        .collect()
}

/// Replace the first occurrence of `needle` with `with`, asserting both are the
/// same length so every xref offset in the fixture still resolves.
fn splice_length_preserving(src: &[u8], needle: &[u8], with: &[u8]) -> Vec<u8> {
    assert_eq!(
        needle.len(),
        with.len(),
        "the fixture edit must not move any byte, or the xref stops resolving"
    );
    let mut out = src.to_vec();
    let at = out
        .windows(needle.len())
        .position(|w| w == needle)
        .expect("the fixture contains the needle");
    out.splice(at..at + needle.len(), with.iter().copied());
    out
}

/// THE headline leg, and the one the design exists for.
///
/// A clean, ordinary, TEXT-BEARING document must report nothing.
///
/// Before the stream-skipping pass this reported **70+ deviations** - 68
/// `invalid-hex-digit` plus a tail of string, paren and delimiter errors - every
/// one of them a byte of an embedded font program being read as COS syntax. A
/// health panel built on that number would have called a perfectly good
/// document broken.
///
/// Without this leg the skip can silently stop working and nothing here notices:
/// the flood returns, and only this assertion is standing between that and a
/// panel that cries wolf on every real PDF.
#[test]
fn a_clean_document_with_embedded_fonts_reports_no_deviations() {
    assert_eq!(names(WITH_FONTS), Vec::<String>::new());
}

/// The synthetic control for the same property, so the headline leg cannot pass
/// merely because `WITH_FONTS` happens to be small.
#[test]
fn a_minimal_clean_document_reports_no_deviations() {
    assert_eq!(names(MINIMAL), Vec::<String>::new());
}

/// The POSITIVE leg: a real defect in **object syntax** must be named.
///
/// The edit swaps the MediaBox's `612 792` for `<ABCDE>` - five hex digits, an
/// odd count, so the last nibble is zero-padded and `OddLengthHex` is recorded.
/// It is length-preserving, so the document still opens, and it is in an OBJECT,
/// not a stream, which is the whole point: a defect inside a stream body is
/// opaque data and is deliberately not reported.
///
/// An earlier version of this leg put the same defect inside a content stream
/// and it stopped being reported. That was the fix working, not a regression -
/// but it is exactly why the leg now states WHERE the defect is.
#[test]
fn a_tolerated_defect_in_object_syntax_is_named() {
    let damaged = splice_length_preserving(MINIMAL, b"612 792", b"<ABCDE>");
    let found = names(&damaged);
    assert!(
        found.iter().any(|n| n == "odd-length-hex"),
        "expected odd-length-hex, got {found:?}"
    );
}

/// The mirror of that leg: a defect INSIDE a stream body must NOT be named.
///
/// Same fixture, same edit position, moved into the content stream. The document
/// is equally damaged as far as a reader is concerned, and the COS layer has no
/// opinion about stream content - so reporting it would be reporting font data
/// and page content as syntax.
///
/// Together with the previous leg this pins BOTH directions: object syntax is
/// reported, stream bodies are not. Either half alone would pass with a
/// `deviations()` that always returned everything, or always returned nothing.
#[test]
fn a_defect_inside_a_stream_body_is_not_reported() {
    let damaged = splice_length_preserving(MINIMAL, b"10 10 1", b"<ABCDE>");
    assert_eq!(
        names(&damaged),
        Vec::<String>::new(),
        "a stream body is opaque data; its bytes are not object syntax"
    );
}

/// Deviations are deduplicated by (name, offset), so one defect is counted once.
#[test]
fn deviations_are_deduplicated_by_name_and_offset() {
    let damaged = splice_length_preserving(MINIMAL, b"612 792", b"<ABCDE>");
    let budget = Budget::profile(Surface::Viewer);
    let mut g = budget.guard_with(&FixedClock(0), CancelToken::new());
    let all = session(&damaged).deviations(&mut g);
    let mut keys: Vec<(&str, u64)> = all.iter().map(|d| (d.name(), d.offset())).collect();
    let before = keys.len();
    keys.sort_unstable();
    keys.dedup();
    assert_eq!(
        before,
        keys.len(),
        "the same deviation at the same offset must not be counted twice"
    );
}

/// The pass is bounded by the caller's budget, and RETURNS either way.
///
/// The panel asks for a report; a hostile file must not turn that into an
/// unbounded read. The assertion is that the call returns at all - whether it
/// completed or stopped early is the budget's business, not this test's.
#[test]
fn the_pass_is_bounded_by_the_caller_budget() {
    let session = session(WITH_FONTS);
    let budget = Budget::profile(Surface::Viewer);
    let mut g = budget.guard_with(&FixedClock(0), CancelToken::new());
    let _ = session.deviations(&mut g);
}
