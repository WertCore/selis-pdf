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
            // Continuation of the current run: `start` was recorded when the
            // run opened and `end` only advances with the byte cursor.
            Some((cur, _start)) if cur == tag => {}
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
        // swash 0.2.10 panics on a malformed `hhea` whose `numberOfHMetrics`
        // is zero (`xmtx::advance` computes `long_metric_count - 1`;
        // fuzzer-found, SL-1.ROB.02 — no fixed swash release exists yet).
        // Reject that case with a typed error before handing the face over.
        if hmetrics_zero(params.font_data) {
            return Err(err!(
                Code::ShapeFont,
                during = "shape",
                detail = "hhea numberOfHMetrics is zero (malformed font)"
            ));
        }
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

/// Reports whether the face's `hhea` table declares `numberOfHMetrics == 0`.
///
/// Scans the sfnt table directory (one level of `ttcf` indirection for face 0)
/// without any arithmetic on document-derived values — every offset addition
/// is `checked_*` and every read is bounds-checked via `get`.
fn hmetrics_zero(font_data: &[u8]) -> bool {
    hmetrics_zero_inner(font_data, 0).unwrap_or(false)
}

fn hmetrics_zero_inner(font_data: &[u8], depth: u8) -> Option<bool> {
    if depth > 2 {
        return None;
    }
    // A collection: face 0's table directory lives at the first offset.
    let face: &[u8] = if font_data.get(0..4)? == *b"ttcf" {
        let off = read_u32(font_data, 12)?;
        font_data.get(off as usize..)?
    } else {
        font_data
    };
    let n_tables = read_u16(face, 4)?.min(4096);
    for i in 0u16..n_tables {
        let rec = usize::from(i).checked_mul(16)?.checked_add(12)?;
        if face.get(rec..rec.checked_add(4)?)? != *b"hhea" {
            continue;
        }
        // Table record: tag(4) checksum(4) offset(4) — read the offset at +8.
        let t_off = read_u32(face, rec.checked_add(8)?)?;
        // hhea: numberOfHMetrics is the final u16 field, at +34.
        let n = read_u16(face, (t_off as usize).checked_add(34)?)?;
        return Some(n == 0);
    }
    Some(false)
}

fn read_u16(data: &[u8], off: usize) -> Option<u16> {
    let b = data.get(off..off.checked_add(2)?)?;
    Some(u16::from_be_bytes([*b.first()?, *b.get(1)?]))
}

fn read_u32(data: &[u8], off: usize) -> Option<u32> {
    let b = data.get(off..off.checked_add(4)?)?;
    Some(u32::from_be_bytes([
        *b.first()?,
        *b.get(1)?,
        *b.get(2)?,
        *b.get(3)?,
    ]))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::indexing_slicing, clippy::arithmetic_side_effects)]

    use super::*;

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

    /// SL-3.SHAPE.04: complex-script text shapes without panicking; the
    /// cluster byte offsets map into the input even when the font lacks the
    /// glyphs (they fall back to `.notdef`).
    #[test]
    fn shapes_arabic_clusters() {
        let shaper = SwashShaper;
        let params = ShapingParams {
            text: "مرحبا",
            font_data: MINI_TTF, // no Arabic glyphs → .notdef, but clusters map
            font_size: 1000.0,
            script: 0x4172_6162, // Arab
            language: Some("ar"),
            features: &[],
        };
        let out = shaper.shape(&params).expect("shape");
        assert!(!out.glyphs.is_empty());
        for g in &out.glyphs {
            // The cluster byte offset is within the 10-byte input.
            assert!(g.cluster < 10);
        }
    }
}
