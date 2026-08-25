//! Adobe Glyph List (SL-3.TEXT.02) — glyph name → Unicode.
//!
//! The AGL maps PostScript glyph names to their Unicode scalar values.
//! Used for text recovery when neither `/ToUnicode` nor the encoding
//! resolves the glyph's character. 586 entries, sorted by name for binary
//! search. Generated from fontTools `agl.AGL2UV`.

/// A glyph-name → Unicode entry.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AglEntry {
    /// The PostScript glyph name.
    pub name: &'static str,
    /// The Unicode scalar value.
    pub unicode: u32,
}

/// The full AGL, sorted by name.
pub const AGL: &[AglEntry] = &[
    AglEntry {
        name: "A",
        unicode: 0x0041,
    },
    AglEntry {
        name: "AE",
        unicode: 0x00C6,
    },
    AglEntry {
        name: "AEacute",
        unicode: 0x01FC,
    },
    AglEntry {
        name: "Aacute",
        unicode: 0x00C1,
    },
    AglEntry {
        name: "Abreve",
        unicode: 0x0102,
    },
    AglEntry {
        name: "Acircumflex",
        unicode: 0x00C2,
    },
    AglEntry {
        name: "Adieresis",
        unicode: 0x00C4,
    },
    AglEntry {
        name: "Agrave",
        unicode: 0x00C0,
    },
    AglEntry {
        name: "Alpha",
        unicode: 0x0391,
    },
    AglEntry {
        name: "Alphatonos",
        unicode: 0x0386,
    },
    AglEntry {
        name: "Amacron",
        unicode: 0x0100,
    },
    AglEntry {
        name: "Aogonek",
        unicode: 0x0104,
    },
    AglEntry {
        name: "Aring",
        unicode: 0x00C5,
    },
    AglEntry {
        name: "Aringacute",
        unicode: 0x01FA,
    },
    AglEntry {
        name: "Atilde",
        unicode: 0x00C3,
    },
    AglEntry {
        name: "B",
        unicode: 0x0042,
    },
    AglEntry {
        name: "Beta",
        unicode: 0x0392,
    },
    AglEntry {
        name: "C",
        unicode: 0x0043,
    },
    AglEntry {
        name: "Cacute",
        unicode: 0x0106,
    },
    AglEntry {
        name: "Ccaron",
        unicode: 0x010C,
    },
    AglEntry {
        name: "Ccedilla",
        unicode: 0x00C7,
    },
    AglEntry {
        name: "Ccircumflex",
        unicode: 0x0108,
    },
    AglEntry {
        name: "Cdotaccent",
        unicode: 0x010A,
    },
    AglEntry {
        name: "Chi",
        unicode: 0x03A7,
    },
    AglEntry {
        name: "D",
        unicode: 0x0044,
    },
    AglEntry {
        name: "Dcaron",
        unicode: 0x010E,
    },
    AglEntry {
        name: "Dcroat",
        unicode: 0x0110,
    },
    AglEntry {
        name: "Delta",
        unicode: 0x2206,
    },
    AglEntry {
        name: "E",
        unicode: 0x0045,
    },
    AglEntry {
        name: "Eacute",
        unicode: 0x00C9,
    },
    AglEntry {
        name: "Ebreve",
        unicode: 0x0114,
    },
    AglEntry {
        name: "Ecaron",
        unicode: 0x011A,
    },
    AglEntry {
        name: "Ecircumflex",
        unicode: 0x00CA,
    },
    AglEntry {
        name: "Edieresis",
        unicode: 0x00CB,
    },
    AglEntry {
        name: "Edotaccent",
        unicode: 0x0116,
    },
    AglEntry {
        name: "Egrave",
        unicode: 0x00C8,
    },
    AglEntry {
        name: "Emacron",
        unicode: 0x0112,
    },
    AglEntry {
        name: "Eng",
        unicode: 0x014A,
    },
    AglEntry {
        name: "Eogonek",
        unicode: 0x0118,
    },
    AglEntry {
        name: "Epsilon",
        unicode: 0x0395,
    },
    AglEntry {
        name: "Epsilontonos",
        unicode: 0x0388,
    },
    AglEntry {
        name: "Eta",
        unicode: 0x0397,
    },
    AglEntry {
        name: "Etatonos",
        unicode: 0x0389,
    },
    AglEntry {
        name: "Eth",
        unicode: 0x00D0,
    },
    AglEntry {
        name: "Euro",
        unicode: 0x20AC,
    },
    AglEntry {
        name: "F",
        unicode: 0x0046,
    },
    AglEntry {
        name: "G",
        unicode: 0x0047,
    },
    AglEntry {
        name: "Gamma",
        unicode: 0x0393,
    },
    AglEntry {
        name: "Gbreve",
        unicode: 0x011E,
    },
    AglEntry {
        name: "Gcaron",
        unicode: 0x01E6,
    },
    AglEntry {
        name: "Gcircumflex",
        unicode: 0x011C,
    },
    AglEntry {
        name: "Gdotaccent",
        unicode: 0x0120,
    },
    AglEntry {
        name: "H",
        unicode: 0x0048,
    },
    AglEntry {
        name: "H18533",
        unicode: 0x25CF,
    },
    AglEntry {
        name: "H18543",
        unicode: 0x25AA,
    },
    AglEntry {
        name: "H18551",
        unicode: 0x25AB,
    },
    AglEntry {
        name: "H22073",
        unicode: 0x25A1,
    },
    AglEntry {
        name: "Hbar",
        unicode: 0x0126,
    },
    AglEntry {
        name: "Hcircumflex",
        unicode: 0x0124,
    },
    AglEntry {
        name: "I",
        unicode: 0x0049,
    },
    AglEntry {
        name: "IJ",
        unicode: 0x0132,
    },
    AglEntry {
        name: "Iacute",
        unicode: 0x00CD,
    },
    AglEntry {
        name: "Ibreve",
        unicode: 0x012C,
    },
    AglEntry {
        name: "Icircumflex",
        unicode: 0x00CE,
    },
    AglEntry {
        name: "Idieresis",
        unicode: 0x00CF,
    },
    AglEntry {
        name: "Idotaccent",
        unicode: 0x0130,
    },
    AglEntry {
        name: "Ifraktur",
        unicode: 0x2111,
    },
    AglEntry {
        name: "Igrave",
        unicode: 0x00CC,
    },
    AglEntry {
        name: "Imacron",
        unicode: 0x012A,
    },
    AglEntry {
        name: "Iogonek",
        unicode: 0x012E,
    },
    AglEntry {
        name: "Iota",
        unicode: 0x0399,
    },
    AglEntry {
        name: "Iotadieresis",
        unicode: 0x03AA,
    },
    AglEntry {
        name: "Iotatonos",
        unicode: 0x038A,
    },
    AglEntry {
        name: "Itilde",
        unicode: 0x0128,
    },
    AglEntry {
        name: "J",
        unicode: 0x004A,
    },
    AglEntry {
        name: "Jcircumflex",
        unicode: 0x0134,
    },
    AglEntry {
        name: "K",
        unicode: 0x004B,
    },
    AglEntry {
        name: "Kappa",
        unicode: 0x039A,
    },
    AglEntry {
        name: "L",
        unicode: 0x004C,
    },
    AglEntry {
        name: "Lacute",
        unicode: 0x0139,
    },
    AglEntry {
        name: "Lambda",
        unicode: 0x039B,
    },
    AglEntry {
        name: "Lcaron",
        unicode: 0x013D,
    },
    AglEntry {
        name: "Ldot",
        unicode: 0x013F,
    },
    AglEntry {
        name: "Lslash",
        unicode: 0x0141,
    },
    AglEntry {
        name: "M",
        unicode: 0x004D,
    },
    AglEntry {
        name: "Mu",
        unicode: 0x039C,
    },
    AglEntry {
        name: "N",
        unicode: 0x004E,
    },
    AglEntry {
        name: "Nacute",
        unicode: 0x0143,
    },
    AglEntry {
        name: "Ncaron",
        unicode: 0x0147,
    },
    AglEntry {
        name: "Ntilde",
        unicode: 0x00D1,
    },
    AglEntry {
        name: "Nu",
        unicode: 0x039D,
    },
    AglEntry {
        name: "O",
        unicode: 0x004F,
    },
    AglEntry {
        name: "OE",
        unicode: 0x0152,
    },
    AglEntry {
        name: "Oacute",
        unicode: 0x00D3,
    },
    AglEntry {
        name: "Obreve",
        unicode: 0x014E,
    },
    AglEntry {
        name: "Ocircumflex",
        unicode: 0x00D4,
    },
    AglEntry {
        name: "Odieresis",
        unicode: 0x00D6,
    },
    AglEntry {
        name: "Ograve",
        unicode: 0x00D2,
    },
    AglEntry {
        name: "Ohorn",
        unicode: 0x01A0,
    },
    AglEntry {
        name: "Ohungarumlaut",
        unicode: 0x0150,
    },
    AglEntry {
        name: "Omacron",
        unicode: 0x014C,
    },
    AglEntry {
        name: "Omega",
        unicode: 0x2126,
    },
    AglEntry {
        name: "Omegatonos",
        unicode: 0x038F,
    },
    AglEntry {
        name: "Omicron",
        unicode: 0x039F,
    },
    AglEntry {
        name: "Omicrontonos",
        unicode: 0x038C,
    },
    AglEntry {
        name: "Oslash",
        unicode: 0x00D8,
    },
    AglEntry {
        name: "Oslashacute",
        unicode: 0x01FE,
    },
    AglEntry {
        name: "Otilde",
        unicode: 0x00D5,
    },
    AglEntry {
        name: "P",
        unicode: 0x0050,
    },
    AglEntry {
        name: "Phi",
        unicode: 0x03A6,
    },
    AglEntry {
        name: "Pi",
        unicode: 0x03A0,
    },
    AglEntry {
        name: "Psi",
        unicode: 0x03A8,
    },
    AglEntry {
        name: "Q",
        unicode: 0x0051,
    },
    AglEntry {
        name: "R",
        unicode: 0x0052,
    },
    AglEntry {
        name: "Racute",
        unicode: 0x0154,
    },
    AglEntry {
        name: "Rcaron",
        unicode: 0x0158,
    },
    AglEntry {
        name: "Rfraktur",
        unicode: 0x211C,
    },
    AglEntry {
        name: "Rho",
        unicode: 0x03A1,
    },
    AglEntry {
        name: "S",
        unicode: 0x0053,
    },
    AglEntry {
        name: "SF010000",
        unicode: 0x250C,
    },
    AglEntry {
        name: "SF020000",
        unicode: 0x2514,
    },
    AglEntry {
        name: "SF030000",
        unicode: 0x2510,
    },
    AglEntry {
        name: "SF040000",
        unicode: 0x2518,
    },
    AglEntry {
        name: "SF050000",
        unicode: 0x253C,
    },
    AglEntry {
        name: "SF060000",
        unicode: 0x252C,
    },
    AglEntry {
        name: "SF070000",
        unicode: 0x2534,
    },
    AglEntry {
        name: "SF080000",
        unicode: 0x251C,
    },
    AglEntry {
        name: "SF090000",
        unicode: 0x2524,
    },
    AglEntry {
        name: "SF100000",
        unicode: 0x2500,
    },
    AglEntry {
        name: "SF110000",
        unicode: 0x2502,
    },
    AglEntry {
        name: "SF190000",
        unicode: 0x2561,
    },
    AglEntry {
        name: "SF200000",
        unicode: 0x2562,
    },
    AglEntry {
        name: "SF210000",
        unicode: 0x2556,
    },
    AglEntry {
        name: "SF220000",
        unicode: 0x2555,
    },
    AglEntry {
        name: "SF230000",
        unicode: 0x2563,
    },
    AglEntry {
        name: "SF240000",
        unicode: 0x2551,
    },
    AglEntry {
        name: "SF250000",
        unicode: 0x2557,
    },
    AglEntry {
        name: "SF260000",
        unicode: 0x255D,
    },
    AglEntry {
        name: "SF270000",
        unicode: 0x255C,
    },
    AglEntry {
        name: "SF280000",
        unicode: 0x255B,
    },
    AglEntry {
        name: "SF360000",
        unicode: 0x255E,
    },
    AglEntry {
        name: "SF370000",
        unicode: 0x255F,
    },
    AglEntry {
        name: "SF380000",
        unicode: 0x255A,
    },
    AglEntry {
        name: "SF390000",
        unicode: 0x2554,
    },
    AglEntry {
        name: "SF400000",
        unicode: 0x2569,
    },
    AglEntry {
        name: "SF410000",
        unicode: 0x2566,
    },
    AglEntry {
        name: "SF420000",
        unicode: 0x2560,
    },
    AglEntry {
        name: "SF430000",
        unicode: 0x2550,
    },
    AglEntry {
        name: "SF440000",
        unicode: 0x256C,
    },
    AglEntry {
        name: "SF450000",
        unicode: 0x2567,
    },
    AglEntry {
        name: "SF460000",
        unicode: 0x2568,
    },
    AglEntry {
        name: "SF470000",
        unicode: 0x2564,
    },
    AglEntry {
        name: "SF480000",
        unicode: 0x2565,
    },
    AglEntry {
        name: "SF490000",
        unicode: 0x2559,
    },
    AglEntry {
        name: "SF500000",
        unicode: 0x2558,
    },
    AglEntry {
        name: "SF510000",
        unicode: 0x2552,
    },
    AglEntry {
        name: "SF520000",
        unicode: 0x2553,
    },
    AglEntry {
        name: "SF530000",
        unicode: 0x256B,
    },
    AglEntry {
        name: "SF540000",
        unicode: 0x256A,
    },
    AglEntry {
        name: "Sacute",
        unicode: 0x015A,
    },
    AglEntry {
        name: "Scaron",
        unicode: 0x0160,
    },
    AglEntry {
        name: "Scedilla",
        unicode: 0x015E,
    },
    AglEntry {
        name: "Scircumflex",
        unicode: 0x015C,
    },
    AglEntry {
        name: "Sigma",
        unicode: 0x03A3,
    },
    AglEntry {
        name: "T",
        unicode: 0x0054,
    },
    AglEntry {
        name: "Tau",
        unicode: 0x03A4,
    },
    AglEntry {
        name: "Tbar",
        unicode: 0x0166,
    },
    AglEntry {
        name: "Tcaron",
        unicode: 0x0164,
    },
    AglEntry {
        name: "Theta",
        unicode: 0x0398,
    },
    AglEntry {
        name: "Thorn",
        unicode: 0x00DE,
    },
    AglEntry {
        name: "U",
        unicode: 0x0055,
    },
    AglEntry {
        name: "Uacute",
        unicode: 0x00DA,
    },
    AglEntry {
        name: "Ubreve",
        unicode: 0x016C,
    },
    AglEntry {
        name: "Ucircumflex",
        unicode: 0x00DB,
    },
    AglEntry {
        name: "Udieresis",
        unicode: 0x00DC,
    },
    AglEntry {
        name: "Ugrave",
        unicode: 0x00D9,
    },
    AglEntry {
        name: "Uhorn",
        unicode: 0x01AF,
    },
    AglEntry {
        name: "Uhungarumlaut",
        unicode: 0x0170,
    },
    AglEntry {
        name: "Umacron",
        unicode: 0x016A,
    },
    AglEntry {
        name: "Uogonek",
        unicode: 0x0172,
    },
    AglEntry {
        name: "Upsilon",
        unicode: 0x03A5,
    },
    AglEntry {
        name: "Upsilon1",
        unicode: 0x03D2,
    },
    AglEntry {
        name: "Upsilondieresis",
        unicode: 0x03AB,
    },
    AglEntry {
        name: "Upsilontonos",
        unicode: 0x038E,
    },
    AglEntry {
        name: "Uring",
        unicode: 0x016E,
    },
    AglEntry {
        name: "Utilde",
        unicode: 0x0168,
    },
    AglEntry {
        name: "V",
        unicode: 0x0056,
    },
    AglEntry {
        name: "W",
        unicode: 0x0057,
    },
    AglEntry {
        name: "Wacute",
        unicode: 0x1E82,
    },
    AglEntry {
        name: "Wcircumflex",
        unicode: 0x0174,
    },
    AglEntry {
        name: "Wdieresis",
        unicode: 0x1E84,
    },
    AglEntry {
        name: "Wgrave",
        unicode: 0x1E80,
    },
    AglEntry {
        name: "X",
        unicode: 0x0058,
    },
    AglEntry {
        name: "Xi",
        unicode: 0x039E,
    },
    AglEntry {
        name: "Y",
        unicode: 0x0059,
    },
    AglEntry {
        name: "Yacute",
        unicode: 0x00DD,
    },
    AglEntry {
        name: "Ycircumflex",
        unicode: 0x0176,
    },
    AglEntry {
        name: "Ydieresis",
        unicode: 0x0178,
    },
    AglEntry {
        name: "Ygrave",
        unicode: 0x1EF2,
    },
    AglEntry {
        name: "Z",
        unicode: 0x005A,
    },
    AglEntry {
        name: "Zacute",
        unicode: 0x0179,
    },
    AglEntry {
        name: "Zcaron",
        unicode: 0x017D,
    },
    AglEntry {
        name: "Zdotaccent",
        unicode: 0x017B,
    },
    AglEntry {
        name: "Zeta",
        unicode: 0x0396,
    },
    AglEntry {
        name: "a",
        unicode: 0x0061,
    },
    AglEntry {
        name: "aacute",
        unicode: 0x00E1,
    },
    AglEntry {
        name: "abreve",
        unicode: 0x0103,
    },
    AglEntry {
        name: "acircumflex",
        unicode: 0x00E2,
    },
    AglEntry {
        name: "acute",
        unicode: 0x00B4,
    },
    AglEntry {
        name: "acutecomb",
        unicode: 0x0301,
    },
    AglEntry {
        name: "adieresis",
        unicode: 0x00E4,
    },
    AglEntry {
        name: "ae",
        unicode: 0x00E6,
    },
    AglEntry {
        name: "aeacute",
        unicode: 0x01FD,
    },
    AglEntry {
        name: "agrave",
        unicode: 0x00E0,
    },
    AglEntry {
        name: "aleph",
        unicode: 0x2135,
    },
    AglEntry {
        name: "alpha",
        unicode: 0x03B1,
    },
    AglEntry {
        name: "alphatonos",
        unicode: 0x03AC,
    },
    AglEntry {
        name: "amacron",
        unicode: 0x0101,
    },
    AglEntry {
        name: "ampersand",
        unicode: 0x0026,
    },
    AglEntry {
        name: "angle",
        unicode: 0x2220,
    },
    AglEntry {
        name: "angleleft",
        unicode: 0x2329,
    },
    AglEntry {
        name: "angleright",
        unicode: 0x232A,
    },
    AglEntry {
        name: "anoteleia",
        unicode: 0x0387,
    },
    AglEntry {
        name: "aogonek",
        unicode: 0x0105,
    },
    AglEntry {
        name: "approxequal",
        unicode: 0x2248,
    },
    AglEntry {
        name: "aring",
        unicode: 0x00E5,
    },
    AglEntry {
        name: "aringacute",
        unicode: 0x01FB,
    },
    AglEntry {
        name: "arrowboth",
        unicode: 0x2194,
    },
    AglEntry {
        name: "arrowdblboth",
        unicode: 0x21D4,
    },
    AglEntry {
        name: "arrowdbldown",
        unicode: 0x21D3,
    },
    AglEntry {
        name: "arrowdblleft",
        unicode: 0x21D0,
    },
    AglEntry {
        name: "arrowdblright",
        unicode: 0x21D2,
    },
    AglEntry {
        name: "arrowdblup",
        unicode: 0x21D1,
    },
    AglEntry {
        name: "arrowdown",
        unicode: 0x2193,
    },
    AglEntry {
        name: "arrowleft",
        unicode: 0x2190,
    },
    AglEntry {
        name: "arrowright",
        unicode: 0x2192,
    },
    AglEntry {
        name: "arrowup",
        unicode: 0x2191,
    },
    AglEntry {
        name: "arrowupdn",
        unicode: 0x2195,
    },
    AglEntry {
        name: "arrowupdnbse",
        unicode: 0x21A8,
    },
    AglEntry {
        name: "asciicircum",
        unicode: 0x005E,
    },
    AglEntry {
        name: "asciitilde",
        unicode: 0x007E,
    },
    AglEntry {
        name: "asterisk",
        unicode: 0x002A,
    },
    AglEntry {
        name: "asteriskmath",
        unicode: 0x2217,
    },
    AglEntry {
        name: "at",
        unicode: 0x0040,
    },
    AglEntry {
        name: "atilde",
        unicode: 0x00E3,
    },
    AglEntry {
        name: "b",
        unicode: 0x0062,
    },
    AglEntry {
        name: "backslash",
        unicode: 0x005C,
    },
    AglEntry {
        name: "bar",
        unicode: 0x007C,
    },
    AglEntry {
        name: "beta",
        unicode: 0x03B2,
    },
    AglEntry {
        name: "block",
        unicode: 0x2588,
    },
    AglEntry {
        name: "braceleft",
        unicode: 0x007B,
    },
    AglEntry {
        name: "braceright",
        unicode: 0x007D,
    },
    AglEntry {
        name: "bracketleft",
        unicode: 0x005B,
    },
    AglEntry {
        name: "bracketright",
        unicode: 0x005D,
    },
    AglEntry {
        name: "breve",
        unicode: 0x02D8,
    },
    AglEntry {
        name: "brokenbar",
        unicode: 0x00A6,
    },
    AglEntry {
        name: "bullet",
        unicode: 0x2022,
    },
    AglEntry {
        name: "c",
        unicode: 0x0063,
    },
    AglEntry {
        name: "cacute",
        unicode: 0x0107,
    },
    AglEntry {
        name: "caron",
        unicode: 0x02C7,
    },
    AglEntry {
        name: "carriagereturn",
        unicode: 0x21B5,
    },
    AglEntry {
        name: "ccaron",
        unicode: 0x010D,
    },
    AglEntry {
        name: "ccedilla",
        unicode: 0x00E7,
    },
    AglEntry {
        name: "ccircumflex",
        unicode: 0x0109,
    },
    AglEntry {
        name: "cdotaccent",
        unicode: 0x010B,
    },
    AglEntry {
        name: "cedilla",
        unicode: 0x00B8,
    },
    AglEntry {
        name: "cent",
        unicode: 0x00A2,
    },
    AglEntry {
        name: "chi",
        unicode: 0x03C7,
    },
    AglEntry {
        name: "circle",
        unicode: 0x25CB,
    },
    AglEntry {
        name: "circlemultiply",
        unicode: 0x2297,
    },
    AglEntry {
        name: "circleplus",
        unicode: 0x2295,
    },
    AglEntry {
        name: "circumflex",
        unicode: 0x02C6,
    },
    AglEntry {
        name: "club",
        unicode: 0x2663,
    },
    AglEntry {
        name: "colon",
        unicode: 0x003A,
    },
    AglEntry {
        name: "colonmonetary",
        unicode: 0x20A1,
    },
    AglEntry {
        name: "comma",
        unicode: 0x002C,
    },
    AglEntry {
        name: "congruent",
        unicode: 0x2245,
    },
    AglEntry {
        name: "copyright",
        unicode: 0x00A9,
    },
    AglEntry {
        name: "currency",
        unicode: 0x00A4,
    },
    AglEntry {
        name: "d",
        unicode: 0x0064,
    },
    AglEntry {
        name: "dagger",
        unicode: 0x2020,
    },
    AglEntry {
        name: "daggerdbl",
        unicode: 0x2021,
    },
    AglEntry {
        name: "dcaron",
        unicode: 0x010F,
    },
    AglEntry {
        name: "dcroat",
        unicode: 0x0111,
    },
    AglEntry {
        name: "degree",
        unicode: 0x00B0,
    },
    AglEntry {
        name: "delta",
        unicode: 0x03B4,
    },
    AglEntry {
        name: "diamond",
        unicode: 0x2666,
    },
    AglEntry {
        name: "dieresis",
        unicode: 0x00A8,
    },
    AglEntry {
        name: "dieresistonos",
        unicode: 0x0385,
    },
    AglEntry {
        name: "divide",
        unicode: 0x00F7,
    },
    AglEntry {
        name: "dkshade",
        unicode: 0x2593,
    },
    AglEntry {
        name: "dnblock",
        unicode: 0x2584,
    },
    AglEntry {
        name: "dollar",
        unicode: 0x0024,
    },
    AglEntry {
        name: "dong",
        unicode: 0x20AB,
    },
    AglEntry {
        name: "dotaccent",
        unicode: 0x02D9,
    },
    AglEntry {
        name: "dotbelowcomb",
        unicode: 0x0323,
    },
    AglEntry {
        name: "dotlessi",
        unicode: 0x0131,
    },
    AglEntry {
        name: "dotmath",
        unicode: 0x22C5,
    },
    AglEntry {
        name: "e",
        unicode: 0x0065,
    },
    AglEntry {
        name: "eacute",
        unicode: 0x00E9,
    },
    AglEntry {
        name: "ebreve",
        unicode: 0x0115,
    },
    AglEntry {
        name: "ecaron",
        unicode: 0x011B,
    },
    AglEntry {
        name: "ecircumflex",
        unicode: 0x00EA,
    },
    AglEntry {
        name: "edieresis",
        unicode: 0x00EB,
    },
    AglEntry {
        name: "edotaccent",
        unicode: 0x0117,
    },
    AglEntry {
        name: "egrave",
        unicode: 0x00E8,
    },
    AglEntry {
        name: "eight",
        unicode: 0x0038,
    },
    AglEntry {
        name: "element",
        unicode: 0x2208,
    },
    AglEntry {
        name: "ellipsis",
        unicode: 0x2026,
    },
    AglEntry {
        name: "emacron",
        unicode: 0x0113,
    },
    AglEntry {
        name: "emdash",
        unicode: 0x2014,
    },
    AglEntry {
        name: "emptyset",
        unicode: 0x2205,
    },
    AglEntry {
        name: "endash",
        unicode: 0x2013,
    },
    AglEntry {
        name: "eng",
        unicode: 0x014B,
    },
    AglEntry {
        name: "eogonek",
        unicode: 0x0119,
    },
    AglEntry {
        name: "epsilon",
        unicode: 0x03B5,
    },
    AglEntry {
        name: "epsilontonos",
        unicode: 0x03AD,
    },
    AglEntry {
        name: "equal",
        unicode: 0x003D,
    },
    AglEntry {
        name: "equivalence",
        unicode: 0x2261,
    },
    AglEntry {
        name: "estimated",
        unicode: 0x212E,
    },
    AglEntry {
        name: "eta",
        unicode: 0x03B7,
    },
    AglEntry {
        name: "etatonos",
        unicode: 0x03AE,
    },
    AglEntry {
        name: "eth",
        unicode: 0x00F0,
    },
    AglEntry {
        name: "exclam",
        unicode: 0x0021,
    },
    AglEntry {
        name: "exclamdbl",
        unicode: 0x203C,
    },
    AglEntry {
        name: "exclamdown",
        unicode: 0x00A1,
    },
    AglEntry {
        name: "existential",
        unicode: 0x2203,
    },
    AglEntry {
        name: "f",
        unicode: 0x0066,
    },
    AglEntry {
        name: "female",
        unicode: 0x2640,
    },
    AglEntry {
        name: "figuredash",
        unicode: 0x2012,
    },
    AglEntry {
        name: "filledbox",
        unicode: 0x25A0,
    },
    AglEntry {
        name: "filledrect",
        unicode: 0x25AC,
    },
    AglEntry {
        name: "five",
        unicode: 0x0035,
    },
    AglEntry {
        name: "fiveeighths",
        unicode: 0x215D,
    },
    AglEntry {
        name: "florin",
        unicode: 0x0192,
    },
    AglEntry {
        name: "four",
        unicode: 0x0034,
    },
    AglEntry {
        name: "fraction",
        unicode: 0x2044,
    },
    AglEntry {
        name: "franc",
        unicode: 0x20A3,
    },
    AglEntry {
        name: "g",
        unicode: 0x0067,
    },
    AglEntry {
        name: "gamma",
        unicode: 0x03B3,
    },
    AglEntry {
        name: "gbreve",
        unicode: 0x011F,
    },
    AglEntry {
        name: "gcaron",
        unicode: 0x01E7,
    },
    AglEntry {
        name: "gcircumflex",
        unicode: 0x011D,
    },
    AglEntry {
        name: "gdotaccent",
        unicode: 0x0121,
    },
    AglEntry {
        name: "germandbls",
        unicode: 0x00DF,
    },
    AglEntry {
        name: "gradient",
        unicode: 0x2207,
    },
    AglEntry {
        name: "grave",
        unicode: 0x0060,
    },
    AglEntry {
        name: "gravecomb",
        unicode: 0x0300,
    },
    AglEntry {
        name: "greater",
        unicode: 0x003E,
    },
    AglEntry {
        name: "greaterequal",
        unicode: 0x2265,
    },
    AglEntry {
        name: "guillemotleft",
        unicode: 0x00AB,
    },
    AglEntry {
        name: "guillemotright",
        unicode: 0x00BB,
    },
    AglEntry {
        name: "guilsinglleft",
        unicode: 0x2039,
    },
    AglEntry {
        name: "guilsinglright",
        unicode: 0x203A,
    },
    AglEntry {
        name: "h",
        unicode: 0x0068,
    },
    AglEntry {
        name: "hbar",
        unicode: 0x0127,
    },
    AglEntry {
        name: "hcircumflex",
        unicode: 0x0125,
    },
    AglEntry {
        name: "heart",
        unicode: 0x2665,
    },
    AglEntry {
        name: "hookabovecomb",
        unicode: 0x0309,
    },
    AglEntry {
        name: "house",
        unicode: 0x2302,
    },
    AglEntry {
        name: "hungarumlaut",
        unicode: 0x02DD,
    },
    AglEntry {
        name: "hyphen",
        unicode: 0x002D,
    },
    AglEntry {
        name: "i",
        unicode: 0x0069,
    },
    AglEntry {
        name: "iacute",
        unicode: 0x00ED,
    },
    AglEntry {
        name: "ibreve",
        unicode: 0x012D,
    },
    AglEntry {
        name: "icircumflex",
        unicode: 0x00EE,
    },
    AglEntry {
        name: "idieresis",
        unicode: 0x00EF,
    },
    AglEntry {
        name: "igrave",
        unicode: 0x00EC,
    },
    AglEntry {
        name: "ij",
        unicode: 0x0133,
    },
    AglEntry {
        name: "imacron",
        unicode: 0x012B,
    },
    AglEntry {
        name: "infinity",
        unicode: 0x221E,
    },
    AglEntry {
        name: "integral",
        unicode: 0x222B,
    },
    AglEntry {
        name: "integralbt",
        unicode: 0x2321,
    },
    AglEntry {
        name: "integraltp",
        unicode: 0x2320,
    },
    AglEntry {
        name: "intersection",
        unicode: 0x2229,
    },
    AglEntry {
        name: "invbullet",
        unicode: 0x25D8,
    },
    AglEntry {
        name: "invcircle",
        unicode: 0x25D9,
    },
    AglEntry {
        name: "invsmileface",
        unicode: 0x263B,
    },
    AglEntry {
        name: "iogonek",
        unicode: 0x012F,
    },
    AglEntry {
        name: "iota",
        unicode: 0x03B9,
    },
    AglEntry {
        name: "iotadieresis",
        unicode: 0x03CA,
    },
    AglEntry {
        name: "iotadieresistonos",
        unicode: 0x0390,
    },
    AglEntry {
        name: "iotatonos",
        unicode: 0x03AF,
    },
    AglEntry {
        name: "itilde",
        unicode: 0x0129,
    },
    AglEntry {
        name: "j",
        unicode: 0x006A,
    },
    AglEntry {
        name: "jcircumflex",
        unicode: 0x0135,
    },
    AglEntry {
        name: "k",
        unicode: 0x006B,
    },
    AglEntry {
        name: "kappa",
        unicode: 0x03BA,
    },
    AglEntry {
        name: "kgreenlandic",
        unicode: 0x0138,
    },
    AglEntry {
        name: "l",
        unicode: 0x006C,
    },
    AglEntry {
        name: "lacute",
        unicode: 0x013A,
    },
    AglEntry {
        name: "lambda",
        unicode: 0x03BB,
    },
    AglEntry {
        name: "lcaron",
        unicode: 0x013E,
    },
    AglEntry {
        name: "ldot",
        unicode: 0x0140,
    },
    AglEntry {
        name: "less",
        unicode: 0x003C,
    },
    AglEntry {
        name: "lessequal",
        unicode: 0x2264,
    },
    AglEntry {
        name: "lfblock",
        unicode: 0x258C,
    },
    AglEntry {
        name: "lira",
        unicode: 0x20A4,
    },
    AglEntry {
        name: "logicaland",
        unicode: 0x2227,
    },
    AglEntry {
        name: "logicalnot",
        unicode: 0x00AC,
    },
    AglEntry {
        name: "logicalor",
        unicode: 0x2228,
    },
    AglEntry {
        name: "longs",
        unicode: 0x017F,
    },
    AglEntry {
        name: "lozenge",
        unicode: 0x25CA,
    },
    AglEntry {
        name: "lslash",
        unicode: 0x0142,
    },
    AglEntry {
        name: "ltshade",
        unicode: 0x2591,
    },
    AglEntry {
        name: "m",
        unicode: 0x006D,
    },
    AglEntry {
        name: "macron",
        unicode: 0x00AF,
    },
    AglEntry {
        name: "male",
        unicode: 0x2642,
    },
    AglEntry {
        name: "minus",
        unicode: 0x2212,
    },
    AglEntry {
        name: "minute",
        unicode: 0x2032,
    },
    AglEntry {
        name: "mu",
        unicode: 0x00B5,
    },
    AglEntry {
        name: "multiply",
        unicode: 0x00D7,
    },
    AglEntry {
        name: "musicalnote",
        unicode: 0x266A,
    },
    AglEntry {
        name: "musicalnotedbl",
        unicode: 0x266B,
    },
    AglEntry {
        name: "n",
        unicode: 0x006E,
    },
    AglEntry {
        name: "nacute",
        unicode: 0x0144,
    },
    AglEntry {
        name: "napostrophe",
        unicode: 0x0149,
    },
    AglEntry {
        name: "ncaron",
        unicode: 0x0148,
    },
    AglEntry {
        name: "nine",
        unicode: 0x0039,
    },
    AglEntry {
        name: "notelement",
        unicode: 0x2209,
    },
    AglEntry {
        name: "notequal",
        unicode: 0x2260,
    },
    AglEntry {
        name: "notsubset",
        unicode: 0x2284,
    },
    AglEntry {
        name: "ntilde",
        unicode: 0x00F1,
    },
    AglEntry {
        name: "nu",
        unicode: 0x03BD,
    },
    AglEntry {
        name: "numbersign",
        unicode: 0x0023,
    },
    AglEntry {
        name: "o",
        unicode: 0x006F,
    },
    AglEntry {
        name: "oacute",
        unicode: 0x00F3,
    },
    AglEntry {
        name: "obreve",
        unicode: 0x014F,
    },
    AglEntry {
        name: "ocircumflex",
        unicode: 0x00F4,
    },
    AglEntry {
        name: "odieresis",
        unicode: 0x00F6,
    },
    AglEntry {
        name: "oe",
        unicode: 0x0153,
    },
    AglEntry {
        name: "ogonek",
        unicode: 0x02DB,
    },
    AglEntry {
        name: "ograve",
        unicode: 0x00F2,
    },
    AglEntry {
        name: "ohorn",
        unicode: 0x01A1,
    },
    AglEntry {
        name: "ohungarumlaut",
        unicode: 0x0151,
    },
    AglEntry {
        name: "omacron",
        unicode: 0x014D,
    },
    AglEntry {
        name: "omega",
        unicode: 0x03C9,
    },
    AglEntry {
        name: "omega1",
        unicode: 0x03D6,
    },
    AglEntry {
        name: "omegatonos",
        unicode: 0x03CE,
    },
    AglEntry {
        name: "omicron",
        unicode: 0x03BF,
    },
    AglEntry {
        name: "omicrontonos",
        unicode: 0x03CC,
    },
    AglEntry {
        name: "one",
        unicode: 0x0031,
    },
    AglEntry {
        name: "onedotenleader",
        unicode: 0x2024,
    },
    AglEntry {
        name: "oneeighth",
        unicode: 0x215B,
    },
    AglEntry {
        name: "onehalf",
        unicode: 0x00BD,
    },
    AglEntry {
        name: "onequarter",
        unicode: 0x00BC,
    },
    AglEntry {
        name: "onethird",
        unicode: 0x2153,
    },
    AglEntry {
        name: "openbullet",
        unicode: 0x25E6,
    },
    AglEntry {
        name: "ordfeminine",
        unicode: 0x00AA,
    },
    AglEntry {
        name: "ordmasculine",
        unicode: 0x00BA,
    },
    AglEntry {
        name: "orthogonal",
        unicode: 0x221F,
    },
    AglEntry {
        name: "oslash",
        unicode: 0x00F8,
    },
    AglEntry {
        name: "oslashacute",
        unicode: 0x01FF,
    },
    AglEntry {
        name: "otilde",
        unicode: 0x00F5,
    },
    AglEntry {
        name: "p",
        unicode: 0x0070,
    },
    AglEntry {
        name: "paragraph",
        unicode: 0x00B6,
    },
    AglEntry {
        name: "parenleft",
        unicode: 0x0028,
    },
    AglEntry {
        name: "parenright",
        unicode: 0x0029,
    },
    AglEntry {
        name: "partialdiff",
        unicode: 0x2202,
    },
    AglEntry {
        name: "percent",
        unicode: 0x0025,
    },
    AglEntry {
        name: "period",
        unicode: 0x002E,
    },
    AglEntry {
        name: "periodcentered",
        unicode: 0x00B7,
    },
    AglEntry {
        name: "perpendicular",
        unicode: 0x22A5,
    },
    AglEntry {
        name: "perthousand",
        unicode: 0x2030,
    },
    AglEntry {
        name: "peseta",
        unicode: 0x20A7,
    },
    AglEntry {
        name: "phi",
        unicode: 0x03C6,
    },
    AglEntry {
        name: "phi1",
        unicode: 0x03D5,
    },
    AglEntry {
        name: "pi",
        unicode: 0x03C0,
    },
    AglEntry {
        name: "plus",
        unicode: 0x002B,
    },
    AglEntry {
        name: "plusminus",
        unicode: 0x00B1,
    },
    AglEntry {
        name: "prescription",
        unicode: 0x211E,
    },
    AglEntry {
        name: "product",
        unicode: 0x220F,
    },
    AglEntry {
        name: "propersubset",
        unicode: 0x2282,
    },
    AglEntry {
        name: "propersuperset",
        unicode: 0x2283,
    },
    AglEntry {
        name: "proportional",
        unicode: 0x221D,
    },
    AglEntry {
        name: "psi",
        unicode: 0x03C8,
    },
    AglEntry {
        name: "q",
        unicode: 0x0071,
    },
    AglEntry {
        name: "question",
        unicode: 0x003F,
    },
    AglEntry {
        name: "questiondown",
        unicode: 0x00BF,
    },
    AglEntry {
        name: "quotedbl",
        unicode: 0x0022,
    },
    AglEntry {
        name: "quotedblbase",
        unicode: 0x201E,
    },
    AglEntry {
        name: "quotedblleft",
        unicode: 0x201C,
    },
    AglEntry {
        name: "quotedblright",
        unicode: 0x201D,
    },
    AglEntry {
        name: "quoteleft",
        unicode: 0x2018,
    },
    AglEntry {
        name: "quotereversed",
        unicode: 0x201B,
    },
    AglEntry {
        name: "quoteright",
        unicode: 0x2019,
    },
    AglEntry {
        name: "quotesinglbase",
        unicode: 0x201A,
    },
    AglEntry {
        name: "quotesingle",
        unicode: 0x0027,
    },
    AglEntry {
        name: "r",
        unicode: 0x0072,
    },
    AglEntry {
        name: "racute",
        unicode: 0x0155,
    },
    AglEntry {
        name: "radical",
        unicode: 0x221A,
    },
    AglEntry {
        name: "rcaron",
        unicode: 0x0159,
    },
    AglEntry {
        name: "reflexsubset",
        unicode: 0x2286,
    },
    AglEntry {
        name: "reflexsuperset",
        unicode: 0x2287,
    },
    AglEntry {
        name: "registered",
        unicode: 0x00AE,
    },
    AglEntry {
        name: "revlogicalnot",
        unicode: 0x2310,
    },
    AglEntry {
        name: "rho",
        unicode: 0x03C1,
    },
    AglEntry {
        name: "ring",
        unicode: 0x02DA,
    },
    AglEntry {
        name: "rtblock",
        unicode: 0x2590,
    },
    AglEntry {
        name: "s",
        unicode: 0x0073,
    },
    AglEntry {
        name: "sacute",
        unicode: 0x015B,
    },
    AglEntry {
        name: "scaron",
        unicode: 0x0161,
    },
    AglEntry {
        name: "scedilla",
        unicode: 0x015F,
    },
    AglEntry {
        name: "scircumflex",
        unicode: 0x015D,
    },
    AglEntry {
        name: "second",
        unicode: 0x2033,
    },
    AglEntry {
        name: "section",
        unicode: 0x00A7,
    },
    AglEntry {
        name: "semicolon",
        unicode: 0x003B,
    },
    AglEntry {
        name: "seven",
        unicode: 0x0037,
    },
    AglEntry {
        name: "seveneighths",
        unicode: 0x215E,
    },
    AglEntry {
        name: "shade",
        unicode: 0x2592,
    },
    AglEntry {
        name: "sigma",
        unicode: 0x03C3,
    },
    AglEntry {
        name: "sigma1",
        unicode: 0x03C2,
    },
    AglEntry {
        name: "similar",
        unicode: 0x223C,
    },
    AglEntry {
        name: "six",
        unicode: 0x0036,
    },
    AglEntry {
        name: "slash",
        unicode: 0x002F,
    },
    AglEntry {
        name: "smileface",
        unicode: 0x263A,
    },
    AglEntry {
        name: "space",
        unicode: 0x0020,
    },
    AglEntry {
        name: "spade",
        unicode: 0x2660,
    },
    AglEntry {
        name: "sterling",
        unicode: 0x00A3,
    },
    AglEntry {
        name: "suchthat",
        unicode: 0x220B,
    },
    AglEntry {
        name: "summation",
        unicode: 0x2211,
    },
    AglEntry {
        name: "sun",
        unicode: 0x263C,
    },
    AglEntry {
        name: "t",
        unicode: 0x0074,
    },
    AglEntry {
        name: "tau",
        unicode: 0x03C4,
    },
    AglEntry {
        name: "tbar",
        unicode: 0x0167,
    },
    AglEntry {
        name: "tcaron",
        unicode: 0x0165,
    },
    AglEntry {
        name: "therefore",
        unicode: 0x2234,
    },
    AglEntry {
        name: "theta",
        unicode: 0x03B8,
    },
    AglEntry {
        name: "theta1",
        unicode: 0x03D1,
    },
    AglEntry {
        name: "thorn",
        unicode: 0x00FE,
    },
    AglEntry {
        name: "three",
        unicode: 0x0033,
    },
    AglEntry {
        name: "threeeighths",
        unicode: 0x215C,
    },
    AglEntry {
        name: "threequarters",
        unicode: 0x00BE,
    },
    AglEntry {
        name: "tilde",
        unicode: 0x02DC,
    },
    AglEntry {
        name: "tildecomb",
        unicode: 0x0303,
    },
    AglEntry {
        name: "tonos",
        unicode: 0x0384,
    },
    AglEntry {
        name: "trademark",
        unicode: 0x2122,
    },
    AglEntry {
        name: "triagdn",
        unicode: 0x25BC,
    },
    AglEntry {
        name: "triaglf",
        unicode: 0x25C4,
    },
    AglEntry {
        name: "triagrt",
        unicode: 0x25BA,
    },
    AglEntry {
        name: "triagup",
        unicode: 0x25B2,
    },
    AglEntry {
        name: "two",
        unicode: 0x0032,
    },
    AglEntry {
        name: "twodotenleader",
        unicode: 0x2025,
    },
    AglEntry {
        name: "twothirds",
        unicode: 0x2154,
    },
    AglEntry {
        name: "u",
        unicode: 0x0075,
    },
    AglEntry {
        name: "uacute",
        unicode: 0x00FA,
    },
    AglEntry {
        name: "ubreve",
        unicode: 0x016D,
    },
    AglEntry {
        name: "ucircumflex",
        unicode: 0x00FB,
    },
    AglEntry {
        name: "udieresis",
        unicode: 0x00FC,
    },
    AglEntry {
        name: "ugrave",
        unicode: 0x00F9,
    },
    AglEntry {
        name: "uhorn",
        unicode: 0x01B0,
    },
    AglEntry {
        name: "uhungarumlaut",
        unicode: 0x0171,
    },
    AglEntry {
        name: "umacron",
        unicode: 0x016B,
    },
    AglEntry {
        name: "underscore",
        unicode: 0x005F,
    },
    AglEntry {
        name: "underscoredbl",
        unicode: 0x2017,
    },
    AglEntry {
        name: "union",
        unicode: 0x222A,
    },
    AglEntry {
        name: "universal",
        unicode: 0x2200,
    },
    AglEntry {
        name: "uogonek",
        unicode: 0x0173,
    },
    AglEntry {
        name: "upblock",
        unicode: 0x2580,
    },
    AglEntry {
        name: "upsilon",
        unicode: 0x03C5,
    },
    AglEntry {
        name: "upsilondieresis",
        unicode: 0x03CB,
    },
    AglEntry {
        name: "upsilondieresistonos",
        unicode: 0x03B0,
    },
    AglEntry {
        name: "upsilontonos",
        unicode: 0x03CD,
    },
    AglEntry {
        name: "uring",
        unicode: 0x016F,
    },
    AglEntry {
        name: "utilde",
        unicode: 0x0169,
    },
    AglEntry {
        name: "v",
        unicode: 0x0076,
    },
    AglEntry {
        name: "w",
        unicode: 0x0077,
    },
    AglEntry {
        name: "wacute",
        unicode: 0x1E83,
    },
    AglEntry {
        name: "wcircumflex",
        unicode: 0x0175,
    },
    AglEntry {
        name: "wdieresis",
        unicode: 0x1E85,
    },
    AglEntry {
        name: "weierstrass",
        unicode: 0x2118,
    },
    AglEntry {
        name: "wgrave",
        unicode: 0x1E81,
    },
    AglEntry {
        name: "x",
        unicode: 0x0078,
    },
    AglEntry {
        name: "xi",
        unicode: 0x03BE,
    },
    AglEntry {
        name: "y",
        unicode: 0x0079,
    },
    AglEntry {
        name: "yacute",
        unicode: 0x00FD,
    },
    AglEntry {
        name: "ycircumflex",
        unicode: 0x0177,
    },
    AglEntry {
        name: "ydieresis",
        unicode: 0x00FF,
    },
    AglEntry {
        name: "yen",
        unicode: 0x00A5,
    },
    AglEntry {
        name: "ygrave",
        unicode: 0x1EF3,
    },
    AglEntry {
        name: "z",
        unicode: 0x007A,
    },
    AglEntry {
        name: "zacute",
        unicode: 0x017A,
    },
    AglEntry {
        name: "zcaron",
        unicode: 0x017E,
    },
    AglEntry {
        name: "zdotaccent",
        unicode: 0x017C,
    },
    AglEntry {
        name: "zero",
        unicode: 0x0030,
    },
    AglEntry {
        name: "zeta",
        unicode: 0x03B6,
    },
];

/// Look up a glyph name in the AGL.
#[must_use]
pub fn glyph_to_unicode(name: &str) -> Option<u32> {
    let i = AGL.binary_search_by(|e| e.name.cmp(name)).ok()?;
    Some(AGL.get(i)?.unicode)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_names_are_found() {
        assert_eq!(glyph_to_unicode("A"), Some(0x41));
        assert_eq!(glyph_to_unicode("agrave"), Some(0xE0));
        assert_eq!(glyph_to_unicode("ampersand"), Some(0x26));
        assert_eq!(glyph_to_unicode("AE"), Some(0xC6));
    }

    #[test]
    fn unknown_name_returns_none() {
        assert_eq!(glyph_to_unicode("noSuchGlyph"), None);
    }
}
