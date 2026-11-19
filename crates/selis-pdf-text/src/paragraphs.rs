//! Paragraph reconstruction with confidence (SL-5.TEXTEDIT.01).
//!
//! The extractor already produces positioned glyphs → runs → words → lines
//! ([`crate::assembly`]) and a reading order ([`crate::order`]). This module
//! takes the *assembled, ordered* lines and groups them into paragraphs,
//! returning a [`Paragraph`] model plus a [`Confidence`] that says how much
//! the four available signals agree with that grouping.
//!
//! It is deliberately **pure over already-assembled lines**: no content-stream
//! parsing and no font resolution happen here — those are the engine's job
//! ([`crate::assembly::gather_glyphs`], `Session::text_unicode`). The only
//! allocation charged to the caller's budget is the grouping buffer; the rest
//! reuses the lines it was given.
//!
//! # Signals
//!
//! Four independent 0..1 signals feed the aggregate (weights in parentheses):
//!
//! * **baseline regularity** (0.30) — how tightly the within-paragraph
//!   baseline gaps cluster (coefficient of variation, `1/(1+cv)`);
//! * **spacing consistency** (0.25) — how much larger the between-paragraph
//!   gaps are than the within ones (`1 - exp(1 - ratio)`) and how uniform those
//!   between-gaps are;
//! * **font uniformity** (0.25) — the fraction of within-paragraph adjacent
//!   line pairs sharing font and size;
//! * **structure-tree agreement** (0.20) — MCID coverage/monotonicity, with a
//!   tagged order scoring higher than a geometric one at equal coverage.
//!
//! The aggregate is the weighted sum, clamped to 0..1. Every signal is
//! documented at its computation site; the exact constants are listed here so
//! a caller can see that the number is a *measured* combination, not a guess.
//!
//! # What confidence means, and what it does not
//!
//! A high [`Confidence::value`] says the four signals agree with the grouping.
//! It is **not** a claim of correctness against hand-labelled data. SL-5.TEXTEDIT.01's
//! DoD asks for a correlation against a 500-page labelled set; that set does not
//! exist in this repo. A small committed synthetic labelled set
//! (`corpus/fixtures/paragraphs/`) is used instead, and the reflow threshold
//! derived from it ([`PROVISIONAL_REFLOW_THRESHOLD`]) is explicitly provisional.

use selis_bytes::Bytes;
use selis_error::Result;
use selis_geom::Rect;
use selis_sandbox::{alloc, BudgetGuard};

use crate::assembly::TextLine;
use crate::order::ReadingOrder;

/// A baseline gap wider than this multiple of the taller line's em starts a
/// new paragraph. Mirrors `reflow::PARAGRAPH_GAP_RATIO` (ordinary leading is
/// 1.2–1.5 em; a blank half-line is wider).
const PARAGRAPH_GAP_RATIO: f64 = 1.6;
/// An indent change wider than this multiple of the em starts a new paragraph.
const INDENT_RATIO: f64 = 0.9;
/// A size change larger than this fraction of the em starts a new paragraph.
const SIZE_TOLERANCE: f64 = 0.15;
/// An MCID jump of more than one across a gap this wide corroborates a break.
const MCID_JUMP_GAP_RATIO: f64 = 1.2;

/// Confidence weights. They sum to 1.0; changing them changes the reported
/// number and therefore the provisional threshold derived from the labelled set.
const W_BASELINE: f64 = 0.30;
const W_SPACING: f64 = 0.25;
const W_FONT: f64 = 0.25;
const W_STRUCTURE: f64 = 0.20;

/// The provisional reflow-editing threshold derived from the small labelled
/// set: the midpoint between the lowest correct-grouping confidence and the
/// highest incorrect-grouping confidence measured by the harness in
/// [`confidence_for`]'s tests. **Provisional** — the DoD's 500-page labelled
/// set is not available, so this is not a measured correlation threshold.
pub const PROVISIONAL_REFLOW_THRESHOLD: f64 = 0.80;

/// The dominant typographic style of a paragraph.
#[derive(Debug, Clone, PartialEq)]
pub struct ParagraphStyle {
    /// The font resource name carried by the paragraph's first glyph.
    pub font: Bytes,
    /// The paragraph's dominant (largest) glyph size, in user space.
    pub size: f64,
}

/// One reconstructed paragraph.
#[derive(Debug, Clone, PartialEq)]
pub struct Paragraph {
    /// The lines in this paragraph, in reading order.
    pub lines: Vec<TextLine>,
    /// The union of the paragraphs' line boxes, in user space.
    pub bbox: Rect,
    /// The dominant font and size, so an editor can style replacement text.
    pub style: ParagraphStyle,
    /// The lines' recovered text, joined with a single space.
    pub text: String,
}

/// The confidence in a paragraph reconstruction: four 0..1 signals and their
/// weighted aggregate.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Confidence {
    /// The aggregate, weighted sum of the four signals, clamped to 0..1.
    pub value: f64,
    /// Tightness of the within-paragraph baseline gaps.
    pub baseline_regularity: f64,
    /// Separation and uniformity of the between-paragraph gaps.
    pub spacing_consistency: f64,
    /// Fraction of within-paragraph line pairs sharing font and size.
    pub font_uniformity: f64,
    /// MCID coverage/monotonicity, with tagged order as a positive prior.
    pub structure_agreement: f64,
}

/// A styled paragraph model: the paragraphs plus the confidence in them.
///
/// `infer_paragraphs` returns the tuple `(Vec<Paragraph>, Confidence)` from
/// the spec's API; this struct is the same result under one name for callers
/// that want to carry them together (e.g. the editor's model cache).
#[derive(Debug, Clone, PartialEq)]
pub struct ParagraphModel {
    /// The reconstructed paragraphs, in reading order.
    pub paragraphs: Vec<Paragraph>,
    /// The confidence in the reconstruction.
    pub confidence: Confidence,
}

impl ParagraphModel {
    /// Bundle the two halves returned by [`infer_paragraphs`].
    #[must_use]
    pub const fn new(paragraphs: Vec<Paragraph>, confidence: Confidence) -> Self {
        Self {
            paragraphs,
            confidence,
        }
    }
}

/// Group already-assembled, already-ordered lines into paragraphs and compute
/// a confidence for the result.
///
/// `reading` is the strategy that produced `lines`: a tagged order lifts the
/// structure-agreement signal, a geometric order does not. The lines are
/// consumed by reference — the returned [`Paragraph`]s clone them so the model
/// owns its content.
///
/// # Budget
///
/// The grouping buffer and the output vector are charged to `g` before
/// allocation; malformed input cannot grow either beyond `lines.len()`.
///
/// # Malformed Input
///
/// Lines without glyphs, non-finite baselines, zero/negative gaps and missing
/// MCIDs never panic: they contribute no evidence to a signal and are grouped
/// by the remaining signals. An empty input yields no paragraphs and a zero
/// confidence (no evidence, not "certain").
///
/// # Errors
///
/// `BUDGET_BYTES` when the grouping buffer or the output vector cannot be
/// budgeted.
pub fn infer_paragraphs(
    lines: &[TextLine],
    reading: &ReadingOrder,
    g: &mut BudgetGuard<'_>,
) -> Result<(Vec<Paragraph>, Confidence)> {
    let groups = group_lines(lines, reading, g)?;
    let confidence = confidence_for(lines, &groups, reading);
    let paragraphs = build_paragraphs(lines, &groups, g)?;
    Ok((paragraphs, confidence))
}

/// [`infer_paragraphs`] with the two halves bundled as a [`ParagraphModel`].
///
/// # Budget
///
/// Identical to [`infer_paragraphs`]: the grouping and output buffers are the
/// only budgeted allocations.
///
/// # Malformed Input
///
/// Identical to [`infer_paragraphs`]; there are no extra panics.
///
/// # Errors
///
/// `BUDGET_BYTES` when a buffer cannot be budgeted.
pub fn infer_model(
    lines: &[TextLine],
    reading: &ReadingOrder,
    g: &mut BudgetGuard<'_>,
) -> Result<ParagraphModel> {
    let (paragraphs, confidence) = infer_paragraphs(lines, reading, g)?;
    Ok(ParagraphModel::new(paragraphs, confidence))
}

/// Compute the confidence of an *arbitrary* grouping of `lines`.
///
/// `groups` holds line indices into `lines`, in reading order; a caller (a
/// labeller, a UI offering a corrected grouping) can therefore ask what
/// confidence the model would assign to a grouping it did not produce. This is
/// the function the labelled-set harness uses to rank correct groupings above
/// incorrect ones.
///
/// # Budget
///
/// None: no allocation is made — each signal is a streaming reduction over the
/// groups it is given.
///
/// # Malformed Input
///
/// Out-of-range indices, empty groups and non-finite coordinates are ignored
/// rather than indexed or panicked on. A grouping with no usable evidence
/// scores 0.0 on the signals that need it.
#[must_use]
pub fn confidence_for(
    lines: &[TextLine],
    groups: &[Vec<usize>],
    reading: &ReadingOrder,
) -> Confidence {
    if lines.is_empty() || groups.is_empty() {
        return Confidence {
            value: 0.0,
            baseline_regularity: 0.0,
            spacing_consistency: 0.0,
            font_uniformity: 0.0,
            structure_agreement: 0.0,
        };
    }
    let baseline_regularity = baseline_regularity(lines, groups);
    let spacing_consistency = spacing_consistency(lines, groups);
    let font_uniformity = font_uniformity(lines, groups);
    let structure_agreement = structure_agreement(lines, groups, reading);
    let value = W_BASELINE * baseline_regularity
        + W_SPACING * spacing_consistency
        + W_FONT * font_uniformity
        + W_STRUCTURE * structure_agreement;
    let value = if value.is_finite() {
        value.clamp(0.0, 1.0)
    } else {
        0.0
    };
    Confidence {
        value,
        baseline_regularity,
        spacing_consistency,
        font_uniformity,
        structure_agreement,
    }
}

/// Group lines by bounded adjacent-pair decisions.
fn group_lines(
    lines: &[TextLine],
    reading: &ReadingOrder,
    g: &mut BudgetGuard<'_>,
) -> Result<Vec<Vec<usize>>> {
    let mut groups: Vec<Vec<usize>> = alloc::vec_with_capacity(g, lines.len())?;
    for (index, line) in lines.iter().enumerate() {
        let previous = groups
            .last()
            .and_then(|group| group.last())
            .copied()
            .and_then(|idx| lines.get(idx));
        if previous.is_some_and(|prev| should_join(prev, line, reading)) {
            if let Some(group) = groups.last_mut() {
                alloc::grow(g, group, 1)?;
                group.push(index);
            }
        } else {
            let mut group = alloc::vec_with_capacity(g, 1)?;
            group.push(index);
            groups.push(group);
        }
    }
    Ok(groups)
}

/// Whether `cur` continues the paragraph `prev` ends.
///
/// Baseline-gap clustering is the primary rule; font/size uniformity, indent
/// stability and (for tagged documents) an MCID jump corroborate or veto it.
fn should_join(prev: &TextLine, cur: &TextLine, reading: &ReadingOrder) -> bool {
    let height = line_size(prev).max(line_size(cur)).max(1.0);
    let gap = (line_baseline(prev) - line_baseline(cur)).abs();
    if !gap.is_finite() || gap > height * PARAGRAPH_GAP_RATIO {
        return false;
    }
    if (line_size(cur) - line_size(prev)).abs() > height * SIZE_TOLERANCE {
        return false;
    }
    if line_font(prev) != line_font(cur) {
        return false;
    }
    if (cur.bbox.x0 - prev.bbox.x0).abs() > height * INDENT_RATIO {
        return false;
    }
    if *reading == ReadingOrder::Structure {
        if let (Some(a), Some(b)) = (line_mcid(prev), line_mcid(cur)) {
            if b.saturating_sub(a) > 1 && gap > height * MCID_JUMP_GAP_RATIO {
                return false;
            }
        }
    }
    true
}

/// Materialise the paragraphs from the line-index groups.
fn build_paragraphs(
    lines: &[TextLine],
    groups: &[Vec<usize>],
    g: &mut BudgetGuard<'_>,
) -> Result<Vec<Paragraph>> {
    let mut out: Vec<Paragraph> = alloc::vec_with_capacity(g, groups.len())?;
    for group in groups {
        let mut bbox: Option<Rect> = None;
        let mut font: Option<Bytes> = None;
        let mut size = 0.0_f64;
        let mut text = String::new();
        let mut owned: Vec<TextLine> = Vec::new();
        for index in group {
            let Some(line) = lines.get(*index) else {
                continue;
            };
            bbox = Some(bbox.map_or(line.bbox, |b| b.union(line.bbox)));
            if font.is_none() {
                font = line_font(line);
            }
            size = size.max(line_size(line));
            let line_text = line_text(line);
            if !line_text.is_empty() {
                if !text.is_empty() {
                    text.push(' ');
                }
                text.push_str(&line_text);
            }
            owned.push(line.clone());
        }
        let Some(bbox) = bbox else { continue };
        out.push(Paragraph {
            lines: owned,
            bbox,
            style: ParagraphStyle {
                font: font.unwrap_or_default(),
                size,
            },
            text,
        });
    }
    Ok(out)
}

/// Signal 1: within-paragraph baseline gaps should be a regular rhythm.
///
/// For each group with at least two lines, the gaps between consecutive line
/// baselines are collected. Their coefficient of variation `cv = sd/mean` is
/// mapped to `1/(1+cv)` — a perfect rhythm scores 1, a ragged mixture of
/// half-lines and whole lines tends to 0. Groups are weighted by their gap
/// count so a long paragraph counts more than a two-liner. With no usable gaps
/// the signal is 0 (no evidence).
fn baseline_regularity(lines: &[TextLine], groups: &[Vec<usize>]) -> f64 {
    let mut weighted = 0.0_f64;
    let mut weight = 0.0_f64;
    for group in groups {
        let mut gaps: Vec<f64> = Vec::new();
        for pair in group.windows(2) {
            let (Some(&a), Some(&b)) = (pair.first(), pair.get(1)) else {
                continue;
            };
            if let Some(gap) = adjacent_gap(lines, a, b) {
                gaps.push(gap);
            }
        }
        if gaps.is_empty() {
            continue;
        }
        let regularity = 1.0 / (1.0 + cv(&gaps));
        let n = gaps.len() as f64;
        weighted += regularity * n;
        weight += n;
    }
    if weight > 0.0 {
        (weighted / weight).clamp(0.0, 1.0)
    } else {
        0.0
    }
}

/// Signal 2: between-paragraph gaps should be distinctly and consistently
/// wider than within-paragraph gaps.
///
/// `ratio = mean_between / mean_within`; the separation term `1 - exp(1-ratio)`
/// is 0 at `ratio = 1` and approaches 1 as the between-gaps dominate. It is
/// multiplied by `1/(1+cv_between)`, so a page whose paragraph gaps vary wildly
/// is less certain than one with a uniform blank line. A single-paragraph page
/// has no boundary evidence and scores 0.
fn spacing_consistency(lines: &[TextLine], groups: &[Vec<usize>]) -> f64 {
    let mut within: Vec<f64> = Vec::new();
    let mut between: Vec<f64> = Vec::new();
    for group in groups {
        for pair in group.windows(2) {
            let (Some(&a), Some(&b)) = (pair.first(), pair.get(1)) else {
                continue;
            };
            if let Some(gap) = adjacent_gap(lines, a, b) {
                within.push(gap);
            }
        }
    }
    for pair in groups.windows(2) {
        let (Some(last), Some(next)) = (pair.first(), pair.get(1)) else {
            continue;
        };
        let (Some(&a), Some(&b)) = (last.last(), next.first()) else {
            continue;
        };
        if let Some(gap) = adjacent_gap(lines, a, b) {
            between.push(gap);
        }
    }
    if between.is_empty() {
        return 0.0;
    }
    let ratio = mean(&between) / mean(&within).max(1.0e-6);
    let separation = (1.0 - (1.0 - ratio).exp()).clamp(0.0, 1.0);
    let uniformity = 1.0 / (1.0 + cv(&between));
    (separation * uniformity).clamp(0.0, 1.0)
}

/// Signal 3: adjacent lines inside one paragraph should share font and size.
///
/// A pair with no glyphs cannot be evaluated and is skipped; a page with no
/// evaluable pairs scores 0 (no evidence). A single font/size page scores 1.
fn font_uniformity(lines: &[TextLine], groups: &[Vec<usize>]) -> f64 {
    let mut total = 0.0_f64;
    let mut uniform = 0.0_f64;
    for group in groups {
        for pair in group.windows(2) {
            let (Some(&a), Some(&b)) = (pair.first(), pair.get(1)) else {
                continue;
            };
            let (Some(la), Some(lb)) = (lines.get(a), lines.get(b)) else {
                continue;
            };
            total += 1.0;
            let taller = line_size(la).max(line_size(lb)).max(1.0);
            if line_font(la) == line_font(lb)
                && (line_size(la) - line_size(lb)).abs() <= taller * SIZE_TOLERANCE
            {
                uniform += 1.0;
            }
        }
    }
    if total > 0.0 {
        (uniform / total).clamp(0.0, 1.0)
    } else {
        0.0
    }
}

/// Signal 4: the structure tree's MCIDs should cover the lines in a monotone
/// reading order.
///
/// `signal` is the monotone fraction over adjacent line pairs, with pairs that
/// lack MCIDs treated as a neutral 0.5 rather than a penalty (untagged pages
/// have no structure to disagree). A tagged order starts from a 0.6 prior, a
/// geometric one from 0.5, and up to 0.4 is added from the measured signal: a
/// fully covered monotone tagged page scores 1.0, an untagged page with no
/// MCIDs scores 0.7.
fn structure_agreement(lines: &[TextLine], groups: &[Vec<usize>], reading: &ReadingOrder) -> f64 {
    let order: Vec<usize> = groups
        .iter()
        .flat_map(|group| group.iter().copied())
        .collect();
    let total_pairs = order.len().saturating_sub(1);
    let mut known = 0.0_f64;
    let mut monotone = 0.0_f64;
    for pair in order.windows(2) {
        let (Some(&a), Some(&b)) = (pair.first(), pair.get(1)) else {
            continue;
        };
        let (Some(la), Some(lb)) = (lines.get(a), lines.get(b)) else {
            continue;
        };
        if let (Some(ma), Some(mb)) = (line_mcid(la), line_mcid(lb)) {
            known += 1.0;
            if mb >= ma {
                monotone += 1.0;
            }
        }
    }
    let coverage = if total_pairs > 0 {
        known / total_pairs as f64
    } else {
        0.0
    };
    let mono = if known > 0.0 { monotone / known } else { 0.5 };
    let signal = coverage * mono + (1.0 - coverage) * 0.5;
    let prior = if *reading == ReadingOrder::Structure {
        0.6
    } else {
        0.5
    };
    (prior + 0.4 * signal).clamp(0.0, 1.0)
}

/// The baseline of a line: its first glyph's origin, falling back to the box's
/// lower edge when the line carries no glyph or non-finite coordinates.
///
/// The model is calibrated for horizontal, top-to-bottom lines — the shape the
/// assembler produces for the overwhelming majority of documents (and the
/// shape the labelled set exercises). Vertical writing still gets a stable
/// number from the box, but the gap ratios are not calibrated for it.
fn line_baseline(line: &TextLine) -> f64 {
    let baseline = first_glyph(line).map_or(line.bbox.y0, |g| g.at.y);
    if baseline.is_finite() {
        baseline
    } else {
        line.bbox.y0
    }
}

/// The dominant size of a line: its first glyph's em, or 0 if none.
fn line_size(line: &TextLine) -> f64 {
    first_glyph(line).map_or(0.0, |g| {
        if g.size.is_finite() && g.size > 0.0 {
            g.size
        } else {
            0.0
        }
    })
}

/// The line's first glyph's font, if any.
fn line_font(line: &TextLine) -> Option<Bytes> {
    first_glyph(line).map(|g| g.font.clone())
}

/// The line's first glyph's MCID, if any.
fn line_mcid(line: &TextLine) -> Option<u32> {
    first_glyph(line).and_then(|g| g.mcid)
}

/// The first glyph of a line, if any.
fn first_glyph(line: &TextLine) -> Option<&selis_pdf_content::text::TextGlyph> {
    line.words
        .iter()
        .flat_map(|word| word.runs.iter())
        .flat_map(|run| run.glyphs.iter())
        .next()
}

/// The recovered text of a line: words joined by a single space, characters
/// read from the recovered glyph codes — the same rule the extractor uses.
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

/// The baseline gap between two line indices, or `None` for a missing,
/// non-finite or non-positive gap.
fn adjacent_gap(lines: &[TextLine], a: usize, b: usize) -> Option<f64> {
    let la = lines.get(a)?;
    let lb = lines.get(b)?;
    let gap = (line_baseline(la) - line_baseline(lb)).abs();
    if gap.is_finite() && gap > 0.0 {
        Some(gap)
    } else {
        None
    }
}

/// The arithmetic mean, or 0 for an empty slice.
fn mean(values: &[f64]) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    values.iter().sum::<f64>() / values.len() as f64
}

/// The coefficient of variation `sd/mean`, capped to a large finite value so
/// a near-zero mean cannot make the derived signal infinite or NaN.
fn cv(values: &[f64]) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    let m = mean(values);
    if m <= 0.0 {
        return 1.0e6;
    }
    let variance = values
        .iter()
        .map(|v| {
            let d = v - m;
            d * d
        })
        .sum::<f64>()
        / values.len() as f64;
    let sd = variance.sqrt();
    if sd.is_finite() {
        (sd / m).min(1.0e6)
    } else {
        1.0e6
    }
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::arithmetic_side_effects,
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        clippy::indexing_slicing,
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic
    )]

    use super::*;
    use selis_bytes::Bytes;
    use selis_geom::{Matrix, Point, Rect};
    use selis_pdf_content::text::TextGlyph;

    fn budget_guard() -> BudgetGuard<'static> {
        selis_sandbox::Budget::unlimited().guard()
    }

    /// A line whose first glyph sits at `(72, y)` with the given font/size/MCID.
    fn line(y: f64, size: f64, font: &str, mcid: Option<u32>, text: &str) -> TextLine {
        let glyphs: Vec<TextGlyph> = text
            .chars()
            .enumerate()
            .map(|(i, ch)| TextGlyph {
                code: u16::try_from(u32::from(ch)).unwrap_or(0xFFFD),
                at: Point::new(72.0 + size * 0.5 * i as f64, y),
                tm: Matrix::IDENTITY,
                advance: size * 0.5,
                font: Bytes::copy_from_slice(font.as_bytes()),
                size,
                space: size * 0.25,
                dir_x: 1.0,
                dir_y: 0.0,
                mcid,
            })
            .collect();
        let bbox = Rect::new(72.0, y, 72.0 + size * 4.0, y + size);
        TextLine {
            words: vec![crate::assembly::TextWord {
                runs: vec![crate::assembly::TextRun { glyphs, bbox }],
                bbox,
            }],
            bbox,
        }
    }

    /// Four lines: two tight lines, a wide gap, two tight lines.
    fn two_paragraph_lines() -> Vec<TextLine> {
        vec![
            line(800.0, 10.0, "F1", Some(1), "First paragraph line one"),
            line(786.0, 10.0, "F1", Some(2), "First paragraph line two"),
            line(740.0, 10.0, "F1", Some(3), "Second paragraph line one"),
            line(726.0, 10.0, "F1", Some(4), "Second paragraph line two"),
        ]
    }

    #[test]
    fn two_paragraphs_are_recovered_with_text_and_style() {
        let lines = two_paragraph_lines();
        let (paragraphs, confidence) =
            infer_paragraphs(&lines, &ReadingOrder::Structure, &mut budget_guard())
                .expect("budget");
        assert_eq!(paragraphs.len(), 2);
        assert_eq!(paragraphs[0].lines.len(), 2);
        assert_eq!(paragraphs[1].lines.len(), 2);
        assert_eq!(
            paragraphs[0].text,
            "First paragraph line one First paragraph line two"
        );
        assert_eq!(paragraphs[1].style.font.as_slice(), b"F1");
        assert_eq!(paragraphs[1].style.size, 10.0);
        assert!(confidence.value > 0.6, "a clean tagged page is confident");
    }

    #[test]
    fn a_font_change_breaks_a_paragraph() {
        let lines = vec![
            line(800.0, 10.0, "F1", None, "same style"),
            line(786.0, 10.0, "F1", None, "same style again"),
            line(772.0, 10.0, "F2", None, "bold lead in"),
            line(758.0, 10.0, "F2", None, "bold continues"),
        ];
        let (paragraphs, _) =
            infer_paragraphs(&lines, &ReadingOrder::Geometry, &mut budget_guard()).expect("budget");
        assert_eq!(paragraphs.len(), 2);
        assert_eq!(paragraphs[0].lines.len(), 2);
        assert_eq!(paragraphs[1].lines.len(), 2);
    }

    #[test]
    fn a_size_change_breaks_a_paragraph() {
        let lines = vec![
            line(800.0, 10.0, "F1", None, "body"),
            line(786.0, 10.0, "F1", None, "body"),
            line(760.0, 18.0, "F1", None, "Large heading"),
        ];
        let (paragraphs, _) =
            infer_paragraphs(&lines, &ReadingOrder::Geometry, &mut budget_guard()).expect("budget");
        assert_eq!(paragraphs.len(), 2);
    }

    #[test]
    fn empty_input_has_no_paragraphs_and_no_evidence() {
        let (paragraphs, confidence) =
            infer_paragraphs(&[], &ReadingOrder::Geometry, &mut budget_guard()).expect("budget");
        assert!(paragraphs.is_empty());
        assert_eq!(confidence.value, 0.0);
    }

    #[test]
    fn malformed_lines_do_not_panic() {
        let lines = vec![
            line(800.0, 10.0, "F1", None, "ok"),
            TextLine {
                words: Vec::new(),
                bbox: Rect::new(0.0, f64::NAN, 0.0, f64::NAN),
            },
            line(700.0, 0.0, "F1", None, ""),
        ];
        let (paragraphs, confidence) =
            infer_paragraphs(&lines, &ReadingOrder::Structure, &mut budget_guard())
                .expect("budget");
        assert!(!paragraphs.is_empty());
        assert!(confidence.value.is_finite());
    }

    /// The small labelled set: every page's correct grouping must outrank the
    /// three wrong groupings the harness constructs, on each individual signal
    /// combination and on the aggregate. This is the evidence behind the
    /// (provisional) threshold, *not* the DoD's 500-page correlation.
    #[test]
    fn labelled_set_ranks_correct_groupings_above_incorrect_ones() {
        let manifest: LabelledManifest = toml::from_str(include_str!(
            "../../../corpus/fixtures/paragraphs/manifest.toml"
        ))
        .expect("manifest parses");
        assert_eq!(manifest.version, 1);
        assert!(
            manifest.pages.len() >= 10,
            "committed labelled set is small but non-trivial"
        );

        let mut correct_min = f64::INFINITY;
        let mut incorrect_max = f64::NEG_INFINITY;
        for page in &manifest.pages {
            let lines: Vec<TextLine> = page
                .lines
                .iter()
                .map(|l| {
                    line(
                        l.y,
                        l.size,
                        &l.font,
                        (l.mcid >= 0).then_some(l.mcid as u32),
                        &l.text,
                    )
                })
                .collect();
            let reading = if page.tagged {
                ReadingOrder::Structure
            } else {
                ReadingOrder::Geometry
            };

            // The model must reproduce the hand-labelled boundaries.
            let (paragraphs, produced) =
                infer_paragraphs(&lines, &reading, &mut budget_guard()).expect("budget");
            let expected_lengths: Vec<usize> = labeled_lengths(&page.paragraph_ends, lines.len());
            let produced_lengths: Vec<usize> = paragraphs.iter().map(|p| p.lines.len()).collect();
            assert_eq!(
                produced_lengths, expected_lengths,
                "page {} grouping",
                page.id
            );

            let correct_groups = labeled_groups(&page.paragraph_ends, lines.len());
            let correct = confidence_for(&lines, &correct_groups, &reading);
            correct_min = correct_min.min(correct.value);
            assert!(
                correct.value > PROVISIONAL_REFLOW_THRESHOLD,
                "page {} correct grouping clears the provisional bar ({})",
                page.id,
                correct.value
            );
            assert!(
                (produced.value - correct.value).abs() < 1.0e-9,
                "produced confidence equals the harness confidence"
            );

            // Wrong grouping 1: merge everything into one paragraph.
            let merged = vec![(0..lines.len()).collect::<Vec<usize>>()];
            // Wrong grouping 2: split every line into its own paragraph.
            let split: Vec<Vec<usize>> = (0..lines.len()).map(|i| vec![i]).collect();
            // Wrong grouping 3: shift each boundary by one line where possible.
            let shifted = shifted_groups(&page.paragraph_ends, lines.len());
            for (name, wrong) in [
                ("merged", &merged),
                ("split", &split),
                ("shifted", &shifted),
            ] {
                let score = confidence_for(&lines, wrong, &reading);
                incorrect_max = incorrect_max.max(score.value);
                assert!(
                    correct.value > score.value,
                    "{}: correct {} must outrank {} {} on page {}",
                    name,
                    correct.value,
                    name,
                    score.value,
                    page.id
                );
            }
        }
        eprintln!(
            "TEXTEDIT.01 labelled set: min correct = {correct_min:.4}, max wrong = {incorrect_max:.4}, midpoint = {:.4}",
            (correct_min + incorrect_max) / 2.0
        );
        assert!(
            correct_min > incorrect_max,
            "every correct grouping outranks every incorrect one (min correct {correct_min}, max wrong {incorrect_max})"
        );
        // The committed constant is the midpoint measured on this set. Keep it
        // honest: if the set is edited, this assertion forces a conscious update.
        let midpoint = (correct_min + incorrect_max) / 2.0;
        assert!(
            (midpoint - PROVISIONAL_REFLOW_THRESHOLD).abs() < 0.05,
            "provisional threshold {PROVISIONAL_REFLOW_THRESHOLD} is derived from the set (midpoint {midpoint})"
        );
    }

    #[derive(serde::Deserialize)]
    struct LabelledManifest {
        version: u32,
        #[allow(dead_code)]
        note: String,
        #[serde(rename = "page")]
        pages: Vec<LabelledPage>,
    }

    #[derive(serde::Deserialize)]
    struct LabelledPage {
        id: String,
        tagged: bool,
        lines: Vec<LabelledLine>,
        paragraph_ends: Vec<usize>,
    }

    #[derive(serde::Deserialize)]
    struct LabelledLine {
        y: f64,
        size: f64,
        font: String,
        text: String,
        /// -1 means "no MCID".
        mcid: i64,
    }

    fn labeled_groups(ends: &[usize], line_count: usize) -> Vec<Vec<usize>> {
        let mut out = Vec::new();
        let mut start = 0;
        for &end in ends {
            out.push((start..end.min(line_count)).collect::<Vec<usize>>());
            start = end.min(line_count);
        }
        if start < line_count {
            out.push((start..line_count).collect::<Vec<usize>>());
        }
        out
    }

    fn labeled_lengths(ends: &[usize], line_count: usize) -> Vec<usize> {
        labeled_groups(ends, line_count)
            .iter()
            .map(Vec::len)
            .collect()
    }

    fn shifted_groups(ends: &[usize], line_count: usize) -> Vec<Vec<usize>> {
        let shifted: Vec<usize> = ends
            .iter()
            .map(|&e| if e + 1 < line_count { e + 1 } else { e })
            .collect();
        labeled_groups(&shifted, line_count)
    }
}
