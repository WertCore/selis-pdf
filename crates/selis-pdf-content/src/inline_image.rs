//! Inline images (`BI`/`ID`/`EI`) (SL-2.CONT.05).
//!
//! Inline images are embedded directly in the content stream between `BI` and
//! `EI` markers. The notorious `EI` detection problem: binary image data may
//! contain the bytes `EI` (0x45 0x49), which the lexer would otherwise
//! interpret as the end marker. The strategy:
//! * if the image dictionary has `/L` (length in bytes), use it as a hard
//!   bound — read exactly `L` bytes after `ID`;
//! * otherwise, scan for the next `EI` with a heuristic validation: the
//!   `EI` must be followed by whitespace or end-of-stream (the "EI" inside
//!   binary data is almost never followed by whitespace).

use selis_bytes::Bytes;

/// A decoded inline image: its dictionary and raw data.
#[derive(Debug, Clone, PartialEq)]
pub struct InlineImage {
    /// The image dictionary (`/W`, `/H`, `/CS`, `/BPC`, …).
    pub dict: Vec<(Bytes, Bytes)>,
    /// The raw image data (bytes between `ID` and `EI`).
    pub data: Vec<u8>,
}

/// Extract the next inline image from the content stream, starting just after
/// `BI` has been consumed.
///
/// Returns `(inline_image, new_position)`.
///
/// # Budget
///
/// The output is bounded by `/L` when present; otherwise by the heuristic
/// scan length.
///
/// # Malformed Input
///
/// Returns `None` when no valid inline image can be found (the heuristic
/// fails, or the stream ends).
pub fn extract_inline_image(src: &[u8], pos: usize) -> Option<(InlineImage, usize)> {
    // 1. Read the dictionary (name-value pairs until `ID`).
    let mut dict = Vec::new();
    let mut p = pos;
    loop {
        // Skip whitespace.
        while let Some(&b) = src.get(p) {
            if matches!(b, b' ' | b'\t' | b'\n' | b'\r' | 0x0c) {
                p = p.saturating_add(1);
            } else {
                break;
            }
        }
        // Check for `ID` marker.
        if src.get(p..p.saturating_add(2)) == Some(b"ID") {
            p = p.saturating_add(2);
            // Skip the single whitespace after `ID` (required by spec).
            if src
                .get(p)
                .is_some_and(|b| matches!(b, b' ' | b'\t' | b'\n' | b'\r'))
            {
                p = p.saturating_add(1);
            }
            break;
        }
        // Read a name (`/Name`).
        if src.get(p) != Some(&b'/') {
            return None; // expected a name
        }
        p = p.saturating_add(1);
        let mut name = Vec::new();
        while let Some(&b) = src.get(p) {
            if matches!(b, b' ' | b'\t' | b'\n' | b'\r' | 0x0c) {
                break;
            }
            name.push(b);
            p = p.saturating_add(1);
        }
        // Read a value (a number or a name).
        // Skip whitespace.
        while let Some(&b) = src.get(p) {
            if matches!(b, b' ' | b'\t' | b'\n' | b'\r' | 0x0c) {
                p = p.saturating_add(1);
            } else {
                break;
            }
        }
        let mut value = Vec::new();
        if src.get(p) == Some(&b'/') {
            value.push(b'/');
            p = p.saturating_add(1);
        }
        while let Some(&b) = src.get(p) {
            if matches!(b, b' ' | b'\t' | b'\n' | b'\r' | 0x0c) {
                break;
            }
            value.push(b);
            p = p.saturating_add(1);
        }
        dict.push((
            Bytes::copy_from_slice(&name),
            Bytes::copy_from_slice(&value),
        ));
    }

    // 2. Determine the data length.
    let length = find_length(&dict);
    let data = if let Some(l) = length {
        // Length-driven: read exactly `l` bytes after `ID`.
        let end = p.saturating_add(l);
        let data = src.get(p..end)?.to_vec();
        p = end;
        data
    } else {
        // Heuristic: scan for `EI` followed by whitespace or end.
        let mut scan = p;
        let mut best = None;
        while scan.saturating_add(2) <= src.len() {
            if src.get(scan..scan.saturating_add(2)) == Some(b"EI") {
                let after = scan.saturating_add(2);
                if after >= src.len()
                    || src.get(after).is_some_and(|b| {
                        matches!(b, b' ' | b'\t' | b'\n' | b'\r' | b'>' | b'/' | b'%')
                    })
                {
                    best = Some(scan);
                    // The first valid `EI` is the best guess.
                    break;
                }
            }
            scan = scan.saturating_add(1);
        }
        let ei = best?;
        // The data runs to `ei`; strip any trailing whitespace that is the
        // separator before `EI`, not image data.
        let mut data_end = ei;
        while data_end > p {
            let b = src.get(data_end.wrapping_sub(1)).copied().unwrap_or(0);
            if matches!(b, b' ' | b'\t' | b'\n' | b'\r' | 0x0c) {
                data_end = data_end.saturating_sub(1);
            } else {
                break;
            }
        }
        let data = src.get(p..data_end)?.to_vec();
        p = ei.saturating_add(2);
        data
    };

    // Skip whitespace after `EI`.
    while let Some(&b) = src.get(p) {
        if matches!(b, b' ' | b'\t' | b'\n' | b'\r' | 0x0c) {
            p = p.saturating_add(1);
        } else {
            break;
        }
    }

    Some((InlineImage { dict, data }, p))
}

/// Look up `/L` in the inline image dictionary.
fn find_length(dict: &[(Bytes, Bytes)]) -> Option<usize> {
    for (k, v) in dict {
        if k.as_slice() == b"L" {
            let s = String::from_utf8_lossy(v.as_slice());
            let n: usize = s.parse().ok()?;
            return Some(n);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    #![allow(clippy::indexing_slicing, clippy::arithmetic_side_effects)]

    use super::*;

    #[test]
    fn length_driven_extraction() {
        // /W 2 /H 2 /BPC 1 /L 4 ID \x00\xff\x00\xff EI
        let src = b"/W 2 /H 2 /BPC 1 /L 4 ID \x00\xff\x00\xff EI";
        let (img, pos) = extract_inline_image(src, 0).expect("extract");
        assert_eq!(img.data.len(), 4);
        assert_eq!(img.data, vec![0x00, 0xff, 0x00, 0xff]);
        // `EI` is consumed (pos points past it).
        assert!(pos > 0);
    }

    #[test]
    fn heuristic_scan_finds_ei() {
        // /W 1 /H 1 /BPC 1 ID \x00\x01 EI
        let src = b"/W 1 /H 1 /BPC 1 ID \x00\x01 EI";
        let (img, _pos) = extract_inline_image(src, 0).expect("extract");
        assert_eq!(img.data.len(), 2);
        assert_eq!(img.data, vec![0x00, 0x01]);
    }

    #[test]
    fn binary_data_containing_ei_bytes() {
        // /W 2 /H 1 /BPC 8 /L 6 ID \x45\x49\x00\xff\x45\x49 EI
        // The binary data contains `EI` (0x45 0x49) at positions 0 and 4.
        // With /L present, the length-driven path reads exactly 6 bytes.
        let src = b"/W 2 /H 1 /BPC 8 /L 6 ID \x45\x49\x00\xff\x45\x49 EI";
        let (img, _pos) = extract_inline_image(src, 0).expect("extract");
        assert_eq!(img.data.len(), 6);
        // All 6 bytes including the `EI`-looking bytes in the middle.
        assert_eq!(img.data[0], 0x45);
        assert_eq!(img.data[1], 0x49);
    }

    #[test]
    fn missing_id_returns_none() {
        let result = extract_inline_image(b"/W 1", 0);
        assert!(result.is_none());
    }
}
