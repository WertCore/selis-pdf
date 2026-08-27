//! The layout engine: block tree → paginated lines of text and rectangles.

use selis_error::Result;
use selis_sandbox::{Budget, BudgetGuard, Resource};

use crate::blocks::{encode_text, measure, split_words, Block, Span, Style};
use crate::PageSize;

/// The page margin in points.
const MARGIN: f64 = 54.0;
/// The body font size in points.
const BODY_SIZE: f64 = 11.0;
/// Line height as a multiple of the font size.
const LINE_FACTOR: f64 = 1.4;
/// The paragraph gap after a block, in points.
const PARA_GAP: f64 = 8.0;
/// The fixed line height of code lines, in points.
const CODE_LINE: f64 = 12.5;
/// The code font size in points.
const CODE_SIZE: f64 = 9.5;
/// The list marker column width in points.
const MARKER_COL: f64 = 14.0;
/// The per-depth list indent in points.
const DEPTH_INDENT: f64 = 16.0;
/// The blockquote indent in points.
const QUOTE_INDENT: f64 = 18.0;

/// Heading font sizes by level (index 0 = level 1).
const HEADING_SIZES: [f64; 6] = [24.0, 18.0, 15.0, 13.0, 12.0, 11.5];

/// Black text.
const BLACK: [f64; 3] = [0.0, 0.0, 0.0];
/// Code-block background.
const CODE_BG: [f64; 3] = [0.93, 0.93, 0.93];
/// Horizontal-rule grey.
const RULE_GREY: [f64; 3] = [0.6, 0.6, 0.6];

/// A laid-out page.
#[derive(Debug)]
pub(crate) struct Page {
    /// The lines, top-down.
    pub lines: Vec<Line>,
}

/// A laid-out line.
#[derive(Debug)]
pub(crate) struct Line {
    /// Distance from the page top to the line's top, in points.
    pub top: f64,
    /// The line height in points.
    pub height: f64,
    /// Distance from the page top to the text baseline, in points.
    pub baseline: f64,
    /// The line's items (rectangles render under text).
    pub items: Vec<Item>,
}

/// A positioned piece of a line.
#[derive(Debug)]
pub(crate) enum Item {
    /// An encoded text run in a standard-14 font.
    Text {
        x: f64,
        font: &'static str,
        size: f64,
        bytes: Vec<u8>,
        color: [f64; 3],
    },
    /// A filled rectangle; `top` is the distance from the page top.
    Rect {
        x: f64,
        top: f64,
        w: f64,
        h: f64,
        color: [f64; 3],
    },
}

/// The pagination state.
#[derive(Debug)]
struct Layout {
    page_w: f64,
    page_h: f64,
    pages: Vec<Page>,
    current: Vec<Line>,
    cursor: f64,
}

impl Layout {
    fn new(page_size: PageSize) -> Self {
        let (page_w, page_h) = page_size.dims();
        Self {
            page_w,
            page_h,
            pages: Vec::new(),
            current: Vec::new(),
            cursor: MARGIN,
        }
    }

    /// Break the page when `h` does not fit below the cursor.
    fn ensure_room(&mut self, h: f64) {
        let bottom = self.page_h - MARGIN;
        if self.cursor + h > bottom && !self.current.is_empty() {
            let lines = std::mem::take(&mut self.current);
            self.pages.push(Page { lines });
            self.cursor = MARGIN;
        }
    }

    fn max_x(&self) -> f64 {
        self.page_w - MARGIN
    }

    /// Append a line of text runs (already wrapped) at the cursor.
    fn push_text_line(
        &mut self,
        runs: &[(Style, String)],
        size_of: &dyn Fn(Style) -> f64,
        first_x: f64,
        factor: f64,
    ) {
        let mut size_max = BODY_SIZE;
        for (style, _) in runs.iter() {
            size_max = size_max.max(size_of(*style));
        }
        let height = size_max * factor;
        self.ensure_room(height);
        let mut items: Vec<Item> = Vec::new();
        let mut x = first_x;
        for (style, text) in runs.iter() {
            if text.is_empty() {
                continue;
            }
            let font = style.font();
            let size = size_of(*style);
            items.push(Item::Text {
                x,
                font,
                size,
                bytes: encode_text(text),
                color: BLACK,
            });
            x += measure(font, text, size);
        }
        let top = self.cursor;
        self.current.push(Line {
            top,
            height,
            baseline: top + size_max,
            items,
        });
        self.cursor += height;
    }

    fn finish(mut self) -> Vec<Page> {
        if self.current.is_empty() && self.pages.is_empty() {
            self.pages.push(Page { lines: Vec::new() });
        } else if !self.current.is_empty() {
            let lines = std::mem::take(&mut self.current);
            self.pages.push(Page { lines });
        }
        self.pages
    }
}

/// Lay a block sequence out into pages.
///
/// # Budget
///
/// Charges the laid-out text bytes against `budget`.
///
/// # Malformed Input
///
/// Never fails on content; `BUDGET_*` on exhaustion.
pub(crate) fn lay_out(
    blocks: &[Block],
    page_size: PageSize,
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
) -> Result<Vec<Page>> {
    let mut layout = Layout::new(page_size);
    let mut first_block = true;
    for block in blocks {
        render_block(&mut layout, block, 0.0, false, &mut first_block);
    }
    let pages = layout.finish();
    // Charge the laid-out text volume once.
    let mut bytes = 0u64;
    for page in &pages {
        for line in &page.lines {
            for item in &line.items {
                if let Item::Text { bytes: b, .. } = item {
                    bytes = bytes.saturating_add(u64::try_from(b.len()).unwrap_or(u64::MAX));
                }
            }
        }
    }
    let _ = budget;
    g.charge(Resource::Bytes, bytes)?;
    Ok(pages)
}

/// Render one block at the given indent.
fn render_block(
    layout: &mut Layout,
    block: &Block,
    indent: f64,
    italicize: bool,
    first_block: &mut bool,
) {
    match block {
        Block::Heading { level, spans } => {
            let idx = usize::from(level.saturating_sub(1).min(5));
            let size = HEADING_SIZES.get(idx).copied().unwrap_or(BODY_SIZE);
            if !*first_block {
                layout.ensure_room(size);
                layout.cursor += size * 0.6;
            }
            let size_of = move |_s: Style| size;
            let runs = wrap_spans(
                spans,
                &|s: Style| {
                    let s = if italicize {
                        s.with_bold().with_italic()
                    } else {
                        s.with_bold()
                    };
                    s
                },
                &size_of,
                MARGIN + indent,
                MARGIN + indent,
                layout.max_x(),
            );
            for line_runs in &runs {
                layout.push_text_line(line_runs, &size_of, MARGIN + indent, 1.3);
            }
            layout.cursor += size * 0.3;
        }
        Block::Paragraph(spans) => {
            if spans.iter().all(|s| s.text.trim().is_empty()) {
                return;
            }
            let size_of = |s: Style| {
                if matches!(s, Style::Code) {
                    BODY_SIZE * 0.9
                } else {
                    BODY_SIZE
                }
            };
            let runs = wrap_spans(
                spans,
                &|s: Style| if italicize { s.with_italic() } else { s },
                &size_of,
                MARGIN + indent,
                MARGIN + indent,
                layout.max_x(),
            );
            for line_runs in &runs {
                layout.push_text_line(line_runs, &size_of, MARGIN + indent, LINE_FACTOR);
            }
            layout.cursor += PARA_GAP;
        }
        Block::ListItem {
            ordered,
            ordinal,
            depth,
            spans,
        } => {
            let depth_indent =
                indent + DEPTH_INDENT * f64::from(u32::try_from(*depth).unwrap_or(3).min(3));
            let marker = if *ordered {
                format!("{ordinal}.")
            } else {
                "\u{2022}".to_string()
            };
            let text_x = MARGIN + depth_indent + MARKER_COL;
            let size_of = |s: Style| {
                if matches!(s, Style::Code) {
                    BODY_SIZE * 0.9
                } else {
                    BODY_SIZE
                }
            };
            let runs = wrap_spans(
                spans,
                &|s: Style| if italicize { s.with_italic() } else { s },
                &size_of,
                text_x,
                text_x,
                layout.max_x(),
            );
            for (idx, line_runs) in runs.iter().enumerate() {
                let size_max = BODY_SIZE;
                let height = size_max * LINE_FACTOR;
                layout.ensure_room(height);
                let top = layout.cursor;
                let mut items: Vec<Item> = Vec::new();
                if idx == 0 {
                    items.push(Item::Text {
                        x: MARGIN + depth_indent,
                        font: Style::Regular.font(),
                        size: BODY_SIZE,
                        bytes: encode_text(&marker),
                        color: BLACK,
                    });
                }
                let mut x = text_x;
                for (style, text) in line_runs.iter() {
                    if text.is_empty() {
                        continue;
                    }
                    let font = style.font();
                    let size = size_of(*style);
                    items.push(Item::Text {
                        x,
                        font,
                        size,
                        bytes: encode_text(text),
                        color: BLACK,
                    });
                    x += measure(font, text, size);
                }
                layout.current.push(Line {
                    top,
                    height,
                    baseline: top + size_max,
                    items,
                });
                layout.cursor += height;
            }
            layout.cursor += 2.0;
        }
        Block::CodeBlock(lines) => {
            layout.ensure_room(CODE_LINE);
            let avail = layout.max_x() - (MARGIN + indent) - 8.0;
            for code_line in lines {
                layout.ensure_room(CODE_LINE);
                let top = layout.cursor;
                let truncated = truncate_to_width(code_line, "Courier", CODE_SIZE, avail);
                let items = vec![
                    Item::Rect {
                        x: MARGIN + indent,
                        top,
                        w: layout.max_x() - (MARGIN + indent),
                        h: CODE_LINE,
                        color: CODE_BG,
                    },
                    Item::Text {
                        x: MARGIN + indent + 4.0,
                        font: "Courier",
                        size: CODE_SIZE,
                        bytes: encode_text(&truncated),
                        color: BLACK,
                    },
                ];
                layout.current.push(Line {
                    top,
                    height: CODE_LINE,
                    baseline: top + CODE_SIZE + 1.5,
                    items,
                });
                layout.cursor += CODE_LINE;
            }
            layout.cursor += PARA_GAP;
        }
        Block::Blockquote(inner) => {
            layout.ensure_room(BODY_SIZE);
            let mut inner_first = true;
            for child in inner {
                render_block(layout, child, indent + QUOTE_INDENT, true, &mut inner_first);
            }
            layout.cursor += 2.0;
        }
        Block::Rule => {
            layout.ensure_room(12.0);
            let top = layout.cursor;
            layout.current.push(Line {
                top,
                height: 12.0,
                baseline: top + 6.0,
                items: vec![Item::Rect {
                    x: MARGIN,
                    top: top + 5.6,
                    w: layout.page_w - 2.0 * MARGIN,
                    h: 0.8,
                    color: RULE_GREY,
                }],
            });
            layout.cursor += 12.0;
        }
    }
    *first_block = false;
}

/// Wrap styled spans into lines of runs.
///
/// `style_map` applies context styles (heading bold, blockquote italic);
/// `size_of` returns each mapped style's point size.
fn wrap_spans(
    spans: &[Span],
    style_map: &dyn Fn(Style) -> Style,
    size_of: &dyn Fn(Style) -> f64,
    first_x: f64,
    cont_x: f64,
    max_x: f64,
) -> Vec<Vec<(Style, String)>> {
    let mut lines: Vec<Vec<(Style, String)>> = Vec::new();
    let mut cur: Vec<(Style, String)> = Vec::new();
    let mut cur_x = first_x;
    let mut prev_trailing = false;

    let mut push_line = |lines: &mut Vec<Vec<(Style, String)>>, cur: &mut Vec<(Style, String)>| {
        if !cur.is_empty() {
            // Drop trailing spaces on the line's last run.
            if let Some(last) = cur.last_mut() {
                while last.1.ends_with(' ') {
                    last.1.pop();
                }
            }
            if cur.last().is_some_and(|(_, t)| t.is_empty()) {
                cur.pop();
            }
            if !cur.is_empty() {
                lines.push(std::mem::take(cur));
            }
        }
    };

    for span in spans {
        let mapped = style_map(span.style);
        let font = mapped.font();
        let size = size_of(mapped);
        for (word, trailing) in split_words(&span.text) {
            let space_w = crate::blocks::char_advance(font, ' ', size);
            let word_w = measure(font, &word, size);
            // Start a new line when the word no longer fits.
            if !cur.is_empty() && prev_trailing && cur_x + space_w + word_w > max_x {
                push_line(&mut lines, &mut cur);
                cur_x = cont_x;
            }
            if cur.is_empty() && cont_x + word_w > max_x && word_w > 0.0 {
                // Hard-split a word wider than the whole line.
                let mut chunk = String::new();
                let mut chunk_w = 0.0f64;
                for c in word.chars() {
                    let cw = crate::blocks::char_advance(font, c, size);
                    if !chunk.is_empty() && cur_x + chunk_w + cw > max_x {
                        push_run(&mut cur, mapped, std::mem::take(&mut chunk));
                        push_line(&mut lines, &mut cur);
                        cur_x = cont_x;
                        chunk_w = 0.0;
                    }
                    chunk.push(c);
                    chunk_w += cw;
                }
                push_run(&mut cur, mapped, chunk);
                cur_x += chunk_w;
            } else {
                if !cur.is_empty() && prev_trailing {
                    if let Some(last) = cur.last_mut() {
                        last.1.push(' ');
                    }
                    cur_x += space_w;
                }
                push_run(&mut cur, mapped, word);
                cur_x += word_w;
            }
            prev_trailing = trailing;
        }
    }
    push_line(&mut lines, &mut cur);
    lines
}

/// Append text to the last run when the style matches, else start a new run.
fn push_run(cur: &mut Vec<(Style, String)>, style: Style, text: String) {
    if text.is_empty() {
        return;
    }
    if let Some(last) = cur.last_mut() {
        if last.0 == style {
            last.1.push_str(&text);
            return;
        }
    }
    cur.push((style, text));
}

/// Truncate a code line to a pixel width.
fn truncate_to_width(line: &str, font: &str, size: f64, max_w: f64) -> String {
    let mut out = String::new();
    let mut w = 0.0f64;
    for c in line.chars() {
        let cw = crate::blocks::char_advance(font, c, size);
        if w + cw > max_w {
            break;
        }
        out.push(c);
        w += cw;
    }
    out
}

#[cfg(test)]
mod tests {
    #![allow(clippy::indexing_slicing, clippy::arithmetic_side_effects)]

    use super::*;
    use selis_sandbox::{Budget, CancelToken, FixedClock};

    fn guard() -> BudgetGuard<'static> {
        Budget::unlimited().guard_with(&FixedClock(0), CancelToken::new())
    }

    fn para(text: &str) -> Block {
        Block::Paragraph(vec![Span {
            style: Style::Regular,
            text: text.to_string(),
        }])
    }

    #[test]
    fn empty_input_yields_one_empty_page() {
        let pages =
            lay_out(&[], PageSize::Letter, &Budget::unlimited(), &mut guard()).expect("layout");
        assert_eq!(pages.len(), 1);
        assert!(pages[0].lines.is_empty());
    }

    #[test]
    fn long_text_wraps_and_paginates() {
        let text = "word ".repeat(1500);
        let blocks = vec![para(text.trim())];
        let pages = lay_out(
            &blocks,
            PageSize::Letter,
            &Budget::unlimited(),
            &mut guard(),
        )
        .expect("layout");
        assert!(pages.len() > 1, "1500 words should span pages");
        // Every line stays inside the margins.
        for page in &pages {
            for line in &page.lines {
                assert!(line.top >= MARGIN - 0.01);
                assert!(line.top + line.height <= 792.0);
            }
        }
    }

    #[test]
    fn words_wider_than_a_line_hard_split() {
        let blocks = vec![para(&"x".repeat(500))];
        let pages = lay_out(
            &blocks,
            PageSize::Letter,
            &Budget::unlimited(),
            &mut guard(),
        )
        .expect("layout");
        assert!(pages[0].lines.len() > 1);
    }

    #[test]
    fn code_blocks_render_rectangles_and_truncate() {
        let blocks = vec![Block::CodeBlock(vec!["y".repeat(300)])];
        let pages = lay_out(
            &blocks,
            PageSize::Letter,
            &Budget::unlimited(),
            &mut guard(),
        )
        .expect("layout");
        let line = &pages[0].lines[0];
        assert!(matches!(line.items[0], Item::Rect { .. }));
        let Item::Text { bytes, .. } = &line.items[1] else {
            panic!("expected text");
        };
        assert!(bytes.len() < 300);
    }

    #[test]
    fn list_items_carry_markers() {
        let blocks = vec![
            Block::ListItem {
                ordered: false,
                ordinal: 1,
                depth: 0,
                spans: vec![Span {
                    style: Style::Regular,
                    text: "first".to_string(),
                }],
            },
            Block::ListItem {
                ordered: true,
                ordinal: 1,
                depth: 0,
                spans: vec![Span {
                    style: Style::Regular,
                    text: "numbered".to_string(),
                }],
            },
        ];
        let pages =
            lay_out(&blocks, PageSize::A4, &Budget::unlimited(), &mut guard()).expect("layout");
        let lines = &pages[0].lines;
        let Item::Text { bytes, .. } = &lines[0].items[0] else {
            panic!("expected marker");
        };
        assert_eq!(bytes, &vec![0x95]); // bullet in WinAnsi
        let Item::Text { bytes, .. } = &lines[1].items[0] else {
            panic!("expected marker");
        };
        assert_eq!(bytes, b"1.");
    }
}
