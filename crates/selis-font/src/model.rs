//! The PDF font dictionary model (SL-3.FONT.01).
//!
//! A plain-Rust mirror of the PDF font dictionary — ISO 32000-2:2020 §9.6.
//! The caller (which holds the COS edge) reads the object graph and builds
//! these values; this crate never sees a COS object. Composite (Type0) fonts
//! carry their first descendant CIDFont in [`FontDict::descendant`].

use selis_bytes::Bytes;

/// The font subtype (`/Subtype`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FontSubtype {
    /// `/Type1` — a simple font.
    Type1,
    /// `/TrueType` — a simple font.
    TrueType,
    /// `/Type3` — glyph procedures as content streams.
    Type3,
    /// `/MMType1` — multiple-master Type1.
    MmType1,
    /// `/Type0` — a composite font (the descendant CIDFont carries metrics).
    Type0,
    /// `/CIDFontType0` — a Type1-based CIDFont (Type0 descendant).
    CidFontType0,
    /// `/CIDFontType2` — a TrueType-based CIDFont (Type0 descendant).
    CidFontType2,
}

impl FontSubtype {
    /// Whether this is a simple (non-composite) font.
    #[must_use]
    pub fn is_simple(self) -> bool {
        matches!(
            self,
            FontSubtype::Type1 | FontSubtype::TrueType | FontSubtype::Type3 | FontSubtype::MmType1
        )
    }
}

/// The `/FontDescriptor` (ISO 32000-2:2020 §9.8).
#[derive(Debug, Clone, PartialEq)]
pub struct FontDescriptor {
    /// `/Flags` — the descriptor flags bitmask.
    pub flags: u32,
    /// `/FontName`.
    pub font_name: Option<String>,
    /// `/FontBBox`, `[llx lly urx ury]`.
    pub font_bbox: Option<[f64; 4]>,
    /// `/Ascent`.
    pub ascent: Option<f64>,
    /// `/Descent`.
    pub descent: Option<f64>,
    /// `/MissingWidth` — the width used for any code with no other width.
    pub missing_width: f64,
    /// `/StemV`.
    pub stem_v: Option<f64>,
}

impl Default for FontDescriptor {
    fn default() -> Self {
        Self {
            flags: 0,
            font_name: None,
            font_bbox: None,
            ascent: None,
            descent: None,
            missing_width: 0.0,
            stem_v: None,
        }
    }
}

/// The embedded font program (the `/FontFile`, `/FontFile2`, `/FontFile3`
/// streams). The bytes are document-derived and immutable ([`Bytes`], ADR-P0007).
#[derive(Debug, Clone, PartialEq)]
pub enum FontFile {
    /// `/FontFile` — Type1 (PFB/PFA).
    Type1(Bytes),
    /// `/FontFile2` — TrueType.
    TrueType(Bytes),
    /// `/FontFile3` — OpenType/CFF (or `CIDFontType0C`).
    OpenType(Bytes),
}

impl FontFile {
    /// The font-program bytes, regardless of flavour.
    #[must_use]
    pub fn data(&self) -> &Bytes {
        match self {
            FontFile::Type1(b) | FontFile::TrueType(b) | FontFile::OpenType(b) => b,
        }
    }
}

/// A PDF font dictionary.
#[derive(Debug, Clone, PartialEq)]
pub struct FontDict {
    /// `/BaseFont`.
    pub base_font: String,
    /// `/Subtype`.
    pub subtype: FontSubtype,
    /// `/FirstChar`.
    pub first_char: u32,
    /// `/LastChar`.
    pub last_char: u32,
    /// `/Widths` — `last_char - first_char + 1` entries in glyph space
    /// (1000 units per em for simple fonts).
    pub widths: Option<Vec<f64>>,
    /// `/FontDescriptor`.
    pub descriptor: Option<FontDescriptor>,
    /// `/FontFile`, `/FontFile2`, or `/FontFile3`.
    pub font_file: Option<FontFile>,
    /// `/DescendantFonts[0]` — present on a Type0 composite font.
    pub descendant: Option<Box<FontDict>>,
}

impl FontDict {
    /// A simple font dictionary with no widths and no descriptor (the tests'
    /// starting point).
    #[must_use]
    pub fn simple(subtype: FontSubtype) -> Self {
        Self {
            base_font: String::new(),
            subtype,
            first_char: 0,
            last_char: 255,
            widths: None,
            descriptor: None,
            font_file: None,
            descendant: None,
        }
    }

    /// The effective `/MissingWidth`: the descriptor's, or 0.
    #[must_use]
    pub fn missing_width(&self) -> f64 {
        self.descriptor
            .as_ref()
            .map(|d| d.missing_width)
            .unwrap_or(0.0)
    }

    /// The widths array, if present and correctly sized
    /// (`last - first + 1` entries). A mis-sized array is a deviation, treated
    /// as absent rather than a page failure.
    #[must_use]
    pub fn widths_sized(&self) -> Option<&[f64]> {
        let values = self.widths.as_ref()?;
        let expected = self
            .last_char
            .checked_sub(self.first_char)
            .map(|d| d.saturating_add(1));
        match expected {
            Some(n) if n as usize == values.len() => Some(values),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn simple_subtypes_are_simple() {
        assert!(FontSubtype::TrueType.is_simple());
        assert!(FontSubtype::Type1.is_simple());
        assert!(FontSubtype::Type3.is_simple());
        assert!(!FontSubtype::Type0.is_simple());
        assert!(!FontSubtype::CidFontType2.is_simple());
    }

    #[test]
    fn missing_width_defaults_to_zero() {
        let dict = FontDict::simple(FontSubtype::TrueType);
        assert_eq!(dict.missing_width(), 0.0);
        let with_descriptor = FontDict {
            descriptor: Some(FontDescriptor {
                missing_width: 500.0,
                ..FontDescriptor::default()
            }),
            ..FontDict::simple(FontSubtype::TrueType)
        };
        assert_eq!(with_descriptor.missing_width(), 500.0);
    }

    #[test]
    fn mis_sized_widths_are_treated_as_absent() {
        let dict = FontDict {
            first_char: 32,
            last_char: 40,
            widths: Some(vec![1.0, 2.0]), // needs 9 entries
            ..FontDict::simple(FontSubtype::Type1)
        };
        assert!(dict.widths_sized().is_none());
    }

    #[test]
    fn correctly_sized_widths_are_exposed() {
        let dict = FontDict {
            first_char: 32,
            last_char: 34,
            widths: Some(vec![1.0, 2.0, 3.0]),
            ..FontDict::simple(FontSubtype::Type1)
        };
        assert_eq!(dict.widths_sized(), Some(&[1.0, 2.0, 3.0][..]));
    }

    #[test]
    fn font_file_data_is_exposed() {
        let data = Bytes::copy_from_slice(b"\x00\x01sfnt");
        let f = FontFile::TrueType(data.clone());
        assert_eq!(f.data(), &data);
        assert_eq!(f.data().as_slice(), b"\x00\x01sfnt");
    }
}
