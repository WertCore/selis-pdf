//! Reading order (SL-3.TEXT.04).
//!
//! Orders assembled lines per the document's structure tree (tagged order,
//! ADR-P0031) when present, falling back to a layout analysis: column
//! detection and an XY-cut-style order (column 1 top-to-bottom, then column
//! 2). A **confidence** is returned — the caller never silently interleaves a
//! two-column document into nonsense.
//!
//! The structure tree (SL-1.DOC.06) exposes the marked-content ids in logical
//! order; the caller associates each assembled line with its MCID.

use crate::assembly::TextLine;

/// The reading-order strategy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReadingOrder {
    /// The structure tree's marked-content order (tagged PDF).
    Structure,
    /// Geometric order inferred from the layout (XY-cut / column analysis).
    Geometry,
}

/// A line with its optional structure-tree marked-content id.
#[derive(Debug, Clone, PartialEq)]
pub struct LineWithMcid {
    /// The assembled line.
    pub line: TextLine,
    /// The marked-content id in the structure tree, if known.
    pub mcid: Option<u32>,
}

/// The result of ordering: the lines plus a confidence.
#[derive(Debug, Clone, PartialEq)]
pub struct OrderResult {
    /// The lines in reading order.
    pub lines: Vec<TextLine>,
    /// The strategy used.
    pub strategy: ReadingOrder,
    /// The confidence in the ordering: 1.0 is certain (tagged), values below
    /// are a measured guess. A two-column document with no clean column
    /// boundary is flagged low rather than silently interleaved.
    pub confidence: f64,
}

/// Order lines per the reading-order strategy.
///
/// `mcid_order` is the sequence of marked-content ids in the structure-tree
/// walk order (the tagged order). When present, lines are ordered by their
/// MCID's position in that walk; lines without an MCID sort after. Otherwise
/// a layout analysis (column detection + XY-cut) orders them, with a
/// confidence.
#[must_use]
pub fn order_lines(lines: Vec<LineWithMcid>, mcid_order: Option<&[u32]>) -> OrderResult {
    if let Some(order) = mcid_order {
        // Structure-first: rank by the MCID position in the structure walk.
        let mut ranked: Vec<(u64, TextLine)> = Vec::with_capacity(lines.len());
        for l in lines {
            let rank = match l.mcid {
                Some(mcid) => position(order, mcid).unwrap_or(u64::MAX),
                None => u64::MAX,
            };
            ranked.push((rank, l.line));
        }
        ranked.sort_by_key(|(rank, _)| *rank);
        return OrderResult {
            lines: ranked.into_iter().map(|(_, l)| l).collect(),
            strategy: ReadingOrder::Structure,
            confidence: 1.0,
        };
    }

    // Geometry: column detection then XY-cut ordering.
    let (columns, confidence) = detect_columns(&lines);
    let ordered = xy_cut_order(lines, &columns);
    OrderResult {
        lines: ordered,
        strategy: ReadingOrder::Geometry,
        confidence,
    }
}

/// The index of `mcid` in `order`, or `None`.
fn position(order: &[u32], mcid: u32) -> Option<u64> {
    order
        .iter()
        .position(|&m| m == mcid)
        .map(|i| u64::try_from(i).unwrap_or(u64::MAX))
}

/// Detect the column structure of a set of lines.
///
/// Returns the column boundaries (the x-ranges, sorted left-to-right) and a
/// confidence: 0.8 for a clean single column, 0.7 for clean multi-column, and
/// 0.4 when the column split is ambiguous.
fn detect_columns(lines: &[LineWithMcid]) -> (Vec<(f64, f64)>, f64) {
    if lines.is_empty() {
        return (Vec::new(), 0.8);
    }
    // The x-centres of the lines, sorted.
    let mut centres: Vec<f64> = lines
        .iter()
        .map(|l| (l.line.bbox.x0 + l.line.bbox.x1) / 2.0)
        .collect();
    centres.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));

    // The median line width.
    let widths: Vec<f64> = lines
        .iter()
        .map(|l| (l.line.bbox.x1 - l.line.bbox.x0).abs())
        .collect();
    let median_width = median(&widths).unwrap_or(0.0);

    // A column boundary is a gap between consecutive x-centres much larger
    // than the typical line width. Within a column, consecutive lines share
    // overlapping x-ranges (gaps ≪ a line width); between columns the gap is
    // large.
    let threshold = median_width * 1.5;
    let mut boundaries: Vec<f64> = Vec::new();
    for pair in centres.windows(2) {
        let a = pair.get(0).copied().unwrap_or(0.0);
        let b = pair.get(1).copied().unwrap_or(0.0);
        if (b - a).abs() > threshold {
            boundaries.push((a + b) / 2.0);
        }
    }

    if boundaries.is_empty() {
        // Single column: full x-range, confident.
        let x0 = lines
            .iter()
            .map(|l| l.line.bbox.x0)
            .fold(f64::INFINITY, f64::min);
        let x1 = lines
            .iter()
            .map(|l| l.line.bbox.x1)
            .fold(f64::NEG_INFINITY, f64::max);
        return (vec![(x0, x1)], 0.8);
    }

    // Build the column x-ranges from the boundaries.
    let mut cols: Vec<(f64, f64)> = Vec::new();
    let mut start = lines
        .iter()
        .map(|l| l.line.bbox.x0)
        .fold(f64::INFINITY, f64::min);
    for b in boundaries {
        cols.push((start, b));
        start = b;
    }
    let end = lines
        .iter()
        .map(|l| l.line.bbox.x1)
        .fold(f64::NEG_INFINITY, f64::max);
    cols.push((start, end));

    // Confidence: a clean multi-column split (each column has comparable
    // width) is confident; otherwise it's a guess.
    let ambiguous = cols.len() > 3
        || cols
            .iter()
            .any(|(x0, x1)| (x1 - x0).abs() < median_width * 0.3);
    (cols, if ambiguous { 0.4 } else { 0.7 })
}

/// Order lines by an XY-cut: column 1 top-to-bottom, then column 2
/// top-to-bottom.
fn xy_cut_order(lines: Vec<LineWithMcid>, columns: &[(f64, f64)]) -> Vec<TextLine> {
    let mut cols: Vec<Vec<TextLine>> = columns.iter().map(|_| Vec::new()).collect();
    for l in lines {
        let cx = (l.line.bbox.x0 + l.line.bbox.x1) / 2.0;
        let idx = columns
            .iter()
            .position(|(x0, x1)| cx >= *x0 && cx <= *x1)
            .unwrap_or(0);
        if let Some(col) = cols.get_mut(idx) {
            col.push(l.line);
        }
    }
    let mut out = Vec::new();
    for col in &mut cols {
        col.sort_by(|a, b| {
            let ay = (a.bbox.y0 + a.bbox.y1) / 2.0;
            let by = (b.bbox.y0 + b.bbox.y1) / 2.0;
            by.partial_cmp(&ay).unwrap_or(std::cmp::Ordering::Equal)
        });
        out.extend(col.drain(..));
    }
    out
}

/// The median of a sorted-able slice, or `None`.
fn median(values: &[f64]) -> Option<f64> {
    if values.is_empty() {
        return None;
    }
    let mut sorted = values.to_vec();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let mid = sorted.len().saturating_div(2);
    if sorted.len() % 2 == 1 {
        Some(*sorted.get(mid)?)
    } else {
        let lo = *sorted.get(mid.saturating_sub(1))?;
        let hi = *sorted.get(mid)?;
        Some((lo + hi) / 2.0)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::indexing_slicing, clippy::arithmetic_side_effects)]

    use super::*;
    use selis_bytes::Bytes;
    use selis_geom::Point;
    use selis_pdf_content::text::TextGlyph;

    fn line_at(x0: f64, y0: f64, x1: f64, y1: f64) -> TextLine {
        let g = TextGlyph {
            code: 65,
            at: Point::new(x0, y0),
            font: Bytes::copy_from_slice(b"F1"),
            size: 12.0,
            mcid: None,
        };
        let run = crate::assembly::TextRun {
            glyphs: vec![g],
            bbox: selis_geom::Rect::new(x0, y0, x1, y1),
        };
        let word = crate::assembly::TextWord {
            runs: vec![run],
            bbox: selis_geom::Rect::new(x0, y0, x1, y1),
        };
        TextLine {
            words: vec![word],
            bbox: selis_geom::Rect::new(x0, y0, x1, y1),
        }
    }

    #[test]
    fn geometry_orders_top_to_bottom() {
        let lines = vec![
            LineWithMcid {
                line: line_at(0.0, 0.0, 50.0, 12.0),
                mcid: None,
            },
            LineWithMcid {
                line: line_at(0.0, 100.0, 50.0, 112.0),
                mcid: None,
            },
        ];
        let result = order_lines(lines, None);
        assert_eq!(result.strategy, ReadingOrder::Geometry);
        assert!(result.confidence > 0.7);
        assert!(result.lines[0].bbox.y0 > result.lines[1].bbox.y0);
    }

    #[test]
    fn structure_order_wins_when_mcid_order_is_given() {
        let lines = vec![
            LineWithMcid {
                line: line_at(0.0, 0.0, 50.0, 12.0),
                mcid: Some(1),
            },
            LineWithMcid {
                line: line_at(0.0, 100.0, 50.0, 112.0),
                mcid: Some(2),
            },
        ];
        let result = order_lines(lines, Some(&[2, 1]));
        assert_eq!(result.strategy, ReadingOrder::Structure);
        assert_eq!(result.confidence, 1.0);
        assert!(result.lines[0].bbox.y0 > result.lines[1].bbox.y0);
    }

    /// Two columns with a wide gap: column 1 top-to-bottom, then column 2.
    #[test]
    fn two_columns_order_left_then_right() {
        let lines = vec![
            // Right column, top.
            LineWithMcid {
                line: line_at(200.0, 100.0, 250.0, 112.0),
                mcid: None,
            },
            // Left column, top.
            LineWithMcid {
                line: line_at(0.0, 100.0, 50.0, 112.0),
                mcid: None,
            },
            // Left column, bottom.
            LineWithMcid {
                line: line_at(0.0, 0.0, 50.0, 12.0),
                mcid: None,
            },
        ];
        let result = order_lines(lines, None);
        assert_eq!(result.strategy, ReadingOrder::Geometry);
        // Left column first: the two left lines (top then bottom), then right.
        assert!(result.lines[0].bbox.x0 < 100.0);
        assert!(result.lines[1].bbox.x0 < 100.0);
        assert!(result.lines[2].bbox.x0 > 100.0);
        // Within the left column, top (y=100) before bottom (y=0).
        assert!(result.lines[0].bbox.y0 > result.lines[1].bbox.y0);
    }

    #[test]
    fn lines_without_mcid_sort_last() {
        let lines = vec![
            LineWithMcid {
                line: line_at(0.0, 0.0, 50.0, 12.0),
                mcid: None,
            },
            LineWithMcid {
                line: line_at(0.0, 50.0, 50.0, 62.0),
                mcid: Some(1),
            },
        ];
        let result = order_lines(lines, Some(&[1]));
        assert!(result.lines[0].bbox.y0 > result.lines[1].bbox.y0);
    }

    #[test]
    fn empty_input_is_empty() {
        let result = order_lines(Vec::new(), None);
        assert!(result.lines.is_empty());
    }
}
