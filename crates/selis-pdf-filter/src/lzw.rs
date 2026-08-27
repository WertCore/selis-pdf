//! LZWDecode (SL-1.FILT.03).
//!
//! PDF LZW uses an initial code size of 9 bits, clear code 256, EOD code 257.
//! Dictionary entries grow from 258; when the code size would exceed 12 bits,
//! behaviour depends on `/EarlyChange`:
//! * 0 (or absent) — keep using the current code size until the table is full.
//! * 1 — clear the table and reset to 9 bits as soon as the limit is reached.
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
    let mut bits: u64 = 0;
    let mut nbits = 0u32;
    let mut code_size = 9u32;
    let mut read_pos = 0usize;
    let mut prev: Option<u32> = None;

    loop {
        g.tick()?;
        // Fill the bit buffer.
        while nbits < 32 && read_pos < data.len() {
            let b = data.get(read_pos).copied().unwrap_or(0);
            bits |= (b as u64) << nbits;
            read_pos = read_pos.saturating_add(1);
            nbits = nbits.saturating_add(8);
        }
        if nbits < code_size {
            break; // clean end of stream
        }
        let mask = (1u64 << code_size).saturating_sub(1);
        let code = u32::try_from(bits & mask).unwrap_or(u32::MAX);
        bits >>= code_size;
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
                return Err(err!(Code::LzwCorrupt, during = "lzw-decode"));
            }
            e.clone()
        } else if code as usize == dict.len() {
            // The early-code-reuse case: the code refers to the entry being
            // added right now, which is prev + first(prev).
            let prev_entry = prev
                .and_then(|p| dict.get(p as usize))
                .ok_or_else(|| err!(Code::LzwCorrupt, during = "lzw-decode"))?;
            if prev_entry.is_empty() {
                return Err(err!(Code::LzwCorrupt, during = "lzw-decode"));
            }
            let mut synth = prev_entry.clone();
            let first = prev_entry.first().copied().unwrap_or(0);
            synth.push(first);
            synth
        } else {
            return Err(err!(Code::LzwCorrupt, during = "lzw-decode"));
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

        // Code size management.
        if dict.len() >= (1usize << code_size) && code_size < 12 {
            if early_change == 1 {
                dict.truncate(258);
                code_size = 9;
            } else {
                code_size = code_size.saturating_add(1);
            }
        }
    }

    Ok(out)
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

    /// A minimal LZW encoder matching PDF's parameters (9-bit start, clear,
    /// EOD). Used to round-trip the decoder.
    fn lzw_encode(data: &[u8]) -> Vec<u8> {
        pack_codes(&lzw_codes(data))
    }

    fn lzw_codes(data: &[u8]) -> Vec<u32> {
        let mut dict: std::collections::HashMap<Vec<u8>, u32> = std::collections::HashMap::new();
        for i in 0..256 {
            dict.insert(vec![i as u8], i as u32);
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
                codes.push(dict[&w]);
                if next_code < MAX_ENTRIES as u32 {
                    dict.insert(wc, next_code);
                    next_code = next_code.saturating_add(1);
                }
                w = vec![b];
            }
        }
        if !w.is_empty() {
            codes.push(dict[&w]);
        }
        codes.push(EOD);
        codes
    }

    /// Pack 9..12-bit codes big-endian-bit-wise (PDF LZW bit order).
    fn pack_codes(codes: &[u32]) -> Vec<u8> {
        let mut out = Vec::new();
        let mut bitbuf: u64 = 0;
        let mut nbits = 0u32;
        let mut code_size = 9u32;
        // The decoder's table: 0..255 literals + 256 CLEAR + 257 EOD = 258.
        let mut dict_len = 258usize;
        for &c in codes {
            bitbuf |= (u64::from(c)) << nbits;
            nbits += code_size;
            while nbits >= 8 {
                out.push((bitbuf & 0xff) as u8);
                bitbuf >>= 8;
                nbits -= 8;
            }
            // Simulate the decoder's table growth (clear resets).
            if c == CLEAR {
                dict_len = 258;
                code_size = 9;
            } else if c != EOD {
                dict_len += 1;
                if dict_len >= (1usize << code_size) && code_size < 12 {
                    code_size += 1;
                }
            }
        }
        if nbits > 0 {
            out.push((bitbuf & 0xff) as u8);
        }
        out
    }

    #[test]
    fn roundtrip_simple() {
        let data = b"TOBEORNOTTOBEORTOBEORNOT";
        let packed = lzw_encode(data);
        let mut g = guard();
        let decoded = lzw_decode(&packed, 0, &mut g).expect("decode");
        assert_eq!(decoded, data);
    }

    #[test]
    fn roundtrip_repetitive() {
        let data = b"AAAAABBBBBCCCCCAAAAABBBBB".to_vec();
        let packed = lzw_encode(&data);
        let mut g = guard();
        let decoded = lzw_decode(&packed, 0, &mut g).expect("decode");
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
        let packed = pack_codes(&codes);
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
