//! SL-3.TEXT.26 regression pin: text extraction must respect the page's
//! visible region, and a Form XObject's text must be reported in *page* space.
//!
//! The evidence file is `issue7454` (pdf.js corpus), filed out of the CONF.03
//! `char-explosion` cluster: a 1-page PDFium annotation-appearance fragment
//! whose page box is 384×111 pt and whose entire content stream is
//!
//! ```text
//! 10 0 0 10 -19.0089 -623.9184 cm / 0.1 0 0 0.1 0 0 cm /QQAPXO2a2908c2 Do
//! ```
//!
//! — a full **A4** Form XObject (`/BBox [0 0 595.28 841.89]`) placed at net
//! scale 1.0 with a translate of (-19.01, -623.92), so only a thin band of the
//! sheet lands inside the page. Measured on the real file, 2026-09-27:
//! selis = pypdf = pdfminer.six = 3 448 characters, MuPDF = pdf.js = 217.
//! The renderers clip; the content-stream walkers (selis among them) emit the
//! whole sheet — and selis additionally reported the positions in Form space,
//! untransformed, so nothing downstream could even tell the text was off the
//! page. Invisible text was offered for search, selection, and copy.
//!
//! The pinned fixture reproduces the shape with two pages:
//!
//! * **page 0** — the `issue7454` placement exactly (net scale 1.0, translate
//!   (-19.0089, -623.9184)): three lines on the A4 sheet at form y 800 / 700 /
//!   400, of which only y=700 (page y ≈ 76.08) is on the 111 pt page.
//! * **page 1** — a *scaled* placement (net 2×, no translation) so the pen
//!   step, the space width, and the em are exercised under a scale, with one
//!   line on the page and one off it.
//!
//! MuPDF 1.23.0 (`mutool draw -F txt`) returns `ON-THE-PAGE-BAND` for page 0 and
//! `SCALED-ON-PAGE` for page 1 — the two strings these tests require and no
//! others. Agreement with pypdf/pdfminer.six is *not* evidence here: neither
//! renders, and this task exists because selis shared their blind spot.
//!
//! ```text
//! cargo test -p selis-cli --test text26_form_placement
//! ```
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::PathBuf;
use std::process::Command;

/// The `selis` binary under test. In CI `cargo test` builds it into a private
/// target dir and `CARGO_BIN_EXE_selis` is authoritative. Locally the target
/// dir is SHARED with concurrent agents, which can overwrite the exe between
/// build and run; set `SELIS_TEST_BIN` to a freshly snapshotted copy in that
/// case (the same bytes, immune to clobber).
fn selis_bin() -> String {
    match std::env::var("SELIS_TEST_BIN") {
        Ok(p) if !p.is_empty() => p,
        _ => env!("CARGO_BIN_EXE_selis").to_string(),
    }
}

const FIXTURE: &[u8] = include_bytes!("fixtures/text26_form_placement.pdf");

/// Drop the fixture into the temp dir and hand back its path. The bytes come
/// from the repo (pinned), the write is test-local.
fn write_fixture() -> PathBuf {
    let mut path = std::env::temp_dir();
    path.push("selis-cli-text26-form-placement.pdf");
    std::fs::write(&path, FIXTURE).expect("write fixture");
    path
}

fn extract(path: &PathBuf, page: usize, format: &str) -> (i32, String, String) {
    let out = Command::new(selis_bin())
        .args(["extract", "--format", format, "--page", &page.to_string()])
        .arg(path)
        .output()
        .expect("run selis extract");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn search(path: &PathBuf, page: usize, query: &str) -> (i32, String, String) {
    let out = Command::new(selis_bin())
        .args(["search", "--page", &page.to_string()])
        .arg(path)
        .arg(query)
        .output()
        .expect("run selis search");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// The DoD's headline: only the on-page band is extracted, and it is the exact
/// string MuPDF returns. Before the fix all three lines came back — the two
/// clipped ones included, at their untransformed Form-space positions.
#[test]
fn text26_only_the_on_page_band_of_the_placed_form_is_extracted() {
    let path = write_fixture();
    let (code, text, err) = extract(&path, 0, "text");
    assert_eq!(code, 0, "extract failed: {err}");
    assert_eq!(
        text.trim_end_matches('\n'),
        "ON-THE-PAGE-BAND",
        "the A4 sheet's off-page lines must not be extracted"
    );
    assert!(
        !text.contains("low-confidence"),
        "the band is real recovered text, not a marker: {text}"
    );
}

/// The other half of the Do: the reported geometry is **page** space, not Form
/// space. The `issue7454` line's origin moves from Form y=700 to page
/// y = 700 − 623.9184 = 76.0816, and x from 72 to 72 − 19.0089 = 52.9911. Before
/// the fix the span read `[72.00, 700.00, …]` — coordinates in a coordinate
/// system the page does not have, which is why no visibility test could exist.
#[test]
fn text26_form_text_is_reported_in_page_space() {
    let path = write_fixture();
    let (code, json, err) = extract(&path, 0, "json");
    assert_eq!(code, 0, "extract json failed: {err}");
    assert!(
        json.contains("ON-THE-PAGE-BAND"),
        "the on-page band must be in the JSON: {json}"
    );
    assert!(
        json.contains("[52.99, 76.08,"),
        "the span is placed by the form's CTM (52.99, 76.08), not Form space: {json}"
    );
    assert!(
        !json.contains("700.00"),
        "no Form-space y may survive into the reported geometry: {json}"
    );
}

/// A *scaled* placement: the same rule, and the pen step and em ride the scale
/// (a form drawn at 2× has 2× glyphs). MuPDF returns `SCALED-ON-PAGE` here too.
#[test]
fn text26_scaled_form_placement_is_placed_and_clipped() {
    let path = write_fixture();
    let (code, text, err) = extract(&path, 1, "text");
    assert_eq!(code, 0, "extract failed: {err}");
    assert_eq!(
        text.trim_end_matches('\n'),
        "SCALED-ON-PAGE",
        "the 2x-placed form's off-page line must not be extracted"
    );
}

/// The DoD's "search and selection never surface an off-page span": the clipped
/// lines are not searchable either. Before the fix `selis search` reported
/// matches for all three lines, with the off-page ones' rects in Form space.
#[test]
fn text26_search_never_surfaces_an_off_page_span() {
    let path = write_fixture();
    for page in [0usize, 1usize] {
        for hidden in ["ABOVE-THE-PAGE", "BELOW-THE-PAGE", "SCALED-OFF-PAGE"] {
            let (code, out, err) = search(&path, page, hidden);
            assert_eq!(code, 0, "search failed: {err}");
            assert!(
                !out.to_uppercase().contains(hidden),
                "page {page}: clipped text {hidden} must not be searchable: {out}"
            );
        }
    }
    // The on-page band is still findable, with a page-space rect. The CLI
    // prints rects to one decimal, so the placed origin reads 53.0, 76.1.
    let (code, out, err) = search(&path, 0, "ON-THE-PAGE-BAND");
    assert_eq!(code, 0, "search failed: {err}");
    assert!(
        out.contains("(53.0,76.1,"),
        "the on-page hit keeps its page-space rect: {out}"
    );
}

/// The DoD's "the existing selection/copy behaviour is unchanged for ordinary
/// pages": a page whose content is not placed by a transform is untouched.
/// `smoke.pdf` is the render+text-oracle corpus fixture whose headline
/// extraction is the TEXT.09 DoD string, and it has no placement transform, so
/// its geometry and text are exactly what the pre-TEXT.26 extractor reported.
#[test]
fn text26_an_unplaced_page_is_unchanged() {
    let smoke = std::env::temp_dir().join("selis-cli-text26-smoke.pdf");
    std::fs::write(
        &smoke,
        include_bytes!("../../../docker/oracles/fixtures/smoke.pdf"),
    )
    .expect("write smoke");
    let (code, text, err) = extract(&smoke, 0, "text");
    assert_eq!(code, 0, "extract failed: {err}");
    assert_eq!(text.trim_end_matches('\n'), "Selis oracle smoke test");
}
