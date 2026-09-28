//! Run, word, and line assembly (SL-3.TEXT.03, SL-3.TEXT.13).
//!
//! Groups the positioned glyphs from the content interpreter into:
//!
//! * **runs** — consecutive glyphs sharing a font and size;
//! * **words** — runs split at space characters (code 0x20) and at advance
//!   gaps exceeding half of the font's own space width (SL-3.TEXT.09);
//! * **lines** — words grouped by baseline position within a tolerance.
//!
//! The baseline model is direction-aware (SL-3.TEXT.13): continuity is the
//! perpendicular distance to the `Tm` writing direction, and word gaps are
//! the along-direction projection minus the pen step — not raw x/y. For
//! identity `Tm` this is exactly the old horizontal model (`|Δy|`, `Δx`),
//! so the horizontal majority is bit-identical; a 90° `Tm` vertical run
//! (and CJK vertical runs stepping along the same axis) stays one run, one
//! word, one line.
//!
//! The output feeds reading-order inference (SL-3.TEXT.04), selection
//! (SL-3.TEXT.05), and search (SL-3.TEXT.06).

use selis_geom::{Point, Rect};

/// A positioned glyph (the content interpreter's `TextGlyph`).
pub use selis_pdf_content::text::TextGlyph;

/// A text run: positioned glyphs sharing a font and size.
#[derive(Debug, Clone, PartialEq)]
pub struct TextRun {
    /// The positioned glyphs, in order.
    pub glyphs: Vec<TextGlyph>,
    /// The run's bounding box (user space).
    pub bbox: Rect,
}

/// A word: glyphs delimited by spaces.
#[derive(Debug, Clone, PartialEq)]
pub struct TextWord {
    /// The runs that make up the word.
    pub runs: Vec<TextRun>,
    /// The word's bounding box (user space).
    pub bbox: Rect,
}

/// A line: words grouped by y-position.
#[derive(Debug, Clone, PartialEq)]
pub struct TextLine {
    /// The words, in reading order (left-to-right within the line).
    pub words: Vec<TextWord>,
    /// The line's bounding box (user space).
    pub bbox: Rect,
}

/// The tolerance for grouping glyphs into lines, as a fraction of the font
/// size (a glyph on a neighbouring line differs by at least the leading).
const LINE_TOLERANCE_FRACTION: f64 = 0.5;

/// How much of a *space width* the excess advance gap over the previous
/// glyph's own pen step must reach before the gap counts as a word break.
/// Half a space forgives wide side-bearing, italic kerning, and the ±0.1 em
/// `TJ` adjustments inside a word while catching the full-space jumps
/// generated without space characters. (SL-3.TEXT.09 replaced an
/// `0.5 × font-size` text-space threshold that split nearly every glyph,
/// because a normal glyph advance *is* about half an em.)
const WORD_GAP_SPACE_FRACTION: f64 = 0.5;

/// Assemble positioned glyphs into runs, then words, then lines.
#[must_use]
pub fn assemble(glyphs: Vec<TextGlyph>) -> Vec<TextLine> {
    assemble_lines(assemble_words(assemble_runs(glyphs)))
}

/// The positioned glyphs a display list *shows*, in content order — one per
/// shown code, carrying the interpreter's pen step and space width
/// (SL-3.TEXT.09). Shared by every extractor surface (CLI, WASM worker) so
/// run/word/line assembly always sees the same glyph stream.
///
/// # Page-space placement and visibility (SL-3.TEXT.26)
///
/// A display-list `Op::Text` records its glyph positions in the user space of
/// the content stream that drew them — for a Form XObject, *Form* space, with
/// the placement (`/Matrix` plus the `cm` that invoked it) parked in the op's
/// resolved `state.ctm`. The render path applies that CTM; the text path did
/// not, so every glyph a Form drew was reported at its untransformed
/// Form-space position, and nothing anywhere compared a glyph against the
/// page's visible region. On a page whose Form is placed mostly off-page
/// (`issue7454`: a full A4 sheet translated onto a 384×111 page) the extractor
/// therefore returned 100 % of the sheet as text, while MuPDF and pdf.js — the
/// two engines that actually *render*, and the only relevant oracle for a
/// viewer — returned just the on-page band. Invisible text was offered for
/// search, selection and copy.
///
/// So this does two things, in the page's own user space:
///
/// 1. **Placement.** Each glyph is carried through its op's `ctm`, so `at`
///    and `tm` are page-space. Every scalar length rides the CTM's stretch
///    *along the direction it is measured in* — the pen step and space width
///    along the writing direction, the em height along its perpendicular.
///    That is exact for the scale-and-translate placements real producers
///    emit and for rotations (a rotation's stretch is 1); under an
///    anisotropic shear each length takes the stretch of its own axis, which
///    is the quantity the box model below actually measures.
/// 2. **Visibility.** `visible` is the page's `/MediaBox ∩ /CropBox`
///    (`Session::page_visible_box`). A glyph whose placed box does not
///    intersect it is **dropped**: the renderer would paint nothing, so the
///    text is not on the page and must not be extracted.
///
/// **A partially overlapping glyph is kept, deliberately.** Half a letter is
/// something the user can see, and both rendering oracles extract it; dropping
/// it would trade one invisible-text failure for a worse one — visible text
/// silently deleted from a page whose `/MediaBox` a producer set slightly too
/// small. The test is a closed-set overlap (see [`overlaps`]), so even a
/// zero-area glyph box standing inside the page counts as visible — a code the
/// font gives no width for still puts ink at its origin, and dropping those
/// would delete real text.
///
/// `visible: None` clips nothing. That is the honest reading of "this page
/// declares no visible region" (no `/MediaBox`, a non-finite box, an empty
/// intersection) and never means "nothing is visible" — see
/// `Session::page_visible_box`.
///
/// The active `W`/`W*` clip stack narrows the region further, by each clip
/// path's **bounding box**. That is a deliberate under-approximation of the
/// clip *region*: a curved clip keeps more than it should rather than less, so
/// it can never delete a visible glyph, while a rectangular clip — by far the
/// common case, and the one `issue7454`-shaped producers write — is exact.
/// Exact arbitrary-path clipping is a rasteriser concern and is not attempted
/// here.
#[must_use]
pub fn gather_glyphs(
    dl: &selis_pdf_content::display_list::DisplayList,
    visible: Option<Rect>,
) -> Vec<TextGlyph> {
    use selis_pdf_content::display_list::Op;
    let mut out = Vec::new();
    for op in &dl.ops {
        if let Op::Text {
            at,
            tm,
            state,
            runs,
        } = op
        {
            // The page's visible region, narrowed by the op's active clip stack.
            let region = visible.map(|page| narrow_by_clip(page, &state.clip));
            let mcid = state.mcid;
            for run in runs {
                for &code in &run.glyphs {
                    let placed = place_glyph(at, *tm, run, state.ctm);
                    if let Some(region) = region {
                        if !overlaps(&glyph_box(&placed), region) {
                            continue; // painted off-page: not text on this page
                        }
                    }
                    out.push(TextGlyph {
                        code,
                        at: placed.at,
                        tm: placed.tm,
                        advance: placed.advance,
                        font: run.font.clone(),
                        size: placed.size,
                        space: placed.space,
                        dir_x: placed.dir_x,
                        dir_y: placed.dir_y,
                        mcid,
                    });
                }
            }
        }
    }
    out
}

/// Narrow a page's visible region by the clip paths active at a text op.
///
/// Each path contributes its axis-aligned **bounding box**, which contains the
/// clip region it actually enforces — so the result is a superset of the true
/// visible region and the filter can only ever be too permissive, never drop a
/// glyph a renderer would paint. A path with no bounds (only a segment-less
/// path, which `exec.rs` never records as a clip) contributes nothing.
/// Rectangular clips, the overwhelmingly common case, are exact.
fn narrow_by_clip(
    page: Rect,
    clip: &[(
        selis_pdf_content::path::Path,
        selis_pdf_content::path::ClipRule,
    )],
) -> Rect {
    let mut region = page;
    for (path, _rule) in clip {
        if let Some(bounds) = path.bounds() {
            match region.intersect(bounds) {
                Some(narrowed) => region = narrowed,
                // An empty intersection is a real "nothing is visible" (a clip
                // entirely off-page), not a reason to stop narrowing. Keep an
                // empty rect: every glyph fails to intersect it and is dropped.
                None => return Rect::new(0.0, 0.0, 0.0, 0.0),
            }
        }
    }
    region
}

/// A glyph's geometry carried from its op's user space into the page's.
struct PlacedGlyph {
    /// The glyph origin, in the page's user space.
    at: Point,
    /// The text matrix, composed with the placement CTM.
    tm: selis_geom::Matrix,
    /// The pen step, in page space, along the writing direction.
    advance: f64,
    /// The em, in page space, measured perpendicular to the writing direction.
    size: f64,
    /// The font's space width, in page space, along the writing direction.
    space: f64,
    /// The writing direction's x component in page space (unit).
    dir_x: f64,
    /// The writing direction's y component in page space.
    dir_y: f64,
}

/// Map one run's glyph origin and lengths through the placement `ctm`.
///
/// The origin and the text matrix compose exactly — `tm.then(ctm)` is the same
/// text-to-page chain the renderer builds. The lengths cannot compose as
/// scalars under a general affine map (a CTM stretches different directions by
/// different factors), so each is scaled by the CTM's stretch along the axis it
/// is measured on: `advance`/`space` along the writing direction, the em height
/// along its perpendicular. `size` is the em the box model and the line
/// tolerance use, and it is measured vertically, so it takes the perpendicular
/// stretch.
///
/// A non-finite or singular CTM (a document may write `0 0 0 0 0 0 cm`) leaves
/// the glyph where it was rather than manufacturing NaN geometry; the render
/// path skips such glyphs outright, and dropping them here instead would change
/// what a zero-scale form contributes to extraction.
fn place_glyph(
    at: &Point,
    tm: selis_geom::Matrix,
    run: &selis_pdf_content::display_list::GlyphRun,
    ctm: selis_geom::Matrix,
) -> PlacedGlyph {
    let mut out = PlacedGlyph {
        at: *at,
        tm,
        advance: run.advance,
        size: run.size,
        space: run.space,
        dir_x: run.dir_x,
        dir_y: run.dir_y,
    };
    if ctm == selis_geom::Matrix::IDENTITY {
        return out; // the common case: bit-identical to the pre-TEXT.26 path
    }
    if !ctm.is_finite() || !at.x.is_finite() || !at.y.is_finite() || !tm.is_finite() {
        return out;
    }
    let (dx, dy) = (run.dir_x, run.dir_y);
    // The writing direction's image under the CTM's linear part, and the
    // perpendicular's — the two axes the run's lengths are measured on.
    let along = stretch(ctm, dx, dy);
    let perp = stretch(ctm, -dy, dx);
    if along <= 0.0 || perp <= 0.0 {
        return out; // singular placement: keep the untransformed geometry
    }
    out.at = ctm.apply(*at);
    out.tm = tm.then(ctm);
    out.advance = run.advance * along;
    out.space = run.space * along;
    out.size = run.size * perp;
    // Re-derive the direction from the placed matrix, exactly as the
    // interpreter's `writing_dir` does, so a rotated placement lays the run
    // along its new axis rather than its old one.
    let (ndx, ndy, _) = writing_dir(&out.tm);
    out.dir_x = ndx;
    out.dir_y = ndy;
    out
}

/// How much `ctm` stretches the unit vector `(ux, uy)` — the length of its
/// image under the linear part. Zero (never negative, never NaN) for a
/// degenerate axis or a non-finite CTM.
fn stretch(ctm: selis_geom::Matrix, ux: f64, uy: f64) -> f64 {
    let x = ctm.a * ux + ctm.c * uy;
    let y = ctm.b * ux + ctm.d * uy;
    let len = x.hypot(y);
    if len.is_finite() {
        len
    } else {
        0.0
    }
}

/// The writing direction of a text matrix: the text-space +x axis `(a, b)`
/// normalised, plus its length (the factor a text-space advance scales by).
///
/// Mirrors `selis_pdf_content::text`'s rule, including its total fallback to
/// `(1, 0)` at scale 0 for a singular or non-finite linear part — a horizontal
/// direction with zero-length advances, never a NaN.
fn writing_dir(matrix: &selis_geom::Matrix) -> (f64, f64, f64) {
    let (a, b) = (matrix.a, matrix.b);
    if !a.is_finite() || !b.is_finite() {
        return (1.0, 0.0, 0.0);
    }
    let scale = a.hypot(b);
    if !scale.is_finite() || scale <= 1e-12 {
        return (1.0, 0.0, 0.0);
    }
    (a / scale, b / scale, scale)
}

/// Whether two rectangles overlap **as closed sets** — a shared point counts.
///
/// This is deliberately not [`Rect::intersect`], which answers a different
/// question: it returns `None` whenever the overlap has no *area*. A glyph box
/// can easily have no area — a code the font gives no width for yields
/// `advance == 0`, so the box collapses to a vertical segment, and a page's
/// `W`-clip can collapse the region the same way. Under area semantics every
/// such glyph reads as "not on the page" and is silently deleted: measured on
/// `text08_encoding_unicode.pdf`, the `/Differences` and MacRoman lines lost
/// their `é` and `°` that way, and a whole zero-advance line vanished. A
/// closed-set test keeps them, because a glyph standing at a point inside the
/// page does put ink there.
///
/// The cost of the closed test is a measure-zero over-permissiveness: a box
/// that exactly abuts the page edge from outside is kept rather than dropped.
/// That is the safe direction — this filter exists to stop *invisible* text
/// being extracted, and a hair of extra tolerance can never delete a glyph a
/// renderer would paint.
fn overlaps(a: &Rect, b: Rect) -> bool {
    a.x1 >= b.x0 && a.x0 <= b.x1 && a.y1 >= b.y0 && a.y0 <= b.y1
}

/// A placed glyph's box: its origin, `advance` along the writing direction, and
/// ±`size` perpendicular — the same generous em box
/// [`crate::selection::glyph_rect`] builds (ascent ≈ 0.8 em, descent ≈ 0.2 em,
/// rounded out to a full em each way), so "visible to the extractor" and
/// "highlightable by selection" cannot disagree about where a glyph is.
fn glyph_box(g: &PlacedGlyph) -> Rect {
    let tip = Point::new(g.at.x + g.dir_x * g.advance, g.at.y + g.dir_y * g.advance);
    let (nx, ny) = (-g.dir_y, g.dir_x);
    let up = Point::new(g.at.x + nx * g.size, g.at.y + ny * g.size);
    let down = Point::new(g.at.x - nx * g.size, g.at.y - ny * g.size);
    Rect::from_points(
        g.at.x.min(tip.x).min(up.x).min(down.x),
        g.at.y.min(tip.y).min(up.y).min(down.y),
        g.at.x.max(tip.x).max(up.x).max(down.x),
        g.at.y.max(tip.y).max(up.y).max(down.y),
    )
}

/// Replace glyph codes with their recovered Unicode scalars in place, so
/// downstream readers (word assembly included) see characters, not codes
/// (SL-3.TEXT.08 / SL-3.SHAPE.04).
///
/// `resolve(font_name, code)` is the caller's document-backed lookup (the
/// engine's `Session::text_unicode`: `/ToUnicode` then the encoding/glyph-name
/// recovery chain); resolution is cached per (font, code). A recovered scalar
/// above `u16::MAX` cannot be carried in the glyph code and keeps the raw
/// code (documented limitation).
///
/// # Malformed Input
///
/// A `None` resolution keeps the legacy byte-as-character reading — recovery
/// failures never drop text, and never guess.
pub fn apply_unicode_recovery(
    glyphs: &mut [TextGlyph],
    resolve: &mut dyn FnMut(&selis_bytes::Bytes, u16) -> Option<u32>,
) {
    let mut cache: std::collections::HashMap<(Vec<u8>, u16), Option<u32>> =
        std::collections::HashMap::new();
    for glyph in glyphs.iter_mut() {
        let key = (glyph.font.as_slice().to_vec(), glyph.code);
        let cached = cache
            .entry(key)
            .or_insert_with(|| resolve(&glyph.font, glyph.code));
        if let Some(uni) = *cached {
            if let Ok(narrow) = u16::try_from(uni) {
                glyph.code = narrow;
            }
        }
    }
}

/// Group consecutive glyphs with the same font and size into runs.
///
/// Continuity is direction-aware (SL-3.TEXT.13): the next glyph joins the
/// run when it lies within the perpendicular tolerance of the run's writing
/// direction — the glyph `g`'s own direction, taken as the continuation
/// axis. Identity `Tm` (`(1, 0)`) reduces the perpendicular check to
/// `|Δy|`, exactly the old horizontal model.
#[must_use]
pub fn assemble_runs(glyphs: Vec<TextGlyph>) -> Vec<TextRun> {
    let mut out: Vec<TextRun> = Vec::new();
    for g in glyphs {
        let g_font = g.font.as_slice().to_vec();
        let g_size = g.size;
        let tol = g_size * LINE_TOLERANCE_FRACTION;
        let same_style = out.last().is_some_and(|r| {
            r.glyphs.first().is_some_and(|first| {
                // Same font/size AND the glyph continues the line's baseline.
                first.font.as_slice() == g_font.as_slice()
                    && first.size == g_size
                    && r.glyphs
                        .last()
                        .is_some_and(|last| perp_distance(&g, last.at) <= tol)
            })
        });
        if same_style {
            if let Some(run) = out.last_mut() {
                let at = g.at;
                run.glyphs.push(g);
                run.bbox = grow_bbox(run.bbox, at);
            }
        } else {
            let at = g.at;
            out.push(TextRun {
                glyphs: vec![g],
                bbox: point_bbox(at),
            });
        }
    }
    out
}

/// The perpendicular distance of a point from the line through `origin`
/// running along `g`'s writing direction (SL-3.TEXT.13): the cross-product
/// magnitude `|dir × (p − origin)|`. For `dir = (1, 0)` this is `|Δy|`, the
/// horizontal baseline model; for a vertical `dir = (0, 1)` it is `|Δx|`.
fn perp_distance(g: &TextGlyph, origin: selis_geom::Point) -> f64 {
    let (dx, dy) = (g.dir_x, g.dir_y);
    let (px, py) = (g.at.x - origin.x, g.at.y - origin.y);
    (dx * py - dy * px).abs()
}

/// Split runs at space characters (code 0x20) into words.
#[must_use]
pub fn assemble_words(runs: Vec<TextRun>) -> Vec<TextWord> {
    let mut out: Vec<TextWord> = Vec::new();
    let mut word_glyphs: Vec<TextGlyph> = Vec::new();
    for run in runs {
        // A run starting on a different baseline breaks the current word.
        let size = word_glyphs.last().map(|g| g.size).unwrap_or(12.0);
        let same_baseline = match (run.glyphs.first(), word_glyphs.last()) {
            (Some(first), Some(last)) => {
                perp_distance(first, last.at) <= size * LINE_TOLERANCE_FRACTION
            }
            _ => true,
        };
        if !same_baseline && !word_glyphs.is_empty() {
            out.push(word_from_glyphs(std::mem::take(&mut word_glyphs)));
        }
        for g in run.glyphs {
            if g.code == 0x20 {
                if !word_glyphs.is_empty() {
                    out.push(word_from_glyphs(std::mem::take(&mut word_glyphs)));
                }
            } else if let Some(last) = word_glyphs.last() {
                // Advance-gap word inference (SL-3.TEXT.09): the next glyph's
                // origin sits one pen step away when the word continues. A gap
                // of more than half an *extra* space beyond that step — in the
                // same (user-space) units as both positions — indicates a space
                // even without a space glyph, common in generated PDFs whose
                // content stream has no space characters at all. The gap is
                // the advance along the writing direction (SL-3.TEXT.13): the
                // projection of `g − last` onto the direction. Identity `Tm`
                // reduces to `Δx`, the old model.
                let gap = along_distance(&g, last.at);
                let over_space = gap - last.advance;
                if last.space > 0.0 && over_space > last.space * WORD_GAP_SPACE_FRACTION {
                    out.push(word_from_glyphs(std::mem::take(&mut word_glyphs)));
                    word_glyphs.push(g);
                } else {
                    word_glyphs.push(g);
                }
            } else {
                word_glyphs.push(g);
            }
        }
    }
    if !word_glyphs.is_empty() {
        out.push(word_from_glyphs(word_glyphs));
    }
    out
}

/// The signed advance of `p` from `origin` along `g`'s writing direction
/// (SL-3.TEXT.13): the projection `dir · (p − origin)`. Identity `Tm`
/// (`(1, 0)`) reduces to `Δx`.
fn along_distance(g: &TextGlyph, origin: selis_geom::Point) -> f64 {
    let (dx, dy) = (g.dir_x, g.dir_y);
    let (px, py) = (g.at.x - origin.x, g.at.y - origin.y);
    dx * px + dy * py
}

/// Group words into lines by baseline clustering.
///
/// Baseline clustering is direction-aware (SL-3.TEXT.13): continuity is the
/// perpendicular distance to the `Tm` writing direction of the line's first
/// glyph, rather than raw y-position. Identity `Tm` (`(1, 0)`) reduces the
/// perpendicular distance to `|Δy|`, exactly the old horizontal model.
#[must_use]
pub fn assemble_lines(words: Vec<TextWord>) -> Vec<TextLine> {
    let mut out: Vec<TextLine> = Vec::new();
    for w in words {
        let size = w
            .runs
            .first()
            .and_then(|r| r.glyphs.first())
            .map(|g| g.size)
            .unwrap_or(12.0);
        let tol = size * LINE_TOLERANCE_FRACTION;
        let w_bbox = w.bbox;
        let w_centre =
            selis_geom::Point::new((w_bbox.x0 + w_bbox.x1) / 2.0, (w_bbox.y0 + w_bbox.y1) / 2.0);

        let same_line = out.last().is_some_and(|l| {
            let first_glyph = l
                .words
                .first()
                .and_then(|w| w.runs.first())
                .and_then(|r| r.glyphs.first());
            match first_glyph {
                Some(g) => perp_distance_pt(g, w_centre) <= tol,
                // A line always has at least one word with at least one run
                // with at least one glyph (the invariant `assemble_words`
                // preserves), so this branch is unreachable in practice; the
                // match keeps the lint set total without a panic primitive.
                None => true,
            }
        });

        if same_line {
            if let Some(line) = out.last_mut() {
                line.words.push(w);
                line.bbox = grow_bbox(line.bbox, Point::new(w_bbox.x1, w_bbox.y1));
            }
        } else {
            out.push(TextLine {
                words: vec![w],
                bbox: w_bbox,
            });
        }
    }
    out
}

/// The perpendicular distance of a point `p` from the line through `origin`
/// running along `g`'s writing direction.
fn perp_distance_pt(g: &TextGlyph, p: selis_geom::Point) -> f64 {
    let (dx, dy) = (g.dir_x, g.dir_y);
    let (px, py) = (p.x - g.at.x, p.y - g.at.y);
    (dx * py - dy * px).abs()
}

/// Build a word from its glyphs (grouping into runs by font/size).
fn word_from_glyphs(glyphs: Vec<TextGlyph>) -> TextWord {
    let runs = assemble_runs(glyphs);
    let mut bbox: Option<Rect> = None;
    for run in &runs {
        for g in &run.glyphs {
            bbox = Some(match bbox {
                Some(b) => grow_bbox(b, g.at),
                None => point_bbox(g.at),
            });
        }
    }
    TextWord {
        runs,
        bbox: bbox.unwrap_or(Rect::new(0.0, 0.0, 0.0, 0.0)),
    }
}

/// The bounding box of a point.
fn point_bbox(p: Point) -> Rect {
    Rect::new(p.x, p.y, p.x, p.y)
}

/// Grow a bounding box to include a point.
fn grow_bbox(b: Rect, p: Point) -> Rect {
    Rect::new(b.x0.min(p.x), b.y0.min(p.y), b.x1.max(p.x), b.y1.max(p.y))
}

/// The SL-3.TEXT.26 regression suite: page-space placement of a Form XObject's
/// text, and the visible-region filter that keeps clipped text out of
/// extraction, search, and selection.
///
/// The end-to-end pin — a synthetic PDF reproducing `issue7454`'s placement
/// (a full A4 Form scaled and translated onto a small page) extracted through
/// the real `selis extract` / `selis search` binaries and diffed against MuPDF
/// — is `apps/cli/tests/text26_form_placement.rs`. These tests pin the unit the
/// end-to-end one rests on, at the level the geometry is actually decided.
#[cfg(test)]
mod visible_region {
    #![allow(clippy::arithmetic_side_effects)]

    use super::*;
    use selis_bytes::Bytes;
    use selis_geom::Matrix;
    use selis_pdf_content::display_list::{DisplayList, GlyphRun, Op, ResolvedState};
    use selis_pdf_content::gstate::GState;
    use selis_pdf_content::path::{ClipRule, Path};

    /// A run of `n` copies of `code` shown at `at` under `tm`, painted with
    /// `ctm` — the display list's own shape, so the test drives the real
    /// `Op::Text` the interpreter emits rather than a stand-in.
    fn text_op(at: Point, tm: Matrix, ctm: Matrix, code: u16, n: usize) -> Op {
        let mut g = GState::new();
        g.ctm = ctm;
        Op::Text {
            at,
            tm,
            state: ResolvedState::from(&g),
            runs: vec![GlyphRun {
                advance: 6.0,
                font: Bytes::copy_from_slice(b"F1"),
                size: 12.0,
                space: 3.0,
                dir_x: 1.0,
                dir_y: 0.0,
                glyphs: vec![code; n],
            }],
        }
    }

    /// The same text op with `clip` active on it.
    fn text_op_clipped(at: Point, ctm: Matrix, clip: Path) -> Op {
        let mut op = text_op(at, Matrix::IDENTITY, ctm, 65, 1);
        if let Op::Text { state, .. } = &mut op {
            state.clip = vec![(clip, ClipRule::NonZero)];
        }
        op
    }

    fn rect_path(r: Rect) -> Path {
        let mut p = Path::new();
        p.rectangle(r);
        p
    }

    /// The `issue7454` placement: net scale 1.0, translate (-19.01, -623.92) —
    /// the transform a full A4 sheet undergoes to land on a 384×111 page. The
    /// real file reaches it through a `/Matrix` and a `cm`; the net is what the
    /// extractor has to honour, and a single matrix keeps the unit test free of
    /// the (pre-existing, separate) `cm`-composition-order divergence between
    /// selis and MuPDF that `apps/cli/tests/text26_form_placement.rs` records.
    fn issue7454_ctm() -> Matrix {
        Matrix::new(1.0, 0.0, 0.0, 1.0, -19.0089, -623.9184)
    }

    /// The three A4 baselines of the `issue7454`-shaped fixture, drawn through
    /// the form: only the middle one lands on the 384×111 page.
    fn issue7454_ops() -> DisplayList {
        let ctm = issue7454_ctm();
        DisplayList {
            ops: vec![
                text_op(Point::new(72.0, 800.0), Matrix::IDENTITY, ctm, 65, 1),
                text_op(Point::new(72.0, 700.0), Matrix::IDENTITY, ctm, 66, 1),
                text_op(Point::new(72.0, 400.0), Matrix::IDENTITY, ctm, 67, 1),
            ],
        }
    }

    /// The headline defect: a glyph the Form drew at Form-space y=800 is
    /// reported at page-space y≈176.08 — *off* the 111 pt page — while the one
    /// at Form y=700 lands at page y≈76.08, on it. Before the fix all three
    /// were reported at their Form-space y (800, 700, 400), which is why the
    /// 94 % of the sheet a renderer clips was extractable.
    #[test]
    fn form_placement_ctm_is_applied_to_reported_positions() {
        let glyphs = gather_glyphs(&issue7454_ops(), None);
        let ys: Vec<f64> = glyphs.iter().map(|g| g.at.y).collect();
        assert!(
            (ys[0] - 176.0816).abs() < 1e-3,
            "Form y=800 places at page y≈176.08, got {}",
            ys[0]
        );
        assert!(
            (ys[1] - 76.0816).abs() < 1e-3,
            "Form y=700 places at page y≈76.08, got {}",
            ys[1]
        );
        assert!(
            (ys[2] + 223.9184).abs() < 1e-3,
            "Form y=400 places at page y≈-223.92, got {}",
            ys[2]
        );
        // The x translation rides too: 72 - 19.0089.
        assert!((glyphs[0].at.x - 52.9911).abs() < 1e-3, "x rides the CTM");
    }

    /// The visibility rule on the same fixture: with the 384×111 page as the
    /// visible region, only the glyph whose placed box lands on the page
    /// survives. The other two are on the page in *Form* space and nowhere in
    /// page space — exactly the invisible text the defect offered for search.
    #[test]
    fn glyphs_placed_off_the_page_are_not_extracted() {
        let page = Rect::new(0.0, 0.0, 384.0, 111.0);
        let glyphs = gather_glyphs(&issue7454_ops(), Some(page));
        assert_eq!(
            glyphs.len(),
            1,
            "only the on-page band is text on this page: {:?}",
            glyphs.iter().map(|g| g.at).collect::<Vec<_>>()
        );
        assert!(
            (glyphs[0].at.y - 76.0816).abs() < 1e-3,
            "the survivor is the on-page band"
        );
    }

    /// The deliberate half-way case, pinned: a glyph whose box *straddles* the
    /// page edge is kept. Half a letter is on the page, both rendering oracles
    /// extract it, and dropping it would silently delete visible text from a
    /// page whose box a producer set slightly too small.
    #[test]
    fn a_partially_overlapping_glyph_is_kept() {
        // Identity placement (so only the filter is under test): a 12 pt glyph
        // at y=6 on a 0..10 page has its box at y −6..18 — half on, half off.
        let dl = DisplayList {
            ops: vec![text_op(
                Point::new(50.0, 6.0),
                Matrix::IDENTITY,
                Matrix::IDENTITY,
                65,
                1,
            )],
        };
        let glyphs = gather_glyphs(&dl, Some(Rect::new(0.0, 0.0, 100.0, 10.0)));
        assert_eq!(glyphs.len(), 1, "a straddling glyph is visible text");
    }

    /// A glyph whose box lies *entirely* outside is dropped — the defect.
    #[test]
    fn a_glyph_entirely_off_the_page_is_dropped() {
        let dl = DisplayList {
            ops: vec![text_op(
                Point::new(50.0, 200.0),
                Matrix::IDENTITY,
                Matrix::IDENTITY,
                65,
                1,
            )],
        };
        assert!(gather_glyphs(&dl, Some(Rect::new(0.0, 0.0, 100.0, 10.0))).is_empty());
    }

    /// A **zero-advance** glyph is still on the page, and this is the case a
    /// naive area test gets wrong. `text08_encoding_unicode.pdf` (caught by the
    /// existing SL-3.TEXT.08 CLI pin when this filter was first written) has
    /// codes its fonts give no width for: `advance == 0` collapses the glyph box
    /// to a vertical segment, and `Rect::intersect` — which answers "does the
    /// overlap have *area*?" — reported no overlap, deleting the MacRoman `é`,
    /// the WinAnsi `°`, and an entire line. The shape here is theirs: origin
    /// (76.032, 760), advance 0, size 16, on a 612×792 page.
    #[test]
    fn a_zero_advance_glyph_inside_the_page_is_kept() {
        let mut op = text_op(
            Point::new(76.032, 760.0),
            Matrix::IDENTITY,
            Matrix::IDENTITY,
            0xB0,
            1,
        );
        if let Op::Text { runs, .. } = &mut op {
            runs[0].advance = 0.0; // the font has no width for this code
        }
        let glyphs = gather_glyphs(
            &DisplayList { ops: vec![op] },
            Some(Rect::new(0.0, 0.0, 612.0, 792.0)),
        );
        assert_eq!(
            glyphs.len(),
            1,
            "a zero-width glyph standing on the page is visible text"
        );
    }

    /// `None` clips nothing: a page that declares no visible region must not
    /// lose its text. Dropping it would be the same class of failure this task
    /// removes, one level up.
    #[test]
    fn no_declared_region_clips_nothing() {
        let ctm = issue7454_ctm();
        let dl = DisplayList {
            ops: vec![
                text_op(Point::new(72.0, 800.0), Matrix::IDENTITY, ctm, 65, 1),
                text_op(Point::new(72.0, 400.0), Matrix::IDENTITY, ctm, 67, 1),
            ],
        };
        assert_eq!(gather_glyphs(&dl, None).len(), 2);
    }

    /// The DoD's "existing behaviour is unchanged for ordinary pages": under an
    /// identity CTM every field is bit-identical to the pre-TEXT.26 gather, so
    /// no page that does not use a placement transform can regress.
    #[test]
    fn identity_ctm_is_bit_identical() {
        let at = Point::new(72.0, 700.0);
        let dl = DisplayList {
            ops: vec![text_op(at, Matrix::IDENTITY, Matrix::IDENTITY, 65, 3)],
        };
        let glyphs = gather_glyphs(&dl, Some(Rect::new(0.0, 0.0, 612.0, 792.0)));
        assert_eq!(glyphs.len(), 3);
        for g in &glyphs {
            assert_eq!(g.at, at);
            assert_eq!(g.tm, Matrix::IDENTITY);
            assert_eq!(g.advance, 6.0);
            assert_eq!(g.space, 3.0);
            assert_eq!(g.size, 12.0);
            assert_eq!((g.dir_x, g.dir_y), (1.0, 0.0));
        }
    }

    /// A scaling placement scales the lengths it must, on the axis each is
    /// measured on: a 2× form doubles the pen step and the space width (the
    /// TEXT.09 word-gap reference, or the scaled form would split every word),
    /// and doubles the em the line tolerance and the glyph box use.
    #[test]
    fn a_scaling_placement_scales_the_lengths_it_measures() {
        let dl = DisplayList {
            ops: vec![text_op(
                Point::new(10.0, 20.0),
                Matrix::IDENTITY,
                Matrix::scale(2.0, 2.0),
                65,
                1,
            )],
        };
        let g = &gather_glyphs(&dl, None)[0];
        assert_eq!(g.at, Point::new(20.0, 40.0));
        assert_eq!(g.advance, 12.0, "the pen step rides the 2× placement");
        assert_eq!(g.space, 6.0, "the word-gap reference rides it too");
        assert_eq!(g.size, 24.0, "the em rides it too");
    }

    /// A rotated placement re-derives the writing direction from the *placed*
    /// matrix, so a 90° form lays its run along page +y and the extractor
    /// reports a vertical line (SL-3.TEXT.13's model, in page space).
    #[test]
    fn a_rotated_placement_relays_the_run_in_page_space() {
        let dl = DisplayList {
            ops: vec![text_op(
                Point::new(10.0, 20.0),
                Matrix::IDENTITY,
                Matrix::rotate_quadrant(1),
                65,
                1,
            )],
        };
        let g = &gather_glyphs(&dl, None)[0];
        assert_eq!(g.at, Point::new(-20.0, 10.0));
        assert!(
            g.dir_x.abs() < 1e-12 && (g.dir_y - 1.0).abs() < 1e-12,
            "the run now writes along page +y, got ({}, {})",
            g.dir_x,
            g.dir_y
        );
        // A rotation stretches nothing, so the lengths are untouched.
        assert_eq!(g.advance, 6.0);
        assert_eq!(g.size, 12.0);
    }

    /// A `W n` clip narrows the region: text the clip path excludes is not
    /// extracted even though the page box would allow it. A rectangular clip —
    /// the common case — is exact.
    #[test]
    fn an_active_rectangular_clip_narrows_the_region() {
        let dl = DisplayList {
            ops: vec![
                text_op_clipped(
                    Point::new(50.0, 50.0),
                    Matrix::IDENTITY,
                    rect_path(Rect::new(0.0, 0.0, 100.0, 60.0)),
                ),
                text_op_clipped(
                    Point::new(50.0, 500.0),
                    Matrix::IDENTITY,
                    rect_path(Rect::new(0.0, 0.0, 100.0, 60.0)),
                ),
            ],
        };
        let glyphs = gather_glyphs(&dl, Some(Rect::new(0.0, 0.0, 612.0, 792.0)));
        assert_eq!(glyphs.len(), 1, "only the glyph inside the clip survives");
        assert_eq!(glyphs[0].at, Point::new(50.0, 50.0));
    }

    /// A clip entirely off the page is a real "nothing is visible", and the
    /// filter must honour it rather than fall back to the unclipped page.
    #[test]
    fn a_clip_entirely_off_page_hides_everything() {
        let dl = DisplayList {
            ops: vec![text_op_clipped(
                Point::new(50.0, 50.0),
                Matrix::IDENTITY,
                rect_path(Rect::new(5000.0, 5000.0, 5100.0, 5100.0)),
            )],
        };
        assert!(gather_glyphs(&dl, Some(Rect::new(0.0, 0.0, 612.0, 792.0))).is_empty());
    }

    /// Hostile geometry must not become NaN positions or a panic: a document
    /// may write a NaN or singular `cm`. Such a glyph keeps its untransformed
    /// geometry (and is then subject to the ordinary page-box test) rather than
    /// poisoning every downstream bbox, sort key, and search rect.
    #[test]
    fn hostile_placement_matrices_stay_finite() {
        let hostile = [
            Matrix::new(0.0, 0.0, 0.0, 0.0, 0.0, 0.0), // singular
            Matrix::new(f64::NAN, 0.0, 0.0, 1.0, 0.0, 0.0),
            Matrix::new(1.0, 0.0, 0.0, f64::INFINITY, 0.0, 0.0),
        ];
        for ctm in hostile {
            let dl = DisplayList {
                ops: vec![text_op(
                    Point::new(72.0, 700.0),
                    Matrix::IDENTITY,
                    ctm,
                    65,
                    1,
                )],
            };
            // Must not panic, and every reported number must stay finite.
            for g in gather_glyphs(&dl, Some(Rect::new(0.0, 0.0, 612.0, 792.0))) {
                assert!(g.at.x.is_finite() && g.at.y.is_finite(), "{:?}", g.at);
                assert!(g.advance.is_finite() && g.size.is_finite());
                assert!(g.dir_x.is_finite() && g.dir_y.is_finite());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::indexing_slicing, clippy::arithmetic_side_effects)]

    use super::*;
    use selis_bytes::Bytes;

    /// A glyph with a typical proportionally-spaced pen model: advance half an
    /// em, space a quarter em (matches what the content interpreter records for
    /// the standard-14 fonts), so gap inference behaves as it does downstream.
    fn glyph(code: u16, x: f64, y: f64, font: &str, size: f64) -> TextGlyph {
        TextGlyph {
            code,
            at: Point::new(x, y),
            tm: selis_geom::Matrix::IDENTITY,
            advance: size * 0.5,
            font: Bytes::copy_from_slice(font.as_bytes()),
            size,
            space: size * 0.25,
            dir_x: 1.0,
            dir_y: 0.0,
            mcid: None,
        }
    }

    /// A glyph with an explicit pen advance and space width.
    fn glyph_pen(code: u16, x: f64, y: f64, size: f64, advance: f64, space: f64) -> TextGlyph {
        TextGlyph {
            code,
            at: Point::new(x, y),
            tm: selis_geom::Matrix::IDENTITY,
            advance,
            font: Bytes::copy_from_slice(b"F1"),
            size,
            space,
            dir_x: 1.0,
            dir_y: 0.0,
            mcid: None,
        }
    }

    #[test]
    fn runs_group_same_font_and_size() {
        let glyphs = vec![
            glyph(65, 0.0, 0.0, "F1", 12.0),
            glyph(66, 6.0, 0.0, "F1", 12.0),
            glyph(67, 12.0, 0.0, "F2", 14.0), // different font
            glyph(68, 18.0, 0.0, "F2", 14.0),
        ];
        let runs = assemble_runs(glyphs);
        assert_eq!(runs.len(), 2);
        assert_eq!(runs[0].glyphs.len(), 2);
        assert_eq!(runs[1].glyphs.len(), 2);
    }

    #[test]
    fn spaces_split_words() {
        let glyphs = vec![
            glyph(65, 0.0, 0.0, "F1", 12.0),   // A
            glyph(0x20, 6.0, 0.0, "F1", 12.0), // space
            glyph(66, 12.0, 0.0, "F1", 12.0),  // B
        ];
        let words = assemble_words(assemble_runs(glyphs));
        assert_eq!(words.len(), 2);
        assert_eq!(words[0].runs[0].glyphs.len(), 1);
        assert_eq!(words[1].runs[0].glyphs.len(), 1);
    }

    #[test]
    fn y_positions_group_lines() {
        let glyphs = vec![
            glyph(65, 0.0, 0.0, "F1", 12.0), // line 1
            glyph(66, 6.0, 0.0, "F1", 12.0),
            glyph(67, 0.0, -14.0, "F1", 12.0), // line 2
        ];
        let lines = assemble(glyphs);
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].words.len(), 1);
        assert_eq!(lines[0].words[0].runs[0].glyphs.len(), 2);
        assert_eq!(lines[1].words[0].runs[0].glyphs.len(), 1);
    }

    /// Advance-gap word inference: a wide horizontal gap separates words even
    /// without a space glyph.
    #[test]
    fn gaps_infer_word_boundaries() {
        let glyphs = vec![
            glyph(65, 0.0, 0.0, "F1", 12.0),  // A
            glyph(66, 6.0, 0.0, "F1", 12.0),  // B (tight)
            glyph(67, 30.0, 0.0, "F1", 12.0), // C (gap > 6 = half an em)
        ];
        let words = assemble_words(assemble_runs(glyphs));
        assert_eq!(words.len(), 2);
        assert_eq!(words[0].runs[0].glyphs.len(), 2);
        assert_eq!(words[1].runs[0].glyphs.len(), 1);
    }

    #[test]
    fn empty_input_yields_no_lines() {
        assert!(assemble(Vec::new()).is_empty());
    }

    /// The SL-3.TEXT.03 DoD case, tracked to its root cause by SL-3.TEXT.09:
    /// a generated content stream with **no space characters at all** still
    /// yields two words — the a→b→c→d run has no `0x20` code anywhere, the
    /// only word signal is the advance-gap jump before `c`.
    #[test]
    fn no_space_characters_in_content_stream_yet_two_words() {
        let glyphs = vec![
            glyph_pen(97, 0.0, 0.0, 12.0, 5.0, 3.0),   // a
            glyph_pen(98, 5.0, 0.0, 12.0, 5.0, 3.0),   // b
            glyph_pen(99, 16.0, 0.0, 12.0, 5.0, 3.0),  // c: 6 past b's step
            glyph_pen(100, 21.0, 0.0, 12.0, 5.0, 3.0), // d (tight)
        ];
        let words = assemble_words(assemble_runs(glyphs));
        assert_eq!(words.len(), 2);
        assert_eq!(words[0].runs[0].glyphs.len(), 2);
        assert_eq!(words[1].runs[0].glyphs.len(), 2);
    }

    /// The regression the SL-3.CONF.01 sweep made headline: glyph advances of
    /// about half an em must NOT each trip a word break. Before the fix the
    /// threshold was `0.5 × font size` against the origin gap — a normal 'e'
    /// advance meets it exactly at 12pt and exceeds it at larger sizes, and
    /// nearly every glyph extracted as its own word.
    #[test]
    fn half_em_glyph_advances_never_split() {
        let size = 24.0;
        // Helvetica widths (per 1000 em) for "Selisoracle" — every consecutive
        // origin gap is the previous glyph's advance; none is a space jump.
        let widths = [
            667.0, 556.0, 222.0, 556.0, 222.0, 556.0, 556.0, 333.0, 556.0, 500.0, 222.0,
        ];
        let mut x = 0.0;
        let mut glyphs = Vec::new();
        for (i, w) in widths.iter().enumerate() {
            let code = [83, 101, 108, 105, 115, 111, 114, 97, 99, 108, 101][i];
            let advance = *w / 1000.0 * size;
            glyphs.push(glyph_pen(
                code,
                x,
                0.0,
                size,
                advance,
                278.0 / 1000.0 * size,
            ));
            x += advance;
        }
        let words = assemble_words(assemble_runs(glyphs));
        // All advances except the very last remain *inside* the word: no gap
        // exceeds its own step by even a tenth of a space.
        assert_eq!(words.len(), 1);
    }

    /// Metrics-free glyphs (no advance/space recorded — a display list built
    /// outside the interpreter) merge rather than fragment: a zero space width
    /// disables gap inference, and never splits per glyph.
    #[test]
    fn unknown_space_width_merges_not_splits() {
        let glyphs = vec![
            glyph_pen(65, 0.0, 0.0, 12.0, 0.0, 0.0),
            glyph_pen(66, 9.0, 0.0, 12.0, 0.0, 0.0),
            glyph_pen(67, 60.0, 0.0, 12.0, 0.0, 0.0),
        ];
        let words = assemble_words(assemble_runs(glyphs));
        assert_eq!(words.len(), 1, "a space width of 0 must not invent breaks");
    }
}

/// GAP-INVARIANT PROPERTIES (SL-3.TEXT.09 DoD): word-gap inference is a
/// function of the pen step vs space width — never of font size alone, and
/// never of coordinate drift.
#[cfg(test)]
mod gap_properties {
    #![allow(
        clippy::arithmetic_side_effects,
        clippy::cast_possible_truncation,
        clippy::cast_possible_wrap,
        clippy::expect_used,
        clippy::indexing_slicing,
        clippy::panic,
        clippy::unwrap_used
    )]

    use super::{assemble_runs, assemble_words};
    use proptest::prelude::*;
    use selis_bytes::Bytes;
    use selis_geom::Point;
    use selis_pdf_content::text::TextGlyph;

    fn glyph(code: u16, x: f64, advance: f64, space: f64) -> TextGlyph {
        TextGlyph {
            code,
            at: Point::new(x, 0.0),
            tm: selis_geom::Matrix::IDENTITY,
            advance,
            font: Bytes::copy_from_slice(b"F1"),
            size: 24.0,
            space,
            dir_x: 1.0,
            dir_y: 0.0,
            mcid: None,
        }
    }

    /// `advance/step` in (0, 50) and word length 1..40.
    /// Uniform tight layout: every gap equals the previous advance → exactly
    /// one word, whatever the (mutually independent) size, advance, and space
    /// width. The half-em split bug (SL-3.TEXT.09) violated this whenever the
    /// advance reached ~0.5 of the font *size* — an accident of the metrics,
    /// since a 0x20-free tight stream must never split.
    ///
    /// A jump of `jump_factor` extra space widths at position `jump_at` must
    /// split (factor ≥ 0.51) and must split exactly once.
    #[derive(Debug, Clone)]
    struct Layout {
        n: usize,
        advance: f64,
        space: f64,
        jump_at: Option<usize>,
        jump_factor: f64,
    }

    fn arbitrary_layout() -> impl Strategy<Value = Layout> {
        (1usize..40, 1.0f64..50.0, 0.5f64..30.0).prop_map(|(n, advance, space)| Layout {
            n,
            advance,
            space,
            jump_at: None,
            jump_factor: 0.0,
        })
    }

    fn build(l: &Layout) -> Vec<TextGlyph> {
        let mut out = Vec::with_capacity(l.n);
        let mut x = 0.0_f64;
        for i in 0..l.n {
            out.push(glyph(65 + (i % 26) as u16, x, l.advance, l.space));
            let mut step = l.advance;
            if l.jump_at == Some(i) {
                step += l.space * l.jump_factor;
            }
            x += step;
        }
        out
    }

    proptest! {
        #[test]
        fn uniform_steps_never_split(l in arbitrary_layout()) {
            let words = assemble_words(assemble_runs(build(&l)));
            prop_assert_eq!(words.len(), 1);
        }

        #[test]
        fn jump_beyond_half_space_always_splits(
            l in arbitrary_layout(),
            idx_factor in 0usize..40,
            jump in 0.51f64..12.0,
        ) {
            let mut l = l;
            prop_assume!(l.n > 1);
            // Place the jump at an interior position whose successor exists.
            l.jump_at = Some(idx_factor % (l.n - 1));
            l.jump_factor = jump;
            let words = assemble_words(assemble_runs(build(&l)));
            prop_assert_eq!(words.len(), 2);
        }
    }
}
