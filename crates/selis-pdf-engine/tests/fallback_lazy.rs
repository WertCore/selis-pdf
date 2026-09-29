//! A standard-14 document with **no embedded font program** renders from a
//! lazily-delivered Liberation face (SL-3.FONT.12).
//!
//! This is the class of document LaTeX, ReportLab and most hand-rolled
//! generators emit: the content stream says `/Helvetica 48 Tf` and the
//! `/Resources` declares no `/FontFile`. Before this work every viewer shipped
//! all twelve faces, so it rendered. A web build that embeds only the Serif
//! subset renders it as **nothing at all** unless the face arrives — which is
//! the whole reason the delivery seam exists.
//!
//! The two properties pinned here are the ones that are easy to get wrong:
//!
//! 1. absent face → the render reports it in `needs`, and does not silently
//!    conclude "this document has no substitute";
//! 2. delivered face → the render paints, and `needs` is empty.
//!
//! Pixels are checked as "did anything get inked", not against exact
//! coordinates: the question is whether the text drew at all, and an exact
//! comparison would be a second, weaker copy of the MuPDF-calibrated geometry
//! test that already guards the desktop path.

// Same scope and same reasoning as the sibling `cjk_lazy.rs`: these tests build
// their own resident set and assert on the render outcome, so an `expect` here
// is the assertion, not a crash primitive reached from untrusted input. The
// engine's production paths keep every deny lint.
#![allow(clippy::expect_used, clippy::indexing_slicing, clippy::panic)]

use selis_geom::Matrix;
use selis_pdf_engine::{Session, TinySkiaBackend};
use selis_sandbox::{Budget, CancelToken, FixedClock};

/// A real one-page document that sets `/Helvetica` with no embedded font
/// program — borrowed from the CLI's own fixtures rather than duplicated, so
/// there is one copy of this class of file in the repo.
///
/// It is the same fixture `apps/cli/tests/text_matrix_layout.rs` renders and
/// checks against MuPDF, which makes it a useful shared reference: the desktop
/// path is pinned to another engine's exact pixel geometry there, and this test
/// pins that the web path draws the same text at all.
const HELVETICA_NO_EMBED: &[u8] =
    include_bytes!("../../../apps/cli/tests/fixtures/text11_tm_scaled.pdf");

fn open() -> Session {
    let budget = Budget::profile(selis_sandbox::Surface::Viewer);
    Session::open(HELVETICA_NO_EMBED.to_vec(), &budget, &FixedClock(0)).expect("open")
}

fn inked(backend: &TinySkiaBackend) -> usize {
    backend
        .pixmap()
        .data()
        .chunks(4)
        .filter(|px| px[0] < 128)
        .count()
}

#[cfg(not(feature = "builtin-fallback-fonts"))]
fn render(session: &Session) -> (usize, Vec<&'static str>) {
    let budget = Budget::profile(selis_sandbox::Surface::Viewer);
    let mut g = budget.guard_with(&FixedClock(0), CancelToken::new());
    let mut backend = TinySkiaBackend::new(612, 792).expect("pixmap");
    let mut set = selis_font::fallback_set::FallbackFontSet::new();
    let outcome = session
        .render_page_fallbacks(0, &mut backend, Matrix::IDENTITY, &budget, &mut g, &mut set)
        .expect("render");
    (inked(&backend), outcome.needs)
}

/// With no face delivered, the render says which face it wanted instead of
/// quietly painting nothing. This is the assertion that distinguishes "this
/// document has no standard-14 font" from "its font has not arrived" — the
/// confusion that would otherwise make a web viewer drop text silently.
///
/// **Gated on the feature, and the gate is load-bearing.** `apps/cli` requests
/// `builtin-fallback-fonts`, and Cargo unifies features across a whole
/// `cargo test --workspace` build — so in that invocation *every* test binary,
/// this one included, links the full twelve-face set and no face is ever
/// missing. The scenario is only reachable in the web configuration, which is
/// `cargo test -p selis-pdf-engine` (the feature is off by default there).
///
/// Both invocations are therefore part of the gate, and CI runs both. The
/// alternative — removing the gate to make a workspace run green — would
/// delete the only test of the delivery seam.
#[cfg(not(feature = "builtin-fallback-fonts"))]
#[test]
fn an_absent_face_is_reported_not_silently_blank() {
    let session = open();
    let (ink, needs) = render(&session);
    assert!(
        needs.contains(&"LiberationSans-Regular"),
        "a /Helvetica document must report the Sans face it needs, got {needs:?}"
    );
    assert_eq!(ink, 0, "nothing can be painted before the face arrives");
}

/// Once the face is installed, the same document paints and reports nothing
/// further. Same session, same document — only the resident set differs, which
/// is the entire difference between the two calls.
#[test]
fn a_delivered_face_paints_and_needs_nothing() {
    let session = open();
    let budget = Budget::profile(selis_sandbox::Surface::Viewer);
    let mut g = budget.guard_with(&FixedClock(0), CancelToken::new());
    let mut backend = TinySkiaBackend::new(612, 792).expect("pixmap");
    let mut set = selis_font::fallback_set::FallbackFontSet::new();
    set.install(
        "LiberationSans-Regular",
        selis_bytes::Bytes::from(
            include_bytes!("../../../assets/fonts/LiberationSans-Regular.ttf").to_vec(),
        ),
    );
    let outcome = session
        .render_page_fallbacks(0, &mut backend, Matrix::IDENTITY, &budget, &mut g, &mut set)
        .expect("render");
    assert!(
        outcome.needs.is_empty(),
        "nothing should still be missing, got {:?}",
        outcome.needs
    );
    assert!(
        inked(&backend) > 20,
        "the substituted text should paint glyph pixels, got {}",
        inked(&backend)
    );
}
