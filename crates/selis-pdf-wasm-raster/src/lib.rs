//! Raster lazy chunk: PDF pages → PNG or JPEG files, on the device.
//!
//! Separate `cdylib` from the viewer and the tools chunk. The tools page never
//! downloads this module until the user asks for images; it is fetched then and
//! cached by the service worker. Like the tools and convert chunks it speaks a
//! pointer ABI and **declares no imports**: the page instantiates it with an
//! empty import object, which is the mechanical form of "this module cannot
//! call out". All document bytes arrive through [`selis_raster_alloc`] and
//! leave through the returned guest allocation; nothing touches a network.
//!
//! The render itself is the engine's own whole-page path
//! ([`Session::render_page_rgba`]), and the encoders are the engine's own
//! writers: `selis-image`'s deterministic PNG and the owned baseline JPEG.
//! One page becomes one file; several pages are returned as one deterministic
//! ZIP so a browser does not have to allow a burst of downloads.
//!
//! Both formats are read back before the chunk returns. A PNG must reproduce
//! the rendered pixels exactly; a JPEG is lossy, so it must decode under the
//! engine's own JPEG reader, match the page's size, and stay inside error bounds
//! measured against the harshest content the coder can honestly be given. A page
//! too large for that reader to reopen is refused rather than shipped unchecked.
//! A file that fails is reported as a failure instead of being handed over with
//! a "verified" label.

#![allow(unsafe_code)]

use std::alloc::{alloc, dealloc, Layout};
use std::sync::Mutex;

use selis_image::{encode_jpeg, encode_png, Image};
use selis_pdf_engine::Session;
use selis_sandbox::{Budget, CancelToken, FixedClock, Surface};

/// Chunk identifier (stable, matches the manifest).
pub const CHUNK_ID: &str = "raster";
/// Chunk version.
pub const CHUNK_VERSION: &str = env!("CARGO_PKG_VERSION");

/// The PNG encoder level. Six is zlib's conventional default: a page-sized
/// image compresses well without the level-9 time cost showing up as a stalled
/// tab. The output is deterministic at any fixed level.
const PNG_LEVEL: u8 = 6;

/// The largest mean channel error a verified JPEG may carry, in 8-bit levels.
///
/// Measured against the pixels the encoder was handed. The honest ceiling is set
/// by the content, not by the writer: a text or line-art page averages under 2
/// levels at any quality, and the harshest input a block transform can be given
/// — uniform noise, which is what a scanned page's grain looks like to the coder
/// — averages 62 levels at quality 1, 48 at quality 25, 28 at quality 50 and 6
/// at quality 90. Ninety-six clears all of that and still refuses a file that is
/// systematically not the page: black for white measures 255, and one page for
/// another of the same size measures in the same region.
///
/// It is not a corruption detector. One damaged byte inside the entropy-coded
/// segment of a 1.7-megapixel page moves the mean by about 1, because the damage
/// stays local; what catches that is the decoder refusing the stream (which is
/// how the writer's own ZRL bug announced itself) and, from
/// [`JPG_SAMPLE_BOUND_MIN_QUALITY`] upward, [`JPG_MAX_SAMPLE_ERROR`].
const JPG_MAX_MEAN_ERROR: u64 = 96;

/// The largest single-sample error a verified JPEG may carry, in 8-bit levels.
///
/// A local witness for the case the mean hides: a handful of badly wrong
/// samples inside an otherwise perfect page. Damage saturates — one wrong byte
/// in the scan puts samples a full 255 levels out — so the ceiling has to sit
/// below that, while honest content at a coarse quality reaches 255 all by
/// itself (noise: 255 at quality 50, 103 at quality 75, 45 at quality 90). The
/// two ranges only separate above [`JPG_SAMPLE_BOUND_MIN_QUALITY`], so that is
/// where this applies and nowhere else. A bound switched on where the format
/// cannot meet it is not a check; it is a outage.
const JPG_MAX_SAMPLE_ERROR: u64 = 160;

/// The lowest quality at which [`JPG_MAX_SAMPLE_ERROR`] is enforced.
///
/// Below it, honest grain and a damaged scan are the same number, and the chunk
/// would be refusing correct files to catch a bug it cannot tell apart.
const JPG_SAMPLE_BOUND_MIN_QUALITY: u8 = 75;

/// The largest JPEG edge this device can read back.
///
/// Verification decodes the file the chunk just wrote, and the engine's JPEG
/// reader declines to open a frame wider or taller than 16384 samples — a limit
/// it applies to untrusted input, which is the right posture for a PDF but not
/// something this chunk can lift for its own output. A page can exceed it: a
/// 2000-point-wide banner at 600 DPI is 16,668 px. A file that cannot be read
/// back is a file that cannot honestly be called verified, so the chunk refuses
/// it and names the knob that fixes it, rather than shipping an unchecked JPEG.
const MAX_JPEG_EDGE: u32 = 16_384;

/// The last error message. A `Mutex`, not a `static mut`: a shared reference to
/// a mutable static is undefined behaviour even on a single thread.
static LAST_ERROR: Mutex<Option<String>> = Mutex::new(None);

fn set_error(message: String) {
    if let Ok(mut slot) = LAST_ERROR.lock() {
        *slot = Some(message);
    }
}

fn clear_error() {
    if let Ok(mut slot) = LAST_ERROR.lock() {
        *slot = None;
    }
}

fn last_error() -> Option<String> {
    LAST_ERROR.lock().ok().and_then(|slot| slot.clone())
}

fn alloc_bytes(data: &[u8]) -> *mut u8 {
    if data.is_empty() {
        return std::ptr::null_mut();
    }
    let Ok(layout) = Layout::from_size_align(data.len(), 1) else {
        return std::ptr::null_mut();
    };
    let ptr = unsafe { alloc(layout) };
    if ptr.is_null() {
        return std::ptr::null_mut();
    }
    unsafe { std::ptr::copy_nonoverlapping(data.as_ptr(), ptr, data.len()) };
    ptr
}

/// # Safety
/// `out_len` must be a writable 4-byte-aligned guest `u32` slot.
#[no_mangle]
pub unsafe extern "C" fn selis_raster_chunk_id(out_len: *mut u32) -> *mut u8 {
    if out_len.is_null() {
        return std::ptr::null_mut();
    }
    let bytes = CHUNK_ID.as_bytes();
    let Ok(len_u32) = u32::try_from(bytes.len()) else {
        return std::ptr::null_mut();
    };
    let Ok(layout) = Layout::from_size_align(bytes.len(), 1) else {
        return std::ptr::null_mut();
    };
    let out = unsafe { alloc(layout) };
    if out.is_null() {
        return std::ptr::null_mut();
    }
    unsafe {
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), out, bytes.len());
        out_len.write(len_u32);
    }
    out
}

/// Allocate `len` bytes of guest memory for the caller to fill.
#[no_mangle]
pub extern "C" fn selis_raster_alloc(len: u32) -> *mut u8 {
    let Ok(size) = usize::try_from(len) else {
        return std::ptr::null_mut();
    };
    if size == 0 {
        return std::ptr::null_mut();
    }
    let Ok(layout) = Layout::from_size_align(size, 1) else {
        return std::ptr::null_mut();
    };
    // SAFETY: `layout` has a non-zero size, which `alloc` requires.
    unsafe { alloc(layout) }
}

/// # Safety
/// `ptr`/`len` must identify a live allocation from this module, freed once.
#[no_mangle]
pub unsafe extern "C" fn selis_raster_free(ptr: *mut u8, len: u32) {
    if ptr.is_null() || len == 0 {
        return;
    }
    let Ok(bytes) = usize::try_from(len) else {
        return;
    };
    let Ok(layout) = Layout::from_size_align(bytes, 1) else {
        return;
    };
    unsafe { dealloc(ptr, layout) };
}

/// Returns the last error message as guest memory, freed with
/// [`selis_raster_free`]. Writes its length to `out_len`.
///
/// # Safety
/// `out_len` must be a writable 4-byte-aligned guest `u32` slot.
#[no_mangle]
pub unsafe extern "C" fn selis_raster_last_error(out_len: *mut u32) -> *mut u8 {
    if out_len.is_null() {
        return std::ptr::null_mut();
    }
    let message = last_error().unwrap_or_default();
    if message.is_empty() {
        out_len.write(0);
        return std::ptr::null_mut();
    }
    let bytes = message.as_bytes();
    let Ok(len_u32) = u32::try_from(bytes.len()) else {
        out_len.write(0);
        return std::ptr::null_mut();
    };
    let ptr = alloc_bytes(bytes);
    if ptr.is_null() {
        out_len.write(0);
        return std::ptr::null_mut();
    }
    out_len.write(len_u32);
    ptr
}

/// Availability probe.
#[no_mangle]
pub extern "C" fn selis_raster_available() -> u32 {
    1
}

/// Render the selected pages to PNG or JPEG.
///
/// `pages_ptr`/`pages_len` is a UTF-8 selection in the same 1-based syntax the
/// page's other tools use (`""` = every page, `"1-3,5"` = those four).
/// `format` selects the writer — `0` is PNG, `1` is JPEG — and `quality` is
/// the JPEG knob (1..=100); the PNG leg ignores it and keeps its own fixed
/// compression level. The return value is one image when the selection has one
/// page, otherwise a deterministic ZIP of `page-NNN.png` or `page-NNN.jpg`
/// entries. `out_files` receives the count and `out_len` the byte length; free
/// the result with [`selis_raster_free`].
///
/// On failure the return value is null and [`selis_raster_last_error`] carries
/// the reason.
///
/// # Safety
/// Both input slices must identify live guest allocations for the duration of
/// the call, and both output slots must be writable `u32`s.
#[no_mangle]
pub unsafe extern "C" fn selis_raster_run(
    input_ptr: *const u8,
    input_len: u32,
    pages_ptr: *const u8,
    pages_len: u32,
    dpi: u32,
    format: u32,
    quality: u32,
    out_len: *mut u32,
    out_files: *mut u32,
) -> *mut u8 {
    clear_error();
    if out_len.is_null() || out_files.is_null() {
        set_error("missing output slot".to_string());
        return std::ptr::null_mut();
    }
    if input_ptr.is_null() || input_len == 0 {
        set_error("empty input".to_string());
        return std::ptr::null_mut();
    }
    let input = unsafe { std::slice::from_raw_parts(input_ptr, input_len as usize) };
    let selection = if pages_ptr.is_null() || pages_len == 0 {
        ""
    } else {
        let bytes = unsafe { std::slice::from_raw_parts(pages_ptr, pages_len as usize) };
        match std::str::from_utf8(bytes) {
            Ok(text) => text,
            Err(_) => {
                set_error("page selection is not valid UTF-8".to_string());
                return std::ptr::null_mut();
            }
        }
    };
    let format = match Format::from_code(format) {
        Ok(format) => format,
        Err(message) => {
            set_error(message);
            return std::ptr::null_mut();
        }
    };
    match run_impl(input, selection, dpi, format, quality) {
        Ok((bytes, files)) => {
            let Ok(len_u32) = u32::try_from(bytes.len()) else {
                set_error("output too large".to_string());
                return std::ptr::null_mut();
            };
            let ptr = alloc_bytes(&bytes);
            if ptr.is_null() {
                set_error("out of memory".to_string());
                return std::ptr::null_mut();
            }
            unsafe {
                out_len.write(len_u32);
                out_files.write(files);
            }
            ptr
        }
        Err(message) => {
            set_error(message);
            std::ptr::null_mut()
        }
    }
}

/// The image formats the chunk writes, numbered as the ABI numbers them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Format {
    /// Lossless PNG, verified sample for sample.
    Png,
    /// Lossy baseline JPEG, verified inside a measured bound.
    Jpg,
}

impl Format {
    /// Decode the ABI's number. An unknown code is a caller bug, and answering
    /// it with PNG — the format that happens to be first — would show the user
    /// a file they did not ask for.
    fn from_code(code: u32) -> Result<Self, String> {
        match code {
            0 => Ok(Self::Png),
            1 => Ok(Self::Jpg),
            other => Err(format!("unknown image format code {other}")),
        }
    }

    /// The file extension, without the dot.
    fn extension(self) -> &'static str {
        match self {
            Self::Png => "png",
            Self::Jpg => "jpg",
        }
    }
}

/// The safe body of [`selis_raster_run`], unit-testable on the host.
fn run_impl(
    input: &[u8],
    selection: &str,
    dpi: u32,
    format: Format,
    quality: u32,
) -> Result<(Vec<u8>, u32), String> {
    if dpi == 0 || dpi > 600 {
        return Err("DPI must be between 1 and 600".to_string());
    }
    // PNG has no quality knob; the level it compresses at is this module's own
    // choice. JPEG's is the user's, and 1..=100 is what the writer accepts.
    let level = match format {
        Format::Png => 0,
        Format::Jpg => {
            if quality == 0 || quality > 100 {
                return Err("JPEG quality must be between 1 and 100".to_string());
            }
            u8::try_from(quality).unwrap_or(100)
        }
    };
    let budget = Budget::profile(Surface::Batch);
    let clock = FixedClock(0);
    let session = Session::open(input.to_vec(), &budget, &clock)
        .map_err(|e| format!("cannot open PDF: {e}"))?;
    let pages = parse_pages(selection, session.len())?;
    let total = u32::try_from(session.len()).unwrap_or(u32::MAX);
    let width = decimal_width(total);
    let mut rendered: Vec<(String, Vec<u8>)> = Vec::with_capacity(pages.len());
    for &page in &pages {
        let mut g = budget.guard_with(&clock, CancelToken::new());
        let pixels = session
            .render_page_rgba(page, f64::from(dpi), &budget, &mut g)
            .map_err(|e| format!("page {} could not be rendered: {e}", page.saturating_add(1)))?;
        let image = Image {
            width: pixels.width,
            height: pixels.height,
            rgba: pixels.rgba,
        };
        let encoded = match format {
            Format::Png => {
                let png = encode_png(&image, PNG_LEVEL, &mut g).map_err(|e| {
                    format!("page {} could not be encoded: {e}", page.saturating_add(1))
                })?;
                verify_png(&image, &png, &mut g, page)?;
                png
            }
            Format::Jpg => {
                check_jpeg_is_verifiable(&image, dpi, page)?;
                let jpg = encode_jpeg(&image, level, &mut g).map_err(|e| {
                    format!("page {} could not be encoded: {e}", page.saturating_add(1))
                })?;
                verify_jpg(&image, &jpg, level, &mut g, page)?;
                jpg
            }
        };
        rendered.push((
            format!(
                "page-{:0width$}.{extension}",
                page.saturating_add(1),
                width = width,
                extension = format.extension()
            ),
            encoded,
        ));
    }
    let files = u32::try_from(rendered.len()).map_err(|_| "too many pages".to_string())?;
    if rendered.len() == 1 {
        return rendered
            .pop()
            .map(|(_, bytes)| (bytes, files))
            .ok_or_else(|| "no pages selected".to_string());
    }
    let entries: Vec<(&str, &[u8])> = rendered
        .iter()
        .map(|(name, bytes)| (name.as_str(), bytes.as_slice()))
        .collect();
    let zip =
        selis_pdf_tools::bundle_zip(&entries).map_err(|e| format!("cannot bundle images: {e}"))?;
    Ok((zip, files))
}

/// Read a PNG back and demand the pixels it was made from.
///
/// This is the strict form of the Phase 2 exit gate: a PNG that does not
/// round-trip is a bug in the writer, and the user must not receive it with a
/// "verified" label on top.
fn verify_png(
    image: &Image,
    bytes: &[u8],
    g: &mut selis_sandbox::BudgetGuard<'_>,
    page: usize,
) -> Result<(), String> {
    let label = page.saturating_add(1);
    let decoded = selis_image::decode_png(bytes, g)
        .map_err(|e| format!("page {label} output could not be read back: {e}"))?;
    if decoded.width != image.width || decoded.height != image.height || decoded.rgba != image.rgba
    {
        return Err(format!(
            "page {label} failed pixel verification: the PNG does not round-trip"
        ));
    }
    Ok(())
}

/// Refuse a JPEG this device cannot read back, before spending time making one.
///
/// The check is about honesty, not capability: the encode would succeed, and the
/// file would probably be fine. It just could not be *verified*, and this chunk
/// does not hand over an unchecked file under a verified label. PNG keeps its
/// exact round trip at any size, so the page is not lost — only the format.
fn check_jpeg_is_verifiable(image: &Image, dpi: u32, page: usize) -> Result<(), String> {
    if image.width <= MAX_JPEG_EDGE && image.height <= MAX_JPEG_EDGE {
        return Ok(());
    }
    let label = page.saturating_add(1);
    Err(format!(
        "page {label} at {dpi} DPI is {}x{} px, and a JPEG wider than {MAX_JPEG_EDGE} px cannot be \
         read back on this device to be verified; lower the DPI, or use PDF to PNG for an exact \
         round trip",
        image.width, image.height
    ))
}

/// Read a JPEG back and demand that it is still the page.
///
/// Four checks, each scoped to what it can actually prove (see the constants):
/// the file must decode at all, which is what a structurally broken stream
/// fails; it must carry the page's own dimensions; it must carry samples; and
/// those samples must average inside [`JPG_MAX_MEAN_ERROR], with
/// [`JPG_MAX_SAMPLE_ERROR`] as the local ceiling from
/// [`JPG_SAMPLE_BOUND_MIN_QUALITY`] upward. Below that quality the coder's own
/// error on grainy content reaches 255 levels, so the local ceiling is not
/// applied rather than being widened until it can never fire.
///
/// The samples compared are the ones the encoder was given, which is
/// [`selis_image::flatten_over_white`] of the render: the alpha rule is not a
/// coding error, and comparing against the pre-flattening buffer would charge
/// the encoder's white background to the coder.
fn verify_jpg(
    image: &Image,
    bytes: &[u8],
    quality: u8,
    g: &mut selis_sandbox::BudgetGuard<'_>,
    page: usize,
) -> Result<(), String> {
    let label = page.saturating_add(1);
    let source = selis_image::flatten_over_white(&image.rgba, g)
        .map_err(|e| format!("page {label} could not be prepared for verification: {e}"))?;
    let decoded = selis_image::decode_jpeg(bytes, g)
        .map_err(|e| format!("page {label} output could not be read back: {e}"))?;
    if decoded.width != image.width || decoded.height != image.height {
        return Err(format!(
            "page {label} failed image verification: the JPEG is {}x{}, the page is {}x{}",
            decoded.width, decoded.height, image.width, image.height
        ));
    }
    let mut total = 0u64;
    let mut worst = 0u64;
    let mut samples = 0u64;
    for (index, px) in decoded.rgba.chunks_exact(4).enumerate() {
        for channel in 0..3usize {
            let (Some(got), Some(want)) = (
                px.get(channel).copied(),
                source
                    .get(index.saturating_mul(3).saturating_add(channel))
                    .copied(),
            ) else {
                continue;
            };
            let difference = u64::from(
                got.saturating_sub(want)
                    .saturating_add(want.saturating_sub(got)),
            );
            total = total.saturating_add(difference);
            worst = worst.max(difference);
            samples = samples.saturating_add(1);
        }
    }
    let mean = match total.checked_div(samples) {
        Some(mean) => mean,
        // The only way this fails is an empty sample count: a frame that
        // decoded at the page's size but carried nothing to compare.
        None => {
            return Err(format!(
                "page {label} failed image verification: the JPEG carries no samples"
            ));
        }
    };
    if mean > JPG_MAX_MEAN_ERROR {
        return Err(format!(
            "page {label} failed image verification: the JPEG differs from the page by {mean} levels on average, more than the {JPG_MAX_MEAN_ERROR} allowed"
        ));
    }
    if quality >= JPG_SAMPLE_BOUND_MIN_QUALITY && worst > JPG_MAX_SAMPLE_ERROR {
        return Err(format!(
            "page {label} failed image verification: a JPEG sample differs from the page by {worst} levels, more than the {JPG_MAX_SAMPLE_ERROR} allowed"
        ));
    }
    Ok(())
}

/// Parse a 1-based page selection. An empty string means every page.
fn parse_pages(text: &str, page_count: usize) -> Result<Vec<usize>, String> {
    let text = text.trim();
    if text.is_empty() {
        return Ok((0..page_count).collect());
    }
    if page_count == 0 {
        return Err("the document has no pages".to_string());
    }
    let mut pages: Vec<usize> = Vec::new();
    for part in text.split(',') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        let (first, last) = match part.split_once('-') {
            Some((a, b)) => (parse_page(a, page_count)?, parse_page(b, page_count)?),
            None => {
                let n = parse_page(part, page_count)?;
                (n, n)
            }
        };
        if first > last {
            return Err(format!("page range {part:?} runs backwards"));
        }
        for n in first..=last {
            let index = n.saturating_sub(1);
            if !pages.contains(&index) {
                pages.push(index);
            }
        }
    }
    if pages.is_empty() {
        return Err("no pages selected".to_string());
    }
    Ok(pages)
}

fn parse_page(text: &str, page_count: usize) -> Result<usize, String> {
    let text = text.trim();
    let n: usize = text
        .parse()
        .map_err(|_| format!("{text:?} is not a page number"))?;
    if n == 0 || n > page_count {
        return Err(format!(
            "page {n} is outside this document (1-{page_count})"
        ));
    }
    Ok(n)
}

/// The number of decimal digits in `n`, at least one.
fn decimal_width(n: u32) -> usize {
    let mut digits = 1usize;
    let mut value = n;
    while value >= 10 {
        value /= 10;
        digits = digits.saturating_add(1);
    }
    digits
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::arithmetic_side_effects,
        clippy::indexing_slicing
    )]

    use super::*;

    const MINIMAL: &[u8] = include_bytes!("../../selis-pdf-engine/src/fixtures/minimal.pdf");
    const NAV: &[u8] = include_bytes!("../../selis-pdf-engine/src/fixtures/nav.pdf");
    const TEXT: &[u8] = include_bytes!("../../selis-pdf-engine/src/fixtures/text.pdf");
    const FORM: &[u8] = include_bytes!("../../selis-pdf-engine/src/fixtures/form.pdf");
    const IMAGE_PDF: &[u8] = include_bytes!("../../selis-pdf-engine/src/fixtures/image.pdf");
    const INDIRECT: &[u8] =
        include_bytes!("../../selis-pdf-engine/src/fixtures/image-indirect-resources.pdf");

    fn guard() -> selis_sandbox::BudgetGuard<'static> {
        Budget::unlimited().guard()
    }

    fn solid(width: u32, height: u32, px: [u8; 4]) -> Image {
        let count = (width as usize) * (height as usize);
        Image {
            width,
            height,
            rgba: px.repeat(count),
        }
    }

    /// Opaque uniform noise: the densest AC energy an 8-bit image offers a block
    /// transform, and therefore the harshest honest input the coder can be handed.
    fn noise_image(width: u32, height: u32) -> Image {
        let mut state = 0x1234_5678u32;
        let mut rgba = Vec::new();
        for _ in 0..(width as usize).saturating_mul(height as usize) {
            for _ in 0..3 {
                state = state.wrapping_mul(1_103_515_245).wrapping_add(12_345);
                rgba.push(u8::try_from((state >> 16) & 0xFF).unwrap_or(0));
            }
            rgba.push(255);
        }
        Image {
            width,
            height,
            rgba,
        }
    }

    /// The mean and worst per-sample difference between the pixels handed to the
    /// JPEG writer and the file that comes back out of the decoder.
    fn jpeg_error(
        source: &[u8],
        bytes: &[u8],
        g: &mut selis_sandbox::BudgetGuard<'_>,
    ) -> (u64, u64) {
        let decoded = selis_image::decode_jpeg(bytes, g).expect("decode");
        let mut total = 0u64;
        let mut worst = 0u64;
        let mut samples = 0u64;
        for (index, px) in decoded.rgba.chunks_exact(4).enumerate() {
            for channel in 0..3usize {
                let want = source[index * 3 + channel];
                let got = px[channel];
                let difference = u64::from(
                    got.saturating_sub(want)
                        .saturating_add(want.saturating_sub(got)),
                );
                total = total.saturating_add(difference);
                worst = worst.max(difference);
                samples = samples.saturating_add(1);
            }
        }
        (total.saturating_div(samples.max(1)), worst)
    }

    /// The page's own pixels, as the chunk's render step produces them.
    fn page_image(
        input: &[u8],
        page: usize,
        dpi: u32,
    ) -> (Image, selis_sandbox::BudgetGuard<'static>) {
        let budget = Budget::profile(Surface::Batch);
        let clock = FixedClock(0);
        let session = Session::open(input.to_vec(), &budget, &clock).expect("open");
        let mut g = budget.guard_with(&clock, CancelToken::new());
        let pixels = session
            .render_page_rgba(page, f64::from(dpi), &budget, &mut g)
            .expect("render");
        drop(g);
        (
            Image {
                width: pixels.width,
                height: pixels.height,
                rgba: pixels.rgba,
            },
            guard(),
        )
    }

    #[test]
    fn chunk_id_is_raster() {
        assert_eq!(CHUNK_ID, "raster");
        assert_eq!(selis_raster_available(), 1);
    }

    #[test]
    fn one_page_is_a_png_that_decodes() {
        let (bytes, files) = run_impl(MINIMAL, "", 72, Format::Png, 0).expect("render");
        assert_eq!(files, 1);
        let image = selis_image::decode_png(&bytes, &mut guard()).expect("decode");
        assert_eq!((image.width, image.height), (100, 100));
        assert!(image
            .rgba
            .chunks_exact(4)
            .all(|px| matches!(px, &[_, _, _, 255])));
        let centre = (50 * 100 + 50) * 4;
        assert_eq!(
            image.rgba.get(centre..centre + 3).expect("centre pixel"),
            &[0, 0, 0]
        );
    }

    #[test]
    fn rendering_the_same_page_twice_is_byte_identical() {
        let (a, _) = run_impl(MINIMAL, "1", 96, Format::Png, 0).expect("first");
        let (b, _) = run_impl(MINIMAL, " 1 ", 96, Format::Png, 0).expect("second");
        assert_eq!(a, b);
    }

    #[test]
    fn png_ignores_the_quality_knob() {
        // The level PNG compresses at is this module's choice, not the user's,
        // so a nonsense quality must not change the file or raise an error.
        let (default, _) = run_impl(MINIMAL, "1", 96, Format::Png, 0).expect("zero");
        let (asked, _) = run_impl(MINIMAL, "1", 96, Format::Png, 999).expect("huge");
        assert_eq!(default, asked);
    }

    #[test]
    fn selection_parsing_is_one_based_and_ordered() {
        assert_eq!(parse_pages("", 3).unwrap(), vec![0, 1, 2]);
        assert_eq!(parse_pages("1-2,3", 3).unwrap(), vec![0, 1, 2]);
        assert_eq!(parse_pages("3,1", 3).unwrap(), vec![2, 0]);
        assert_eq!(parse_pages("2-2", 3).unwrap(), vec![1]);
        assert!(parse_pages("0", 3).is_err());
        assert!(parse_pages("4", 3).is_err());
        assert!(parse_pages("3-1", 3).is_err());
        assert!(parse_pages("x", 3).is_err());
    }

    #[test]
    fn dpi_is_bounded_and_reported() {
        assert!(run_impl(MINIMAL, "", 0, Format::Png, 0).is_err());
        assert!(run_impl(MINIMAL, "", 1000, Format::Png, 0).is_err());
    }

    #[test]
    fn several_pages_become_one_zip() {
        let (bytes, files) = run_impl(NAV, "", 36, Format::Png, 0).expect("render");
        assert_eq!(files, 2);
        assert_eq!(bytes.get(..2), Some(b"PK".as_slice()));
        assert!(bytes
            .windows(10)
            .any(|window| window == b"page-1.png".as_slice()));
        assert!(bytes
            .windows(10)
            .any(|window| window == b"page-2.png".as_slice()));
    }

    #[test]
    fn a_repeated_page_is_listed_once() {
        // The fixture has one page; asking for it twice must not duplicate it,
        // which is why the answer is a bare PNG rather than a ZIP.
        let (bytes, files) = run_impl(MINIMAL, "1,1", 36, Format::Png, 0).expect("render");
        assert_eq!(files, 1);
        assert_eq!(
            bytes.get(..8),
            Some([0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A].as_slice())
        );
        let err = run_impl(MINIMAL, "2", 36, Format::Png, 0).expect_err("out of range");
        assert!(err.contains("outside this document"), "{err}");
    }

    #[test]
    fn entry_names_zero_pad_to_the_page_count() {
        assert_eq!(decimal_width(1), 1);
        assert_eq!(decimal_width(9), 1);
        assert_eq!(decimal_width(10), 2);
        assert_eq!(decimal_width(100), 3);
    }

    #[test]
    fn format_codes_are_the_abi_numbers() {
        assert_eq!(Format::from_code(0).unwrap(), Format::Png);
        assert_eq!(Format::from_code(1).unwrap(), Format::Jpg);
        let err = Format::from_code(2).expect_err("unknown code");
        assert!(err.contains("unknown image format code 2"), "{err}");
        assert_eq!(Format::Png.extension(), "png");
        assert_eq!(Format::Jpg.extension(), "jpg");
    }

    #[test]
    fn one_page_is_a_jpeg_that_decodes() {
        let (bytes, files) = run_impl(MINIMAL, "", 72, Format::Jpg, 90).expect("render");
        assert_eq!(files, 1);
        assert_eq!(bytes.get(..2), Some([0xFF, 0xD8].as_slice()));
        assert_eq!(bytes.get(bytes.len() - 2..), Some([0xFF, 0xD9].as_slice()));
        let image = selis_image::decode_jpeg(&bytes, &mut guard()).expect("decode");
        assert_eq!((image.width, image.height), (100, 100));
    }

    #[test]
    fn jpeg_pages_are_named_by_the_format() {
        let (bytes, files) = run_impl(NAV, "", 36, Format::Jpg, 85).expect("render");
        assert_eq!(files, 2);
        assert_eq!(bytes.get(..2), Some(b"PK".as_slice()));
        assert!(bytes
            .windows(10)
            .any(|window| window == b"page-1.jpg".as_slice()));
        assert!(bytes
            .windows(10)
            .any(|window| window == b"page-2.jpg".as_slice()));
        assert!(
            !bytes
                .windows(10)
                .any(|window| window == b"page-1.png".as_slice()),
            "a JPEG run must not carry PNG entries"
        );
    }

    #[test]
    fn rendering_the_same_page_to_jpeg_twice_is_byte_identical() {
        let (a, _) = run_impl(MINIMAL, "1", 96, Format::Jpg, 75).expect("first");
        let (b, _) = run_impl(MINIMAL, " 1 ", 96, Format::Jpg, 75).expect("second");
        assert_eq!(a, b);
    }

    #[test]
    fn jpeg_quality_is_bounded_and_reported() {
        let low = run_impl(MINIMAL, "", 72, Format::Jpg, 0).expect_err("quality 0");
        assert!(
            low.contains("JPEG quality must be between 1 and 100"),
            "{low}"
        );
        let high = run_impl(MINIMAL, "", 72, Format::Jpg, 101).expect_err("quality 101");
        assert!(
            high.contains("JPEG quality must be between 1 and 100"),
            "{high}"
        );
    }

    #[test]
    fn a_damaged_scan_is_refused_by_verification() {
        // The witness that the bounds are not decoration: one wrong byte inside
        // the entropy-coded segment of an honest 1.7-megapixel page. The mean
        // barely moves — the damage is local — so it is the sample ceiling, and
        // the decoder's own parsing, that refuse the file.
        let (image, mut g) = page_image(TEXT, 0, 150);
        let honest = encode_jpeg(&image, 90, &mut g).expect("encode");
        let sos = honest
            .windows(2)
            .rposition(|w| w == [0xFF, 0xDA])
            .expect("SOS");
        // Past the marker, its length, the sample-count byte and one 3-byte
        // component triplet, plus the trailing SOS parameters.
        let scan_start = sos.saturating_add(2 + 6 + 3 * 3 + 3);
        let body = honest.len().saturating_sub(scan_start);
        for fraction in [4usize, 8, 16, 64] {
            let offset = scan_start.saturating_add(body.saturating_div(fraction));
            let mut damaged = honest.clone();
            if let Some(byte) = damaged.get_mut(offset) {
                *byte ^= 0x55;
            }
            let outcome = verify_jpg(&image, &damaged, 90, &mut g, 0);
            assert!(
                outcome.is_err(),
                "damage at byte {offset} of the scan passed verification"
            );
        }
    }

    #[test]
    fn the_harsh_honest_content_stays_under_the_bounds_it_is_given() {
        // The constants quote a measurement, so the measurement lives here:
        // uniform noise is the worst case a block transform has, and these are
        // the numbers the bounds were set against.
        let noise = noise_image(256, 256);
        let mut g = guard();
        let source = selis_image::flatten_over_white(&noise.rgba, &mut g).expect("flatten");
        for (quality, expected_mean, expected_worst) in
            [(1u8, 62u64, 255u64), (50, 28, 203), (90, 6, 45)]
        {
            let bytes = encode_jpeg(&noise, quality, &mut g).expect("encode");
            let (mean, worst) = jpeg_error(&source, &bytes, &mut g);
            assert_eq!(
                (mean, worst),
                (expected_mean, expected_worst),
                "noise at quality {quality}: the bound docs quote these numbers"
            );
            assert!(
                mean <= JPG_MAX_MEAN_ERROR,
                "noise at quality {quality} averaged {mean}, over the bound"
            );
            if quality >= JPG_SAMPLE_BOUND_MIN_QUALITY {
                assert!(
                    worst <= JPG_MAX_SAMPLE_ERROR,
                    "noise at quality {quality} peaked at {worst}, over the bound"
                );
            }
        }
    }

    #[test]
    fn an_honest_page_clears_the_jpeg_verification_bounds() {
        // The constants are a contract with the measurements in their docs: every
        // fixture, at every quality the tool offers, must survive its own
        // verification. If this fails, the bound is wrong or the encoder is — the
        // assertion is not what needs widening.
        let fixtures = [
            ("minimal", MINIMAL),
            ("text", TEXT),
            ("nav", NAV),
            ("form", FORM),
            ("image", IMAGE_PDF),
            ("indirect", INDIRECT),
        ];
        for (name, input) in fixtures {
            let (image, mut g) = page_image(input, 0, 72);
            for quality in [1u8, 10, 25, 50, 75, 90, 100] {
                let bytes = encode_jpeg(&image, quality, &mut g)
                    .unwrap_or_else(|e| panic!("{name} at 72 DPI quality {quality}: {e}"));
                if let Err(e) = verify_jpg(&image, &bytes, quality, &mut g, 0) {
                    panic!("{name} at 72 DPI quality {quality} refused its own page: {e}");
                }
            }
        }
    }

    #[test]
    fn a_megapixel_text_page_clears_the_jpeg_verification_bounds() {
        // The harshest honest case in the corpus: 1.7 megapixels of small
        // black-on-white glyph edges, where JPEG ringing peaked at 87 levels and
        // the mean stayed under 1.
        let (image, mut g) = page_image(TEXT, 0, 150);
        for quality in [1u8, 25, 50, 90, 100] {
            let bytes = encode_jpeg(&image, quality, &mut g).expect("encode");
            if let Err(e) = verify_jpg(&image, &bytes, quality, &mut g, 0) {
                panic!("the 150 DPI text page refused itself at quality {quality}: {e}");
            }
        }
    }

    #[test]
    fn check_jpeg_is_verifiable_holds_at_the_reader_s_limit() {
        let at_the_limit = solid(MAX_JPEG_EDGE, 8, [0, 0, 0, 255]);
        assert!(check_jpeg_is_verifiable(&at_the_limit, 300, 0).is_ok());
        let too_tall = solid(8, MAX_JPEG_EDGE.saturating_add(1), [0, 0, 0, 255]);
        assert!(check_jpeg_is_verifiable(&too_tall, 300, 0).is_err());
    }

    #[test]
    fn a_page_wider_than_the_reader_is_refused_before_any_file_is_written() {
        // A banner page: 17,000 px across at 72 DPI is a 680-kilopixel canvas, so
        // the render is cheap and the JPEG it would produce is one this device
        // cannot open again. The run must say so instead of shipping it, and PNG
        // — whose reader is ours — must still work.
        let wide = wide_page_pdf(17_000.0, 40.0);
        let err = run_impl(&wide, "", 72, Format::Jpg, 80).expect_err("oversized JPEG");
        assert!(err.contains("17000x40"), "{err}");
        assert!(err.contains("cannot be read back"), "{err}");
        assert!(err.contains("PDF to PNG"), "{err}");
        let (bytes, files) = run_impl(&wide, "", 72, Format::Png, 0).expect("png still works");
        assert_eq!(files, 1);
        assert_eq!(
            bytes.get(..8),
            Some([0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A].as_slice())
        );
    }

    /// A one-page document of the given size in points, with an empty content
    /// stream: the page is a blank canvas, which is all a raster test needs.
    /// Written by hand — the chunk has no business depending on the COS writer.
    fn wide_page_pdf(width: f64, height: f64) -> Vec<u8> {
        let objects: [Vec<u8>; 4] = [
            b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n".to_vec(),
            b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n".to_vec(),
            format!(
                "3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 {width} {height}] \
                 /Resources << >> /Contents 4 0 R >>\nendobj\n"
            )
            .into_bytes(),
            b"4 0 obj\n<< /Length 0 >>\nstream\n\nendstream\nendobj\n".to_vec(),
        ];
        let mut out = b"%PDF-1.4\n".to_vec();
        let mut offsets = Vec::new();
        for object in &objects {
            offsets.push(out.len());
            out.extend_from_slice(object);
        }
        let xref = out.len();
        let size = offsets.len().saturating_add(1);
        out.extend_from_slice(format!("xref\n0 {size}\n").as_bytes());
        out.extend_from_slice(b"0000000000 65535 f \n");
        for offset in &offsets {
            out.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
        }
        out.extend_from_slice(
            format!("trailer\n<< /Size {size} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n")
                .as_bytes(),
        );
        out
    }

    #[test]
    fn verify_jpg_refuses_a_file_that_is_not_the_page() {
        // A black page against an honest white JPEG: the mean is enormous, and
        // this is the witness that the check is wired to something real.
        let page = solid(8, 8, [0, 0, 0, 255]);
        let other = solid(8, 8, [255, 255, 255, 255]);
        let bytes = encode_jpeg(&other, 90, &mut guard()).expect("encode");
        let err = verify_jpg(&page, &bytes, 90, &mut guard(), 0).expect_err("must refuse");
        assert!(err.contains("differs from the page"), "{err}");
        assert!(err.contains("on average"), "{err}");
    }

    #[test]
    fn verify_jpg_refuses_a_file_of_the_wrong_size() {
        let page = solid(8, 8, [10, 20, 30, 255]);
        let small = solid(4, 4, [10, 20, 30, 255]);
        let bytes = encode_jpeg(&small, 90, &mut guard()).expect("encode");
        let err = verify_jpg(&page, &bytes, 90, &mut guard(), 3).expect_err("must refuse");
        assert!(err.contains("the JPEG is 4x4"), "{err}");
        assert!(err.contains("page 4"), "{err}");
    }

    #[test]
    fn verify_jpg_refuses_a_few_badly_wrong_pixels() {
        // One blazing pixel in a dark page: averaged over the frame it is
        // nothing, which is exactly the failure the sample bound exists for.
        let page = solid(8, 8, [0, 0, 0, 255]);
        let mut burned = solid(8, 8, [0, 0, 0, 255]);
        for channel in 0..4 {
            burned.rgba[channel] = 255;
        }
        let bytes = encode_jpeg(&burned, 100, &mut guard()).expect("encode");
        let err = verify_jpg(&page, &bytes, 100, &mut guard(), 0).expect_err("must refuse");
        assert!(
            err.contains("a JPEG sample differs from the page by"),
            "{err}"
        );
    }

    #[test]
    fn verify_jpg_says_nothing_about_ringing_at_the_coarsest_quality() {
        // Below JPG_SAMPLE_BOUND_MIN_QUALITY an honest page rings past the
        // sample ceiling on its own, so the ceiling is not applied there — the
        // same file must pass at quality 1 and refuse at quality 100.
        let page = solid(8, 8, [0, 0, 0, 255]);
        let mut burned = solid(8, 8, [0, 0, 0, 255]);
        for channel in 0..4 {
            burned.rgba[channel] = 255;
        }
        let bytes = encode_jpeg(&burned, 1, &mut guard()).expect("encode");
        verify_jpg(&page, &bytes, 1, &mut guard(), 0).expect("quality 1 is exempt");
    }

    #[test]
    fn verify_png_refuses_a_file_that_is_not_the_page() {
        let page = solid(8, 8, [10, 20, 30, 255]);
        let other = solid(8, 8, [200, 128, 64, 255]);
        let bytes = encode_png(&other, PNG_LEVEL, &mut guard()).expect("encode");
        let err = verify_png(&page, &bytes, &mut guard(), 0).expect_err("must refuse");
        assert!(err.contains("the PNG does not round-trip"), "{err}");
    }
}
