//! The font engine of the Selis PDF engine (Phase 3).
//!
//! Owns the font side of text: the PDF font dictionary model, encodings and
//! character mapping, embedded TrueType/CFF/Type1 outlines, CID fonts and
//! CMaps, the standard-14 metric-compatible substitution table, and
//! subsetting/re-embedding (the ADR-P0024 prerequisite).
//!
//! Open-sourced under Apache-2.0 (ADR-P0030). The crate is **COS-free**: the
//! caller (selis-pdf-content, which holds the allowed L2 edge) reads the
//! document's object graph and hands this crate plain-Rust model values. Every
//! entry point that consumes document-derived bytes takes a `Budget`
//! (ADR-P0006).

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]
#![forbid(unsafe_code)]

pub mod cid;
pub mod cjk;
pub mod cmap;
pub mod cmapfile;
pub mod encoding;
pub mod fallback;
pub mod model;
pub mod outline;
pub mod standard14;
pub mod subset;
pub mod tables;
pub mod type1;
pub mod type3;
pub mod width;

pub use cid::{resolve_cid_widths, CidWidthEntry, CidWidths};
pub use cjk::{chunk_for, is_cjk, CjkChunk, CHUNKS};
pub use cmap::{select_cmap, CmapEncoding};
pub use cmapfile::{parse_cmap, CMap, CidRange};
pub use encoding::{resolve, BaseEncoding, DifferenceItem, Encoding, FontEncoding};
pub use fallback::{can_render, fallback_order, match_substitute, substitute, FontMatchHints};
pub use model::{FontDescriptor, FontDict, FontFile, FontSubtype};
pub use outline::{glyph_count, glyph_id_for_name, glyph_name, outline_glyph, Outline, OutlineCmd};
pub use standard14::{is_standard, width as standard14_width, FontMetrics, GlyphWidth, ALL_FONTS};
pub use subset::{add_glyph, subset_ttf, GlyphSet};
pub use type1::{
    glyph_count as type1_glyph_count, glyph_name as type1_glyph_name, parse as parse_type1,
    Type1Font,
};
pub use type3::{glyph_metrics, GlyphProcedureMetrics, Type3Font};
pub use width::{parse_ttf_metrics, resolve as resolve_widths, ResolvedFont};
