//! TrueType font subsetting and re-embedding (SL-3.FONT.11).
//!
//! Subsets a TrueType font to a glyph set, transitively including the
//! components of composite glyphs, serialises a valid new SFNT font, and
//! **merges new glyphs into an existing subset** — the ADR-P0024 editing
//! path. CFF fonts are currently returned as-is (deviation — CFF subsetting
//! lands in a follow-up).
//!
//! The output font uses:
//! * `loca` long format (indexToLocFormat = 1) — always correct, no format
//!   ambiguity;
//! * `cmap` format 12 (segmented coverage) — the simplest for Unicode
//!   subsets;
//! * checksum adjustment per the TrueType/OpenType spec (§14 — the whole
//!   file's sum plus the head table's `checksumAdjustment` field zeroes out).

use std::collections::{BTreeMap, BTreeSet};

use selis_bytes::Bytes;
use selis_error::Result;
use selis_sandbox::{alloc, BudgetGuard};
use skrifa::instance::{LocationRef, Size};
use skrifa::{FontRef, MetadataProvider, Tag};

use crate::outline::OutlineCmd;

/// The glyph ids to keep in a subset.
#[derive(Debug, Clone)]
pub struct GlyphSet {
    /// The glyph ids (0-based) to keep. Duplicates and out-of-range ids are
    /// silently ignored; `.notdef` (glyph 0) is always kept.
    pub keep: Vec<u16>,
}

/// Subset a TrueType font to the glyphs in `keep`, transitively including
/// composite components.
///
/// Returns `None` for a broken font (deviation) or for CFF fonts (deferred).
///
/// # Budget
///
/// Charges the output font bytes (document-derived; the output is bounded by
/// the input).
///
/// # Malformed Input
///
/// `BUDGET_BYTES` when exhausted. A broken font → `Ok(None)`.
pub fn subset_ttf(data: &Bytes, keep: &GlyphSet, g: &mut BudgetGuard<'_>) -> Result<Option<Bytes>> {
    let font = match FontRef::new(data.as_slice()) {
        Ok(f) => f,
        Err(_) => return Ok(None),
    };
    if font.table_data(Tag::new(b"glyf")).is_none() {
        return Ok(None); // CFF fonts are not subsetted (follow-up)
    }
    let Some(glyf_d) = font.table_data(Tag::new(b"glyf")) else {
        return Ok(None);
    };
    let Some(head_d) = font.table_data(Tag::new(b"head")) else {
        return Ok(None);
    };
    let Some(hmtx_d) = font.table_data(Tag::new(b"hmtx")) else {
        return Ok(None);
    };
    let Some(maxp_d) = font.table_data(Tag::new(b"maxp")) else {
        return Ok(None);
    };
    let Some(hhea_d) = font.table_data(Tag::new(b"hhea")) else {
        return Ok(None);
    };
    let Some(loca_d) = font.table_data(Tag::new(b"loca")) else {
        return Ok(None);
    };
    let glyf = glyf_d.as_bytes();
    let head = head_d.as_bytes();
    let hmtx = hmtx_d.as_bytes();
    let maxp = maxp_d.as_bytes();
    let hhea = hhea_d.as_bytes();
    let loca = loca_d.as_bytes();

    let metrics = font.metrics(Size::unscaled(), LocationRef::default());
    let num_glyphs = metrics.glyph_count;

    let mut keep_set: BTreeSet<u16> = keep.keep.iter().copied().collect();
    keep_set.retain(|&g| g < num_glyphs);
    keep_set.insert(0); // .notdef must always be present
    composite_closure(glyf, loca, head, num_glyphs, &mut keep_set);

    let loca_is_short = read_u16(head, 50) == 0; // indexToLocFormat

    let keep_sorted: Vec<u16> = keep_set.iter().copied().collect();
    let renumber: BTreeMap<u16, u16> = keep_sorted
        .iter()
        .copied()
        .enumerate()
        .map(|(new, old)| (old, u16::try_from(new).unwrap_or(u16::MAX)))
        .collect();
    let new_count = u16::try_from(keep_sorted.len()).unwrap_or(u16::MAX);

    // Build new glyf + loca offsets.
    let mut new_loca_offsets: Vec<u32> = Vec::new();
    let mut new_glyf = Vec::new();
    for &old_gid in &keep_sorted {
        let offset = loca_offset(loca, old_gid, loca_is_short);
        let next_offset = loca_offset(loca, old_gid.saturating_add(1), loca_is_short);
        new_loca_offsets.push(u32::try_from(new_glyf.len()).unwrap_or(u32::MAX));
        let len = next_offset.saturating_sub(offset);
        let start = offset;
        let end = start
            .saturating_add(len)
            .min(u32::try_from(glyf.len()).unwrap_or(u32::MAX));
        if start < end {
            let start = usize::try_from(start).unwrap_or(0);
            let end = usize::try_from(end).unwrap_or(0);
            if let Some(slice) = glyf.get(start..end) {
                new_glyf.extend_from_slice(slice);
            }
        }
    }
    new_loca_offsets.push(u32::try_from(new_glyf.len()).unwrap_or(u32::MAX));

    // Build new hmtx.
    let orig_num_hmetrics = read_u16(hhea, 34) as u32;
    let n = orig_num_hmetrics.min(new_count as u32);
    let mut new_hmtx = Vec::new();
    for &old_gid in keep_sorted.iter().take(usize::try_from(n).unwrap_or(0)) {
        let off = u32::from(old_gid).saturating_mul(4);
        let aw = read_u16(hmtx, off);
        let lsb = read_u16(hmtx, off.saturating_add(2));
        new_hmtx.extend_from_slice(&aw.to_be_bytes());
        new_hmtx.extend_from_slice(&lsb.to_be_bytes());
    }

    // Build new cmap (format 12).
    let new_cmap = build_cmap_format12(&font, &keep_sorted, &renumber);

    // Build head (indexToLocFormat = long).
    let mut new_head = slice_prefix(head, 54);
    set_u16(&mut new_head, 50, 1); // indexToLocFormat
    set_bytes(&mut new_head, 36, &compute_subset_bbox(&new_glyf)); // xMin..yMax

    // Build maxp — keep the full table, update numGlyphs at offset 4.
    let mut new_maxp = slice_prefix(maxp, 32);
    set_u16(&mut new_maxp, 4, new_count);

    // Build hhea — keep the full table, update advanceWidthMax and
    // numberOfHMetrics.
    let mut new_hhea = slice_prefix(hhea, 36);
    let max_aw = max_advance(&new_hmtx);
    set_u16(&mut new_hhea, 2, max_aw);
    set_u16(&mut new_hhea, 34, new_count);

    // Build post (format 3.0: no names).
    let new_post = build_post_format3();

    let name_data = font
        .table_data(Tag::new(b"name"))
        .map(|d| d.as_bytes())
        .unwrap_or(&[]);
    let tables: [(&str, &[u8]); 9] = [
        ("cmap", &new_cmap),
        ("glyf", &new_glyf),
        ("head", &new_head),
        ("hhea", &new_hhea),
        ("hmtx", &new_hmtx),
        ("loca", &new_loca_bytes(&new_loca_offsets)),
        ("maxp", &new_maxp),
        ("name", name_data),
        ("post", &new_post),
    ];
    // assemble_sfnt charges the output buffer through the sandbox allocator.
    let output = assemble_sfnt(&tables, g)?;

    let patched = patch_checksum(&output);
    Ok(Some(Bytes::copy_from_slice(&patched)))
}

/// Merge a new glyph into an existing subset font (ADR-P0024 editing path).
///
/// Appends a simple glyph (a polygon of `Move`/`Line`/`Close` commands; curves
/// must be flattened by the caller) built from `outline`, maps `code` to it in
/// the new cmap, and re-serialises the font.
///
/// Returns `None` for a broken font, a CFF font, or an outline that cannot be
/// turned into a simple glyph (deviation, never an error).
///
/// # Budget
///
/// Charges the output font bytes.
///
/// # Malformed Input
///
/// `BUDGET_BYTES` when exhausted; otherwise `Ok(None)` for a broken font.
pub fn add_glyph(
    data: &Bytes,
    code: u32,
    outline: &[OutlineCmd],
    advance: u16,
    g: &mut BudgetGuard<'_>,
) -> Result<Option<Bytes>> {
    let font = match FontRef::new(data.as_slice()) {
        Ok(f) => f,
        Err(_) => return Ok(None),
    };
    if font.table_data(Tag::new(b"glyf")).is_none() {
        return Ok(None); // CFF fonts are not merged (follow-up)
    }
    let Some(glyf_d) = font.table_data(Tag::new(b"glyf")) else {
        return Ok(None);
    };
    let Some(head_d) = font.table_data(Tag::new(b"head")) else {
        return Ok(None);
    };
    let Some(hmtx_d) = font.table_data(Tag::new(b"hmtx")) else {
        return Ok(None);
    };
    let Some(maxp_d) = font.table_data(Tag::new(b"maxp")) else {
        return Ok(None);
    };
    let Some(hhea_d) = font.table_data(Tag::new(b"hhea")) else {
        return Ok(None);
    };
    let Some(loca_d) = font.table_data(Tag::new(b"loca")) else {
        return Ok(None);
    };
    let glyf = glyf_d.as_bytes();
    let head = head_d.as_bytes();
    let hmtx = hmtx_d.as_bytes();
    let maxp = maxp_d.as_bytes();
    let hhea = hhea_d.as_bytes();
    let loca = loca_d.as_bytes();

    let metrics = font.metrics(Size::unscaled(), LocationRef::default());
    let num_glyphs = metrics.glyph_count;
    let loca_is_short = read_u16(head, 50) == 0;

    // The new simple glyph.
    let Some(new_glyph) = build_simple_glyph(outline) else {
        return Ok(None);
    };
    let new_gid = num_glyphs;

    // Build the new glyf + loca (append the new glyph).
    let mut new_glyf = Vec::new();
    let mut new_loca_offsets = Vec::new();
    for old_gid in 0..num_glyphs {
        let offset = loca_offset(loca, old_gid, loca_is_short);
        let next_offset = loca_offset(loca, old_gid.saturating_add(1), loca_is_short);
        new_loca_offsets.push(u32::try_from(new_glyf.len()).unwrap_or(u32::MAX));
        let start = offset;
        let end = next_offset.min(u32::try_from(glyf.len()).unwrap_or(u32::MAX));
        if start < end {
            let s = usize::try_from(start).unwrap_or(0);
            let e = usize::try_from(end).unwrap_or(0);
            if let Some(slice) = glyf.get(s..e) {
                new_glyf.extend_from_slice(slice);
            }
        }
    }
    new_loca_offsets.push(u32::try_from(new_glyf.len()).unwrap_or(u32::MAX));
    new_loca_offsets
        .push(u32::try_from(new_glyf.len().saturating_add(new_glyph.len())).unwrap_or(u32::MAX));
    new_glyf.extend_from_slice(&new_glyph);

    // Build the new hmtx (append the advance).
    let orig_num_hmetrics = read_u16(hhea, 34) as u32;
    let mut new_hmtx = Vec::new();
    for old_gid in 0..orig_num_hmetrics.min(u32::from(num_glyphs)) {
        let off = old_gid.saturating_mul(4);
        let aw = read_u16(hmtx, off);
        let lsb = read_u16(hmtx, off.saturating_add(2));
        new_hmtx.extend_from_slice(&aw.to_be_bytes());
        new_hmtx.extend_from_slice(&lsb.to_be_bytes());
    }
    let lsb = read_i16_at(head, 44); // xMin as the new glyph's lsb
    new_hmtx.extend_from_slice(&advance.to_be_bytes());
    new_hmtx.extend_from_slice(&lsb.to_be_bytes());

    // Build the new cmap (add the new mapping).
    let mut code_to_new: BTreeMap<u32, u16> = BTreeMap::new();
    for (c, old_gid) in font.charmap().mappings() {
        let old_gid16 = u16::try_from(old_gid.to_u32()).unwrap_or(u16::MAX);
        code_to_new.insert(c, old_gid16);
    }
    code_to_new.insert(code, new_gid);
    let new_cmap = build_cmap_format12_from_map(&code_to_new);

    // Build head (indexToLocFormat = long).
    let mut new_head = slice_prefix(head, 54);
    set_u16(&mut new_head, 50, 1);
    set_bytes(&mut new_head, 36, &bbox_bytes(&new_glyf, new_glyph.len()));

    // Build maxp.
    let mut new_maxp = slice_prefix(maxp, 32);
    set_u16(&mut new_maxp, 4, num_glyphs.saturating_add(1));

    // Build hhea.
    let mut new_hhea = slice_prefix(hhea, 36);
    let max_aw = max_advance(&new_hmtx);
    set_u16(&mut new_hhea, 2, max_aw);
    set_u16(&mut new_hhea, 34, num_glyphs.saturating_add(1));

    let new_post = build_post_format3();
    let name_data = font
        .table_data(Tag::new(b"name"))
        .map(|d| d.as_bytes())
        .unwrap_or(&[]);
    let tables: [(&str, &[u8]); 9] = [
        ("cmap", &new_cmap),
        ("glyf", &new_glyf),
        ("head", &new_head),
        ("hhea", &new_hhea),
        ("hmtx", &new_hmtx),
        ("loca", &new_loca_bytes(&new_loca_offsets)),
        ("maxp", &new_maxp),
        ("name", name_data),
        ("post", &new_post),
    ];
    // assemble_sfnt charges the output buffer through the sandbox allocator.
    let output = assemble_sfnt(&tables, g)?;

    let patched = patch_checksum(&output);
    Ok(Some(Bytes::copy_from_slice(&patched)))
}

/// The `[xMin yMin xMax yMax]` bytes for a glyf whose last `new_len` bytes are
/// the new glyph (the first glyph supplies the bbox).
fn bbox_bytes(glyf: &[u8], new_len: usize) -> [u8; 8] {
    let _ = new_len;
    let mut out = [0u8; 8];
    if glyf.len() < 10 {
        return out;
    }
    out[0..2].copy_from_slice(&read_i16_at(glyf, 2).to_be_bytes());
    out[2..4].copy_from_slice(&read_i16_at(glyf, 4).to_be_bytes());
    out[4..6].copy_from_slice(&read_i16_at(glyf, 6).to_be_bytes());
    out[6..8].copy_from_slice(&read_i16_at(glyf, 8).to_be_bytes());
    out
}

/// Build a simple TrueType glyph (polygon) from `Move`/`Line`/`Close`
/// commands. Curves return `None`.
fn build_simple_glyph(commands: &[OutlineCmd]) -> Option<Vec<u8>> {
    let mut contours: Vec<Vec<(i16, i16)>> = Vec::new();
    let mut current: Vec<(i16, i16)> = Vec::new();
    for cmd in commands {
        match cmd {
            OutlineCmd::Move { x, y } => {
                if !current.is_empty() {
                    contours.push(std::mem::take(&mut current));
                }
                current.push((num_i16(*x)?, num_i16(*y)?));
            }
            OutlineCmd::Line { x, y } => {
                current.push((num_i16(*x)?, num_i16(*y)?));
            }
            OutlineCmd::Close => {
                if !current.is_empty() {
                    contours.push(std::mem::take(&mut current));
                }
            }
            _ => return None, // curves must be flattened by the caller
        }
    }
    if !current.is_empty() {
        contours.push(current);
    }
    if contours.is_empty() {
        return None;
    }
    // Compute the point list and bbox.
    let mut points: Vec<(i16, i16)> = Vec::new();
    let mut end_pts: Vec<u16> = Vec::new();
    let mut x_min = i16::MAX;
    let mut y_min = i16::MAX;
    let mut x_max = i16::MIN;
    let mut y_max = i16::MIN;
    for c in &contours {
        for &(x, y) in c {
            x_min = x_min.min(x);
            y_min = y_min.min(y);
            x_max = x_max.max(x);
            y_max = y_max.max(y);
        }
        end_pts.push(
            u16::try_from(points.len().saturating_add(c.len()).saturating_sub(1))
                .unwrap_or(u16::MAX),
        );
        points.extend_from_slice(c);
    }
    if x_min > x_max || y_min > y_max {
        return None;
    }
    // Encode the glyph: contours, bbox, endPts, instructionLength=0, flags,
    // x deltas, y deltas.
    let mut out = Vec::new();
    out.extend_from_slice(
        &i16::try_from(contours.len())
            .unwrap_or(i16::MAX)
            .to_be_bytes(),
    );
    out.extend_from_slice(&x_min.to_be_bytes());
    out.extend_from_slice(&y_min.to_be_bytes());
    out.extend_from_slice(&x_max.to_be_bytes());
    out.extend_from_slice(&y_max.to_be_bytes());
    for e in &end_pts {
        out.extend_from_slice(&e.to_be_bytes());
    }
    out.extend_from_slice(&0u16.to_be_bytes()); // instructionLength
    let mut flags: Vec<u8> = Vec::new();
    let mut xs: Vec<u8> = Vec::new();
    let mut ys: Vec<u8> = Vec::new();
    let mut prev_x = 0i16;
    let mut prev_y = 0i16;
    for &(x, y) in &points {
        let dx = x.wrapping_sub(prev_x);
        let dy = y.wrapping_sub(prev_y);
        prev_x = x;
        prev_y = y;
        let mut flag = 0x01u8; // ON_CURVE_POINT
        if dx >= 0 && i32::from(dx) <= 255 {
            flag |= 0x02 | 0x10; // X_SHORT_VECTOR + positive
                                 // dx is 0..=255 here — the narrowing is exact in range.
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            xs.push(dx as u8);
        } else if dx < 0 && i32::from(dx) >= -255 {
            flag |= 0x02; // X_SHORT_VECTOR (negative)
                          // |dx| is 1..=255 here — the narrowing is exact in range.
            #[allow(clippy::cast_possible_truncation)]
            xs.push(dx.unsigned_abs() as u8);
        } else {
            xs.extend_from_slice(&dx.to_be_bytes());
        }
        if dy >= 0 && i32::from(dy) <= 255 {
            flag |= 0x04 | 0x20; // Y_SHORT_VECTOR + positive
                                 // dy is 0..=255 here — the narrowing is exact in range.
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            ys.push(dy as u8);
        } else if dy < 0 && i32::from(dy) >= -255 {
            flag |= 0x04; // Y_SHORT_VECTOR (negative)
                          // |dy| is 1..=255 here — the narrowing is exact in range.
            #[allow(clippy::cast_possible_truncation)]
            ys.push(dy.unsigned_abs() as u8);
        } else {
            ys.extend_from_slice(&dy.to_be_bytes());
        }
        flags.push(flag);
    }
    out.extend_from_slice(&flags);
    out.extend_from_slice(&xs);
    out.extend_from_slice(&ys);
    Some(out)
}

/// A finite f64 within the TrueType coordinate range, as i16.
fn num_i16(v: f64) -> Option<i16> {
    if !v.is_finite() || v < -32768.0 || v > 32767.0 {
        return None;
    }
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    {
        Some(v as i16)
    }
}

/// Copy the first `n` bytes of a slice into a new Vec (never panics).
fn slice_prefix(data: &[u8], n: usize) -> Vec<u8> {
    data.iter().take(n).copied().collect()
}

/// Set a big-endian u16 at an offset (no-op when out of bounds).
fn set_u16(data: &mut [u8], off: usize, value: u16) {
    let bytes = value.to_be_bytes();
    if let Some(slot) = data.get_mut(off) {
        *slot = bytes[0];
    }
    if let Some(slot) = data.get_mut(off.saturating_add(1)) {
        *slot = bytes[1];
    }
}

/// Copy bytes into a slice at an offset (no-op when out of bounds).
fn set_bytes(data: &mut [u8], off: usize, value: &[u8]) {
    let end = off.saturating_add(value.len());
    if end <= data.len() {
        if let Some(slot) = data.get_mut(off..end) {
            slot.copy_from_slice(value);
        }
    }
}

fn new_loca_bytes(offsets: &[u32]) -> Vec<u8> {
    let mut out = Vec::new();
    for &o in offsets {
        out.extend_from_slice(&o.to_be_bytes());
    }
    out
}

fn read_u16(data: &[u8], off: u32) -> u16 {
    let off = usize::try_from(off).unwrap_or(0);
    let hi = data.get(off).copied().unwrap_or(0);
    let lo = data.get(off.saturating_add(1)).copied().unwrap_or(0);
    u16::from_be_bytes([hi, lo])
}

fn read_u32(data: &[u8], off: u32) -> u32 {
    let off = usize::try_from(off).unwrap_or(0);
    let b0 = data.get(off).copied().unwrap_or(0);
    let b1 = data.get(off.saturating_add(1)).copied().unwrap_or(0);
    let b2 = data.get(off.saturating_add(2)).copied().unwrap_or(0);
    let b3 = data.get(off.saturating_add(3)).copied().unwrap_or(0);
    u32::from_be_bytes([b0, b1, b2, b3])
}

fn loca_offset(loca: &[u8], gid: u16, is_short: bool) -> u32 {
    if is_short {
        let off = u32::from(gid).saturating_mul(2);
        u32::from(read_u16(loca, off)).saturating_mul(2)
    } else {
        let off = u32::from(gid).saturating_mul(4);
        read_u32(loca, off)
    }
}

/// Add composite components to the keep set.
fn composite_closure(
    glyf: &[u8],
    loca: &[u8],
    head: &[u8],
    num_glyphs: u16,
    keep: &mut BTreeSet<u16>,
) {
    let is_short = read_u16(head, 50) == 0; // indexToLocFormat
    let mut added = true;
    while added {
        added = false;
        for gid in keep.clone().iter().copied() {
            if gid >= num_glyphs {
                continue;
            }
            let off = loca_offset(loca, gid, is_short);
            let off = usize::try_from(off).unwrap_or(0);
            let cnt = read_i16_at(glyf, off);
            if cnt != -1 {
                continue;
            }
            let mut pos = off.saturating_add(10);
            loop {
                let flags = read_u16_at(glyf, pos);
                let comp_gid = read_u16_at(glyf, pos.saturating_add(2));
                if comp_gid < num_glyphs && keep.insert(comp_gid) {
                    added = true;
                }
                pos = pos.saturating_add(4); // flags + glyphIndex
                if flags & 0x0001 != 0 {
                    pos = pos.saturating_add(4); // word args
                } else {
                    pos = pos.saturating_add(2); // byte args
                }
                if flags & 0x0008 != 0 {
                    pos = pos.saturating_add(2);
                } else if flags & 0x0040 != 0 {
                    pos = pos.saturating_add(4);
                } else if flags & 0x0080 != 0 {
                    pos = pos.saturating_add(8);
                }
                if flags & 0x0020 == 0 {
                    break; // no more components
                }
            }
        }
    }
}

fn read_i16_at(data: &[u8], off: usize) -> i16 {
    let hi = data.get(off).copied().unwrap_or(0);
    let lo = data.get(off.saturating_add(1)).copied().unwrap_or(0);
    i16::from_be_bytes([hi, lo])
}

fn read_u16_at(data: &[u8], off: usize) -> u16 {
    let hi = data.get(off).copied().unwrap_or(0);
    let lo = data.get(off.saturating_add(1)).copied().unwrap_or(0);
    u16::from_be_bytes([hi, lo])
}

/// The first glyph's bounding box `[xMin yMin xMax yMax]` as 8 bytes.
fn compute_subset_bbox(glyf: &[u8]) -> [u8; 8] {
    let mut out = [0u8; 8];
    if glyf.len() < 10 {
        return out;
    }
    out[0..2].copy_from_slice(&read_i16_at(glyf, 2).to_be_bytes());
    out[2..4].copy_from_slice(&read_i16_at(glyf, 4).to_be_bytes());
    out[4..6].copy_from_slice(&read_i16_at(glyf, 6).to_be_bytes());
    out[6..8].copy_from_slice(&read_i16_at(glyf, 8).to_be_bytes());
    out
}

fn build_cmap_format12(font: &FontRef<'_>, keep: &[u16], renumber: &BTreeMap<u16, u16>) -> Vec<u8> {
    // keep scopes the subset; the format-12 table maps every charmap entry
    // that survives renumbering, which is exactly the kept set's image.
    let _ = keep;
    let mut code_to_new: BTreeMap<u32, u16> = BTreeMap::new();
    for (code, old_gid) in font.charmap().mappings() {
        let old_gid16 = u16::try_from(old_gid.to_u32()).unwrap_or(u16::MAX);
        if let Some(&new_gid) = renumber.get(&old_gid16) {
            code_to_new.insert(code, new_gid);
        }
    }
    build_cmap_format12_from_map(&code_to_new)
}

fn build_cmap_format12_from_map(code_to_new: &BTreeMap<u32, u16>) -> Vec<u8> {
    let mut groups: Vec<[u32; 3]> = Vec::new(); // [start, end, start_gid]
    let mut iter = code_to_new.iter();
    if let Some((&code, &gid)) = iter.next() {
        let mut start = code;
        let mut end = code;
        let mut start_gid = u32::from(gid);
        for (&code, &gid) in iter {
            if code == end.saturating_add(1)
                && u32::from(gid) == start_gid.saturating_add(code.saturating_sub(start))
            {
                end = code;
            } else {
                groups.push([start, end, start_gid]);
                start = code;
                end = code;
                start_gid = u32::from(gid);
            }
        }
        groups.push([start, end, start_gid]);
    }
    let ng = u32::try_from(groups.len()).unwrap_or(u32::MAX);
    let subtable_size = 16u32.saturating_add(12u32.saturating_mul(ng));
    let mut out = Vec::new();
    out.extend_from_slice(&0u16.to_be_bytes()); // version
    out.extend_from_slice(&1u16.to_be_bytes()); // numTables
    out.extend_from_slice(&3u16.to_be_bytes()); // platform
    out.extend_from_slice(&1u16.to_be_bytes()); // encoding
    out.extend_from_slice(&12u32.to_be_bytes()); // subtable offset
    out.extend_from_slice(&12u16.to_be_bytes()); // format
    out.extend_from_slice(&0u16.to_be_bytes()); // reserved
    out.extend_from_slice(&subtable_size.to_be_bytes()); // length
    out.extend_from_slice(&0u32.to_be_bytes()); // language
    out.extend_from_slice(&ng.to_be_bytes()); // numGroups
    for g in &groups {
        out.extend_from_slice(&g[0].to_be_bytes());
        out.extend_from_slice(&g[1].to_be_bytes());
        out.extend_from_slice(&g[2].to_be_bytes());
    }
    out
}

fn build_post_format3() -> Vec<u8> {
    let mut out = [0u8; 32];
    set_u32(&mut out, 0, 3); // formatType
    out.to_vec()
}

fn set_u32(data: &mut [u8], off: usize, value: u32) {
    let bytes = value.to_be_bytes();
    for (i, b) in bytes.iter().enumerate() {
        if let Some(slot) = data.get_mut(off.saturating_add(i)) {
            *slot = *b;
        }
    }
}

fn assemble_sfnt(tables: &[(&str, &[u8])], g: &mut BudgetGuard<'_>) -> Result<Vec<u8>> {
    let num_tables = u16::try_from(tables.len()).unwrap_or(u16::MAX);
    let dir_size = 12u32.saturating_add(u32::from(num_tables).saturating_mul(16));
    let mut offset = dir_size;
    let mut entries = Vec::new();
    for (tag, data) in tables {
        let l = u32::try_from(data.len()).unwrap_or(u32::MAX);
        let padded_len = l.saturating_add(3) & !3;
        entries.push((tag.as_bytes(), data, padded_len, offset));
        offset = offset.saturating_add(padded_len);
    }
    let mut out = alloc::vec_filled(g, usize::try_from(offset).unwrap_or(usize::MAX), 0u8)?;
    set_bytes(&mut out, 0, b"\x00\x01\x00\x00");
    set_u16(&mut out, 4, num_tables);
    let mut pos = 12usize;
    for (tag_bytes, _data, padded_len, data_offset) in &entries {
        set_bytes(&mut out, pos, tag_bytes);
        set_u32(&mut out, pos.saturating_add(8), *data_offset);
        set_u32(&mut out, pos.saturating_add(12), *padded_len);
        pos = pos.saturating_add(16);
    }
    for (_tag_bytes, data, _padded_len, data_offset) in &entries {
        let start = usize::try_from(*data_offset).unwrap_or(0);
        let end = start.saturating_add(data.len());
        if let Some(slot) = out.get_mut(start..end) {
            slot.copy_from_slice(data);
        }
    }
    Ok(out)
}

/// Compute the checksumAdjustment and patch it into the head table.
fn patch_checksum(output: &[u8]) -> Vec<u8> {
    let mut patched = output.to_vec();
    let num_tables = read_u16(output, 4);
    let mut head_offset = 0u32;
    let mut head_pos = 12usize;
    for _ in 0..num_tables {
        let tag = output.get(head_pos..head_pos.saturating_add(4));
        let off = read_u32(
            output,
            u32::try_from(head_pos.saturating_add(8)).unwrap_or(0),
        );
        if tag == Some(b"head") {
            head_offset = off;
            break;
        }
        head_pos = head_pos.saturating_add(16);
    }
    let cksum_field_off = usize::try_from(head_offset.saturating_add(8)).unwrap_or(0);
    let mut sum = 0u32;
    let mut i = 0usize;
    while i.saturating_add(4) <= patched.len() {
        let v = read_u32(&patched, u32::try_from(i).unwrap_or(0));
        if i >= cksum_field_off && i < cksum_field_off.saturating_add(4) {
            sum = sum.wrapping_add(0);
        } else {
            sum = sum.wrapping_add(v);
        }
        i = i.saturating_add(4);
    }
    let adjustment = 0xB1B0AFBAu32.wrapping_sub(sum);
    set_u32(&mut patched, cksum_field_off, adjustment);
    patched
}

/// The maximum advance width in a big-endian hmtx entry list.
fn max_advance(hmtx: &[u8]) -> u16 {
    let mut max = 0u16;
    let mut i = 0usize;
    while i.saturating_add(4) <= hmtx.len() {
        let aw = read_u16(hmtx, u32::try_from(i).unwrap_or(0));
        max = max.max(aw);
        i = i.saturating_add(4);
    }
    max
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::outline::outline_glyph;
    use selis_sandbox::Budget;

    fn guard() -> BudgetGuard<'static> {
        Budget::unlimited().guard()
    }

    #[test]
    fn subset_drops_unwanted_glyphs() {
        let mut g = guard();
        let data = Bytes::copy_from_slice(include_bytes!("../tests/fixtures/mini.ttf"));
        let keep = GlyphSet { keep: vec![1] };
        let subset = subset_ttf(&data, &keep, &mut g)
            .expect("subset")
            .expect("font");
        let font = FontRef::new(subset.as_slice()).expect("parse subset");
        let met = font.metrics(Size::unscaled(), LocationRef::default());
        // .notdef + A.
        assert_eq!(met.glyph_count, 2);
        let o0 = outline_glyph(&Bytes::copy_from_slice(subset.as_slice()), 0, &mut g)
            .expect("outline")
            .expect("gid 0");
        assert_eq!(o0.bbox.expect("bbox"), [0.0, 0.0, 500.0, 500.0]);
        let o1 = outline_glyph(&Bytes::copy_from_slice(subset.as_slice()), 1, &mut g)
            .expect("outline")
            .expect("gid 1");
        assert_eq!(o1.bbox.expect("bbox"), [0.0, 0.0, 700.0, 600.0]);
        // The charmap no longer maps B (0x42) — only A (0x41).
        assert!(font.charmap().map(0x41u32).is_some());
        assert!(font.charmap().map(0x42u32).is_none());
        assert!(
            outline_glyph(&Bytes::copy_from_slice(subset.as_slice()), 2, &mut g)
                .expect("outline")
                .is_none()
        );
    }

    #[test]
    fn composite_components_are_transitively_kept() {
        let mut g = guard();
        let data = Bytes::copy_from_slice(include_bytes!("../tests/fixtures/composite.ttf"));
        let keep = GlyphSet { keep: vec![2] };
        let subset = subset_ttf(&data, &keep, &mut g)
            .expect("subset")
            .expect("font");
        let font = FontRef::new(subset.as_slice()).expect("parse subset");
        let met = font.metrics(Size::unscaled(), LocationRef::default());
        // .notdef + A (component) + B.
        assert_eq!(met.glyph_count, 3);
        let b = outline_glyph(&Bytes::copy_from_slice(subset.as_slice()), 2, &mut g)
            .expect("outline")
            .expect("gid 2 (B)");
        assert_eq!(b.bbox.expect("bbox"), [0.0, 0.0, 1400.0, 600.0]);
    }

    #[test]
    fn broken_font_returns_none() {
        let mut g = guard();
        let garbage = Bytes::copy_from_slice(b"not a font");
        let keep = GlyphSet { keep: vec![0] };
        assert!(subset_ttf(&garbage, &keep, &mut g)
            .expect("subset")
            .is_none());
    }

    /// DoD: merge a new glyph into a subset — add a glyph that was absent
    /// from the original font and verify it renders.
    #[test]
    fn add_new_glyph_to_subset() {
        let mut g = guard();
        let data = Bytes::copy_from_slice(include_bytes!("../tests/fixtures/mini.ttf"));
        // Keep only .notdef (gid 0) and A (gid 1).
        let subset = subset_ttf(&data, &GlyphSet { keep: vec![1] }, &mut g)
            .expect("subset")
            .expect("font");
        // Add a new glyph for code 0x43 ('C') with a 200×300 box, advance 400.
        let new_font = add_glyph(
            &subset,
            0x43,
            &[
                OutlineCmd::Move { x: 0.0, y: 0.0 },
                OutlineCmd::Line { x: 0.0, y: 300.0 },
                OutlineCmd::Line { x: 200.0, y: 300.0 },
                OutlineCmd::Line { x: 200.0, y: 0.0 },
                OutlineCmd::Close,
            ],
            400,
            &mut g,
        )
        .expect("add_glyph")
        .expect("new font");
        let font = FontRef::new(new_font.as_slice()).expect("parse");
        let met = font.metrics(Size::unscaled(), LocationRef::default());
        // .notdef + A + C = 3 glyphs.
        assert_eq!(met.glyph_count, 3);
        // The charmap maps 0x43 → gid 2.
        let c_gid = font.charmap().map(0x43u32).expect("C in cmap");
        assert_eq!(c_gid.to_u32(), 2);
        // Render gid 2 and verify it's the 200×300 box.
        let c = outline_glyph(&Bytes::copy_from_slice(new_font.as_slice()), 2, &mut g)
            .expect("outline")
            .expect("gid 2 (C)");
        assert_eq!(c.bbox.expect("bbox"), [0.0, 0.0, 200.0, 300.0]);
        // The original glyphs are unchanged.
        let a = outline_glyph(&Bytes::copy_from_slice(new_font.as_slice()), 1, &mut g)
            .expect("outline")
            .expect("gid 1 (A)");
        assert_eq!(a.bbox.expect("bbox"), [0.0, 0.0, 700.0, 600.0]);
    }
}
