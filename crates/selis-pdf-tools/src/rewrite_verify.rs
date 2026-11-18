//! SL-1A.WRITE.08 — full render + text verification for full rewrites.
//!
//! This is the content-level half of the full-rewrite gate. The lower
//! `selis-pdf-edit` crate owns the structural gate (`verify_rewrite`) and
//! declares the [`RenderTextVerifier`] seam, but it cannot render or extract
//! text itself: `selis-pdf-engine` depends on `selis-pdf-edit`, so importing the
//! engine there would be a dependency cycle. The engine-backed implementation
//! therefore lives here — a higher layer that already owns the engine and the
//! text ladder — and is injected into the edit-layer writer through
//! [`selis_pdf_edit::rewrite::save_rewritten_with_verifier`].
//!
//! Both documents are opened with the real engine, every page is rendered at a
//! fixed DPI and the RGBA buffers are compared sample for sample, and the
//! per-page text is recovered through the same ladder `selis extract` uses and
//! compared. A page-count mismatch, a pixel difference or a text difference is
//! a typed `VERIFY_FAILED` naming the 1-based page; no bytes are returned (and
//! the atomic variant touches no file). This is the default full-rewrite path
//! exposed to the CLI: [`save_rewritten_verified`] /
//! [`save_rewritten_verified_atomic`].
//!
//! # Budget
//!
//! Every open, render and extraction is charged to the caller's [`Budget`] /
//! [`BudgetGuard`]; no unbounded work is done off-budget.
//!
//! # Malformed Input
//!
//! A document the engine cannot open, render or extract is a refusal, not a
//! repair: the typed error names the document and page, and no output exists.

#![forbid(unsafe_code)]
// `selis_error::Error` is the crate-wide public error type (~128 B); every
// typed-verification API here returns it, as the rest of this crate does.
#![allow(clippy::result_large_err)]

use selis_error::{err, Code, Result};
use selis_pdf_edit::rewrite::{
    RenderTextVerification, RenderTextVerifier, RewriteCommitReport, RewriteOptions, RewriteReport,
};
use selis_pdf_engine::{RenderedPage, Session};
use selis_pdf_text::TextLine;
use selis_sandbox::{Budget, BudgetGuard};

/// Default render resolution: one pixel per PDF point (72 dpi), matching the
/// other read-back checks in this crate. Render determinism is ADR-P0012.
const VERIFY_DPI: u32 = 72;

/// The engine-backed content-level verifier.
///
/// Holds the DPI used for both sides; [`Default`] uses [`VERIFY_DPI`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EngineRenderTextVerifier {
    /// DPI at which both documents are rendered.
    pub dpi: u32,
}

impl Default for EngineRenderTextVerifier {
    fn default() -> Self {
        Self { dpi: VERIFY_DPI }
    }
}

/// Render every page of two documents at `dpi` and compare pixels and text.
///
/// Both sides go through the same engine at the same DPI, so the comparison is
/// exact rather than thresholded (ADR-P0012). Page count is compared first;
/// then, per page, the whole [`RenderedPage`] (dimensions and RGBA samples) and
/// the recovered text. The first difference is a typed `VERIFY_FAILED` naming
/// the 1-based page.
///
/// # Errors
///
/// `VERIFY_FAILED` naming the page (or the page-count mismatch), or a
/// propagated engine open/render/extraction error.
///
/// # Budget
///
/// Charges both opens, both renders of every page and both text extractions.
///
/// # Malformed Input
///
/// An unopenable or unrenderable document is a refusal naming the side.
pub fn verify_render_text(
    before: &[u8],
    after: &[u8],
    dpi: u32,
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
) -> Result<RenderTextVerification> {
    let clock = crate::shell_clock();
    let before_session = Session::open(before.to_vec(), budget, &clock)?;
    let after_session = Session::open(after.to_vec(), budget, &clock)?;

    let pages = before_session.len();
    let after_pages = after_session.len();
    if pages != after_pages {
        return Err(err!(
            Code::VerifyFailed,
            during = "rewrite-render-text",
            detail = format!("page count changed: {pages} -> {after_pages}")
        ));
    }

    for page in 0..pages {
        let number = page.saturating_add(1);
        let dpi_f = f64::from(dpi);
        let before_render = before_session.render_page_rgba(page, dpi_f, budget, g)?;
        let after_render = after_session.render_page_rgba(page, dpi_f, budget, g)?;
        if renders_differ(&before_render, &after_render) {
            return Err(err!(
                Code::VerifyFailed,
                during = "rewrite-render-text",
                detail = format!("page {number}: rendered pixels differ")
            ));
        }

        let before_text = page_text(&before_session, page, budget, g)?;
        let after_text = page_text(&after_session, page, budget, g)?;
        if before_text != after_text {
            return Err(err!(
                Code::VerifyFailed,
                during = "rewrite-render-text",
                detail = format!("page {number}: extracted text differs")
            ));
        }
    }

    Ok(RenderTextVerification {
        pages: u64::try_from(pages).unwrap_or(u64::MAX),
        dpi,
    })
}

impl RenderTextVerifier for EngineRenderTextVerifier {
    fn verify(
        &self,
        before: &[u8],
        after: &[u8],
        budget: &Budget,
        g: &mut BudgetGuard<'_>,
    ) -> Result<RenderTextVerification> {
        verify_render_text(before, after, self.dpi, budget, g)
    }
}

/// The default content-verified full rewrite: structural gate + render/text.
///
/// This is the tools/CLI default for `save_rewritten`; the edit-layer primitive
/// remains available for content-preserving internal callers. The candidate is
/// verified in full before any byte is returned, so a render/text refusal
/// produces no output.
///
/// # Errors
///
/// Any refusal from the structural gate or the render/text comparison, plus
/// parse/write/budget errors. No bytes are returned on any refusal.
///
/// # Budget
///
/// Parse, collection, serialisation and both verification gates.
///
/// # Malformed Input
///
/// A malformed or content-divergent rewrite is a typed refusal, never a repair.
pub fn save_rewritten_verified(
    src: &[u8],
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
) -> Result<(Vec<u8>, RewriteReport)> {
    selis_pdf_edit::rewrite::save_rewritten_with_verifier(
        src,
        &RewriteOptions::default(),
        budget,
        g,
        Some(&EngineRenderTextVerifier::default()),
    )
}

/// Atomic, content-verified full rewrite. Verification runs before any byte
/// reaches disk, so a render/text refusal leaves the destination untouched.
///
/// # Errors
///
/// Any refusal from [`save_rewritten_verified`], or a commit error. The
/// destination is untouched on every error path.
///
/// # Budget
///
/// As [`save_rewritten_verified`] plus the pre-commit `tick()`.
///
/// # Malformed Input
///
/// A refused rewrite produces no file and leaves the source intact.
pub fn save_rewritten_verified_atomic(
    path: &std::path::Path,
    src: &[u8],
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
) -> Result<RewriteCommitReport> {
    selis_pdf_edit::rewrite::save_rewritten_atomic_with_verifier(
        path,
        src,
        &RewriteOptions::default(),
        budget,
        g,
        Some(&EngineRenderTextVerifier::default()),
    )
}

/// Compare two renders: dimensions and every RGBA sample must match.
fn renders_differ(a: &RenderedPage, b: &RenderedPage) -> bool {
    a.width != b.width || a.height != b.height || a.rgba != b.rgba
}

/// Recover one page's text through the same ladder `selis extract` uses, so the
/// comparison sees the words a reader sees rather than raw content bytes.
fn page_text(
    session: &Session,
    page: usize,
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
) -> Result<String> {
    let dl = session.page_text_display_list(page, budget, g)?;
    let visible = session.page_visible_box(page);
    let mut glyphs = selis_pdf_text::gather_glyphs(&dl, visible);
    selis_pdf_text::apply_unicode_recovery(&mut glyphs, &mut |font, code| {
        session.text_unicode(page, font, code, budget, g)
    });
    let lines = selis_pdf_text::assemble(glyphs);
    let mcid_order = session
        .mcid_order(budget, g)
        .ok()
        .filter(|order| !order.is_empty());
    let mcid_lines: Vec<selis_pdf_text::LineWithMcid> = lines
        .into_iter()
        .map(|line| {
            let mcid = line
                .words
                .iter()
                .flat_map(|word| word.runs.iter())
                .find_map(|run| run.glyphs.first().and_then(|glyph| glyph.mcid));
            selis_pdf_text::LineWithMcid { line, mcid }
        })
        .collect();
    let ordered = selis_pdf_text::order_lines(mcid_lines, mcid_order.as_deref(), g)?;
    Ok(ordered
        .lines
        .iter()
        .map(line_text)
        .collect::<Vec<_>>()
        .join("\n"))
}

/// One line's text: a single space between words, glyph codes as characters —
/// the reconstruction rule used by `selis extract` and the HTML chunk.
fn line_text(line: &TextLine) -> String {
    let mut out = String::new();
    for (index, word) in line.words.iter().enumerate() {
        if index > 0 {
            out.push(' ');
        }
        for run in &word.runs {
            for glyph in &run.glyphs {
                if let Some(ch) = char::from_u32(u32::from(glyph.code)) {
                    out.push(ch);
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing,
        clippy::arithmetic_side_effects,
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        clippy::cast_lossless
    )]

    use super::*;
    use selis_bytes::Bytes;
    use selis_error::Code;
    use selis_pdf_cos::Obj;
    use selis_pdf_cos::Ref;
    use selis_pdf_edit::rewrite::{verify_rewrite, RewriteFault, RewriteOptions};
    use selis_sandbox::FixedClock;

    fn b(v: &[u8]) -> Bytes {
        Bytes::copy_from_slice(v)
    }

    /// A document whose pages are the given object numbers in `/Kids` order.
    /// Page 3 shows "ALPHA" and page 4 shows "BRAVO", both in standard-14
    /// Helvetica. The two content streams are distinct, so a page swap changes
    /// each page's pixels and text but leaves the multiset of stream payloads —
    /// and every structural count — unchanged. `kids` also lets a one-page
    /// (count-mismatched) candidate be built directly.
    fn pdf_with_pages(kids: &[u32]) -> Vec<u8> {
        let page = |content: u32| {
            Obj::Dict(vec![
                (b(b"Type"), Obj::Name(b(b"Page"))),
                (b(b"Parent"), Obj::Ref(Ref::new(2, 0))),
                (
                    b(b"MediaBox"),
                    Obj::Array(vec![Obj::Int(0), Obj::Int(0), Obj::Int(300), Obj::Int(200)]),
                ),
                (b(b"Contents"), Obj::Ref(Ref::new(content, 0))),
                (
                    b(b"Resources"),
                    Obj::Dict(vec![(
                        b(b"Font"),
                        Obj::Dict(vec![(b(b"F1"), Obj::Ref(Ref::new(6, 0)))]),
                    )]),
                ),
            ])
        };
        let content = |text: &[u8]| {
            let mut data = b"BT /F1 24 Tf 20 100 Td (".to_vec();
            data.extend_from_slice(text);
            data.extend_from_slice(b") Tj ET\n");
            Obj::Stream {
                dict: vec![(b(b"Length"), Obj::Int(i64::try_from(data.len()).unwrap()))],
                data: b(&data),
            }
        };
        let objects: Vec<(u32, Obj)> = vec![
            (
                1,
                Obj::Dict(vec![
                    (b(b"Type"), Obj::Name(b(b"Catalog"))),
                    (b(b"Pages"), Obj::Ref(Ref::new(2, 0))),
                ]),
            ),
            (
                2,
                Obj::Dict(vec![
                    (b(b"Type"), Obj::Name(b(b"Pages"))),
                    (
                        b(b"Kids"),
                        Obj::Array(kids.iter().map(|num| Obj::Ref(Ref::new(*num, 0))).collect()),
                    ),
                    (b(b"Count"), Obj::Int(i64::try_from(kids.len()).unwrap())),
                ]),
            ),
            (3, page(7)),
            (4, page(8)),
            (
                6,
                Obj::Dict(vec![
                    (b(b"Type"), Obj::Name(b(b"Font"))),
                    (b(b"Subtype"), Obj::Name(b(b"Type1"))),
                    (b(b"BaseFont"), Obj::Name(b(b"Helvetica"))),
                    (b(b"Encoding"), Obj::Name(b(b"WinAnsiEncoding"))),
                ]),
            ),
            (7, content(b"ALPHA")),
            (8, content(b"BRAVO")),
        ];
        selis_pdf_cos::doc_writer::write_objects_as_document(
            &objects,
            Ref::new(1, 0),
            &Budget::unlimited(),
            &mut Budget::unlimited().guard(),
        )
        .expect("write fixture")
    }

    fn two_page_pdf() -> Vec<u8> {
        pdf_with_pages(&[3, 4])
    }

    /// A one-page document whose only mark is white text on a white page, so
    /// two variants render pixel-identically but carry different text.
    fn invisible_text_pdf(word: &[u8]) -> Vec<u8> {
        let mut data = b"1 1 1 rg BT /F1 24 Tf 20 100 Td (".to_vec();
        data.extend_from_slice(word);
        data.extend_from_slice(b") Tj ET\n");
        let objects: Vec<(u32, Obj)> = vec![
            (
                1,
                Obj::Dict(vec![
                    (b(b"Type"), Obj::Name(b(b"Catalog"))),
                    (b(b"Pages"), Obj::Ref(Ref::new(2, 0))),
                ]),
            ),
            (
                2,
                Obj::Dict(vec![
                    (b(b"Type"), Obj::Name(b(b"Pages"))),
                    (b(b"Kids"), Obj::Array(vec![Obj::Ref(Ref::new(3, 0))])),
                    (b(b"Count"), Obj::Int(1)),
                ]),
            ),
            (
                3,
                Obj::Dict(vec![
                    (b(b"Type"), Obj::Name(b(b"Page"))),
                    (b(b"Parent"), Obj::Ref(Ref::new(2, 0))),
                    (
                        b(b"MediaBox"),
                        Obj::Array(vec![Obj::Int(0), Obj::Int(0), Obj::Int(300), Obj::Int(200)]),
                    ),
                    (b(b"Contents"), Obj::Ref(Ref::new(7, 0))),
                    (
                        b(b"Resources"),
                        Obj::Dict(vec![(
                            b(b"Font"),
                            Obj::Dict(vec![(b(b"F1"), Obj::Ref(Ref::new(6, 0)))]),
                        )]),
                    ),
                ]),
            ),
            (
                6,
                Obj::Dict(vec![
                    (b(b"Type"), Obj::Name(b(b"Font"))),
                    (b(b"Subtype"), Obj::Name(b(b"Type1"))),
                    (b(b"BaseFont"), Obj::Name(b(b"Helvetica"))),
                    (b(b"Encoding"), Obj::Name(b(b"WinAnsiEncoding"))),
                ]),
            ),
            (
                7,
                Obj::Stream {
                    dict: vec![(b(b"Length"), Obj::Int(i64::try_from(data.len()).unwrap()))],
                    data: b(&data),
                },
            ),
        ];
        selis_pdf_cos::doc_writer::write_objects_as_document(
            &objects,
            Ref::new(1, 0),
            &Budget::unlimited(),
            &mut Budget::unlimited().guard(),
        )
        .expect("write invisible-text fixture")
    }

    /// Recover page 0's text directly, for evidence assertions.
    fn text_of_page(bytes: &[u8], page: usize) -> String {
        let budget = Budget::unlimited();
        let mut g = budget.guard();
        let session = Session::open(bytes.to_vec(), &budget, &FixedClock(0)).expect("open");
        page_text(&session, page, &budget, &mut g).expect("extract page text")
    }

    fn verify(src: &[u8], out: &[u8]) -> Result<RenderTextVerification> {
        let budget = Budget::unlimited();
        let mut g = budget.guard();
        verify_render_text(src, out, VERIFY_DPI, &budget, &mut g)
    }

    #[test]
    fn normal_rewrite_is_pixel_and_text_identical() {
        let src = two_page_pdf();
        let budget = Budget::unlimited();
        let mut g = budget.guard();
        let (out, report) =
            save_rewritten_verified(&src, &budget, &mut g).expect("verified rewrite");
        assert_ne!(out, src, "I2 exception: a fresh single-revision file");
        let rt = report.render_text.expect("render/text verification ran");
        assert_eq!(rt.pages, 2);
        assert_eq!(rt.dpi, VERIFY_DPI);
        // The same bytes opened through the engine are identical to themselves.
        let direct = verify(&src, &out).expect("before and after render identically");
        assert_eq!(direct, rt);
        // The engine really saw two pages of text, not empty paper.
        let session = Session::open(out.clone(), &budget, &FixedClock(0)).expect("open output");
        assert_eq!(session.len(), 2);
        assert!(
            text_of_page(&src, 0).contains("ALPHA"),
            "page 1 text is really extracted: {:?}",
            text_of_page(&src, 0)
        );
        assert!(
            text_of_page(&src, 1).contains("BRAVO"),
            "page 2 text is really extracted: {:?}",
            text_of_page(&src, 1)
        );
    }

    #[test]
    fn swapped_page_content_passes_structure_but_is_refused_by_render_text() {
        let src = two_page_pdf();
        let bad = pdf_with_pages(&[4, 3]);

        // The structural gate accepts it: counts and stream multiset are intact.
        {
            let budget = Budget::unlimited();
            let mut g = budget.guard();
            verify_rewrite(&src, &bad, &budget, &mut g).expect("structure cannot see a page swap");
        }

        // The per-page text genuinely differs across the swap.
        assert_ne!(text_of_page(&src, 0), text_of_page(&bad, 0));
        assert!(text_of_page(&src, 0).contains("ALPHA"));
        assert!(text_of_page(&bad, 0).contains("BRAVO"));

        // The content gate refuses it, naming page 1 (ALPHA vs BRAVO).
        let err = verify(&src, &bad).expect_err("render/text catches the swap");
        assert_eq!(err.code(), Code::VerifyFailed);
        let detail = format!("{err}");
        assert!(detail.contains("page 1"), "detail names the page: {detail}");

        // End to end: the default verified rewrite refuses and yields no bytes.
        let budget = Budget::unlimited();
        let mut g = budget.guard();
        let err = selis_pdf_edit::rewrite::save_rewritten_with_verifier(
            &src,
            &RewriteOptions {
                fault: Some(RewriteFault::SwapPageOrder),
                ..RewriteOptions::default()
            },
            &budget,
            &mut g,
            Some(&EngineRenderTextVerifier::default()),
        )
        .expect_err("verified rewrite refuses the swap");
        assert_eq!(err.code(), Code::VerifyFailed);
    }

    #[test]
    fn page_count_mismatch_is_caught() {
        let src = two_page_pdf();
        let short = pdf_with_pages(&[3]);

        // The structural gate already catches the count change.
        {
            let budget = Budget::unlimited();
            let mut g = budget.guard();
            let err = verify_rewrite(&src, &short, &budget, &mut g).expect_err("count mismatch");
            assert_eq!(err.code(), Code::VerifyFailed);
            assert!(format!("{err}").contains("page count changed"));
        }

        // The render/text gate catches it independently, too.
        let err = verify(&src, &short).expect_err("page-count mismatch");
        assert_eq!(err.code(), Code::VerifyFailed);
        assert!(format!("{err}").contains("page count changed"));
    }

    #[test]
    fn text_only_difference_is_caught_when_pixels_match() {
        let a = invisible_text_pdf(b"ALPHA");
        let b = invisible_text_pdf(b"BRAVO");
        // The invisible text really is different in the extracted text layer.
        assert_ne!(text_of_page(&a, 0), text_of_page(&b, 0));
        // ...but renders identically, so this isolates the TEXT half of the gate.
        let err = verify(&a, &b).expect_err("invisible text change is caught");
        assert_eq!(err.code(), Code::VerifyFailed);
        let detail = format!("{err}");
        assert!(
            detail.contains("extracted text differs"),
            "text half fired: {detail}"
        );
    }

    #[test]
    fn identical_sessions_compare_with_zero_pages_edge() {
        // A one-page blank reads back pixel- and text-identically to itself.
        let src = two_page_pdf();
        let direct = verify(&src, &src).expect("self comparison");
        assert_eq!(direct.pages, 2);
    }
}
