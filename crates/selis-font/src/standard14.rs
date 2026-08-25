//! Standard-14 font metrics (SL-3.FONT.08).
//!
//! The exact AFM glyph widths (in 1000/em glyph space) for the 14 PDF
//! standard fonts, transcribed from pdf.js `src/core/metrics.js`
//! (Apache-2.0). Each entry is sorted by glyph name for binary search.
//!
//! Courier variants have a uniform width of 600; the others carry their
//! full AFM tables.

/// A single glyph-width entry.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GlyphWidth {
    /// The PostScript/PDF glyph name.
    pub name: &'static str,
    /// The advance width in 1000/em glyph space.
    pub width: u16,
}

/// A standard-14 font's metrics.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FontMetrics {
    /// The font name (e.g. `"Helvetica"`, `"Times-Roman"`).
    pub name: &'static str,
    /// A uniform advance for every glyph (Courier variants), or `None`.
    pub uniform: Option<u16>,
    /// The glyph widths, sorted by name (empty for uniform fonts).
    pub glyphs: &'static [GlyphWidth],
}

/// `Courier` — a monospace font; every glyph is 600 wide.
pub const Courier: &[GlyphWidth] = &[GlyphWidth {
    name: "space",
    width: 600,
}];

/// `Courier-Bold` — a monospace font; every glyph is 600 wide.
pub const Courier_Bold: &[GlyphWidth] = &[GlyphWidth {
    name: "space",
    width: 600,
}];

/// `Courier-BoldOblique` — a monospace font; every glyph is 600 wide.
pub const Courier_BoldOblique: &[GlyphWidth] = &[GlyphWidth {
    name: "space",
    width: 600,
}];

/// `Courier-Oblique` — a monospace font; every glyph is 600 wide.
pub const Courier_Oblique: &[GlyphWidth] = &[GlyphWidth {
    name: "space",
    width: 600,
}];

/// `Helvetica` — 315 glyphs.
pub const Helvetica: &[GlyphWidth] = &[
    GlyphWidth {
        name: "A",
        width: 667,
    },
    GlyphWidth {
        name: "AE",
        width: 1000,
    },
    GlyphWidth {
        name: "Aacute",
        width: 667,
    },
    GlyphWidth {
        name: "Abreve",
        width: 667,
    },
    GlyphWidth {
        name: "Acircumflex",
        width: 667,
    },
    GlyphWidth {
        name: "Adieresis",
        width: 667,
    },
    GlyphWidth {
        name: "Agrave",
        width: 667,
    },
    GlyphWidth {
        name: "Amacron",
        width: 667,
    },
    GlyphWidth {
        name: "Aogonek",
        width: 667,
    },
    GlyphWidth {
        name: "Aring",
        width: 667,
    },
    GlyphWidth {
        name: "Atilde",
        width: 667,
    },
    GlyphWidth {
        name: "B",
        width: 667,
    },
    GlyphWidth {
        name: "C",
        width: 722,
    },
    GlyphWidth {
        name: "Cacute",
        width: 722,
    },
    GlyphWidth {
        name: "Ccaron",
        width: 722,
    },
    GlyphWidth {
        name: "Ccedilla",
        width: 722,
    },
    GlyphWidth {
        name: "D",
        width: 722,
    },
    GlyphWidth {
        name: "Dcaron",
        width: 722,
    },
    GlyphWidth {
        name: "Dcroat",
        width: 722,
    },
    GlyphWidth {
        name: "Delta",
        width: 612,
    },
    GlyphWidth {
        name: "E",
        width: 667,
    },
    GlyphWidth {
        name: "Eacute",
        width: 667,
    },
    GlyphWidth {
        name: "Ecaron",
        width: 667,
    },
    GlyphWidth {
        name: "Ecircumflex",
        width: 667,
    },
    GlyphWidth {
        name: "Edieresis",
        width: 667,
    },
    GlyphWidth {
        name: "Edotaccent",
        width: 667,
    },
    GlyphWidth {
        name: "Egrave",
        width: 667,
    },
    GlyphWidth {
        name: "Emacron",
        width: 667,
    },
    GlyphWidth {
        name: "Eogonek",
        width: 667,
    },
    GlyphWidth {
        name: "Eth",
        width: 722,
    },
    GlyphWidth {
        name: "Euro",
        width: 556,
    },
    GlyphWidth {
        name: "F",
        width: 611,
    },
    GlyphWidth {
        name: "G",
        width: 778,
    },
    GlyphWidth {
        name: "Gbreve",
        width: 778,
    },
    GlyphWidth {
        name: "Gcommaaccent",
        width: 778,
    },
    GlyphWidth {
        name: "H",
        width: 722,
    },
    GlyphWidth {
        name: "I",
        width: 278,
    },
    GlyphWidth {
        name: "Iacute",
        width: 278,
    },
    GlyphWidth {
        name: "Icircumflex",
        width: 278,
    },
    GlyphWidth {
        name: "Idieresis",
        width: 278,
    },
    GlyphWidth {
        name: "Idotaccent",
        width: 278,
    },
    GlyphWidth {
        name: "Igrave",
        width: 278,
    },
    GlyphWidth {
        name: "Imacron",
        width: 278,
    },
    GlyphWidth {
        name: "Iogonek",
        width: 278,
    },
    GlyphWidth {
        name: "J",
        width: 500,
    },
    GlyphWidth {
        name: "K",
        width: 667,
    },
    GlyphWidth {
        name: "Kcommaaccent",
        width: 667,
    },
    GlyphWidth {
        name: "L",
        width: 556,
    },
    GlyphWidth {
        name: "Lacute",
        width: 556,
    },
    GlyphWidth {
        name: "Lcaron",
        width: 556,
    },
    GlyphWidth {
        name: "Lcommaaccent",
        width: 556,
    },
    GlyphWidth {
        name: "Lslash",
        width: 556,
    },
    GlyphWidth {
        name: "M",
        width: 833,
    },
    GlyphWidth {
        name: "N",
        width: 722,
    },
    GlyphWidth {
        name: "Nacute",
        width: 722,
    },
    GlyphWidth {
        name: "Ncaron",
        width: 722,
    },
    GlyphWidth {
        name: "Ncommaaccent",
        width: 722,
    },
    GlyphWidth {
        name: "Ntilde",
        width: 722,
    },
    GlyphWidth {
        name: "O",
        width: 778,
    },
    GlyphWidth {
        name: "OE",
        width: 1000,
    },
    GlyphWidth {
        name: "Oacute",
        width: 778,
    },
    GlyphWidth {
        name: "Ocircumflex",
        width: 778,
    },
    GlyphWidth {
        name: "Odieresis",
        width: 778,
    },
    GlyphWidth {
        name: "Ograve",
        width: 778,
    },
    GlyphWidth {
        name: "Ohungarumlaut",
        width: 778,
    },
    GlyphWidth {
        name: "Omacron",
        width: 778,
    },
    GlyphWidth {
        name: "Oslash",
        width: 778,
    },
    GlyphWidth {
        name: "Otilde",
        width: 778,
    },
    GlyphWidth {
        name: "P",
        width: 667,
    },
    GlyphWidth {
        name: "Q",
        width: 778,
    },
    GlyphWidth {
        name: "R",
        width: 722,
    },
    GlyphWidth {
        name: "Racute",
        width: 722,
    },
    GlyphWidth {
        name: "Rcaron",
        width: 722,
    },
    GlyphWidth {
        name: "Rcommaaccent",
        width: 722,
    },
    GlyphWidth {
        name: "S",
        width: 667,
    },
    GlyphWidth {
        name: "Sacute",
        width: 667,
    },
    GlyphWidth {
        name: "Scaron",
        width: 667,
    },
    GlyphWidth {
        name: "Scedilla",
        width: 667,
    },
    GlyphWidth {
        name: "Scommaaccent",
        width: 667,
    },
    GlyphWidth {
        name: "T",
        width: 611,
    },
    GlyphWidth {
        name: "Tcaron",
        width: 611,
    },
    GlyphWidth {
        name: "Tcommaaccent",
        width: 611,
    },
    GlyphWidth {
        name: "Thorn",
        width: 667,
    },
    GlyphWidth {
        name: "U",
        width: 722,
    },
    GlyphWidth {
        name: "Uacute",
        width: 722,
    },
    GlyphWidth {
        name: "Ucircumflex",
        width: 722,
    },
    GlyphWidth {
        name: "Udieresis",
        width: 722,
    },
    GlyphWidth {
        name: "Ugrave",
        width: 722,
    },
    GlyphWidth {
        name: "Uhungarumlaut",
        width: 722,
    },
    GlyphWidth {
        name: "Umacron",
        width: 722,
    },
    GlyphWidth {
        name: "Uogonek",
        width: 722,
    },
    GlyphWidth {
        name: "Uring",
        width: 722,
    },
    GlyphWidth {
        name: "V",
        width: 667,
    },
    GlyphWidth {
        name: "W",
        width: 944,
    },
    GlyphWidth {
        name: "X",
        width: 667,
    },
    GlyphWidth {
        name: "Y",
        width: 667,
    },
    GlyphWidth {
        name: "Yacute",
        width: 667,
    },
    GlyphWidth {
        name: "Ydieresis",
        width: 667,
    },
    GlyphWidth {
        name: "Z",
        width: 611,
    },
    GlyphWidth {
        name: "Zacute",
        width: 611,
    },
    GlyphWidth {
        name: "Zcaron",
        width: 611,
    },
    GlyphWidth {
        name: "Zdotaccent",
        width: 611,
    },
    GlyphWidth {
        name: "a",
        width: 556,
    },
    GlyphWidth {
        name: "aacute",
        width: 556,
    },
    GlyphWidth {
        name: "abreve",
        width: 556,
    },
    GlyphWidth {
        name: "acircumflex",
        width: 556,
    },
    GlyphWidth {
        name: "acute",
        width: 333,
    },
    GlyphWidth {
        name: "adieresis",
        width: 556,
    },
    GlyphWidth {
        name: "ae",
        width: 889,
    },
    GlyphWidth {
        name: "agrave",
        width: 556,
    },
    GlyphWidth {
        name: "amacron",
        width: 556,
    },
    GlyphWidth {
        name: "ampersand",
        width: 667,
    },
    GlyphWidth {
        name: "aogonek",
        width: 556,
    },
    GlyphWidth {
        name: "aring",
        width: 556,
    },
    GlyphWidth {
        name: "asciicircum",
        width: 469,
    },
    GlyphWidth {
        name: "asciitilde",
        width: 584,
    },
    GlyphWidth {
        name: "asterisk",
        width: 389,
    },
    GlyphWidth {
        name: "at",
        width: 1015,
    },
    GlyphWidth {
        name: "atilde",
        width: 556,
    },
    GlyphWidth {
        name: "b",
        width: 556,
    },
    GlyphWidth {
        name: "backslash",
        width: 278,
    },
    GlyphWidth {
        name: "bar",
        width: 260,
    },
    GlyphWidth {
        name: "braceleft",
        width: 334,
    },
    GlyphWidth {
        name: "braceright",
        width: 334,
    },
    GlyphWidth {
        name: "bracketleft",
        width: 278,
    },
    GlyphWidth {
        name: "bracketright",
        width: 278,
    },
    GlyphWidth {
        name: "breve",
        width: 333,
    },
    GlyphWidth {
        name: "brokenbar",
        width: 260,
    },
    GlyphWidth {
        name: "bullet",
        width: 350,
    },
    GlyphWidth {
        name: "c",
        width: 500,
    },
    GlyphWidth {
        name: "cacute",
        width: 500,
    },
    GlyphWidth {
        name: "caron",
        width: 333,
    },
    GlyphWidth {
        name: "ccaron",
        width: 500,
    },
    GlyphWidth {
        name: "ccedilla",
        width: 500,
    },
    GlyphWidth {
        name: "cedilla",
        width: 333,
    },
    GlyphWidth {
        name: "cent",
        width: 556,
    },
    GlyphWidth {
        name: "circumflex",
        width: 333,
    },
    GlyphWidth {
        name: "colon",
        width: 278,
    },
    GlyphWidth {
        name: "comma",
        width: 278,
    },
    GlyphWidth {
        name: "commaaccent",
        width: 250,
    },
    GlyphWidth {
        name: "copyright",
        width: 737,
    },
    GlyphWidth {
        name: "currency",
        width: 556,
    },
    GlyphWidth {
        name: "d",
        width: 556,
    },
    GlyphWidth {
        name: "dagger",
        width: 556,
    },
    GlyphWidth {
        name: "daggerdbl",
        width: 556,
    },
    GlyphWidth {
        name: "dcaron",
        width: 643,
    },
    GlyphWidth {
        name: "dcroat",
        width: 556,
    },
    GlyphWidth {
        name: "degree",
        width: 400,
    },
    GlyphWidth {
        name: "dieresis",
        width: 333,
    },
    GlyphWidth {
        name: "divide",
        width: 584,
    },
    GlyphWidth {
        name: "dollar",
        width: 556,
    },
    GlyphWidth {
        name: "dotaccent",
        width: 333,
    },
    GlyphWidth {
        name: "dotlessi",
        width: 278,
    },
    GlyphWidth {
        name: "e",
        width: 556,
    },
    GlyphWidth {
        name: "eacute",
        width: 556,
    },
    GlyphWidth {
        name: "ecaron",
        width: 556,
    },
    GlyphWidth {
        name: "ecircumflex",
        width: 556,
    },
    GlyphWidth {
        name: "edieresis",
        width: 556,
    },
    GlyphWidth {
        name: "edotaccent",
        width: 556,
    },
    GlyphWidth {
        name: "egrave",
        width: 556,
    },
    GlyphWidth {
        name: "eight",
        width: 556,
    },
    GlyphWidth {
        name: "ellipsis",
        width: 1000,
    },
    GlyphWidth {
        name: "emacron",
        width: 556,
    },
    GlyphWidth {
        name: "emdash",
        width: 1000,
    },
    GlyphWidth {
        name: "endash",
        width: 556,
    },
    GlyphWidth {
        name: "eogonek",
        width: 556,
    },
    GlyphWidth {
        name: "equal",
        width: 584,
    },
    GlyphWidth {
        name: "eth",
        width: 556,
    },
    GlyphWidth {
        name: "exclam",
        width: 278,
    },
    GlyphWidth {
        name: "exclamdown",
        width: 333,
    },
    GlyphWidth {
        name: "f",
        width: 278,
    },
    GlyphWidth {
        name: "fi",
        width: 500,
    },
    GlyphWidth {
        name: "five",
        width: 556,
    },
    GlyphWidth {
        name: "fl",
        width: 500,
    },
    GlyphWidth {
        name: "florin",
        width: 556,
    },
    GlyphWidth {
        name: "four",
        width: 556,
    },
    GlyphWidth {
        name: "fraction",
        width: 167,
    },
    GlyphWidth {
        name: "g",
        width: 556,
    },
    GlyphWidth {
        name: "gbreve",
        width: 556,
    },
    GlyphWidth {
        name: "gcommaaccent",
        width: 556,
    },
    GlyphWidth {
        name: "germandbls",
        width: 611,
    },
    GlyphWidth {
        name: "grave",
        width: 333,
    },
    GlyphWidth {
        name: "greater",
        width: 584,
    },
    GlyphWidth {
        name: "greaterequal",
        width: 549,
    },
    GlyphWidth {
        name: "guillemotleft",
        width: 556,
    },
    GlyphWidth {
        name: "guillemotright",
        width: 556,
    },
    GlyphWidth {
        name: "guilsinglleft",
        width: 333,
    },
    GlyphWidth {
        name: "guilsinglright",
        width: 333,
    },
    GlyphWidth {
        name: "h",
        width: 556,
    },
    GlyphWidth {
        name: "hungarumlaut",
        width: 333,
    },
    GlyphWidth {
        name: "hyphen",
        width: 333,
    },
    GlyphWidth {
        name: "i",
        width: 222,
    },
    GlyphWidth {
        name: "iacute",
        width: 278,
    },
    GlyphWidth {
        name: "icircumflex",
        width: 278,
    },
    GlyphWidth {
        name: "idieresis",
        width: 278,
    },
    GlyphWidth {
        name: "igrave",
        width: 278,
    },
    GlyphWidth {
        name: "imacron",
        width: 278,
    },
    GlyphWidth {
        name: "iogonek",
        width: 222,
    },
    GlyphWidth {
        name: "j",
        width: 222,
    },
    GlyphWidth {
        name: "k",
        width: 500,
    },
    GlyphWidth {
        name: "kcommaaccent",
        width: 500,
    },
    GlyphWidth {
        name: "l",
        width: 222,
    },
    GlyphWidth {
        name: "lacute",
        width: 222,
    },
    GlyphWidth {
        name: "lcaron",
        width: 299,
    },
    GlyphWidth {
        name: "lcommaaccent",
        width: 222,
    },
    GlyphWidth {
        name: "less",
        width: 584,
    },
    GlyphWidth {
        name: "lessequal",
        width: 549,
    },
    GlyphWidth {
        name: "logicalnot",
        width: 584,
    },
    GlyphWidth {
        name: "lozenge",
        width: 471,
    },
    GlyphWidth {
        name: "lslash",
        width: 222,
    },
    GlyphWidth {
        name: "m",
        width: 833,
    },
    GlyphWidth {
        name: "macron",
        width: 333,
    },
    GlyphWidth {
        name: "minus",
        width: 584,
    },
    GlyphWidth {
        name: "mu",
        width: 556,
    },
    GlyphWidth {
        name: "multiply",
        width: 584,
    },
    GlyphWidth {
        name: "n",
        width: 556,
    },
    GlyphWidth {
        name: "nacute",
        width: 556,
    },
    GlyphWidth {
        name: "ncaron",
        width: 556,
    },
    GlyphWidth {
        name: "ncommaaccent",
        width: 556,
    },
    GlyphWidth {
        name: "nine",
        width: 556,
    },
    GlyphWidth {
        name: "notequal",
        width: 549,
    },
    GlyphWidth {
        name: "ntilde",
        width: 556,
    },
    GlyphWidth {
        name: "numbersign",
        width: 556,
    },
    GlyphWidth {
        name: "o",
        width: 556,
    },
    GlyphWidth {
        name: "oacute",
        width: 556,
    },
    GlyphWidth {
        name: "ocircumflex",
        width: 556,
    },
    GlyphWidth {
        name: "odieresis",
        width: 556,
    },
    GlyphWidth {
        name: "oe",
        width: 944,
    },
    GlyphWidth {
        name: "ogonek",
        width: 333,
    },
    GlyphWidth {
        name: "ograve",
        width: 556,
    },
    GlyphWidth {
        name: "ohungarumlaut",
        width: 556,
    },
    GlyphWidth {
        name: "omacron",
        width: 556,
    },
    GlyphWidth {
        name: "one",
        width: 556,
    },
    GlyphWidth {
        name: "onehalf",
        width: 834,
    },
    GlyphWidth {
        name: "onequarter",
        width: 834,
    },
    GlyphWidth {
        name: "onesuperior",
        width: 333,
    },
    GlyphWidth {
        name: "ordfeminine",
        width: 370,
    },
    GlyphWidth {
        name: "ordmasculine",
        width: 365,
    },
    GlyphWidth {
        name: "oslash",
        width: 611,
    },
    GlyphWidth {
        name: "otilde",
        width: 556,
    },
    GlyphWidth {
        name: "p",
        width: 556,
    },
    GlyphWidth {
        name: "paragraph",
        width: 537,
    },
    GlyphWidth {
        name: "parenleft",
        width: 333,
    },
    GlyphWidth {
        name: "parenright",
        width: 333,
    },
    GlyphWidth {
        name: "partialdiff",
        width: 476,
    },
    GlyphWidth {
        name: "percent",
        width: 889,
    },
    GlyphWidth {
        name: "period",
        width: 278,
    },
    GlyphWidth {
        name: "periodcentered",
        width: 278,
    },
    GlyphWidth {
        name: "perthousand",
        width: 1000,
    },
    GlyphWidth {
        name: "plus",
        width: 584,
    },
    GlyphWidth {
        name: "plusminus",
        width: 584,
    },
    GlyphWidth {
        name: "q",
        width: 556,
    },
    GlyphWidth {
        name: "question",
        width: 556,
    },
    GlyphWidth {
        name: "questiondown",
        width: 611,
    },
    GlyphWidth {
        name: "quotedbl",
        width: 355,
    },
    GlyphWidth {
        name: "quotedblbase",
        width: 333,
    },
    GlyphWidth {
        name: "quotedblleft",
        width: 333,
    },
    GlyphWidth {
        name: "quotedblright",
        width: 333,
    },
    GlyphWidth {
        name: "quoteleft",
        width: 222,
    },
    GlyphWidth {
        name: "quoteright",
        width: 222,
    },
    GlyphWidth {
        name: "quotesinglbase",
        width: 222,
    },
    GlyphWidth {
        name: "quotesingle",
        width: 191,
    },
    GlyphWidth {
        name: "r",
        width: 333,
    },
    GlyphWidth {
        name: "racute",
        width: 333,
    },
    GlyphWidth {
        name: "radical",
        width: 453,
    },
    GlyphWidth {
        name: "rcaron",
        width: 333,
    },
    GlyphWidth {
        name: "rcommaaccent",
        width: 333,
    },
    GlyphWidth {
        name: "registered",
        width: 737,
    },
    GlyphWidth {
        name: "ring",
        width: 333,
    },
    GlyphWidth {
        name: "s",
        width: 500,
    },
    GlyphWidth {
        name: "sacute",
        width: 500,
    },
    GlyphWidth {
        name: "scaron",
        width: 500,
    },
    GlyphWidth {
        name: "scedilla",
        width: 500,
    },
    GlyphWidth {
        name: "scommaaccent",
        width: 500,
    },
    GlyphWidth {
        name: "section",
        width: 556,
    },
    GlyphWidth {
        name: "semicolon",
        width: 278,
    },
    GlyphWidth {
        name: "seven",
        width: 556,
    },
    GlyphWidth {
        name: "six",
        width: 556,
    },
    GlyphWidth {
        name: "slash",
        width: 278,
    },
    GlyphWidth {
        name: "space",
        width: 278,
    },
    GlyphWidth {
        name: "sterling",
        width: 556,
    },
    GlyphWidth {
        name: "summation",
        width: 600,
    },
    GlyphWidth {
        name: "t",
        width: 278,
    },
    GlyphWidth {
        name: "tcaron",
        width: 317,
    },
    GlyphWidth {
        name: "tcommaaccent",
        width: 278,
    },
    GlyphWidth {
        name: "thorn",
        width: 556,
    },
    GlyphWidth {
        name: "three",
        width: 556,
    },
    GlyphWidth {
        name: "threequarters",
        width: 834,
    },
    GlyphWidth {
        name: "threesuperior",
        width: 333,
    },
    GlyphWidth {
        name: "tilde",
        width: 333,
    },
    GlyphWidth {
        name: "trademark",
        width: 1000,
    },
    GlyphWidth {
        name: "two",
        width: 556,
    },
    GlyphWidth {
        name: "twosuperior",
        width: 333,
    },
    GlyphWidth {
        name: "u",
        width: 556,
    },
    GlyphWidth {
        name: "uacute",
        width: 556,
    },
    GlyphWidth {
        name: "ucircumflex",
        width: 556,
    },
    GlyphWidth {
        name: "udieresis",
        width: 556,
    },
    GlyphWidth {
        name: "ugrave",
        width: 556,
    },
    GlyphWidth {
        name: "uhungarumlaut",
        width: 556,
    },
    GlyphWidth {
        name: "umacron",
        width: 556,
    },
    GlyphWidth {
        name: "underscore",
        width: 556,
    },
    GlyphWidth {
        name: "uogonek",
        width: 556,
    },
    GlyphWidth {
        name: "uring",
        width: 556,
    },
    GlyphWidth {
        name: "v",
        width: 500,
    },
    GlyphWidth {
        name: "w",
        width: 722,
    },
    GlyphWidth {
        name: "x",
        width: 500,
    },
    GlyphWidth {
        name: "y",
        width: 500,
    },
    GlyphWidth {
        name: "yacute",
        width: 500,
    },
    GlyphWidth {
        name: "ydieresis",
        width: 500,
    },
    GlyphWidth {
        name: "yen",
        width: 556,
    },
    GlyphWidth {
        name: "z",
        width: 500,
    },
    GlyphWidth {
        name: "zacute",
        width: 500,
    },
    GlyphWidth {
        name: "zcaron",
        width: 500,
    },
    GlyphWidth {
        name: "zdotaccent",
        width: 500,
    },
    GlyphWidth {
        name: "zero",
        width: 556,
    },
];

/// `Helvetica-Bold` — 315 glyphs.
pub const Helvetica_Bold: &[GlyphWidth] = &[
    GlyphWidth {
        name: "A",
        width: 722,
    },
    GlyphWidth {
        name: "AE",
        width: 1000,
    },
    GlyphWidth {
        name: "Aacute",
        width: 722,
    },
    GlyphWidth {
        name: "Abreve",
        width: 722,
    },
    GlyphWidth {
        name: "Acircumflex",
        width: 722,
    },
    GlyphWidth {
        name: "Adieresis",
        width: 722,
    },
    GlyphWidth {
        name: "Agrave",
        width: 722,
    },
    GlyphWidth {
        name: "Amacron",
        width: 722,
    },
    GlyphWidth {
        name: "Aogonek",
        width: 722,
    },
    GlyphWidth {
        name: "Aring",
        width: 722,
    },
    GlyphWidth {
        name: "Atilde",
        width: 722,
    },
    GlyphWidth {
        name: "B",
        width: 722,
    },
    GlyphWidth {
        name: "C",
        width: 722,
    },
    GlyphWidth {
        name: "Cacute",
        width: 722,
    },
    GlyphWidth {
        name: "Ccaron",
        width: 722,
    },
    GlyphWidth {
        name: "Ccedilla",
        width: 722,
    },
    GlyphWidth {
        name: "D",
        width: 722,
    },
    GlyphWidth {
        name: "Dcaron",
        width: 722,
    },
    GlyphWidth {
        name: "Dcroat",
        width: 722,
    },
    GlyphWidth {
        name: "Delta",
        width: 612,
    },
    GlyphWidth {
        name: "E",
        width: 667,
    },
    GlyphWidth {
        name: "Eacute",
        width: 667,
    },
    GlyphWidth {
        name: "Ecaron",
        width: 667,
    },
    GlyphWidth {
        name: "Ecircumflex",
        width: 667,
    },
    GlyphWidth {
        name: "Edieresis",
        width: 667,
    },
    GlyphWidth {
        name: "Edotaccent",
        width: 667,
    },
    GlyphWidth {
        name: "Egrave",
        width: 667,
    },
    GlyphWidth {
        name: "Emacron",
        width: 667,
    },
    GlyphWidth {
        name: "Eogonek",
        width: 667,
    },
    GlyphWidth {
        name: "Eth",
        width: 722,
    },
    GlyphWidth {
        name: "Euro",
        width: 556,
    },
    GlyphWidth {
        name: "F",
        width: 611,
    },
    GlyphWidth {
        name: "G",
        width: 778,
    },
    GlyphWidth {
        name: "Gbreve",
        width: 778,
    },
    GlyphWidth {
        name: "Gcommaaccent",
        width: 778,
    },
    GlyphWidth {
        name: "H",
        width: 722,
    },
    GlyphWidth {
        name: "I",
        width: 278,
    },
    GlyphWidth {
        name: "Iacute",
        width: 278,
    },
    GlyphWidth {
        name: "Icircumflex",
        width: 278,
    },
    GlyphWidth {
        name: "Idieresis",
        width: 278,
    },
    GlyphWidth {
        name: "Idotaccent",
        width: 278,
    },
    GlyphWidth {
        name: "Igrave",
        width: 278,
    },
    GlyphWidth {
        name: "Imacron",
        width: 278,
    },
    GlyphWidth {
        name: "Iogonek",
        width: 278,
    },
    GlyphWidth {
        name: "J",
        width: 556,
    },
    GlyphWidth {
        name: "K",
        width: 722,
    },
    GlyphWidth {
        name: "Kcommaaccent",
        width: 722,
    },
    GlyphWidth {
        name: "L",
        width: 611,
    },
    GlyphWidth {
        name: "Lacute",
        width: 611,
    },
    GlyphWidth {
        name: "Lcaron",
        width: 611,
    },
    GlyphWidth {
        name: "Lcommaaccent",
        width: 611,
    },
    GlyphWidth {
        name: "Lslash",
        width: 611,
    },
    GlyphWidth {
        name: "M",
        width: 833,
    },
    GlyphWidth {
        name: "N",
        width: 722,
    },
    GlyphWidth {
        name: "Nacute",
        width: 722,
    },
    GlyphWidth {
        name: "Ncaron",
        width: 722,
    },
    GlyphWidth {
        name: "Ncommaaccent",
        width: 722,
    },
    GlyphWidth {
        name: "Ntilde",
        width: 722,
    },
    GlyphWidth {
        name: "O",
        width: 778,
    },
    GlyphWidth {
        name: "OE",
        width: 1000,
    },
    GlyphWidth {
        name: "Oacute",
        width: 778,
    },
    GlyphWidth {
        name: "Ocircumflex",
        width: 778,
    },
    GlyphWidth {
        name: "Odieresis",
        width: 778,
    },
    GlyphWidth {
        name: "Ograve",
        width: 778,
    },
    GlyphWidth {
        name: "Ohungarumlaut",
        width: 778,
    },
    GlyphWidth {
        name: "Omacron",
        width: 778,
    },
    GlyphWidth {
        name: "Oslash",
        width: 778,
    },
    GlyphWidth {
        name: "Otilde",
        width: 778,
    },
    GlyphWidth {
        name: "P",
        width: 667,
    },
    GlyphWidth {
        name: "Q",
        width: 778,
    },
    GlyphWidth {
        name: "R",
        width: 722,
    },
    GlyphWidth {
        name: "Racute",
        width: 722,
    },
    GlyphWidth {
        name: "Rcaron",
        width: 722,
    },
    GlyphWidth {
        name: "Rcommaaccent",
        width: 722,
    },
    GlyphWidth {
        name: "S",
        width: 667,
    },
    GlyphWidth {
        name: "Sacute",
        width: 667,
    },
    GlyphWidth {
        name: "Scaron",
        width: 667,
    },
    GlyphWidth {
        name: "Scedilla",
        width: 667,
    },
    GlyphWidth {
        name: "Scommaaccent",
        width: 667,
    },
    GlyphWidth {
        name: "T",
        width: 611,
    },
    GlyphWidth {
        name: "Tcaron",
        width: 611,
    },
    GlyphWidth {
        name: "Tcommaaccent",
        width: 611,
    },
    GlyphWidth {
        name: "Thorn",
        width: 667,
    },
    GlyphWidth {
        name: "U",
        width: 722,
    },
    GlyphWidth {
        name: "Uacute",
        width: 722,
    },
    GlyphWidth {
        name: "Ucircumflex",
        width: 722,
    },
    GlyphWidth {
        name: "Udieresis",
        width: 722,
    },
    GlyphWidth {
        name: "Ugrave",
        width: 722,
    },
    GlyphWidth {
        name: "Uhungarumlaut",
        width: 722,
    },
    GlyphWidth {
        name: "Umacron",
        width: 722,
    },
    GlyphWidth {
        name: "Uogonek",
        width: 722,
    },
    GlyphWidth {
        name: "Uring",
        width: 722,
    },
    GlyphWidth {
        name: "V",
        width: 667,
    },
    GlyphWidth {
        name: "W",
        width: 944,
    },
    GlyphWidth {
        name: "X",
        width: 667,
    },
    GlyphWidth {
        name: "Y",
        width: 667,
    },
    GlyphWidth {
        name: "Yacute",
        width: 667,
    },
    GlyphWidth {
        name: "Ydieresis",
        width: 667,
    },
    GlyphWidth {
        name: "Z",
        width: 611,
    },
    GlyphWidth {
        name: "Zacute",
        width: 611,
    },
    GlyphWidth {
        name: "Zcaron",
        width: 611,
    },
    GlyphWidth {
        name: "Zdotaccent",
        width: 611,
    },
    GlyphWidth {
        name: "a",
        width: 556,
    },
    GlyphWidth {
        name: "aacute",
        width: 556,
    },
    GlyphWidth {
        name: "abreve",
        width: 556,
    },
    GlyphWidth {
        name: "acircumflex",
        width: 556,
    },
    GlyphWidth {
        name: "acute",
        width: 333,
    },
    GlyphWidth {
        name: "adieresis",
        width: 556,
    },
    GlyphWidth {
        name: "ae",
        width: 889,
    },
    GlyphWidth {
        name: "agrave",
        width: 556,
    },
    GlyphWidth {
        name: "amacron",
        width: 556,
    },
    GlyphWidth {
        name: "ampersand",
        width: 722,
    },
    GlyphWidth {
        name: "aogonek",
        width: 556,
    },
    GlyphWidth {
        name: "aring",
        width: 556,
    },
    GlyphWidth {
        name: "asciicircum",
        width: 584,
    },
    GlyphWidth {
        name: "asciitilde",
        width: 584,
    },
    GlyphWidth {
        name: "asterisk",
        width: 389,
    },
    GlyphWidth {
        name: "at",
        width: 975,
    },
    GlyphWidth {
        name: "atilde",
        width: 556,
    },
    GlyphWidth {
        name: "b",
        width: 611,
    },
    GlyphWidth {
        name: "backslash",
        width: 278,
    },
    GlyphWidth {
        name: "bar",
        width: 280,
    },
    GlyphWidth {
        name: "braceleft",
        width: 389,
    },
    GlyphWidth {
        name: "braceright",
        width: 389,
    },
    GlyphWidth {
        name: "bracketleft",
        width: 333,
    },
    GlyphWidth {
        name: "bracketright",
        width: 333,
    },
    GlyphWidth {
        name: "breve",
        width: 333,
    },
    GlyphWidth {
        name: "brokenbar",
        width: 280,
    },
    GlyphWidth {
        name: "bullet",
        width: 350,
    },
    GlyphWidth {
        name: "c",
        width: 556,
    },
    GlyphWidth {
        name: "cacute",
        width: 556,
    },
    GlyphWidth {
        name: "caron",
        width: 333,
    },
    GlyphWidth {
        name: "ccaron",
        width: 556,
    },
    GlyphWidth {
        name: "ccedilla",
        width: 556,
    },
    GlyphWidth {
        name: "cedilla",
        width: 333,
    },
    GlyphWidth {
        name: "cent",
        width: 556,
    },
    GlyphWidth {
        name: "circumflex",
        width: 333,
    },
    GlyphWidth {
        name: "colon",
        width: 333,
    },
    GlyphWidth {
        name: "comma",
        width: 278,
    },
    GlyphWidth {
        name: "commaaccent",
        width: 250,
    },
    GlyphWidth {
        name: "copyright",
        width: 737,
    },
    GlyphWidth {
        name: "currency",
        width: 556,
    },
    GlyphWidth {
        name: "d",
        width: 611,
    },
    GlyphWidth {
        name: "dagger",
        width: 556,
    },
    GlyphWidth {
        name: "daggerdbl",
        width: 556,
    },
    GlyphWidth {
        name: "dcaron",
        width: 743,
    },
    GlyphWidth {
        name: "dcroat",
        width: 611,
    },
    GlyphWidth {
        name: "degree",
        width: 400,
    },
    GlyphWidth {
        name: "dieresis",
        width: 333,
    },
    GlyphWidth {
        name: "divide",
        width: 584,
    },
    GlyphWidth {
        name: "dollar",
        width: 556,
    },
    GlyphWidth {
        name: "dotaccent",
        width: 333,
    },
    GlyphWidth {
        name: "dotlessi",
        width: 278,
    },
    GlyphWidth {
        name: "e",
        width: 556,
    },
    GlyphWidth {
        name: "eacute",
        width: 556,
    },
    GlyphWidth {
        name: "ecaron",
        width: 556,
    },
    GlyphWidth {
        name: "ecircumflex",
        width: 556,
    },
    GlyphWidth {
        name: "edieresis",
        width: 556,
    },
    GlyphWidth {
        name: "edotaccent",
        width: 556,
    },
    GlyphWidth {
        name: "egrave",
        width: 556,
    },
    GlyphWidth {
        name: "eight",
        width: 556,
    },
    GlyphWidth {
        name: "ellipsis",
        width: 1000,
    },
    GlyphWidth {
        name: "emacron",
        width: 556,
    },
    GlyphWidth {
        name: "emdash",
        width: 1000,
    },
    GlyphWidth {
        name: "endash",
        width: 556,
    },
    GlyphWidth {
        name: "eogonek",
        width: 556,
    },
    GlyphWidth {
        name: "equal",
        width: 584,
    },
    GlyphWidth {
        name: "eth",
        width: 611,
    },
    GlyphWidth {
        name: "exclam",
        width: 333,
    },
    GlyphWidth {
        name: "exclamdown",
        width: 333,
    },
    GlyphWidth {
        name: "f",
        width: 333,
    },
    GlyphWidth {
        name: "fi",
        width: 611,
    },
    GlyphWidth {
        name: "five",
        width: 556,
    },
    GlyphWidth {
        name: "fl",
        width: 611,
    },
    GlyphWidth {
        name: "florin",
        width: 556,
    },
    GlyphWidth {
        name: "four",
        width: 556,
    },
    GlyphWidth {
        name: "fraction",
        width: 167,
    },
    GlyphWidth {
        name: "g",
        width: 611,
    },
    GlyphWidth {
        name: "gbreve",
        width: 611,
    },
    GlyphWidth {
        name: "gcommaaccent",
        width: 611,
    },
    GlyphWidth {
        name: "germandbls",
        width: 611,
    },
    GlyphWidth {
        name: "grave",
        width: 333,
    },
    GlyphWidth {
        name: "greater",
        width: 584,
    },
    GlyphWidth {
        name: "greaterequal",
        width: 549,
    },
    GlyphWidth {
        name: "guillemotleft",
        width: 556,
    },
    GlyphWidth {
        name: "guillemotright",
        width: 556,
    },
    GlyphWidth {
        name: "guilsinglleft",
        width: 333,
    },
    GlyphWidth {
        name: "guilsinglright",
        width: 333,
    },
    GlyphWidth {
        name: "h",
        width: 611,
    },
    GlyphWidth {
        name: "hungarumlaut",
        width: 333,
    },
    GlyphWidth {
        name: "hyphen",
        width: 333,
    },
    GlyphWidth {
        name: "i",
        width: 278,
    },
    GlyphWidth {
        name: "iacute",
        width: 278,
    },
    GlyphWidth {
        name: "icircumflex",
        width: 278,
    },
    GlyphWidth {
        name: "idieresis",
        width: 278,
    },
    GlyphWidth {
        name: "igrave",
        width: 278,
    },
    GlyphWidth {
        name: "imacron",
        width: 278,
    },
    GlyphWidth {
        name: "iogonek",
        width: 278,
    },
    GlyphWidth {
        name: "j",
        width: 278,
    },
    GlyphWidth {
        name: "k",
        width: 556,
    },
    GlyphWidth {
        name: "kcommaaccent",
        width: 556,
    },
    GlyphWidth {
        name: "l",
        width: 278,
    },
    GlyphWidth {
        name: "lacute",
        width: 278,
    },
    GlyphWidth {
        name: "lcaron",
        width: 400,
    },
    GlyphWidth {
        name: "lcommaaccent",
        width: 278,
    },
    GlyphWidth {
        name: "less",
        width: 584,
    },
    GlyphWidth {
        name: "lessequal",
        width: 549,
    },
    GlyphWidth {
        name: "logicalnot",
        width: 584,
    },
    GlyphWidth {
        name: "lozenge",
        width: 494,
    },
    GlyphWidth {
        name: "lslash",
        width: 278,
    },
    GlyphWidth {
        name: "m",
        width: 889,
    },
    GlyphWidth {
        name: "macron",
        width: 333,
    },
    GlyphWidth {
        name: "minus",
        width: 584,
    },
    GlyphWidth {
        name: "mu",
        width: 611,
    },
    GlyphWidth {
        name: "multiply",
        width: 584,
    },
    GlyphWidth {
        name: "n",
        width: 611,
    },
    GlyphWidth {
        name: "nacute",
        width: 611,
    },
    GlyphWidth {
        name: "ncaron",
        width: 611,
    },
    GlyphWidth {
        name: "ncommaaccent",
        width: 611,
    },
    GlyphWidth {
        name: "nine",
        width: 556,
    },
    GlyphWidth {
        name: "notequal",
        width: 549,
    },
    GlyphWidth {
        name: "ntilde",
        width: 611,
    },
    GlyphWidth {
        name: "numbersign",
        width: 556,
    },
    GlyphWidth {
        name: "o",
        width: 611,
    },
    GlyphWidth {
        name: "oacute",
        width: 611,
    },
    GlyphWidth {
        name: "ocircumflex",
        width: 611,
    },
    GlyphWidth {
        name: "odieresis",
        width: 611,
    },
    GlyphWidth {
        name: "oe",
        width: 944,
    },
    GlyphWidth {
        name: "ogonek",
        width: 333,
    },
    GlyphWidth {
        name: "ograve",
        width: 611,
    },
    GlyphWidth {
        name: "ohungarumlaut",
        width: 611,
    },
    GlyphWidth {
        name: "omacron",
        width: 611,
    },
    GlyphWidth {
        name: "one",
        width: 556,
    },
    GlyphWidth {
        name: "onehalf",
        width: 834,
    },
    GlyphWidth {
        name: "onequarter",
        width: 834,
    },
    GlyphWidth {
        name: "onesuperior",
        width: 333,
    },
    GlyphWidth {
        name: "ordfeminine",
        width: 370,
    },
    GlyphWidth {
        name: "ordmasculine",
        width: 365,
    },
    GlyphWidth {
        name: "oslash",
        width: 611,
    },
    GlyphWidth {
        name: "otilde",
        width: 611,
    },
    GlyphWidth {
        name: "p",
        width: 611,
    },
    GlyphWidth {
        name: "paragraph",
        width: 556,
    },
    GlyphWidth {
        name: "parenleft",
        width: 333,
    },
    GlyphWidth {
        name: "parenright",
        width: 333,
    },
    GlyphWidth {
        name: "partialdiff",
        width: 494,
    },
    GlyphWidth {
        name: "percent",
        width: 889,
    },
    GlyphWidth {
        name: "period",
        width: 278,
    },
    GlyphWidth {
        name: "periodcentered",
        width: 278,
    },
    GlyphWidth {
        name: "perthousand",
        width: 1000,
    },
    GlyphWidth {
        name: "plus",
        width: 584,
    },
    GlyphWidth {
        name: "plusminus",
        width: 584,
    },
    GlyphWidth {
        name: "q",
        width: 611,
    },
    GlyphWidth {
        name: "question",
        width: 611,
    },
    GlyphWidth {
        name: "questiondown",
        width: 611,
    },
    GlyphWidth {
        name: "quotedbl",
        width: 474,
    },
    GlyphWidth {
        name: "quotedblbase",
        width: 500,
    },
    GlyphWidth {
        name: "quotedblleft",
        width: 500,
    },
    GlyphWidth {
        name: "quotedblright",
        width: 500,
    },
    GlyphWidth {
        name: "quoteleft",
        width: 278,
    },
    GlyphWidth {
        name: "quoteright",
        width: 278,
    },
    GlyphWidth {
        name: "quotesinglbase",
        width: 278,
    },
    GlyphWidth {
        name: "quotesingle",
        width: 238,
    },
    GlyphWidth {
        name: "r",
        width: 389,
    },
    GlyphWidth {
        name: "racute",
        width: 389,
    },
    GlyphWidth {
        name: "radical",
        width: 549,
    },
    GlyphWidth {
        name: "rcaron",
        width: 389,
    },
    GlyphWidth {
        name: "rcommaaccent",
        width: 389,
    },
    GlyphWidth {
        name: "registered",
        width: 737,
    },
    GlyphWidth {
        name: "ring",
        width: 333,
    },
    GlyphWidth {
        name: "s",
        width: 556,
    },
    GlyphWidth {
        name: "sacute",
        width: 556,
    },
    GlyphWidth {
        name: "scaron",
        width: 556,
    },
    GlyphWidth {
        name: "scedilla",
        width: 556,
    },
    GlyphWidth {
        name: "scommaaccent",
        width: 556,
    },
    GlyphWidth {
        name: "section",
        width: 556,
    },
    GlyphWidth {
        name: "semicolon",
        width: 333,
    },
    GlyphWidth {
        name: "seven",
        width: 556,
    },
    GlyphWidth {
        name: "six",
        width: 556,
    },
    GlyphWidth {
        name: "slash",
        width: 278,
    },
    GlyphWidth {
        name: "space",
        width: 278,
    },
    GlyphWidth {
        name: "sterling",
        width: 556,
    },
    GlyphWidth {
        name: "summation",
        width: 600,
    },
    GlyphWidth {
        name: "t",
        width: 333,
    },
    GlyphWidth {
        name: "tcaron",
        width: 389,
    },
    GlyphWidth {
        name: "tcommaaccent",
        width: 333,
    },
    GlyphWidth {
        name: "thorn",
        width: 611,
    },
    GlyphWidth {
        name: "three",
        width: 556,
    },
    GlyphWidth {
        name: "threequarters",
        width: 834,
    },
    GlyphWidth {
        name: "threesuperior",
        width: 333,
    },
    GlyphWidth {
        name: "tilde",
        width: 333,
    },
    GlyphWidth {
        name: "trademark",
        width: 1000,
    },
    GlyphWidth {
        name: "two",
        width: 556,
    },
    GlyphWidth {
        name: "twosuperior",
        width: 333,
    },
    GlyphWidth {
        name: "u",
        width: 611,
    },
    GlyphWidth {
        name: "uacute",
        width: 611,
    },
    GlyphWidth {
        name: "ucircumflex",
        width: 611,
    },
    GlyphWidth {
        name: "udieresis",
        width: 611,
    },
    GlyphWidth {
        name: "ugrave",
        width: 611,
    },
    GlyphWidth {
        name: "uhungarumlaut",
        width: 611,
    },
    GlyphWidth {
        name: "umacron",
        width: 611,
    },
    GlyphWidth {
        name: "underscore",
        width: 556,
    },
    GlyphWidth {
        name: "uogonek",
        width: 611,
    },
    GlyphWidth {
        name: "uring",
        width: 611,
    },
    GlyphWidth {
        name: "v",
        width: 556,
    },
    GlyphWidth {
        name: "w",
        width: 778,
    },
    GlyphWidth {
        name: "x",
        width: 556,
    },
    GlyphWidth {
        name: "y",
        width: 556,
    },
    GlyphWidth {
        name: "yacute",
        width: 556,
    },
    GlyphWidth {
        name: "ydieresis",
        width: 556,
    },
    GlyphWidth {
        name: "yen",
        width: 556,
    },
    GlyphWidth {
        name: "z",
        width: 500,
    },
    GlyphWidth {
        name: "zacute",
        width: 500,
    },
    GlyphWidth {
        name: "zcaron",
        width: 500,
    },
    GlyphWidth {
        name: "zdotaccent",
        width: 500,
    },
    GlyphWidth {
        name: "zero",
        width: 556,
    },
];

/// `Helvetica-BoldOblique` — 315 glyphs.
pub const Helvetica_BoldOblique: &[GlyphWidth] = &[
    GlyphWidth {
        name: "A",
        width: 722,
    },
    GlyphWidth {
        name: "AE",
        width: 1000,
    },
    GlyphWidth {
        name: "Aacute",
        width: 722,
    },
    GlyphWidth {
        name: "Abreve",
        width: 722,
    },
    GlyphWidth {
        name: "Acircumflex",
        width: 722,
    },
    GlyphWidth {
        name: "Adieresis",
        width: 722,
    },
    GlyphWidth {
        name: "Agrave",
        width: 722,
    },
    GlyphWidth {
        name: "Amacron",
        width: 722,
    },
    GlyphWidth {
        name: "Aogonek",
        width: 722,
    },
    GlyphWidth {
        name: "Aring",
        width: 722,
    },
    GlyphWidth {
        name: "Atilde",
        width: 722,
    },
    GlyphWidth {
        name: "B",
        width: 722,
    },
    GlyphWidth {
        name: "C",
        width: 722,
    },
    GlyphWidth {
        name: "Cacute",
        width: 722,
    },
    GlyphWidth {
        name: "Ccaron",
        width: 722,
    },
    GlyphWidth {
        name: "Ccedilla",
        width: 722,
    },
    GlyphWidth {
        name: "D",
        width: 722,
    },
    GlyphWidth {
        name: "Dcaron",
        width: 722,
    },
    GlyphWidth {
        name: "Dcroat",
        width: 722,
    },
    GlyphWidth {
        name: "Delta",
        width: 612,
    },
    GlyphWidth {
        name: "E",
        width: 667,
    },
    GlyphWidth {
        name: "Eacute",
        width: 667,
    },
    GlyphWidth {
        name: "Ecaron",
        width: 667,
    },
    GlyphWidth {
        name: "Ecircumflex",
        width: 667,
    },
    GlyphWidth {
        name: "Edieresis",
        width: 667,
    },
    GlyphWidth {
        name: "Edotaccent",
        width: 667,
    },
    GlyphWidth {
        name: "Egrave",
        width: 667,
    },
    GlyphWidth {
        name: "Emacron",
        width: 667,
    },
    GlyphWidth {
        name: "Eogonek",
        width: 667,
    },
    GlyphWidth {
        name: "Eth",
        width: 722,
    },
    GlyphWidth {
        name: "Euro",
        width: 556,
    },
    GlyphWidth {
        name: "F",
        width: 611,
    },
    GlyphWidth {
        name: "G",
        width: 778,
    },
    GlyphWidth {
        name: "Gbreve",
        width: 778,
    },
    GlyphWidth {
        name: "Gcommaaccent",
        width: 778,
    },
    GlyphWidth {
        name: "H",
        width: 722,
    },
    GlyphWidth {
        name: "I",
        width: 278,
    },
    GlyphWidth {
        name: "Iacute",
        width: 278,
    },
    GlyphWidth {
        name: "Icircumflex",
        width: 278,
    },
    GlyphWidth {
        name: "Idieresis",
        width: 278,
    },
    GlyphWidth {
        name: "Idotaccent",
        width: 278,
    },
    GlyphWidth {
        name: "Igrave",
        width: 278,
    },
    GlyphWidth {
        name: "Imacron",
        width: 278,
    },
    GlyphWidth {
        name: "Iogonek",
        width: 278,
    },
    GlyphWidth {
        name: "J",
        width: 556,
    },
    GlyphWidth {
        name: "K",
        width: 722,
    },
    GlyphWidth {
        name: "Kcommaaccent",
        width: 722,
    },
    GlyphWidth {
        name: "L",
        width: 611,
    },
    GlyphWidth {
        name: "Lacute",
        width: 611,
    },
    GlyphWidth {
        name: "Lcaron",
        width: 611,
    },
    GlyphWidth {
        name: "Lcommaaccent",
        width: 611,
    },
    GlyphWidth {
        name: "Lslash",
        width: 611,
    },
    GlyphWidth {
        name: "M",
        width: 833,
    },
    GlyphWidth {
        name: "N",
        width: 722,
    },
    GlyphWidth {
        name: "Nacute",
        width: 722,
    },
    GlyphWidth {
        name: "Ncaron",
        width: 722,
    },
    GlyphWidth {
        name: "Ncommaaccent",
        width: 722,
    },
    GlyphWidth {
        name: "Ntilde",
        width: 722,
    },
    GlyphWidth {
        name: "O",
        width: 778,
    },
    GlyphWidth {
        name: "OE",
        width: 1000,
    },
    GlyphWidth {
        name: "Oacute",
        width: 778,
    },
    GlyphWidth {
        name: "Ocircumflex",
        width: 778,
    },
    GlyphWidth {
        name: "Odieresis",
        width: 778,
    },
    GlyphWidth {
        name: "Ograve",
        width: 778,
    },
    GlyphWidth {
        name: "Ohungarumlaut",
        width: 778,
    },
    GlyphWidth {
        name: "Omacron",
        width: 778,
    },
    GlyphWidth {
        name: "Oslash",
        width: 778,
    },
    GlyphWidth {
        name: "Otilde",
        width: 778,
    },
    GlyphWidth {
        name: "P",
        width: 667,
    },
    GlyphWidth {
        name: "Q",
        width: 778,
    },
    GlyphWidth {
        name: "R",
        width: 722,
    },
    GlyphWidth {
        name: "Racute",
        width: 722,
    },
    GlyphWidth {
        name: "Rcaron",
        width: 722,
    },
    GlyphWidth {
        name: "Rcommaaccent",
        width: 722,
    },
    GlyphWidth {
        name: "S",
        width: 667,
    },
    GlyphWidth {
        name: "Sacute",
        width: 667,
    },
    GlyphWidth {
        name: "Scaron",
        width: 667,
    },
    GlyphWidth {
        name: "Scedilla",
        width: 667,
    },
    GlyphWidth {
        name: "Scommaaccent",
        width: 667,
    },
    GlyphWidth {
        name: "T",
        width: 611,
    },
    GlyphWidth {
        name: "Tcaron",
        width: 611,
    },
    GlyphWidth {
        name: "Tcommaaccent",
        width: 611,
    },
    GlyphWidth {
        name: "Thorn",
        width: 667,
    },
    GlyphWidth {
        name: "U",
        width: 722,
    },
    GlyphWidth {
        name: "Uacute",
        width: 722,
    },
    GlyphWidth {
        name: "Ucircumflex",
        width: 722,
    },
    GlyphWidth {
        name: "Udieresis",
        width: 722,
    },
    GlyphWidth {
        name: "Ugrave",
        width: 722,
    },
    GlyphWidth {
        name: "Uhungarumlaut",
        width: 722,
    },
    GlyphWidth {
        name: "Umacron",
        width: 722,
    },
    GlyphWidth {
        name: "Uogonek",
        width: 722,
    },
    GlyphWidth {
        name: "Uring",
        width: 722,
    },
    GlyphWidth {
        name: "V",
        width: 667,
    },
    GlyphWidth {
        name: "W",
        width: 944,
    },
    GlyphWidth {
        name: "X",
        width: 667,
    },
    GlyphWidth {
        name: "Y",
        width: 667,
    },
    GlyphWidth {
        name: "Yacute",
        width: 667,
    },
    GlyphWidth {
        name: "Ydieresis",
        width: 667,
    },
    GlyphWidth {
        name: "Z",
        width: 611,
    },
    GlyphWidth {
        name: "Zacute",
        width: 611,
    },
    GlyphWidth {
        name: "Zcaron",
        width: 611,
    },
    GlyphWidth {
        name: "Zdotaccent",
        width: 611,
    },
    GlyphWidth {
        name: "a",
        width: 556,
    },
    GlyphWidth {
        name: "aacute",
        width: 556,
    },
    GlyphWidth {
        name: "abreve",
        width: 556,
    },
    GlyphWidth {
        name: "acircumflex",
        width: 556,
    },
    GlyphWidth {
        name: "acute",
        width: 333,
    },
    GlyphWidth {
        name: "adieresis",
        width: 556,
    },
    GlyphWidth {
        name: "ae",
        width: 889,
    },
    GlyphWidth {
        name: "agrave",
        width: 556,
    },
    GlyphWidth {
        name: "amacron",
        width: 556,
    },
    GlyphWidth {
        name: "ampersand",
        width: 722,
    },
    GlyphWidth {
        name: "aogonek",
        width: 556,
    },
    GlyphWidth {
        name: "aring",
        width: 556,
    },
    GlyphWidth {
        name: "asciicircum",
        width: 584,
    },
    GlyphWidth {
        name: "asciitilde",
        width: 584,
    },
    GlyphWidth {
        name: "asterisk",
        width: 389,
    },
    GlyphWidth {
        name: "at",
        width: 975,
    },
    GlyphWidth {
        name: "atilde",
        width: 556,
    },
    GlyphWidth {
        name: "b",
        width: 611,
    },
    GlyphWidth {
        name: "backslash",
        width: 278,
    },
    GlyphWidth {
        name: "bar",
        width: 280,
    },
    GlyphWidth {
        name: "braceleft",
        width: 389,
    },
    GlyphWidth {
        name: "braceright",
        width: 389,
    },
    GlyphWidth {
        name: "bracketleft",
        width: 333,
    },
    GlyphWidth {
        name: "bracketright",
        width: 333,
    },
    GlyphWidth {
        name: "breve",
        width: 333,
    },
    GlyphWidth {
        name: "brokenbar",
        width: 280,
    },
    GlyphWidth {
        name: "bullet",
        width: 350,
    },
    GlyphWidth {
        name: "c",
        width: 556,
    },
    GlyphWidth {
        name: "cacute",
        width: 556,
    },
    GlyphWidth {
        name: "caron",
        width: 333,
    },
    GlyphWidth {
        name: "ccaron",
        width: 556,
    },
    GlyphWidth {
        name: "ccedilla",
        width: 556,
    },
    GlyphWidth {
        name: "cedilla",
        width: 333,
    },
    GlyphWidth {
        name: "cent",
        width: 556,
    },
    GlyphWidth {
        name: "circumflex",
        width: 333,
    },
    GlyphWidth {
        name: "colon",
        width: 333,
    },
    GlyphWidth {
        name: "comma",
        width: 278,
    },
    GlyphWidth {
        name: "commaaccent",
        width: 250,
    },
    GlyphWidth {
        name: "copyright",
        width: 737,
    },
    GlyphWidth {
        name: "currency",
        width: 556,
    },
    GlyphWidth {
        name: "d",
        width: 611,
    },
    GlyphWidth {
        name: "dagger",
        width: 556,
    },
    GlyphWidth {
        name: "daggerdbl",
        width: 556,
    },
    GlyphWidth {
        name: "dcaron",
        width: 743,
    },
    GlyphWidth {
        name: "dcroat",
        width: 611,
    },
    GlyphWidth {
        name: "degree",
        width: 400,
    },
    GlyphWidth {
        name: "dieresis",
        width: 333,
    },
    GlyphWidth {
        name: "divide",
        width: 584,
    },
    GlyphWidth {
        name: "dollar",
        width: 556,
    },
    GlyphWidth {
        name: "dotaccent",
        width: 333,
    },
    GlyphWidth {
        name: "dotlessi",
        width: 278,
    },
    GlyphWidth {
        name: "e",
        width: 556,
    },
    GlyphWidth {
        name: "eacute",
        width: 556,
    },
    GlyphWidth {
        name: "ecaron",
        width: 556,
    },
    GlyphWidth {
        name: "ecircumflex",
        width: 556,
    },
    GlyphWidth {
        name: "edieresis",
        width: 556,
    },
    GlyphWidth {
        name: "edotaccent",
        width: 556,
    },
    GlyphWidth {
        name: "egrave",
        width: 556,
    },
    GlyphWidth {
        name: "eight",
        width: 556,
    },
    GlyphWidth {
        name: "ellipsis",
        width: 1000,
    },
    GlyphWidth {
        name: "emacron",
        width: 556,
    },
    GlyphWidth {
        name: "emdash",
        width: 1000,
    },
    GlyphWidth {
        name: "endash",
        width: 556,
    },
    GlyphWidth {
        name: "eogonek",
        width: 556,
    },
    GlyphWidth {
        name: "equal",
        width: 584,
    },
    GlyphWidth {
        name: "eth",
        width: 611,
    },
    GlyphWidth {
        name: "exclam",
        width: 333,
    },
    GlyphWidth {
        name: "exclamdown",
        width: 333,
    },
    GlyphWidth {
        name: "f",
        width: 333,
    },
    GlyphWidth {
        name: "fi",
        width: 611,
    },
    GlyphWidth {
        name: "five",
        width: 556,
    },
    GlyphWidth {
        name: "fl",
        width: 611,
    },
    GlyphWidth {
        name: "florin",
        width: 556,
    },
    GlyphWidth {
        name: "four",
        width: 556,
    },
    GlyphWidth {
        name: "fraction",
        width: 167,
    },
    GlyphWidth {
        name: "g",
        width: 611,
    },
    GlyphWidth {
        name: "gbreve",
        width: 611,
    },
    GlyphWidth {
        name: "gcommaaccent",
        width: 611,
    },
    GlyphWidth {
        name: "germandbls",
        width: 611,
    },
    GlyphWidth {
        name: "grave",
        width: 333,
    },
    GlyphWidth {
        name: "greater",
        width: 584,
    },
    GlyphWidth {
        name: "greaterequal",
        width: 549,
    },
    GlyphWidth {
        name: "guillemotleft",
        width: 556,
    },
    GlyphWidth {
        name: "guillemotright",
        width: 556,
    },
    GlyphWidth {
        name: "guilsinglleft",
        width: 333,
    },
    GlyphWidth {
        name: "guilsinglright",
        width: 333,
    },
    GlyphWidth {
        name: "h",
        width: 611,
    },
    GlyphWidth {
        name: "hungarumlaut",
        width: 333,
    },
    GlyphWidth {
        name: "hyphen",
        width: 333,
    },
    GlyphWidth {
        name: "i",
        width: 278,
    },
    GlyphWidth {
        name: "iacute",
        width: 278,
    },
    GlyphWidth {
        name: "icircumflex",
        width: 278,
    },
    GlyphWidth {
        name: "idieresis",
        width: 278,
    },
    GlyphWidth {
        name: "igrave",
        width: 278,
    },
    GlyphWidth {
        name: "imacron",
        width: 278,
    },
    GlyphWidth {
        name: "iogonek",
        width: 278,
    },
    GlyphWidth {
        name: "j",
        width: 278,
    },
    GlyphWidth {
        name: "k",
        width: 556,
    },
    GlyphWidth {
        name: "kcommaaccent",
        width: 556,
    },
    GlyphWidth {
        name: "l",
        width: 278,
    },
    GlyphWidth {
        name: "lacute",
        width: 278,
    },
    GlyphWidth {
        name: "lcaron",
        width: 400,
    },
    GlyphWidth {
        name: "lcommaaccent",
        width: 278,
    },
    GlyphWidth {
        name: "less",
        width: 584,
    },
    GlyphWidth {
        name: "lessequal",
        width: 549,
    },
    GlyphWidth {
        name: "logicalnot",
        width: 584,
    },
    GlyphWidth {
        name: "lozenge",
        width: 494,
    },
    GlyphWidth {
        name: "lslash",
        width: 278,
    },
    GlyphWidth {
        name: "m",
        width: 889,
    },
    GlyphWidth {
        name: "macron",
        width: 333,
    },
    GlyphWidth {
        name: "minus",
        width: 584,
    },
    GlyphWidth {
        name: "mu",
        width: 611,
    },
    GlyphWidth {
        name: "multiply",
        width: 584,
    },
    GlyphWidth {
        name: "n",
        width: 611,
    },
    GlyphWidth {
        name: "nacute",
        width: 611,
    },
    GlyphWidth {
        name: "ncaron",
        width: 611,
    },
    GlyphWidth {
        name: "ncommaaccent",
        width: 611,
    },
    GlyphWidth {
        name: "nine",
        width: 556,
    },
    GlyphWidth {
        name: "notequal",
        width: 549,
    },
    GlyphWidth {
        name: "ntilde",
        width: 611,
    },
    GlyphWidth {
        name: "numbersign",
        width: 556,
    },
    GlyphWidth {
        name: "o",
        width: 611,
    },
    GlyphWidth {
        name: "oacute",
        width: 611,
    },
    GlyphWidth {
        name: "ocircumflex",
        width: 611,
    },
    GlyphWidth {
        name: "odieresis",
        width: 611,
    },
    GlyphWidth {
        name: "oe",
        width: 944,
    },
    GlyphWidth {
        name: "ogonek",
        width: 333,
    },
    GlyphWidth {
        name: "ograve",
        width: 611,
    },
    GlyphWidth {
        name: "ohungarumlaut",
        width: 611,
    },
    GlyphWidth {
        name: "omacron",
        width: 611,
    },
    GlyphWidth {
        name: "one",
        width: 556,
    },
    GlyphWidth {
        name: "onehalf",
        width: 834,
    },
    GlyphWidth {
        name: "onequarter",
        width: 834,
    },
    GlyphWidth {
        name: "onesuperior",
        width: 333,
    },
    GlyphWidth {
        name: "ordfeminine",
        width: 370,
    },
    GlyphWidth {
        name: "ordmasculine",
        width: 365,
    },
    GlyphWidth {
        name: "oslash",
        width: 611,
    },
    GlyphWidth {
        name: "otilde",
        width: 611,
    },
    GlyphWidth {
        name: "p",
        width: 611,
    },
    GlyphWidth {
        name: "paragraph",
        width: 556,
    },
    GlyphWidth {
        name: "parenleft",
        width: 333,
    },
    GlyphWidth {
        name: "parenright",
        width: 333,
    },
    GlyphWidth {
        name: "partialdiff",
        width: 494,
    },
    GlyphWidth {
        name: "percent",
        width: 889,
    },
    GlyphWidth {
        name: "period",
        width: 278,
    },
    GlyphWidth {
        name: "periodcentered",
        width: 278,
    },
    GlyphWidth {
        name: "perthousand",
        width: 1000,
    },
    GlyphWidth {
        name: "plus",
        width: 584,
    },
    GlyphWidth {
        name: "plusminus",
        width: 584,
    },
    GlyphWidth {
        name: "q",
        width: 611,
    },
    GlyphWidth {
        name: "question",
        width: 611,
    },
    GlyphWidth {
        name: "questiondown",
        width: 611,
    },
    GlyphWidth {
        name: "quotedbl",
        width: 474,
    },
    GlyphWidth {
        name: "quotedblbase",
        width: 500,
    },
    GlyphWidth {
        name: "quotedblleft",
        width: 500,
    },
    GlyphWidth {
        name: "quotedblright",
        width: 500,
    },
    GlyphWidth {
        name: "quoteleft",
        width: 278,
    },
    GlyphWidth {
        name: "quoteright",
        width: 278,
    },
    GlyphWidth {
        name: "quotesinglbase",
        width: 278,
    },
    GlyphWidth {
        name: "quotesingle",
        width: 238,
    },
    GlyphWidth {
        name: "r",
        width: 389,
    },
    GlyphWidth {
        name: "racute",
        width: 389,
    },
    GlyphWidth {
        name: "radical",
        width: 549,
    },
    GlyphWidth {
        name: "rcaron",
        width: 389,
    },
    GlyphWidth {
        name: "rcommaaccent",
        width: 389,
    },
    GlyphWidth {
        name: "registered",
        width: 737,
    },
    GlyphWidth {
        name: "ring",
        width: 333,
    },
    GlyphWidth {
        name: "s",
        width: 556,
    },
    GlyphWidth {
        name: "sacute",
        width: 556,
    },
    GlyphWidth {
        name: "scaron",
        width: 556,
    },
    GlyphWidth {
        name: "scedilla",
        width: 556,
    },
    GlyphWidth {
        name: "scommaaccent",
        width: 556,
    },
    GlyphWidth {
        name: "section",
        width: 556,
    },
    GlyphWidth {
        name: "semicolon",
        width: 333,
    },
    GlyphWidth {
        name: "seven",
        width: 556,
    },
    GlyphWidth {
        name: "six",
        width: 556,
    },
    GlyphWidth {
        name: "slash",
        width: 278,
    },
    GlyphWidth {
        name: "space",
        width: 278,
    },
    GlyphWidth {
        name: "sterling",
        width: 556,
    },
    GlyphWidth {
        name: "summation",
        width: 600,
    },
    GlyphWidth {
        name: "t",
        width: 333,
    },
    GlyphWidth {
        name: "tcaron",
        width: 389,
    },
    GlyphWidth {
        name: "tcommaaccent",
        width: 333,
    },
    GlyphWidth {
        name: "thorn",
        width: 611,
    },
    GlyphWidth {
        name: "three",
        width: 556,
    },
    GlyphWidth {
        name: "threequarters",
        width: 834,
    },
    GlyphWidth {
        name: "threesuperior",
        width: 333,
    },
    GlyphWidth {
        name: "tilde",
        width: 333,
    },
    GlyphWidth {
        name: "trademark",
        width: 1000,
    },
    GlyphWidth {
        name: "two",
        width: 556,
    },
    GlyphWidth {
        name: "twosuperior",
        width: 333,
    },
    GlyphWidth {
        name: "u",
        width: 611,
    },
    GlyphWidth {
        name: "uacute",
        width: 611,
    },
    GlyphWidth {
        name: "ucircumflex",
        width: 611,
    },
    GlyphWidth {
        name: "udieresis",
        width: 611,
    },
    GlyphWidth {
        name: "ugrave",
        width: 611,
    },
    GlyphWidth {
        name: "uhungarumlaut",
        width: 611,
    },
    GlyphWidth {
        name: "umacron",
        width: 611,
    },
    GlyphWidth {
        name: "underscore",
        width: 556,
    },
    GlyphWidth {
        name: "uogonek",
        width: 611,
    },
    GlyphWidth {
        name: "uring",
        width: 611,
    },
    GlyphWidth {
        name: "v",
        width: 556,
    },
    GlyphWidth {
        name: "w",
        width: 778,
    },
    GlyphWidth {
        name: "x",
        width: 556,
    },
    GlyphWidth {
        name: "y",
        width: 556,
    },
    GlyphWidth {
        name: "yacute",
        width: 556,
    },
    GlyphWidth {
        name: "ydieresis",
        width: 556,
    },
    GlyphWidth {
        name: "yen",
        width: 556,
    },
    GlyphWidth {
        name: "z",
        width: 500,
    },
    GlyphWidth {
        name: "zacute",
        width: 500,
    },
    GlyphWidth {
        name: "zcaron",
        width: 500,
    },
    GlyphWidth {
        name: "zdotaccent",
        width: 500,
    },
    GlyphWidth {
        name: "zero",
        width: 556,
    },
];

/// `Helvetica-Oblique` — 315 glyphs.
pub const Helvetica_Oblique: &[GlyphWidth] = &[
    GlyphWidth {
        name: "A",
        width: 667,
    },
    GlyphWidth {
        name: "AE",
        width: 1000,
    },
    GlyphWidth {
        name: "Aacute",
        width: 667,
    },
    GlyphWidth {
        name: "Abreve",
        width: 667,
    },
    GlyphWidth {
        name: "Acircumflex",
        width: 667,
    },
    GlyphWidth {
        name: "Adieresis",
        width: 667,
    },
    GlyphWidth {
        name: "Agrave",
        width: 667,
    },
    GlyphWidth {
        name: "Amacron",
        width: 667,
    },
    GlyphWidth {
        name: "Aogonek",
        width: 667,
    },
    GlyphWidth {
        name: "Aring",
        width: 667,
    },
    GlyphWidth {
        name: "Atilde",
        width: 667,
    },
    GlyphWidth {
        name: "B",
        width: 667,
    },
    GlyphWidth {
        name: "C",
        width: 722,
    },
    GlyphWidth {
        name: "Cacute",
        width: 722,
    },
    GlyphWidth {
        name: "Ccaron",
        width: 722,
    },
    GlyphWidth {
        name: "Ccedilla",
        width: 722,
    },
    GlyphWidth {
        name: "D",
        width: 722,
    },
    GlyphWidth {
        name: "Dcaron",
        width: 722,
    },
    GlyphWidth {
        name: "Dcroat",
        width: 722,
    },
    GlyphWidth {
        name: "Delta",
        width: 612,
    },
    GlyphWidth {
        name: "E",
        width: 667,
    },
    GlyphWidth {
        name: "Eacute",
        width: 667,
    },
    GlyphWidth {
        name: "Ecaron",
        width: 667,
    },
    GlyphWidth {
        name: "Ecircumflex",
        width: 667,
    },
    GlyphWidth {
        name: "Edieresis",
        width: 667,
    },
    GlyphWidth {
        name: "Edotaccent",
        width: 667,
    },
    GlyphWidth {
        name: "Egrave",
        width: 667,
    },
    GlyphWidth {
        name: "Emacron",
        width: 667,
    },
    GlyphWidth {
        name: "Eogonek",
        width: 667,
    },
    GlyphWidth {
        name: "Eth",
        width: 722,
    },
    GlyphWidth {
        name: "Euro",
        width: 556,
    },
    GlyphWidth {
        name: "F",
        width: 611,
    },
    GlyphWidth {
        name: "G",
        width: 778,
    },
    GlyphWidth {
        name: "Gbreve",
        width: 778,
    },
    GlyphWidth {
        name: "Gcommaaccent",
        width: 778,
    },
    GlyphWidth {
        name: "H",
        width: 722,
    },
    GlyphWidth {
        name: "I",
        width: 278,
    },
    GlyphWidth {
        name: "Iacute",
        width: 278,
    },
    GlyphWidth {
        name: "Icircumflex",
        width: 278,
    },
    GlyphWidth {
        name: "Idieresis",
        width: 278,
    },
    GlyphWidth {
        name: "Idotaccent",
        width: 278,
    },
    GlyphWidth {
        name: "Igrave",
        width: 278,
    },
    GlyphWidth {
        name: "Imacron",
        width: 278,
    },
    GlyphWidth {
        name: "Iogonek",
        width: 278,
    },
    GlyphWidth {
        name: "J",
        width: 500,
    },
    GlyphWidth {
        name: "K",
        width: 667,
    },
    GlyphWidth {
        name: "Kcommaaccent",
        width: 667,
    },
    GlyphWidth {
        name: "L",
        width: 556,
    },
    GlyphWidth {
        name: "Lacute",
        width: 556,
    },
    GlyphWidth {
        name: "Lcaron",
        width: 556,
    },
    GlyphWidth {
        name: "Lcommaaccent",
        width: 556,
    },
    GlyphWidth {
        name: "Lslash",
        width: 556,
    },
    GlyphWidth {
        name: "M",
        width: 833,
    },
    GlyphWidth {
        name: "N",
        width: 722,
    },
    GlyphWidth {
        name: "Nacute",
        width: 722,
    },
    GlyphWidth {
        name: "Ncaron",
        width: 722,
    },
    GlyphWidth {
        name: "Ncommaaccent",
        width: 722,
    },
    GlyphWidth {
        name: "Ntilde",
        width: 722,
    },
    GlyphWidth {
        name: "O",
        width: 778,
    },
    GlyphWidth {
        name: "OE",
        width: 1000,
    },
    GlyphWidth {
        name: "Oacute",
        width: 778,
    },
    GlyphWidth {
        name: "Ocircumflex",
        width: 778,
    },
    GlyphWidth {
        name: "Odieresis",
        width: 778,
    },
    GlyphWidth {
        name: "Ograve",
        width: 778,
    },
    GlyphWidth {
        name: "Ohungarumlaut",
        width: 778,
    },
    GlyphWidth {
        name: "Omacron",
        width: 778,
    },
    GlyphWidth {
        name: "Oslash",
        width: 778,
    },
    GlyphWidth {
        name: "Otilde",
        width: 778,
    },
    GlyphWidth {
        name: "P",
        width: 667,
    },
    GlyphWidth {
        name: "Q",
        width: 778,
    },
    GlyphWidth {
        name: "R",
        width: 722,
    },
    GlyphWidth {
        name: "Racute",
        width: 722,
    },
    GlyphWidth {
        name: "Rcaron",
        width: 722,
    },
    GlyphWidth {
        name: "Rcommaaccent",
        width: 722,
    },
    GlyphWidth {
        name: "S",
        width: 667,
    },
    GlyphWidth {
        name: "Sacute",
        width: 667,
    },
    GlyphWidth {
        name: "Scaron",
        width: 667,
    },
    GlyphWidth {
        name: "Scedilla",
        width: 667,
    },
    GlyphWidth {
        name: "Scommaaccent",
        width: 667,
    },
    GlyphWidth {
        name: "T",
        width: 611,
    },
    GlyphWidth {
        name: "Tcaron",
        width: 611,
    },
    GlyphWidth {
        name: "Tcommaaccent",
        width: 611,
    },
    GlyphWidth {
        name: "Thorn",
        width: 667,
    },
    GlyphWidth {
        name: "U",
        width: 722,
    },
    GlyphWidth {
        name: "Uacute",
        width: 722,
    },
    GlyphWidth {
        name: "Ucircumflex",
        width: 722,
    },
    GlyphWidth {
        name: "Udieresis",
        width: 722,
    },
    GlyphWidth {
        name: "Ugrave",
        width: 722,
    },
    GlyphWidth {
        name: "Uhungarumlaut",
        width: 722,
    },
    GlyphWidth {
        name: "Umacron",
        width: 722,
    },
    GlyphWidth {
        name: "Uogonek",
        width: 722,
    },
    GlyphWidth {
        name: "Uring",
        width: 722,
    },
    GlyphWidth {
        name: "V",
        width: 667,
    },
    GlyphWidth {
        name: "W",
        width: 944,
    },
    GlyphWidth {
        name: "X",
        width: 667,
    },
    GlyphWidth {
        name: "Y",
        width: 667,
    },
    GlyphWidth {
        name: "Yacute",
        width: 667,
    },
    GlyphWidth {
        name: "Ydieresis",
        width: 667,
    },
    GlyphWidth {
        name: "Z",
        width: 611,
    },
    GlyphWidth {
        name: "Zacute",
        width: 611,
    },
    GlyphWidth {
        name: "Zcaron",
        width: 611,
    },
    GlyphWidth {
        name: "Zdotaccent",
        width: 611,
    },
    GlyphWidth {
        name: "a",
        width: 556,
    },
    GlyphWidth {
        name: "aacute",
        width: 556,
    },
    GlyphWidth {
        name: "abreve",
        width: 556,
    },
    GlyphWidth {
        name: "acircumflex",
        width: 556,
    },
    GlyphWidth {
        name: "acute",
        width: 333,
    },
    GlyphWidth {
        name: "adieresis",
        width: 556,
    },
    GlyphWidth {
        name: "ae",
        width: 889,
    },
    GlyphWidth {
        name: "agrave",
        width: 556,
    },
    GlyphWidth {
        name: "amacron",
        width: 556,
    },
    GlyphWidth {
        name: "ampersand",
        width: 667,
    },
    GlyphWidth {
        name: "aogonek",
        width: 556,
    },
    GlyphWidth {
        name: "aring",
        width: 556,
    },
    GlyphWidth {
        name: "asciicircum",
        width: 469,
    },
    GlyphWidth {
        name: "asciitilde",
        width: 584,
    },
    GlyphWidth {
        name: "asterisk",
        width: 389,
    },
    GlyphWidth {
        name: "at",
        width: 1015,
    },
    GlyphWidth {
        name: "atilde",
        width: 556,
    },
    GlyphWidth {
        name: "b",
        width: 556,
    },
    GlyphWidth {
        name: "backslash",
        width: 278,
    },
    GlyphWidth {
        name: "bar",
        width: 260,
    },
    GlyphWidth {
        name: "braceleft",
        width: 334,
    },
    GlyphWidth {
        name: "braceright",
        width: 334,
    },
    GlyphWidth {
        name: "bracketleft",
        width: 278,
    },
    GlyphWidth {
        name: "bracketright",
        width: 278,
    },
    GlyphWidth {
        name: "breve",
        width: 333,
    },
    GlyphWidth {
        name: "brokenbar",
        width: 260,
    },
    GlyphWidth {
        name: "bullet",
        width: 350,
    },
    GlyphWidth {
        name: "c",
        width: 500,
    },
    GlyphWidth {
        name: "cacute",
        width: 500,
    },
    GlyphWidth {
        name: "caron",
        width: 333,
    },
    GlyphWidth {
        name: "ccaron",
        width: 500,
    },
    GlyphWidth {
        name: "ccedilla",
        width: 500,
    },
    GlyphWidth {
        name: "cedilla",
        width: 333,
    },
    GlyphWidth {
        name: "cent",
        width: 556,
    },
    GlyphWidth {
        name: "circumflex",
        width: 333,
    },
    GlyphWidth {
        name: "colon",
        width: 278,
    },
    GlyphWidth {
        name: "comma",
        width: 278,
    },
    GlyphWidth {
        name: "commaaccent",
        width: 250,
    },
    GlyphWidth {
        name: "copyright",
        width: 737,
    },
    GlyphWidth {
        name: "currency",
        width: 556,
    },
    GlyphWidth {
        name: "d",
        width: 556,
    },
    GlyphWidth {
        name: "dagger",
        width: 556,
    },
    GlyphWidth {
        name: "daggerdbl",
        width: 556,
    },
    GlyphWidth {
        name: "dcaron",
        width: 643,
    },
    GlyphWidth {
        name: "dcroat",
        width: 556,
    },
    GlyphWidth {
        name: "degree",
        width: 400,
    },
    GlyphWidth {
        name: "dieresis",
        width: 333,
    },
    GlyphWidth {
        name: "divide",
        width: 584,
    },
    GlyphWidth {
        name: "dollar",
        width: 556,
    },
    GlyphWidth {
        name: "dotaccent",
        width: 333,
    },
    GlyphWidth {
        name: "dotlessi",
        width: 278,
    },
    GlyphWidth {
        name: "e",
        width: 556,
    },
    GlyphWidth {
        name: "eacute",
        width: 556,
    },
    GlyphWidth {
        name: "ecaron",
        width: 556,
    },
    GlyphWidth {
        name: "ecircumflex",
        width: 556,
    },
    GlyphWidth {
        name: "edieresis",
        width: 556,
    },
    GlyphWidth {
        name: "edotaccent",
        width: 556,
    },
    GlyphWidth {
        name: "egrave",
        width: 556,
    },
    GlyphWidth {
        name: "eight",
        width: 556,
    },
    GlyphWidth {
        name: "ellipsis",
        width: 1000,
    },
    GlyphWidth {
        name: "emacron",
        width: 556,
    },
    GlyphWidth {
        name: "emdash",
        width: 1000,
    },
    GlyphWidth {
        name: "endash",
        width: 556,
    },
    GlyphWidth {
        name: "eogonek",
        width: 556,
    },
    GlyphWidth {
        name: "equal",
        width: 584,
    },
    GlyphWidth {
        name: "eth",
        width: 556,
    },
    GlyphWidth {
        name: "exclam",
        width: 278,
    },
    GlyphWidth {
        name: "exclamdown",
        width: 333,
    },
    GlyphWidth {
        name: "f",
        width: 278,
    },
    GlyphWidth {
        name: "fi",
        width: 500,
    },
    GlyphWidth {
        name: "five",
        width: 556,
    },
    GlyphWidth {
        name: "fl",
        width: 500,
    },
    GlyphWidth {
        name: "florin",
        width: 556,
    },
    GlyphWidth {
        name: "four",
        width: 556,
    },
    GlyphWidth {
        name: "fraction",
        width: 167,
    },
    GlyphWidth {
        name: "g",
        width: 556,
    },
    GlyphWidth {
        name: "gbreve",
        width: 556,
    },
    GlyphWidth {
        name: "gcommaaccent",
        width: 556,
    },
    GlyphWidth {
        name: "germandbls",
        width: 611,
    },
    GlyphWidth {
        name: "grave",
        width: 333,
    },
    GlyphWidth {
        name: "greater",
        width: 584,
    },
    GlyphWidth {
        name: "greaterequal",
        width: 549,
    },
    GlyphWidth {
        name: "guillemotleft",
        width: 556,
    },
    GlyphWidth {
        name: "guillemotright",
        width: 556,
    },
    GlyphWidth {
        name: "guilsinglleft",
        width: 333,
    },
    GlyphWidth {
        name: "guilsinglright",
        width: 333,
    },
    GlyphWidth {
        name: "h",
        width: 556,
    },
    GlyphWidth {
        name: "hungarumlaut",
        width: 333,
    },
    GlyphWidth {
        name: "hyphen",
        width: 333,
    },
    GlyphWidth {
        name: "i",
        width: 222,
    },
    GlyphWidth {
        name: "iacute",
        width: 278,
    },
    GlyphWidth {
        name: "icircumflex",
        width: 278,
    },
    GlyphWidth {
        name: "idieresis",
        width: 278,
    },
    GlyphWidth {
        name: "igrave",
        width: 278,
    },
    GlyphWidth {
        name: "imacron",
        width: 278,
    },
    GlyphWidth {
        name: "iogonek",
        width: 222,
    },
    GlyphWidth {
        name: "j",
        width: 222,
    },
    GlyphWidth {
        name: "k",
        width: 500,
    },
    GlyphWidth {
        name: "kcommaaccent",
        width: 500,
    },
    GlyphWidth {
        name: "l",
        width: 222,
    },
    GlyphWidth {
        name: "lacute",
        width: 222,
    },
    GlyphWidth {
        name: "lcaron",
        width: 299,
    },
    GlyphWidth {
        name: "lcommaaccent",
        width: 222,
    },
    GlyphWidth {
        name: "less",
        width: 584,
    },
    GlyphWidth {
        name: "lessequal",
        width: 549,
    },
    GlyphWidth {
        name: "logicalnot",
        width: 584,
    },
    GlyphWidth {
        name: "lozenge",
        width: 471,
    },
    GlyphWidth {
        name: "lslash",
        width: 222,
    },
    GlyphWidth {
        name: "m",
        width: 833,
    },
    GlyphWidth {
        name: "macron",
        width: 333,
    },
    GlyphWidth {
        name: "minus",
        width: 584,
    },
    GlyphWidth {
        name: "mu",
        width: 556,
    },
    GlyphWidth {
        name: "multiply",
        width: 584,
    },
    GlyphWidth {
        name: "n",
        width: 556,
    },
    GlyphWidth {
        name: "nacute",
        width: 556,
    },
    GlyphWidth {
        name: "ncaron",
        width: 556,
    },
    GlyphWidth {
        name: "ncommaaccent",
        width: 556,
    },
    GlyphWidth {
        name: "nine",
        width: 556,
    },
    GlyphWidth {
        name: "notequal",
        width: 549,
    },
    GlyphWidth {
        name: "ntilde",
        width: 556,
    },
    GlyphWidth {
        name: "numbersign",
        width: 556,
    },
    GlyphWidth {
        name: "o",
        width: 556,
    },
    GlyphWidth {
        name: "oacute",
        width: 556,
    },
    GlyphWidth {
        name: "ocircumflex",
        width: 556,
    },
    GlyphWidth {
        name: "odieresis",
        width: 556,
    },
    GlyphWidth {
        name: "oe",
        width: 944,
    },
    GlyphWidth {
        name: "ogonek",
        width: 333,
    },
    GlyphWidth {
        name: "ograve",
        width: 556,
    },
    GlyphWidth {
        name: "ohungarumlaut",
        width: 556,
    },
    GlyphWidth {
        name: "omacron",
        width: 556,
    },
    GlyphWidth {
        name: "one",
        width: 556,
    },
    GlyphWidth {
        name: "onehalf",
        width: 834,
    },
    GlyphWidth {
        name: "onequarter",
        width: 834,
    },
    GlyphWidth {
        name: "onesuperior",
        width: 333,
    },
    GlyphWidth {
        name: "ordfeminine",
        width: 370,
    },
    GlyphWidth {
        name: "ordmasculine",
        width: 365,
    },
    GlyphWidth {
        name: "oslash",
        width: 611,
    },
    GlyphWidth {
        name: "otilde",
        width: 556,
    },
    GlyphWidth {
        name: "p",
        width: 556,
    },
    GlyphWidth {
        name: "paragraph",
        width: 537,
    },
    GlyphWidth {
        name: "parenleft",
        width: 333,
    },
    GlyphWidth {
        name: "parenright",
        width: 333,
    },
    GlyphWidth {
        name: "partialdiff",
        width: 476,
    },
    GlyphWidth {
        name: "percent",
        width: 889,
    },
    GlyphWidth {
        name: "period",
        width: 278,
    },
    GlyphWidth {
        name: "periodcentered",
        width: 278,
    },
    GlyphWidth {
        name: "perthousand",
        width: 1000,
    },
    GlyphWidth {
        name: "plus",
        width: 584,
    },
    GlyphWidth {
        name: "plusminus",
        width: 584,
    },
    GlyphWidth {
        name: "q",
        width: 556,
    },
    GlyphWidth {
        name: "question",
        width: 556,
    },
    GlyphWidth {
        name: "questiondown",
        width: 611,
    },
    GlyphWidth {
        name: "quotedbl",
        width: 355,
    },
    GlyphWidth {
        name: "quotedblbase",
        width: 333,
    },
    GlyphWidth {
        name: "quotedblleft",
        width: 333,
    },
    GlyphWidth {
        name: "quotedblright",
        width: 333,
    },
    GlyphWidth {
        name: "quoteleft",
        width: 222,
    },
    GlyphWidth {
        name: "quoteright",
        width: 222,
    },
    GlyphWidth {
        name: "quotesinglbase",
        width: 222,
    },
    GlyphWidth {
        name: "quotesingle",
        width: 191,
    },
    GlyphWidth {
        name: "r",
        width: 333,
    },
    GlyphWidth {
        name: "racute",
        width: 333,
    },
    GlyphWidth {
        name: "radical",
        width: 453,
    },
    GlyphWidth {
        name: "rcaron",
        width: 333,
    },
    GlyphWidth {
        name: "rcommaaccent",
        width: 333,
    },
    GlyphWidth {
        name: "registered",
        width: 737,
    },
    GlyphWidth {
        name: "ring",
        width: 333,
    },
    GlyphWidth {
        name: "s",
        width: 500,
    },
    GlyphWidth {
        name: "sacute",
        width: 500,
    },
    GlyphWidth {
        name: "scaron",
        width: 500,
    },
    GlyphWidth {
        name: "scedilla",
        width: 500,
    },
    GlyphWidth {
        name: "scommaaccent",
        width: 500,
    },
    GlyphWidth {
        name: "section",
        width: 556,
    },
    GlyphWidth {
        name: "semicolon",
        width: 278,
    },
    GlyphWidth {
        name: "seven",
        width: 556,
    },
    GlyphWidth {
        name: "six",
        width: 556,
    },
    GlyphWidth {
        name: "slash",
        width: 278,
    },
    GlyphWidth {
        name: "space",
        width: 278,
    },
    GlyphWidth {
        name: "sterling",
        width: 556,
    },
    GlyphWidth {
        name: "summation",
        width: 600,
    },
    GlyphWidth {
        name: "t",
        width: 278,
    },
    GlyphWidth {
        name: "tcaron",
        width: 317,
    },
    GlyphWidth {
        name: "tcommaaccent",
        width: 278,
    },
    GlyphWidth {
        name: "thorn",
        width: 556,
    },
    GlyphWidth {
        name: "three",
        width: 556,
    },
    GlyphWidth {
        name: "threequarters",
        width: 834,
    },
    GlyphWidth {
        name: "threesuperior",
        width: 333,
    },
    GlyphWidth {
        name: "tilde",
        width: 333,
    },
    GlyphWidth {
        name: "trademark",
        width: 1000,
    },
    GlyphWidth {
        name: "two",
        width: 556,
    },
    GlyphWidth {
        name: "twosuperior",
        width: 333,
    },
    GlyphWidth {
        name: "u",
        width: 556,
    },
    GlyphWidth {
        name: "uacute",
        width: 556,
    },
    GlyphWidth {
        name: "ucircumflex",
        width: 556,
    },
    GlyphWidth {
        name: "udieresis",
        width: 556,
    },
    GlyphWidth {
        name: "ugrave",
        width: 556,
    },
    GlyphWidth {
        name: "uhungarumlaut",
        width: 556,
    },
    GlyphWidth {
        name: "umacron",
        width: 556,
    },
    GlyphWidth {
        name: "underscore",
        width: 556,
    },
    GlyphWidth {
        name: "uogonek",
        width: 556,
    },
    GlyphWidth {
        name: "uring",
        width: 556,
    },
    GlyphWidth {
        name: "v",
        width: 500,
    },
    GlyphWidth {
        name: "w",
        width: 722,
    },
    GlyphWidth {
        name: "x",
        width: 500,
    },
    GlyphWidth {
        name: "y",
        width: 500,
    },
    GlyphWidth {
        name: "yacute",
        width: 500,
    },
    GlyphWidth {
        name: "ydieresis",
        width: 500,
    },
    GlyphWidth {
        name: "yen",
        width: 556,
    },
    GlyphWidth {
        name: "z",
        width: 500,
    },
    GlyphWidth {
        name: "zacute",
        width: 500,
    },
    GlyphWidth {
        name: "zcaron",
        width: 500,
    },
    GlyphWidth {
        name: "zdotaccent",
        width: 500,
    },
    GlyphWidth {
        name: "zero",
        width: 556,
    },
];

/// `Symbol` — 190 glyphs.
pub const Symbol: &[GlyphWidth] = &[
    GlyphWidth {
        name: "Alpha",
        width: 722,
    },
    GlyphWidth {
        name: "Beta",
        width: 667,
    },
    GlyphWidth {
        name: "Chi",
        width: 722,
    },
    GlyphWidth {
        name: "Delta",
        width: 612,
    },
    GlyphWidth {
        name: "Epsilon",
        width: 611,
    },
    GlyphWidth {
        name: "Eta",
        width: 722,
    },
    GlyphWidth {
        name: "Euro",
        width: 750,
    },
    GlyphWidth {
        name: "Gamma",
        width: 603,
    },
    GlyphWidth {
        name: "Ifraktur",
        width: 686,
    },
    GlyphWidth {
        name: "Iota",
        width: 333,
    },
    GlyphWidth {
        name: "Kappa",
        width: 722,
    },
    GlyphWidth {
        name: "Lambda",
        width: 686,
    },
    GlyphWidth {
        name: "Mu",
        width: 889,
    },
    GlyphWidth {
        name: "Nu",
        width: 722,
    },
    GlyphWidth {
        name: "Omega",
        width: 768,
    },
    GlyphWidth {
        name: "Omicron",
        width: 722,
    },
    GlyphWidth {
        name: "Phi",
        width: 763,
    },
    GlyphWidth {
        name: "Pi",
        width: 768,
    },
    GlyphWidth {
        name: "Psi",
        width: 795,
    },
    GlyphWidth {
        name: "Rfraktur",
        width: 795,
    },
    GlyphWidth {
        name: "Rho",
        width: 556,
    },
    GlyphWidth {
        name: "Sigma",
        width: 592,
    },
    GlyphWidth {
        name: "Tau",
        width: 611,
    },
    GlyphWidth {
        name: "Theta",
        width: 741,
    },
    GlyphWidth {
        name: "Upsilon",
        width: 690,
    },
    GlyphWidth {
        name: "Upsilon1",
        width: 620,
    },
    GlyphWidth {
        name: "Xi",
        width: 645,
    },
    GlyphWidth {
        name: "Zeta",
        width: 611,
    },
    GlyphWidth {
        name: "aleph",
        width: 823,
    },
    GlyphWidth {
        name: "alpha",
        width: 631,
    },
    GlyphWidth {
        name: "ampersand",
        width: 778,
    },
    GlyphWidth {
        name: "angle",
        width: 768,
    },
    GlyphWidth {
        name: "angleleft",
        width: 329,
    },
    GlyphWidth {
        name: "angleright",
        width: 329,
    },
    GlyphWidth {
        name: "apple",
        width: 790,
    },
    GlyphWidth {
        name: "approxequal",
        width: 549,
    },
    GlyphWidth {
        name: "arrowboth",
        width: 1042,
    },
    GlyphWidth {
        name: "arrowdblboth",
        width: 1042,
    },
    GlyphWidth {
        name: "arrowdbldown",
        width: 603,
    },
    GlyphWidth {
        name: "arrowdblleft",
        width: 987,
    },
    GlyphWidth {
        name: "arrowdblright",
        width: 987,
    },
    GlyphWidth {
        name: "arrowdblup",
        width: 603,
    },
    GlyphWidth {
        name: "arrowdown",
        width: 603,
    },
    GlyphWidth {
        name: "arrowhorizex",
        width: 1000,
    },
    GlyphWidth {
        name: "arrowleft",
        width: 987,
    },
    GlyphWidth {
        name: "arrowright",
        width: 987,
    },
    GlyphWidth {
        name: "arrowup",
        width: 603,
    },
    GlyphWidth {
        name: "arrowvertex",
        width: 603,
    },
    GlyphWidth {
        name: "asteriskmath",
        width: 500,
    },
    GlyphWidth {
        name: "bar",
        width: 200,
    },
    GlyphWidth {
        name: "beta",
        width: 549,
    },
    GlyphWidth {
        name: "braceex",
        width: 494,
    },
    GlyphWidth {
        name: "braceleft",
        width: 480,
    },
    GlyphWidth {
        name: "braceleftbt",
        width: 494,
    },
    GlyphWidth {
        name: "braceleftmid",
        width: 494,
    },
    GlyphWidth {
        name: "bracelefttp",
        width: 494,
    },
    GlyphWidth {
        name: "braceright",
        width: 480,
    },
    GlyphWidth {
        name: "bracerightbt",
        width: 494,
    },
    GlyphWidth {
        name: "bracerightmid",
        width: 494,
    },
    GlyphWidth {
        name: "bracerighttp",
        width: 494,
    },
    GlyphWidth {
        name: "bracketleft",
        width: 333,
    },
    GlyphWidth {
        name: "bracketleftbt",
        width: 384,
    },
    GlyphWidth {
        name: "bracketleftex",
        width: 384,
    },
    GlyphWidth {
        name: "bracketlefttp",
        width: 384,
    },
    GlyphWidth {
        name: "bracketright",
        width: 333,
    },
    GlyphWidth {
        name: "bracketrightbt",
        width: 384,
    },
    GlyphWidth {
        name: "bracketrightex",
        width: 384,
    },
    GlyphWidth {
        name: "bracketrighttp",
        width: 384,
    },
    GlyphWidth {
        name: "bullet",
        width: 460,
    },
    GlyphWidth {
        name: "carriagereturn",
        width: 658,
    },
    GlyphWidth {
        name: "chi",
        width: 549,
    },
    GlyphWidth {
        name: "circlemultiply",
        width: 768,
    },
    GlyphWidth {
        name: "circleplus",
        width: 768,
    },
    GlyphWidth {
        name: "club",
        width: 753,
    },
    GlyphWidth {
        name: "colon",
        width: 278,
    },
    GlyphWidth {
        name: "comma",
        width: 250,
    },
    GlyphWidth {
        name: "congruent",
        width: 549,
    },
    GlyphWidth {
        name: "copyrightsans",
        width: 790,
    },
    GlyphWidth {
        name: "copyrightserif",
        width: 790,
    },
    GlyphWidth {
        name: "degree",
        width: 400,
    },
    GlyphWidth {
        name: "delta",
        width: 494,
    },
    GlyphWidth {
        name: "diamond",
        width: 753,
    },
    GlyphWidth {
        name: "divide",
        width: 549,
    },
    GlyphWidth {
        name: "dotmath",
        width: 250,
    },
    GlyphWidth {
        name: "eight",
        width: 500,
    },
    GlyphWidth {
        name: "element",
        width: 713,
    },
    GlyphWidth {
        name: "ellipsis",
        width: 1000,
    },
    GlyphWidth {
        name: "emptyset",
        width: 823,
    },
    GlyphWidth {
        name: "epsilon",
        width: 439,
    },
    GlyphWidth {
        name: "equal",
        width: 549,
    },
    GlyphWidth {
        name: "equivalence",
        width: 549,
    },
    GlyphWidth {
        name: "eta",
        width: 603,
    },
    GlyphWidth {
        name: "exclam",
        width: 333,
    },
    GlyphWidth {
        name: "existential",
        width: 549,
    },
    GlyphWidth {
        name: "five",
        width: 500,
    },
    GlyphWidth {
        name: "florin",
        width: 500,
    },
    GlyphWidth {
        name: "four",
        width: 500,
    },
    GlyphWidth {
        name: "fraction",
        width: 167,
    },
    GlyphWidth {
        name: "gamma",
        width: 411,
    },
    GlyphWidth {
        name: "gradient",
        width: 713,
    },
    GlyphWidth {
        name: "greater",
        width: 549,
    },
    GlyphWidth {
        name: "greaterequal",
        width: 549,
    },
    GlyphWidth {
        name: "heart",
        width: 753,
    },
    GlyphWidth {
        name: "infinity",
        width: 713,
    },
    GlyphWidth {
        name: "integral",
        width: 274,
    },
    GlyphWidth {
        name: "integralbt",
        width: 686,
    },
    GlyphWidth {
        name: "integralex",
        width: 686,
    },
    GlyphWidth {
        name: "integraltp",
        width: 686,
    },
    GlyphWidth {
        name: "intersection",
        width: 768,
    },
    GlyphWidth {
        name: "iota",
        width: 329,
    },
    GlyphWidth {
        name: "kappa",
        width: 549,
    },
    GlyphWidth {
        name: "lambda",
        width: 549,
    },
    GlyphWidth {
        name: "less",
        width: 549,
    },
    GlyphWidth {
        name: "lessequal",
        width: 549,
    },
    GlyphWidth {
        name: "logicaland",
        width: 603,
    },
    GlyphWidth {
        name: "logicalnot",
        width: 713,
    },
    GlyphWidth {
        name: "logicalor",
        width: 603,
    },
    GlyphWidth {
        name: "lozenge",
        width: 494,
    },
    GlyphWidth {
        name: "minus",
        width: 549,
    },
    GlyphWidth {
        name: "minute",
        width: 247,
    },
    GlyphWidth {
        name: "mu",
        width: 576,
    },
    GlyphWidth {
        name: "multiply",
        width: 549,
    },
    GlyphWidth {
        name: "nine",
        width: 500,
    },
    GlyphWidth {
        name: "notelement",
        width: 713,
    },
    GlyphWidth {
        name: "notequal",
        width: 549,
    },
    GlyphWidth {
        name: "notsubset",
        width: 713,
    },
    GlyphWidth {
        name: "nu",
        width: 521,
    },
    GlyphWidth {
        name: "numbersign",
        width: 500,
    },
    GlyphWidth {
        name: "omega",
        width: 686,
    },
    GlyphWidth {
        name: "omega1",
        width: 713,
    },
    GlyphWidth {
        name: "omicron",
        width: 549,
    },
    GlyphWidth {
        name: "one",
        width: 500,
    },
    GlyphWidth {
        name: "parenleft",
        width: 333,
    },
    GlyphWidth {
        name: "parenleftbt",
        width: 384,
    },
    GlyphWidth {
        name: "parenleftex",
        width: 384,
    },
    GlyphWidth {
        name: "parenlefttp",
        width: 384,
    },
    GlyphWidth {
        name: "parenright",
        width: 333,
    },
    GlyphWidth {
        name: "parenrightbt",
        width: 384,
    },
    GlyphWidth {
        name: "parenrightex",
        width: 384,
    },
    GlyphWidth {
        name: "parenrighttp",
        width: 384,
    },
    GlyphWidth {
        name: "partialdiff",
        width: 494,
    },
    GlyphWidth {
        name: "percent",
        width: 833,
    },
    GlyphWidth {
        name: "period",
        width: 250,
    },
    GlyphWidth {
        name: "perpendicular",
        width: 658,
    },
    GlyphWidth {
        name: "phi",
        width: 521,
    },
    GlyphWidth {
        name: "phi1",
        width: 603,
    },
    GlyphWidth {
        name: "pi",
        width: 549,
    },
    GlyphWidth {
        name: "plus",
        width: 549,
    },
    GlyphWidth {
        name: "plusminus",
        width: 549,
    },
    GlyphWidth {
        name: "product",
        width: 823,
    },
    GlyphWidth {
        name: "propersubset",
        width: 713,
    },
    GlyphWidth {
        name: "propersuperset",
        width: 713,
    },
    GlyphWidth {
        name: "proportional",
        width: 713,
    },
    GlyphWidth {
        name: "psi",
        width: 686,
    },
    GlyphWidth {
        name: "question",
        width: 444,
    },
    GlyphWidth {
        name: "radical",
        width: 549,
    },
    GlyphWidth {
        name: "radicalex",
        width: 500,
    },
    GlyphWidth {
        name: "reflexsubset",
        width: 713,
    },
    GlyphWidth {
        name: "reflexsuperset",
        width: 713,
    },
    GlyphWidth {
        name: "registersans",
        width: 790,
    },
    GlyphWidth {
        name: "registerserif",
        width: 790,
    },
    GlyphWidth {
        name: "rho",
        width: 549,
    },
    GlyphWidth {
        name: "second",
        width: 411,
    },
    GlyphWidth {
        name: "semicolon",
        width: 278,
    },
    GlyphWidth {
        name: "seven",
        width: 500,
    },
    GlyphWidth {
        name: "sigma",
        width: 603,
    },
    GlyphWidth {
        name: "sigma1",
        width: 439,
    },
    GlyphWidth {
        name: "similar",
        width: 549,
    },
    GlyphWidth {
        name: "six",
        width: 500,
    },
    GlyphWidth {
        name: "slash",
        width: 278,
    },
    GlyphWidth {
        name: "space",
        width: 250,
    },
    GlyphWidth {
        name: "spade",
        width: 753,
    },
    GlyphWidth {
        name: "suchthat",
        width: 439,
    },
    GlyphWidth {
        name: "summation",
        width: 713,
    },
    GlyphWidth {
        name: "tau",
        width: 439,
    },
    GlyphWidth {
        name: "therefore",
        width: 863,
    },
    GlyphWidth {
        name: "theta",
        width: 521,
    },
    GlyphWidth {
        name: "theta1",
        width: 631,
    },
    GlyphWidth {
        name: "three",
        width: 500,
    },
    GlyphWidth {
        name: "trademarksans",
        width: 786,
    },
    GlyphWidth {
        name: "trademarkserif",
        width: 890,
    },
    GlyphWidth {
        name: "two",
        width: 500,
    },
    GlyphWidth {
        name: "underscore",
        width: 500,
    },
    GlyphWidth {
        name: "union",
        width: 768,
    },
    GlyphWidth {
        name: "universal",
        width: 713,
    },
    GlyphWidth {
        name: "upsilon",
        width: 576,
    },
    GlyphWidth {
        name: "weierstrass",
        width: 987,
    },
    GlyphWidth {
        name: "xi",
        width: 493,
    },
    GlyphWidth {
        name: "zero",
        width: 500,
    },
    GlyphWidth {
        name: "zeta",
        width: 494,
    },
];

/// `Times-Roman` — 315 glyphs.
pub const Times_Roman: &[GlyphWidth] = &[
    GlyphWidth {
        name: "A",
        width: 722,
    },
    GlyphWidth {
        name: "AE",
        width: 889,
    },
    GlyphWidth {
        name: "Aacute",
        width: 722,
    },
    GlyphWidth {
        name: "Abreve",
        width: 722,
    },
    GlyphWidth {
        name: "Acircumflex",
        width: 722,
    },
    GlyphWidth {
        name: "Adieresis",
        width: 722,
    },
    GlyphWidth {
        name: "Agrave",
        width: 722,
    },
    GlyphWidth {
        name: "Amacron",
        width: 722,
    },
    GlyphWidth {
        name: "Aogonek",
        width: 722,
    },
    GlyphWidth {
        name: "Aring",
        width: 722,
    },
    GlyphWidth {
        name: "Atilde",
        width: 722,
    },
    GlyphWidth {
        name: "B",
        width: 667,
    },
    GlyphWidth {
        name: "C",
        width: 667,
    },
    GlyphWidth {
        name: "Cacute",
        width: 667,
    },
    GlyphWidth {
        name: "Ccaron",
        width: 667,
    },
    GlyphWidth {
        name: "Ccedilla",
        width: 667,
    },
    GlyphWidth {
        name: "D",
        width: 722,
    },
    GlyphWidth {
        name: "Dcaron",
        width: 722,
    },
    GlyphWidth {
        name: "Dcroat",
        width: 722,
    },
    GlyphWidth {
        name: "Delta",
        width: 612,
    },
    GlyphWidth {
        name: "E",
        width: 611,
    },
    GlyphWidth {
        name: "Eacute",
        width: 611,
    },
    GlyphWidth {
        name: "Ecaron",
        width: 611,
    },
    GlyphWidth {
        name: "Ecircumflex",
        width: 611,
    },
    GlyphWidth {
        name: "Edieresis",
        width: 611,
    },
    GlyphWidth {
        name: "Edotaccent",
        width: 611,
    },
    GlyphWidth {
        name: "Egrave",
        width: 611,
    },
    GlyphWidth {
        name: "Emacron",
        width: 611,
    },
    GlyphWidth {
        name: "Eogonek",
        width: 611,
    },
    GlyphWidth {
        name: "Eth",
        width: 722,
    },
    GlyphWidth {
        name: "Euro",
        width: 500,
    },
    GlyphWidth {
        name: "F",
        width: 556,
    },
    GlyphWidth {
        name: "G",
        width: 722,
    },
    GlyphWidth {
        name: "Gbreve",
        width: 722,
    },
    GlyphWidth {
        name: "Gcommaaccent",
        width: 722,
    },
    GlyphWidth {
        name: "H",
        width: 722,
    },
    GlyphWidth {
        name: "I",
        width: 333,
    },
    GlyphWidth {
        name: "Iacute",
        width: 333,
    },
    GlyphWidth {
        name: "Icircumflex",
        width: 333,
    },
    GlyphWidth {
        name: "Idieresis",
        width: 333,
    },
    GlyphWidth {
        name: "Idotaccent",
        width: 333,
    },
    GlyphWidth {
        name: "Igrave",
        width: 333,
    },
    GlyphWidth {
        name: "Imacron",
        width: 333,
    },
    GlyphWidth {
        name: "Iogonek",
        width: 333,
    },
    GlyphWidth {
        name: "J",
        width: 389,
    },
    GlyphWidth {
        name: "K",
        width: 722,
    },
    GlyphWidth {
        name: "Kcommaaccent",
        width: 722,
    },
    GlyphWidth {
        name: "L",
        width: 611,
    },
    GlyphWidth {
        name: "Lacute",
        width: 611,
    },
    GlyphWidth {
        name: "Lcaron",
        width: 611,
    },
    GlyphWidth {
        name: "Lcommaaccent",
        width: 611,
    },
    GlyphWidth {
        name: "Lslash",
        width: 611,
    },
    GlyphWidth {
        name: "M",
        width: 889,
    },
    GlyphWidth {
        name: "N",
        width: 722,
    },
    GlyphWidth {
        name: "Nacute",
        width: 722,
    },
    GlyphWidth {
        name: "Ncaron",
        width: 722,
    },
    GlyphWidth {
        name: "Ncommaaccent",
        width: 722,
    },
    GlyphWidth {
        name: "Ntilde",
        width: 722,
    },
    GlyphWidth {
        name: "O",
        width: 722,
    },
    GlyphWidth {
        name: "OE",
        width: 889,
    },
    GlyphWidth {
        name: "Oacute",
        width: 722,
    },
    GlyphWidth {
        name: "Ocircumflex",
        width: 722,
    },
    GlyphWidth {
        name: "Odieresis",
        width: 722,
    },
    GlyphWidth {
        name: "Ograve",
        width: 722,
    },
    GlyphWidth {
        name: "Ohungarumlaut",
        width: 722,
    },
    GlyphWidth {
        name: "Omacron",
        width: 722,
    },
    GlyphWidth {
        name: "Oslash",
        width: 722,
    },
    GlyphWidth {
        name: "Otilde",
        width: 722,
    },
    GlyphWidth {
        name: "P",
        width: 556,
    },
    GlyphWidth {
        name: "Q",
        width: 722,
    },
    GlyphWidth {
        name: "R",
        width: 667,
    },
    GlyphWidth {
        name: "Racute",
        width: 667,
    },
    GlyphWidth {
        name: "Rcaron",
        width: 667,
    },
    GlyphWidth {
        name: "Rcommaaccent",
        width: 667,
    },
    GlyphWidth {
        name: "S",
        width: 556,
    },
    GlyphWidth {
        name: "Sacute",
        width: 556,
    },
    GlyphWidth {
        name: "Scaron",
        width: 556,
    },
    GlyphWidth {
        name: "Scedilla",
        width: 556,
    },
    GlyphWidth {
        name: "Scommaaccent",
        width: 556,
    },
    GlyphWidth {
        name: "T",
        width: 611,
    },
    GlyphWidth {
        name: "Tcaron",
        width: 611,
    },
    GlyphWidth {
        name: "Tcommaaccent",
        width: 611,
    },
    GlyphWidth {
        name: "Thorn",
        width: 556,
    },
    GlyphWidth {
        name: "U",
        width: 722,
    },
    GlyphWidth {
        name: "Uacute",
        width: 722,
    },
    GlyphWidth {
        name: "Ucircumflex",
        width: 722,
    },
    GlyphWidth {
        name: "Udieresis",
        width: 722,
    },
    GlyphWidth {
        name: "Ugrave",
        width: 722,
    },
    GlyphWidth {
        name: "Uhungarumlaut",
        width: 722,
    },
    GlyphWidth {
        name: "Umacron",
        width: 722,
    },
    GlyphWidth {
        name: "Uogonek",
        width: 722,
    },
    GlyphWidth {
        name: "Uring",
        width: 722,
    },
    GlyphWidth {
        name: "V",
        width: 722,
    },
    GlyphWidth {
        name: "W",
        width: 944,
    },
    GlyphWidth {
        name: "X",
        width: 722,
    },
    GlyphWidth {
        name: "Y",
        width: 722,
    },
    GlyphWidth {
        name: "Yacute",
        width: 722,
    },
    GlyphWidth {
        name: "Ydieresis",
        width: 722,
    },
    GlyphWidth {
        name: "Z",
        width: 611,
    },
    GlyphWidth {
        name: "Zacute",
        width: 611,
    },
    GlyphWidth {
        name: "Zcaron",
        width: 611,
    },
    GlyphWidth {
        name: "Zdotaccent",
        width: 611,
    },
    GlyphWidth {
        name: "a",
        width: 444,
    },
    GlyphWidth {
        name: "aacute",
        width: 444,
    },
    GlyphWidth {
        name: "abreve",
        width: 444,
    },
    GlyphWidth {
        name: "acircumflex",
        width: 444,
    },
    GlyphWidth {
        name: "acute",
        width: 333,
    },
    GlyphWidth {
        name: "adieresis",
        width: 444,
    },
    GlyphWidth {
        name: "ae",
        width: 667,
    },
    GlyphWidth {
        name: "agrave",
        width: 444,
    },
    GlyphWidth {
        name: "amacron",
        width: 444,
    },
    GlyphWidth {
        name: "ampersand",
        width: 778,
    },
    GlyphWidth {
        name: "aogonek",
        width: 444,
    },
    GlyphWidth {
        name: "aring",
        width: 444,
    },
    GlyphWidth {
        name: "asciicircum",
        width: 469,
    },
    GlyphWidth {
        name: "asciitilde",
        width: 541,
    },
    GlyphWidth {
        name: "asterisk",
        width: 500,
    },
    GlyphWidth {
        name: "at",
        width: 921,
    },
    GlyphWidth {
        name: "atilde",
        width: 444,
    },
    GlyphWidth {
        name: "b",
        width: 500,
    },
    GlyphWidth {
        name: "backslash",
        width: 278,
    },
    GlyphWidth {
        name: "bar",
        width: 200,
    },
    GlyphWidth {
        name: "braceleft",
        width: 480,
    },
    GlyphWidth {
        name: "braceright",
        width: 480,
    },
    GlyphWidth {
        name: "bracketleft",
        width: 333,
    },
    GlyphWidth {
        name: "bracketright",
        width: 333,
    },
    GlyphWidth {
        name: "breve",
        width: 333,
    },
    GlyphWidth {
        name: "brokenbar",
        width: 200,
    },
    GlyphWidth {
        name: "bullet",
        width: 350,
    },
    GlyphWidth {
        name: "c",
        width: 444,
    },
    GlyphWidth {
        name: "cacute",
        width: 444,
    },
    GlyphWidth {
        name: "caron",
        width: 333,
    },
    GlyphWidth {
        name: "ccaron",
        width: 444,
    },
    GlyphWidth {
        name: "ccedilla",
        width: 444,
    },
    GlyphWidth {
        name: "cedilla",
        width: 333,
    },
    GlyphWidth {
        name: "cent",
        width: 500,
    },
    GlyphWidth {
        name: "circumflex",
        width: 333,
    },
    GlyphWidth {
        name: "colon",
        width: 278,
    },
    GlyphWidth {
        name: "comma",
        width: 250,
    },
    GlyphWidth {
        name: "commaaccent",
        width: 250,
    },
    GlyphWidth {
        name: "copyright",
        width: 760,
    },
    GlyphWidth {
        name: "currency",
        width: 500,
    },
    GlyphWidth {
        name: "d",
        width: 500,
    },
    GlyphWidth {
        name: "dagger",
        width: 500,
    },
    GlyphWidth {
        name: "daggerdbl",
        width: 500,
    },
    GlyphWidth {
        name: "dcaron",
        width: 588,
    },
    GlyphWidth {
        name: "dcroat",
        width: 500,
    },
    GlyphWidth {
        name: "degree",
        width: 400,
    },
    GlyphWidth {
        name: "dieresis",
        width: 333,
    },
    GlyphWidth {
        name: "divide",
        width: 564,
    },
    GlyphWidth {
        name: "dollar",
        width: 500,
    },
    GlyphWidth {
        name: "dotaccent",
        width: 333,
    },
    GlyphWidth {
        name: "dotlessi",
        width: 278,
    },
    GlyphWidth {
        name: "e",
        width: 444,
    },
    GlyphWidth {
        name: "eacute",
        width: 444,
    },
    GlyphWidth {
        name: "ecaron",
        width: 444,
    },
    GlyphWidth {
        name: "ecircumflex",
        width: 444,
    },
    GlyphWidth {
        name: "edieresis",
        width: 444,
    },
    GlyphWidth {
        name: "edotaccent",
        width: 444,
    },
    GlyphWidth {
        name: "egrave",
        width: 444,
    },
    GlyphWidth {
        name: "eight",
        width: 500,
    },
    GlyphWidth {
        name: "ellipsis",
        width: 1000,
    },
    GlyphWidth {
        name: "emacron",
        width: 444,
    },
    GlyphWidth {
        name: "emdash",
        width: 1000,
    },
    GlyphWidth {
        name: "endash",
        width: 500,
    },
    GlyphWidth {
        name: "eogonek",
        width: 444,
    },
    GlyphWidth {
        name: "equal",
        width: 564,
    },
    GlyphWidth {
        name: "eth",
        width: 500,
    },
    GlyphWidth {
        name: "exclam",
        width: 333,
    },
    GlyphWidth {
        name: "exclamdown",
        width: 333,
    },
    GlyphWidth {
        name: "f",
        width: 333,
    },
    GlyphWidth {
        name: "fi",
        width: 556,
    },
    GlyphWidth {
        name: "five",
        width: 500,
    },
    GlyphWidth {
        name: "fl",
        width: 556,
    },
    GlyphWidth {
        name: "florin",
        width: 500,
    },
    GlyphWidth {
        name: "four",
        width: 500,
    },
    GlyphWidth {
        name: "fraction",
        width: 167,
    },
    GlyphWidth {
        name: "g",
        width: 500,
    },
    GlyphWidth {
        name: "gbreve",
        width: 500,
    },
    GlyphWidth {
        name: "gcommaaccent",
        width: 500,
    },
    GlyphWidth {
        name: "germandbls",
        width: 500,
    },
    GlyphWidth {
        name: "grave",
        width: 333,
    },
    GlyphWidth {
        name: "greater",
        width: 564,
    },
    GlyphWidth {
        name: "greaterequal",
        width: 549,
    },
    GlyphWidth {
        name: "guillemotleft",
        width: 500,
    },
    GlyphWidth {
        name: "guillemotright",
        width: 500,
    },
    GlyphWidth {
        name: "guilsinglleft",
        width: 333,
    },
    GlyphWidth {
        name: "guilsinglright",
        width: 333,
    },
    GlyphWidth {
        name: "h",
        width: 500,
    },
    GlyphWidth {
        name: "hungarumlaut",
        width: 333,
    },
    GlyphWidth {
        name: "hyphen",
        width: 333,
    },
    GlyphWidth {
        name: "i",
        width: 278,
    },
    GlyphWidth {
        name: "iacute",
        width: 278,
    },
    GlyphWidth {
        name: "icircumflex",
        width: 278,
    },
    GlyphWidth {
        name: "idieresis",
        width: 278,
    },
    GlyphWidth {
        name: "igrave",
        width: 278,
    },
    GlyphWidth {
        name: "imacron",
        width: 278,
    },
    GlyphWidth {
        name: "iogonek",
        width: 278,
    },
    GlyphWidth {
        name: "j",
        width: 278,
    },
    GlyphWidth {
        name: "k",
        width: 500,
    },
    GlyphWidth {
        name: "kcommaaccent",
        width: 500,
    },
    GlyphWidth {
        name: "l",
        width: 278,
    },
    GlyphWidth {
        name: "lacute",
        width: 278,
    },
    GlyphWidth {
        name: "lcaron",
        width: 344,
    },
    GlyphWidth {
        name: "lcommaaccent",
        width: 278,
    },
    GlyphWidth {
        name: "less",
        width: 564,
    },
    GlyphWidth {
        name: "lessequal",
        width: 549,
    },
    GlyphWidth {
        name: "logicalnot",
        width: 564,
    },
    GlyphWidth {
        name: "lozenge",
        width: 471,
    },
    GlyphWidth {
        name: "lslash",
        width: 278,
    },
    GlyphWidth {
        name: "m",
        width: 778,
    },
    GlyphWidth {
        name: "macron",
        width: 333,
    },
    GlyphWidth {
        name: "minus",
        width: 564,
    },
    GlyphWidth {
        name: "mu",
        width: 500,
    },
    GlyphWidth {
        name: "multiply",
        width: 564,
    },
    GlyphWidth {
        name: "n",
        width: 500,
    },
    GlyphWidth {
        name: "nacute",
        width: 500,
    },
    GlyphWidth {
        name: "ncaron",
        width: 500,
    },
    GlyphWidth {
        name: "ncommaaccent",
        width: 500,
    },
    GlyphWidth {
        name: "nine",
        width: 500,
    },
    GlyphWidth {
        name: "notequal",
        width: 549,
    },
    GlyphWidth {
        name: "ntilde",
        width: 500,
    },
    GlyphWidth {
        name: "numbersign",
        width: 500,
    },
    GlyphWidth {
        name: "o",
        width: 500,
    },
    GlyphWidth {
        name: "oacute",
        width: 500,
    },
    GlyphWidth {
        name: "ocircumflex",
        width: 500,
    },
    GlyphWidth {
        name: "odieresis",
        width: 500,
    },
    GlyphWidth {
        name: "oe",
        width: 722,
    },
    GlyphWidth {
        name: "ogonek",
        width: 333,
    },
    GlyphWidth {
        name: "ograve",
        width: 500,
    },
    GlyphWidth {
        name: "ohungarumlaut",
        width: 500,
    },
    GlyphWidth {
        name: "omacron",
        width: 500,
    },
    GlyphWidth {
        name: "one",
        width: 500,
    },
    GlyphWidth {
        name: "onehalf",
        width: 750,
    },
    GlyphWidth {
        name: "onequarter",
        width: 750,
    },
    GlyphWidth {
        name: "onesuperior",
        width: 300,
    },
    GlyphWidth {
        name: "ordfeminine",
        width: 276,
    },
    GlyphWidth {
        name: "ordmasculine",
        width: 310,
    },
    GlyphWidth {
        name: "oslash",
        width: 500,
    },
    GlyphWidth {
        name: "otilde",
        width: 500,
    },
    GlyphWidth {
        name: "p",
        width: 500,
    },
    GlyphWidth {
        name: "paragraph",
        width: 453,
    },
    GlyphWidth {
        name: "parenleft",
        width: 333,
    },
    GlyphWidth {
        name: "parenright",
        width: 333,
    },
    GlyphWidth {
        name: "partialdiff",
        width: 476,
    },
    GlyphWidth {
        name: "percent",
        width: 833,
    },
    GlyphWidth {
        name: "period",
        width: 250,
    },
    GlyphWidth {
        name: "periodcentered",
        width: 250,
    },
    GlyphWidth {
        name: "perthousand",
        width: 1000,
    },
    GlyphWidth {
        name: "plus",
        width: 564,
    },
    GlyphWidth {
        name: "plusminus",
        width: 564,
    },
    GlyphWidth {
        name: "q",
        width: 500,
    },
    GlyphWidth {
        name: "question",
        width: 444,
    },
    GlyphWidth {
        name: "questiondown",
        width: 444,
    },
    GlyphWidth {
        name: "quotedbl",
        width: 408,
    },
    GlyphWidth {
        name: "quotedblbase",
        width: 444,
    },
    GlyphWidth {
        name: "quotedblleft",
        width: 444,
    },
    GlyphWidth {
        name: "quotedblright",
        width: 444,
    },
    GlyphWidth {
        name: "quoteleft",
        width: 333,
    },
    GlyphWidth {
        name: "quoteright",
        width: 333,
    },
    GlyphWidth {
        name: "quotesinglbase",
        width: 333,
    },
    GlyphWidth {
        name: "quotesingle",
        width: 180,
    },
    GlyphWidth {
        name: "r",
        width: 333,
    },
    GlyphWidth {
        name: "racute",
        width: 333,
    },
    GlyphWidth {
        name: "radical",
        width: 453,
    },
    GlyphWidth {
        name: "rcaron",
        width: 333,
    },
    GlyphWidth {
        name: "rcommaaccent",
        width: 333,
    },
    GlyphWidth {
        name: "registered",
        width: 760,
    },
    GlyphWidth {
        name: "ring",
        width: 333,
    },
    GlyphWidth {
        name: "s",
        width: 389,
    },
    GlyphWidth {
        name: "sacute",
        width: 389,
    },
    GlyphWidth {
        name: "scaron",
        width: 389,
    },
    GlyphWidth {
        name: "scedilla",
        width: 389,
    },
    GlyphWidth {
        name: "scommaaccent",
        width: 389,
    },
    GlyphWidth {
        name: "section",
        width: 500,
    },
    GlyphWidth {
        name: "semicolon",
        width: 278,
    },
    GlyphWidth {
        name: "seven",
        width: 500,
    },
    GlyphWidth {
        name: "six",
        width: 500,
    },
    GlyphWidth {
        name: "slash",
        width: 278,
    },
    GlyphWidth {
        name: "space",
        width: 250,
    },
    GlyphWidth {
        name: "sterling",
        width: 500,
    },
    GlyphWidth {
        name: "summation",
        width: 600,
    },
    GlyphWidth {
        name: "t",
        width: 278,
    },
    GlyphWidth {
        name: "tcaron",
        width: 326,
    },
    GlyphWidth {
        name: "tcommaaccent",
        width: 278,
    },
    GlyphWidth {
        name: "thorn",
        width: 500,
    },
    GlyphWidth {
        name: "three",
        width: 500,
    },
    GlyphWidth {
        name: "threequarters",
        width: 750,
    },
    GlyphWidth {
        name: "threesuperior",
        width: 300,
    },
    GlyphWidth {
        name: "tilde",
        width: 333,
    },
    GlyphWidth {
        name: "trademark",
        width: 980,
    },
    GlyphWidth {
        name: "two",
        width: 500,
    },
    GlyphWidth {
        name: "twosuperior",
        width: 300,
    },
    GlyphWidth {
        name: "u",
        width: 500,
    },
    GlyphWidth {
        name: "uacute",
        width: 500,
    },
    GlyphWidth {
        name: "ucircumflex",
        width: 500,
    },
    GlyphWidth {
        name: "udieresis",
        width: 500,
    },
    GlyphWidth {
        name: "ugrave",
        width: 500,
    },
    GlyphWidth {
        name: "uhungarumlaut",
        width: 500,
    },
    GlyphWidth {
        name: "umacron",
        width: 500,
    },
    GlyphWidth {
        name: "underscore",
        width: 500,
    },
    GlyphWidth {
        name: "uogonek",
        width: 500,
    },
    GlyphWidth {
        name: "uring",
        width: 500,
    },
    GlyphWidth {
        name: "v",
        width: 500,
    },
    GlyphWidth {
        name: "w",
        width: 722,
    },
    GlyphWidth {
        name: "x",
        width: 500,
    },
    GlyphWidth {
        name: "y",
        width: 500,
    },
    GlyphWidth {
        name: "yacute",
        width: 500,
    },
    GlyphWidth {
        name: "ydieresis",
        width: 500,
    },
    GlyphWidth {
        name: "yen",
        width: 500,
    },
    GlyphWidth {
        name: "z",
        width: 444,
    },
    GlyphWidth {
        name: "zacute",
        width: 444,
    },
    GlyphWidth {
        name: "zcaron",
        width: 444,
    },
    GlyphWidth {
        name: "zdotaccent",
        width: 444,
    },
    GlyphWidth {
        name: "zero",
        width: 500,
    },
];

/// `Times-Bold` — 315 glyphs.
pub const Times_Bold: &[GlyphWidth] = &[
    GlyphWidth {
        name: "A",
        width: 722,
    },
    GlyphWidth {
        name: "AE",
        width: 1000,
    },
    GlyphWidth {
        name: "Aacute",
        width: 722,
    },
    GlyphWidth {
        name: "Abreve",
        width: 722,
    },
    GlyphWidth {
        name: "Acircumflex",
        width: 722,
    },
    GlyphWidth {
        name: "Adieresis",
        width: 722,
    },
    GlyphWidth {
        name: "Agrave",
        width: 722,
    },
    GlyphWidth {
        name: "Amacron",
        width: 722,
    },
    GlyphWidth {
        name: "Aogonek",
        width: 722,
    },
    GlyphWidth {
        name: "Aring",
        width: 722,
    },
    GlyphWidth {
        name: "Atilde",
        width: 722,
    },
    GlyphWidth {
        name: "B",
        width: 667,
    },
    GlyphWidth {
        name: "C",
        width: 722,
    },
    GlyphWidth {
        name: "Cacute",
        width: 722,
    },
    GlyphWidth {
        name: "Ccaron",
        width: 722,
    },
    GlyphWidth {
        name: "Ccedilla",
        width: 722,
    },
    GlyphWidth {
        name: "D",
        width: 722,
    },
    GlyphWidth {
        name: "Dcaron",
        width: 722,
    },
    GlyphWidth {
        name: "Dcroat",
        width: 722,
    },
    GlyphWidth {
        name: "Delta",
        width: 612,
    },
    GlyphWidth {
        name: "E",
        width: 667,
    },
    GlyphWidth {
        name: "Eacute",
        width: 667,
    },
    GlyphWidth {
        name: "Ecaron",
        width: 667,
    },
    GlyphWidth {
        name: "Ecircumflex",
        width: 667,
    },
    GlyphWidth {
        name: "Edieresis",
        width: 667,
    },
    GlyphWidth {
        name: "Edotaccent",
        width: 667,
    },
    GlyphWidth {
        name: "Egrave",
        width: 667,
    },
    GlyphWidth {
        name: "Emacron",
        width: 667,
    },
    GlyphWidth {
        name: "Eogonek",
        width: 667,
    },
    GlyphWidth {
        name: "Eth",
        width: 722,
    },
    GlyphWidth {
        name: "Euro",
        width: 500,
    },
    GlyphWidth {
        name: "F",
        width: 611,
    },
    GlyphWidth {
        name: "G",
        width: 778,
    },
    GlyphWidth {
        name: "Gbreve",
        width: 778,
    },
    GlyphWidth {
        name: "Gcommaaccent",
        width: 778,
    },
    GlyphWidth {
        name: "H",
        width: 778,
    },
    GlyphWidth {
        name: "I",
        width: 389,
    },
    GlyphWidth {
        name: "Iacute",
        width: 389,
    },
    GlyphWidth {
        name: "Icircumflex",
        width: 389,
    },
    GlyphWidth {
        name: "Idieresis",
        width: 389,
    },
    GlyphWidth {
        name: "Idotaccent",
        width: 389,
    },
    GlyphWidth {
        name: "Igrave",
        width: 389,
    },
    GlyphWidth {
        name: "Imacron",
        width: 389,
    },
    GlyphWidth {
        name: "Iogonek",
        width: 389,
    },
    GlyphWidth {
        name: "J",
        width: 500,
    },
    GlyphWidth {
        name: "K",
        width: 778,
    },
    GlyphWidth {
        name: "Kcommaaccent",
        width: 778,
    },
    GlyphWidth {
        name: "L",
        width: 667,
    },
    GlyphWidth {
        name: "Lacute",
        width: 667,
    },
    GlyphWidth {
        name: "Lcaron",
        width: 667,
    },
    GlyphWidth {
        name: "Lcommaaccent",
        width: 667,
    },
    GlyphWidth {
        name: "Lslash",
        width: 667,
    },
    GlyphWidth {
        name: "M",
        width: 944,
    },
    GlyphWidth {
        name: "N",
        width: 722,
    },
    GlyphWidth {
        name: "Nacute",
        width: 722,
    },
    GlyphWidth {
        name: "Ncaron",
        width: 722,
    },
    GlyphWidth {
        name: "Ncommaaccent",
        width: 722,
    },
    GlyphWidth {
        name: "Ntilde",
        width: 722,
    },
    GlyphWidth {
        name: "O",
        width: 778,
    },
    GlyphWidth {
        name: "OE",
        width: 1000,
    },
    GlyphWidth {
        name: "Oacute",
        width: 778,
    },
    GlyphWidth {
        name: "Ocircumflex",
        width: 778,
    },
    GlyphWidth {
        name: "Odieresis",
        width: 778,
    },
    GlyphWidth {
        name: "Ograve",
        width: 778,
    },
    GlyphWidth {
        name: "Ohungarumlaut",
        width: 778,
    },
    GlyphWidth {
        name: "Omacron",
        width: 778,
    },
    GlyphWidth {
        name: "Oslash",
        width: 778,
    },
    GlyphWidth {
        name: "Otilde",
        width: 778,
    },
    GlyphWidth {
        name: "P",
        width: 611,
    },
    GlyphWidth {
        name: "Q",
        width: 778,
    },
    GlyphWidth {
        name: "R",
        width: 722,
    },
    GlyphWidth {
        name: "Racute",
        width: 722,
    },
    GlyphWidth {
        name: "Rcaron",
        width: 722,
    },
    GlyphWidth {
        name: "Rcommaaccent",
        width: 722,
    },
    GlyphWidth {
        name: "S",
        width: 556,
    },
    GlyphWidth {
        name: "Sacute",
        width: 556,
    },
    GlyphWidth {
        name: "Scaron",
        width: 556,
    },
    GlyphWidth {
        name: "Scedilla",
        width: 556,
    },
    GlyphWidth {
        name: "Scommaaccent",
        width: 556,
    },
    GlyphWidth {
        name: "T",
        width: 667,
    },
    GlyphWidth {
        name: "Tcaron",
        width: 667,
    },
    GlyphWidth {
        name: "Tcommaaccent",
        width: 667,
    },
    GlyphWidth {
        name: "Thorn",
        width: 611,
    },
    GlyphWidth {
        name: "U",
        width: 722,
    },
    GlyphWidth {
        name: "Uacute",
        width: 722,
    },
    GlyphWidth {
        name: "Ucircumflex",
        width: 722,
    },
    GlyphWidth {
        name: "Udieresis",
        width: 722,
    },
    GlyphWidth {
        name: "Ugrave",
        width: 722,
    },
    GlyphWidth {
        name: "Uhungarumlaut",
        width: 722,
    },
    GlyphWidth {
        name: "Umacron",
        width: 722,
    },
    GlyphWidth {
        name: "Uogonek",
        width: 722,
    },
    GlyphWidth {
        name: "Uring",
        width: 722,
    },
    GlyphWidth {
        name: "V",
        width: 722,
    },
    GlyphWidth {
        name: "W",
        width: 1000,
    },
    GlyphWidth {
        name: "X",
        width: 722,
    },
    GlyphWidth {
        name: "Y",
        width: 722,
    },
    GlyphWidth {
        name: "Yacute",
        width: 722,
    },
    GlyphWidth {
        name: "Ydieresis",
        width: 722,
    },
    GlyphWidth {
        name: "Z",
        width: 667,
    },
    GlyphWidth {
        name: "Zacute",
        width: 667,
    },
    GlyphWidth {
        name: "Zcaron",
        width: 667,
    },
    GlyphWidth {
        name: "Zdotaccent",
        width: 667,
    },
    GlyphWidth {
        name: "a",
        width: 500,
    },
    GlyphWidth {
        name: "aacute",
        width: 500,
    },
    GlyphWidth {
        name: "abreve",
        width: 500,
    },
    GlyphWidth {
        name: "acircumflex",
        width: 500,
    },
    GlyphWidth {
        name: "acute",
        width: 333,
    },
    GlyphWidth {
        name: "adieresis",
        width: 500,
    },
    GlyphWidth {
        name: "ae",
        width: 722,
    },
    GlyphWidth {
        name: "agrave",
        width: 500,
    },
    GlyphWidth {
        name: "amacron",
        width: 500,
    },
    GlyphWidth {
        name: "ampersand",
        width: 833,
    },
    GlyphWidth {
        name: "aogonek",
        width: 500,
    },
    GlyphWidth {
        name: "aring",
        width: 500,
    },
    GlyphWidth {
        name: "asciicircum",
        width: 581,
    },
    GlyphWidth {
        name: "asciitilde",
        width: 520,
    },
    GlyphWidth {
        name: "asterisk",
        width: 500,
    },
    GlyphWidth {
        name: "at",
        width: 930,
    },
    GlyphWidth {
        name: "atilde",
        width: 500,
    },
    GlyphWidth {
        name: "b",
        width: 556,
    },
    GlyphWidth {
        name: "backslash",
        width: 278,
    },
    GlyphWidth {
        name: "bar",
        width: 220,
    },
    GlyphWidth {
        name: "braceleft",
        width: 394,
    },
    GlyphWidth {
        name: "braceright",
        width: 394,
    },
    GlyphWidth {
        name: "bracketleft",
        width: 333,
    },
    GlyphWidth {
        name: "bracketright",
        width: 333,
    },
    GlyphWidth {
        name: "breve",
        width: 333,
    },
    GlyphWidth {
        name: "brokenbar",
        width: 220,
    },
    GlyphWidth {
        name: "bullet",
        width: 350,
    },
    GlyphWidth {
        name: "c",
        width: 444,
    },
    GlyphWidth {
        name: "cacute",
        width: 444,
    },
    GlyphWidth {
        name: "caron",
        width: 333,
    },
    GlyphWidth {
        name: "ccaron",
        width: 444,
    },
    GlyphWidth {
        name: "ccedilla",
        width: 444,
    },
    GlyphWidth {
        name: "cedilla",
        width: 333,
    },
    GlyphWidth {
        name: "cent",
        width: 500,
    },
    GlyphWidth {
        name: "circumflex",
        width: 333,
    },
    GlyphWidth {
        name: "colon",
        width: 333,
    },
    GlyphWidth {
        name: "comma",
        width: 250,
    },
    GlyphWidth {
        name: "commaaccent",
        width: 250,
    },
    GlyphWidth {
        name: "copyright",
        width: 747,
    },
    GlyphWidth {
        name: "currency",
        width: 500,
    },
    GlyphWidth {
        name: "d",
        width: 556,
    },
    GlyphWidth {
        name: "dagger",
        width: 500,
    },
    GlyphWidth {
        name: "daggerdbl",
        width: 500,
    },
    GlyphWidth {
        name: "dcaron",
        width: 672,
    },
    GlyphWidth {
        name: "dcroat",
        width: 556,
    },
    GlyphWidth {
        name: "degree",
        width: 400,
    },
    GlyphWidth {
        name: "dieresis",
        width: 333,
    },
    GlyphWidth {
        name: "divide",
        width: 570,
    },
    GlyphWidth {
        name: "dollar",
        width: 500,
    },
    GlyphWidth {
        name: "dotaccent",
        width: 333,
    },
    GlyphWidth {
        name: "dotlessi",
        width: 278,
    },
    GlyphWidth {
        name: "e",
        width: 444,
    },
    GlyphWidth {
        name: "eacute",
        width: 444,
    },
    GlyphWidth {
        name: "ecaron",
        width: 444,
    },
    GlyphWidth {
        name: "ecircumflex",
        width: 444,
    },
    GlyphWidth {
        name: "edieresis",
        width: 444,
    },
    GlyphWidth {
        name: "edotaccent",
        width: 444,
    },
    GlyphWidth {
        name: "egrave",
        width: 444,
    },
    GlyphWidth {
        name: "eight",
        width: 500,
    },
    GlyphWidth {
        name: "ellipsis",
        width: 1000,
    },
    GlyphWidth {
        name: "emacron",
        width: 444,
    },
    GlyphWidth {
        name: "emdash",
        width: 1000,
    },
    GlyphWidth {
        name: "endash",
        width: 500,
    },
    GlyphWidth {
        name: "eogonek",
        width: 444,
    },
    GlyphWidth {
        name: "equal",
        width: 570,
    },
    GlyphWidth {
        name: "eth",
        width: 500,
    },
    GlyphWidth {
        name: "exclam",
        width: 333,
    },
    GlyphWidth {
        name: "exclamdown",
        width: 333,
    },
    GlyphWidth {
        name: "f",
        width: 333,
    },
    GlyphWidth {
        name: "fi",
        width: 556,
    },
    GlyphWidth {
        name: "five",
        width: 500,
    },
    GlyphWidth {
        name: "fl",
        width: 556,
    },
    GlyphWidth {
        name: "florin",
        width: 500,
    },
    GlyphWidth {
        name: "four",
        width: 500,
    },
    GlyphWidth {
        name: "fraction",
        width: 167,
    },
    GlyphWidth {
        name: "g",
        width: 500,
    },
    GlyphWidth {
        name: "gbreve",
        width: 500,
    },
    GlyphWidth {
        name: "gcommaaccent",
        width: 500,
    },
    GlyphWidth {
        name: "germandbls",
        width: 556,
    },
    GlyphWidth {
        name: "grave",
        width: 333,
    },
    GlyphWidth {
        name: "greater",
        width: 570,
    },
    GlyphWidth {
        name: "greaterequal",
        width: 549,
    },
    GlyphWidth {
        name: "guillemotleft",
        width: 500,
    },
    GlyphWidth {
        name: "guillemotright",
        width: 500,
    },
    GlyphWidth {
        name: "guilsinglleft",
        width: 333,
    },
    GlyphWidth {
        name: "guilsinglright",
        width: 333,
    },
    GlyphWidth {
        name: "h",
        width: 556,
    },
    GlyphWidth {
        name: "hungarumlaut",
        width: 333,
    },
    GlyphWidth {
        name: "hyphen",
        width: 333,
    },
    GlyphWidth {
        name: "i",
        width: 278,
    },
    GlyphWidth {
        name: "iacute",
        width: 278,
    },
    GlyphWidth {
        name: "icircumflex",
        width: 278,
    },
    GlyphWidth {
        name: "idieresis",
        width: 278,
    },
    GlyphWidth {
        name: "igrave",
        width: 278,
    },
    GlyphWidth {
        name: "imacron",
        width: 278,
    },
    GlyphWidth {
        name: "iogonek",
        width: 278,
    },
    GlyphWidth {
        name: "j",
        width: 333,
    },
    GlyphWidth {
        name: "k",
        width: 556,
    },
    GlyphWidth {
        name: "kcommaaccent",
        width: 556,
    },
    GlyphWidth {
        name: "l",
        width: 278,
    },
    GlyphWidth {
        name: "lacute",
        width: 278,
    },
    GlyphWidth {
        name: "lcaron",
        width: 394,
    },
    GlyphWidth {
        name: "lcommaaccent",
        width: 278,
    },
    GlyphWidth {
        name: "less",
        width: 570,
    },
    GlyphWidth {
        name: "lessequal",
        width: 549,
    },
    GlyphWidth {
        name: "logicalnot",
        width: 570,
    },
    GlyphWidth {
        name: "lozenge",
        width: 494,
    },
    GlyphWidth {
        name: "lslash",
        width: 278,
    },
    GlyphWidth {
        name: "m",
        width: 833,
    },
    GlyphWidth {
        name: "macron",
        width: 333,
    },
    GlyphWidth {
        name: "minus",
        width: 570,
    },
    GlyphWidth {
        name: "mu",
        width: 556,
    },
    GlyphWidth {
        name: "multiply",
        width: 570,
    },
    GlyphWidth {
        name: "n",
        width: 556,
    },
    GlyphWidth {
        name: "nacute",
        width: 556,
    },
    GlyphWidth {
        name: "ncaron",
        width: 556,
    },
    GlyphWidth {
        name: "ncommaaccent",
        width: 556,
    },
    GlyphWidth {
        name: "nine",
        width: 500,
    },
    GlyphWidth {
        name: "notequal",
        width: 549,
    },
    GlyphWidth {
        name: "ntilde",
        width: 556,
    },
    GlyphWidth {
        name: "numbersign",
        width: 500,
    },
    GlyphWidth {
        name: "o",
        width: 500,
    },
    GlyphWidth {
        name: "oacute",
        width: 500,
    },
    GlyphWidth {
        name: "ocircumflex",
        width: 500,
    },
    GlyphWidth {
        name: "odieresis",
        width: 500,
    },
    GlyphWidth {
        name: "oe",
        width: 722,
    },
    GlyphWidth {
        name: "ogonek",
        width: 333,
    },
    GlyphWidth {
        name: "ograve",
        width: 500,
    },
    GlyphWidth {
        name: "ohungarumlaut",
        width: 500,
    },
    GlyphWidth {
        name: "omacron",
        width: 500,
    },
    GlyphWidth {
        name: "one",
        width: 500,
    },
    GlyphWidth {
        name: "onehalf",
        width: 750,
    },
    GlyphWidth {
        name: "onequarter",
        width: 750,
    },
    GlyphWidth {
        name: "onesuperior",
        width: 300,
    },
    GlyphWidth {
        name: "ordfeminine",
        width: 300,
    },
    GlyphWidth {
        name: "ordmasculine",
        width: 330,
    },
    GlyphWidth {
        name: "oslash",
        width: 500,
    },
    GlyphWidth {
        name: "otilde",
        width: 500,
    },
    GlyphWidth {
        name: "p",
        width: 556,
    },
    GlyphWidth {
        name: "paragraph",
        width: 540,
    },
    GlyphWidth {
        name: "parenleft",
        width: 333,
    },
    GlyphWidth {
        name: "parenright",
        width: 333,
    },
    GlyphWidth {
        name: "partialdiff",
        width: 494,
    },
    GlyphWidth {
        name: "percent",
        width: 1000,
    },
    GlyphWidth {
        name: "period",
        width: 250,
    },
    GlyphWidth {
        name: "periodcentered",
        width: 250,
    },
    GlyphWidth {
        name: "perthousand",
        width: 1000,
    },
    GlyphWidth {
        name: "plus",
        width: 570,
    },
    GlyphWidth {
        name: "plusminus",
        width: 570,
    },
    GlyphWidth {
        name: "q",
        width: 556,
    },
    GlyphWidth {
        name: "question",
        width: 500,
    },
    GlyphWidth {
        name: "questiondown",
        width: 500,
    },
    GlyphWidth {
        name: "quotedbl",
        width: 555,
    },
    GlyphWidth {
        name: "quotedblbase",
        width: 500,
    },
    GlyphWidth {
        name: "quotedblleft",
        width: 500,
    },
    GlyphWidth {
        name: "quotedblright",
        width: 500,
    },
    GlyphWidth {
        name: "quoteleft",
        width: 333,
    },
    GlyphWidth {
        name: "quoteright",
        width: 333,
    },
    GlyphWidth {
        name: "quotesinglbase",
        width: 333,
    },
    GlyphWidth {
        name: "quotesingle",
        width: 278,
    },
    GlyphWidth {
        name: "r",
        width: 444,
    },
    GlyphWidth {
        name: "racute",
        width: 444,
    },
    GlyphWidth {
        name: "radical",
        width: 549,
    },
    GlyphWidth {
        name: "rcaron",
        width: 444,
    },
    GlyphWidth {
        name: "rcommaaccent",
        width: 444,
    },
    GlyphWidth {
        name: "registered",
        width: 747,
    },
    GlyphWidth {
        name: "ring",
        width: 333,
    },
    GlyphWidth {
        name: "s",
        width: 389,
    },
    GlyphWidth {
        name: "sacute",
        width: 389,
    },
    GlyphWidth {
        name: "scaron",
        width: 389,
    },
    GlyphWidth {
        name: "scedilla",
        width: 389,
    },
    GlyphWidth {
        name: "scommaaccent",
        width: 389,
    },
    GlyphWidth {
        name: "section",
        width: 500,
    },
    GlyphWidth {
        name: "semicolon",
        width: 333,
    },
    GlyphWidth {
        name: "seven",
        width: 500,
    },
    GlyphWidth {
        name: "six",
        width: 500,
    },
    GlyphWidth {
        name: "slash",
        width: 278,
    },
    GlyphWidth {
        name: "space",
        width: 250,
    },
    GlyphWidth {
        name: "sterling",
        width: 500,
    },
    GlyphWidth {
        name: "summation",
        width: 600,
    },
    GlyphWidth {
        name: "t",
        width: 333,
    },
    GlyphWidth {
        name: "tcaron",
        width: 416,
    },
    GlyphWidth {
        name: "tcommaaccent",
        width: 333,
    },
    GlyphWidth {
        name: "thorn",
        width: 556,
    },
    GlyphWidth {
        name: "three",
        width: 500,
    },
    GlyphWidth {
        name: "threequarters",
        width: 750,
    },
    GlyphWidth {
        name: "threesuperior",
        width: 300,
    },
    GlyphWidth {
        name: "tilde",
        width: 333,
    },
    GlyphWidth {
        name: "trademark",
        width: 1000,
    },
    GlyphWidth {
        name: "two",
        width: 500,
    },
    GlyphWidth {
        name: "twosuperior",
        width: 300,
    },
    GlyphWidth {
        name: "u",
        width: 556,
    },
    GlyphWidth {
        name: "uacute",
        width: 556,
    },
    GlyphWidth {
        name: "ucircumflex",
        width: 556,
    },
    GlyphWidth {
        name: "udieresis",
        width: 556,
    },
    GlyphWidth {
        name: "ugrave",
        width: 556,
    },
    GlyphWidth {
        name: "uhungarumlaut",
        width: 556,
    },
    GlyphWidth {
        name: "umacron",
        width: 556,
    },
    GlyphWidth {
        name: "underscore",
        width: 500,
    },
    GlyphWidth {
        name: "uogonek",
        width: 556,
    },
    GlyphWidth {
        name: "uring",
        width: 556,
    },
    GlyphWidth {
        name: "v",
        width: 500,
    },
    GlyphWidth {
        name: "w",
        width: 722,
    },
    GlyphWidth {
        name: "x",
        width: 500,
    },
    GlyphWidth {
        name: "y",
        width: 500,
    },
    GlyphWidth {
        name: "yacute",
        width: 500,
    },
    GlyphWidth {
        name: "ydieresis",
        width: 500,
    },
    GlyphWidth {
        name: "yen",
        width: 500,
    },
    GlyphWidth {
        name: "z",
        width: 444,
    },
    GlyphWidth {
        name: "zacute",
        width: 444,
    },
    GlyphWidth {
        name: "zcaron",
        width: 444,
    },
    GlyphWidth {
        name: "zdotaccent",
        width: 444,
    },
    GlyphWidth {
        name: "zero",
        width: 500,
    },
];

/// `Times-BoldItalic` — 315 glyphs.
pub const Times_BoldItalic: &[GlyphWidth] = &[
    GlyphWidth {
        name: "A",
        width: 667,
    },
    GlyphWidth {
        name: "AE",
        width: 944,
    },
    GlyphWidth {
        name: "Aacute",
        width: 667,
    },
    GlyphWidth {
        name: "Abreve",
        width: 667,
    },
    GlyphWidth {
        name: "Acircumflex",
        width: 667,
    },
    GlyphWidth {
        name: "Adieresis",
        width: 667,
    },
    GlyphWidth {
        name: "Agrave",
        width: 667,
    },
    GlyphWidth {
        name: "Amacron",
        width: 667,
    },
    GlyphWidth {
        name: "Aogonek",
        width: 667,
    },
    GlyphWidth {
        name: "Aring",
        width: 667,
    },
    GlyphWidth {
        name: "Atilde",
        width: 667,
    },
    GlyphWidth {
        name: "B",
        width: 667,
    },
    GlyphWidth {
        name: "C",
        width: 667,
    },
    GlyphWidth {
        name: "Cacute",
        width: 667,
    },
    GlyphWidth {
        name: "Ccaron",
        width: 667,
    },
    GlyphWidth {
        name: "Ccedilla",
        width: 667,
    },
    GlyphWidth {
        name: "D",
        width: 722,
    },
    GlyphWidth {
        name: "Dcaron",
        width: 722,
    },
    GlyphWidth {
        name: "Dcroat",
        width: 722,
    },
    GlyphWidth {
        name: "Delta",
        width: 612,
    },
    GlyphWidth {
        name: "E",
        width: 667,
    },
    GlyphWidth {
        name: "Eacute",
        width: 667,
    },
    GlyphWidth {
        name: "Ecaron",
        width: 667,
    },
    GlyphWidth {
        name: "Ecircumflex",
        width: 667,
    },
    GlyphWidth {
        name: "Edieresis",
        width: 667,
    },
    GlyphWidth {
        name: "Edotaccent",
        width: 667,
    },
    GlyphWidth {
        name: "Egrave",
        width: 667,
    },
    GlyphWidth {
        name: "Emacron",
        width: 667,
    },
    GlyphWidth {
        name: "Eogonek",
        width: 667,
    },
    GlyphWidth {
        name: "Eth",
        width: 722,
    },
    GlyphWidth {
        name: "Euro",
        width: 500,
    },
    GlyphWidth {
        name: "F",
        width: 667,
    },
    GlyphWidth {
        name: "G",
        width: 722,
    },
    GlyphWidth {
        name: "Gbreve",
        width: 722,
    },
    GlyphWidth {
        name: "Gcommaaccent",
        width: 722,
    },
    GlyphWidth {
        name: "H",
        width: 778,
    },
    GlyphWidth {
        name: "I",
        width: 389,
    },
    GlyphWidth {
        name: "Iacute",
        width: 389,
    },
    GlyphWidth {
        name: "Icircumflex",
        width: 389,
    },
    GlyphWidth {
        name: "Idieresis",
        width: 389,
    },
    GlyphWidth {
        name: "Idotaccent",
        width: 389,
    },
    GlyphWidth {
        name: "Igrave",
        width: 389,
    },
    GlyphWidth {
        name: "Imacron",
        width: 389,
    },
    GlyphWidth {
        name: "Iogonek",
        width: 389,
    },
    GlyphWidth {
        name: "J",
        width: 500,
    },
    GlyphWidth {
        name: "K",
        width: 667,
    },
    GlyphWidth {
        name: "Kcommaaccent",
        width: 667,
    },
    GlyphWidth {
        name: "L",
        width: 611,
    },
    GlyphWidth {
        name: "Lacute",
        width: 611,
    },
    GlyphWidth {
        name: "Lcaron",
        width: 611,
    },
    GlyphWidth {
        name: "Lcommaaccent",
        width: 611,
    },
    GlyphWidth {
        name: "Lslash",
        width: 611,
    },
    GlyphWidth {
        name: "M",
        width: 889,
    },
    GlyphWidth {
        name: "N",
        width: 722,
    },
    GlyphWidth {
        name: "Nacute",
        width: 722,
    },
    GlyphWidth {
        name: "Ncaron",
        width: 722,
    },
    GlyphWidth {
        name: "Ncommaaccent",
        width: 722,
    },
    GlyphWidth {
        name: "Ntilde",
        width: 722,
    },
    GlyphWidth {
        name: "O",
        width: 722,
    },
    GlyphWidth {
        name: "OE",
        width: 944,
    },
    GlyphWidth {
        name: "Oacute",
        width: 722,
    },
    GlyphWidth {
        name: "Ocircumflex",
        width: 722,
    },
    GlyphWidth {
        name: "Odieresis",
        width: 722,
    },
    GlyphWidth {
        name: "Ograve",
        width: 722,
    },
    GlyphWidth {
        name: "Ohungarumlaut",
        width: 722,
    },
    GlyphWidth {
        name: "Omacron",
        width: 722,
    },
    GlyphWidth {
        name: "Oslash",
        width: 722,
    },
    GlyphWidth {
        name: "Otilde",
        width: 722,
    },
    GlyphWidth {
        name: "P",
        width: 611,
    },
    GlyphWidth {
        name: "Q",
        width: 722,
    },
    GlyphWidth {
        name: "R",
        width: 667,
    },
    GlyphWidth {
        name: "Racute",
        width: 667,
    },
    GlyphWidth {
        name: "Rcaron",
        width: 667,
    },
    GlyphWidth {
        name: "Rcommaaccent",
        width: 667,
    },
    GlyphWidth {
        name: "S",
        width: 556,
    },
    GlyphWidth {
        name: "Sacute",
        width: 556,
    },
    GlyphWidth {
        name: "Scaron",
        width: 556,
    },
    GlyphWidth {
        name: "Scedilla",
        width: 556,
    },
    GlyphWidth {
        name: "Scommaaccent",
        width: 556,
    },
    GlyphWidth {
        name: "T",
        width: 611,
    },
    GlyphWidth {
        name: "Tcaron",
        width: 611,
    },
    GlyphWidth {
        name: "Tcommaaccent",
        width: 611,
    },
    GlyphWidth {
        name: "Thorn",
        width: 611,
    },
    GlyphWidth {
        name: "U",
        width: 722,
    },
    GlyphWidth {
        name: "Uacute",
        width: 722,
    },
    GlyphWidth {
        name: "Ucircumflex",
        width: 722,
    },
    GlyphWidth {
        name: "Udieresis",
        width: 722,
    },
    GlyphWidth {
        name: "Ugrave",
        width: 722,
    },
    GlyphWidth {
        name: "Uhungarumlaut",
        width: 722,
    },
    GlyphWidth {
        name: "Umacron",
        width: 722,
    },
    GlyphWidth {
        name: "Uogonek",
        width: 722,
    },
    GlyphWidth {
        name: "Uring",
        width: 722,
    },
    GlyphWidth {
        name: "V",
        width: 667,
    },
    GlyphWidth {
        name: "W",
        width: 889,
    },
    GlyphWidth {
        name: "X",
        width: 667,
    },
    GlyphWidth {
        name: "Y",
        width: 611,
    },
    GlyphWidth {
        name: "Yacute",
        width: 611,
    },
    GlyphWidth {
        name: "Ydieresis",
        width: 611,
    },
    GlyphWidth {
        name: "Z",
        width: 611,
    },
    GlyphWidth {
        name: "Zacute",
        width: 611,
    },
    GlyphWidth {
        name: "Zcaron",
        width: 611,
    },
    GlyphWidth {
        name: "Zdotaccent",
        width: 611,
    },
    GlyphWidth {
        name: "a",
        width: 500,
    },
    GlyphWidth {
        name: "aacute",
        width: 500,
    },
    GlyphWidth {
        name: "abreve",
        width: 500,
    },
    GlyphWidth {
        name: "acircumflex",
        width: 500,
    },
    GlyphWidth {
        name: "acute",
        width: 333,
    },
    GlyphWidth {
        name: "adieresis",
        width: 500,
    },
    GlyphWidth {
        name: "ae",
        width: 722,
    },
    GlyphWidth {
        name: "agrave",
        width: 500,
    },
    GlyphWidth {
        name: "amacron",
        width: 500,
    },
    GlyphWidth {
        name: "ampersand",
        width: 778,
    },
    GlyphWidth {
        name: "aogonek",
        width: 500,
    },
    GlyphWidth {
        name: "aring",
        width: 500,
    },
    GlyphWidth {
        name: "asciicircum",
        width: 570,
    },
    GlyphWidth {
        name: "asciitilde",
        width: 570,
    },
    GlyphWidth {
        name: "asterisk",
        width: 500,
    },
    GlyphWidth {
        name: "at",
        width: 832,
    },
    GlyphWidth {
        name: "atilde",
        width: 500,
    },
    GlyphWidth {
        name: "b",
        width: 500,
    },
    GlyphWidth {
        name: "backslash",
        width: 278,
    },
    GlyphWidth {
        name: "bar",
        width: 220,
    },
    GlyphWidth {
        name: "braceleft",
        width: 348,
    },
    GlyphWidth {
        name: "braceright",
        width: 348,
    },
    GlyphWidth {
        name: "bracketleft",
        width: 333,
    },
    GlyphWidth {
        name: "bracketright",
        width: 333,
    },
    GlyphWidth {
        name: "breve",
        width: 333,
    },
    GlyphWidth {
        name: "brokenbar",
        width: 220,
    },
    GlyphWidth {
        name: "bullet",
        width: 350,
    },
    GlyphWidth {
        name: "c",
        width: 444,
    },
    GlyphWidth {
        name: "cacute",
        width: 444,
    },
    GlyphWidth {
        name: "caron",
        width: 333,
    },
    GlyphWidth {
        name: "ccaron",
        width: 444,
    },
    GlyphWidth {
        name: "ccedilla",
        width: 444,
    },
    GlyphWidth {
        name: "cedilla",
        width: 333,
    },
    GlyphWidth {
        name: "cent",
        width: 500,
    },
    GlyphWidth {
        name: "circumflex",
        width: 333,
    },
    GlyphWidth {
        name: "colon",
        width: 333,
    },
    GlyphWidth {
        name: "comma",
        width: 250,
    },
    GlyphWidth {
        name: "commaaccent",
        width: 250,
    },
    GlyphWidth {
        name: "copyright",
        width: 747,
    },
    GlyphWidth {
        name: "currency",
        width: 500,
    },
    GlyphWidth {
        name: "d",
        width: 500,
    },
    GlyphWidth {
        name: "dagger",
        width: 500,
    },
    GlyphWidth {
        name: "daggerdbl",
        width: 500,
    },
    GlyphWidth {
        name: "dcaron",
        width: 608,
    },
    GlyphWidth {
        name: "dcroat",
        width: 500,
    },
    GlyphWidth {
        name: "degree",
        width: 400,
    },
    GlyphWidth {
        name: "dieresis",
        width: 333,
    },
    GlyphWidth {
        name: "divide",
        width: 570,
    },
    GlyphWidth {
        name: "dollar",
        width: 500,
    },
    GlyphWidth {
        name: "dotaccent",
        width: 333,
    },
    GlyphWidth {
        name: "dotlessi",
        width: 278,
    },
    GlyphWidth {
        name: "e",
        width: 444,
    },
    GlyphWidth {
        name: "eacute",
        width: 444,
    },
    GlyphWidth {
        name: "ecaron",
        width: 444,
    },
    GlyphWidth {
        name: "ecircumflex",
        width: 444,
    },
    GlyphWidth {
        name: "edieresis",
        width: 444,
    },
    GlyphWidth {
        name: "edotaccent",
        width: 444,
    },
    GlyphWidth {
        name: "egrave",
        width: 444,
    },
    GlyphWidth {
        name: "eight",
        width: 500,
    },
    GlyphWidth {
        name: "ellipsis",
        width: 1000,
    },
    GlyphWidth {
        name: "emacron",
        width: 444,
    },
    GlyphWidth {
        name: "emdash",
        width: 1000,
    },
    GlyphWidth {
        name: "endash",
        width: 500,
    },
    GlyphWidth {
        name: "eogonek",
        width: 444,
    },
    GlyphWidth {
        name: "equal",
        width: 570,
    },
    GlyphWidth {
        name: "eth",
        width: 500,
    },
    GlyphWidth {
        name: "exclam",
        width: 389,
    },
    GlyphWidth {
        name: "exclamdown",
        width: 389,
    },
    GlyphWidth {
        name: "f",
        width: 333,
    },
    GlyphWidth {
        name: "fi",
        width: 556,
    },
    GlyphWidth {
        name: "five",
        width: 500,
    },
    GlyphWidth {
        name: "fl",
        width: 556,
    },
    GlyphWidth {
        name: "florin",
        width: 500,
    },
    GlyphWidth {
        name: "four",
        width: 500,
    },
    GlyphWidth {
        name: "fraction",
        width: 167,
    },
    GlyphWidth {
        name: "g",
        width: 500,
    },
    GlyphWidth {
        name: "gbreve",
        width: 500,
    },
    GlyphWidth {
        name: "gcommaaccent",
        width: 500,
    },
    GlyphWidth {
        name: "germandbls",
        width: 500,
    },
    GlyphWidth {
        name: "grave",
        width: 333,
    },
    GlyphWidth {
        name: "greater",
        width: 570,
    },
    GlyphWidth {
        name: "greaterequal",
        width: 549,
    },
    GlyphWidth {
        name: "guillemotleft",
        width: 500,
    },
    GlyphWidth {
        name: "guillemotright",
        width: 500,
    },
    GlyphWidth {
        name: "guilsinglleft",
        width: 333,
    },
    GlyphWidth {
        name: "guilsinglright",
        width: 333,
    },
    GlyphWidth {
        name: "h",
        width: 556,
    },
    GlyphWidth {
        name: "hungarumlaut",
        width: 333,
    },
    GlyphWidth {
        name: "hyphen",
        width: 333,
    },
    GlyphWidth {
        name: "i",
        width: 278,
    },
    GlyphWidth {
        name: "iacute",
        width: 278,
    },
    GlyphWidth {
        name: "icircumflex",
        width: 278,
    },
    GlyphWidth {
        name: "idieresis",
        width: 278,
    },
    GlyphWidth {
        name: "igrave",
        width: 278,
    },
    GlyphWidth {
        name: "imacron",
        width: 278,
    },
    GlyphWidth {
        name: "iogonek",
        width: 278,
    },
    GlyphWidth {
        name: "j",
        width: 278,
    },
    GlyphWidth {
        name: "k",
        width: 500,
    },
    GlyphWidth {
        name: "kcommaaccent",
        width: 500,
    },
    GlyphWidth {
        name: "l",
        width: 278,
    },
    GlyphWidth {
        name: "lacute",
        width: 278,
    },
    GlyphWidth {
        name: "lcaron",
        width: 382,
    },
    GlyphWidth {
        name: "lcommaaccent",
        width: 278,
    },
    GlyphWidth {
        name: "less",
        width: 570,
    },
    GlyphWidth {
        name: "lessequal",
        width: 549,
    },
    GlyphWidth {
        name: "logicalnot",
        width: 606,
    },
    GlyphWidth {
        name: "lozenge",
        width: 494,
    },
    GlyphWidth {
        name: "lslash",
        width: 278,
    },
    GlyphWidth {
        name: "m",
        width: 778,
    },
    GlyphWidth {
        name: "macron",
        width: 333,
    },
    GlyphWidth {
        name: "minus",
        width: 606,
    },
    GlyphWidth {
        name: "mu",
        width: 576,
    },
    GlyphWidth {
        name: "multiply",
        width: 570,
    },
    GlyphWidth {
        name: "n",
        width: 556,
    },
    GlyphWidth {
        name: "nacute",
        width: 556,
    },
    GlyphWidth {
        name: "ncaron",
        width: 556,
    },
    GlyphWidth {
        name: "ncommaaccent",
        width: 556,
    },
    GlyphWidth {
        name: "nine",
        width: 500,
    },
    GlyphWidth {
        name: "notequal",
        width: 549,
    },
    GlyphWidth {
        name: "ntilde",
        width: 556,
    },
    GlyphWidth {
        name: "numbersign",
        width: 500,
    },
    GlyphWidth {
        name: "o",
        width: 500,
    },
    GlyphWidth {
        name: "oacute",
        width: 500,
    },
    GlyphWidth {
        name: "ocircumflex",
        width: 500,
    },
    GlyphWidth {
        name: "odieresis",
        width: 500,
    },
    GlyphWidth {
        name: "oe",
        width: 722,
    },
    GlyphWidth {
        name: "ogonek",
        width: 333,
    },
    GlyphWidth {
        name: "ograve",
        width: 500,
    },
    GlyphWidth {
        name: "ohungarumlaut",
        width: 500,
    },
    GlyphWidth {
        name: "omacron",
        width: 500,
    },
    GlyphWidth {
        name: "one",
        width: 500,
    },
    GlyphWidth {
        name: "onehalf",
        width: 750,
    },
    GlyphWidth {
        name: "onequarter",
        width: 750,
    },
    GlyphWidth {
        name: "onesuperior",
        width: 300,
    },
    GlyphWidth {
        name: "ordfeminine",
        width: 266,
    },
    GlyphWidth {
        name: "ordmasculine",
        width: 300,
    },
    GlyphWidth {
        name: "oslash",
        width: 500,
    },
    GlyphWidth {
        name: "otilde",
        width: 500,
    },
    GlyphWidth {
        name: "p",
        width: 500,
    },
    GlyphWidth {
        name: "paragraph",
        width: 500,
    },
    GlyphWidth {
        name: "parenleft",
        width: 333,
    },
    GlyphWidth {
        name: "parenright",
        width: 333,
    },
    GlyphWidth {
        name: "partialdiff",
        width: 494,
    },
    GlyphWidth {
        name: "percent",
        width: 833,
    },
    GlyphWidth {
        name: "period",
        width: 250,
    },
    GlyphWidth {
        name: "periodcentered",
        width: 250,
    },
    GlyphWidth {
        name: "perthousand",
        width: 1000,
    },
    GlyphWidth {
        name: "plus",
        width: 570,
    },
    GlyphWidth {
        name: "plusminus",
        width: 570,
    },
    GlyphWidth {
        name: "q",
        width: 500,
    },
    GlyphWidth {
        name: "question",
        width: 500,
    },
    GlyphWidth {
        name: "questiondown",
        width: 500,
    },
    GlyphWidth {
        name: "quotedbl",
        width: 555,
    },
    GlyphWidth {
        name: "quotedblbase",
        width: 500,
    },
    GlyphWidth {
        name: "quotedblleft",
        width: 500,
    },
    GlyphWidth {
        name: "quotedblright",
        width: 500,
    },
    GlyphWidth {
        name: "quoteleft",
        width: 333,
    },
    GlyphWidth {
        name: "quoteright",
        width: 333,
    },
    GlyphWidth {
        name: "quotesinglbase",
        width: 333,
    },
    GlyphWidth {
        name: "quotesingle",
        width: 278,
    },
    GlyphWidth {
        name: "r",
        width: 389,
    },
    GlyphWidth {
        name: "racute",
        width: 389,
    },
    GlyphWidth {
        name: "radical",
        width: 549,
    },
    GlyphWidth {
        name: "rcaron",
        width: 389,
    },
    GlyphWidth {
        name: "rcommaaccent",
        width: 389,
    },
    GlyphWidth {
        name: "registered",
        width: 747,
    },
    GlyphWidth {
        name: "ring",
        width: 333,
    },
    GlyphWidth {
        name: "s",
        width: 389,
    },
    GlyphWidth {
        name: "sacute",
        width: 389,
    },
    GlyphWidth {
        name: "scaron",
        width: 389,
    },
    GlyphWidth {
        name: "scedilla",
        width: 389,
    },
    GlyphWidth {
        name: "scommaaccent",
        width: 389,
    },
    GlyphWidth {
        name: "section",
        width: 500,
    },
    GlyphWidth {
        name: "semicolon",
        width: 333,
    },
    GlyphWidth {
        name: "seven",
        width: 500,
    },
    GlyphWidth {
        name: "six",
        width: 500,
    },
    GlyphWidth {
        name: "slash",
        width: 278,
    },
    GlyphWidth {
        name: "space",
        width: 250,
    },
    GlyphWidth {
        name: "sterling",
        width: 500,
    },
    GlyphWidth {
        name: "summation",
        width: 600,
    },
    GlyphWidth {
        name: "t",
        width: 278,
    },
    GlyphWidth {
        name: "tcaron",
        width: 366,
    },
    GlyphWidth {
        name: "tcommaaccent",
        width: 278,
    },
    GlyphWidth {
        name: "thorn",
        width: 500,
    },
    GlyphWidth {
        name: "three",
        width: 500,
    },
    GlyphWidth {
        name: "threequarters",
        width: 750,
    },
    GlyphWidth {
        name: "threesuperior",
        width: 300,
    },
    GlyphWidth {
        name: "tilde",
        width: 333,
    },
    GlyphWidth {
        name: "trademark",
        width: 1000,
    },
    GlyphWidth {
        name: "two",
        width: 500,
    },
    GlyphWidth {
        name: "twosuperior",
        width: 300,
    },
    GlyphWidth {
        name: "u",
        width: 556,
    },
    GlyphWidth {
        name: "uacute",
        width: 556,
    },
    GlyphWidth {
        name: "ucircumflex",
        width: 556,
    },
    GlyphWidth {
        name: "udieresis",
        width: 556,
    },
    GlyphWidth {
        name: "ugrave",
        width: 556,
    },
    GlyphWidth {
        name: "uhungarumlaut",
        width: 556,
    },
    GlyphWidth {
        name: "umacron",
        width: 556,
    },
    GlyphWidth {
        name: "underscore",
        width: 500,
    },
    GlyphWidth {
        name: "uogonek",
        width: 556,
    },
    GlyphWidth {
        name: "uring",
        width: 556,
    },
    GlyphWidth {
        name: "v",
        width: 444,
    },
    GlyphWidth {
        name: "w",
        width: 667,
    },
    GlyphWidth {
        name: "x",
        width: 500,
    },
    GlyphWidth {
        name: "y",
        width: 444,
    },
    GlyphWidth {
        name: "yacute",
        width: 444,
    },
    GlyphWidth {
        name: "ydieresis",
        width: 444,
    },
    GlyphWidth {
        name: "yen",
        width: 500,
    },
    GlyphWidth {
        name: "z",
        width: 389,
    },
    GlyphWidth {
        name: "zacute",
        width: 389,
    },
    GlyphWidth {
        name: "zcaron",
        width: 389,
    },
    GlyphWidth {
        name: "zdotaccent",
        width: 389,
    },
    GlyphWidth {
        name: "zero",
        width: 500,
    },
];

/// `Times-Italic` — 315 glyphs.
pub const Times_Italic: &[GlyphWidth] = &[
    GlyphWidth {
        name: "A",
        width: 611,
    },
    GlyphWidth {
        name: "AE",
        width: 889,
    },
    GlyphWidth {
        name: "Aacute",
        width: 611,
    },
    GlyphWidth {
        name: "Abreve",
        width: 611,
    },
    GlyphWidth {
        name: "Acircumflex",
        width: 611,
    },
    GlyphWidth {
        name: "Adieresis",
        width: 611,
    },
    GlyphWidth {
        name: "Agrave",
        width: 611,
    },
    GlyphWidth {
        name: "Amacron",
        width: 611,
    },
    GlyphWidth {
        name: "Aogonek",
        width: 611,
    },
    GlyphWidth {
        name: "Aring",
        width: 611,
    },
    GlyphWidth {
        name: "Atilde",
        width: 611,
    },
    GlyphWidth {
        name: "B",
        width: 611,
    },
    GlyphWidth {
        name: "C",
        width: 667,
    },
    GlyphWidth {
        name: "Cacute",
        width: 667,
    },
    GlyphWidth {
        name: "Ccaron",
        width: 667,
    },
    GlyphWidth {
        name: "Ccedilla",
        width: 667,
    },
    GlyphWidth {
        name: "D",
        width: 722,
    },
    GlyphWidth {
        name: "Dcaron",
        width: 722,
    },
    GlyphWidth {
        name: "Dcroat",
        width: 722,
    },
    GlyphWidth {
        name: "Delta",
        width: 612,
    },
    GlyphWidth {
        name: "E",
        width: 611,
    },
    GlyphWidth {
        name: "Eacute",
        width: 611,
    },
    GlyphWidth {
        name: "Ecaron",
        width: 611,
    },
    GlyphWidth {
        name: "Ecircumflex",
        width: 611,
    },
    GlyphWidth {
        name: "Edieresis",
        width: 611,
    },
    GlyphWidth {
        name: "Edotaccent",
        width: 611,
    },
    GlyphWidth {
        name: "Egrave",
        width: 611,
    },
    GlyphWidth {
        name: "Emacron",
        width: 611,
    },
    GlyphWidth {
        name: "Eogonek",
        width: 611,
    },
    GlyphWidth {
        name: "Eth",
        width: 722,
    },
    GlyphWidth {
        name: "Euro",
        width: 500,
    },
    GlyphWidth {
        name: "F",
        width: 611,
    },
    GlyphWidth {
        name: "G",
        width: 722,
    },
    GlyphWidth {
        name: "Gbreve",
        width: 722,
    },
    GlyphWidth {
        name: "Gcommaaccent",
        width: 722,
    },
    GlyphWidth {
        name: "H",
        width: 722,
    },
    GlyphWidth {
        name: "I",
        width: 333,
    },
    GlyphWidth {
        name: "Iacute",
        width: 333,
    },
    GlyphWidth {
        name: "Icircumflex",
        width: 333,
    },
    GlyphWidth {
        name: "Idieresis",
        width: 333,
    },
    GlyphWidth {
        name: "Idotaccent",
        width: 333,
    },
    GlyphWidth {
        name: "Igrave",
        width: 333,
    },
    GlyphWidth {
        name: "Imacron",
        width: 333,
    },
    GlyphWidth {
        name: "Iogonek",
        width: 333,
    },
    GlyphWidth {
        name: "J",
        width: 444,
    },
    GlyphWidth {
        name: "K",
        width: 667,
    },
    GlyphWidth {
        name: "Kcommaaccent",
        width: 667,
    },
    GlyphWidth {
        name: "L",
        width: 556,
    },
    GlyphWidth {
        name: "Lacute",
        width: 556,
    },
    GlyphWidth {
        name: "Lcaron",
        width: 611,
    },
    GlyphWidth {
        name: "Lcommaaccent",
        width: 556,
    },
    GlyphWidth {
        name: "Lslash",
        width: 556,
    },
    GlyphWidth {
        name: "M",
        width: 833,
    },
    GlyphWidth {
        name: "N",
        width: 667,
    },
    GlyphWidth {
        name: "Nacute",
        width: 667,
    },
    GlyphWidth {
        name: "Ncaron",
        width: 667,
    },
    GlyphWidth {
        name: "Ncommaaccent",
        width: 667,
    },
    GlyphWidth {
        name: "Ntilde",
        width: 667,
    },
    GlyphWidth {
        name: "O",
        width: 722,
    },
    GlyphWidth {
        name: "OE",
        width: 944,
    },
    GlyphWidth {
        name: "Oacute",
        width: 722,
    },
    GlyphWidth {
        name: "Ocircumflex",
        width: 722,
    },
    GlyphWidth {
        name: "Odieresis",
        width: 722,
    },
    GlyphWidth {
        name: "Ograve",
        width: 722,
    },
    GlyphWidth {
        name: "Ohungarumlaut",
        width: 722,
    },
    GlyphWidth {
        name: "Omacron",
        width: 722,
    },
    GlyphWidth {
        name: "Oslash",
        width: 722,
    },
    GlyphWidth {
        name: "Otilde",
        width: 722,
    },
    GlyphWidth {
        name: "P",
        width: 611,
    },
    GlyphWidth {
        name: "Q",
        width: 722,
    },
    GlyphWidth {
        name: "R",
        width: 611,
    },
    GlyphWidth {
        name: "Racute",
        width: 611,
    },
    GlyphWidth {
        name: "Rcaron",
        width: 611,
    },
    GlyphWidth {
        name: "Rcommaaccent",
        width: 611,
    },
    GlyphWidth {
        name: "S",
        width: 500,
    },
    GlyphWidth {
        name: "Sacute",
        width: 500,
    },
    GlyphWidth {
        name: "Scaron",
        width: 500,
    },
    GlyphWidth {
        name: "Scedilla",
        width: 500,
    },
    GlyphWidth {
        name: "Scommaaccent",
        width: 500,
    },
    GlyphWidth {
        name: "T",
        width: 556,
    },
    GlyphWidth {
        name: "Tcaron",
        width: 556,
    },
    GlyphWidth {
        name: "Tcommaaccent",
        width: 556,
    },
    GlyphWidth {
        name: "Thorn",
        width: 611,
    },
    GlyphWidth {
        name: "U",
        width: 722,
    },
    GlyphWidth {
        name: "Uacute",
        width: 722,
    },
    GlyphWidth {
        name: "Ucircumflex",
        width: 722,
    },
    GlyphWidth {
        name: "Udieresis",
        width: 722,
    },
    GlyphWidth {
        name: "Ugrave",
        width: 722,
    },
    GlyphWidth {
        name: "Uhungarumlaut",
        width: 722,
    },
    GlyphWidth {
        name: "Umacron",
        width: 722,
    },
    GlyphWidth {
        name: "Uogonek",
        width: 722,
    },
    GlyphWidth {
        name: "Uring",
        width: 722,
    },
    GlyphWidth {
        name: "V",
        width: 611,
    },
    GlyphWidth {
        name: "W",
        width: 833,
    },
    GlyphWidth {
        name: "X",
        width: 611,
    },
    GlyphWidth {
        name: "Y",
        width: 556,
    },
    GlyphWidth {
        name: "Yacute",
        width: 556,
    },
    GlyphWidth {
        name: "Ydieresis",
        width: 556,
    },
    GlyphWidth {
        name: "Z",
        width: 556,
    },
    GlyphWidth {
        name: "Zacute",
        width: 556,
    },
    GlyphWidth {
        name: "Zcaron",
        width: 556,
    },
    GlyphWidth {
        name: "Zdotaccent",
        width: 556,
    },
    GlyphWidth {
        name: "a",
        width: 500,
    },
    GlyphWidth {
        name: "aacute",
        width: 500,
    },
    GlyphWidth {
        name: "abreve",
        width: 500,
    },
    GlyphWidth {
        name: "acircumflex",
        width: 500,
    },
    GlyphWidth {
        name: "acute",
        width: 333,
    },
    GlyphWidth {
        name: "adieresis",
        width: 500,
    },
    GlyphWidth {
        name: "ae",
        width: 667,
    },
    GlyphWidth {
        name: "agrave",
        width: 500,
    },
    GlyphWidth {
        name: "amacron",
        width: 500,
    },
    GlyphWidth {
        name: "ampersand",
        width: 778,
    },
    GlyphWidth {
        name: "aogonek",
        width: 500,
    },
    GlyphWidth {
        name: "aring",
        width: 500,
    },
    GlyphWidth {
        name: "asciicircum",
        width: 422,
    },
    GlyphWidth {
        name: "asciitilde",
        width: 541,
    },
    GlyphWidth {
        name: "asterisk",
        width: 500,
    },
    GlyphWidth {
        name: "at",
        width: 920,
    },
    GlyphWidth {
        name: "atilde",
        width: 500,
    },
    GlyphWidth {
        name: "b",
        width: 500,
    },
    GlyphWidth {
        name: "backslash",
        width: 278,
    },
    GlyphWidth {
        name: "bar",
        width: 275,
    },
    GlyphWidth {
        name: "braceleft",
        width: 400,
    },
    GlyphWidth {
        name: "braceright",
        width: 400,
    },
    GlyphWidth {
        name: "bracketleft",
        width: 389,
    },
    GlyphWidth {
        name: "bracketright",
        width: 389,
    },
    GlyphWidth {
        name: "breve",
        width: 333,
    },
    GlyphWidth {
        name: "brokenbar",
        width: 275,
    },
    GlyphWidth {
        name: "bullet",
        width: 350,
    },
    GlyphWidth {
        name: "c",
        width: 444,
    },
    GlyphWidth {
        name: "cacute",
        width: 444,
    },
    GlyphWidth {
        name: "caron",
        width: 333,
    },
    GlyphWidth {
        name: "ccaron",
        width: 444,
    },
    GlyphWidth {
        name: "ccedilla",
        width: 444,
    },
    GlyphWidth {
        name: "cedilla",
        width: 333,
    },
    GlyphWidth {
        name: "cent",
        width: 500,
    },
    GlyphWidth {
        name: "circumflex",
        width: 333,
    },
    GlyphWidth {
        name: "colon",
        width: 333,
    },
    GlyphWidth {
        name: "comma",
        width: 250,
    },
    GlyphWidth {
        name: "commaaccent",
        width: 250,
    },
    GlyphWidth {
        name: "copyright",
        width: 760,
    },
    GlyphWidth {
        name: "currency",
        width: 500,
    },
    GlyphWidth {
        name: "d",
        width: 500,
    },
    GlyphWidth {
        name: "dagger",
        width: 500,
    },
    GlyphWidth {
        name: "daggerdbl",
        width: 500,
    },
    GlyphWidth {
        name: "dcaron",
        width: 544,
    },
    GlyphWidth {
        name: "dcroat",
        width: 500,
    },
    GlyphWidth {
        name: "degree",
        width: 400,
    },
    GlyphWidth {
        name: "dieresis",
        width: 333,
    },
    GlyphWidth {
        name: "divide",
        width: 675,
    },
    GlyphWidth {
        name: "dollar",
        width: 500,
    },
    GlyphWidth {
        name: "dotaccent",
        width: 333,
    },
    GlyphWidth {
        name: "dotlessi",
        width: 278,
    },
    GlyphWidth {
        name: "e",
        width: 444,
    },
    GlyphWidth {
        name: "eacute",
        width: 444,
    },
    GlyphWidth {
        name: "ecaron",
        width: 444,
    },
    GlyphWidth {
        name: "ecircumflex",
        width: 444,
    },
    GlyphWidth {
        name: "edieresis",
        width: 444,
    },
    GlyphWidth {
        name: "edotaccent",
        width: 444,
    },
    GlyphWidth {
        name: "egrave",
        width: 444,
    },
    GlyphWidth {
        name: "eight",
        width: 500,
    },
    GlyphWidth {
        name: "ellipsis",
        width: 889,
    },
    GlyphWidth {
        name: "emacron",
        width: 444,
    },
    GlyphWidth {
        name: "emdash",
        width: 889,
    },
    GlyphWidth {
        name: "endash",
        width: 500,
    },
    GlyphWidth {
        name: "eogonek",
        width: 444,
    },
    GlyphWidth {
        name: "equal",
        width: 675,
    },
    GlyphWidth {
        name: "eth",
        width: 500,
    },
    GlyphWidth {
        name: "exclam",
        width: 333,
    },
    GlyphWidth {
        name: "exclamdown",
        width: 389,
    },
    GlyphWidth {
        name: "f",
        width: 278,
    },
    GlyphWidth {
        name: "fi",
        width: 500,
    },
    GlyphWidth {
        name: "five",
        width: 500,
    },
    GlyphWidth {
        name: "fl",
        width: 500,
    },
    GlyphWidth {
        name: "florin",
        width: 500,
    },
    GlyphWidth {
        name: "four",
        width: 500,
    },
    GlyphWidth {
        name: "fraction",
        width: 167,
    },
    GlyphWidth {
        name: "g",
        width: 500,
    },
    GlyphWidth {
        name: "gbreve",
        width: 500,
    },
    GlyphWidth {
        name: "gcommaaccent",
        width: 500,
    },
    GlyphWidth {
        name: "germandbls",
        width: 500,
    },
    GlyphWidth {
        name: "grave",
        width: 333,
    },
    GlyphWidth {
        name: "greater",
        width: 675,
    },
    GlyphWidth {
        name: "greaterequal",
        width: 549,
    },
    GlyphWidth {
        name: "guillemotleft",
        width: 500,
    },
    GlyphWidth {
        name: "guillemotright",
        width: 500,
    },
    GlyphWidth {
        name: "guilsinglleft",
        width: 333,
    },
    GlyphWidth {
        name: "guilsinglright",
        width: 333,
    },
    GlyphWidth {
        name: "h",
        width: 500,
    },
    GlyphWidth {
        name: "hungarumlaut",
        width: 333,
    },
    GlyphWidth {
        name: "hyphen",
        width: 333,
    },
    GlyphWidth {
        name: "i",
        width: 278,
    },
    GlyphWidth {
        name: "iacute",
        width: 278,
    },
    GlyphWidth {
        name: "icircumflex",
        width: 278,
    },
    GlyphWidth {
        name: "idieresis",
        width: 278,
    },
    GlyphWidth {
        name: "igrave",
        width: 278,
    },
    GlyphWidth {
        name: "imacron",
        width: 278,
    },
    GlyphWidth {
        name: "iogonek",
        width: 278,
    },
    GlyphWidth {
        name: "j",
        width: 278,
    },
    GlyphWidth {
        name: "k",
        width: 444,
    },
    GlyphWidth {
        name: "kcommaaccent",
        width: 444,
    },
    GlyphWidth {
        name: "l",
        width: 278,
    },
    GlyphWidth {
        name: "lacute",
        width: 278,
    },
    GlyphWidth {
        name: "lcaron",
        width: 300,
    },
    GlyphWidth {
        name: "lcommaaccent",
        width: 278,
    },
    GlyphWidth {
        name: "less",
        width: 675,
    },
    GlyphWidth {
        name: "lessequal",
        width: 549,
    },
    GlyphWidth {
        name: "logicalnot",
        width: 675,
    },
    GlyphWidth {
        name: "lozenge",
        width: 471,
    },
    GlyphWidth {
        name: "lslash",
        width: 278,
    },
    GlyphWidth {
        name: "m",
        width: 722,
    },
    GlyphWidth {
        name: "macron",
        width: 333,
    },
    GlyphWidth {
        name: "minus",
        width: 675,
    },
    GlyphWidth {
        name: "mu",
        width: 500,
    },
    GlyphWidth {
        name: "multiply",
        width: 675,
    },
    GlyphWidth {
        name: "n",
        width: 500,
    },
    GlyphWidth {
        name: "nacute",
        width: 500,
    },
    GlyphWidth {
        name: "ncaron",
        width: 500,
    },
    GlyphWidth {
        name: "ncommaaccent",
        width: 500,
    },
    GlyphWidth {
        name: "nine",
        width: 500,
    },
    GlyphWidth {
        name: "notequal",
        width: 549,
    },
    GlyphWidth {
        name: "ntilde",
        width: 500,
    },
    GlyphWidth {
        name: "numbersign",
        width: 500,
    },
    GlyphWidth {
        name: "o",
        width: 500,
    },
    GlyphWidth {
        name: "oacute",
        width: 500,
    },
    GlyphWidth {
        name: "ocircumflex",
        width: 500,
    },
    GlyphWidth {
        name: "odieresis",
        width: 500,
    },
    GlyphWidth {
        name: "oe",
        width: 667,
    },
    GlyphWidth {
        name: "ogonek",
        width: 333,
    },
    GlyphWidth {
        name: "ograve",
        width: 500,
    },
    GlyphWidth {
        name: "ohungarumlaut",
        width: 500,
    },
    GlyphWidth {
        name: "omacron",
        width: 500,
    },
    GlyphWidth {
        name: "one",
        width: 500,
    },
    GlyphWidth {
        name: "onehalf",
        width: 750,
    },
    GlyphWidth {
        name: "onequarter",
        width: 750,
    },
    GlyphWidth {
        name: "onesuperior",
        width: 300,
    },
    GlyphWidth {
        name: "ordfeminine",
        width: 276,
    },
    GlyphWidth {
        name: "ordmasculine",
        width: 310,
    },
    GlyphWidth {
        name: "oslash",
        width: 500,
    },
    GlyphWidth {
        name: "otilde",
        width: 500,
    },
    GlyphWidth {
        name: "p",
        width: 500,
    },
    GlyphWidth {
        name: "paragraph",
        width: 523,
    },
    GlyphWidth {
        name: "parenleft",
        width: 333,
    },
    GlyphWidth {
        name: "parenright",
        width: 333,
    },
    GlyphWidth {
        name: "partialdiff",
        width: 476,
    },
    GlyphWidth {
        name: "percent",
        width: 833,
    },
    GlyphWidth {
        name: "period",
        width: 250,
    },
    GlyphWidth {
        name: "periodcentered",
        width: 250,
    },
    GlyphWidth {
        name: "perthousand",
        width: 1000,
    },
    GlyphWidth {
        name: "plus",
        width: 675,
    },
    GlyphWidth {
        name: "plusminus",
        width: 675,
    },
    GlyphWidth {
        name: "q",
        width: 500,
    },
    GlyphWidth {
        name: "question",
        width: 500,
    },
    GlyphWidth {
        name: "questiondown",
        width: 500,
    },
    GlyphWidth {
        name: "quotedbl",
        width: 420,
    },
    GlyphWidth {
        name: "quotedblbase",
        width: 556,
    },
    GlyphWidth {
        name: "quotedblleft",
        width: 556,
    },
    GlyphWidth {
        name: "quotedblright",
        width: 556,
    },
    GlyphWidth {
        name: "quoteleft",
        width: 333,
    },
    GlyphWidth {
        name: "quoteright",
        width: 333,
    },
    GlyphWidth {
        name: "quotesinglbase",
        width: 333,
    },
    GlyphWidth {
        name: "quotesingle",
        width: 214,
    },
    GlyphWidth {
        name: "r",
        width: 389,
    },
    GlyphWidth {
        name: "racute",
        width: 389,
    },
    GlyphWidth {
        name: "radical",
        width: 453,
    },
    GlyphWidth {
        name: "rcaron",
        width: 389,
    },
    GlyphWidth {
        name: "rcommaaccent",
        width: 389,
    },
    GlyphWidth {
        name: "registered",
        width: 760,
    },
    GlyphWidth {
        name: "ring",
        width: 333,
    },
    GlyphWidth {
        name: "s",
        width: 389,
    },
    GlyphWidth {
        name: "sacute",
        width: 389,
    },
    GlyphWidth {
        name: "scaron",
        width: 389,
    },
    GlyphWidth {
        name: "scedilla",
        width: 389,
    },
    GlyphWidth {
        name: "scommaaccent",
        width: 389,
    },
    GlyphWidth {
        name: "section",
        width: 500,
    },
    GlyphWidth {
        name: "semicolon",
        width: 333,
    },
    GlyphWidth {
        name: "seven",
        width: 500,
    },
    GlyphWidth {
        name: "six",
        width: 500,
    },
    GlyphWidth {
        name: "slash",
        width: 278,
    },
    GlyphWidth {
        name: "space",
        width: 250,
    },
    GlyphWidth {
        name: "sterling",
        width: 500,
    },
    GlyphWidth {
        name: "summation",
        width: 600,
    },
    GlyphWidth {
        name: "t",
        width: 278,
    },
    GlyphWidth {
        name: "tcaron",
        width: 300,
    },
    GlyphWidth {
        name: "tcommaaccent",
        width: 278,
    },
    GlyphWidth {
        name: "thorn",
        width: 500,
    },
    GlyphWidth {
        name: "three",
        width: 500,
    },
    GlyphWidth {
        name: "threequarters",
        width: 750,
    },
    GlyphWidth {
        name: "threesuperior",
        width: 300,
    },
    GlyphWidth {
        name: "tilde",
        width: 333,
    },
    GlyphWidth {
        name: "trademark",
        width: 980,
    },
    GlyphWidth {
        name: "two",
        width: 500,
    },
    GlyphWidth {
        name: "twosuperior",
        width: 300,
    },
    GlyphWidth {
        name: "u",
        width: 500,
    },
    GlyphWidth {
        name: "uacute",
        width: 500,
    },
    GlyphWidth {
        name: "ucircumflex",
        width: 500,
    },
    GlyphWidth {
        name: "udieresis",
        width: 500,
    },
    GlyphWidth {
        name: "ugrave",
        width: 500,
    },
    GlyphWidth {
        name: "uhungarumlaut",
        width: 500,
    },
    GlyphWidth {
        name: "umacron",
        width: 500,
    },
    GlyphWidth {
        name: "underscore",
        width: 500,
    },
    GlyphWidth {
        name: "uogonek",
        width: 500,
    },
    GlyphWidth {
        name: "uring",
        width: 500,
    },
    GlyphWidth {
        name: "v",
        width: 444,
    },
    GlyphWidth {
        name: "w",
        width: 667,
    },
    GlyphWidth {
        name: "x",
        width: 444,
    },
    GlyphWidth {
        name: "y",
        width: 444,
    },
    GlyphWidth {
        name: "yacute",
        width: 444,
    },
    GlyphWidth {
        name: "ydieresis",
        width: 444,
    },
    GlyphWidth {
        name: "yen",
        width: 500,
    },
    GlyphWidth {
        name: "z",
        width: 389,
    },
    GlyphWidth {
        name: "zacute",
        width: 389,
    },
    GlyphWidth {
        name: "zcaron",
        width: 389,
    },
    GlyphWidth {
        name: "zdotaccent",
        width: 389,
    },
    GlyphWidth {
        name: "zero",
        width: 500,
    },
];

/// `ZapfDingbats` — 202 glyphs.
pub const ZapfDingbats: &[GlyphWidth] = &[
    GlyphWidth {
        name: "a1",
        width: 974,
    },
    GlyphWidth {
        name: "a10",
        width: 692,
    },
    GlyphWidth {
        name: "a100",
        width: 668,
    },
    GlyphWidth {
        name: "a101",
        width: 732,
    },
    GlyphWidth {
        name: "a102",
        width: 544,
    },
    GlyphWidth {
        name: "a103",
        width: 544,
    },
    GlyphWidth {
        name: "a104",
        width: 910,
    },
    GlyphWidth {
        name: "a105",
        width: 911,
    },
    GlyphWidth {
        name: "a106",
        width: 667,
    },
    GlyphWidth {
        name: "a107",
        width: 760,
    },
    GlyphWidth {
        name: "a108",
        width: 760,
    },
    GlyphWidth {
        name: "a109",
        width: 626,
    },
    GlyphWidth {
        name: "a11",
        width: 960,
    },
    GlyphWidth {
        name: "a110",
        width: 694,
    },
    GlyphWidth {
        name: "a111",
        width: 595,
    },
    GlyphWidth {
        name: "a112",
        width: 776,
    },
    GlyphWidth {
        name: "a117",
        width: 690,
    },
    GlyphWidth {
        name: "a118",
        width: 791,
    },
    GlyphWidth {
        name: "a119",
        width: 790,
    },
    GlyphWidth {
        name: "a12",
        width: 939,
    },
    GlyphWidth {
        name: "a120",
        width: 788,
    },
    GlyphWidth {
        name: "a121",
        width: 788,
    },
    GlyphWidth {
        name: "a122",
        width: 788,
    },
    GlyphWidth {
        name: "a123",
        width: 788,
    },
    GlyphWidth {
        name: "a124",
        width: 788,
    },
    GlyphWidth {
        name: "a125",
        width: 788,
    },
    GlyphWidth {
        name: "a126",
        width: 788,
    },
    GlyphWidth {
        name: "a127",
        width: 788,
    },
    GlyphWidth {
        name: "a128",
        width: 788,
    },
    GlyphWidth {
        name: "a129",
        width: 788,
    },
    GlyphWidth {
        name: "a13",
        width: 549,
    },
    GlyphWidth {
        name: "a130",
        width: 788,
    },
    GlyphWidth {
        name: "a131",
        width: 788,
    },
    GlyphWidth {
        name: "a132",
        width: 788,
    },
    GlyphWidth {
        name: "a133",
        width: 788,
    },
    GlyphWidth {
        name: "a134",
        width: 788,
    },
    GlyphWidth {
        name: "a135",
        width: 788,
    },
    GlyphWidth {
        name: "a136",
        width: 788,
    },
    GlyphWidth {
        name: "a137",
        width: 788,
    },
    GlyphWidth {
        name: "a138",
        width: 788,
    },
    GlyphWidth {
        name: "a139",
        width: 788,
    },
    GlyphWidth {
        name: "a14",
        width: 855,
    },
    GlyphWidth {
        name: "a140",
        width: 788,
    },
    GlyphWidth {
        name: "a141",
        width: 788,
    },
    GlyphWidth {
        name: "a142",
        width: 788,
    },
    GlyphWidth {
        name: "a143",
        width: 788,
    },
    GlyphWidth {
        name: "a144",
        width: 788,
    },
    GlyphWidth {
        name: "a145",
        width: 788,
    },
    GlyphWidth {
        name: "a146",
        width: 788,
    },
    GlyphWidth {
        name: "a147",
        width: 788,
    },
    GlyphWidth {
        name: "a148",
        width: 788,
    },
    GlyphWidth {
        name: "a149",
        width: 788,
    },
    GlyphWidth {
        name: "a15",
        width: 911,
    },
    GlyphWidth {
        name: "a150",
        width: 788,
    },
    GlyphWidth {
        name: "a151",
        width: 788,
    },
    GlyphWidth {
        name: "a152",
        width: 788,
    },
    GlyphWidth {
        name: "a153",
        width: 788,
    },
    GlyphWidth {
        name: "a154",
        width: 788,
    },
    GlyphWidth {
        name: "a155",
        width: 788,
    },
    GlyphWidth {
        name: "a156",
        width: 788,
    },
    GlyphWidth {
        name: "a157",
        width: 788,
    },
    GlyphWidth {
        name: "a158",
        width: 788,
    },
    GlyphWidth {
        name: "a159",
        width: 788,
    },
    GlyphWidth {
        name: "a16",
        width: 933,
    },
    GlyphWidth {
        name: "a160",
        width: 894,
    },
    GlyphWidth {
        name: "a161",
        width: 838,
    },
    GlyphWidth {
        name: "a162",
        width: 924,
    },
    GlyphWidth {
        name: "a163",
        width: 1016,
    },
    GlyphWidth {
        name: "a164",
        width: 458,
    },
    GlyphWidth {
        name: "a165",
        width: 924,
    },
    GlyphWidth {
        name: "a166",
        width: 918,
    },
    GlyphWidth {
        name: "a167",
        width: 927,
    },
    GlyphWidth {
        name: "a168",
        width: 928,
    },
    GlyphWidth {
        name: "a169",
        width: 928,
    },
    GlyphWidth {
        name: "a17",
        width: 945,
    },
    GlyphWidth {
        name: "a170",
        width: 834,
    },
    GlyphWidth {
        name: "a171",
        width: 873,
    },
    GlyphWidth {
        name: "a172",
        width: 828,
    },
    GlyphWidth {
        name: "a173",
        width: 924,
    },
    GlyphWidth {
        name: "a174",
        width: 917,
    },
    GlyphWidth {
        name: "a175",
        width: 930,
    },
    GlyphWidth {
        name: "a176",
        width: 931,
    },
    GlyphWidth {
        name: "a177",
        width: 463,
    },
    GlyphWidth {
        name: "a178",
        width: 883,
    },
    GlyphWidth {
        name: "a179",
        width: 836,
    },
    GlyphWidth {
        name: "a18",
        width: 974,
    },
    GlyphWidth {
        name: "a180",
        width: 867,
    },
    GlyphWidth {
        name: "a181",
        width: 696,
    },
    GlyphWidth {
        name: "a182",
        width: 874,
    },
    GlyphWidth {
        name: "a183",
        width: 760,
    },
    GlyphWidth {
        name: "a184",
        width: 946,
    },
    GlyphWidth {
        name: "a185",
        width: 865,
    },
    GlyphWidth {
        name: "a186",
        width: 967,
    },
    GlyphWidth {
        name: "a187",
        width: 831,
    },
    GlyphWidth {
        name: "a188",
        width: 873,
    },
    GlyphWidth {
        name: "a189",
        width: 927,
    },
    GlyphWidth {
        name: "a19",
        width: 755,
    },
    GlyphWidth {
        name: "a190",
        width: 970,
    },
    GlyphWidth {
        name: "a191",
        width: 918,
    },
    GlyphWidth {
        name: "a192",
        width: 748,
    },
    GlyphWidth {
        name: "a193",
        width: 836,
    },
    GlyphWidth {
        name: "a194",
        width: 771,
    },
    GlyphWidth {
        name: "a195",
        width: 888,
    },
    GlyphWidth {
        name: "a196",
        width: 748,
    },
    GlyphWidth {
        name: "a197",
        width: 771,
    },
    GlyphWidth {
        name: "a198",
        width: 888,
    },
    GlyphWidth {
        name: "a199",
        width: 867,
    },
    GlyphWidth {
        name: "a2",
        width: 961,
    },
    GlyphWidth {
        name: "a20",
        width: 846,
    },
    GlyphWidth {
        name: "a200",
        width: 696,
    },
    GlyphWidth {
        name: "a201",
        width: 874,
    },
    GlyphWidth {
        name: "a202",
        width: 974,
    },
    GlyphWidth {
        name: "a203",
        width: 762,
    },
    GlyphWidth {
        name: "a204",
        width: 759,
    },
    GlyphWidth {
        name: "a205",
        width: 509,
    },
    GlyphWidth {
        name: "a206",
        width: 410,
    },
    GlyphWidth {
        name: "a21",
        width: 762,
    },
    GlyphWidth {
        name: "a22",
        width: 761,
    },
    GlyphWidth {
        name: "a23",
        width: 571,
    },
    GlyphWidth {
        name: "a24",
        width: 677,
    },
    GlyphWidth {
        name: "a25",
        width: 763,
    },
    GlyphWidth {
        name: "a26",
        width: 760,
    },
    GlyphWidth {
        name: "a27",
        width: 759,
    },
    GlyphWidth {
        name: "a28",
        width: 754,
    },
    GlyphWidth {
        name: "a29",
        width: 786,
    },
    GlyphWidth {
        name: "a3",
        width: 980,
    },
    GlyphWidth {
        name: "a30",
        width: 788,
    },
    GlyphWidth {
        name: "a31",
        width: 788,
    },
    GlyphWidth {
        name: "a32",
        width: 790,
    },
    GlyphWidth {
        name: "a33",
        width: 793,
    },
    GlyphWidth {
        name: "a34",
        width: 794,
    },
    GlyphWidth {
        name: "a35",
        width: 816,
    },
    GlyphWidth {
        name: "a36",
        width: 823,
    },
    GlyphWidth {
        name: "a37",
        width: 789,
    },
    GlyphWidth {
        name: "a38",
        width: 841,
    },
    GlyphWidth {
        name: "a39",
        width: 823,
    },
    GlyphWidth {
        name: "a4",
        width: 719,
    },
    GlyphWidth {
        name: "a40",
        width: 833,
    },
    GlyphWidth {
        name: "a41",
        width: 816,
    },
    GlyphWidth {
        name: "a42",
        width: 831,
    },
    GlyphWidth {
        name: "a43",
        width: 923,
    },
    GlyphWidth {
        name: "a44",
        width: 744,
    },
    GlyphWidth {
        name: "a45",
        width: 723,
    },
    GlyphWidth {
        name: "a46",
        width: 749,
    },
    GlyphWidth {
        name: "a47",
        width: 790,
    },
    GlyphWidth {
        name: "a48",
        width: 792,
    },
    GlyphWidth {
        name: "a49",
        width: 695,
    },
    GlyphWidth {
        name: "a5",
        width: 789,
    },
    GlyphWidth {
        name: "a50",
        width: 776,
    },
    GlyphWidth {
        name: "a51",
        width: 768,
    },
    GlyphWidth {
        name: "a52",
        width: 792,
    },
    GlyphWidth {
        name: "a53",
        width: 759,
    },
    GlyphWidth {
        name: "a54",
        width: 707,
    },
    GlyphWidth {
        name: "a55",
        width: 708,
    },
    GlyphWidth {
        name: "a56",
        width: 682,
    },
    GlyphWidth {
        name: "a57",
        width: 701,
    },
    GlyphWidth {
        name: "a58",
        width: 826,
    },
    GlyphWidth {
        name: "a59",
        width: 815,
    },
    GlyphWidth {
        name: "a6",
        width: 494,
    },
    GlyphWidth {
        name: "a60",
        width: 789,
    },
    GlyphWidth {
        name: "a61",
        width: 789,
    },
    GlyphWidth {
        name: "a62",
        width: 707,
    },
    GlyphWidth {
        name: "a63",
        width: 687,
    },
    GlyphWidth {
        name: "a64",
        width: 696,
    },
    GlyphWidth {
        name: "a65",
        width: 689,
    },
    GlyphWidth {
        name: "a66",
        width: 786,
    },
    GlyphWidth {
        name: "a67",
        width: 787,
    },
    GlyphWidth {
        name: "a68",
        width: 713,
    },
    GlyphWidth {
        name: "a69",
        width: 791,
    },
    GlyphWidth {
        name: "a7",
        width: 552,
    },
    GlyphWidth {
        name: "a70",
        width: 785,
    },
    GlyphWidth {
        name: "a71",
        width: 791,
    },
    GlyphWidth {
        name: "a72",
        width: 873,
    },
    GlyphWidth {
        name: "a73",
        width: 761,
    },
    GlyphWidth {
        name: "a74",
        width: 762,
    },
    GlyphWidth {
        name: "a75",
        width: 759,
    },
    GlyphWidth {
        name: "a76",
        width: 892,
    },
    GlyphWidth {
        name: "a77",
        width: 892,
    },
    GlyphWidth {
        name: "a78",
        width: 788,
    },
    GlyphWidth {
        name: "a79",
        width: 784,
    },
    GlyphWidth {
        name: "a8",
        width: 537,
    },
    GlyphWidth {
        name: "a81",
        width: 438,
    },
    GlyphWidth {
        name: "a82",
        width: 138,
    },
    GlyphWidth {
        name: "a83",
        width: 277,
    },
    GlyphWidth {
        name: "a84",
        width: 415,
    },
    GlyphWidth {
        name: "a85",
        width: 509,
    },
    GlyphWidth {
        name: "a86",
        width: 410,
    },
    GlyphWidth {
        name: "a87",
        width: 234,
    },
    GlyphWidth {
        name: "a88",
        width: 234,
    },
    GlyphWidth {
        name: "a89",
        width: 390,
    },
    GlyphWidth {
        name: "a9",
        width: 577,
    },
    GlyphWidth {
        name: "a90",
        width: 390,
    },
    GlyphWidth {
        name: "a91",
        width: 276,
    },
    GlyphWidth {
        name: "a92",
        width: 276,
    },
    GlyphWidth {
        name: "a93",
        width: 317,
    },
    GlyphWidth {
        name: "a94",
        width: 317,
    },
    GlyphWidth {
        name: "a95",
        width: 334,
    },
    GlyphWidth {
        name: "a96",
        width: 334,
    },
    GlyphWidth {
        name: "a97",
        width: 392,
    },
    GlyphWidth {
        name: "a98",
        width: 392,
    },
    GlyphWidth {
        name: "a99",
        width: 668,
    },
    GlyphWidth {
        name: "space",
        width: 278,
    },
];

/// All 14 standard font metrics, one per font name.
pub const ALL_FONTS: &[FontMetrics] = &[
    FontMetrics {
        name: "Courier",
        uniform: Some(600),
        glyphs: Courier,
    },
    FontMetrics {
        name: "Courier-Bold",
        uniform: Some(600),
        glyphs: Courier_Bold,
    },
    FontMetrics {
        name: "Courier-BoldOblique",
        uniform: Some(600),
        glyphs: Courier_BoldOblique,
    },
    FontMetrics {
        name: "Courier-Oblique",
        uniform: Some(600),
        glyphs: Courier_Oblique,
    },
    FontMetrics {
        name: "Helvetica",
        uniform: None,
        glyphs: Helvetica,
    },
    FontMetrics {
        name: "Helvetica-Bold",
        uniform: None,
        glyphs: Helvetica_Bold,
    },
    FontMetrics {
        name: "Helvetica-BoldOblique",
        uniform: None,
        glyphs: Helvetica_BoldOblique,
    },
    FontMetrics {
        name: "Helvetica-Oblique",
        uniform: None,
        glyphs: Helvetica_Oblique,
    },
    FontMetrics {
        name: "Symbol",
        uniform: None,
        glyphs: Symbol,
    },
    FontMetrics {
        name: "Times-Roman",
        uniform: None,
        glyphs: Times_Roman,
    },
    FontMetrics {
        name: "Times-Bold",
        uniform: None,
        glyphs: Times_Bold,
    },
    FontMetrics {
        name: "Times-BoldItalic",
        uniform: None,
        glyphs: Times_BoldItalic,
    },
    FontMetrics {
        name: "Times-Italic",
        uniform: None,
        glyphs: Times_Italic,
    },
    FontMetrics {
        name: "ZapfDingbats",
        uniform: None,
        glyphs: ZapfDingbats,
    },
];

/// Look up a glyph's advance width for a standard-14 font.
///
/// Returns `None` when the font name is not a standard-14 font or the
/// glyph name is not in that font's AFM table.
#[must_use]
pub fn width(font_name: &str, glyph_name: &str) -> Option<u16> {
    let font = ALL_FONTS.iter().find(|f| f.name == font_name)?;
    if let Some(uniform) = font.uniform {
        return Some(uniform); // Courier: every glyph is 600 wide
    }
    font.glyphs
        .binary_search_by(|g| g.name.cmp(glyph_name))
        .ok()
        .and_then(|i| font.glyphs.get(i).map(|g| g.width))
}

/// Whether a font name is one of the standard-14.
#[must_use]
pub fn is_standard(font_name: &str) -> bool {
    ALL_FONTS.iter().any(|f| f.name == font_name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn helvetica_spot_checks() {
        assert_eq!(width("Helvetica", "space"), Some(278));
        assert_eq!(width("Helvetica", "A"), Some(667));
        assert_eq!(width("Helvetica", "a"), Some(556));
        assert_eq!(width("Helvetica", "nonexistent"), None);
    }

    #[test]
    fn courier_is_uniform() {
        assert_eq!(width("Courier", "space"), Some(600));
        assert_eq!(width("Courier", "A"), Some(600));
        assert_eq!(width("Courier", "nonexistent"), Some(600));
        assert_eq!(width("Courier-Bold", "A"), Some(600));
    }

    #[test]
    fn unknown_font_returns_none() {
        assert_eq!(width("Nonexistent", "space"), None);
    }

    #[test]
    fn is_standard_recognises_all() {
        for name in [
            "Courier",
            "Courier-Bold",
            "Courier-BoldOblique",
            "Courier-Oblique",
            "Helvetica",
            "Helvetica-Bold",
            "Helvetica-BoldOblique",
            "Helvetica-Oblique",
            "Symbol",
            "Times-Roman",
            "Times-Bold",
            "Times-BoldItalic",
            "Times-Italic",
            "ZapfDingbats",
        ] {
            assert!(is_standard(name), "{name} should be standard");
        }
        assert!(!is_standard("Nonexistent"));
    }
}
