//! The document model shared by the Markdown/HTML parsers and the layout
//! engine, plus the WinAnsi text encoder and the standard-14 font metric
//! measurer.

use std::collections::BTreeMap;

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
}

/// A run of styled text inside a block.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Span {
    pub style: Style,
    pub text: String,
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
    CodeBlock(Vec<String>),
    /// A blockquote wrapping further blocks.
    Blockquote(Vec<Block>),
    /// A horizontal rule.
    Rule,
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
