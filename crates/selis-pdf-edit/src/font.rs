//! The page's fonts, as the re-flow needs them: what every code is worth.
//!
//! The re-flow measures the text it replaces and the text that replaces it,
//! in the page's OWN font, so the text that follows on the same line can be
//! pushed along by the difference. The arithmetic is the font engine's —
//! [`selis_font::width::resolve`] — so the numbers are the engine's numbers.
//! What is here is the reading of the font dictionaries, because the edit
//! crate reads the document's object graph itself.
//!
//! A font that carries no widths of its own (a standard-14 face, which is
//! most of them) gets the font engine's metric table; a font that carries
//! them gets its own. A font this cannot measure is left out, and the
//! re-flow then leaves the text where it was rather than guessing.

use std::collections::BTreeMap;

use selis_bytes::Bytes;
use selis_font::cid::{resolve_cid_widths, CidWidthEntry};
use selis_font::model::{FontDescriptor, FontDict, FontSubtype};
use selis_font::width::ResolvedFont;
use selis_pdf_cos::Obj;
use selis_sandbox::{Budget, BudgetGuard};

/// How many `/W` entries a font's widths may hold.
///
/// The array is document-derived and can be any length; the bound keeps a
/// hostile one from turning into unbounded work.
const MAX_CID_WIDTH_ENTRIES: usize = 65_536;

/// A page font's advances.
pub(crate) struct PageFont {
    widths: ResolvedFont,
    code_width: usize,
    cid: bool,
}

impl PageFont {
    /// The advance of a code, in glyph space (1000 units per em).
    #[must_use]
    pub(crate) fn advance(&self, code: u32) -> f64 {
        self.widths.width(code)
    }

    /// The width of a code in bytes: 1 for a simple font, 2 for a CID font.
    #[must_use]
    pub(crate) fn code_width(&self) -> usize {
        self.code_width
    }

    /// Whether the font is a CID font.
    ///
    /// Word spacing applies to a simple font's code 32 only.
    #[must_use]
    pub(crate) fn is_cid(&self) -> bool {
        self.cid
    }
}

/// The page's fonts, by resource name.
pub(crate) type PageFonts = BTreeMap<Vec<u8>, PageFont>;

/// The page's fonts, read into their advances.
pub(crate) fn page_fonts(
    src: &[u8],
    doc: &selis_pdf_cos::Doc,
    page_dict: &[(Bytes, Obj)],
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
) -> PageFonts {
    let mut out = PageFonts::new();
    let Some(resources) = crate::resolve_dict_entry(src, doc, page_dict, b"Resources", budget, g)
    else {
        return out;
    };
    let Some(fonts) = crate::resolve_dict_entry(src, doc, &resources, b"Font", budget, g) else {
        return out;
    };
    for (name, value) in &fonts {
        let Some(font) = font_of(src, doc, value, budget, g) else {
            continue;
        };
        out.insert(name.to_vec(), font);
    }
    out
}

/// One font's advances, read from its dictionary.
fn font_of(
    src: &[u8],
    doc: &selis_pdf_cos::Doc,
    value: &Obj,
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
) -> Option<PageFont> {
    let dict = crate::as_dict(src, doc, value, budget, g)?;
    let subtype = subtype_of(&dict);
    let mut fd = FontDict::simple(subtype);
    fd.base_font = text_of(&dict, b"BaseFont").unwrap_or_default();
    fd.first_char = int_of(&dict, b"FirstChar").unwrap_or(0);
    fd.last_char = int_of(&dict, b"LastChar").unwrap_or(255);
    fd.widths = widths_of(&dict);
    fd.descriptor = Some(FontDescriptor {
        missing_width: missing_width(src, doc, &dict, budget, g),
        ..FontDescriptor::default()
    });
    let mut code_width = 1usize;
    let mut cid = false;
    if subtype == FontSubtype::Type0 {
        // A composite font's metrics live in its first descendant CIDFont.
        code_width = 2;
        cid = true;
        fd.descendant = Some(Box::new(descendant_of(src, doc, &dict, budget, g)?));
    } else if matches!(
        subtype,
        FontSubtype::CidFontType0 | FontSubtype::CidFontType2
    ) {
        // A bare descendant CIDFont used as a page font: its own metrics.
        code_width = 2;
        cid = true;
        fd.cid_widths = Some(cid_widths_of(src, doc, &dict, budget, g));
    }
    let widths = selis_font::width::resolve(&fd, g).ok()?;
    Some(PageFont {
        widths,
        code_width,
        cid,
    })
}

/// The first descendant CIDFont of a composite font.
fn descendant_of(
    src: &[u8],
    doc: &selis_pdf_cos::Doc,
    dict: &[(Bytes, Obj)],
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
) -> Option<FontDict> {
    let Obj::Array(items) = crate::dict_get(dict, b"DescendantFonts")? else {
        return None;
    };
    let first = items.first()?;
    let child = crate::as_dict(src, doc, first, budget, g)?;
    let subtype = match text_of(&child, b"Subtype").as_deref() {
        Some("CIDFontType0") => FontSubtype::CidFontType0,
        _ => FontSubtype::CidFontType2,
    };
    let mut fd = FontDict::simple(subtype);
    fd.base_font = text_of(&child, b"BaseFont").unwrap_or_default();
    fd.descriptor = Some(FontDescriptor {
        missing_width: missing_width(src, doc, &child, budget, g),
        ..FontDescriptor::default()
    });
    fd.cid_widths = Some(cid_widths_of(src, doc, &child, budget, g));
    Some(fd)
}

/// The `/W` and `/DW` of a CIDFont, resolved into per-CID widths.
fn cid_widths_of(
    src: &[u8],
    doc: &selis_pdf_cos::Doc,
    dict: &[(Bytes, Obj)],
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
) -> selis_font::cid::CidWidths {
    // `/DW` is 1000 unless the font says otherwise.
    let mut default = 1000.0f64;
    if let Some(value) = crate::dict_get(dict, b"DW") {
        default = number_of(value).unwrap_or(default);
    }
    let _ = (src, doc, budget, g);
    resolve_cid_widths(&cid_width_entries(dict), default)
}

/// The `/W` array as width entries.
///
/// Both ISO 32000-1 §9.7.4.3 forms are honoured: `c [w…]` and
/// `c_first c_last w`. The form is decided by the type of the element that
/// follows the leading CID.
fn cid_width_entries(dict: &[(Bytes, Obj)]) -> Vec<CidWidthEntry> {
    let mut entries = Vec::new();
    let Some(Obj::Array(items)) = crate::dict_get(dict, b"W") else {
        return entries;
    };
    let mut i = 0usize;
    while i < items.len() && entries.len() < MAX_CID_WIDTH_ENTRIES {
        let Some(first) = items.get(i).and_then(|o| int_of_obj(o)) else {
            i = i.saturating_add(1);
            continue;
        };
        match items.get(i.saturating_add(1)) {
            Some(Obj::Array(widths)) => {
                let values: Vec<f64> = widths.iter().filter_map(number_of).collect();
                entries.push(CidWidthEntry::Cid(first));
                entries.push(CidWidthEntry::Widths(values));
                i = i.saturating_add(2);
            }
            Some(next) if number_of(next).is_some() => {
                let last = int_of_obj(next).unwrap_or(first);
                if let Some(width) = items.get(i.saturating_add(2)).and_then(number_of) {
                    entries.push(CidWidthEntry::Range { first, last, width });
                }
                i = i.saturating_add(3);
            }
            _ => {
                i = i.saturating_add(1);
            }
        }
    }
    entries
}

/// The `/MissingWidth` of a font's descriptor, or 0.
fn missing_width(
    src: &[u8],
    doc: &selis_pdf_cos::Doc,
    dict: &[(Bytes, Obj)],
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
) -> f64 {
    let Some(descriptor) = crate::dict_get(dict, b"FontDescriptor") else {
        return 0.0;
    };
    let Some(descriptor) = crate::as_dict(src, doc, descriptor, budget, g) else {
        return 0.0;
    };
    crate::dict_get(&descriptor, b"MissingWidth")
        .and_then(number_of)
        .unwrap_or(0.0)
}

/// The `/Widths` array of a simple font.
fn widths_of(dict: &[(Bytes, Obj)]) -> Option<Vec<f64>> {
    let Obj::Array(items) = crate::dict_get(dict, b"Widths")? else {
        return None;
    };
    let widths: Vec<f64> = items.iter().filter_map(number_of).collect();
    if widths.is_empty() {
        None
    } else {
        Some(widths)
    }
}

/// The font's subtype.
fn subtype_of(dict: &[(Bytes, Obj)]) -> FontSubtype {
    match text_of(dict, b"Subtype").as_deref() {
        Some("Type1") => FontSubtype::Type1,
        Some("TrueType") => FontSubtype::TrueType,
        Some("Type3") => FontSubtype::Type3,
        Some("MMType1") => FontSubtype::MmType1,
        Some("Type0") => FontSubtype::Type0,
        Some("CIDFontType0") => FontSubtype::CidFontType0,
        Some("CIDFontType2") => FontSubtype::CidFontType2,
        _ => FontSubtype::TrueType,
    }
}

/// The text of a name entry.
fn text_of(dict: &[(Bytes, Obj)], key: &[u8]) -> Option<String> {
    match crate::dict_get(dict, key)? {
        Obj::Name(name) => Some(String::from_utf8_lossy(name.as_slice()).into_owned()),
        _ => None,
    }
}

/// The integer value of an entry.
fn int_of(dict: &[(Bytes, Obj)], key: &[u8]) -> Option<u32> {
    crate::dict_get(dict, key).and_then(int_of_obj)
}

/// The integer value of an object.
fn int_of_obj(value: &Obj) -> Option<u32> {
    match value {
        Obj::Int(v) => u32::try_from(*v).ok(),
        _ => None,
    }
}

/// The number an object carries, as a real.
fn number_of(value: &Obj) -> Option<f64> {
    match value {
        Obj::Int(v) => Some(*v as f64),
        Obj::Real { scaled, scale } => Some(*scaled as f64 / 10f64.powi(*scale as i32)),
        _ => None,
    }
}
