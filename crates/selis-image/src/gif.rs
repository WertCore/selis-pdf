//! GIF87a/89a, read by this crate.
//!
//! A data-URI `<img>` can carry a GIF, and the conversion path needs its
//! samples rather than its bytes: PDF has no `GIFDecode` filter. This module
//! carries a small, self-contained reader for the subset a document image
//! normally uses — GIF87a and GIF89a, LZW-compressed indexed pixels, an
//! optional global or per-frame local colour table, interlaced scanlines and
//! the transparent index of a graphic control extension. It returns the same
//! [`Image`] the PNG and WebP readers return, so the conversion path can reuse
//! its one Flate re-encode.
//!
//! The reader is deliberately narrow and says so by name when it meets a
//! construct it does not implement. Frames are composited in file order by
//! overwriting their rectangle on a transparent canvas; there is no animation
//! timing and only the "leave the pixels" disposal method is accepted — a
//! frame that asks for restore-to-background or restore-to-previous is
//! `IMAGE_UNSUPPORTED`, as is a GIF with an unknown extension that changes the
//! image. Structural damage (a bad header, a short block, a missing trailer,
//! a frame outside the logical screen, an LZW code outside the dictionary, a
//! palette index outside its table) is `IMAGE_MALFORMED`. No partial image is
//! ever returned.
//!
//! Every allocation — the canvas, the LZW working set and the sub-block
//! accumulator — is charged to the caller's [`BudgetGuard`], and the logical
//! screen is checked against the same dimension and pixel caps PNG uses
//! before any of it is attempted.

use selis_error::{err, Code, Result};
use selis_sandbox::{alloc, BudgetGuard, Resource};

use crate::Image;

/// The two magic strings a GIF may open with (GIF87a / GIF89a, §2 of the spec).
const HEADER_87: &[u8; 6] = b"GIF87a";
const HEADER_89: &[u8; 6] = b"GIF89a";

/// A logical screen side above this is refused before anything is allocated.
const MAX_SIDE: u32 = 8192;
/// A logical screen above this many pixels is refused for the same reason.
const MAX_PIXELS: u64 = 8_000_000;
/// A cap on frames in one file. A document image has one; a short animation is
/// still flattened, but a hostile file cannot make us composite forever.
const MAX_FRAMES: usize = 256;
/// The LZW dictionary never has more than 12-bit codes.
const DICT_SIZE: usize = 4096;
/// A cap on the compressed bytes one frame may contribute. Four bytes per
/// pixel plus a little slack covers every ordinary LZW stream (including the
/// clear codes an encoder emits to keep its code width small).
const MAX_LZW_BYTES: u64 = 4 * 1_000_000;

/// Block introducers (GIF89a §2.9).
const EXTENSION: u8 = 0x21;
const IMAGE_SEPARATOR: u8 = 0x2C;
const TRAILER: u8 = 0x3B;

/// Graphic control extension label and its fixed block layout (GIF89a §2.6).
const GRAPHIC_CONTROL: u8 = 0xF9;
const GCE_LEN: u8 = 4;
const GCE_TRANSPARENCY: u8 = 0x01;
/// Disposal methods 0 (no action) and 1 (leave in place) composite identically
/// for this reader; 2 (restore to background) and 3 (restore to previous) need
/// a background/previous buffer we deliberately do not model, so they are
/// refused by name.
const GCE_DISPOSAL_MASK: u8 = 0x1C;
const GCE_DISPOSAL_SHIFT: u32 = 2;

/// One graphic control extension, applying to the next image block only.
#[derive(Debug, Clone, Copy, Default)]
struct Control {
    /// The palette index that must be painted as transparent, if any.
    transparent: Option<u8>,
}

/// Decode a GIF87a/89a file to RGBA8.
///
/// The logical screen supplies the canvas size. Each image block is LZW-decoded
/// into palette indices, de-interlaced when its descriptor says so, looked up
/// in its local colour table if it has one (otherwise the global table), and
/// painted at the descriptor's offset. A graphic control extension's
/// transparent index becomes alpha 0; all other pixels are opaque. Later frames
/// overwrite earlier ones, which flattens a short animation to its final ink
/// without ever fetching or painting anything outside the file.
///
/// # Budget
///
/// Charges the canvas against `Bytes` and `Pixels`, and every LZW working
/// buffer and sub-block accumulator against `Bytes`. The logical screen is
/// checked against [`MAX_SIDE`] and [`MAX_PIXELS`] first, so a hostile header
/// cannot ask for an unbounded allocation.
///
/// # Malformed Input
///
/// `IMAGE_MALFORMED` for a bad signature or screen, a truncated header/block/
/// trailer, a frame that reaches outside the screen, an LZW code outside its
/// dictionary, an early LZW end code, and a palette index outside its table.
/// `IMAGE_UNSUPPORTED` for an LZW minimum code size outside 2..=8 and for a
/// frame disposal method this reader does not composite. No partial image is
/// returned.
pub fn decode_gif(data: &[u8], g: &mut BudgetGuard<'_>) -> Result<Image> {
    if data.get(..6) != Some(HEADER_87.as_slice()) && data.get(..6) != Some(HEADER_89.as_slice()) {
        return Err(malformed("not a GIF87a or GIF89a file"));
    }
    let width = u32::from(le16(data, 6)?);
    let height = u32::from(le16(data, 8)?);
    if width == 0 || height == 0 {
        return Err(malformed("an empty logical screen"));
    }
    if width > MAX_SIDE || height > MAX_SIDE {
        return Err(malformed("a screen side above the 8192-pixel cap"));
    }
    if u64::from(width).saturating_mul(u64::from(height)) > MAX_PIXELS {
        return Err(malformed("a screen above the pixel cap"));
    }
    let packed = data.get(10).copied().unwrap_or(0);
    let mut at = 13usize;
    let global = if packed & 0x80 != 0 {
        let size = table_len(packed)?;
        let table = read_table(data, &mut at, size)?;
        table
    } else {
        Vec::new()
    };

    let pixels = usize::try_from(width)
        .unwrap_or(usize::MAX)
        .saturating_mul(usize::try_from(height).unwrap_or(usize::MAX));
    let mut canvas = alloc::vec_with_capacity::<u8>(g, pixels.saturating_mul(4))?;
    canvas.resize(pixels.saturating_mul(4), 0);
    g.charge(Resource::Pixels, u64::try_from(pixels).unwrap_or(u64::MAX))?;

    let mut control = Control::default();
    let mut frames = 0usize;
    let mut saw_trailer = false;
    while at < data.len() {
        match data.get(at).copied().unwrap_or(0) {
            TRAILER => {
                saw_trailer = true;
                break;
            }
            EXTENSION => {
                at = at.saturating_add(1);
                let label = data.get(at).copied().unwrap_or(0);
                at = at.saturating_add(1);
                if label == GRAPHIC_CONTROL {
                    control = read_control(data, &mut at)?;
                } else {
                    skip_sub_blocks(data, &mut at)?;
                }
            }
            IMAGE_SEPARATOR => {
                frames = frames.saturating_add(1);
                if frames > MAX_FRAMES {
                    return Err(malformed("more frames than the 256-frame cap"));
                }
                paint_frame(
                    data,
                    &mut at,
                    &global,
                    control,
                    width,
                    height,
                    &mut canvas,
                    g,
                )?;
                control = Control::default();
            }
            other => {
                return Err(malformed(&format!(
                    "unknown GIF block introducer 0x{other:02X}"
                )));
            }
        }
    }
    if frames == 0 {
        return Err(malformed("a GIF with no image block"));
    }
    if !saw_trailer {
        return Err(malformed("a GIF with no trailer"));
    }
    Ok(Image {
        width,
        height,
        rgba: canvas,
    })
}

/// The number of entries `2^(N+1)` a global/local table's size field asks for.
#[allow(clippy::arithmetic_side_effects)]
fn table_len(packed: u8) -> Result<usize> {
    let exponent = u32::from(packed & 0x07).saturating_add(1);
    1usize
        .checked_shl(exponent)
        .ok_or_else(|| malformed("a colour table size overflow"))
}

/// Read a colour table of `size` three-byte entries.
fn read_table(data: &[u8], at: &mut usize, size: usize) -> Result<Vec<[u8; 3]>> {
    let bytes = size.saturating_mul(3);
    let end = at.saturating_add(bytes);
    let raw = data
        .get(*at..end)
        .ok_or_else(|| malformed("a colour table that reaches outside the file"))?;
    *at = end;
    let mut table = Vec::with_capacity(size);
    for rgb in raw.chunks_exact(3) {
        table.push([
            rgb.first().copied().unwrap_or(0),
            rgb.get(1).copied().unwrap_or(0),
            rgb.get(2).copied().unwrap_or(0),
        ]);
    }
    Ok(table)
}

/// Read the fixed fields of a graphic control extension.
fn read_control(data: &[u8], at: &mut usize) -> Result<Control> {
    let len = data.get(*at).copied().unwrap_or(0);
    if len != GCE_LEN {
        return Err(malformed("a graphic control extension of the wrong length"));
    }
    let body = data
        .get(at.saturating_add(1)..at.saturating_add(1 + usize::from(len)))
        .ok_or_else(|| malformed("a truncated graphic control extension"))?;
    *at = at.saturating_add(1 + usize::from(len));
    let packed = body.first().copied().unwrap_or(0);
    let disposal = packed & GCE_DISPOSAL_MASK;
    if disposal > (1u8 << GCE_DISPOSAL_SHIFT) {
        return Err(unsupported("a GIF frame disposal method"));
    }
    let index = body.get(3).copied().unwrap_or(0);
    if data.get(*at).copied() != Some(0) {
        return Err(malformed("an unterminated graphic control extension"));
    }
    *at = at.saturating_add(1);
    Ok(Control {
        transparent: if packed & GCE_TRANSPARENCY != 0 {
            Some(index)
        } else {
            None
        },
    })
}

/// Skip the sub-blocks of an extension we do not interpret.
fn skip_sub_blocks(data: &[u8], at: &mut usize) -> Result<()> {
    loop {
        let len = usize::from(data.get(*at).copied().unwrap_or(0));
        *at = at.saturating_add(1);
        if len == 0 {
            return Ok(());
        }
        let end = at.saturating_add(len);
        if data.get(*at..end).is_none() {
            return Err(malformed("a truncated extension sub-block"));
        }
        *at = end;
    }
}

/// Decode and paint one image block.
#[allow(clippy::arithmetic_side_effects)]
fn paint_frame(
    data: &[u8],
    at: &mut usize,
    global: &[[u8; 3]],
    control: Control,
    screen_w: u32,
    screen_h: u32,
    canvas: &mut [u8],
    g: &mut BudgetGuard<'_>,
) -> Result<()> {
    let left = u32::from(le16(data, at.saturating_add(1))?);
    let top = u32::from(le16(data, at.saturating_add(3))?);
    let width = u32::from(le16(data, at.saturating_add(5))?);
    let height = u32::from(le16(data, at.saturating_add(7))?);
    let packed = data.get(at.saturating_add(9)).copied().unwrap_or(0);
    *at = at.saturating_add(10);
    if width == 0 || height == 0 {
        return Err(malformed("a zero-sized image block"));
    }
    if left.saturating_add(width) > screen_w || top.saturating_add(height) > screen_h {
        return Err(malformed("an image block outside the logical screen"));
    }
    let table = if packed & 0x80 != 0 {
        let size = table_len(packed)?;
        read_table(data, at, size)?
    } else {
        global.to_vec()
    };
    let interlace = packed & 0x40 != 0;
    if packed & 0x20 != 0 {
        return Err(unsupported("a sorted colour table"));
    }
    let min_code_size = data.get(*at).copied().unwrap_or(0);
    *at = at.saturating_add(1);
    let compressed = read_sub_blocks(data, at, g)?;
    let expected = usize::try_from(width)
        .unwrap_or(usize::MAX)
        .saturating_mul(usize::try_from(height).unwrap_or(usize::MAX));
    let indices = lzw_decode(&compressed, min_code_size, expected, g)?;
    let wide = usize::try_from(width).unwrap_or(usize::MAX);
    let high = usize::try_from(height).unwrap_or(usize::MAX);
    let screen = usize::try_from(screen_w).unwrap_or(usize::MAX);
    let ox = usize::try_from(left).unwrap_or(usize::MAX);
    let oy = usize::try_from(top).unwrap_or(usize::MAX);
    for row in 0..high {
        let target = if interlace {
            interlace_row(row, high).unwrap_or(0)
        } else {
            row
        };
        let src = row.saturating_mul(wide);
        for x in 0..wide {
            let index = usize::from(indices.get(src.saturating_add(x)).copied().unwrap_or(0));
            let rgb = table
                .get(index)
                .ok_or_else(|| malformed("a palette index outside the table"))?;
            let alpha = if control.transparent == Some(u8::try_from(index).unwrap_or(0)) {
                0
            } else {
                255
            };
            let px = oy
                .saturating_add(target)
                .saturating_mul(screen)
                .saturating_add(ox.saturating_add(x))
                .saturating_mul(4);
            if let Some(pixel) = canvas.get_mut(px..px.saturating_add(4)) {
                if let Some(cell) = pixel.get_mut(0) {
                    *cell = rgb[0];
                }
                if let Some(cell) = pixel.get_mut(1) {
                    *cell = rgb[1];
                }
                if let Some(cell) = pixel.get_mut(2) {
                    *cell = rgb[2];
                }
                if let Some(cell) = pixel.get_mut(3) {
                    *cell = alpha;
                }
            }
        }
    }
    Ok(())
}

/// Which raster row the `row`-th de-interlaced scanline belongs to (§2 of the
/// GIF spec: passes start at 0/4/2/1 with strides 8/8/4/2).
#[allow(clippy::arithmetic_side_effects)]
fn interlace_row(row: usize, height: usize) -> Option<usize> {
    let passes: [(usize, usize); 4] = [(0, 8), (4, 8), (2, 4), (1, 2)];
    let mut seen = 0usize;
    for (start, step) in passes {
        let mut y = start;
        while y < height {
            if seen == row {
                return Some(y);
            }
            y = y.saturating_add(step);
            seen = seen.saturating_add(1);
        }
    }
    None
}

/// Collect the data sub-blocks of one image into a single LZW stream.
fn read_sub_blocks(data: &[u8], at: &mut usize, g: &mut BudgetGuard<'_>) -> Result<Vec<u8>> {
    let mut out: Vec<u8> = Vec::new();
    let mut reserved = 0usize;
    loop {
        let len = usize::from(data.get(*at).copied().unwrap_or(0));
        *at = at.saturating_add(1);
        if len == 0 {
            return Ok(out);
        }
        let end = at.saturating_add(len);
        let bytes = data
            .get(*at..end)
            .ok_or_else(|| malformed("a truncated LZW sub-block"))?;
        if u64::try_from(out.len().saturating_add(len)).unwrap_or(u64::MAX) > MAX_LZW_BYTES {
            return Err(malformed("an LZW stream above the byte cap"));
        }
        if out.len().saturating_add(len) > reserved {
            let extra = out.len().saturating_add(len).saturating_sub(reserved);
            alloc::grow(g, &mut out, extra)?;
            reserved = reserved.saturating_add(extra);
        }
        out.extend_from_slice(bytes);
        *at = end;
    }
}

/// Decode GIF LZW codes into the `expected` palette indices of one frame.
///
/// Codes 0..`clear` are literal palette indices; `clear` resets the dictionary
/// and `end` closes the stream. The dictionary is kept as a prefix/suffix pair
/// rather than strings so its size is bounded by [`DICT_SIZE`] regardless of
/// how the stream is built.
#[allow(clippy::arithmetic_side_effects)]
fn lzw_decode(
    data: &[u8],
    min_code_size: u8,
    expected: usize,
    g: &mut BudgetGuard<'_>,
) -> Result<Vec<u8>> {
    if !(2..=8).contains(&min_code_size) {
        return Err(unsupported("an LZW minimum code size outside 2..=8"));
    }
    let clear = 1usize << min_code_size;
    let end = clear.saturating_add(1);
    let mut code_size = usize::from(min_code_size).saturating_add(1);
    let mut next = end.saturating_add(1);
    let mut prefix = alloc::vec_with_capacity::<i16>(g, DICT_SIZE)?;
    let mut suffix = alloc::vec_with_capacity::<u8>(g, DICT_SIZE)?;
    let mut stack = alloc::vec_with_capacity::<u8>(g, DICT_SIZE)?;
    let mut out = alloc::vec_with_capacity::<u8>(g, expected)?;
    prefix.resize(DICT_SIZE, -1);
    suffix.resize(DICT_SIZE, 0);
    for (code, byte) in (0..clear).enumerate() {
        if let Some(cell) = suffix.get_mut(code) {
            *cell = u8::try_from(byte).unwrap_or(0);
        }
    }

    let mut bit = 0usize;
    let mut old: Option<usize> = None;
    while out.len() < expected {
        let Some(code) = read_lzw_code(data, &mut bit, code_size) else {
            return Err(malformed("an LZW stream that ends before the frame"));
        };
        if code == clear {
            for (slot, byte) in (0..clear).enumerate() {
                if let Some(cell) = suffix.get_mut(slot) {
                    *cell = u8::try_from(byte).unwrap_or(0);
                }
            }
            code_size = usize::from(min_code_size).saturating_add(1);
            next = end.saturating_add(1);
            old = None;
            continue;
        }
        if code == end {
            return Err(malformed("an LZW end code before the frame is full"));
        }
        if code > next || (code == next && old.is_none()) {
            return Err(malformed("an LZW code outside the dictionary"));
        }
        stack.clear();
        let first = if code == next {
            let base = old.unwrap_or(0);
            let first = push_code(base, &prefix, &suffix, &mut stack)?;
            stack.push(first);
            first
        } else {
            push_code(code, &prefix, &suffix, &mut stack)?
        };
        if stack.len().saturating_add(out.len()) > expected {
            return Err(malformed("an LZW stream longer than the frame"));
        }
        for byte in stack.iter().rev() {
            out.push(*byte);
        }
        if let Some(previous) = old {
            if next < DICT_SIZE {
                if let (Some(p), Some(s)) = (prefix.get_mut(next), suffix.get_mut(next)) {
                    *p = i16::try_from(previous).unwrap_or(-1);
                    *s = first;
                }
                next = next.saturating_add(1);
                if next == (1usize << code_size) && code_size < 12 {
                    code_size = code_size.saturating_add(1);
                }
            }
        }
        old = Some(code);
    }
    Ok(out)
}

/// Walk one dictionary entry back to its root, pushing its bytes in reverse
/// order, and return its first byte (the one a later entry inherits).
fn push_code(code: usize, prefix: &[i16], suffix: &[u8], stack: &mut Vec<u8>) -> Result<u8> {
    let mut current = code;
    let mut steps = 0usize;
    loop {
        if current >= DICT_SIZE || steps >= DICT_SIZE {
            return Err(malformed("a cyclic LZW dictionary entry"));
        }
        let byte = suffix.get(current).copied().unwrap_or(0);
        stack.push(byte);
        let previous = prefix.get(current).copied().unwrap_or(-1);
        if previous < 0 {
            // `current` is a root: its suffix byte is the index itself, which
            // is also the first byte of this dictionary string.
            return Ok(byte);
        }
        current = usize::try_from(previous).unwrap_or(usize::MAX);
        steps = steps.saturating_add(1);
    }
}

/// Read one little-endian 16-bit field at `at`.
fn le16(data: &[u8], at: usize) -> Result<u16> {
    let end = at.saturating_add(2);
    let bytes: [u8; 2] = data
        .get(at..end)
        .and_then(|slice| slice.try_into().ok())
        .ok_or_else(|| malformed("a truncated GIF field"))?;
    Ok(u16::from_le_bytes(bytes))
}

/// Read `n` low bits at the current LZW stream position, or `None` at EOF.
#[allow(clippy::arithmetic_side_effects, clippy::integer_division)]
fn read_lzw_code(data: &[u8], at: &mut usize, n: usize) -> Option<usize> {
    let mut value = 0usize;
    for bit in 0..n {
        let byte = *data.get(at.saturating_div(8))?;
        let in_byte = (*at) % 8;
        value |= usize::from((byte >> in_byte) & 1) << bit;
        *at = at.saturating_add(1);
    }
    Some(value)
}

/// A file-level failure: bytes that do not describe what they claim to.
fn malformed(detail: &str) -> selis_error::Error {
    err!(Code::ImageMalformed, during = "gif-decode", detail = detail)
}

/// A valid GIF construct this reader does not composite.
fn unsupported(detail: &str) -> selis_error::Error {
    err!(
        Code::ImageUnsupported,
        during = "gif-decode",
        detail = detail
    )
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::indexing_slicing,
        clippy::arithmetic_side_effects,
        clippy::cast_possible_truncation,
        clippy::integer_division
    )]

    use super::*;
    use selis_sandbox::{Budget, CancelToken, FixedClock};

    fn guard() -> BudgetGuard<'static> {
        Budget::unlimited().guard_with(&FixedClock(0), CancelToken::new())
    }

    /// A tiny standard-base64 decoder, so the fixtures stay readable text.
    fn b64(text: &str) -> Vec<u8> {
        let value = |byte: u8| match byte {
            b'A'..=b'Z' => byte - b'A',
            b'a'..=b'z' => byte - b'a' + 26,
            b'0'..=b'9' => byte - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            b'=' => 0,
            _ => panic!("not base64"),
        };
        let bytes: Vec<u8> = text.bytes().filter(|b| !b.is_ascii_whitespace()).collect();
        let mut out = Vec::new();
        for chunk in bytes.chunks(4) {
            let n = (u32::from(value(chunk[0])) << 18)
                | (u32::from(value(*chunk.get(1).unwrap_or(&b'A'))) << 12)
                | (u32::from(value(*chunk.get(2).unwrap_or(&b'A'))) << 6)
                | u32::from(value(*chunk.get(3).unwrap_or(&b'A')));
            out.push((n >> 16) as u8);
            if chunk.len() > 2 {
                out.push((n >> 8) as u8);
            }
            if chunk.len() > 3 {
                out.push(n as u8);
            }
        }
        out
    }

    fn decode(text: &str) -> Image {
        let bytes = b64(text);
        decode_gif(&bytes, &mut guard()).expect("decode GIF fixture")
    }

    /// Real GIFs written by Pillow/libgif: the LZW streams use the growing
    /// dictionary and the clear codes a production encoder emits.
    #[test]
    fn pillow_gifs_decode_to_their_exact_rgba_samples() {
        let red = decode("R0lGODdhBAAEAIEAAP8AAAAAAAAAAAAAACwAAAAABAAEAAAICQABCBxIsCCAgAA7");
        assert_eq!((red.width, red.height), (4, 4));
        assert_eq!(red.rgba, [255, 0, 0, 255].repeat(16));

        let checker =
            decode("R0lGODdhBAAEAIEAAP8AAACA/wAAAAAAACwAAAAABAAEAAAIDAABBBA4cKBAggcDAgA7");
        for y in 0..4 {
            for x in 0..4 {
                let at = (y * 4 + x) * 4;
                let expected = if (x + y) % 2 == 0 {
                    [255, 0, 0, 255]
                } else {
                    [0, 128, 255, 255]
                };
                assert_eq!(
                    [
                        checker.rgba[at],
                        checker.rgba[at + 1],
                        checker.rgba[at + 2],
                        checker.rgba[at + 3]
                    ],
                    expected,
                    "pixel {x},{y}"
                );
            }
        }

        let green = decode("R0lGODdhIAAgAIEAAACgAAAAAAAAAAAAACwAAAAAIAAgAEAINQABCBxIsKDBgwgTKlzIsKHDhxAjSpxIsaLFixgzatzIsaPHjyBDihxJsqTJkyhTqlzJUmRAADs=");
        assert_eq!((green.width, green.height), (32, 32));
        assert_eq!(green.rgba, [0, 160, 0, 255].repeat(32 * 32));
    }

    #[test]
    fn a_graphic_control_transparent_index_becomes_alpha_zero() {
        let image = decode(
            "R0lGODlhBAAEAIEAAP8AAAAA/wAAAAAAACH5BAEAAAEALAAAAAAEAAQAAAgOAAEACBBAIEGDBQcGCAgAOw==",
        );
        for y in 0..4 {
            for x in 0..4 {
                let at = (y * 4 + x) * 4;
                let expected = if x < 2 {
                    [255, 0, 0, 255]
                } else {
                    [0, 0, 255, 0]
                };
                assert_eq!(
                    [
                        image.rgba[at],
                        image.rgba[at + 1],
                        image.rgba[at + 2],
                        image.rgba[at + 3]
                    ],
                    expected,
                    "pixel {x},{y}"
                );
            }
        }
    }

    /// Emit the codes of a literal-only LZW stream. A clear code every two
    /// pixels keeps the code width at the initial three bits, so the fixture
    /// exercises interlace/tables rather than dictionary growth.
    #[allow(clippy::arithmetic_side_effects)]
    fn lzw_literals(indices: &[u8]) -> Vec<u8> {
        let clear = 4u32;
        let end = 5u32;
        let mut codes = vec![clear];
        let mut since_clear = 0usize;
        for &index in indices {
            if since_clear == 2 {
                codes.push(clear);
                since_clear = 0;
            }
            codes.push(u32::from(index));
            since_clear += 1;
        }
        codes.push(end);
        let mut bits = 0u64;
        let mut used = 0u32;
        let mut bytes = Vec::new();
        for code in codes {
            bits |= u64::from(code) << used;
            used += 3;
            while used >= 8 {
                bytes.push((bits & 0xFF) as u8);
                bits >>= 8;
                used -= 8;
            }
        }
        if used > 0 {
            bytes.push((bits & 0xFF) as u8);
        }
        bytes
    }

    /// A GIF built byte by byte, so the interlaced-image and local-table paths
    /// can be driven without relying on an encoder that happens to use them.
    fn custom_gif(
        width: u16,
        height: u16,
        rows: &[Vec<u8>],
        interlace: bool,
        local: bool,
    ) -> Vec<u8> {
        let mut out = b"GIF89a".to_vec();
        out.extend_from_slice(&width.to_le_bytes());
        out.extend_from_slice(&height.to_le_bytes());
        out.push(0xF1); // global table, 4 entries
        out.push(0);
        out.push(0);
        // Global palette: red, green, blue, white.
        out.extend_from_slice(&[255, 0, 0, 0, 255, 0, 0, 0, 255, 255, 255, 255]);
        // Graphic control: transparency on index 3 (white).
        out.extend_from_slice(&[0x21, 0xF9, 0x04, 0x01, 0, 0, 3, 0]);
        out.push(0x2C);
        out.extend_from_slice(&[0, 0, 0, 0]);
        out.extend_from_slice(&width.to_le_bytes());
        out.extend_from_slice(&height.to_le_bytes());
        let mut descriptor_flags = 0u8;
        if interlace {
            descriptor_flags |= 0x40;
        }
        if local {
            descriptor_flags |= 0x81; // local table, 4 entries
        }
        out.push(descriptor_flags);
        if local {
            out.extend_from_slice(&[0, 0, 0, 255, 255, 0, 0, 255, 255, 255, 0, 255]);
        }
        // Reorder the rows into the four interlace passes when requested.
        let mut order: Vec<usize> = (0..usize::from(height)).collect();
        if interlace {
            order = [(0usize, 8usize), (4, 8), (2, 4), (1, 2)]
                .into_iter()
                .flat_map(|(start, step)| (start..usize::from(height)).step_by(step))
                .collect();
        }
        let mut indices = Vec::new();
        for row in order {
            if let Some(bytes) = rows.get(row) {
                indices.extend_from_slice(bytes);
            }
        }
        let compressed = lzw_literals(&indices);
        out.push(2); // LZW minimum code size
        for chunk in compressed.chunks(255) {
            out.push(chunk.len() as u8);
            out.extend_from_slice(chunk);
        }
        out.push(0);
        out.push(0x3B);
        out
    }

    #[test]
    fn interlace_local_table_offset_and_transparency_all_compose() {
        // A 4x8 frame: each row is a solid palette index. Interlaced storage
        // reorders the rows; the decoder must put them back.
        let rows: Vec<Vec<u8>> = (0u8..8).map(|i| vec![i % 4; 4]).collect();
        let gif = custom_gif(4, 8, &rows, true, true);
        let image = decode_gif(&gif, &mut guard()).expect("interlaced GIF");
        // Local table (black, yellow, cyan, magenta); index 3 is transparent.
        let palette = [
            [0, 0, 0, 255],
            [255, 255, 0, 255],
            [0, 255, 255, 255],
            [255, 0, 255, 0],
        ];
        for row in 0..8 {
            let expected = &palette[row % 4];
            for x in 0..4 {
                let at = row * 4 * 4 + x * 4;
                assert_eq!(&image.rgba[at..at + 4], expected, "row {row}, column {x}");
            }
        }
    }

    #[test]
    fn malformed_and_truncated_gifs_are_refused_without_partial_output() {
        let good = b64("R0lGODdhBAAEAIEAAP8AAAAAAAAAAAAAACwAAAAABAAEAAAICQABCBxIsCCAgAA7");
        let cases: Vec<Vec<u8>> = vec![
            b"not a gif at all".to_vec(),
            good.get(..10).unwrap_or(&[]).to_vec(), // short screen descriptor
            good.get(..30).unwrap_or(&[]).to_vec(), // no trailer or complete frame
            {
                let mut bad = good.clone();
                if let Some(slot) = bad.get_mut(30) {
                    *slot = 0xFF; // frame width reaches outside the screen
                }
                bad
            },
        ];
        for case in cases {
            let error = decode_gif(&case, &mut guard()).expect_err("must refuse");
            assert_eq!(error.code(), Code::ImageMalformed, "{case:?}: {error:?}");
        }
        // A minimum code size the format does not allow is a different typed
        // refusal: the stream is structurally sound but outside the subset.
        let mut bad_width = good.clone();
        if let Some(slot) = bad_width.get_mut(35) {
            *slot = 1;
        }
        assert_eq!(
            decode_gif(&bad_width, &mut guard())
                .expect_err("bad code size")
                .code(),
            Code::ImageUnsupported
        );
        // A palette index past the end of the table is malformed too: build a
        // one-pixel GIF whose LZW stream emits index 2 with a two-colour table.
        let mut bad = b"GIF89a".to_vec();
        bad.extend_from_slice(&[1, 0, 1, 0, 0x81, 0, 0, 255, 0, 0, 0, 0, 255]);
        bad.extend_from_slice(&[0x2C, 0, 0, 0, 0, 1, 0, 1, 0, 0]);
        bad.extend_from_slice(&[2, 1, 0x12, 0]); // one code: literal index 2
        bad.push(0x3B);
        let error = decode_gif(&bad, &mut guard()).expect_err("palette overrun");
        assert_eq!(error.code(), Code::ImageMalformed, "{error:?}");
    }

    #[test]
    fn an_unsupported_disposal_method_is_refused_by_name() {
        let mut gif = b64(
            "R0lGODlhBAAEAIEAAP8AAAAA/wAAAAAAACH5BAEAAAEALAAAAAAEAAQAAAgOAAEACBBAIEGDBQcGCAgAOw==",
        );
        // The graphic control's packed byte sits at offset 28 in this fixture.
        if let Some(slot) = gif.get_mut(28) {
            *slot = 0x09; // transparency flag plus disposal method 2
        }
        let error = decode_gif(&gif, &mut guard()).expect_err("disposal method");
        assert_eq!(error.code(), Code::ImageUnsupported, "{error:?}");
        assert!(
            error.to_string().contains("disposal"),
            "refused with other words: {error}"
        );
    }

    #[test]
    fn a_budget_too_small_for_the_canvas_refuses_the_gif() {
        let bytes = b64("R0lGODdhIAAgAIEAAACgAAAAAAAAAAAAACwAAAAAIAAgAEAINQABCBxIsKDBgwgTKlzIsKHDhxAjSpxIsaLFixgzatzIsaPHjyBDihxJsqTJkyhTqlzJUmRAADs=");
        let small = Budget {
            bytes: 1024,
            pixels: 1,
            ..Budget::unlimited()
        };
        let mut g = small.guard();
        assert!(decode_gif(&bytes, &mut g).is_err());
    }
}
