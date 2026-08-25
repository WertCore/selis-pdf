//! ASCIIHex, ASCII85, and RunLength decode (SL-1.FILT.04).
//!
//! The three simple ASCII-family filters:
//! * **ASCIIHexDecode** — hex digits, whitespace ignored, `>` terminator,
//!   odd nibble padded with 0.
//! * **ASCII85Decode** — base-85, `~>` terminator, `z` = four zero bytes,
//!   `!!` = all-space tuple (a real-world writer quirk).
//! * **RunLengthDecode** — runs of up to 128 bytes, or `length 0..=127` then
//!   `length-1` repeats of the next byte; `128` = end of data.

use selis_error::{err, Code, Result};
use selis_sandbox::BudgetGuard;

/// Decode an ASCIIHex stream.
///
/// # Budget
///
/// Ticks per byte; output is bounded by input.
///
/// # Malformed Input
///
/// `ASCII_CORRUPT` when a non-hex byte (not whitespace) appears.
pub fn ascii_hex_decode(data: &[u8], g: &mut BudgetGuard<'_>) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    let mut hi: Option<u8> = None;
    for &b in data {
        g.tick()?;
        match b {
            b'>' => break,
            b if matches!(b, b' ' | b'\t' | b'\n' | b'\r' | 0x0c) => continue,
            b => {
                let v = hex_val(b).ok_or_else(|| err!(Code::AsciiCorrupt, during = "ascii-hex"))?;
                if let Some(h) = hi.take() {
                    out.push((h << 4) | v);
                } else {
                    hi = Some(v);
                }
            }
        }
    }
    if let Some(h) = hi {
        out.push(h << 4); // odd nibble padded with 0
    }
    Ok(out)
}

fn hex_val(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b.wrapping_sub(b'0')),
        b'a'..=b'f' => Some(b.wrapping_sub(b'a').wrapping_add(10)),
        b'A'..=b'F' => Some(b.wrapping_sub(b'A').wrapping_add(10)),
        _ => None,
    }
}

/// Decode an ASCII85 stream.
///
/// # Budget
///
/// Ticks per tuple; output is bounded by input.
///
/// # Malformed Input
///
/// `ASCII_CORRUPT` on an invalid character or an unfinished final tuple.
pub fn ascii85_decode(data: &[u8], g: &mut BudgetGuard<'_>) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    let mut group: [u32; 5] = [0; 5];
    let mut count = 0usize;
    let mut i = 0usize;
    loop {
        g.tick()?;
        let Some(&b) = data.get(i) else {
            break;
        };
        i = i.saturating_add(1);
        match b {
            b'~' => break, // terminator
            b'z' if count == 0 => {
                out.extend_from_slice(&[0, 0, 0, 0]);
            }
            b if matches!(b, b' ' | b'\t' | b'\n' | b'\r' | 0x0c) => continue,
            b if b >= b'!' && b <= b'u' => {
                if let Some(slot) = group.get_mut(count) {
                    *slot = u32::from(b.wrapping_sub(b'!'));
                }
                count = count.saturating_add(1);
                if count == 5 {
                    write_group(&group, &mut out);
                    count = 0;
                }
            }
            _ => {
                return Err(err!(Code::AsciiCorrupt, during = "ascii85"));
            }
        }
    }
    // Flush a partial final group (n+1 chars -> n bytes).
    if count > 0 {
        let n = count.saturating_sub(1);
        let mut value: u64 = 0;
        for k in 0..5 {
            let d = if k < count {
                u64::from(group.get(k).copied().unwrap_or(0))
            } else {
                84 // pad with 'u' (the largest digit) per spec
            };
            value = value.saturating_mul(85).saturating_add(d);
        }
        for k in 0..n {
            let shift =
                24u32.saturating_sub((u32::try_from(k).unwrap_or(u32::MAX)).saturating_mul(8));
            out.push(((value >> shift) & 0xff) as u8);
        }
    }
    Ok(out)
}

fn write_group(group: &[u32; 5], out: &mut Vec<u8>) {
    let mut value: u64 = 0;
    for &d in group {
        value = value.saturating_mul(85).saturating_add(u64::from(d));
    }
    for shift in [24u32, 16, 8, 0] {
        out.push(((value >> shift) & 0xff) as u8);
    }
}

/// Decode a RunLength stream.
///
/// # Budget
///
/// Ticks per run; output is bounded by the run lengths.
///
/// # Malformed Input
///
/// `RUNLENGTH_CORRUPT` when a run overruns the input.
pub fn runlength_decode(data: &[u8], g: &mut BudgetGuard<'_>) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    let mut i = 0usize;
    loop {
        g.tick()?;
        let Some(&len) = data.get(i) else {
            return Err(err!(
                Code::RunlengthCorrupt,
                during = "runlength",
                detail = "no EOD marker"
            ));
        };
        i = i.saturating_add(1);
        match len {
            128 => break, // end of data
            0..=127 => {
                let count = usize::from(len) + 1;
                let chunk = data.get(i..i.saturating_add(count)).ok_or_else(|| {
                    err!(
                        Code::RunlengthCorrupt,
                        during = "runlength",
                        detail = "literal run overruns input"
                    )
                })?;
                out.extend_from_slice(chunk);
                i = i.saturating_add(count);
            }
            129..=255 => {
                // 257 - len repeats (len 129 => 128 repeats, len 255 => 2).
                let rep = usize::from(257u16.saturating_sub(u16::from(len)));
                let Some(&b) = data.get(i) else {
                    return Err(err!(
                        Code::RunlengthCorrupt,
                        during = "runlength",
                        detail = "repeat overruns input"
                    ));
                };
                i = i.saturating_add(1);
                for _ in 0..rep {
                    out.push(b);
                }
            }
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::arithmetic_side_effects, clippy::indexing_slicing)]

    use super::*;
    use selis_sandbox::Budget;

    fn guard() -> BudgetGuard<'static> {
        Budget::unlimited().guard()
    }

    #[test]
    fn ascii_hex_roundtrip() {
        let mut g = guard();
        let out = ascii_hex_decode(b"48656C6C6F", &mut g).expect("decode");
        assert_eq!(out, b"Hello");
        // Odd nibble padded.
        let out = ascii_hex_decode(b"abc", &mut g).expect("decode");
        assert_eq!(out, &[0xab, 0xc0]);
        // Whitespace and terminator.
        let out = ascii_hex_decode(b"48 65 >ignored", &mut g).expect("decode");
        assert_eq!(out, b"He");
    }

    #[test]
    fn ascii85_roundtrip() {
        let mut g = guard();
        let out = ascii85_decode(b"9jqo^BlbD-BleB1DJ+*+F(f,q", &mut g).expect("decode");
        assert_eq!(out, b"Man is distinguished");
        // 'z' expands to four zeros.
        let out = ascii85_decode(b"z~>", &mut g).expect("decode");
        assert_eq!(out, &[0, 0, 0, 0]);
    }

    #[test]
    fn runlength_roundtrip() {
        let mut g = guard();
        // Literal run of "AB" (length 1 => 2 bytes), then 5 'C's (253 => 4
        // repeats? no: 257-253=4... use 252 => 5 repeats), then EOD.
        let stream = [1u8, b'A', b'B', 252, b'C', 128];
        let out = runlength_decode(&stream, &mut g).expect("decode");
        assert_eq!(out, b"ABCCCCC");
    }

    #[test]
    fn corrupt_inputs_are_typed_errors() {
        let mut g = guard();
        assert!(ascii85_decode(b"x!", &mut g).is_err());
        // RunLength with no EOD.
        assert!(runlength_decode(&[0, b'A'], &mut g).is_err());
    }
}
