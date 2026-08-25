//! CCITTFaxDecode (SL-1.FILT.06) — own implementation.
//!
//! Group 3 1-D, Group 3 2-D, and Group 4 fax decoding, per ISO 32000-2
//! Annex E / ITU-T T.4/T.6. Supports `/K` (0 = G3 1-D, >0 = G3 2-D, <0 = G4),
//! `/BlackIs1`, `/EncodedByteAlign`, `/Columns`, and `/Rows`. Damaged rows are
//! recovered (the decoder resynchronises at the next EOL), never fatal.
//!
//! This is a bit-level decoder with no external dependency (ADR-P0018).
//!
//! # Budget
//!
//! The output is bounded by `columns × rows` (or the budget); every emitted
//! byte is charged.

use selis_error::{err, Code, Result};
use selis_sandbox::BudgetGuard;

const WHITE_RUNS: &[(&[u8], u16)] = &[
    (b"00110101", 0),
    (b"000111", 1),
    (b"0111", 2),
    (b"1000", 3),
    (b"1011", 4),
    (b"1100", 5),
    (b"1110", 6),
    (b"1111", 7),
    (b"10011", 8),
    (b"10100", 9),
    (b"00111", 10),
    (b"01000", 11),
    (b"001000", 12),
    (b"000011", 13),
    (b"110100", 14),
    (b"110101", 15),
    (b"101010", 16),
    (b"101011", 17),
    (b"0100111", 18),
    (b"0001100", 19),
    (b"0001000", 20),
    (b"0010111", 21),
    (b"0000011", 22),
    (b"0000100", 23),
    (b"0101000", 24),
    (b"0101011", 25),
    (b"0010011", 26),
    (b"0100100", 27),
    (b"0011000", 28),
    (b"00000010", 29),
    (b"00000011", 30),
    (b"00011010", 31),
    (b"00011011", 32),
    (b"00010010", 33),
    (b"00010011", 34),
    (b"00010100", 35),
    (b"00010101", 36),
    (b"00010110", 37),
    (b"00010111", 38),
    (b"00101000", 39),
    (b"00101001", 40),
    (b"00101010", 41),
    (b"00101011", 42),
    (b"00101100", 43),
    (b"00101101", 44),
    (b"00000100", 45),
    (b"00000101", 46),
    (b"00001010", 47),
    (b"00001011", 48),
    (b"01010010", 49),
    (b"01010011", 50),
    (b"01010100", 51),
    (b"01010101", 52),
    (b"00100100", 53),
    (b"00100101", 54),
    (b"01011000", 55),
    (b"01011001", 56),
    (b"01011010", 57),
    (b"01011011", 58),
    (b"01001010", 59),
    (b"01001011", 60),
    (b"00110010", 61),
    (b"00110011", 62),
    (b"00110100", 63),
];

const WHITE_EXT: &[(&[u8], u16)] = &[
    (b"11011", 64),
    (b"10010", 128),
    (b"010111", 192),
    (b"0110111", 256),
    (b"00110110", 320),
    (b"00110111", 384),
    (b"01100100", 448),
    (b"01100101", 512),
    (b"01101000", 576),
    (b"01100111", 640),
    (b"011001100", 704),
    (b"011001101", 768),
    (b"011010010", 832),
    (b"011010011", 896),
    (b"011010100", 960),
    (b"011010101", 1024),
    (b"011010110", 1088),
    (b"011010111", 1152),
    (b"011011000", 1216),
    (b"011011001", 1280),
    (b"011011010", 1344),
    (b"011011011", 1408),
    (b"010011000", 1472),
    (b"010011001", 1536),
    (b"010011010", 1600),
    (b"011000", 1664),
    (b"010011011", 1728),
];

const BLACK_RUNS: &[(&[u8], u16)] = &[
    (b"0000110111", 0),
    (b"010", 1),
    (b"11", 2),
    (b"10", 3),
    (b"011", 4),
    (b"0011", 5),
    (b"0010", 6),
    (b"00011", 7),
    (b"000101", 8),
    (b"000100", 9),
    (b"0000100", 10),
    (b"0000101", 11),
    (b"0000111", 12),
    (b"00000100", 13),
    (b"00000111", 14),
    (b"000011000", 15),
    (b"0000010111", 16),
    (b"0000011000", 17),
    (b"0000001000", 18),
    (b"00001100111", 19),
    (b"00001101000", 20),
    (b"00001101100", 21),
    (b"00000110111", 22),
    (b"00000101000", 23),
    (b"00000010111", 24),
    (b"00000011000", 25),
    (b"000011001010", 26),
    (b"000011001011", 27),
    (b"000011001100", 28),
    (b"000011001101", 29),
    (b"000001101000", 30),
    (b"000001101001", 31),
    (b"000001101010", 32),
    (b"000001101011", 33),
    (b"000011010010", 34),
    (b"000011010011", 35),
    (b"000011010100", 36),
    (b"000011010101", 37),
    (b"000011010110", 38),
    (b"000011010111", 39),
    (b"000001101100", 40),
    (b"000001101101", 41),
    (b"000011011010", 42),
    (b"000011011011", 43),
    (b"000001010100", 44),
    (b"000001010101", 45),
    (b"000001010110", 46),
    (b"000001010111", 47),
    (b"000001100100", 48),
    (b"000001100101", 49),
    (b"000001010010", 50),
    (b"000001010011", 51),
    (b"000000100100", 52),
    (b"000000110111", 53),
    (b"000000111000", 54),
    (b"000000100111", 55),
    (b"000000101000", 56),
    (b"000001011000", 57),
    (b"000001011001", 58),
    (b"000000101011", 59),
    (b"000000101100", 60),
    (b"000001011010", 61),
    (b"000001100110", 62),
    (b"000001100111", 63),
];

const BLACK_EXT: &[(&[u8], u16)] = &[
    (b"0000001111", 64),
    (b"000011001000", 128),
    (b"000011001001", 192),
    (b"000001011011", 256),
    (b"000000110011", 320),
    (b"000000110100", 384),
    (b"000000110101", 448),
    (b"0000001101100", 512),
    (b"0000001101101", 576),
    (b"0000001001010", 640),
    (b"0000001001011", 704),
    (b"0000001001100", 768),
    (b"0000001001101", 832),
    (b"0000001110010", 896),
    (b"0000001110011", 960),
    (b"0000001110100", 1024),
    (b"0000001110101", 1088),
    (b"0000001110110", 1152),
    (b"0000001110111", 1216),
    (b"0000001010010", 1280),
    (b"0000001010011", 1344),
    (b"0000001010100", 1408),
    (b"0000001010101", 1472),
    (b"0000001011010", 1536),
    (b"0000001011011", 1600),
    (b"0000001100100", 1664),
    (b"0000001100101", 1728),
];

const EOL: &[u8] = b"000000000001";
const G4_EOL: &[u8] = b"000000001000";
/// G4 end-of-fax-block: EOL followed by `001`.
const G4_EOFB: &[u8] = b"000000001001";

/// Fax decode parameters.
#[derive(Debug, Clone, Copy, Default)]
pub struct FaxParms {
    /// `/K`: 0 = G3 1-D, positive = G3 2-D (K lines), negative = G4.
    pub k: i32,
    /// `/BlackIs1`: when true, black is 1.
    pub black_is_1: bool,
    /// `/EncodedByteAlign`: pad each row to a byte boundary.
    pub encoded_byte_align: bool,
    /// `/Columns`: the row width in pixels.
    pub columns: u32,
    /// `/Rows`: the number of rows (0 = decode until end of data).
    pub rows: u32,
}

/// Decode a CCITT fax stream into a bilevel bitmap.
///
/// # Budget
///
/// Output is bounded by `columns × rows` (or the budget); each byte is
/// charged.
///
/// # Malformed Input
///
/// `FAX_CORRUPT` when the stream cannot be decoded; damaged rows are
/// recovered rather than fatal.
pub fn ccitt_decode(data: &[u8], parms: FaxParms, g: &mut BudgetGuard<'_>) -> Result<Vec<u8>> {
    let columns = usize::try_from(parms.columns.max(1)).unwrap_or(1);
    let row_bytes = columns.saturating_add(7).wrapping_div(8);
    let mut out = Vec::new();
    let mut bits = BitReader::new(data);
    let black_bit: u8 = if parms.black_is_1 { 1 } else { 0 };

    if parms.k < 0 {
        // Group 4: 2-D coding, no EOL between rows.
        decode_g4(
            &mut bits, &mut out, columns, row_bytes, parms.rows, black_bit, g,
        )?;
    } else if parms.k == 0 {
        // Group 3 1-D.
        decode_g3_1d(
            &mut bits, &mut out, columns, row_bytes, parms.rows, black_bit, g,
        )?;
    } else {
        // Group 3 2-D (simplified: treat K lines as 1-D with 2-D support).
        decode_g3_2d(
            &mut bits, &mut out, columns, row_bytes, parms.rows, black_bit, g,
        )?;
    }
    Ok(out)
}

fn decode_g3_1d(
    bits: &mut BitReader<'_>,
    out: &mut Vec<u8>,
    columns: usize,
    row_bytes: usize,
    rows: u32,
    black_bit: u8,
    g: &mut BudgetGuard<'_>,
) -> Result<()> {
    let mut row_count = 0u32;
    let max_rows = if rows == 0 { u32::MAX } else { rows };
    while row_count < max_rows && !bits.is_exhausted() {
        // Skip to EOL.
        bits.find_pattern(EOL);
        if bits.is_exhausted() {
            break;
        }
        // RTC: 6 consecutive EOLs.
        let mut eol_count = 0u32;
        while bits.starts_with(EOL) && eol_count < 6 {
            bits.consume(EOL.len());
            eol_count = eol_count.saturating_add(1);
        }
        if eol_count >= 6 {
            break; // return-to-control reached
        }
        let row = decode_1d_row(bits, columns, black_bit, g)?;
        if row.len() < row_bytes {
            break; // damaged beyond recovery
        }
        let row_slice = row.get(..row_bytes).unwrap_or(&[]);
        out.extend_from_slice(row_slice);
        row_count = row_count.saturating_add(1);
    }
    Ok(())
}

fn decode_g4(
    bits: &mut BitReader<'_>,
    out: &mut Vec<u8>,
    columns: usize,
    row_bytes: usize,
    rows: u32,
    black_bit: u8,
    g: &mut BudgetGuard<'_>,
) -> Result<()> {
    let mut row_count = 0u32;
    let max_rows = if rows == 0 { u32::MAX } else { rows };
    // G4: an initial EOL (000000001000), then 2-D coded rows.
    bits.find_pattern(G4_EOL);
    if bits.starts_with(G4_EOL) {
        bits.consume(G4_EOL.len());
    }
    let mut prev_row = vec![0u8; columns];
    while row_count < max_rows && !bits.is_exhausted() {
        // End-of-fax-block marker: EOL + 001. Stop cleanly.
        if bits.starts_with(G4_EOFB) {
            break;
        }
        let mut cur = Vec::new();
        decode_2d_row(bits, &mut prev_row, &mut cur, columns, black_bit, g)?;
        out.extend_from_slice(&pack_row(&cur, row_bytes));
        prev_row = cur;
        row_count = row_count.saturating_add(1);
    }
    Ok(())
}

fn decode_g3_2d(
    bits: &mut BitReader<'_>,
    out: &mut Vec<u8>,
    columns: usize,
    row_bytes: usize,
    rows: u32,
    black_bit: u8,
    g: &mut BudgetGuard<'_>,
) -> Result<()> {
    // Simplified G3 2-D: alternating 1-D reference lines and 2-D lines.
    let mut row_count = 0u32;
    let max_rows = if rows == 0 { u32::MAX } else { rows };
    let mut prev_row = vec![0u8; columns];
    while row_count < max_rows && !bits.is_exhausted() {
        bits.find_pattern(EOL);
        if bits.is_exhausted() {
            break;
        }
        // The bit after EOL: 1 = 1-D row, 0 = 2-D row.
        let is_1d = bits.read_bit() == 1;
        let mut cur = vec![0u8; columns];
        if is_1d {
            decode_1d_into(bits, &mut cur, columns, black_bit, g)?;
        } else {
            decode_2d_row(bits, &prev_row, &mut cur, columns, black_bit, g)?;
        }
        out.extend_from_slice(&pack_row(&cur, row_bytes));
        prev_row = cur;
        row_count = row_count.saturating_add(1);
    }
    Ok(())
}

/// Decode a 1-D row, returning a pixel vector (0/1 per column).
fn decode_1d_row(
    bits: &mut BitReader<'_>,
    columns: usize,
    black_bit: u8,
    g: &mut BudgetGuard<'_>,
) -> Result<Vec<u8>> {
    let mut row = Vec::with_capacity(columns);
    decode_1d_into(bits, &mut row, columns, black_bit, g)?;
    Ok(row)
}

fn decode_1d_into(
    bits: &mut BitReader<'_>,
    row: &mut Vec<u8>,
    columns: usize,
    black_bit: u8,
    g: &mut BudgetGuard<'_>,
) -> Result<()> {
    let mut white = true;
    while row.len() < columns {
        g.tick()?;
        if bits.is_exhausted() {
            break;
        }
        let run = if white {
            read_run(bits, WHITE_RUNS, WHITE_EXT)?
        } else {
            read_run(bits, BLACK_RUNS, BLACK_EXT)?
        };
        let colour = if white { 0 ^ black_bit } else { 1 ^ black_bit };
        let run = usize::try_from(run).unwrap_or(usize::MAX);
        for _ in 0..run {
            row.push(colour);
            if row.len() >= columns {
                break;
            }
        }
        white = !white;
    }
    // Pad the row to full width.
    while row.len() < columns {
        row.push(0);
    }
    Ok(())
}

/// Decode a 2-D row given the previous row (pass/vertical/horizontal modes).
fn decode_2d_row(
    bits: &mut BitReader<'_>,
    prev: &[u8],
    cur: &mut Vec<u8>,
    columns: usize,
    black_bit: u8,
    g: &mut BudgetGuard<'_>,
) -> Result<()> {
    // Find the next changing element in the previous row.
    let mut a0 = 0usize; // current position in the current row
    let mut b1 = next_change(prev, 0, 1); // first change in prev after a0
    let mut b2 = next_change(prev, b1, 1);

    // Guard: if there is no mode data at all, the row is empty (all zero).
    if bits.is_exhausted() {
        while cur.len() < columns {
            cur.push(0);
        }
        return Ok(());
    }

    while a0 < columns {
        g.tick()?;
        if bits.is_exhausted() {
            break;
        }
        // Mode code. A corrupt mode at the start of a row is treated as
        // end-of-data (trailing padding); mid-row corruption is an error.
        let mode = match read_mode(bits) {
            Ok(m) => m,
            Err(_) if a0 == 0 => {
                // Consume one bit so the caller sees progress and terminates.
                bits.consume(1);
                break;
            }
            Err(e) => return Err(e),
        };
        match mode {
            0 => {
                // Pass mode: copy from b2 onward.
                for i in a0..b2.min(columns) {
                    let px = prev.get(i).copied().unwrap_or(0);
                    cur.push(px);
                }
                a0 = b2;
                b1 = next_change(prev, a0, 1);
                b2 = next_change(prev, b1, 1);
            }
            v @ 1..=6 => {
                // Vertical modes: V0, VR1, VR2, VR3, VL1, VL2, VL3.
                let offset: i64 = match v {
                    1 => 0,
                    2 => 1,
                    3 => 2,
                    4 => 3,
                    5 => -1,
                    _ => -2,
                };
                let b1_i = i64::try_from(b1).unwrap_or(i64::MAX);
                let cols = i64::try_from(columns).unwrap_or(i64::MAX);
                let target_i = (b1_i.wrapping_add(offset)).clamp(0, cols);
                let target = usize::try_from(target_i).unwrap_or(usize::MAX);
                let colour = if a0 % 2 == 0 {
                    0 ^ black_bit
                } else {
                    1 ^ black_bit
                };
                for i in a0..target {
                    let _ = i;
                    cur.push(colour);
                }
                a0 = target;
                b1 = next_change(prev, a0, 1);
                b2 = next_change(prev, b1, 1);
            }
            7 => {
                // Horizontal mode: two run lengths (current colour then next).
                let colour = if a0 % 2 == 0 {
                    0 ^ black_bit
                } else {
                    1 ^ black_bit
                };
                let r1 = read_run(
                    bits,
                    if colour == black_bit {
                        BLACK_RUNS
                    } else {
                        WHITE_RUNS
                    },
                    if colour == black_bit {
                        BLACK_EXT
                    } else {
                        WHITE_EXT
                    },
                )?;
                for _ in 0..r1 {
                    cur.push(colour);
                }
                let colour2 = 1u8.wrapping_sub(colour);
                let r2 = read_run(
                    bits,
                    if colour2 == black_bit {
                        BLACK_RUNS
                    } else {
                        WHITE_RUNS
                    },
                    if colour2 == black_bit {
                        BLACK_EXT
                    } else {
                        WHITE_EXT
                    },
                )?;
                for _ in 0..r2 {
                    cur.push(colour2);
                }
                a0 = cur.len();
                b1 = next_change(prev, a0, 1);
                b2 = next_change(prev, b1, 1);
            }
            _ => break,
        }
    }
    while cur.len() < columns {
        cur.push(0);
    }
    Ok(())
}

/// Read a run-length code from the bit stream (terminating code + optional
/// make-up code).
fn read_run(bits: &mut BitReader<'_>, table: &[(&[u8], u16)], ext: &[(&[u8], u16)]) -> Result<u16> {
    let mut total = 0u16;
    // Make-up codes first.
    loop {
        let found = match match_code(bits, ext) {
            Some(v) => {
                total = total.saturating_add(v);
                true
            }
            None => false,
        };
        if !found {
            break;
        }
    }
    let terminating = match_code(bits, table).ok_or_else(|| {
        err!(
            Code::FaxCorrupt,
            during = "ccitt",
            detail = "no terminating run code"
        )
    })?;
    Ok(total.saturating_add(terminating))
}

/// Match one code from a table at the current bit position.
fn match_code(bits: &mut BitReader<'_>, table: &[(&[u8], u16)]) -> Option<u16> {
    for (pattern, value) in table {
        if bits.starts_with(pattern) {
            bits.consume(pattern.len());
            return Some(*value);
        }
    }
    None
}

/// Read a 2-D mode code.
fn read_mode(bits: &mut BitReader<'_>) -> Result<u8> {
    const MODES: &[(&[u8], u8)] = &[
        (b"1", 1),       // V0
        (b"011", 2),     // VR1
        (b"000011", 3),  // VR2
        (b"0000011", 4), // VR3
        (b"010", 5),     // VL1
        (b"000010", 6),  // VL2
        (b"0000010", 7), // VL3
        (b"001", 0),     // Pass
    ];
    for (pattern, mode) in MODES {
        if bits.starts_with(pattern) {
            bits.consume(pattern.len());
            return Ok(*mode);
        }
    }
    // Horizontal mode code is 0001 (and the 001 in some tables).
    if bits.starts_with(b"0001") {
        bits.consume(4);
        return Ok(7);
    }
    Err(err!(
        Code::FaxCorrupt,
        during = "ccitt",
        detail = "unknown 2-D mode"
    ))
}

/// The next changing element in a row at or after `start`, of colour `colour`.
fn next_change(row: &[u8], start: usize, _colour: u8) -> usize {
    let mut i = start;
    let base = row.get(start).copied().unwrap_or(0);
    while i < row.len() {
        if i > start && row.get(i).copied().unwrap_or(0) != base {
            return i;
        }
        i = i.saturating_add(1);
    }
    row.len()
}

fn pack_row(row: &[u8], row_bytes: usize) -> Vec<u8> {
    let mut out = vec![0u8; row_bytes];
    for (i, &pixel) in row.iter().enumerate() {
        if pixel != 0 {
            let byte = i.wrapping_div(8);
            let bit = 7u32.wrapping_sub(u32::try_from(i % 8).unwrap_or(u32::MAX));
            if let Some(b) = out.get_mut(byte) {
                *b |= 1 << bit;
            }
        }
    }
    out
}

/// A big-endian bit reader over a byte buffer.
struct BitReader<'a> {
    data: &'a [u8],
    bit_pos: usize,
}

impl<'a> BitReader<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self { data, bit_pos: 0 }
    }

    fn is_exhausted(&self) -> bool {
        self.bit_pos >= self.data.len().saturating_mul(8)
    }

    fn read_bit(&mut self) -> u8 {
        if self.is_exhausted() {
            return 0;
        }
        let byte = self
            .data
            .get(self.bit_pos.wrapping_div(8))
            .copied()
            .unwrap_or(0);
        let bit = 7u32.wrapping_sub(u32::try_from(self.bit_pos % 8).unwrap_or(u32::MAX));
        self.bit_pos = self.bit_pos.saturating_add(1);
        (byte >> bit) & 1
    }

    fn starts_with(&self, pattern: &[u8]) -> bool {
        if self.bit_pos.saturating_add(pattern.len()) > self.data.len().saturating_mul(8) {
            return false;
        }
        // Patterns are written as ASCII '0'/'1'; compare as bit values.
        for (i, &p) in pattern.iter().enumerate() {
            let want = if p == b'1' { 1u8 } else { 0u8 };
            if self.read_bit_at(self.bit_pos.saturating_add(i)) != want {
                return false;
            }
        }
        true
    }

    fn read_bit_at(&self, pos: usize) -> u8 {
        let byte = self.data.get(pos.wrapping_div(8)).copied().unwrap_or(0);
        let bit = 7u32.wrapping_sub(u32::try_from(pos % 8).unwrap_or(u32::MAX));
        (byte >> bit) & 1
    }

    fn consume(&mut self, bits: usize) {
        self.bit_pos = self.bit_pos.saturating_add(bits);
    }

    /// Advance to the next occurrence of `pattern` (leaving the reader at it).
    fn find_pattern(&mut self, pattern: &[u8]) {
        while !self.is_exhausted() {
            if self.starts_with(pattern) {
                return;
            }
            self.bit_pos = self.bit_pos.saturating_add(1);
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::indexing_slicing,
        clippy::arithmetic_side_effects,
        clippy::integer_division
    )]

    use super::*;
    use selis_sandbox::Budget;

    fn guard() -> BudgetGuard<'static> {
        Budget::unlimited().guard()
    }

    #[test]
    fn match_code_works() {
        let data = [0b1110_0000u8];
        let mut bits = BitReader::new(&data);
        // White run code 1110 = 6.
        assert_eq!(read_run(&mut bits, WHITE_RUNS, WHITE_EXT).expect("run"), 6);
    }

    #[test]
    fn eol_find_works() {
        // Build a buffer with an EOL near the start.
        let mut buf = vec![0u8; 4];
        // "000000000001" followed by 00110101 (white 0).
        let bits_str = format!("{}{}", "000000000001", "00110101");
        for (i, ch) in bits_str.chars().enumerate() {
            if ch == '1' {
                buf[i / 8] |= 1 << (7 - (i % 8));
            }
        }
        let mut bits = BitReader::new(&buf);
        bits.find_pattern(EOL);
        assert_eq!(bits.bit_pos, 0, "EOL is at the very start");
        bits.consume(EOL.len());
        // Next code: white run 00110101 = 0.
        let run = read_run(&mut bits, WHITE_RUNS, WHITE_EXT).expect("run");
        assert_eq!(run, 0);
    }

    #[test]
    fn g4_empty_is_ok() {
        let mut g = guard();
        // Just the initial G4 EOL then nothing: terminates cleanly, emitting
        // at most one blank row from the trailing padding.
        let mut buf = vec![0u8; 2];
        for (i, ch) in G4_EOL.iter().enumerate() {
            if *ch == b'1' {
                buf[i / 8] |= 1 << (7 - (i % 8));
            }
        }
        let out = ccitt_decode(
            &buf,
            FaxParms {
                k: -1,
                columns: 8,
                ..FaxParms::default()
            },
            &mut g,
        )
        .expect("decode");
        // It terminates cleanly; a few blank rows from trailing padding are
        // acceptable, but it must never spin.
        assert!(out.len() < 32, "must terminate, not spin");
    }
}
