//! The layout engine: block tree → paginated lines of text and rectangles.

use selis_error::Result;
use selis_sandbox::{Budget, BudgetGuard, Resource};

use crate::blocks::{
    encode_text, measure, split_words, BackgroundSize, Block, Span, Style, TextAlign,
};
use crate::image::ImageSource;
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
    /// Distance from the page top to the line's top, in points. Carried for
    /// the styled-output path (SL-2.CONVERT); the plain-text renderer orders
    /// lines by construction (they are pushed top-down).
    #[allow(dead_code)]
    pub top: f64,
    /// The line height in points. Carried for the styled-output path
    /// (SL-2.CONVERT); see [`Line::top`].
    #[allow(dead_code)]
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
        /// The fill colour in RGB [0,1], emitted as an `rg` operator.
        color: [f64; 3],
    },
    /// A filled rectangle; `top` is the distance from the page top.
    Rect {
        x: f64,
        /// Distance from the page top; carried for the styled-output path
        /// (SL-2.CONVERT) — the plain-text renderer orders by `y` only.
        #[allow(dead_code)]
        top: f64,
        /// Carried with `top` for the styled-output path (SL-2.CONVERT).
        #[allow(dead_code)]
        height: f64,
        w: f64,
        h: f64,
        color: [f64; 3],
    },
    /// A clickable link region over a run of text. The emitter turns this into
    /// a real `/Subtype /Link` annotation with a `/S /URI` action.
    LinkBox {
        x: f64,
        /// Distance from the page top to the box's top edge.
        top: f64,
        w: f64,
        h: f64,
        uri: String,
    },
    /// A JPEG/PNG (`Image`) or SVG (`Form`) drawn as an `XObject` at a measured
    /// rectangle. The emitter writes the stream and assigns the `/ImN` name.
    Image {
        x: f64,
        /// Distance from the page top to the box's top edge.
        top: f64,
        w: f64,
        h: f64,
        image: ImageSource,
    },
}

/// The effective point size of a run: an inline `font-size` overrides the
/// block's base size, everything else falls back to it.
fn eff_size(run: &Span, base: f64) -> f64 {
    match run.size {
        Some(size) if size.is_finite() && size > 0.0 => size,
        _ => base,
    }
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

    /// Append a line of already-wrapped runs at the cursor. `left_x` and
    /// `right_x` bound the text column; `align` positions the line within it.
    fn push_text_line(
        &mut self,
        runs: &[Span],
        size_of: &dyn Fn(Style) -> f64,
        left_x: f64,
        right_x: f64,
        factor: f64,
        align: TextAlign,
    ) {
        let sized: Vec<(&Span, f64)> = runs
            .iter()
            .map(|run| (run, eff_size(run, size_of(run.style))))
            .collect();
        let size_max = sized
            .iter()
            .fold(BODY_SIZE, |max, (_, size)| max.max(*size));
        let height = size_max * factor;
        self.ensure_room(height);
        let top = self.cursor;
        let width: f64 = sized
            .iter()
            .map(|(run, size)| measure(run.style.font(), &run.text, *size))
            .sum();
        let shift = match align {
            TextAlign::Left => 0.0,
            TextAlign::Center => (right_x - left_x - width) * 0.5,
            TextAlign::Right => right_x - left_x - width,
        }
        .max(0.0);
        let mut x = left_x + shift;
        let mut items: Vec<Item> = Vec::new();
        for (run, size) in sized {
            if run.text.is_empty() {
                continue;
            }
            let font = run.style.font();
            let w = measure(font, &run.text, size);
            // An inline `background-image` data URI paints the run's own text
            // box. The emitter draws images before text, so the image sits
            // behind the glyphs rather than over them. `background-size`, when
            // present, resizes that image inside the box (see `background_box`);
            // without it the legacy stretch-to-box behaviour is kept.
            if let Some(background) = &run.background {
                let (bw, bh) = match run.background_size {
                    Some(mode) => background_box(
                        (
                            f64::from(background.width) * IMAGE_PX_TO_PT,
                            f64::from(background.height) * IMAGE_PX_TO_PT,
                        ),
                        (w, height),
                        mode,
                        (self.max_x() - x, self.page_h - 2.0 * MARGIN),
                    ),
                    None => (w, height),
                };
                items.push(Item::Image {
                    x,
                    top,
                    w: bw,
                    h: bh,
                    image: ImageSource::Raster(background.clone()),
                });
            }
            items.push(Item::Text {
                x,
                font,
                size,
                bytes: encode_text(&run.text),
                color: run.color.unwrap_or(BLACK),
            });
            if let Some(uri) = &run.href {
                items.push(Item::LinkBox {
                    x,
                    top,
                    w,
                    h: height,
                    uri: uri.clone(),
                });
            }
            x += w;
        }
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
/// Charges the laid-out text bytes and embedded image bytes against `budget`.
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
                match item {
                    Item::Text { bytes: b, .. } => {
                        bytes = bytes.saturating_add(u64::try_from(b.len()).unwrap_or(u64::MAX));
                    }
                    Item::Image { image, .. } => {
                        let len = match image {
                            ImageSource::Raster(img) => img.data.len(),
                            ImageSource::Svg(svg) => svg.content.len(),
                        };
                        bytes = bytes.saturating_add(u64::try_from(len).unwrap_or(u64::MAX));
                    }
                    Item::Rect { .. } | Item::LinkBox { .. } => {}
                }
            }
        }
    }
    let _ = budget;
    g.charge(Resource::Bytes, bytes)?;
    Ok(pages)
}

/// Internal horizontal gap around table cells.
const TABLE_CELL_PAD: f64 = 4.0;
/// The floor of a column's width, in points.
const TABLE_MIN_COL: f64 = 28.0;
/// The line thickness of table rules.
const TABLE_RULE_TOP: f64 = 0.9;
/// The line thickness of table rules inside the body.
const TABLE_RULE_MID: f64 = 0.5;
/// The header row's background grey.
const HEADER_BG: [f64; 3] = [0.93, 0.93, 0.93];
/// The rule grey, a shade darker than the code-block background.
const TABLE_RULE: [f64; 3] = [0.72, 0.72, 0.72];

/// Lay a table out as, per row, wrapped cell lines on a shared baseline.
///
/// The style is booktabs: a heavy rule above the header, a lighter one below
/// it, hairlines between body rows and a heavy rule at the bottom. There are
/// no vertical rules; column separation is the padding. A row that does not
/// fit moves to the next page whole.
fn render_table(layout: &mut Layout, head: &[Vec<Span>], rows: &[Vec<Vec<Span>>], indent: f64) {
    let table_left = MARGIN + indent;
    let avail = layout.max_x() - table_left;
    let cols = head
        .len()
        .max(rows.iter().map(|row| row.len()).max().unwrap_or(0));
    if cols == 0 || avail <= 0.0 {
        layout.cursor += PARA_GAP;
        return;
    }

    // Every row padded to the column count so the paths below cannot index
    // past the end of a short row.
    let pad_cell = |cell: &Vec<Span>| -> Vec<Span> { cell.clone() };
    let normalize_row = |row: &[Vec<Span>]| -> Vec<Vec<Span>> {
        let mut cells: Vec<Vec<Span>> = row.iter().map(&pad_cell).collect();
        cells.resize(cols, Vec::new());
        cells
    };
    let head_n = if head.is_empty() {
        Vec::new()
    } else {
        normalize_row(head)
    };
    let rows_n: Vec<Vec<Vec<Span>>> = rows.iter().map(|row| normalize_row(row)).collect();

    // Natural widths: the widest cell in each column, capped at the page.
    let mut widths = vec![TABLE_MIN_COL; cols];
    for (index, width) in widths.iter_mut().enumerate() {
        let mut widest = 0.0f64;
        let mut consider = |cell: &[Span], bold: bool| {
            // Code cells are wider than Helvetica at the same size, so the
            // natural width must be measured per span, not from plain text.
            let cell_width: f64 = cell
                .iter()
                .map(|span| {
                    let font = if bold {
                        "Helvetica-Bold"
                    } else {
                        span.style.font()
                    };
                    measure(font, &span.text, BODY_SIZE)
                })
                .sum();
            widest = widest.max(cell_width);
        };
        if let Some(cell) = head_n.get(index) {
            consider(cell, true);
        }
        for row in &rows_n {
            if let Some(cell) = row.get(index) {
                consider(cell, false);
            }
        }
        *width = (widest + 2.0 * TABLE_CELL_PAD).clamp(TABLE_MIN_COL, avail);
    }
    let natural: f64 = widths.iter().sum();
    if natural > avail {
        let floor = (avail / f64::from(u32::try_from(cols).unwrap_or(1))).max(8.0);
        for width in &mut widths {
            *width = (*width * avail / natural).max(floor);
        }
        let capped: f64 = widths.iter().sum();
        if capped > avail {
            let scale = avail / capped;
            for width in &mut widths {
                *width *= scale;
            }
        }
    }
    // Cumulative left edge of each column, relative to the table.
    let mut offsets = vec![table_left; cols];
    let mut x = table_left;
    for (index, width) in widths.iter().enumerate() {
        if let Some(slot) = offsets.get_mut(index) {
            *slot = x;
        }
        x += width;
    }

    // Lay one row: returns its height, having appended its lines at the cursor.
    let lay_row = |layout: &mut Layout, cells: &[Vec<Span>], bold: bool| -> f64 {
        let size_of = |_s: Style| BODY_SIZE;
        let mapped = |s: Style| if bold { s.with_bold() } else { s };
        let mut wrapped: Vec<Vec<Vec<Span>>> = Vec::with_capacity(cells.len());
        for (index, cell) in cells.iter().enumerate() {
            let width = widths.get(index).copied().unwrap_or(TABLE_MIN_COL);
            let left = offsets.get(index).copied().unwrap_or(table_left) + TABLE_CELL_PAD;
            let right = left + width - 2.0 * TABLE_CELL_PAD;
            wrapped.push(wrap_spans(cell, &mapped, &size_of, left, left, right));
        }
        let line_count = wrapped.iter().map(Vec::len).max().unwrap_or(1).max(1);
        let height = f64::from(u32::try_from(line_count).unwrap_or(1)) * BODY_SIZE * LINE_FACTOR
            + 2.0 * TABLE_CELL_PAD;
        layout.ensure_room(height);
        let top = layout.cursor;
        if bold {
            layout.current.push(Line {
                top,
                height,
                baseline: top + BODY_SIZE + TABLE_CELL_PAD,
                items: vec![Item::Rect {
                    x: table_left,
                    top,
                    height,
                    w: avail,
                    h: height,
                    color: HEADER_BG,
                }],
            });
        }
        for line_index in 0..line_count {
            let mut items: Vec<Item> = Vec::new();
            for (index, cell_lines) in wrapped.iter().enumerate() {
                if let Some(line_runs) = cell_lines.get(line_index) {
                    let cell_left =
                        offsets.get(index).copied().unwrap_or(table_left) + TABLE_CELL_PAD;
                    let cell_width = widths.get(index).copied().unwrap_or(TABLE_MIN_COL);
                    let available = cell_width - 2.0 * TABLE_CELL_PAD;
                    let align = cells
                        .get(index)
                        .and_then(|cell| cell.first())
                        .and_then(|span| span.align)
                        .unwrap_or(TextAlign::Left);
                    let line_width: f64 = line_runs
                        .iter()
                        .map(|run| {
                            measure(
                                run.style.font(),
                                &run.text,
                                eff_size(run, size_of(run.style)),
                            )
                        })
                        .sum();
                    let shift = match align {
                        TextAlign::Left => 0.0,
                        TextAlign::Center => (available - line_width) * 0.5,
                        TextAlign::Right => available - line_width,
                    }
                    .max(0.0);
                    let mut cell_x = cell_left + shift;
                    for run in line_runs.iter() {
                        if run.text.is_empty() {
                            continue;
                        }
                        let font = run.style.font();
                        let size = eff_size(run, size_of(run.style));
                        let w = measure(font, &run.text, size);
                        items.push(Item::Text {
                            x: cell_x,
                            font,
                            size,
                            bytes: encode_text(&run.text),
                            color: run.color.unwrap_or(BLACK),
                        });
                        if let Some(uri) = &run.href {
                            items.push(Item::LinkBox {
                                x: cell_x,
                                top: top
                                    + TABLE_CELL_PAD
                                    + f64::from(u32::try_from(line_index).unwrap_or(0))
                                        * BODY_SIZE
                                        * LINE_FACTOR,
                                w,
                                h: BODY_SIZE * LINE_FACTOR,
                                uri: uri.clone(),
                            });
                        }
                        cell_x += w;
                    }
                }
            }
            let baseline = top
                + TABLE_CELL_PAD
                + BODY_SIZE
                + f64::from(u32::try_from(line_index).unwrap_or(0)) * BODY_SIZE * LINE_FACTOR;
            layout.current.push(Line {
                top: baseline - BODY_SIZE,
                height: BODY_SIZE * LINE_FACTOR,
                baseline,
                items,
            });
        }
        layout.cursor += height;
        height
    };

    let rule = |layout: &mut Layout, thickness: f64| {
        layout.ensure_room(thickness);
        let top = layout.cursor;
        layout.current.push(Line {
            top,
            height: thickness,
            baseline: top + thickness,
            items: vec![Item::Rect {
                x: table_left,
                top,
                height: thickness,
                w: avail,
                h: thickness,
                color: TABLE_RULE,
            }],
        });
        layout.cursor += thickness;
    };

    rule(layout, TABLE_RULE_TOP);
    if !head_n.is_empty() {
        lay_row(layout, &head_n, true);
        rule(layout, TABLE_RULE_MID);
    }
    for row in rows_n.iter() {
        lay_row(layout, row, false);
        rule(layout, TABLE_RULE_MID);
    }
    layout.cursor += PARA_GAP;
}

/// Pixels-to-points for an image with no explicit size, matching the crate's
/// 96-dpi convention used for `font-size: …px`.
const IMAGE_PX_TO_PT: f64 = 0.75;

/// Compute an image source's placement `(width, height)` in points.
///
/// Intrinsic image pixels are converted at 96 dpi; an SVG's canvas is already
/// in points. Explicit `width`/`height` override them; when only one is given
/// the other follows the intrinsic aspect ratio. The box is clamped to the
/// available column (and the page height), preserving aspect ratio. Non-finite
/// inputs fall back to intrinsic.
fn image_box(
    intrinsic: (f64, f64),
    width: Option<f64>,
    height: Option<f64>,
    avail_w: f64,
    avail_h: f64,
) -> (f64, f64) {
    let (intrinsic_w, intrinsic_h) = intrinsic;
    let explicit_w = width.filter(|v| v.is_finite() && *v > 0.0);
    let explicit_h = height.filter(|v| v.is_finite() && *v > 0.0);
    let mut w = explicit_w.unwrap_or(intrinsic_w);
    let mut h = explicit_h.unwrap_or(intrinsic_h);
    if explicit_w.is_some() && explicit_h.is_none() && intrinsic_w > 0.0 {
        h = w * intrinsic_h / intrinsic_w;
    } else if explicit_w.is_none() && explicit_h.is_some() && intrinsic_h > 0.0 {
        w = h * intrinsic_w / intrinsic_h;
    }
    if w > avail_w && w > 0.0 {
        let scale = avail_w / w;
        w *= scale;
        h *= scale;
    }
    if h > avail_h && h > 0.0 {
        let scale = avail_h / h;
        w *= scale;
        h *= scale;
    }
    (w.max(0.0), h.max(0.0))
}

/// Resolve a `background-size` declaration against an element box.
///
/// `auto` uses the image's intrinsic size; `contain`/`cover` scale it to fit or
/// fill the box while preserving aspect; absolute lengths use the declared
/// values (a single length derives the other axis from the intrinsic ratio).
/// The result is clamped to the page's available width/height, and to the
/// element box for every mode except `cover` (which deliberately overflows
/// when necessary). There is no `background-position` or clipping: the image is
/// always painted from the box's top-left corner.
fn background_box(
    intrinsic: (f64, f64),
    element: (f64, f64),
    mode: BackgroundSize,
    avail: (f64, f64),
) -> (f64, f64) {
    let (iw, ih) = intrinsic;
    let (ew, eh) = element;
    let (avail_w, avail_h) = avail;
    if !(iw > 0.0 && ih > 0.0 && ew > 0.0 && eh > 0.0) {
        return (ew.max(0.0), eh.max(0.0));
    }
    let mut w = iw;
    let mut h = ih;
    match mode {
        BackgroundSize::Auto => {}
        BackgroundSize::Contain => {
            let scale = (ew / iw).min(eh / ih);
            w = iw * scale;
            h = ih * scale;
        }
        BackgroundSize::Cover => {
            let scale = (ew / iw).max(eh / ih);
            w = iw * scale;
            h = ih * scale;
        }
        BackgroundSize::Length { width, height } => {
            let declared_w = width.filter(|v| v.is_finite() && *v > 0.0);
            let declared_h = height.filter(|v| v.is_finite() && *v > 0.0);
            match (declared_w, declared_h) {
                (Some(pw), Some(ph)) => {
                    w = pw;
                    h = ph;
                }
                (Some(pw), None) => {
                    w = pw;
                    h = pw * ih / iw;
                }
                (None, Some(ph)) => {
                    h = ph;
                    w = ph * iw / ih;
                }
                (None, None) => {}
            }
        }
    }
    // Clamp to the page column first, then (except for `cover`) to the box.
    if w > avail_w && w > 0.0 {
        let scale = avail_w / w;
        w *= scale;
        h *= scale;
    }
    if h > avail_h && h > 0.0 {
        let scale = avail_h / h;
        w *= scale;
        h *= scale;
    }
    if mode != BackgroundSize::Cover {
        if w > ew && w > 0.0 {
            let scale = ew / w;
            w *= scale;
            h *= scale;
        }
        if h > eh && h > 0.0 {
            let scale = eh / h;
            w *= scale;
            h *= scale;
        }
    }
    (w.max(0.0), h.max(0.0))
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
                    if italicize {
                        s.with_bold().with_italic()
                    } else {
                        s.with_bold()
                    }
                },
                &size_of,
                MARGIN + indent,
                MARGIN + indent,
                layout.max_x(),
            );
            let align = block_align(spans);
            for line_runs in &runs {
                layout.push_text_line(
                    line_runs,
                    &size_of,
                    MARGIN + indent,
                    layout.max_x(),
                    1.3,
                    align,
                );
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
            let align = block_align(spans);
            for line_runs in &runs {
                layout.push_text_line(
                    line_runs,
                    &size_of,
                    MARGIN + indent,
                    layout.max_x(),
                    LINE_FACTOR,
                    align,
                );
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
                let align = block_align(spans);
                let line_width: f64 = line_runs
                    .iter()
                    .map(|run| {
                        measure(
                            run.style.font(),
                            &run.text,
                            eff_size(run, size_of(run.style)),
                        )
                    })
                    .sum();
                let shift = match align {
                    TextAlign::Left => 0.0,
                    TextAlign::Center => (layout.max_x() - text_x - line_width) * 0.5,
                    TextAlign::Right => layout.max_x() - text_x - line_width,
                }
                .max(0.0);
                let mut x = text_x + shift;
                for run in line_runs.iter() {
                    if run.text.is_empty() {
                        continue;
                    }
                    let font = run.style.font();
                    let size = eff_size(run, size_of(run.style));
                    let w = measure(font, &run.text, size);
                    items.push(Item::Text {
                        x,
                        font,
                        size,
                        bytes: encode_text(&run.text),
                        color: run.color.unwrap_or(BLACK),
                    });
                    if let Some(uri) = &run.href {
                        items.push(Item::LinkBox {
                            x,
                            top,
                            w,
                            h: height,
                            uri: uri.clone(),
                        });
                    }
                    x += w;
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
        Block::Code(lines) => {
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
                        height: CODE_LINE,
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
        Block::Quote(inner) => {
            layout.ensure_room(BODY_SIZE);
            let mut inner_first = true;
            for child in inner {
                render_block(layout, child, indent + QUOTE_INDENT, true, &mut inner_first);
            }
            layout.cursor += 2.0;
        }
        Block::Table { head, rows } => {
            render_table(layout, head, rows, indent);
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
                    height: 0.8,
                    w: layout.page_w - 2.0 * MARGIN,
                    h: 0.8,
                    color: RULE_GREY,
                }],
            });
            layout.cursor += 12.0;
        }
        Block::Image(img) => {
            let avail_w = layout.max_x() - (MARGIN + indent);
            let avail_h = layout.page_h - 2.0 * MARGIN;
            let (w, h) = image_box(
                img.image.intrinsic_pt(),
                img.width,
                img.height,
                avail_w,
                avail_h,
            );
            if w > 0.0 && h > 0.0 {
                // Break before the image if it would not fit on this page.
                layout.ensure_room(h);
                let top = layout.cursor;
                layout.current.push(Line {
                    top,
                    height: h,
                    baseline: top + h,
                    items: vec![Item::Image {
                        x: MARGIN + indent,
                        top,
                        w,
                        h,
                        image: img.image.clone(),
                    }],
                });
                layout.cursor += h + PARA_GAP;
            }
        }
        Block::PageBreak => {
            if !layout.current.is_empty() {
                let lines = std::mem::take(&mut layout.current);
                layout.pages.push(Page { lines });
                layout.cursor = MARGIN;
            }
        }
    }
    *first_block = false;
}

/// The alignment of a block, carried on its first span (see
/// [`Span::align`](crate::blocks::Span::align)).
fn block_align(spans: &[Span]) -> TextAlign {
    spans
        .first()
        .and_then(|span| span.align)
        .unwrap_or(TextAlign::Left)
}

/// Wrap styled spans into lines of runs. Each run keeps its inline properties
/// (`href`, `color`, `size`, `align`) so layout can position it and the emitter
/// can colour it / hang a link annotation off it.
///
/// `style_map` applies context styles (heading bold, blockquote italic);
/// `size_of` returns each mapped style's base point size.
fn wrap_spans(
    spans: &[Span],
    style_map: &dyn Fn(Style) -> Style,
    size_of: &dyn Fn(Style) -> f64,
    first_x: f64,
    cont_x: f64,
    max_x: f64,
) -> Vec<Vec<Span>> {
    let mut lines: Vec<Vec<Span>> = Vec::new();
    let mut cur: Vec<Span> = Vec::new();
    let mut cur_x = first_x;
    let mut prev_trailing = false;

    let push_line = |lines: &mut Vec<Vec<Span>>, cur: &mut Vec<Span>| {
        if let Some(last) = cur.last_mut() {
            while last.text.ends_with(' ') {
                last.text.pop();
            }
        }
        if cur.last().is_some_and(|run| run.text.is_empty()) {
            cur.pop();
        }
        if !cur.is_empty() {
            lines.push(std::mem::take(cur));
        }
    };

    for span in spans {
        let mapped = style_map(span.style);
        let font = mapped.font();
        let size = eff_size(span, size_of(mapped));
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
                        push_run(&mut cur, mapped, std::mem::take(&mut chunk), span);
                        push_line(&mut lines, &mut cur);
                        cur_x = cont_x;
                        chunk_w = 0.0;
                    }
                    chunk.push(c);
                    chunk_w += cw;
                }
                push_run(&mut cur, mapped, chunk, span);
                cur_x += chunk_w;
            } else {
                if !cur.is_empty() && prev_trailing {
                    if let Some(last) = cur.last_mut() {
                        last.text.push(' ');
                    }
                    cur_x += space_w;
                }
                push_run(&mut cur, mapped, word, span);
                cur_x += word_w;
            }
            prev_trailing = trailing;
        }
    }
    push_line(&mut lines, &mut cur);
    lines
}

/// Append text to the last run when its style *and* inline properties match,
/// else start a new run so colour / size / link boundaries are preserved.
fn push_run(cur: &mut Vec<Span>, style: Style, text: String, source: &Span) {
    if text.is_empty() {
        return;
    }
    if let Some(last) = cur.last_mut() {
        if last.style == style
            && last.href == source.href
            && last.color == source.color
            && last.size == source.size
            && last.align == source.align
            && last.background == source.background
            && last.background_size == source.background_size
        {
            last.text.push_str(&text);
            return;
        }
    }
    cur.push(Span {
        style,
        text,
        href: source.href.clone(),
        color: source.color,
        size: source.size,
        align: source.align,
        background: source.background.clone(),
        background_size: source.background_size,
    });
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
            ..Default::default()
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
        let blocks = vec![Block::Code(vec!["y".repeat(300)])];
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
                    ..Default::default()
                }],
            },
            Block::ListItem {
                ordered: true,
                ordinal: 1,
                depth: 0,
                spans: vec![Span {
                    style: Style::Regular,
                    text: "numbered".to_string(),
                    ..Default::default()
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
