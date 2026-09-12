//! LZWDecode (SL-1.FILT.03).
//!
//! PDF LZW uses an initial code size of 9 bits, clear code 256, EOD code 257,
//! and packs codes **MSB-first** into the byte stream (ISO 32000-2 §7.4.6.2 —
//! a LSB-first reader decodes nothing but its own tests). Dictionary entries
//! grow from 258; the code width widens 9 → 12 bits as the table outgrows the
//! current width:
//! * `/EarlyChange` 0 — widen exactly when the table reaches 2^n entries;
//! * `/EarlyChange` 1 (the spec default) — widen one code early, at 2^n − 1.
//!
//! The early-code-reuse malformation: the encoder emits a code for the entry
//! it is *about* to add, and the decoder must synthesise it from the previous
//! entry (`prev + first(prev)`). This is the "KwKwK" case every LZW decoder
//! implements.

use selis_error::{err, Code, Result};
use selis_sandbox::{alloc, BudgetGuard};

const CLEAR: u32 = 256;
const EOD: u32 = 257;
const MAX_ENTRIES: usize = 4096; // 12-bit table

/// Decode an LZW-compressed stream.
///
/// # Budget
///
/// Charges `Objects` per emitted code and ticks, so a hostile stream of
/// infinite codes terminates.
///
/// # Malformed Input
///
/// `LZW_CORRUPT` when a code cannot be resolved.
pub fn lzw_decode(data: &[u8], early_change: u8, g: &mut BudgetGuard<'_>) -> Result<Vec<u8>> {
    // Dictionary: entries indexed by code. Codes 0..=255 are single bytes,
    // 256 = clear, 257 = EOD, 258+ grow.
    let mut dict: Vec<Vec<u8>> = alloc::vec_with_capacity(g, MAX_ENTRIES)?;
    for i in 0..256u32 {
        dict.push(vec![u8::try_from(i).unwrap_or(0)]);
    }
    dict.push(Vec::new()); // 256 clear (placeholder)
    dict.push(Vec::new()); // 257 EOD (placeholder)

    let mut out: Vec<u8> = Vec::new();
    // MSB-first bit stream: the next code ends at bit `nbits` of `bit_buf`.
    let mut bit_buf: u64 = 0;
    let mut nbits = 0u32;
    let mut code_size = 9u32;
    let mut read_pos = 0usize;
    let mut prev: Option<u32> = None;

    loop {
        g.tick()?;
        // Fill the bit buffer (keep at least one code + one byte of headroom).
        while nbits < 32 && read_pos < data.len() {
            let b = data.get(read_pos).copied().unwrap_or(0);
            bit_buf = bit_buf
                .checked_shl(8)
                .unwrap_or(0)
                .wrapping_add(u64::from(b));
            read_pos = read_pos.saturating_add(1);
            nbits = nbits.saturating_add(8);
        }
        if nbits < code_size {
            break; // clean end of stream
        }
        let mask = 1u64.checked_shl(code_size).unwrap_or(0).wrapping_sub(1);
        let code = u32::try_from(bit_buf.wrapping_shr(nbits.wrapping_sub(code_size)) & mask)
            .unwrap_or(u32::MAX);
        nbits = nbits.saturating_sub(code_size);

        if code == CLEAR {
            dict.truncate(258);
            code_size = 9;
            prev = None;
            continue;
        }
        if code == EOD {
            break;
        }
        g.charge_one(selis_sandbox::Resource::Objects)?;

        // Resolve the entry, synthesising the not-yet-added entry if needed.
        let entry: Vec<u8> = if let Some(e) = dict.get(code as usize) {
            if code >= 258 && e.is_empty() {
                // Entry slot exists but is empty — should not happen in a
                // well-formed stream; treat as corrupt.
                return Err(err!(
                    Code::LzwCorrupt,
                    during = "lzw-decode",
                    detail = format!("empty slot for code {code} (dict len {})", dict.len())
                ));
            }
            e.clone()
        } else if code as usize == dict.len() {
            // The early-code-reuse case: the code refers to the entry being
            // added right now, which is prev + first(prev).
            let prev_entry = prev.and_then(|p| dict.get(p as usize)).ok_or_else(|| {
                err!(
                    Code::LzwCorrupt,
                    during = "lzw-decode",
                    detail = format!("reuse code {code} with no previous code")
                )
            })?;
            if prev_entry.is_empty() {
                return Err(err!(
                    Code::LzwCorrupt,
                    during = "lzw-decode",
                    detail = format!("reuse code {code} after an empty previous entry")
                ));
            }
            let mut synth = prev_entry.clone();
            let first = prev_entry.first().copied().unwrap_or(0);
            synth.push(first);
            synth
        } else {
            return Err(err!(
                Code::LzwCorrupt,
                during = "lzw-decode",
                detail = format!(
                    "code {code} outside the table (dict len {}, width {code_size})",
                    dict.len()
                )
            ));
        };

        out.extend_from_slice(&entry);

        // Add the new dictionary entry: prev + first(entry).
        if let Some(p) = prev {
            if let Some(pe) = dict.get(p as usize) {
                let mut new_entry = pe.clone();
                new_entry.push(*entry.first().unwrap_or(&0));
                if dict.len() < MAX_ENTRIES {
                    dict.push(new_entry);
                }
            }
        }
        prev = Some(code);

        // Code size management (PDF §7.4.6.2): the code width grows as the
        // dictionary outgrows the current width, 9 → 12 bits. With
        // /EarlyChange 1 (the spec default) the width grows one code early —
        // at len == 2^n − 1; with 0, at len == 2^n. The dictionary is never
        // reset here: a mid-stream truncate garbled every real-world LZW
        // stream past its first width change (a silent blank page), and at
        // 12 bits a full dictionary simply stops growing until the encoder's
        // next CLEAR.
        if code_size < 12 {
            let base = 1usize.checked_shl(code_size).unwrap_or(usize::MAX);
            let threshold = if early_change == 1 {
                base.saturating_sub(1)
            } else {
                base
            };
            if dict.len() >= threshold {
                code_size = code_size.saturating_add(1);
            }
        }
    }

    Ok(out)
}

/// Encode `data` as an LZW stream (the inverse of [`lzw_decode`]).
///
/// A compact reference encoder mirroring the decoder's table growth exactly
/// (an entry per code except the first after a CLEAR; width widens at the
/// decoder's threshold). Used by the corpus generator to emit LZW-encoded
/// content streams and by the round-trip tests.
///
/// # Budget
///
/// The input is document data; callers charge it before encoding.
///
/// # Malformed Input
///
/// Encoding never fails (any byte string encodes); the output is always a
/// well-formed stream for [`lzw_decode`].
#[must_use]
pub fn lzw_encode(data: &[u8], early_change: u8) -> Vec<u8> {
    let mut dict: std::collections::HashMap<Vec<u8>, u32> = std::collections::HashMap::new();
    for i in 0..256u32 {
        dict.insert(
            vec![u8::try_from(i).unwrap_or(0)],
            u32::try_from(i).unwrap_or(0),
        );
    }
    let mut next_code: u32 = 258; // 256 = CLEAR, 257 = EOD are reserved
    let mut codes: Vec<u32> = vec![CLEAR];
    let mut w: Vec<u8> = Vec::new();
    for &b in data {
        let mut wc = w.clone();
        wc.push(b);
        if dict.contains_key(&wc) {
            w = wc;
        } else {
            if let Some(&c) = dict.get(&w) {
                codes.push(c);
            }
            if next_code < u32::try_from(MAX_ENTRIES).unwrap_or(u32::MAX) {
                dict.insert(wc, next_code);
                next_code = next_code.saturating_add(1);
            }
            w = vec![b];
        }
    }
    if !w.is_empty() {
        if let Some(&c) = dict.get(&w) {
            codes.push(c);
        }
    }
    codes.push(EOD);
    pack_codes(&codes, early_change)
}

/// Pack 9..12-bit codes MSB-first (PDF LZW bit order), mirroring the
/// decoder's table growth exactly: an entry is added for every code except
/// the first after a CLEAR, and the width grows at the decoder's threshold
/// (`early` selects the spec's two conventions).
fn pack_codes(codes: &[u32], early: u8) -> Vec<u8> {
    let mut out = Vec::new();
    let mut bit_buf: u64 = 0;
    let mut nbits = 0u32;
    let mut code_size = 9u32;
    // The decoder's table: 0..255 literals + 256 CLEAR + 257 EOD = 258.
    let mut dict_len = 258usize;
    let mut first_after_clear = true;
    for &c in codes {
        bit_buf = bit_buf
            .checked_shl(code_size)
            .unwrap_or(0)
            .wrapping_add(u64::from(c));
        nbits = nbits.saturating_add(code_size);
        while nbits >= 8 {
            let shift = nbits.wrapping_sub(8);
            out.push(
                #[allow(clippy::cast_possible_truncation)]
                {
                    (bit_buf.wrapping_shr(shift) & 0xff) as u8
                },
            );
            nbits = nbits.wrapping_sub(8);
        }
        if c == CLEAR {
            dict_len = 258;
            code_size = 9;
            first_after_clear = true;
        } else if c != EOD {
            if first_after_clear {
                first_after_clear = false;
            } else {
                dict_len = dict_len.saturating_add(1);
            }
            let base = 1usize.checked_shl(code_size).unwrap_or(usize::MAX);
            let threshold = if early == 1 {
                base.saturating_sub(1)
            } else {
                base
            };
            if dict_len >= threshold && code_size < 12 {
                code_size = code_size.saturating_add(1);
            }
        }
    }
    if nbits > 0 {
        out.push(
            #[allow(clippy::cast_possible_truncation)]
            {
                (bit_buf
                    .checked_shl(8u32.wrapping_sub(nbits.min(8)))
                    .unwrap_or(0)
                    & 0xff) as u8
            },
        );
    }
    out
}

#[cfg(test)]
mod tests {
    // Test-only helpers pack bits and index dictionaries with known inputs.
    #![allow(
        clippy::indexing_slicing,
        clippy::arithmetic_side_effects,
        clippy::cast_sign_loss,
        clippy::cast_possible_truncation
    )]

    use super::*;
    use selis_sandbox::Budget;

    fn guard() -> BudgetGuard<'static> {
        Budget::unlimited().guard()
    }

    #[test]
    fn roundtrip_simple() {
        let data = b"TOBEORNOTTOBEORTOBEORNOT";
        let packed = lzw_encode(data, 0);
        let mut g = guard();
        let decoded = lzw_decode(&packed, 0, &mut g).expect("decode");
        assert_eq!(decoded, data);
    }

    #[test]
    fn roundtrip_repetitive() {
        let data = b"AAAAABBBBBCCCCCAAAAABBBBB".to_vec();
        let packed = lzw_encode(&data, 0);
        let mut g = guard();
        let decoded = lzw_decode(&packed, 0, &mut g).expect("decode");
        assert_eq!(decoded, data);
    }

    /// A stream large enough to cross the 512-entry width change: the width
    /// must widen 9 → 10 → 11 → 12 without truncating the dictionary
    /// (SL-2.RAST.13: a mid-stream truncate garbled real-world LZW content
    /// and the page painted blank).
    #[test]
    fn roundtrip_crosses_the_width_change() {
        let mut data = Vec::new();
        // Highly compressible but long enough to grow the dictionary past
        // 512 entries many times over (each "XYZW" run adds entries).
        for i in 0..2000u32 {
            let b = (i % 251) as u8;
            data.extend_from_slice(&[b, b.wrapping_add(1), b, b, 0]);
        }
        let mut g = guard();
        for early in [0u8, 1] {
            let packed = lzw_encode(&data, early);
            let decoded =
                lzw_decode(&packed, early, &mut g).unwrap_or_else(|e| panic!("early={early}: {e}"));
            assert_eq!(decoded, data, "early={early}");
        }
    }

    /// The dictionary survives a width change without a reset: decoding a
    /// long stream never truncates entries back to the 258 base.
    #[test]
    fn width_grows_without_dictionary_reset() {
        let mut data = Vec::new();
        for i in 0..2000u32 {
            let b = (i % 251) as u8;
            data.extend_from_slice(&[b, b.wrapping_add(2), b]);
        }
        let mut g = guard();
        // early_change = 1: the spec default. The old behaviour truncated the
        // dictionary at 512 entries and re-emitted 9-bit codes, so the tail
        // decoded as garbage or LzwCorrupt.
        let packed = lzw_encode(&data, 1);
        let decoded = lzw_decode(&packed, 1, &mut g).expect("decode");
        assert_eq!(decoded, data);
    }

    #[test]
    fn clear_code_resets() {
        let mut data = Vec::new();
        data.extend_from_slice(b"AB");
        // Force a clear in the middle by re-encoding with an explicit clear.
        let mut codes = vec![CLEAR];
        // Encode "AB" manually: A=65, B=66.
        codes.push(65);
        codes.push(66);
        codes.push(CLEAR);
        codes.push(65);
        codes.push(EOD);
        let packed = pack_codes(&codes, 0);
        let mut g = guard();
        let decoded = lzw_decode(&packed, 0, &mut g).expect("decode");
        assert_eq!(decoded, b"ABA", "after clear, the next literal is 'A'");
    }

    #[test]
    fn corrupt_stream_is_a_typed_error() {
        let mut g = guard();
        // A stream that forces an out-of-range code.
        let bad = vec![0xff, 0xff, 0xff, 0xff];
        let r = lzw_decode(&bad, 0, &mut g);
        assert!(r.is_err());
        assert_eq!(r.expect_err("err").code(), Code::LzwCorrupt);
    }
}
