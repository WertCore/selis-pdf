//! Bundled Liberation TrueType faces for **inbound** font embedding
//! (SL-5.CONV.05).
//!
//! The converter's default output uses the standard-14 base fonts with
//! WinAnsiEncoding and no font programs. When a caller asks for embedding,
//! this module supplies one of the two metric-compatible Liberation families
//! and subsets it to the glyphs actually shown on the page, so the emitted PDF
//! carries a real `/FontFile2` program rather than a bare base-font name.
//!
//! The faces are the SIL-OFL Liberation TTFs already shipped with the engine
//! in `assets/fonts/`. Only TrueType is
//! handled here: CFF/Type1 embedding is deliberately deferred (the subsetter
//! in `selis-font` refuses CFF), and the monospaced code face stays a
//! standard-14 `/Type1` font because the two bundled families are sans/serif.
//!
//! The font bytes are compiled in only when the `embed-fonts` Cargo feature is
//! on. The wasm convert chunk builds `selis-pdf-convert` with that feature off
//! (its C ABI exposes no font choice and the chunk is size-gated), so an
//! embedding request there fails loudly with `ConvertUnsupported` instead of
//! silently falling back to standard-14.

use std::collections::BTreeSet;

use selis_bytes::Bytes;
use selis_error::{Code, Ctx, Error, Result};
use selis_sandbox::BudgetGuard;

use crate::blocks::Style;

/// Which bundled Liberation family to embed.
///
/// A family embeds all four of its faces (regular, bold, italic, bold-italic)
/// and uses each for the matching text face: body, heading, blockquote and
/// bold-italic runs. The monospaced code face is not affected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FontChoice {
    /// The four Liberation Sans faces (metric-compatible with Helvetica).
    Sans,
    /// The four Liberation Serif faces (metric-compatible with Times).
    Serif,
}

impl FontChoice {
    /// The bundled PostScript face name for one of the four text styles.
    #[must_use]
    pub(crate) const fn face(self, style: Style) -> &'static str {
        match (self, style) {
            (Self::Sans, Style::Regular) => "LiberationSans-Regular",
            (Self::Sans, Style::Bold) => "LiberationSans-Bold",
            (Self::Sans, Style::Italic) => "LiberationSans-Italic",
            (Self::Sans, Style::BoldItalic) => "LiberationSans-BoldItalic",
            (Self::Serif, Style::Regular) => "LiberationSerif-Regular",
            (Self::Serif, Style::Bold) => "LiberationSerif-Bold",
            (Self::Serif, Style::Italic) => "LiberationSerif-Italic",
            (Self::Serif, Style::BoldItalic) => "LiberationSerif-BoldItalic",
            // `Style::Code` has no bundled face in either family; the emitter
            // never asks for it, but keep the match total without a panic.
            (_, Style::Code) => "LiberationMono-Regular",
        }
    }
}

/// The four styles an embedded family supplies, in resource-key order.
pub(crate) const EMBEDDED_STYLES: [Style; 4] = [
    Style::Regular,
    Style::Bold,
    Style::Italic,
    Style::BoldItalic,
];

/// The full program of one bundled face, or `None` when this build was
/// compiled without the embedded faces.
///
/// # Budget
///
/// None: the bytes are compiled into the binary, not charged at runtime; the
/// subsetter charges the emitted subset.
///
/// # Malformed Input
///
/// `None` for the code style, which has no bundled sans/serif face.
#[cfg(feature = "embed-fonts")]
pub(crate) fn face_program(choice: FontChoice, style: Style) -> Option<&'static [u8]> {
    Some(match (choice, style) {
        (FontChoice::Sans, Style::Regular) => {
            include_bytes!("../../../assets/fonts/LiberationSans-Regular.ttf")
        }
        (FontChoice::Sans, Style::Bold) => {
            include_bytes!("../../../assets/fonts/LiberationSans-Bold.ttf")
        }
        (FontChoice::Sans, Style::Italic) => {
            include_bytes!("../../../assets/fonts/LiberationSans-Italic.ttf")
        }
        (FontChoice::Sans, Style::BoldItalic) => {
            include_bytes!("../../../assets/fonts/LiberationSans-BoldItalic.ttf")
        }
        (FontChoice::Serif, Style::Regular) => {
            include_bytes!("../../../assets/fonts/LiberationSerif-Regular.ttf")
        }
        (FontChoice::Serif, Style::Bold) => {
            include_bytes!("../../../assets/fonts/LiberationSerif-Bold.ttf")
        }
        (FontChoice::Serif, Style::Italic) => {
            include_bytes!("../../../assets/fonts/LiberationSerif-Italic.ttf")
        }
        (FontChoice::Serif, Style::BoldItalic) => {
            include_bytes!("../../../assets/fonts/LiberationSerif-BoldItalic.ttf")
        }
        // The emitter never requests the code face; keep the match total.
        (_, Style::Code) => return None,
    })
}

/// Variant for builds without the bundled faces: embedding is refused by name.
///
/// # Budget
///
/// None: no bytes are compiled or charged.
///
/// # Malformed Input
///
/// Always `None`; the caller turns that into a typed `ConvertUnsupported`.
#[cfg(not(feature = "embed-fonts"))]
pub(crate) fn face_program(_choice: FontChoice, _style: Style) -> Option<&'static [u8]> {
    None
}

/// A subset TrueType face, ready to be written as a PDF font object.
pub(crate) struct EmbeddedFace {
    /// The embedded font's PostScript name (also the PDF `/BaseFont`).
    pub base_font: &'static str,
    /// The subset SFNT program, written verbatim behind `/FontFile2`.
    pub program: Bytes,
    /// 256 `/Widths` entries indexed by WinAnsi byte, in 1/1000 text space.
    pub widths: Vec<i64>,
}

/// Subset one bundled face to the WinAnsi bytes actually shown with it.
///
/// # Budget
///
/// Charges the subset program's bytes through `selis_font::subset_ttf` against
/// `g`'s `Resource::Bytes` counter; the caller charges the written stream and
/// object-resolution units separately.
///
/// # Malformed Input
///
/// `ConvertUnsupported` when this build has no bundled faces or when the
/// bundled program cannot be subset. Budget exhaustion propagates as
/// `BUDGET_*`. Bytes with no cmap glyph are skipped (the same `.notdef`
/// deviation a renderer would show), never guessed; an empty text still yields
/// a valid `.notdef`-only program.
pub(crate) fn build_face(
    choice: FontChoice,
    style: Style,
    used: &BTreeSet<u8>,
    base14_font: &str,
    g: &mut BudgetGuard<'_>,
) -> Result<EmbeddedFace> {
    let source = face_program(choice, style).ok_or_else(|| {
        Error::with(
            Code::ConvertUnsupported,
            Ctx::new().detail("font embedding is not compiled into this build"),
        )
    })?;

    let source = Bytes::copy_from_slice(source);

    // PDF simple fonts map a byte through /Encoding to a glyph *name*; a
    // TrueType program then maps that name's Unicode value through its cmap.
    // Keep exactly the glyphs the used bytes resolve to (plus .notdef, which
    // the subsetter always keeps).
    let mut keep: BTreeSet<u16> = BTreeSet::new();
    for &byte in used {
        let Some(&name) = selis_font::tables::WinAnsiEncoding.get(usize::from(byte)) else {
            continue;
        };
        if name.is_empty() {
            continue;
        }
        if let Some(unicode) = selis_font::agl::glyph_to_unicode(name) {
            if let Some(gid) = selis_font::glyph_id_for_char(&source, unicode) {
                let _ = keep.insert(gid);
            }
        }
    }
    // An empty text still needs a parseable program; glyph 0 is always kept.
    let keep: Vec<u16> = keep.into_iter().collect();
    let program = selis_font::subset_ttf(&source, &selis_font::GlyphSet { keep }, g)?
        .ok_or_else(|| Error::new(Code::ConvertUnsupported))?;

    // Widths mirror the exact AFM advances the layout engine measured with,
    // so laying out with standard-14 and rendering the embedded, metric-
    // compatible Liberation face produce identical glyph positions.
    let widths = (0..=255u8)
        .map(|byte| {
            let name = selis_font::tables::WinAnsiEncoding
                .get(usize::from(byte))
                .copied()
                .unwrap_or("");
            i64::from(selis_font::standard14::width(base14_font, name).unwrap_or(0))
        })
        .collect();

    Ok(EmbeddedFace {
        base_font: choice.face(style),
        program,
        widths,
    })
}
