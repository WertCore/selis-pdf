//! The document model shared by the Markdown/HTML parsers and the layout
//! engine, plus the WinAnsi text encoder and the standard-14 font metric
//! measurer.

use std::collections::BTreeMap;

use crate::image::{EmbeddedImage, ImageSource};

/// The inline font style of a text span.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum Style {
    /// Helvetica.
    #[default]
    Regular,
    /// Helvetica-Bold.
    Bold,
    /// Helvetica-Oblique.
    Italic,
    /// Helvetica-BoldOblique.
    BoldItalic,
    /// Courier (preformatted/code text).
    Code,
}

impl Style {
    /// The standard-14 font this style renders with.
    pub(crate) const fn font(self) -> &'static str {
        match self {
            Style::Regular => "Helvetica",
            Style::Bold => "Helvetica-Bold",
            Style::Italic => "Helvetica-Oblique",
            Style::BoldItalic => "Helvetica-BoldOblique",
            Style::Code => "Courier",
        }
    }

    /// The style with bold added.
    pub(crate) const fn with_bold(self) -> Style {
        match self {
            Style::Regular | Style::Bold => Style::Bold,
            Style::Italic | Style::BoldItalic => Style::BoldItalic,
            Style::Code => Style::Code,
        }
    }

    /// The style with italic added.
    pub(crate) const fn with_italic(self) -> Style {
        match self {
            Style::Regular | Style::Italic => Style::Italic,
            Style::Bold | Style::BoldItalic => Style::BoldItalic,
            Style::Code => Style::Code,
        }
    }

    /// The style with bold removed (`font-weight: normal`). Code is untouched:
    /// a CSS weight does not turn a preformatted run into proportional text.
    ///
    /// # Budget
    ///
    /// A constant enum match; no allocation.
    ///
    /// # Malformed Input
    ///
    /// Infallible: unknown variants are returned unchanged.
    pub(crate) const fn without_bold(self) -> Style {
        match self {
            Style::Bold => Style::Regular,
            Style::BoldItalic => Style::Italic,
            other => other,
        }
    }

    /// The style with italic removed (`font-style: normal`). Code is untouched.
    ///
    /// # Budget
    ///
    /// A constant enum match; no allocation.
    ///
    /// # Malformed Input
    ///
    /// Infallible: unknown variants are returned unchanged.
    pub(crate) const fn without_italic(self) -> Style {
        match self {
            Style::Italic => Style::Regular,
            Style::BoldItalic => Style::Bold,
            other => other,
        }
    }
}

/// A CSS length kept unresolved until layout knows the font size and column.
///
/// Only `pt`, `px` (converted at 96 dpi by the parser), `em`, `rem` and `%` are
/// recognised; `vh`/`vw` and every other unit are refused by name. A percentage
/// is stored as a fraction (`50%` becomes `Percent(0.5)`). `em` and font-size
/// percentages resolve against the computed font size, `rem` against the 11 pt
/// root size, and box percentages against the text column width.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum Length {
    /// Absolute points (the parser folds `px` and bare numbers into this).
    Pt(f64),
    /// Multiples of the element's computed font size.
    Em(f64),
    /// Multiples of the root font size.
    Rem(f64),
    /// Fraction of the reference dimension (font size or text column).
    Percent(f64),
}

impl Default for Length {
    fn default() -> Self {
        Self::Pt(0.0)
    }
}

impl Length {
    /// Resolve a box length against a computed font size, root size and column.
    ///
    /// # Budget
    ///
    /// Constant work, no allocation.
    ///
    /// # Malformed Input
    ///
    /// Non-finite inputs produce a non-finite result; callers clamp it before
    /// using it for geometry.
    pub(crate) fn resolve(self, font_size: f64, root_size: f64, column: f64) -> f64 {
        match self {
            Self::Pt(v) => v,
            Self::Em(v) => v * font_size,
            Self::Rem(v) => v * root_size,
            Self::Percent(v) => v * column,
        }
    }

    /// Resolve a font-relative length against the inherited font size. Used
    /// for `font-size` itself, where a percentage is relative to the parent's
    /// size rather than to the text column.
    ///
    /// # Budget
    ///
    /// Constant work, no allocation.
    ///
    /// # Malformed Input
    ///
    /// Non-finite inputs produce a non-finite result; `eff_size` clamps it.
    pub(crate) fn resolve_font(self, parent_size: f64, root_size: f64) -> f64 {
        match self {
            Self::Pt(v) => v,
            Self::Em(v) | Self::Percent(v) => v * parent_size,
            Self::Rem(v) => v * root_size,
        }
    }
}

/// The `line-height` subset: a unitless multiplier or an explicit length.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum LineHeight {
    /// A unitless factor of the computed font size (`1.5`).
    Factor(f64),
    /// A length (`1.5em`, `18px`, `120%`) resolved against the font size.
    Length(Length),
}

/// The four sides of a `margin` or `padding` shorthand, unresolved.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub(crate) struct BoxEdges {
    /// Top edge.
    pub top: Length,
    /// Right edge.
    pub right: Length,
    /// Bottom edge.
    pub bottom: Length,
    /// Left edge.
    pub left: Length,
}

/// A solid border on a block box: width and colour. Other border styles
/// (dashed, dotted, double, …) are not part of this subset.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Border {
    /// Border thickness, resolved against the computed font size at layout.
    pub width: Length,
    /// Fill colour of the four drawn strips (black when only a width is given).
    pub color: [f64; 3],
}

/// `text-decoration` lines to draw under or across a run of text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct TextDecoration {
    /// Draw a rule just below the baseline.
    pub underline: bool,
    /// Draw a rule through the middle of the glyphs.
    pub line_through: bool,
}

impl TextDecoration {
    /// Whether no decoration rule needs drawing.
    ///
    /// # Budget
    ///
    /// Constant work, no allocation.
    ///
    /// # Malformed Input
    ///
    /// Infallible; only the two boolean flags are inspected.
    pub(crate) fn is_empty(self) -> bool {
        !self.underline && !self.line_through
    }
}

/// Block-level CSS carried on every span a block produces, so layout can read
/// the box properties from the first span exactly as it reads alignment.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub(crate) struct BlockCss {
    /// Outer spacing around the box.
    pub margin: Option<BoxEdges>,
    /// Inner spacing between the border and the text.
    pub padding: Option<BoxEdges>,
    /// Solid border strips, drawn behind the text.
    pub border: Option<Border>,
    /// Fill colour of the block's padding box.
    pub background_color: Option<[f64; 3]>,
}

/// Paragraph / table-cell text alignment carried on the spans of that block.
///
/// Alignment is a property of the containing block, but the block model keeps
/// spans as its only styled unit; the value is therefore stored on every span
/// a block produces and the layout reads it from the first span. Only
/// `left`/`center`/`right` are recognised — any other CSS value is ignored.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum TextAlign {
    /// Flush left (the default).
    #[default]
    Left,
    /// Centre within the available width.
    Center,
    /// Flush right.
    Right,
}

/// How a `background-image` is sized inside its element box.
///
/// This is the narrow CSS `background-size` subset: `auto`, `contain`, `cover`
/// and one or two absolute lengths. `None` on a span means the legacy behaviour
/// (stretch the image to the run's text box) is kept.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum BackgroundSize {
    /// Intrinsic image size.
    Auto,
    /// Scale down to fit entirely inside the box, preserving aspect ratio.
    Contain,
    /// Scale up to cover the box, preserving aspect ratio.
    Cover,
    /// Explicit absolute lengths in points. A missing axis follows the image's
    /// intrinsic aspect ratio from the other one.
    Length {
        width: Option<f64>,
        height: Option<f64>,
    },
}

/// A run of styled text inside a block.
///
/// Besides the base [`Style`], a span may carry a small set of inline
/// properties recovered from HTML `style=` attributes / Markdown link syntax.
/// Everything is optional: a converter that does not understand a property
/// leaves it at `None` and the layout falls back to its defaults. Unknown CSS
/// is never guessed at.
#[derive(Debug, Clone, PartialEq, Default)]
pub(crate) struct Span {
    pub style: Style,
    pub text: String,
    /// Link target (`<a href>` / `[text](url)`). Drives a real URI annotation.
    pub href: Option<String>,
    /// Explicit fill colour as RGB in [0, 1] (inline `color:`).
    pub color: Option<[f64; 3]>,
    /// Explicit font size (inline / rulesheet `font-size:`), resolved in layout.
    pub size: Option<Length>,
    /// Bold override from `font-weight` (`true` = bold, `false` = normal).
    pub weight: Option<bool>,
    /// Italic override from `font-style` (`true` = italic/oblique).
    pub italic: Option<bool>,
    /// `line-height`, as a factor or a length resolved against the font size.
    pub line_height: Option<LineHeight>,
    /// `text-decoration` rules to draw for this run.
    pub decoration: TextDecoration,
    /// Text alignment of the containing paragraph/cell, if specified.
    pub align: Option<TextAlign>,
    /// An inline `background-image: url(data:…)` decoded image, painted behind
    /// this run's text box. Only JPEG/PNG data URIs are accepted by the parser.
    pub background: Option<EmbeddedImage>,
    /// `background-size` for that image; `None` keeps the legacy stretch-to-box
    /// behaviour.
    pub background_size: Option<BackgroundSize>,
    /// Block box fill / border / spacing, read from the first span of a block.
    pub block_css: BlockCss,
}

/// An embedded JPEG/PNG/SVG placed as its own block (HTML `<img>` or a
/// standalone Markdown `![alt](data:image/…)` paragraph).
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ImageBlock {
    /// The validated raster or vector source and its intrinsic geometry.
    pub image: ImageSource,
    /// Explicit placement width in points (HTML `width`), if given. A bare
    /// number is points; `px` is converted at 96 dpi, matching `font-size`.
    pub width: Option<f64>,
    /// Explicit placement height in points (HTML `height`), if given.
    pub height: Option<f64>,
}

/// A layout-level document block.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Block {
    /// An ATX heading, level 1–6.
    Heading { level: u8, spans: Vec<Span> },
    /// A flow paragraph.
    Paragraph(Vec<Span>),
    /// A list item; `ordinal` is its 1-based position for ordered lists.
    ListItem {
        ordered: bool,
        ordinal: usize,
        depth: usize,
        spans: Vec<Span>,
    },
    /// Preformatted code lines (rendered verbatim in Courier).
    // `Code` and `Quote`, not `CodeBlock` and `Blockquote` (SL-0.WS.11):
    // inside `enum Block` the longer names stutter — `Block::CodeBlock` — and
    // clippy's `enum_variant_names` is right about it.
    Code(Vec<String>),
    /// A blockquote wrapping further blocks.
    Quote(Vec<Block>),
    /// A horizontal rule.
    Rule,
    /// An embedded JPEG/PNG/SVG image (data URI only), placed at its measured
    /// box. SVG sources become a `Form` XObject, rasters an `Image` XObject.
    Image(ImageBlock),
    /// Force the block that follows onto a new page. Only the OOXML slide
    /// and sheet readers emit this: a deck's slide and a workbook's sheet are
    /// page boundaries a flow layout would otherwise blur.
    PageBreak,
    /// A table: the header row (empty when the source had none) and the body
    /// rows. Every row has the same number of cells; the parsers pad short
    /// rows with empty cells rather than fail.
    Table {
        /// The header row's cells (may be empty).
        head: Vec<Vec<Span>>,
        /// The body rows; each cell is a span list.
        rows: Vec<Vec<Vec<Span>>>,
    },
}

/// Explicit byte assignments for characters the AGL lookup may miss
/// (char, WinAnsi byte).
const EXTRA_BYTES: &[(char, u8)] = &[
    ('\u{0020}', 0x20), // space
    ('\u{00A0}', 0xA0), // nbsp
    ('\u{20AC}', 0x80), // Euro
    ('\u{201A}', 0x82), // quotesinglbase
    ('\u{0192}', 0x83), // florin
    ('\u{201E}', 0x84), // quotedblbase
    ('\u{2026}', 0x85), // ellipsis
    ('\u{2020}', 0x86), // dagger
    ('\u{2021}', 0x87), // daggerdbl
    ('\u{02C6}', 0x88), // circumflex
    ('\u{2030}', 0x89), // perthousand
    ('\u{0160}', 0x8A), // Scaron
    ('\u{2039}', 0x8B), // guilsinglleft
    ('\u{0152}', 0x8C), // OE
    ('\u{017D}', 0x8E), // Zcaron
    ('\u{2018}', 0x91), // quoteleft
    ('\u{2019}', 0x92), // quoteright
    ('\u{201C}', 0x93), // quotedblleft
    ('\u{201D}', 0x94), // quotedblright
    ('\u{2022}', 0x95), // bullet
    ('\u{2013}', 0x96), // endash
    ('\u{2014}', 0x97), // emdash
    ('\u{02DC}', 0x98), // tilde
    ('\u{2122}', 0x99), // trademark
    ('\u{0161}', 0x9A), // scaron
    ('\u{203A}', 0x9B), // guilsinglright
    ('\u{0153}', 0x9C), // oe
    ('\u{017E}', 0x9E), // zcaron
    ('\u{0178}', 0x9F), // Ydieresis
];

/// The WinAnsi char → byte map, built once from the encoding table + AGL.
fn winansi_map() -> &'static BTreeMap<char, u8> {
    static MAP: std::sync::OnceLock<BTreeMap<char, u8>> = std::sync::OnceLock::new();
    MAP.get_or_init(|| {
        let mut map: BTreeMap<char, u8> = BTreeMap::new();
        for (byte, name) in selis_font::tables::WinAnsiEncoding.iter().enumerate() {
            if *name == ".notdef" || byte < 0x20 {
                continue;
            }
            if let Some(u) = selis_font::glyph_to_unicode(name) {
                if let Some(c) = char::from_u32(u) {
                    let byte8 = u8::try_from(byte).unwrap_or(0);
                    map.entry(c).or_insert(byte8);
                }
            }
        }
        for &(c, byte) in EXTRA_BYTES {
            map.entry(c).or_insert(byte);
        }
        map
    })
}

/// Encode one character as a WinAnsi byte, or `None` when unrepresentable.
pub(crate) fn encode_char(c: char) -> Option<u8> {
    winansi_map().get(&c).copied()
}

/// Encode text to WinAnsi bytes, replacing unrepresentable characters with
/// `?`.
pub(crate) fn encode_text(text: &str) -> Vec<u8> {
    text.chars()
        .map(|c| encode_char(c).unwrap_or(b'?'))
        .collect()
}

/// The advance of one character at `size` points in a standard-14 font.
///
/// Unmappable characters are measured as `?`; missing AFM entries fall back
/// to 500/1000 em.
pub(crate) fn char_advance(font: &str, c: char, size: f64) -> f64 {
    let width = encode_char(c)
        .map(usize::from)
        .and_then(|byte| selis_font::tables::WinAnsiEncoding.get(byte))
        .and_then(|name| selis_font::standard14::width(font, name))
        .map(f64::from)
        .unwrap_or(500.0);
    width / 1000.0 * size
}

/// The advance of a text run at `size` points in a standard-14 font.
pub(crate) fn measure(font: &str, text: &str, size: f64) -> f64 {
    text.chars().map(|c| char_advance(font, c, size)).sum()
}

/// Split `text` into words and separators for wrapping. Each item is
/// `(word, has_trailing_space)`.
pub(crate) fn split_words(text: &str) -> Vec<(String, bool)> {
    let mut out: Vec<(String, bool)> = Vec::new();
    let mut word = String::new();
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c.is_whitespace() {
            if !word.is_empty() {
                // Peek past any run of whitespace: the trailing-space flag is
                // true unless the whitespace ends the text.
                let mut space = String::new();
                while let Some(&w) = chars.peek() {
                    if w.is_whitespace() {
                        space.push(w);
                        chars.next();
                    } else {
                        break;
                    }
                }
                let trailing = chars.peek().is_some();
                out.push((std::mem::take(&mut word), trailing));
            }
        } else {
            word.push(c);
        }
    }
    if !word.is_empty() {
        out.push((word, false));
    }
    out
}
