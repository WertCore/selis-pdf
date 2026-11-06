//! Table reconstruction from positioned text (Phase 3 — table extraction).
//!
//! A PDF has no tables, only words at coordinates. This module infers one from
//! the only evidence the document actually holds: consecutive lines whose words
//! start at the same x positions. Nothing here invents a column the page does
//! not draw, and every table carries the share of its cells that landed in
//! their column — a confidence a reader can judge rather than a claim.
//!
//! # What this does not claim
//!
//! * **Borders.** A ruled table is not required and a drawn line is not read:
//!   alignment is the whole signal, so a two-column layout can be returned as a
//!   table when a reader would call it prose.
//! * **Merged cells.** A spanning cell appears in the column its left edge
//!   names; the columns it covers stay empty on that row.
//! * **Nested tables and rotated text.** Only running lines with ≥ 2 cells are
//!   grouped, so neither is reconstructed.

use selis_geom::Rect;

use crate::assembly::{TextLine, TextWord};

/// The fewest lines that make a table.
pub const MIN_TABLE_ROWS: usize = 2;
/// The fewest columns that make a table.
pub const MIN_TABLE_COLUMNS: usize = 2;
/// The smallest column tolerance, in points.
///
/// A column's cells start at "the same" x only up to the width of the ink; the
/// tolerance is half the line's type size, floored here so a 4 pt footnote
/// still tolerates a whole point of slack.
pub const MIN_COLUMN_TOLERANCE: f64 = 2.0;
/// How far apart two rows of one table may sit, as a multiple of the type size.
///
/// A table row is taller than a body line: the cell carries vertical padding
/// and, in a ruled table, a rule — so the next row's baseline can be two type
/// sizes away and still belong to the same table. The bound is deliberately
/// tighter than a blank line between paragraphs, which is how a run ends, but
/// wider than one leading, because generated tables (this repository's own
/// converter included) pad their cells.
pub const ROW_GAP_FACTOR: f64 = 2.6;

/// One reconstructed table on one page.
#[derive(Debug, Clone, PartialEq)]
pub struct Table {
    /// The page the table sits on, 1-based.
    pub page: u32,
    /// How many columns the table has.
    pub columns: usize,
    /// The rows, each holding one cell per column (an absent cell is empty).
    pub rows: Vec<Vec<String>>,
    /// The share of cells that started within tolerance of their column.
    pub confidence: f64,
    /// The box that holds the table's own lines.
    pub bbox: Rect,
}

/// One cell's evidence: where its ink starts, how tall it is, what it says.
#[derive(Debug, Clone, PartialEq)]
struct Cell {
    left: f64,
    right: f64,
    size: f64,
    text: String,
}

/// One line's cells.
#[derive(Debug, Clone, PartialEq)]
struct LineCells {
    cells: Vec<Cell>,
    bbox: Rect,
}

/// The recovered text of one word: its runs concatenated, no separator.
///
/// Words are already split by the assembler, so the space between two words is
/// geometry. Inside a cell that geometry becomes a single space — a cell that
/// holds two juxtaposed runs ("Total" + "4") is one piece of text, not two
/// cells, because the assembler said it was one word.
fn word_text(word: &TextWord) -> String {
    let mut out = String::new();
    for run in &word.runs {
        for glyph in &run.glyphs {
            if let Some(ch) = char::from_u32(u32::from(glyph.code)) {
                out.push(ch);
            }
        }
    }
    out
}

/// One line's cells, each word being a cell.
fn line_cells(line: &TextLine) -> LineCells {
    let mut cells = Vec::with_capacity(line.words.len());
    for word in &line.words {
        let text = word_text(word);
        if text.trim().is_empty() {
            continue;
        }
        cells.push(Cell {
            left: word.bbox.x0,
            right: word.bbox.x1,
            size: word
                .runs
                .iter()
                .flat_map(|run| run.glyphs.iter())
                .map(|glyph| glyph.size)
                .fold(0.0_f64, f64::max),
            text,
        });
    }
    LineCells {
        cells,
        bbox: line.bbox,
    }
}

/// The tolerance a run of lines is clustered with: half the tallest type size
/// in the run, floored at [`MIN_COLUMN_TOLERANCE`].
fn tolerance(lines: &[LineCells]) -> f64 {
    let tallest = lines
        .iter()
        .flat_map(|line| line.cells.iter())
        .map(|cell| cell.size)
        .fold(0.0_f64, f64::max);
    (tallest * 0.5).max(MIN_COLUMN_TOLERANCE)
}

/// Find every table on one page.
///
/// A run of consecutive lines with at least [`MIN_TABLE_COLUMNS`] cells each is
/// a table when it is at least [`MIN_TABLE_ROWS`] lines long. A run is broken
/// by a line with fewer cells (prose) and by a gap of more than one line height
/// (a new block), so two paragraphs that happen to share a left margin at two
/// x positions do not become a table.
#[must_use]
pub fn tables_on_page(lines: &[TextLine], page: u32) -> Vec<Table> {
    let numbered: Vec<LineCells> = lines.iter().map(line_cells).collect();
    let mut tables = Vec::new();
    let mut run: Vec<LineCells> = Vec::new();
    for line in numbered {
        let continues = run.last().is_some_and(|previous| {
            // The run continues while the next line's baseline sits within
            // [`ROW_GAP_FACTOR`] type sizes of the previous one. The measure is
            // the type size, not the box height: an assembled line's box can be
            // a bare baseline (zero height) and a zero-height bar would break
            // every real table. Cell padding and rules make a table row taller
            // than a body line, so the factor is above one leading.
            let gap = (previous.bbox.y0 - line.bbox.y0).abs();
            let size = previous
                .cells
                .iter()
                .chain(line.cells.iter())
                .map(|cell| cell.size)
                .fold(0.0_f64, f64::max)
                .max(1.0);
            gap <= size * ROW_GAP_FACTOR
        });
        if line.cells.len() >= MIN_TABLE_COLUMNS && (run.is_empty() || continues) {
            run.push(line);
            continue;
        }
        if let Some(table) = table_from_run(&run, page) {
            tables.push(table);
        }
        run.clear();
        if line.cells.len() >= MIN_TABLE_COLUMNS {
            run.push(line);
        }
    }
    if let Some(table) = table_from_run(&run, page) {
        tables.push(table);
    }
    tables
}

/// Turn one run of multi-cell lines into a table, if it is one.
fn table_from_run(run: &[LineCells], page: u32) -> Option<Table> {
    if run.len() < MIN_TABLE_ROWS {
        return None;
    }
    let tolerance = tolerance(run);
    // Column origins are the left edges more than one line agrees on. A single
    // cell that starts somewhere else is not a column of its own: it belongs to
    // no column, so its slot stays empty and the confidence reports the miss
    // instead of the table growing a column for one row.
    // The first line of the run names the columns: a header row is the page's
    // own statement of where its columns start. A column the first line does
    // not have is added only when at least two later lines agree on it, so one
    // stray cell cannot invent a column while a table whose first row is short
    // still keeps its real ones.
    let mut columns: Vec<f64> = Vec::new();
    if let Some(first) = run.first() {
        for cell in &first.cells {
            if columns
                .iter()
                .any(|origin| (cell.left - *origin).abs() <= tolerance)
            {
                continue;
            }
            columns.push(cell.left);
        }
    }
    let mut lefts: Vec<f64> = run
        .iter()
        .skip(1)
        .flat_map(|line| line.cells.iter().map(|cell| cell.left))
        .collect();
    lefts.sort_by(f64::total_cmp);
    let mut clusters: Vec<(f64, usize)> = Vec::new();
    for left in lefts {
        match clusters.last_mut() {
            Some((origin, count)) if (left - *origin).abs() <= tolerance => {
                *count = count.saturating_add(1);
            }
            _ => clusters.push((left, 1)),
        }
    }
    for (origin, count) in clusters {
        if count < MIN_TABLE_ROWS {
            continue;
        }
        if columns
            .iter()
            .any(|column| (origin - *column).abs() <= tolerance)
        {
            continue;
        }
        columns.push(origin);
    }
    columns.sort_by(f64::total_cmp);
    if columns.len() < MIN_TABLE_COLUMNS {
        return None;
    }
    let mut matched = 0usize;
    let mut total = 0usize;
    let mut rows: Vec<Vec<String>> = Vec::with_capacity(run.len());
    for line in run {
        let mut row = vec![String::new(); columns.len()];
        let mut last_column: Option<usize> = None;
        for cell in &line.cells {
            total = total.saturating_add(1);
            let nearest = columns
                .iter()
                .enumerate()
                .filter(|(index, _)| last_column.is_none_or(|last| *index > last))
                .min_by(|(_, a), (_, b)| {
                    (cell.left - **a).abs().total_cmp(&(cell.left - **b).abs())
                });
            let Some((index, origin)) = nearest else {
                continue;
            };
            if (cell.left - *origin).abs() <= tolerance {
                matched = matched.saturating_add(1);
                if let Some(slot) = row.get_mut(index) {
                    if slot.is_empty() {
                        *slot = cell.text.clone();
                    } else {
                        slot.push(' ');
                        slot.push_str(&cell.text);
                    }
                }
                last_column = Some(index);
            }
        }
        rows.push(row);
    }
    // A cell that matches no column leaves its column empty but still counts
    // against the confidence: the report is about the page's evidence, not
    // about how much of it the reconstruction kept.
    let confidence = if total == 0 {
        0.0
    } else {
        let ratio = (matched as f64) / (total as f64);
        (ratio * 100.0).round() / 100.0
    };
    let mut bbox = run
        .first()
        .map(|line| line.bbox)
        .unwrap_or(Rect::new(0.0, 0.0, 0.0, 0.0));
    for line in run {
        bbox = Rect::new(
            bbox.x0.min(line.bbox.x0),
            bbox.y0.min(line.bbox.y0),
            bbox.x1.max(line.bbox.x1),
            bbox.y1.max(line.bbox.y1),
        );
    }
    Some(Table {
        page,
        columns: columns.len(),
        rows,
        confidence,
        bbox,
    })
}

/// Quote one CSV field when it needs quoting.
fn csv_field(value: &str) -> String {
    let needs_quotes = value.contains([',', '"', '\n', '\r']) || value.trim() != value;
    if !needs_quotes {
        return value.to_string();
    }
    let mut out = String::from("\"");
    for ch in value.chars() {
        if ch == '"' {
            out.push('"');
        }
        out.push(ch);
    }
    out.push('"');
    out
}

/// Every table as one CSV file.
///
/// Tables are separated by a `#` comment line naming the page, the table on
/// that page and its confidence, so the file is self-describing without a side
/// channel. A `#` comment is not part of RFC 4180; it is used because a reader
/// that ignores comments still sees every row, and a reader that does not needs
/// to know where one table ends and the next begins.
#[must_use]
pub fn to_csv(tables: &[Table]) -> String {
    let mut out = String::new();
    for (index, table) in tables.iter().enumerate() {
        if index > 0 {
            out.push('\n');
        }
        out.push_str(&format!(
            "# page {}, table {} ({} columns, confidence {:.2})\n",
            table.page,
            index.saturating_add(1),
            table.columns,
            table.confidence
        ));
        for row in &table.rows {
            for (column, cell) in row.iter().enumerate() {
                if column > 0 {
                    out.push(',');
                }
                out.push_str(&csv_field(cell));
            }
            out.push('\n');
        }
    }
    out
}

#[cfg(test)]
mod tests {
    #![allow(clippy::indexing_slicing, clippy::arithmetic_side_effects)]

    use super::*;
    use selis_bytes::Bytes;
    use selis_geom::{Point, Rect};
    use selis_pdf_content::text::TextGlyph;

    fn glyph(code: u16, size: f64, x: f64) -> TextGlyph {
        TextGlyph {
            advance: size * 0.5,
            code,
            at: Point::new(x, 0.0),
            tm: selis_geom::Matrix::IDENTITY,
            font: Bytes::copy_from_slice(b"F1"),
            size,
            space: size * 0.25,
            dir_x: 1.0,
            dir_y: 0.0,
            mcid: None,
        }
    }

    /// One line from (text, x, width) cells, at `y`.
    fn line(cells: &[(&str, f64, f64)], y: f64, size: f64) -> TextLine {
        let words = cells
            .iter()
            .map(|(text, x, width)| {
                let glyphs = text
                    .chars()
                    .enumerate()
                    .map(|(index, ch)| {
                        let advance = size * 0.5;
                        glyph(ch as u16, size, x + advance * index as f64)
                    })
                    .collect();
                TextWord {
                    runs: vec![crate::assembly::TextRun {
                        glyphs,
                        bbox: Rect::new(*x, y, x + width, y + size),
                    }],
                    bbox: Rect::new(*x, y, x + width, y + size),
                }
            })
            .collect();
        TextLine {
            words,
            bbox: Rect::new(
                0.0,
                y,
                cells.iter().map(|(_, x, w)| x + w).fold(0.0, f64::max),
                y + size,
            ),
        }
    }

    #[test]
    fn three_aligned_rows_are_one_table() {
        let lines = vec![
            line(
                &[("Name", 10.0, 40.0), ("Amount", 200.0, 50.0)],
                700.0,
                10.0,
            ),
            line(&[("Ada", 10.0, 30.0), ("1200", 200.0, 30.0)], 685.0, 10.0),
            line(&[("Grace", 10.0, 40.0), ("900", 200.0, 25.0)], 670.0, 10.0),
        ];
        let tables = tables_on_page(&lines, 1);
        assert_eq!(tables.len(), 1);
        assert_eq!(tables[0].columns, 2);
        assert_eq!(tables[0].rows.len(), 3);
        assert_eq!(tables[0].rows[1][1], "1200");
        assert!(tables[0].confidence >= 0.99, "{}", tables[0].confidence);
    }

    #[test]
    fn padded_rows_are_one_table() {
        // 11 pt body, 1.4 leading, 4 pt cell padding and a rule: the next row
        // starts 23.9 pt below the last, which is 2.17 type sizes. A table the
        // convert card just wrote has to be readable by the same crate.
        let lines = vec![
            line(&[("Item", 58.0, 12.84), ("Qty", 98.41, 12.22)], 108.3, 11.0),
            line(&[("Tea", 58.0, 12.84), ("4", 98.41, 0.0)], 132.2, 11.0),
            line(&[("Coffee", 58.0, 26.29), ("2", 98.41, 0.0)], 156.1, 11.0),
        ];
        let tables = tables_on_page(&lines, 1);
        assert_eq!(tables.len(), 1);
        assert_eq!(tables[0].columns, 2);
        assert_eq!(tables[0].rows.len(), 3);
        assert_eq!(tables[0].rows[2][0], "Coffee");
    }

    #[test]
    fn a_single_row_is_not_a_table() {
        let lines = vec![line(
            &[("Name", 10.0, 40.0), ("Amount", 200.0, 50.0)],
            700.0,
            10.0,
        )];
        assert!(tables_on_page(&lines, 1).is_empty());
    }

    #[test]
    fn prose_between_tables_breaks_the_run() {
        let mut lines = vec![
            line(&[("A", 10.0, 10.0), ("B", 200.0, 10.0)], 700.0, 10.0),
            line(&[("1", 10.0, 10.0), ("2", 200.0, 10.0)], 685.0, 10.0),
        ];
        lines.push(line(
            &[(
                "A paragraph of prose that runs the width of the page.",
                10.0,
                400.0,
            )],
            670.0,
            10.0,
        ));
        lines.push(line(&[("C", 10.0, 10.0), ("D", 200.0, 10.0)], 640.0, 10.0));
        lines.push(line(&[("3", 10.0, 10.0), ("4", 200.0, 10.0)], 625.0, 10.0));
        let tables = tables_on_page(&lines, 2);
        assert_eq!(tables.len(), 2);
        assert!(
            tables.iter().all(|table| table.page == 2),
            "each table carries its own page"
        );
    }

    #[test]
    fn a_misaligned_cell_lowers_the_confidence_instead_of_moving_a_column() {
        let lines = vec![
            line(
                &[("Name", 10.0, 40.0), ("Amount", 200.0, 50.0)],
                700.0,
                10.0,
            ),
            // The second cell starts 40 pt right of the column: it belongs to
            // no column, so the row's cell stays empty and the confidence says
            // one cell of four did not land.
            line(&[("Ada", 10.0, 30.0), ("1200", 240.0, 30.0)], 685.0, 10.0),
        ];
        let tables = tables_on_page(&lines, 1);
        assert_eq!(tables.len(), 1);
        assert_eq!(tables[0].rows[1][1], "");
        assert!(tables[0].confidence < 0.9, "{}", tables[0].confidence);
    }

    #[test]
    fn csv_quotes_a_cell_with_a_comma_and_names_the_page() {
        let table = Table {
            page: 7,
            columns: 2,
            rows: vec![
                vec!["City".to_string(), "Note".to_string()],
                vec!["Paris".to_string(), "a, b".to_string()],
            ],
            confidence: 1.0,
            bbox: Rect::new(0.0, 0.0, 100.0, 100.0),
        };
        let csv = to_csv(&[table]);
        assert!(csv.starts_with("# page 7, table 1 (2 columns, confidence 1.00)"));
        assert!(csv.contains("\"a, b\""));
    }
}
