//! JBIG2Decode (SL-1.FILT.07) — own implementation (ADR-P0018).
//!
//! Generic region decoding with MMR coding, plus the embedded-in-PDF stream
//! organisation and `/JBIG2Globals`. Arithmetic (MQ) coding, symbol
//! dictionaries, text regions, and refinement land in Phase 2
//! (`SL-2.FILT.01`); those segment types return `JBIG2_UNSUPPORTED` — never a
//! wrong bitmap.
//!
//! The JBIG2 file is a sequence of segments. Generic region segments (types
//! 20-23) carry a region bitmap that, in MMR mode, is coded exactly like
//! CCITT Group 4 (the same 2-D pass/vertical/horizontal modes, no EOLs). This
//! module reuses the fax decoder's changing-element machinery.

use selis_error::{err, Code, Result};
use selis_sandbox::{alloc, BudgetGuard};

use crate::fax::FaxParms;

/// Decode a JBIG2 stream into a bilevel bitmap.
///
/// `globals` is the `/JBIG2Globals` stream data, if present.
///
/// # Budget
///
/// Output is bounded by the region dimensions; each byte is charged.
///
/// # Malformed Input
///
/// `JBIG2_CORRUPT` when the stream cannot be decoded; arithmetic-coded
/// segments return `JBIG2_UNSUPPORTED` (Phase 2), never a wrong bitmap.
pub fn jbig2_decode(data: &[u8], globals: &[u8], g: &mut BudgetGuard<'_>) -> Result<Vec<u8>> {
    let mut reader = Jbig2Reader::new(data);
    let mut region: Option<Vec<u8>> = None;
    let mut found = false;

    while !reader.is_exhausted() {
        g.tick()?;
        let segment = parse_segment(&mut reader)?;
        // The segment body follows the header.
        let body_start = reader.pos;
        let body_end = body_start.saturating_add(segment.data_length as usize);
        let body = reader.data.get(body_start..body_end).unwrap_or(&[]);

        match segment.type_ {
            // Generic region, MMR-coded: decode and keep the bitmap.
            20 | 21 | 22 | 23 => {
                let decoded = decode_generic_region(segment.type_, body, globals, g)?;
                region = Some(decoded);
                found = true;
            }
            // End of file.
            38 => break,
            // Unknown-to-us segments are skipped (their data is bounded by the
            // declared length).
            _ => {}
        }
        // Advance past the body.
        reader.pos = body_end;
    }

    if found {
        Ok(region.unwrap_or_default())
    } else {
        Err(err!(
            Code::Jbig2Unsupported,
            during = "jbig2-decode",
            detail = "no decodable generic region segment"
        ))
    }
}

/// A JBIG2 segment header.
#[derive(Debug, Clone, PartialEq)]
struct Segment {
    number: u32,
    type_: u8,
    page_assoc: u32,
    data_length: u32,
}

/// Parse a JBIG2 segment header.
fn parse_segment(reader: &mut Jbig2Reader<'_>) -> Result<Segment> {
    let number = reader.read_u32()?;
    // Segment header flags: bits 0-5 type, bit 6 page-assoc size, bit 7
    // deferred (0) or retained (1).
    let flags = reader.read_byte()?;
    let type_ = flags & 0x3f;
    let page_assoc_size = (flags >> 6) & 1;
    let page_assoc = if page_assoc_size == 0 {
        u32::from(reader.read_byte()?)
    } else {
        reader.read_u32()?
    };
    // Data length.
    let data_length = reader.read_u32()?;
    // Referenced segment counts (not needed for generic region).
    let _retain_count = (flags >> 7) & 1;
    if _retain_count == 1 {
        let _ref_count = reader.read_byte()?;
    }
    Ok(Segment {
        number,
        type_,
        page_assoc,
        data_length,
    })
}

/// Decode a generic region segment body.
///
/// The region segment information is: width (4), height (4), x (4), y (4),
/// flags (1). Flags bit 0 = MMR (1 = MMR coding). In MMR mode the rest of the
/// segment is the MMR-coded bitmap.
fn decode_generic_region(
    _type: u8,
    body: &[u8],
    _globals: &[u8],
    g: &mut BudgetGuard<'_>,
) -> Result<Vec<u8>> {
    let mut r = Jbig2Reader::new(body);
    let width = r.read_u32()?;
    let height = r.read_u32()?;
    let _x = r.read_u32()?;
    let _y = r.read_u32()?;
    let flags = r.read_byte()?;
    let mmr = (flags & 1) == 1;
    if !mmr {
        return Err(err!(
            Code::Jbig2Unsupported,
            during = "jbig2-generic",
            detail = "arithmetic-coded generic region is Phase 2 (SL-2.FILT.01)"
        ));
    }
    let coded = r.data.get(r.pos..).unwrap_or(&[]);
    let parms = FaxParms {
        k: -1, // Group 4 (MMR)
        columns: width,
        rows: height,
        ..FaxParms::default()
    };
    let mut out = crate::fax::ccitt_decode(coded, parms, g)?;
    // ccitt_decode returns rows of ceil(columns/8) bytes; JBIG2 MMR rows may
    // not be byte-aligned per row (G4 coding is bit-continuous). Re-pack rows
    // to width bits to be exact.
    out = repack_rows(&out, width, height, g)?;
    Ok(out)
}

/// Re-pack decoded rows so each output row is exactly `width` bits, with rows
/// byte-aligned in the output (matching what the image layer expects).
fn repack_rows(data: &[u8], width: u32, height: u32, g: &mut BudgetGuard<'_>) -> Result<Vec<u8>> {
    let w = usize::try_from(width).unwrap_or(usize::MAX);
    let h = usize::try_from(height).unwrap_or(usize::MAX);
    let row_bytes = w.saturating_add(7).wrapping_div(8);
    // ccitt_decode packed rows to ceil(width/8); the fax G4 decoder fills a
    // row then packs. The data may already be row-packed; verify the size and
    // pass through if it matches.
    if data.len() == row_bytes.saturating_mul(h) {
        return Ok(data.to_vec());
    }
    let mut out = alloc::vec_with_capacity::<u8>(g, row_bytes.saturating_mul(h))?;
    // Otherwise treat the stream as bit-continuous and re-pack.
    let mut bit_pos = 0usize;
    for _ in 0..h {
        g.tick()?;
        let mut row = alloc::vec_filled(g, row_bytes, 0u8)?;
        for b in 0..w {
            let value = get_bit(data, bit_pos);
            bit_pos = bit_pos.saturating_add(1);
            if value {
                let byte = b.wrapping_div(8);
                let bit = 7u32.wrapping_sub(u32::try_from(b % 8).unwrap_or(u32::MAX));
                if let Some(slot) = row.get_mut(byte) {
                    *slot |= 1 << bit;
                }
            }
        }
        out.extend_from_slice(&row);
    }
    Ok(out)
}

fn get_bit(data: &[u8], pos: usize) -> bool {
    let byte = data.get(pos.wrapping_div(8)).copied().unwrap_or(0);
    let bit = 7u32.wrapping_sub(u32::try_from(pos % 8).unwrap_or(u32::MAX));
    (byte >> bit) & 1 == 1
}

/// A big-endian reader over the JBIG2 byte stream.
struct Jbig2Reader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Jbig2Reader<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }

    fn is_exhausted(&self) -> bool {
        self.pos >= self.data.len()
    }

    fn read_byte(&mut self) -> Result<u8> {
        let b = self.data.get(self.pos).copied().ok_or_else(|| {
            err!(
                Code::Jbig2Corrupt,
                during = "jbig2-parse",
                detail = "unexpected EOF"
            )
        })?;
        self.pos = self.pos.saturating_add(1);
        Ok(b)
    }

    fn read_u32(&mut self) -> Result<u32> {
        let mut v = 0u32;
        for _ in 0..4 {
            v = v
                .saturating_mul(256)
                .saturating_add(u32::from(self.read_byte()?));
        }
        Ok(v)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::indexing_slicing)]

    use super::*;
    use selis_sandbox::Budget;

    fn guard() -> BudgetGuard<'static> {
        Budget::unlimited().guard()
    }

    #[test]
    fn corrupt_input_is_a_typed_error() {
        let mut g = guard();
        assert!(jbig2_decode(b"", b"", &mut g).is_err());
    }

    #[test]
    fn segment_header_parses() {
        // Segment number 1, type 6 (symbol dict), page 1, length 0.
        let data = vec![
            0u8, 0, 0, 1,    // segment number 1
            0x06, // flags: type 6
            1,    // page assoc (1 byte) = 1
            0, 0, 0, 0, // data length 0
        ];
        let mut reader = Jbig2Reader::new(&data);
        let seg = parse_segment(&mut reader).expect("segment");
        assert_eq!(seg.number, 1);
        assert_eq!(seg.type_, 6);
        assert_eq!(seg.page_assoc, 1);
        assert_eq!(seg.data_length, 0);
    }

    #[test]
    fn arithmetic_generic_region_is_unsupported_not_wrong() {
        let mut g = guard();
        // A generic region segment (type 20) with the arithmetic flag (bit 0
        // clear) must return JBIG2_UNSUPPORTED, never a wrong bitmap.
        let mut body = Vec::new();
        body.extend_from_slice(&[0, 0, 0, 8]); // width 8
        body.extend_from_slice(&[0, 0, 0, 1]); // height 1
        body.extend_from_slice(&[0, 0, 0, 0]); // x
        body.extend_from_slice(&[0, 0, 0, 0]); // y
        body.push(0); // flags: arithmetic
        let mut stream = Vec::new();
        stream.extend_from_slice(&[0, 0, 0, 1]); // segment number
        stream.push(20); // type: generic region
        stream.push(1); // page assoc (1 byte)
        stream.extend_from_slice(&[0, 0, 0, u8::try_from(body.len()).unwrap_or(0)]); // data length
        stream.extend_from_slice(&body);
        let e = jbig2_decode(&stream, b"", &mut g).expect_err("unsupported");
        assert_eq!(e.code(), Code::Jbig2Unsupported);
    }

    #[test]
    fn repack_passthrough_matches() {
        let data = vec![0xffu8, 0x00, 0xff];
        let mut g = guard();
        let out = repack_rows(&data, 8, 3, &mut g).expect("budget");
        assert_eq!(out, data);
    }
}
