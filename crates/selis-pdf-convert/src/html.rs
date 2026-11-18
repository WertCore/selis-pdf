//! The HTML subset parser (see the crate docs for the supported subset).

use crate::blocks::{
    BackgroundSize, Block, BlockCss, Border, BoxEdges, ImageBlock, Length, LineHeight, Span, Style,
    TextAlign, TextDecoration,
};
use crate::image::{parse_data_uri, parse_image_source, EmbeddedImage};

/// An open-list context.
#[derive(Debug)]
struct ListCtx {
    ordered: bool,
    counter: usize,
}

/// The narrow set of inline properties this converter understands. Each field
/// is optional so nested elements can override only what they declare; unknown
/// declarations are dropped and never guessed at.
#[derive(Debug, Default, Clone)]
struct InlineProps {
    color: Option<[f64; 3]>,
    size: Option<Length>,
    align: Option<TextAlign>,
    /// `background-image: url(data:image/…)` decoded through the same image
    /// path as `<img src>`; painted behind the runs of this element.
    background: Option<EmbeddedImage>,
    /// `background-size` for that image, when declared.
    background_size: Option<BackgroundSize>,
    /// `font-weight: normal|bold`, mapped onto Helvetica / Helvetica-Bold.
    weight: Option<bool>,
    /// `font-style: normal|italic|oblique`, mapped onto Helvetica-Oblique.
    italic: Option<bool>,
    /// `line-height`, kept unresolved for layout.
    line_height: Option<LineHeight>,
    /// `text-decoration` rules. `None` means undeclared; `Some(default)`
    /// (`text-decoration: none`) explicitly clears an inherited decoration.
    decoration: Option<TextDecoration>,
    /// Block box properties (margin / padding / border / background-color).
    box_css: BlockCss,
}

impl InlineProps {
    /// Read every supported declaration from a CSS declaration list. This is
    /// shared by inline `style=` attributes and the `<style>` rulesheet;
    /// unknown declarations and values are ignored, never guessed.
    ///
    /// # Budget
    ///
    /// One declaration slice list per call, bounded by the attribute/rulesheet
    /// text (see [`split_declarations`]).
    ///
    /// # Malformed Input
    ///
    /// Never fails: a malformed declaration simply contributes no property.
    fn from_css(style: &str) -> Self {
        let mut out = Self::default();
        for declaration in split_declarations(style) {
            let mut parts = declaration.splitn(2, ':');
            let property = parts.next().unwrap_or("").trim().to_ascii_lowercase();
            let value = parts.next().unwrap_or("").trim();
            match property.as_str() {
                "color" => {
                    if let Some(color) = css_color(value) {
                        out.color = Some(color);
                    }
                }
                "font-size" => {
                    if let Some(size) = css_font_size(value) {
                        out.size = Some(size);
                    }
                }
                "text-align" => {
                    if let Some(align) = css_align(value) {
                        out.align = Some(align);
                    }
                }
                "background-image" => {
                    if let Some(image) = css_background_image(value) {
                        out.background = Some(image);
                    }
                }
                "background-size" => {
                    if let Some(size) = css_background_size(value) {
                        out.background_size = Some(size);
                    }
                }
                "font-weight" => {
                    if let Some(bold) = css_font_weight(value) {
                        out.weight = Some(bold);
                    }
                }
                "font-style" => {
                    if let Some(italic) = css_font_style(value) {
                        out.italic = Some(italic);
                    }
                }
                "line-height" => {
                    if let Some(height) = css_line_height(value) {
                        out.line_height = Some(height);
                    }
                }
                "text-decoration" => {
                    if let Some(decoration) = css_text_decoration(value) {
                        out.decoration = Some(decoration);
                    }
                }
                "background-color" => {
                    if let Some(color) = css_color(value) {
                        out.box_css.background_color = Some(color);
                    }
                }
                "margin" => {
                    if let Some(edges) = css_box_edges(value) {
                        out.box_css.margin = Some(edges);
                    }
                }
                "padding" => {
                    if let Some(edges) = css_box_edges(value) {
                        out.box_css.padding = Some(edges);
                    }
                }
                "margin-top" | "margin-right" | "margin-bottom" | "margin-left" => {
                    if let Some(length) = css_length(value) {
                        let mut edges = out.box_css.margin.unwrap_or_default();
                        match property.as_str() {
                            "margin-top" => edges.top = length,
                            "margin-right" => edges.right = length,
                            "margin-bottom" => edges.bottom = length,
                            _ => edges.left = length,
                        }
                        out.box_css.margin = Some(edges);
                    }
                }
                "padding-top" | "padding-right" | "padding-bottom" | "padding-left" => {
                    if let Some(length) = css_length(value) {
                        let mut edges = out.box_css.padding.unwrap_or_default();
                        match property.as_str() {
                            "padding-top" => edges.top = length,
                            "padding-right" => edges.right = length,
                            "padding-bottom" => edges.bottom = length,
                            _ => edges.left = length,
                        }
                        out.box_css.padding = Some(edges);
                    }
                }
                "border" => {
                    if let Some(border) = css_border(value) {
                        out.box_css.border = Some(border);
                    }
                }
                _ => {}
            }
        }
        out
    }

    /// Read declarations from an inline `style=` attribute plus the legacy
    /// `align=` attribute for block alignment. Remote URLs in
    /// `background-image` are refused by name and dropped by the image parser.
    fn from_attrs(style: Option<&str>, align: Option<&str>) -> Self {
        let mut out = Self::from_css(style.unwrap_or(""));
        if out.align.is_none() {
            out.align = align.and_then(css_align);
        }
        out
    }

    /// Cascade two levels: each property declared on `over` wins over `self`.
    /// This is the entire cascade this converter implements — rulesheet value
    /// first, inline `style=` value second; unknown/absent properties fall
    /// through to the rulesheet (and ultimately to layout defaults).
    fn overridden_by(self, over: Self) -> Self {
        Self {
            color: over.color.or(self.color),
            size: over.size.or(self.size),
            align: over.align.or(self.align),
            background: over.background.or(self.background),
            background_size: over.background_size.or(self.background_size),
            weight: over.weight.or(self.weight),
            italic: over.italic.or(self.italic),
            line_height: over.line_height.or(self.line_height),
            // Decoration and block-box groups cascade as a whole: a later
            // declaration of one member replaces the group rather than merging
            // individual sides from two rules.
            decoration: over.decoration.or(self.decoration),
            box_css: BlockCss {
                margin: over.box_css.margin.or(self.box_css.margin),
                padding: over.box_css.padding.or(self.box_css.padding),
                border: over.box_css.border.or(self.box_css.border),
                background_color: over
                    .box_css
                    .background_color
                    .or(self.box_css.background_color),
            },
        }
    }
}

/// A simple CSS selector: an optional element type, an optional ID and zero or
/// more classes, in any order after the type. Descendant/child/universal/pseudo
/// and attribute selectors are rejected by [`Self::parse`].
#[derive(Debug, Clone, PartialEq, Eq)]
struct SimpleSelector {
    tag: Option<String>,
    id: Option<String>,
    classes: Vec<String>,
}

impl SimpleSelector {
    /// Parse one selector token. Valid forms are `tag`, `.c`, `#i`, and simple
    /// compounds such as `tag.c#i` / `.a.b`. Every character must belong to a
    /// type or class/ID identifier, so whitespace, combinators, `*`, `:`, `[`,
    /// `,` and at-rules are all refused.
    ///
    /// # Budget
    ///
    /// A handful of owned lowercase identifiers bounded by the selector text.
    ///
    /// # Malformed Input
    ///
    /// `None` for anything that is not a simple selector; never fails.
    fn parse(input: &str) -> Option<Self> {
        let text = input.trim();
        if text.is_empty() {
            return None;
        }
        let rejects = |c: char| {
            c.is_whitespace()
                || matches!(
                    c,
                    '>' | '+' | '~' | '*' | ':' | '[' | ']' | ',' | '(' | ')' | '='
                )
        };
        if text.chars().any(rejects) {
            return None;
        }
        let chars: Vec<char> = text.chars().collect();
        let mut at = 0usize;
        let mut tag = None;
        if chars.first().is_some_and(|c| c.is_ascii_alphanumeric()) {
            let start = at;
            while chars
                .get(at)
                .is_some_and(|c| c.is_ascii_alphanumeric() || *c == '-')
            {
                at = at.saturating_add(1);
            }
            tag = Some(
                chars
                    .get(start..at)?
                    .iter()
                    .collect::<String>()
                    .to_ascii_lowercase(),
            );
        }
        let mut id = None;
        let mut classes = Vec::new();
        while at < chars.len() {
            let marker = *chars.get(at)?;
            if marker != '.' && marker != '#' {
                return None;
            }
            at = at.saturating_add(1);
            let start = at;
            while chars
                .get(at)
                .is_some_and(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
            {
                at = at.saturating_add(1);
            }
            if at == start {
                return None;
            }
            let ident: String = chars
                .get(start..at)?
                .iter()
                .collect::<String>()
                .to_ascii_lowercase();
            if marker == '#' {
                id = Some(ident);
            } else {
                classes.push(ident);
            }
        }
        if tag.is_none() && id.is_none() && classes.is_empty() {
            return None;
        }
        Some(Self { tag, id, classes })
    }

    /// CSS specificity as `(id, class, type)`. Simple selectors carry at most
    /// one type and one ID but may list several classes.
    ///
    /// # Budget
    ///
    /// Constant work, no allocation; class counts above 255 saturate.
    ///
    /// # Malformed Input
    ///
    /// Infallible; only already-parsed identifiers are inspected.
    fn specificity(&self) -> (u8, u8, u8) {
        (
            u8::from(self.id.is_some()),
            u8::try_from(self.classes.len()).unwrap_or(u8::MAX),
            u8::from(self.tag.is_some()),
        )
    }

    /// Whether an element with these attributes matches this selector.
    ///
    /// # Budget
    ///
    /// Linear in the selector's class count; no allocation.
    ///
    /// # Malformed Input
    ///
    /// Infallible: a missing/empty attribute simply fails that comparison.
    fn matches(&self, tag: &str, id: Option<&str>, classes: &[String]) -> bool {
        self.tag.as_deref().is_none_or(|wanted| wanted == tag)
            && self
                .id
                .as_deref()
                .is_none_or(|wanted| wanted == id.unwrap_or(""))
            && self
                .classes
                .iter()
                .all(|wanted| classes.iter().any(|have| have == wanted))
    }
}

/// One rulesheet rule: its selector, declarations, specificity and source
/// position. Later rules win ties, matching CSS source order.
#[derive(Debug)]
struct Rule {
    selector: SimpleSelector,
    props: InlineProps,
    specificity: (u8, u8, u8),
    order: usize,
}

/// The single `<style>` block's rules, in source order.
#[derive(Debug, Default)]
struct Rulesheet {
    rules: Vec<Rule>,
}

impl Rulesheet {
    /// Cascade every rule matching an element into one property set. Rules are
    /// applied from lowest to highest `(specificity, source order)`, each one
    /// overriding only the properties it actually declares.
    ///
    /// # Budget
    ///
    /// One slice of matching rules and one property set, both bounded by the
    /// rulesheet size (the first `<style>` block only).
    ///
    /// # Malformed Input
    ///
    /// Infallible: no rule matches an element with no attributes when only
    /// class/ID selectors were supplied.
    fn props_for(&self, tag: &str, id: Option<&str>, classes: &[String]) -> InlineProps {
        let mut matching: Vec<&Rule> = self
            .rules
            .iter()
            .filter(|rule| rule.selector.matches(tag, id, classes))
            .collect();
        matching.sort_by_key(|rule| (rule.specificity, rule.order));
        let mut out = InlineProps::default();
        for rule in matching {
            out = out.overridden_by(rule.props.clone());
        }
        out
    }
}

/// A styled open element: its tag name (for matching the close tag), the
/// inline properties it declared and, for `<a>`, its link target.
#[derive(Debug)]
struct StyleFrame {
    tag: String,
    props: InlineProps,
    href: Option<String>,
}

/// An open table: the header cells (empty until a `<th>` row closes) and the
/// body rows, each cell a span list.
#[derive(Debug, Default)]
struct TableBuilder {
    head: Vec<Vec<Span>>,
    rows: Vec<Vec<Vec<Span>>>,
    row: Option<RowBuilder>,
}

/// A row being filled.
#[derive(Debug, Default)]
struct RowBuilder {
    cells: Vec<Vec<Span>>,
    cell: Option<Vec<Span>>,
    saw_th: bool,
}

/// The iterative HTML-subset parser.
#[derive(Debug)]
struct Parser<'a> {
    chars: &'a [char],
    pos: usize,
    blocks: Vec<Block>,
    spans: Vec<Span>,
    text_buf: String,
    bold: usize,
    italic: usize,
    code_depth: usize,
    href_depth: usize,
    heading: Option<u8>,
    lists: Vec<ListCtx>,
    in_item: bool,
    item_ordinal: usize,
    table: Option<TableBuilder>,
    /// Open elements that asked for a page break *after* them (CSS
    /// `page-break-after`/`break-after`), so the break can be emitted when the
    /// matching close tag arrives.
    break_after_stack: Vec<String>,
    /// Inline-style / link frames for currently open styled elements. The
    /// innermost declaration of each property wins (see `current_props`).
    styles: Vec<StyleFrame>,
    /// The single `<style>` rulesheet, cascaded under every element's own
    /// inline `style=` attribute after specificity/source-order resolution.
    rules: Rulesheet,
}

/// Parse an HTML subset into layout blocks.
///
/// Every input parses: broken markup, unknown tags and unknown entities
/// degrade to text.
pub(crate) fn parse(input: &str) -> Vec<Block> {
    let text = input.replace('\r', "");
    let rules = parse_rulesheet(&text);
    let chars: Vec<char> = text.chars().collect();
    let mut p = Parser {
        chars: &chars,
        pos: 0,
        blocks: Vec::new(),
        spans: Vec::new(),
        text_buf: String::new(),
        bold: 0,
        italic: 0,
        code_depth: 0,
        href_depth: 0,
        heading: None,
        lists: Vec::new(),
        in_item: false,
        item_ordinal: 0,
        table: None,
        break_after_stack: Vec::new(),
        styles: Vec::new(),
        rules,
    };
    p.run();
    p.finish();
    p.blocks
}

impl<'a> Parser<'a> {
    fn run(&mut self) {
        while self.pos < self.chars.len() {
            let c = self.chars.get(self.pos).copied().unwrap_or_default();
            match c {
                '<' => self.handle_tag(),
                '&' => self.handle_entity(),
                _ => {
                    self.text_buf.push(c);
                    self.pos = self.pos.saturating_add(1);
                }
            }
        }
    }

    /// End-of-input: close any open list item, table or paragraph.
    fn finish(&mut self) {
        if self.in_item {
            self.close_item();
        }
        self.close_table();
        self.flush_paragraph();
    }

    /// Whether a `<td>`/`<th>` is open: inline content is routed into it.
    fn in_table_cell(&self) -> bool {
        self.table
            .as_ref()
            .and_then(|table| table.row.as_ref())
            .is_some_and(|row| row.cell.is_some())
    }

    /// Close the open cell, if any, moving its spans into the row.
    fn close_cell(&mut self) {
        if !self.in_table_cell() {
            return;
        }
        self.flush_text();
        self.heading = None;
        let cell = std::mem::take(&mut self.spans);
        if let Some(table) = self.table.as_mut() {
            if let Some(row) = table.row.as_mut() {
                row.cells.push(cell);
                row.cell = None;
            }
        }
    }

    /// Close the open row, if any, filing it as the header or a body row.
    fn close_row(&mut self) {
        self.close_cell();
        if let Some(table) = self.table.as_mut() {
            if let Some(row) = table.row.take() {
                if row.saw_th && table.head.is_empty() && table.rows.is_empty() {
                    table.head = row.cells;
                } else {
                    table.rows.push(row.cells);
                }
            }
        }
    }

    /// Close the table, if any, emitting it as a block.
    fn close_table(&mut self) {
        self.close_row();
        if let Some(table) = self.table.take() {
            let cols = table
                .head
                .len()
                .max(table.rows.iter().map(|row| row.len()).max().unwrap_or(0));
            if cols > 0 {
                let normalize = |row: &[Vec<Span>]| -> Vec<Vec<Span>> {
                    let mut cells = row.to_vec();
                    cells.resize(cols, Vec::new());
                    cells
                };
                let head: Vec<Vec<Span>> = if table.head.is_empty() {
                    Vec::new()
                } else {
                    normalize(&table.head)
                };
                let rows: Vec<Vec<Vec<Span>>> =
                    table.rows.iter().map(|row| normalize(row)).collect();
                self.blocks.push(Block::Table { head, rows });
            }
        }
    }

    fn cur_style(&self) -> Style {
        let mut s = Style::Regular;
        if self.bold > 0 {
            s = s.with_bold();
        }
        if self.italic > 0 {
            s = s.with_italic();
        }
        if self.code_depth > 0 {
            s = Style::Code;
        }
        if self.href_depth > 0 && matches!(s, Style::Regular) {
            s = Style::Italic;
        }
        // CSS overrides the tag-derived style property by property. `normal`
        // clears a bold/italic inherited from a parent tag; `Code` is kept.
        let props = self.current_props();
        if let Some(bold) = props.weight {
            s = if bold {
                s.with_bold()
            } else {
                s.without_bold()
            };
        }
        if let Some(italic) = props.italic {
            s = if italic {
                s.with_italic()
            } else {
                s.without_italic()
            };
        }
        s
    }

    /// The effective inline properties: innermost declaration of each wins.
    fn current_props(&self) -> InlineProps {
        let mut out = InlineProps::default();
        for frame in self.styles.iter().rev() {
            if out.color.is_none() {
                out.color = frame.props.color;
            }
            if out.size.is_none() {
                out.size = frame.props.size;
            }
            if out.align.is_none() {
                out.align = frame.props.align;
            }
            if out.background.is_none() {
                out.background = frame.props.background.clone();
            }
            if out.background_size.is_none() {
                out.background_size = frame.props.background_size;
            }
            if out.weight.is_none() {
                out.weight = frame.props.weight;
            }
            if out.italic.is_none() {
                out.italic = frame.props.italic;
            }
            if out.line_height.is_none() {
                out.line_height = frame.props.line_height;
            }
            if out.decoration.is_none() {
                out.decoration = frame.props.decoration;
            }
            if out.box_css.margin.is_none() {
                out.box_css.margin = frame.props.box_css.margin;
            }
            if out.box_css.padding.is_none() {
                out.box_css.padding = frame.props.box_css.padding;
            }
            if out.box_css.border.is_none() {
                out.box_css.border = frame.props.box_css.border;
            }
            if out.box_css.background_color.is_none() {
                out.box_css.background_color = frame.props.box_css.background_color;
            }
        }
        out
    }

    /// The link target of the innermost open `<a>`, if any.
    fn current_href(&self) -> Option<&str> {
        self.styles.iter().rev().find_map(|f| f.href.as_deref())
    }

    /// Pop the frame for `name` and everything nested above it. Malformed
    /// nesting therefore restores the outer style rather than guessing.
    fn pop_style_frame(&mut self, name: &str) {
        if let Some(pos) = self.styles.iter().rposition(|f| f.tag == name) {
            self.styles.truncate(pos);
        }
    }

    /// Collapse whitespace runs (HTML text semantics) and flush the text
    /// buffer as a span.
    fn flush_text(&mut self) {
        if self.text_buf.is_empty() {
            return;
        }
        let collapsed: String = self
            .text_buf
            .chars()
            .map(|c| if c.is_whitespace() { ' ' } else { c })
            .collect();
        self.text_buf.clear();
        if collapsed.is_empty() {
            return;
        }
        let style = self.cur_style();
        let props = self.current_props();
        let href = self.current_href().map(str::to_string);
        if let Some(last) = self.spans.last_mut() {
            if last.style == style
                && last.href == href
                && last.color == props.color
                && last.size == props.size
                && last.align == props.align
                && last.background == props.background
                && last.background_size == props.background_size
                && last.weight == props.weight
                && last.italic == props.italic
                && last.line_height == props.line_height
                && last.decoration == props.decoration.unwrap_or_default()
                && last.block_css == props.box_css
            {
                last.text.push_str(&collapsed);
                return;
            }
        }
        self.spans.push(Span {
            style,
            text: collapsed,
            href,
            color: props.color,
            size: props.size,
            weight: props.weight,
            italic: props.italic,
            line_height: props.line_height,
            decoration: props.decoration.unwrap_or_default(),
            align: props.align,
            background: props.background,
            background_size: props.background_size,
            block_css: props.box_css,
        });
    }

    fn spans_are_empty(&self) -> bool {
        self.spans.iter().all(|s| s.text.trim().is_empty())
    }

    /// Embed an `<img>` whose `src` is a JPEG, PNG or SVG data URI. Anything
    /// else (remote URL, file path, GIF/WebP data URI, malformed image, unsafe
    /// SVG) is refused with a typed image error and skipped: no text is painted
    /// and no XObject is emitted. Explicit `width`/`height` attributes override
    /// the intrinsic geometry in layout.
    fn push_image(&mut self, src: Option<&str>, width: Option<&str>, height: Option<&str>) {
        self.flush_text();
        let Some(src) = src else { return };
        let Ok(image) = parse_image_source(src) else {
            return;
        };
        let width = width.and_then(image_dimension);
        let height = height.and_then(image_dimension);
        self.blocks.push(Block::Image(ImageBlock {
            image,
            width,
            height,
        }));
    }

    /// Flush the current spans as a paragraph (or heading, when one is open).
    /// When a list item is open, the spans belong to it instead. Inside a
    /// table cell the spans stay in the cell.
    fn flush_paragraph(&mut self) {
        if self.in_table_cell() {
            self.flush_text();
            self.heading = None;
            return;
        }
        if self.in_item {
            self.close_item();
            return;
        }
        self.flush_text();
        if self.spans_are_empty() {
            self.spans.clear();
            self.heading = None;
            return;
        }
        let spans = std::mem::take(&mut self.spans);
        if let Some(level) = self.heading.take() {
            self.blocks.push(Block::Heading { level, spans });
        } else {
            self.blocks.push(Block::Paragraph(spans));
        }
    }

    /// Close the current list item, emitting a `ListItem`. Inside a table
    /// cell the spans stay in the cell.
    fn close_item(&mut self) {
        if self.in_table_cell() {
            self.flush_text();
            return;
        }
        self.flush_text();
        if self.spans_are_empty() {
            self.spans.clear();
            self.in_item = false;
            return;
        }
        let (ordered, depth) = match self.lists.last() {
            Some(ctx) => (ctx.ordered, self.lists.len().saturating_sub(1)),
            None => (false, 0),
        };
        let ordinal = self.item_ordinal;
        self.blocks.push(Block::ListItem {
            ordered,
            ordinal,
            depth: depth.min(3),
            spans: std::mem::take(&mut self.spans),
        });
        self.in_item = false;
    }

    fn handle_entity(&mut self) {
        // self.pos is at '&'
        let start = self.pos.saturating_add(1);
        let mut end = start;
        while let Some(&c) = self.chars.get(end) {
            if c == ';' || end.saturating_sub(start) > 31 {
                break;
            }
            end = end.saturating_add(1);
        }
        let found = self.chars.get(end) == Some(&';');
        let name: String = self
            .chars
            .get(start..end)
            .map_or_else(String::new, |s| s.iter().collect());
        if found {
            if let Some(replacement) = decode_entity(&name) {
                self.text_buf.push_str(&replacement);
                self.pos = end.saturating_add(1);
                return;
            }
        }
        // Unknown or unterminated: emit literally.
        self.text_buf.push('&');
        self.pos = self.pos.saturating_add(1);
    }

    fn handle_tag(&mut self) {
        // self.pos is at '<'
        if starts_with_ci(self.chars, self.pos, "<!--") {
            self.pos = find_ci(self.chars, "-->", self.pos)
                .map(|i| i.saturating_add(3))
                .unwrap_or(self.chars.len());
            return;
        }
        if starts_with_ci(self.chars, self.pos, "<!") || starts_with_ci(self.chars, self.pos, "<?")
        {
            self.pos = find_char(self.chars, '>', self.pos)
                .map(|i| i.saturating_add(1))
                .unwrap_or(self.chars.len());
            return;
        }

        let closing = self.chars.get(self.pos.saturating_add(1)) == Some(&'/');
        let name_start = if closing {
            self.pos.saturating_add(2)
        } else {
            self.pos.saturating_add(1)
        };
        let mut p = name_start;
        while let Some(&c) = self.chars.get(p) {
            if c.is_ascii_alphanumeric() {
                p = p.saturating_add(1);
            } else {
                break;
            }
        }
        let name: String = self
            .chars
            .get(name_start..p)
            .map_or_else(String::new, |s| s.iter().collect())
            .to_ascii_lowercase();
        if name.is_empty() {
            // A stray '<': emit literally.
            self.text_buf.push('<');
            self.pos = self.pos.saturating_add(1);
            return;
        }

        // Read attributes (href/style/align for styling; src/width/height for
        // images) and find the tag's end.
        let mut href: Option<String> = None;
        let mut style: Option<String> = None;
        let mut align: Option<String> = None;
        let mut src: Option<String> = None;
        let mut width: Option<String> = None;
        let mut height: Option<String> = None;
        let mut class: Option<String> = None;
        let mut id: Option<String> = None;
        let mut self_closing = false;
        let mut q = p;
        loop {
            match self.chars.get(q).copied() {
                None => {
                    self.pos = self.chars.len();
                    return;
                }
                Some('>') => {
                    q = q.saturating_add(1);
                    break;
                }
                Some('/') => {
                    if self.chars.get(q.saturating_add(1)) == Some(&'>') {
                        self_closing = true;
                        q = q.saturating_add(2);
                        break;
                    }
                    q = q.saturating_add(1);
                }
                Some(c) if c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == ':' => {
                    let attr_start = q;
                    while let Some(&ac) = self.chars.get(q) {
                        if ac.is_ascii_alphanumeric() || ac == '-' || ac == '_' || ac == ':' {
                            q = q.saturating_add(1);
                        } else {
                            break;
                        }
                    }
                    let attr: String = self
                        .chars
                        .get(attr_start..q)
                        .map_or_else(String::new, |s| s.iter().collect())
                        .to_ascii_lowercase();
                    // Skip whitespace, expect '='.
                    while self.chars.get(q) == Some(&' ') {
                        q = q.saturating_add(1);
                    }
                    if self.chars.get(q) == Some(&'=') {
                        q = q.saturating_add(1);
                        while self.chars.get(q) == Some(&' ') {
                            q = q.saturating_add(1);
                        }
                        let quoted = matches!(self.chars.get(q), Some('"') | Some('\''));
                        if quoted {
                            let quote = self.chars.get(q).copied().unwrap_or('"');
                            q = q.saturating_add(1);
                            let vstart = q;
                            while let Some(&vc) = self.chars.get(q) {
                                if vc == quote {
                                    break;
                                }
                                q = q.saturating_add(1);
                            }
                            let value: String = self
                                .chars
                                .get(vstart..q)
                                .map_or_else(String::new, |s| s.iter().collect());
                            if quoted {
                                q = q.saturating_add(1);
                            }
                            keep_attribute(
                                &attr,
                                value,
                                &mut href,
                                &mut style,
                                &mut align,
                                &mut src,
                                &mut width,
                                &mut height,
                                &mut class,
                                &mut id,
                            );
                        } else {
                            let vstart = q;
                            while let Some(&vc) = self.chars.get(q) {
                                if vc.is_whitespace() || vc == '>' {
                                    break;
                                }
                                q = q.saturating_add(1);
                            }
                            let value: String = self
                                .chars
                                .get(vstart..q)
                                .map_or_else(String::new, |s| s.iter().collect());
                            keep_attribute(
                                &attr,
                                value,
                                &mut href,
                                &mut style,
                                &mut align,
                                &mut src,
                                &mut width,
                                &mut height,
                                &mut class,
                                &mut id,
                            );
                        }
                    }
                }
                Some(_) => {
                    q = q.saturating_add(1);
                }
            }
        }
        self.pos = q;

        let (break_before, break_after) = css_page_breaks(style.as_deref());
        if closing {
            self.close_tag(&name);
            self.pop_style_frame(&name);
            // A break the matching open tag asked for lands after its content.
            if self
                .break_after_stack
                .last()
                .is_some_and(|open| open == &name)
            {
                self.break_after_stack.pop();
                self.emit_page_break();
            }
        } else {
            if break_before {
                self.emit_page_break();
            }
            if name == "img" {
                self.push_image(src.as_deref(), width.as_deref(), height.as_deref());
                return;
            }
            self.open_tag(&name, href.as_deref(), self_closing);
            if !self_closing && is_styled_container(&name) {
                // Two-level cascade: the rulesheet value is the base, and the
                // element's own inline declarations override it property by
                // property. Nested frames then apply innermost-wins as before.
                let classes: Vec<String> = class
                    .as_deref()
                    .unwrap_or("")
                    .split_ascii_whitespace()
                    .map(str::to_ascii_lowercase)
                    .collect();
                let element_id = id.as_deref().map(str::to_ascii_lowercase);
                let from_sheet = self.rules.props_for(&name, element_id.as_deref(), &classes);
                let inline = InlineProps::from_attrs(style.as_deref(), align.as_deref());
                self.styles.push(StyleFrame {
                    tag: name.clone(),
                    props: from_sheet.overridden_by(inline),
                    href: if name == "a" { href } else { None },
                });
            }
            if break_after {
                if self_closing {
                    self.emit_page_break();
                } else {
                    self.break_after_stack.push(name.clone());
                }
            }
        }
    }

    /// Close any open paragraph and start a new page.
    fn emit_page_break(&mut self) {
        self.flush_paragraph();
        self.blocks.push(Block::PageBreak);
    }

    #[allow(clippy::too_many_lines)]
    fn open_tag(&mut self, name: &str, href: Option<&str>, self_closing: bool) {
        match name {
            "br" => {
                self.flush_paragraph();
            }
            "hr" => {
                self.flush_paragraph();
                self.blocks.push(Block::Rule);
            }
            "script" | "style" => {
                let close = format!("</{name}");
                self.pos = find_ci(self.chars, &close, self.pos).unwrap_or(self.chars.len());
            }
            "pre" => {
                self.flush_paragraph();
                let (lines, new_pos) = extract_pre(self.chars, self.pos);
                self.pos = new_pos;
                if !lines.is_empty() {
                    self.blocks.push(Block::Code(lines));
                }
            }
            "blockquote" => {
                self.flush_paragraph();
                let (inner, new_pos) = extract_balanced(self.chars, self.pos, "blockquote");
                self.pos = new_pos;
                self.blocks.push(Block::Quote(parse(&inner)));
            }
            "ul" | "ol" => {
                if self.in_table_cell() {
                    // Lists inside cells are text, not structure.
                    return;
                }
                self.flush_paragraph();
                self.lists.push(ListCtx {
                    ordered: name == "ol",
                    counter: 0,
                });
            }
            "li" => {
                if self.in_table_cell() {
                    self.flush_text();
                    return;
                }
                if self.in_item {
                    self.close_item();
                }
                self.in_item = true;
                if let Some(ctx) = self.lists.last_mut() {
                    ctx.counter = ctx.counter.saturating_add(1);
                    self.item_ordinal = ctx.counter;
                } else {
                    // A stray <li> outside a list: render as an unordered item.
                    self.lists.push(ListCtx {
                        ordered: false,
                        counter: 1,
                    });
                    self.item_ordinal = 1;
                }
            }
            "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => {
                self.flush_paragraph();
                let level = name
                    .get(1..2)
                    .and_then(|d| d.parse::<u8>().ok())
                    .unwrap_or(6);
                self.heading = Some(level);
            }
            "b" | "strong" => {
                self.flush_text();
                self.bold = self.bold.saturating_add(1);
            }
            "i" | "em" => {
                self.flush_text();
                self.italic = self.italic.saturating_add(1);
            }
            "code" => {
                self.flush_text();
                self.code_depth = self.code_depth.saturating_add(1);
            }
            "span" => {
                // Inline styling boundary: flush so the styled run is its own
                // span (unknown tag otherwise just unwraps its text).
                self.flush_text();
            }
            "a" => {
                self.flush_text();
                if href.is_some() {
                    self.href_depth = self.href_depth.saturating_add(1);
                }
            }
            "table" => {
                if self.in_table_cell() || self.table.is_some() {
                    // Nested tables inside cells are text, not structure.
                } else {
                    self.flush_paragraph();
                    self.table = Some(TableBuilder::default());
                }
            }
            "thead" | "tbody" | "tfoot" | "caption" => {
                // Section markers: no layout effect in the subset.
            }
            "tr" => {
                if self.table.is_some() {
                    self.close_row();
                    if let Some(table) = self.table.as_mut() {
                        table.row = Some(RowBuilder::default());
                    }
                    self.heading = None;
                }
            }
            "td" | "th" => {
                if self.table.is_some() {
                    if self.table.as_ref().and_then(|t| t.row.as_ref()).is_none() {
                        if let Some(table) = self.table.as_mut() {
                            table.row = Some(RowBuilder::default());
                        }
                    }
                    self.close_cell();
                    if let Some(table) = self.table.as_mut() {
                        if let Some(row) = table.row.as_mut() {
                            if name == "th" {
                                row.saw_th = true;
                            }
                            row.cell = Some(Vec::new());
                        }
                    }
                    self.heading = None;
                } else {
                    self.flush_paragraph();
                }
            }
            "p" | "div" | "section" | "article" | "main" | "header" | "footer" | "nav"
            | "aside" | "figure" | "figcaption" | "form" | "fieldset" | "center" | "details"
            | "summary" => {
                self.flush_paragraph();
            }
            _ => {
                // Unknown tags unwrap: their text flows into the current block.
            }
        }
        let _ = self_closing;
    }

    fn close_tag(&mut self, name: &str) {
        match name {
            "table" => {
                self.close_table();
            }
            "td" | "th" => {
                self.close_cell();
            }
            "tr" => {
                self.close_row();
            }
            "thead" | "tbody" | "tfoot" | "caption" => {}
            "p" | "div" | "section" | "article" | "main" | "header" | "footer" | "nav"
            | "aside" | "figure" | "figcaption" | "form" | "fieldset" | "center" | "details"
            | "summary" => {
                self.flush_paragraph();
            }
            "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => {
                self.flush_paragraph();
            }
            "li" => {
                if self.in_item {
                    self.close_item();
                }
            }
            "ul" | "ol" => {
                if self.in_item {
                    self.close_item();
                }
                self.lists.pop();
                if self.lists.is_empty() {
                    // A paragraph break after the outermost list.
                    self.flush_paragraph();
                }
            }
            "b" | "strong" => {
                self.flush_text();
                self.bold = self.bold.saturating_sub(1);
            }
            "i" | "em" => {
                self.flush_text();
                self.italic = self.italic.saturating_sub(1);
            }
            "code" => {
                self.flush_text();
                self.code_depth = self.code_depth.saturating_sub(1);
            }
            "span" => {
                self.flush_text();
            }
            "a" => {
                self.flush_text();
                self.href_depth = self.href_depth.saturating_sub(1);
            }
            _ => {}
        }
    }
}

/// Extract a `<pre>` body: raw text until `</pre>`, with inner tags stripped
/// and entities decoded. Returns the code lines and the position after the
/// closing tag.
fn extract_pre(chars: &[char], from: usize) -> (Vec<String>, usize) {
    let end = find_ci(chars, "</pre", from).unwrap_or(chars.len());
    let inner: String = chars
        .get(from..end)
        .map_or_else(String::new, |s| s.iter().collect());
    // Strip inner tags (typically <code>).
    let mut stripped = String::new();
    let mut in_tag = false;
    for c in inner.chars() {
        match c {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ => {
                if !in_tag {
                    stripped.push(c);
                }
            }
        }
    }
    let decoded = decode_all_entities(&stripped);
    let mut lines: Vec<String> = decoded.lines().map(str::to_string).collect();
    // Drop a leading and trailing empty line (markup whitespace).
    if lines.first().is_some_and(|l| l.trim().is_empty()) {
        lines.remove(0);
    }
    while lines.last().is_some_and(|l| l.trim().is_empty()) {
        lines.pop();
    }
    // Position after "</pre" plus its '>' if present.
    let after = if end < chars.len() {
        find_char(chars, '>', end)
            .map(|i| i.saturating_add(1))
            .unwrap_or(chars.len())
    } else {
        chars.len()
    };
    (lines, after)
}

/// Extract the inner text of a balanced container element starting right after
/// its opening tag. Returns the inner text and the position after the matching
/// closing tag.
fn extract_balanced(chars: &[char], from: usize, tag: &str) -> (String, usize) {
    let mut depth = 1usize;
    let mut p = from;
    let open_pat = format!("<{tag}");
    let close_pat = format!("</{tag}");
    let mut inner_end = chars.len();
    while p < chars.len() {
        if starts_with_ci(chars, p, &close_pat) {
            depth = depth.saturating_sub(1);
            if depth == 0 {
                inner_end = p;
                p = find_char(chars, '>', p)
                    .map(|i| i.saturating_add(1))
                    .unwrap_or(chars.len());
                break;
            }
            p = find_char(chars, '>', p)
                .map(|i| i.saturating_add(1))
                .unwrap_or(chars.len());
        } else if starts_with_ci(chars, p, &open_pat) {
            // Only count genuine opening tags (followed by space or '>').
            let after = p.saturating_add(open_pat.len());
            if matches!(
                chars.get(after),
                Some(' ') | Some('>') | Some('\t') | Some('\n')
            ) {
                depth = depth.saturating_add(1);
            }
            p = p.saturating_add(1);
        } else {
            p = p.saturating_add(1);
        }
    }
    let inner: String = chars
        .get(from..inner_end)
        .map_or_else(String::new, |s| s.iter().collect());
    (inner, p)
}

/// Decode every `&entity;` in `text`.
fn decode_all_entities(text: &str) -> String {
    let mut out = String::new();
    let chars: Vec<char> = text.chars().collect();
    let mut i = 0usize;
    while let Some(&c) = chars.get(i) {
        if c == '&' {
            let start = i.saturating_add(1);
            let mut end = start;
            while let Some(&e) = chars.get(end) {
                if e == ';' || end.saturating_sub(start) > 31 {
                    break;
                }
                end = end.saturating_add(1);
            }
            if chars.get(end) == Some(&';') {
                let name: String = chars
                    .get(start..end)
                    .map_or_else(String::new, |s| s.iter().collect());
                if let Some(replacement) = decode_entity(&name) {
                    out.push_str(&replacement);
                    i = end.saturating_add(1);
                    continue;
                }
            }
            out.push('&');
            i = i.saturating_add(1);
        } else {
            out.push(c);
            i = i.saturating_add(1);
        }
    }
    out
}

/// Decode one entity name (without the `&` and `;`).
fn decode_entity(name: &str) -> Option<String> {
    if let Some(num) = name.strip_prefix('#') {
        let code = if let Some(hex) = num.strip_prefix('x').or_else(|| num.strip_prefix('X')) {
            u32::from_str_radix(hex, 16).ok()?
        } else {
            num.parse::<u32>().ok()?
        };
        return char::from_u32(code).map(|c| c.to_string());
    }
    let c = match name {
        "amp" => '&',
        "lt" => '<',
        "gt" => '>',
        "quot" => '"',
        "apos" => '\'',
        "nbsp" => '\u{00A0}',
        "shy" => '\u{00AD}',
        "mdash" => '\u{2014}',
        "ndash" => '\u{2013}',
        "hellip" => '\u{2026}',
        "laquo" => '\u{00AB}',
        "raquo" => '\u{00BB}',
        "ldquo" => '\u{201C}',
        "rdquo" => '\u{201D}',
        "lsquo" => '\u{2018}',
        "rsquo" => '\u{2019}',
        "sbquo" => '\u{201A}',
        "bdquo" => '\u{201E}',
        "bull" | "bullet" => '\u{2022}',
        "middot" => '\u{00B7}',
        "copy" => '\u{00A9}',
        "reg" => '\u{00AE}',
        "trade" => '\u{2122}',
        "times" => '\u{00D7}',
        "divide" => '\u{00F7}',
        "plusmn" => '\u{00B1}',
        "deg" => '\u{00B0}',
        "micro" => '\u{00B5}',
        "para" => '\u{00B6}',
        "sect" => '\u{00A7}',
        "dagger" => '\u{2020}',
        "Dagger" => '\u{2021}',
        "permil" => '\u{2030}',
        "euro" => '\u{20AC}',
        "pound" => '\u{00A3}',
        "yen" => '\u{00A5}',
        "cent" => '\u{00A2}',
        "curren" => '\u{00A4}',
        "szlig" => '\u{00DF}',
        "agrave" => '\u{00E0}',
        "aacute" => '\u{00E1}',
        "acirc" => '\u{00E2}',
        "atilde" => '\u{00E3}',
        "auml" => '\u{00E4}',
        "aring" => '\u{00E5}',
        "aelig" => '\u{00E6}',
        "ccedil" => '\u{00E7}',
        "egrave" => '\u{00E8}',
        "eacute" => '\u{00E9}',
        "ecirc" => '\u{00EA}',
        "euml" => '\u{00EB}',
        "igrave" => '\u{00EC}',
        "iacute" => '\u{00ED}',
        "icirc" => '\u{00EE}',
        "iuml" => '\u{00EF}',
        "eth" => '\u{00F0}',
        "ntilde" => '\u{00F1}',
        "ograve" => '\u{00F2}',
        "oacute" => '\u{00F3}',
        "ocirc" => '\u{00F4}',
        "otilde" => '\u{00F5}',
        "ouml" => '\u{00F6}',
        "oslash" => '\u{00F8}',
        "ugrave" => '\u{00F9}',
        "uacute" => '\u{00FA}',
        "ucirc" => '\u{00FB}',
        "uuml" => '\u{00FC}',
        "yacute" => '\u{00FD}',
        "thorn" => '\u{00FE}',
        "yuml" => '\u{00FF}',
        "Agrave" => '\u{00C0}',
        "Aacute" => '\u{00C1}',
        "Acirc" => '\u{00C2}',
        "Atilde" => '\u{00C3}',
        "Auml" => '\u{00C4}',
        "Aring" => '\u{00C5}',
        "AElig" => '\u{00C6}',
        "Ccedil" => '\u{00C7}',
        "Egrave" => '\u{00C8}',
        "Eacute" => '\u{00C9}',
        "Ecirc" => '\u{00CA}',
        "Euml" => '\u{00CB}',
        "Igrave" => '\u{00CC}',
        "Iacute" => '\u{00CD}',
        "Icirc" => '\u{00CE}',
        "Iuml" => '\u{00CF}',
        "ETH" => '\u{00D0}',
        "Ntilde" => '\u{00D1}',
        "Ograve" => '\u{00D2}',
        "Oacute" => '\u{00D3}',
        "Ocirc" => '\u{00D4}',
        "Otilde" => '\u{00D5}',
        "Ouml" => '\u{00D6}',
        "Oslash" => '\u{00D8}',
        "Ugrave" => '\u{00D9}',
        "Uacute" => '\u{00DA}',
        "Ucirc" => '\u{00DB}',
        "Uuml" => '\u{00DC}',
        "Yacute" => '\u{00DD}',
        "THORN" => '\u{00DE}',
        _ => return None,
    };
    Some(c.to_string())
}

/// Whether an opening tag carries inline style state that must be restored on
/// its close. Void elements and the two "extract wholesale" containers
/// (`<pre>`, `<blockquote>`) are excluded: they never reach `close_tag`.
fn is_styled_container(name: &str) -> bool {
    matches!(
        name,
        "p" | "div"
            | "section"
            | "article"
            | "main"
            | "header"
            | "footer"
            | "nav"
            | "aside"
            | "figure"
            | "figcaption"
            | "form"
            | "fieldset"
            | "center"
            | "details"
            | "summary"
            | "h1"
            | "h2"
            | "h3"
            | "h4"
            | "h5"
            | "h6"
            | "b"
            | "strong"
            | "i"
            | "em"
            | "code"
            | "a"
            | "span"
            | "ul"
            | "ol"
            | "li"
            | "table"
            | "thead"
            | "tbody"
            | "tfoot"
            | "caption"
            | "tr"
            | "td"
            | "th"
    )
}

/// The narrow `text-align` / `align` subset: only left, centre and right.
/// Any other value (including `justify`) is ignored.
fn css_align(value: &str) -> Option<TextAlign> {
    match value.trim().to_ascii_lowercase().as_str() {
        "left" => Some(TextAlign::Left),
        "center" => Some(TextAlign::Center),
        "right" => Some(TextAlign::Right),
        _ => None,
    }
}

/// Split an inline `style=` value on top-level semicolons only.
///
/// Semicolons inside a quoted string or a `url(...)` token are data (a data URI
/// carries `image/png;base64,…`), not declaration separators. Everything else
/// is left verbatim; a stray quote/paren simply keeps the rest of the value in
/// the current declaration rather than failing the whole attribute.
///
/// # Budget
///
/// Allocates one slice list proportional to the number of declarations; the
/// source attribute is bounded by the document text.
///
/// # Malformed Input
///
/// Never fails: unbalanced quotes or parentheses are tolerated and yield one
/// declaration covering the remainder of the string.
fn split_declarations(style: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = 0usize;
    let mut depth = 0u32;
    let mut quote: Option<char> = None;
    for (i, ch) in style.char_indices() {
        if let Some(q) = quote {
            if ch == q {
                quote = None;
            }
        } else {
            match ch {
                '\'' | '"' => quote = Some(ch),
                '(' => depth = depth.saturating_add(1),
                ')' => depth = depth.saturating_sub(1),
                ';' if depth == 0 => {
                    out.push(style.get(start..i).unwrap_or(""));
                    start = i.saturating_add(1);
                }
                _ => {}
            }
        }
    }
    out.push(style.get(start..).unwrap_or(""));
    out
}

/// Parse the narrow `background-image: url("data:image/…")` subset.
///
/// Exactly one `url(...)` token is accepted, quoted (`"` or `'`) or bare. Its
/// payload goes through the same [`parse_data_uri`] as `<img src>`, so only
/// base64 JPEG/PNG data URIs decode; remote URLs are refused *by name* and
/// dropped. Gradients, multiple images and other values return `None` and are
/// ignored by the caller — this is not a general CSS engine.
///
/// # Budget
///
/// No charge at this boundary: the decoded image's bytes are charged when the
/// converter writes its XObject and lays out the runs.
///
/// # Malformed Input
///
/// `None` for anything that is not a single `url(...)` data URI, and for any
/// image the parser refuses (bad base64, malformed JPEG/PNG, unsupported PNG
/// feature, remote scheme). No partial image is returned.
fn css_background_image(value: &str) -> Option<EmbeddedImage> {
    let v = value.trim();
    if v.len() < 5 || !v.get(..4)?.eq_ignore_ascii_case("url(") || !v.ends_with(')') {
        return None;
    }
    let inner = v.get(4..v.len().saturating_sub(1))?.trim();
    let uri = if inner.len() >= 2
        && ((inner.starts_with('"') && inner.ends_with('"'))
            || (inner.starts_with('\'') && inner.ends_with('\'')))
    {
        inner.get(1..inner.len().saturating_sub(1))?
    } else {
        inner
    };
    parse_data_uri(uri).ok()
}

/// Parse the narrow CSS `background-size` subset: `auto`, `contain`, `cover`,
/// and one or two absolute lengths (`pt`, `px` at 96 dpi, or a bare number in
/// points). Percentages, keywords other than the four above and relative units
/// are ignored. A single length sets the width; the height follows the image's
/// intrinsic aspect ratio in layout.
///
/// # Budget
///
/// Allocates at most a couple of small string slices from the declaration;
/// the attribute is bounded by the document text.
///
/// # Malformed Input
///
/// `None` for any unsupported value, an empty value, or a non-finite/non-positive
/// length — the caller leaves the legacy stretch-to-box behaviour in place.
fn css_background_size(value: &str) -> Option<BackgroundSize> {
    let v = value.trim().to_ascii_lowercase();
    match v.as_str() {
        "auto" => return Some(BackgroundSize::Auto),
        "contain" => return Some(BackgroundSize::Contain),
        "cover" => return Some(BackgroundSize::Cover),
        _ => {}
    }
    let mut tokens = v.split_ascii_whitespace();
    let first = image_dimension(tokens.next()?)?;
    let second = match tokens.next() {
        Some(token) if token != "auto" => Some(image_dimension(token)?),
        _ => None,
    };
    if tokens.next().is_some() {
        return None;
    }
    Some(BackgroundSize::Length {
        width: Some(first),
        height: second,
    })
}

/// The inner text of the first `<style>` element, if any.
///
/// # Budget
///
/// One lowercase copy of the input for the ASCII-only tag search.
///
/// # Malformed Input
///
/// Never fails: an unterminated or self-closing `<style>` contributes no rules.
fn first_style_block(input: &str) -> Option<&str> {
    let lower = input.to_ascii_lowercase();
    let start = lower.find("<style")?;
    let after_open = lower
        .get(start..)?
        .find('>')?
        .saturating_add(start)
        .saturating_add(1);
    let close = lower
        .get(after_open..)?
        .find("</style")?
        .saturating_add(after_open);
    input.get(after_open..close)
}

/// Remove `/* … */` comments from a CSS text (unterminated comments run to EOF).
///
/// # Budget
///
/// Allocates one string bounded by the rulesheet text.
///
/// # Malformed Input
///
/// Never fails; an unterminated comment is simply dropped with the rest.
fn strip_css_comments(css: &str) -> String {
    let mut out = String::with_capacity(css.len());
    let mut rest = css;
    while let Some(at) = rest.find("/*") {
        out.push_str(rest.get(..at).unwrap_or(""));
        let from = at.saturating_add(2);
        let Some(end) = rest.get(from..).and_then(|tail| tail.find("*/")) else {
            return out;
        };
        rest = rest
            .get(from.saturating_add(end).saturating_add(2)..)
            .unwrap_or("");
    }
    out.push_str(rest);
    out
}

/// The fixed viewport width (in CSS px) used to evaluate the one supported
/// `@media` feature. HTML parsing has no page geometry, so a single documented
/// width keeps the condition deterministic — never a guess at the reader's
/// actual window.
const MEDIA_WIDTH_PX: f64 = 800.0;

/// Evaluate the narrow `@media (min-width|max-width|width: <px|pt>)` condition.
///
/// A single condition (optionally after `screen`/`all` and `and`) is accepted;
/// media types such as `print`, feature lists and every other at-rule are
/// refused. `@media` never changes specificity, only whether its rules exist.
///
/// # Budget
///
/// A lowercase copy of the prelude and a handful of small slices.
///
/// # Malformed Input
///
/// `false` for anything that is not exactly one supported condition.
fn media_matches(prelude: &str) -> bool {
    let lower = prelude.trim().to_ascii_lowercase();
    let Some(rest) = lower.strip_prefix("@media") else {
        return false;
    };
    let condition = rest
        .split("and")
        .map(str::trim)
        .find(|part| part.starts_with('('))
        .unwrap_or("");
    let Some(inner) = condition
        .strip_prefix('(')
        .and_then(|text| text.strip_suffix(')'))
    else {
        return false;
    };
    let mut pair = inner.splitn(2, ':');
    let feature = pair.next().unwrap_or("").trim();
    let value = pair.next().unwrap_or("").trim();
    let (number, scale) = if let Some(n) = value.strip_suffix("px") {
        (n, 1.0)
    } else if let Some(n) = value.strip_suffix("pt") {
        (n, 96.0_f64 / 72.0_f64)
    } else {
        return false;
    };
    let Ok(limit) = number.trim().parse::<f64>() else {
        return false;
    };
    if !limit.is_finite() || limit < 0.0 {
        return false;
    }
    let limit = limit * scale;
    match feature {
        "max-width" => MEDIA_WIDTH_PX <= limit,
        "min-width" => MEDIA_WIDTH_PX >= limit,
        "width" => (MEDIA_WIDTH_PX - limit).abs() < 0.5,
        _ => false,
    }
}

/// Parse the single inline `<style>` rulesheet into simple-selector rules.
///
/// Supported selectors: an element type, a class (`.c`), an ID (`#i`) and
/// simple compounds of those (`p.c`, `.a.b`, `p#i.c`). Selectors are matched
/// against each element's `class`/`id` attributes and cascaded by specificity
/// (ID > class > type) then source order. Supported declarations are exactly
/// the inline set (see [`InlineProps::from_css`]). `@media` with one width
/// condition is honoured; every other at-rule and its contents are ignored.
/// So are descendant/child/universal/pseudo/attribute selectors, a second
/// `<style>` block and external stylesheets. Nothing is guessed or fetched.
///
/// # Budget
///
/// The CSS text is bounded by the document, and every accepted rule stores a
/// small selector plus its declarations; no work is proportional to the page
/// count because matching happens once, at element open time.
///
/// # Malformed Input
///
/// Never fails: unbalanced braces, stray text and unsupported selectors simply
/// produce no rule. Unclosed at-rule blocks silently swallow the remainder.
fn parse_rulesheet(input: &str) -> Rulesheet {
    let Some(css) = first_style_block(input) else {
        return Rulesheet::default();
    };
    let css = strip_css_comments(css);
    let mut sheet = Rulesheet::default();
    // A stack lets nested style rules inside `@media` keep their own selector
    // and declarations, while unsupported at-rules swallow everything inside.
    let mut stack: Vec<BlockCtx> = Vec::new();
    let mut prelude = String::new();
    for ch in css.chars() {
        // Decide the action from an immutable peek so we can mutate afterwards.
        enum Action {
            Root,
            InAtRule(bool),
            InStyle,
        }
        let action = match stack.last() {
            None => Action::Root,
            Some(BlockCtx::AtRule { include }) => Action::InAtRule(*include),
            Some(BlockCtx::Style { .. }) => Action::InStyle,
        };
        match ch {
            '{' => match action {
                Action::Root | Action::InAtRule(_) => {
                    let parent = match action {
                        Action::Root => true,
                        Action::InAtRule(include) => include,
                        Action::InStyle => false,
                    };
                    let text = std::mem::take(&mut prelude);
                    if text.trim().starts_with('@') {
                        stack.push(BlockCtx::AtRule {
                            include: parent && media_matches(&text),
                        });
                    } else {
                        stack.push(BlockCtx::Style {
                            selector: text,
                            body: String::new(),
                            include: parent,
                            nested: 0,
                        });
                    }
                }
                Action::InStyle => {
                    if let Some(BlockCtx::Style { body, nested, .. }) = stack.last_mut() {
                        *nested = nested.saturating_add(1);
                        body.push(ch);
                    }
                }
            },
            '}' => match stack.last() {
                Some(BlockCtx::Style { nested, .. }) if *nested > 0 => {
                    if let Some(BlockCtx::Style { body, nested, .. }) = stack.last_mut() {
                        *nested = nested.saturating_sub(1);
                        body.push(ch);
                    }
                }
                Some(BlockCtx::Style { .. }) => {
                    if let Some(BlockCtx::Style {
                        selector,
                        body,
                        include,
                        ..
                    }) = stack.pop()
                    {
                        if include {
                            add_rulesheet_rule(&selector, &body, &mut sheet);
                        }
                    }
                }
                Some(BlockCtx::AtRule { .. }) => {
                    stack.pop();
                }
                None => {}
            },
            _ => match action {
                Action::Root | Action::InAtRule(_) => prelude.push(ch),
                Action::InStyle => {
                    if let Some(BlockCtx::Style { body, .. }) = stack.last_mut() {
                        body.push(ch);
                    }
                }
            },
        }
    }
    sheet
}

/// One open brace context while reading a rulesheet.
enum BlockCtx {
    /// An at-rule block: `include` says whether nested rules are kept.
    AtRule { include: bool },
    /// A style rule, with its selector, declaration body and nested-brace count.
    Style {
        selector: String,
        body: String,
        include: bool,
        nested: usize,
    },
}

/// Add one `{ selector { declarations } }` rule when its selector is a
/// supported simple selector. Every rule is kept in source order; specificity
/// is computed once here and used when the rule is later matched.
///
/// # Budget
///
/// One selector + one property set per accepted rule, bounded by the CSS text.
///
/// # Malformed Input
///
/// An unsupported selector or empty declaration body contributes no rule.
fn add_rulesheet_rule(selector: &str, body: &str, rules: &mut Rulesheet) {
    let Some(selector) = SimpleSelector::parse(selector) else {
        return;
    };
    let specificity = selector.specificity();
    let props = InlineProps::from_css(body);
    let order = rules.rules.len();
    rules.rules.push(Rule {
        selector,
        props,
        specificity,
        order,
    });
}

/// Store a recognised attribute into the tag scanner's scratch slots. Shared
/// by the quoted and unquoted value paths so the match is compiled once. The
/// slot list is deliberately flat (rather than a struct) to keep the scanner's
/// per-tag scratch state allocation-free.
#[allow(clippy::too_many_arguments)]
fn keep_attribute(
    attr: &str,
    value: String,
    href: &mut Option<String>,
    style: &mut Option<String>,
    align: &mut Option<String>,
    src: &mut Option<String>,
    width: &mut Option<String>,
    height: &mut Option<String>,
    class: &mut Option<String>,
    id: &mut Option<String>,
) {
    match attr {
        "href" => *href = Some(value),
        "style" => *style = Some(value),
        "align" => *align = Some(value),
        "src" => *src = Some(value),
        "width" => *width = Some(value),
        "height" => *height = Some(value),
        "class" => *class = Some(value),
        "id" => *id = Some(value),
        _ => {}
    }
}

/// Parse an HTML image `width`/`height` attribute into points.
///
/// The units mirror [`css_font_size`] — a bare number is points, `pt` is
/// points, `px` is converted at 96 dpi (×0.75). Unlike font sizes there is no
/// upper clamp here: the layout clamps the placement rectangle to the page's
/// text width instead. Non-positive, non-finite and unknown values are refused.
fn image_dimension(value: &str) -> Option<f64> {
    let v = value.trim().to_ascii_lowercase();
    let (number, scale) = if let Some(n) = v.strip_suffix("pt") {
        (n.trim(), 1.0)
    } else if let Some(n) = v.strip_suffix("px") {
        (n.trim(), 0.75)
    } else if v
        .chars()
        .last()
        .is_some_and(|c| c.is_ascii_digit() || c == '.')
    {
        (v.as_str(), 1.0)
    } else {
        return None;
    };
    let size = number.parse::<f64>().ok()?;
    if !size.is_finite() || size <= 0.0 {
        return None;
    }
    Some(size * scale)
}

/// Parse one finite CSS number.
///
/// # Budget
///
/// One string allocation bounded by the declaration text.
///
/// # Malformed Input
///
/// `None` for text that is not a finite number.
fn css_number(value: &str) -> Option<f64> {
    let number = value.trim().parse::<f64>().ok()?;
    number.is_finite().then_some(number)
}

/// Parse one non-negative CSS number (margins/padding are not allowed to be
/// negative in this subset).
///
/// # Budget
///
/// One string allocation bounded by the declaration value.
///
/// # Malformed Input
///
/// `None` for non-numeric, non-finite or negative text.
fn css_nonnegative(value: &str) -> Option<f64> {
    let number = css_number(value)?;
    (number >= 0.0).then_some(number)
}

/// Parse the supported CSS length units.
///
/// `pt`, `px` (folded to points at 96 dpi) and a bare number (points) are
/// absolute; `em` and `rem` stay relative; a percentage becomes a fraction.
/// `vh`/`vw` are viewport units and are **refused by name**, never approximated.
/// Negative lengths are refused; every other unit or keyword returns `None`.
///
/// # Budget
///
/// One lowercase copy of the declaration value.
///
/// # Malformed Input
///
/// `None` for anything outside the documented unit set or a non-finite value.
fn css_length(value: &str) -> Option<Length> {
    let v = value.trim().to_ascii_lowercase();
    if v.ends_with("vh") || v.ends_with("vw") {
        return None;
    }
    if let Some(number) = v.strip_suffix("rem") {
        return css_nonnegative(number).map(Length::Rem);
    }
    if let Some(number) = v.strip_suffix("em") {
        return css_nonnegative(number).map(Length::Em);
    }
    if let Some(number) = v.strip_suffix("pt") {
        return css_nonnegative(number).map(Length::Pt);
    }
    if let Some(number) = v.strip_suffix("px") {
        return css_nonnegative(number).map(|n| Length::Pt(n * 0.75_f64));
    }
    if let Some(number) = v.strip_suffix('%') {
        return css_nonnegative(number).map(|n| Length::Percent(n / 100.0_f64));
    }
    if v.chars()
        .last()
        .is_some_and(|c| c.is_ascii_digit() || c == '.')
    {
        return css_nonnegative(&v).map(Length::Pt);
    }
    None
}

/// A narrow `font-size` parser. Absolute lengths are clamped to [1, 72] pt;
/// `em`/`rem`/`%` are kept relative and clamped when layout resolves them.
/// Keywords (`small`, `larger`, …) and viewport units are ignored.
///
/// # Budget
///
/// One lowercase copy of the declaration value via [`css_length`].
///
/// # Malformed Input
///
/// `None` for anything [`css_length`] refuses.
fn css_font_size(value: &str) -> Option<Length> {
    match css_length(value)? {
        Length::Pt(points) => Some(Length::Pt(points.clamp(1.0, 72.0))),
        relative => Some(relative),
    }
}

/// Parse the 1–4 value `margin` / `padding` shorthand, mirroring CSS's
/// top/right/bottom/left expansion. Relative units are kept for layout.
///
/// # Budget
///
/// One token list bounded by the declaration text.
///
/// # Malformed Input
///
/// `None` for zero, more than four, or unparseable values — the property is
/// then left at its inherited/default value.
fn css_box_edges(value: &str) -> Option<BoxEdges> {
    let lengths: Vec<Length> = value
        .split_ascii_whitespace()
        .map(css_length)
        .collect::<Option<Vec<_>>>()?;
    match lengths.len() {
        1 => {
            let all = lengths.first().copied()?;
            Some(BoxEdges {
                top: all,
                right: all,
                bottom: all,
                left: all,
            })
        }
        2 => {
            let vertical = lengths.first().copied()?;
            let horizontal = lengths.get(1).copied()?;
            Some(BoxEdges {
                top: vertical,
                right: horizontal,
                bottom: vertical,
                left: horizontal,
            })
        }
        3 => {
            let top = lengths.first().copied()?;
            let horizontal = lengths.get(1).copied()?;
            let bottom = lengths.get(2).copied()?;
            Some(BoxEdges {
                top,
                right: horizontal,
                bottom,
                left: horizontal,
            })
        }
        4 => Some(BoxEdges {
            top: lengths.first().copied()?,
            right: lengths.get(1).copied()?,
            bottom: lengths.get(2).copied()?,
            left: lengths.get(3).copied()?,
        }),
        _ => None,
    }
}

/// Parse `line-height`: a positive unitless factor or a supported length.
/// `normal` and viewport units are ignored (the layout default is kept).
///
/// # Budget
///
/// One lowercase copy of the declaration value.
///
/// # Malformed Input
///
/// `None` for zero, negative, unknown units or an empty value.
fn css_line_height(value: &str) -> Option<LineHeight> {
    let v = value.trim().to_ascii_lowercase();
    if v.is_empty() || v == "normal" || v.ends_with("vh") || v.ends_with("vw") {
        return None;
    }
    if !v.chars().any(|c| c.is_ascii_alphabetic() || c == '%') {
        let factor = css_number(&v)?;
        return (factor > 0.0).then_some(LineHeight::Factor(factor));
    }
    css_length(&v).map(LineHeight::Length)
}

/// The `font-weight` subset: `normal` (400) and `bold` (700). Numeric aliases
/// are accepted; every other value is ignored.
///
/// # Budget
///
/// One lowercase copy of the declaration value.
///
/// # Malformed Input
///
/// `None` for any value outside the two documented pairs.
fn css_font_weight(value: &str) -> Option<bool> {
    match value.trim().to_ascii_lowercase().as_str() {
        "normal" | "400" => Some(false),
        "bold" | "700" => Some(true),
        _ => None,
    }
}

/// The `font-style` subset: `normal` versus `italic`/`oblique`, both mapped to
/// Helvetica-Oblique. Every other value is ignored.
///
/// # Budget
///
/// One lowercase copy of the declaration value.
///
/// # Malformed Input
///
/// `None` for any value outside the three documented keywords.
fn css_font_style(value: &str) -> Option<bool> {
    match value.trim().to_ascii_lowercase().as_str() {
        "normal" => Some(false),
        "italic" | "oblique" => Some(true),
        _ => None,
    }
}

/// The `text-decoration` subset: `underline` and `line-through` lines, and
/// `none` to clear an inherited decoration. Colour/style tokens such as
/// `dotted` or `wavy` are not part of the subset and are ignored individually;
/// a value with no recognised line is dropped entirely.
///
/// # Budget
///
/// One lowercase copy and one token list bounded by the declaration text.
///
/// # Malformed Input
///
/// `None` for an empty value or one naming no supported line.
fn css_text_decoration(value: &str) -> Option<TextDecoration> {
    let lower = value.trim().to_ascii_lowercase();
    let tokens: Vec<&str> = lower.split_ascii_whitespace().collect();
    if tokens.is_empty() {
        return None;
    }
    if tokens.contains(&"none") {
        return Some(TextDecoration::default());
    }
    let underline = tokens.contains(&"underline");
    let line_through = tokens.contains(&"line-through");
    if !underline && !line_through {
        return None;
    }
    Some(TextDecoration {
        underline,
        line_through,
    })
}

/// Parse the `border` shorthand: a leading length plus optional `solid` and a
/// colour. Only solid borders are understood; dashed/dotted/double/… and
/// hidden borders are refused in full, never silently drawn as solid.
///
/// # Budget
///
/// One token list bounded by the declaration text.
///
/// # Malformed Input
///
/// `None` without a leading supported length, or when an unsupported border
/// style is named. An unrecognised colour token is ignored (black is used).
fn css_border(value: &str) -> Option<Border> {
    let tokens: Vec<&str> = value.split_ascii_whitespace().collect();
    let width = css_length(tokens.first().copied()?)?;
    if tokens.iter().any(|token| {
        matches!(
            *token,
            "dashed" | "dotted" | "double" | "groove" | "ridge" | "inset" | "outset" | "hidden"
        )
    }) {
        return None;
    }
    let mut color = None;
    for token in tokens.iter().skip(1) {
        if *token == "solid" {
            continue;
        }
        if let Some(parsed) = css_color(token) {
            color = Some(parsed);
        }
    }
    Some(Border {
        width,
        color: color.unwrap_or([0.0, 0.0, 0.0]),
    })
}

/// A narrow `color` parser: `#rgb` / `#rrggbb`, `rgb(r,g,b)` (0–255) and a
/// small fixed set of named colours. Anything else is ignored.
fn css_color(value: &str) -> Option<[f64; 3]> {
    let v = value.trim().to_ascii_lowercase();
    if let Some(hex) = v.strip_prefix('#') {
        return parse_hex_color(hex);
    }
    if let Some(inner) = v.strip_prefix("rgb(").and_then(|s| s.strip_suffix(')')) {
        let mut parts = inner.split(',');
        let r = parse_byte(parts.next()?)?;
        let g = parse_byte(parts.next()?)?;
        let b = parse_byte(parts.next()?)?;
        return Some([r, g, b]);
    }
    named_color(&v)
}

fn parse_byte(s: &str) -> Option<f64> {
    let value = s.trim().parse::<u8>().ok()?;
    Some(f64::from(value) / 255.0)
}

fn parse_hex_color(hex: &str) -> Option<[f64; 3]> {
    let expanded = if hex.len() == 3 {
        let chars: Vec<char> = hex.chars().collect();
        let a = chars.first()?;
        let b = chars.get(1)?;
        let c = chars.get(2)?;
        format!("{a}{a}{b}{b}{c}{c}")
    } else if hex.len() == 6 {
        hex.to_string()
    } else {
        return None;
    };
    if !expanded.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    let r = u8::from_str_radix(expanded.get(0..2)?, 16).ok()?;
    let g = u8::from_str_radix(expanded.get(2..4)?, 16).ok()?;
    let b = u8::from_str_radix(expanded.get(4..6)?, 16).ok()?;
    Some([
        f64::from(r) / 255.0,
        f64::from(g) / 255.0,
        f64::from(b) / 255.0,
    ])
}

fn named_color(name: &str) -> Option<[f64; 3]> {
    let (r, g, b) = match name {
        "black" => (0, 0, 0),
        "white" => (255, 255, 255),
        "red" => (255, 0, 0),
        "green" => (0, 128, 0),
        "blue" => (0, 0, 255),
        "navy" => (0, 0, 128),
        "gray" | "grey" => (128, 128, 128),
        "yellow" => (255, 255, 0),
        "purple" => (128, 0, 128),
        "orange" => (255, 165, 0),
        _ => return None,
    };
    Some([
        f64::from(r) / 255.0,
        f64::from(g) / 255.0,
        f64::from(b) / 255.0,
    ])
}

/// Case-insensitive prefix match at `pos`.
/// Read the page-break intent from a `style` attribute: `(before, after)`.
///
/// Honours both the classic `page-break-before`/`page-break-after` and the
/// modern `break-before`/`break-after`, with any forcing value (`always`,
/// `page`, `left`, `right`). `avoid`, `auto`, an absent property, or any other
/// value is no break. This is the one piece of CSS the converter interprets;
/// everything else in a `style` attribute is ignored, as the crate docs say.
fn css_page_breaks(style: Option<&str>) -> (bool, bool) {
    let Some(style) = style else {
        return (false, false);
    };
    let lower = style.to_ascii_lowercase();
    let forces = |property: &str| -> bool {
        lower.split(';').any(|declaration| {
            let mut parts = declaration.splitn(2, ':');
            let key = parts.next().unwrap_or("").trim();
            let value = parts.next().unwrap_or("").split_whitespace().next();
            key == property && matches!(value, Some("always" | "page" | "left" | "right"))
        })
    };
    let before = forces("page-break-before") || forces("break-before");
    let after = forces("page-break-after") || forces("break-after");
    (before, after)
}

fn starts_with_ci(chars: &[char], pos: usize, needle: &str) -> bool {
    let mut p = pos;
    for n in needle.chars() {
        let Some(&c) = chars.get(p) else {
            return false;
        };
        if !c.eq_ignore_ascii_case(&n) {
            return false;
        }
        p = p.saturating_add(1);
    }
    true
}

/// Find the first occurrence of `needle` (case-insensitive) at or after `from`.
fn find_ci(chars: &[char], needle: &str, from: usize) -> Option<usize> {
    if needle.is_empty() {
        return Some(from);
    }
    let mut p = from;
    while p < chars.len() {
        if starts_with_ci(chars, p, needle) {
            return Some(p);
        }
        p = p.saturating_add(1);
    }
    None
}

/// Find the first occurrence of `c` at or after `from`.
fn find_char(chars: &[char], c: char, from: usize) -> Option<usize> {
    let mut p = from;
    while p < chars.len() {
        if chars.get(p) == Some(&c) {
            return Some(p);
        }
        p = p.saturating_add(1);
    }
    None
}

#[cfg(test)]
mod tests {
    #![allow(clippy::indexing_slicing, clippy::arithmetic_side_effects)]

    use super::*;

    /// The 1-based index of the first page break in `blocks`, if any.
    fn first_break(blocks: &[Block]) -> Option<usize> {
        blocks.iter().position(|b| matches!(b, Block::PageBreak))
    }

    #[test]
    fn page_break_before_starts_a_new_page() {
        let blocks = parse("<p>one</p><div style=\"page-break-before:always\"><p>two</p></div>");
        let at = first_break(&blocks).expect("a page break");
        // The break sits between the first paragraph and the second.
        assert!(matches!(&blocks[at - 1], Block::Paragraph(_)), "{blocks:?}");
        assert!(matches!(&blocks[at + 1], Block::Paragraph(_)), "{blocks:?}");
    }

    #[test]
    fn page_break_after_lands_after_the_element() {
        let blocks = parse("<div style=\"page-break-after:always\"><p>one</p></div><p>two</p>");
        let at = first_break(&blocks).expect("a page break");
        // "one" is before the break, "two" after it.
        let Block::Paragraph(before) = &blocks[at - 1] else {
            panic!("expected a paragraph before the break: {blocks:?}");
        };
        assert!(before.iter().any(|s| s.text.contains("one")));
        let Block::Paragraph(after) = &blocks[at + 1] else {
            panic!("expected a paragraph after the break: {blocks:?}");
        };
        assert!(after.iter().any(|s| s.text.contains("two")));
    }

    #[test]
    fn the_modern_break_before_page_syntax_is_honoured() {
        let blocks = parse("<h2 style=\"break-before: page\">Next</h2>");
        assert_eq!(first_break(&blocks), Some(0), "break is before the heading");
    }

    #[test]
    fn a_non_forcing_break_value_does_nothing() {
        let blocks = parse("<div style=\"page-break-before:avoid\"><p>x</p></div>");
        assert!(
            first_break(&blocks).is_none(),
            "avoid is not a break: {blocks:?}"
        );
    }

    #[test]
    fn headings_paragraphs_and_styles() {
        let blocks = parse("<h1>Title</h1><p>Hello <b>bold</b> and <i>italic</i>.</p>");
        assert!(matches!(&blocks[0], Block::Heading { level: 1, .. }));
        let Block::Paragraph(spans) = &blocks[1] else {
            panic!("expected paragraph");
        };
        assert!(spans
            .iter()
            .any(|s| s.style == Style::Bold && s.text.contains("bold")));
        assert!(spans
            .iter()
            .any(|s| s.style == Style::Italic && s.text.contains("italic")));
    }

    #[test]
    fn lists_keep_order_and_nesting() {
        let blocks = parse("<ol><li>one</li><li>two<ul><li>inner</li></ul></li></ol>");
        let items: Vec<&Block> = blocks
            .iter()
            .filter(|b| matches!(b, Block::ListItem { .. }))
            .collect();
        assert_eq!(items.len(), 3);
        let Block::ListItem {
            ordered, ordinal, ..
        } = items[1]
        else {
            panic!("expected item");
        };
        assert!(*ordered);
        assert_eq!(*ordinal, 2);
        let Block::ListItem { ordered, depth, .. } = items[2] else {
            panic!("expected item");
        };
        assert!(!*ordered);
        assert_eq!(*depth, 1);
    }

    #[test]
    fn pre_blocks_keep_text_verbatim() {
        let blocks = parse("<pre><code>let x = 1;\nlet y = 2;</code></pre>");
        let Block::Code(lines) = &blocks[0] else {
            panic!("expected code block");
        };
        assert_eq!(lines, &["let x = 1;".to_string(), "let y = 2;".to_string()]);
    }

    #[test]
    fn entities_decode() {
        let blocks = parse("<p>AT&amp;T &mdash; caf&#233; &#x2014; done</p>");
        let Block::Paragraph(spans) = &blocks[0] else {
            panic!("expected paragraph");
        };
        let joined: String = spans.iter().map(|s| s.text.as_str()).collect();
        assert!(joined.contains("AT&T"));
        assert!(joined.contains("caf\u{00E9}"));
        assert!(joined.contains('\u{2014}'));
    }

    #[test]
    fn scripts_and_unknown_tags_are_dropped_or_unwrapped() {
        let blocks = parse("<script>var x = 1;</script><div><span>visible</span></div>");
        let Block::Paragraph(spans) = &blocks[0] else {
            panic!("expected paragraph");
        };
        let joined: String = spans.iter().map(|s| s.text.as_str()).collect();
        assert!(joined.contains("visible"));
        assert!(!joined.contains("var x"));
    }

    #[test]
    fn blockquotes_nest_blocks() {
        let blocks = parse("<blockquote><p>quoted</p></blockquote>");
        let Block::Quote(inner) = &blocks[0] else {
            panic!("expected blockquote");
        };
        assert!(matches!(&inner[0], Block::Paragraph(_)));
    }

    #[test]
    fn broken_markup_degrades_to_text() {
        let blocks = parse("<p>unclosed <b>bold runs on");
        assert_eq!(blocks.len(), 1);
        let Block::Paragraph(spans) = &blocks[0] else {
            panic!("expected paragraph");
        };
        let joined: String = spans.iter().map(|s| s.text.as_str()).collect();
        assert!(joined.contains("bold runs on"));
    }

    #[test]
    fn tables_parse_with_th_header() {
        let blocks = parse(
            "<table><tr><th>Name</th><th>Amount</th></tr><tr><td>Ada</td><td>1200</td></tr></table>",
        );
        assert_eq!(blocks.len(), 1);
        let Block::Table { head, rows } = &blocks[0] else {
            panic!("expected a table");
        };
        assert_eq!(
            head.iter()
                .map(|cell| cell.iter().map(|s| s.text.as_str()).collect::<String>())
                .collect::<Vec<_>>(),
            vec!["Name".to_string(), "Amount".to_string()]
        );
        assert_eq!(
            rows[0][1]
                .iter()
                .map(|s| s.text.as_str())
                .collect::<String>(),
            "1200"
        );
    }

    #[test]
    fn tables_without_th_get_no_head() {
        let blocks = parse("<table><tr><td>a</td></tr></table>");
        let Block::Table { head, rows } = &blocks[0] else {
            panic!("expected a table");
        };
        assert!(head.is_empty());
        assert_eq!(rows.len(), 1);
    }

    #[test]
    fn inline_markup_flows_into_cells() {
        let blocks = parse("<table><tr><td><b>bold</b> cell</td></tr></table>");
        let Block::Table { rows, .. } = &blocks[0] else {
            panic!("expected a table");
        };
        let cell = &rows[0][0];
        assert!(cell
            .iter()
            .any(|s| s.style == Style::Bold && s.text == "bold"));
    }

    #[test]
    fn inline_style_declarations_reach_the_spans() {
        let blocks = parse(
            "<p style=\"color:red;text-align:center\">before \
             <span style=\"color:#00ff00;font-size:20pt\">styled</span> after</p>",
        );
        let Block::Paragraph(spans) = &blocks[0] else {
            panic!("expected a paragraph");
        };
        let styled = spans
            .iter()
            .find(|span| span.text == "styled")
            .expect("the styled run");
        // The inner colour and size override the outer colour; alignment is
        // inherited from the paragraph.
        assert_eq!(styled.color, Some([0.0, 1.0, 0.0]));
        assert_eq!(styled.size, Some(Length::Pt(20.0)));
        assert_eq!(styled.align, Some(TextAlign::Center));
        let before = spans
            .iter()
            .find(|span| span.text.contains("before"))
            .expect("the plain run");
        assert_eq!(before.color, Some([1.0, 0.0, 0.0]));
        assert_eq!(before.size, None);
    }

    #[test]
    fn css_declarations_keep_semicolons_inside_urls() {
        let parts = split_declarations(
            "color:red; background-image:url(data:image/png;base64,AAA); font-size:9pt",
        );
        assert_eq!(parts.len(), 3);
        assert!(parts[1].contains("image/png;base64,AAA"));
        assert!(parts[2].contains("font-size"));
    }

    #[test]
    fn background_image_refuses_remote_urls_and_non_urls() {
        assert!(css_background_image("url(https://example.com/a.png)").is_none());
        assert!(css_background_image("linear-gradient(red, blue)").is_none());
        assert!(css_background_image("none").is_none());
        assert!(css_background_image("url(data:image/gif;base64,R0lGOD)").is_none());
        let blocks = parse("<p style=\"background-image:url(https://example.com/a.png)\">x</p>");
        let Block::Paragraph(spans) = &blocks[0] else {
            panic!("expected a paragraph");
        };
        assert!(spans.iter().all(|span| span.background.is_none()));
    }

    #[test]
    fn link_href_is_carried_on_the_link_text() {
        let blocks = parse("<p>go <a href=\"https://selis.example/a\">there</a> now</p>");
        let Block::Paragraph(spans) = &blocks[0] else {
            panic!("expected a paragraph");
        };
        let linked = spans
            .iter()
            .find(|span| span.text == "there")
            .expect("the link text");
        assert_eq!(linked.href.as_deref(), Some("https://selis.example/a"));
        assert!(spans
            .iter()
            .filter(|span| span.href.is_some())
            .all(|span| span.text == "there"));
    }

    #[test]
    fn align_attribute_sets_cell_alignment() {
        let blocks = parse("<table><tr><td align=\"right\">value</td></tr></table>");
        let Block::Table { rows, .. } = &blocks[0] else {
            panic!("expected a table");
        };
        assert_eq!(rows[0][0][0].align, Some(TextAlign::Right));
    }

    #[test]
    fn css_helpers_ignore_unsupported_values() {
        assert_eq!(css_color("not-a-colour"), None);
        assert_eq!(css_color("#ggg"), None);
        assert_eq!(css_font_size("12pt"), Some(Length::Pt(12.0)));
        assert_eq!(css_font_size("12px"), Some(Length::Pt(9.0)));
        assert_eq!(css_font_size("2em"), Some(Length::Em(2.0)));
        assert_eq!(css_font_size("80%"), Some(Length::Percent(0.8)));
        assert_eq!(css_font_size("100vh"), None);
        assert_eq!(css_font_size("small"), None);
        assert_eq!(css_align("justify"), None);
    }

    #[test]
    fn relative_lengths_resolve_against_font_root_and_column() {
        assert_eq!(Length::Em(2.0).resolve(11.0, 11.0, 500.0), 22.0);
        assert_eq!(Length::Rem(2.0).resolve(11.0, 11.0, 500.0), 22.0);
        assert_eq!(Length::Percent(0.5).resolve(11.0, 11.0, 500.0), 250.0);
        assert_eq!(Length::Em(2.0).resolve_font(20.0, 11.0), 40.0);
        assert_eq!(Length::Percent(0.5).resolve_font(20.0, 11.0), 10.0);
        assert_eq!(css_length("1.5rem"), Some(Length::Rem(1.5)));
        assert_eq!(css_length("10vh"), None);
        assert_eq!(css_length("20vw"), None);
        assert_eq!(css_length("-2em"), None);
    }

    #[test]
    fn new_declarations_are_parsed_without_guessing() {
        assert_eq!(css_font_weight("bold"), Some(true));
        assert_eq!(css_font_weight("normal"), Some(false));
        assert_eq!(css_font_weight("bolder"), None);
        assert_eq!(css_font_style("italic"), Some(true));
        assert_eq!(css_font_style("oblique"), Some(true));
        assert_eq!(css_font_style("normal"), Some(false));
        assert_eq!(css_line_height("1.5"), Some(LineHeight::Factor(1.5)));
        assert_eq!(
            css_line_height("12pt"),
            Some(LineHeight::Length(Length::Pt(12.0)))
        );
        assert_eq!(css_line_height("normal"), None);
        assert_eq!(css_line_height("50vh"), None);
        assert_eq!(
            css_text_decoration("underline line-through"),
            Some(TextDecoration {
                underline: true,
                line_through: true,
            })
        );
        assert_eq!(css_text_decoration("none"), Some(TextDecoration::default()));
        assert_eq!(css_text_decoration("dotted"), None);
        assert_eq!(
            css_border("2px solid #ff0000"),
            Some(Border {
                width: Length::Pt(1.5),
                color: [1.0, 0.0, 0.0],
            })
        );
        assert_eq!(css_border("1px dashed red"), None);
        assert_eq!(
            css_box_edges("1pt 2pt"),
            Some(BoxEdges {
                top: Length::Pt(1.0),
                right: Length::Pt(2.0),
                bottom: Length::Pt(1.0),
                left: Length::Pt(2.0),
            })
        );
        assert_eq!(
            css_box_edges("1em 2em 3em 4em").map(|e| e.left),
            Some(Length::Em(4.0))
        );
        assert_eq!(css_box_edges("1em 2em 3em 4em 5em"), None);
    }

    #[test]
    fn background_size_parses_only_the_documented_subset() {
        assert_eq!(css_background_size("auto"), Some(BackgroundSize::Auto));
        assert_eq!(
            css_background_size("CONTAIN"),
            Some(BackgroundSize::Contain)
        );
        assert_eq!(css_background_size("cover"), Some(BackgroundSize::Cover));
        assert_eq!(
            css_background_size("10pt 5px"),
            Some(BackgroundSize::Length {
                width: Some(10.0),
                height: Some(3.75),
            })
        );
        assert_eq!(
            css_background_size("12px"),
            Some(BackgroundSize::Length {
                width: Some(9.0),
                height: None,
            })
        );
        assert_eq!(css_background_size("50%"), None);
        assert_eq!(css_background_size("repeat"), None);
        assert_eq!(css_background_size("auto auto auto"), None);
    }

    #[test]
    fn rulesheet_keeps_simple_selectors_and_ignores_the_rest() {
        let rules = parse_rulesheet(
            "<style>/* c */ p{color:red;font-size:14pt} h2{color:blue}\
             li{text-align:right} td{background-size:cover}\
             .c{color:green}#i{color:green}p.x{color:green}\
             div p{color:#ffff00}*>*:hover{[color:red]}\
             @media print{p{color:#ff00ff}}unknown{color:#ff0000}\
             p{bogus:1}</style><style>p{color:purple}</style>",
        );
        // The second <style>, descendant/universal/pseudo/attribute selectors,
        // `@media print`, unknown elements and unknown declarations are gone.
        assert_eq!(rules.rules.len(), 9, "rules: {rules:?}");
        let plain = rules.props_for("p", None, &[]);
        assert_eq!(plain.color, Some([1.0, 0.0, 0.0]));
        assert_eq!(plain.size, Some(Length::Pt(14.0)));
        assert_eq!(
            rules.props_for("h2", None, &[]).color,
            Some([0.0, 0.0, 1.0])
        );
        assert_eq!(
            rules.props_for("li", None, &[]).align,
            Some(TextAlign::Right)
        );
        assert_eq!(
            rules.props_for("td", None, &[]).background_size,
            Some(BackgroundSize::Cover)
        );
        assert_eq!(
            rules.props_for("div", None, &["c".to_string()]).color,
            Some([0.0, 128.0 / 255.0, 0.0])
        );
        assert_eq!(
            rules.props_for("p", Some("i"), &[]).color,
            Some([0.0, 128.0 / 255.0, 0.0])
        );
        assert_eq!(
            rules.props_for("p", None, &["x".to_string()]).color,
            Some([0.0, 128.0 / 255.0, 0.0])
        );
    }

    #[test]
    fn specificity_orders_id_above_class_above_type_then_source_order() {
        let rules = parse_rulesheet(
            "<style>\
             p{color:#ff0000}\
             .hit{color:#00ff00}\
             #top{color:#0000ff}\
             .late{color:#ffff00}\
             .early{color:#00ffff}\
             </style>",
        );
        // Two equal-specificity class rules: `.early` comes later, so it wins.
        let both = rules.props_for("p", None, &["early".into(), "late".into()]);
        assert_eq!(both.color, Some([0.0, 1.0, 1.0]));
        // A class outranks the type rule even though it came first.
        let class = rules.props_for("p", None, &["hit".into()]);
        assert_eq!(class.color, Some([0.0, 1.0, 0.0]));
        // An id outranks both, regardless of source position.
        let id = rules.props_for("p", Some("top"), &["hit".into()]);
        assert_eq!(id.color, Some([0.0, 0.0, 1.0]));
    }

    #[test]
    fn media_width_rules_are_included_but_other_media_are_not() {
        let rules = parse_rulesheet(
            "<style>@media (max-width: 600px){p{color:#ff0000}}\
             @media screen and (min-width: 400px){p{font-size:16pt}}\
             @media print{p{color:#00ff00}}\
             @supports (color:red){p{color:#0000ff}}</style>",
        );
        let props = rules.props_for("p", None, &[]);
        assert_eq!(props.color, None, "neither print nor @supports applies");
        assert_eq!(props.size, Some(Length::Pt(16.0)));
    }

    #[test]
    fn class_and_id_attributes_reach_the_spans() {
        let blocks = parse(
            "<style>.mark{color:#ff0000}#lead{font-weight:bold}p.mark{text-decoration:underline}\
             </style><p class=\"mark\" id=\"lead\">Hi</p>",
        );
        let Block::Paragraph(spans) = &blocks[0] else {
            panic!("expected paragraph");
        };
        assert_eq!(spans[0].color, Some([1.0, 0.0, 0.0]));
        assert!(spans[0].decoration.underline);
        assert_eq!(spans[0].style, Style::Bold);
        assert_eq!(spans[0].weight, Some(true));
    }

    #[test]
    fn inline_style_overrides_the_rulesheet_property_by_property() {
        let blocks = parse(
            "<style>p{color:red;font-size:14pt;text-align:center}</style>\
             <p style=\"color:blue\">one</p><p>two</p>",
        );
        let Block::Paragraph(one) = &blocks[0] else {
            panic!("first paragraph");
        };
        let Block::Paragraph(two) = &blocks[1] else {
            panic!("second paragraph");
        };
        // Inline colour wins; size and alignment fall through to the sheet.
        assert_eq!(one[0].color, Some([0.0, 0.0, 1.0]));
        assert_eq!(one[0].size, Some(Length::Pt(14.0)));
        assert_eq!(one[0].align, Some(TextAlign::Center));
        // The unstyled paragraph takes the sheet's colour unchanged.
        assert_eq!(two[0].color, Some([1.0, 0.0, 0.0]));
        assert_eq!(two[0].size, Some(Length::Pt(14.0)));
    }
}
