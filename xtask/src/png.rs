//! Minimal PNG decode for oracle render outputs (SL-2.CONF.01).
//!
//! The oracle drivers (docker/oracles/) are the only producers: the PDFium
//! driver emits stored (uncompressed) zlib blocks, pdf.js emits standard
//! compressed IDAT — both are plain zlib streams, so the shared flate filter
//! inflates them (`selis_pdf_filter::flate`, the same decoder the engine uses
//! for FlateDecode). Interlaced / 16-bit / palette PNGs are refused: no
//! oracle driver produces them, and a refusal is a typed outcome, not a wrong
//! verdict. This is harness-side decode of *our own oracle tooling's* output —
//! never a Selis parse path.

/// The decoded image: 8-bit RGB, row-major, no alpha.
pub(crate) struct DecodedPng {
    pub width: u32,
    pub height: u32,
    /// `width * height * 3` RGB bytes.
    pub rgb: Vec<u8>,
}

/// The comparison pixel cap mirrored from the sweep (`sweep.rs`); a PNG above
/// it is refused before allocation.
const MAX_DECODE_PIXELS: u64 = 40_000_000;

/// Parse PNG dimensions from the IHDR header bytes.
pub(crate) fn dimensions(data: &[u8]) -> Result<(u32, u32), String> {
    const SIG: &[u8] = b"\x89PNG\r\n\x1a\n";
    if data.len() < 33 || !data.starts_with(SIG) {
        return Err("png: bad signature or short header".to_string());
    }
    if &data[12..16] != b"IHDR" {
        return Err("png: first chunk is not IHDR".to_string());
    }
    Ok((be32(&data[16..20]), be32(&data[20..24])))
}

fn be32(b: &[u8]) -> u32 {
    u32::from(b[0]) << 24 | u32::from(b[1]) << 16 | u32::from(b[2]) << 8 | u32::from(b[3])
}

/// Decode an 8-bit truecolour (RGB or RGBA, non-interlaced) PNG into RGB.
pub(crate) fn decode(data: &[u8]) -> Result<DecodedPng, String> {
    let (w, h) = dimensions(data)?;
    if w == 0 || h == 0 {
        return Err("png: empty canvas".to_string());
    }
    if u64::from(w) * u64::from(h) > MAX_DECODE_PIXELS {
        return Err(format!("png: {w}x{h} exceeds the decode cap"));
    }
    let w_us = usize::try_from(w).unwrap_or(0);
    let h_us = usize::try_from(h).unwrap_or(0);

    // Walk chunks: IHDR fields, concatenated IDAT.
    let mut pos = 8usize;
    let mut depth = 0u8;
    let mut colour = 0u8;
    let mut interlace = 0u8;
    let mut idat: Vec<u8> = Vec::new();
    while pos + 8 <= data.len() {
        let len = be32(&data[pos..pos + 4]);
        let Some(kind) = data.get(pos + 4..pos + 8) else {
            break;
        };
        let body_start = pos + 8;
        let body_end = body_start
            .checked_add(usize::try_from(len).unwrap_or(usize::MAX))
            .ok_or("png: chunk overflow")?;
        if body_end > data.len() {
            return Err("png: truncated chunk".to_string());
        }
        let body = &data[body_start..body_end];
        match kind {
            b"IHDR" => {
                let [.., d, c, _, _, i] = body else {
                    return Err("png: short IHDR".to_string());
                };
                depth = *d;
                colour = *c;
                interlace = *i;
            }
            b"IDAT" => idat.extend_from_slice(body),
            b"IEND" => break,
            _ => {}
        }
        pos = body_end.checked_add(4).ok_or("png: chunk overflow")?;
    }
    if depth != 8 || (colour != 2 && colour != 6) {
        return Err(format!("png: unsupported depth/colour {depth}/{colour}"));
    }
    if interlace != 0 {
        return Err("png: interlaced not supported".to_string());
    }
    let channels = if colour == 6 { 4usize } else { 3usize };
    let raw_len = h_us
        .saturating_mul(w_us.saturating_mul(channels))
        .saturating_add(h_us);
    let inflated = flate_idat(&idat, raw_len)?;
    if inflated.len() < raw_len {
        return Err("png: truncated image data".to_string());
    }
    // Unfilter (PNG 1.2 §6.3), then drop alpha.
    let stride = w_us.saturating_mul(channels);
    let mut out = vec![0u8; w_us.saturating_mul(h_us).saturating_mul(3)];
    let mut prev = vec![0u8; stride];
    for y in 0..h_us {
        let row_start = y * (stride + 1);
        let filter = inflated[row_start];
        let Some(row) = inflated.get(row_start + 1..row_start + 1 + stride) else {
            return Err("png: truncated row".to_string());
        };
        let mut cur = vec![0u8; stride];
        for x in 0..stride {
            let a = if x >= channels { cur[x - channels] } else { 0 };
            let b = prev[x];
            let c = if x >= channels { prev[x - channels] } else { 0 };
            let v = match filter {
                0 => i32::from(row[x]),
                1 => i32::from(row[x]).saturating_add(i32::from(a)),
                2 => i32::from(row[x]).saturating_add(i32::from(b)),
                3 => {
                    i32::from(row[x]).saturating_add(i32::from(a).saturating_add(i32::from(b)) >> 1)
                }
                4 => {
                    let p = i32::from(a)
                        .saturating_add(i32::from(b))
                        .saturating_sub(i32::from(c));
                    let pa = p
                        .saturating_sub(i32::from(a))
                        .abs()
                        .max(i32::from(a).saturating_sub(p));
                    let pb = p
                        .saturating_sub(i32::from(b))
                        .abs()
                        .max(i32::from(b).saturating_sub(p));
                    let pc = p
                        .saturating_sub(i32::from(c))
                        .abs()
                        .max(i32::from(c).saturating_sub(p));
                    i32::from(row[x]).saturating_add(if pa <= pb && pa <= pc {
                        i32::from(a)
                    } else if pb <= pc {
                        i32::from(b)
                    } else {
                        i32::from(c)
                    })
                }
                _ => return Err(format!("png: bad filter {filter}")),
            };
            cur[x] = u8::try_from(v.rem_euclid(256)).unwrap_or(0);
        }
        for x in 0..w_us {
            out[y * w_us * 3 + x * 3] = cur[x * channels];
            out[y * w_us * 3 + x * 3 + 1] = cur[x * channels + 1];
            out[y * w_us * 3 + x * 3 + 2] = cur[x * channels + 2];
        }
        prev = cur;
    }
    Ok(DecodedPng {
        width: w,
        height: h,
        rgb: out,
    })
}

/// Inflate the PNG IDAT stream via the shared flate filter, bounded by the
/// expected raw size (plus slack for the zlib framing).
///
/// # Budget
///
/// The decode is bounded by the expected raw scanline size and charged to a
/// harness-local unlimited budget guard: the input is the oracle driver's
/// output for an already size-capped canvas, not hostile document data.
fn flate_idat(idat: &[u8], expected: usize) -> Result<Vec<u8>, String> {
    let budget = selis_sandbox::Budget::unlimited();
    let clock = selis_sandbox::shell_clock();
    let mut guard = budget.guard_with(&clock, selis_sandbox::CancelToken::new());
    let limit = u64::try_from(expected)
        .unwrap_or(u64::MAX)
        .saturating_add(65_536);
    selis_pdf_filter::flate::flate_decode_bounded(idat, limit, &mut guard)
        .map_err(|e| format!("png: flate: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decode_round_trips_a_stored_block_image() {
        let (w, h) = (3u32, 2u32);
        let raw = vec![
            200, 100, 50, 0, 255, 0, 10, 20, 30, 250, 240, 230, 1, 2, 3, 4, 5, 6,
        ];
        let png = stored_block_png(w, h, &raw);
        let img = decode(&png).expect("decodes");
        assert_eq!((img.width, img.height), (w, h));
        assert_eq!(img.rgb, raw);
    }

    #[test]
    fn decode_handles_sub_filter() {
        // Row (filter 1, Sub): each byte predicted from the left neighbour.
        let raw = vec![10u8, 20, 30, 5, 5, 5];
        let mut idat = vec![1u8];
        idat.extend_from_slice(&raw);
        let png = png_with_idat(2, 1, &idat);
        let img = decode(&png).expect("decodes");
        assert_eq!(img.rgb, vec![10, 20, 30, 15, 25, 35]);
    }

    #[test]
    fn decode_drops_alpha_from_rgba() {
        // One RGBA row: filter byte 0, then 2 pixels x RGBA.
        let zlib = zlib_stored(&[0u8, 1, 2, 3, 255, 4, 5, 6, 0]);
        let png = variant_png(2, 1, 8, 6, 0, &zlib);
        let img = decode(&png).expect("decodes");
        assert_eq!(img.rgb, vec![1, 2, 3, 4, 5, 6]);
    }

    #[test]
    fn decode_rejects_non_png_bytes() {
        assert!(decode(b"not a png at all").is_err());
    }

    #[test]
    fn decode_rejects_interlaced_and_palette() {
        let raw = vec![0u8; 6];
        let png = variant_png(2, 1, 8, 3, 0, &zlib_stored(&raw));
        assert!(decode(&png).is_err());
        let png = variant_png(2, 1, 8, 2, 1, &zlib_stored(&raw));
        assert!(decode(&png).is_err());
    }

    #[test]
    fn dimensions_peek_reads_ihdr() {
        let png = stored_block_png(7, 5, &[0u8; 7 * 5 * 3]);
        assert_eq!(dimensions(&png), Ok((7, 5)));
        assert!(dimensions(b"short").is_err());
    }

    // ── helpers ──

    /// A minimal valid PNG (zlib stored blocks, no compression), the same
    /// encoding the PDFium oracle driver emits.
    fn stored_block_png(w: u32, h: u32, raw: &[u8]) -> Vec<u8> {
        // Filter byte 0 (None) per row, 3 bytes per pixel.
        let mut idat = Vec::new();
        let stride = usize::try_from(w).unwrap_or(1) * 3;
        for row in raw.chunks(stride) {
            idat.push(0);
            idat.extend_from_slice(row);
        }
        png_with_idat(w, h, &idat)
    }

    fn png_with_idat(w: u32, h: u32, idat_body: &[u8]) -> Vec<u8> {
        variant_png(w, h, 8, 2, 0, &zlib_stored(idat_body))
    }

    fn variant_png(w: u32, h: u32, depth: u8, colour: u8, interlace: u8, zlib: &[u8]) -> Vec<u8> {
        let mut png = Vec::new();
        png.extend_from_slice(b"\x89PNG\r\n\x1a\n");
        let mut ihdr = Vec::new();
        ihdr.extend_from_slice(&w.to_be_bytes());
        ihdr.extend_from_slice(&h.to_be_bytes());
        ihdr.extend_from_slice(&[depth, colour, 0, 0, interlace]);
        chunk(&mut png, b"IHDR", &ihdr);
        chunk(&mut png, b"IDAT", zlib);
        chunk(&mut png, b"IEND", &[]);
        png
    }

    /// Wrap `body` in a zlib stream with stored (uncompressed) deflate blocks
    /// and a correct adler32.
    fn zlib_stored(body: &[u8]) -> Vec<u8> {
        let mut zlib = vec![0x78u8, 0x01];
        let chunks: Vec<&[u8]> = body.chunks(65_535).collect();
        for (i, part) in chunks.iter().enumerate() {
            zlib.push(u8::from(i + 1 == chunks.len()));
            let len = u16::try_from(part.len()).unwrap_or(0);
            zlib.extend_from_slice(&len.to_le_bytes());
            zlib.extend_from_slice(&(!len).to_le_bytes());
            zlib.extend_from_slice(part);
        }
        let mut a: u32 = 1;
        let mut b: u32 = 0;
        for &byte in body {
            a = (a + u32::from(byte)) % 65_521;
            b = (b + a) % 65_521;
        }
        zlib.extend_from_slice(&(b << 16 | a).to_be_bytes());
        zlib
    }

    fn chunk(out: &mut Vec<u8>, kind: &[u8; 4], body: &[u8]) {
        out.extend_from_slice(&(body.len() as u32).to_be_bytes());
        out.extend_from_slice(kind);
        out.extend_from_slice(body);
        let mut crc_input = kind.to_vec();
        crc_input.extend_from_slice(body);
        let mut crc: u32 = 0xFFFF_FFFF;
        for byte in &crc_input {
            crc ^= u32::from(*byte);
            for _ in 0..8 {
                crc = if crc & 1 != 0 {
                    0xEDB8_8320 ^ (crc >> 1)
                } else {
                    crc >> 1
                };
            }
        }
        out.extend_from_slice(&(crc ^ 0xFFFF_FFFF).to_be_bytes());
    }
}
