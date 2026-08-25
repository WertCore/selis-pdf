//! Text shaping (SL-3.SHAPE.01).
//!
//! The `Shaper` trait is the boundary between font data and **positioned
//! glyphs**. It is used for *authored* text and reflowed edits only — existing
//! content is replayed by glyph id and never re-shaped. That separation is
//! enforced mechanically: `selis-shape` (and its `swash` dependency) has
//! **no edge** from `selis-pdf-content` in `layers.toml`, so `check-layers`
//! keeps swash out of the replay path (the DoD).
//!
//! Open-sourced under Apache-2.0 (ADR-P0030).

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]
#![forbid(unsafe_code)]

use selis_error::{err, Code, Result};
use swash::shape::ShapeContext;
use swash::FontRef;

pub mod bidi;
pub mod linebreak;

pub use bidi::{
    bidi_levels, mirrored_glyph, paragraph_direction, reorder_text, visual_runs, BaseDirection,
    VisualRun,
};
pub use linebreak::{justified_advance, line_breaks, BreakOpportunity};

/// A single positioned glyph, in font-size units (points).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PositionedGlyph {
    /// The glyph id in the font.
    pub glyph_id: u16,
    /// The horizontal advance (points).
    pub x_advance: f32,
    /// The vertical advance (points).
    pub y_advance: f32,
    /// The horizontal pen offset (points).
    pub x_offset: f32,
    /// The vertical pen offset (points).
    pub y_offset: f32,
    /// The character cluster index (byte offset into the input text).
    pub cluster: u32,
}

/// The result of shaping a text run.
#[derive(Debug, Clone, PartialEq)]
pub struct ShapedBuffer {
    /// The positioned glyphs, in shaping order.
    pub glyphs: Vec<PositionedGlyph>,
    /// The total advance width (points).
    pub width: f32,
}

/// An OpenType feature to apply: `(tag, value)`.
pub type Feature = (u32, u16);

/// Parameters for shaping a run of text.
pub struct ShapingParams<'a> {
    /// The text to shape.
    pub text: &'a str,
    /// The font program bytes (TrueType or OpenType).
    pub font_data: &'a [u8],
    /// The font size in points.
    pub font_size: f32,
    /// The ISO 15924 script tag (e.g. `0x4C61746E` for Latin).
    pub script: u32,
    /// The BCP 47 language tag, or `None`.
    pub language: Option<&'a str>,
    /// OpenType features to apply.
    pub features: &'a [Feature],
}

/// The shaper trait. Implementations convert `text + font → positioned glyphs`.
pub trait Shaper {
    /// Shape a text run.
    ///
    /// # Malformed Input
    ///
    /// `SHAPE_FONT` when the font program cannot be parsed.
    fn shape(&self, params: &ShapingParams<'_>) -> Result<ShapedBuffer>;
}

/// A run of text with a single script (ISO 15924).
#[derive(Debug, Clone, PartialEq)]
pub struct ScriptRun {
    /// The ISO 15924 script tag (e.g. `0x4C61746E` for Latin).
    pub script: u32,
    /// The byte range in the input text.
    pub start: usize,
    /// The byte range in the input text.
    pub end: usize,
}

/// Itemise text into single-script runs, by ISO 15924 script boundary.
///
/// Common (inherited/common) code points inherit the surrounding script.
/// `None` is returned for a script the caller should fall back to.
#[must_use]
pub fn script_runs(text: &str) -> Vec<ScriptRun> {
    use unicode_script::UnicodeScript;
    let mut runs: Vec<ScriptRun> = Vec::new();
    let mut current: Option<(u32, usize)> = None; // (script tag, start byte)
    let mut byte = 0usize;
    for ch in text.chars() {
        let ch_script = ch.script();
        let tag = match ch_script {
            unicode_script::Script::Common | unicode_script::Script::Inherited => current
                .map(|(s, _)| s)
                .unwrap_or_else(|| script_tag(unicode_script::Script::Common)),
            s => script_tag(s),
        };
        let ch_len = ch.len_utf8();
        match current {
            Some((cur, start)) if cur == tag => {}
            Some((cur, start)) => {
                runs.push(ScriptRun {
                    script: cur,
                    start,
                    end: byte,
                });
                current = Some((tag, byte));
            }
            None => current = Some((tag, byte)),
        }
        byte = byte.saturating_add(ch_len);
    }
    if let Some((script, start)) = current {
        runs.push(ScriptRun {
            script,
            start,
            end: byte,
        });
    }
    runs
}

/// The ISO 15924 four-letter tag of a script, as a `u32`.
fn script_tag(script: unicode_script::Script) -> u32 {
    let name = script.short_name(); // e.g. "Latn", "Hani"
    let mut tag = 0u32;
    for b in name.bytes().take(4) {
        tag = (tag << 8) | u32::from(b);
    }
    tag
}

/// The `swash` shaping backend (SL-3.SHAPE.01).
pub struct SwashShaper;

impl Shaper for SwashShaper {
    fn shape(&self, params: &ShapingParams<'_>) -> Result<ShapedBuffer> {
        let font = FontRef::from_index(params.font_data, 0).ok_or_else(|| {
            err!(
                Code::ShapeFont,
                during = "shape",
                detail = "font program invalid"
            )
        })?;
        let ppem = params.font_size; // swash's size is pixels per em = font size

        // The ISO 15924 tag is directly a swash OpenType tag; unknown scripts
        // fall back to Latin.
        let script =
            swash::text::Script::from_opentype(params.script).unwrap_or(swash::text::Script::Latin);
        let lang = params.language.and_then(swash::text::Language::parse);

        let mut ctx = ShapeContext::new();
        let mut glyphs = Vec::new();
        let mut width = 0.0f32;
        let mut shaper = ctx
            .builder(font)
            .script(script)
            .language(lang)
            .size(ppem)
            .build();
        shaper.add_str(params.text);
        shaper.shape_with(|cluster| {
            for g in cluster.glyphs {
                let gid = u16::from(g.id);
                let cluster_byte = cluster.source.start;
                glyphs.push(PositionedGlyph {
                    glyph_id: gid,
                    x_advance: g.advance,
                    y_advance: 0.0,
                    x_offset: g.x,
                    y_offset: -g.y,
                    cluster: cluster_byte,
                });
                width += g.advance;
            }
        });

        Ok(ShapedBuffer { glyphs, width })
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::indexing_slicing, clippy::arithmetic_side_effects)]

    use super::*;
    use selis_sandbox::Budget;

    const MINI_TTF: &[u8] = include_bytes!("../../selis-font/tests/fixtures/mini.ttf");

    #[test]
    fn script_runs_split_on_script_boundaries() {
        let runs = script_runs("Hello世界");
        assert_eq!(runs.len(), 2);
        assert_eq!(runs[0].script, 0x4C61746E); // Latn
        assert_eq!(&"Hello世界"[runs[0].start..runs[0].end], "Hello");
        assert_eq!(runs[1].script, 0x48616E69); // Hani
        assert_eq!(&"Hello世界"[runs[1].start..runs[1].end], "世界");
    }

    #[test]
    fn shapes_latin_run() {
        let shaper = SwashShaper;
        let params = ShapingParams {
            text: "AB",
            font_data: MINI_TTF,
            font_size: 1000.0,
            script: 0x4C61746E, // Latn
            language: Some("en"),
            features: &[],
        };
        let out = shaper.shape(&params).expect("shape");
        // A and B are simple glyphs; expect two positioned glyphs.
        assert_eq!(out.glyphs.len(), 2);
        // Advances match the fixture: A=700, B=900 at font_size=upem(1000).
        assert!((out.glyphs[0].x_advance - 700.0).abs() < 1.0);
        assert!((out.glyphs[1].x_advance - 900.0).abs() < 1.0);
        assert!((out.width - 1600.0).abs() < 2.0);
    }

    #[test]
    fn invalid_font_is_typed_error() {
        let shaper = SwashShaper;
        let params = ShapingParams {
            text: "A",
            font_data: b"not a font",
            font_size: 12.0,
            script: 0x4C61746E,
            language: None,
            features: &[],
        };
        let e = shaper.shape(&params).expect_err("invalid font");
        assert_eq!(e.code(), Code::ShapeFont);
    }

    #[test]
    fn clusters_map_to_input_bytes() {
        let shaper = SwashShaper;
        let params = ShapingParams {
            text: "AB",
            font_data: MINI_TTF,
            font_size: 1000.0,
            script: 0x4C61746E,
            language: None,
            features: &[],
        };
        let out = shaper.shape(&params).expect("shape");
        // The first glyph belongs to byte 0, the second to byte 1.
        assert_eq!(out.glyphs[0].cluster, 0);
        assert_eq!(out.glyphs[1].cluster, 1);
    }
}
