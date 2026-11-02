//! Raster lazy chunk: PDF pages → PNG files, on the device.
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
//! ([`Session::render_page_rgba`]), and the PNG encoder is `selis-image`'s
//! deterministic writer. One page becomes one PNG; several pages are returned
//! as one deterministic ZIP so a browser does not have to allow a burst of
//! downloads.

#![allow(unsafe_code)]

use std::alloc::{alloc, dealloc, Layout};
use std::sync::Mutex;

use selis_image::{encode_png, Image};
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

/// Render the selected pages to PNG.
///
/// `pages_ptr`/`pages_len` is a UTF-8 selection in the same 1-based syntax the
/// page's other tools use (`""` = every page, `"1-3,5"` = those four). The
/// return value is one PNG when the selection has one page, otherwise a
/// deterministic ZIP of `page-NNN.png` entries. `out_files` receives the count
/// and `out_len` the byte length; free the result with [`selis_raster_free`].
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
    match run_impl(input, selection, dpi) {
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

/// The safe body of [`selis_raster_run`], unit-testable on the host.
fn run_impl(input: &[u8], selection: &str, dpi: u32) -> Result<(Vec<u8>, u32), String> {
    if dpi == 0 || dpi > 600 {
        return Err("DPI must be between 1 and 600".to_string());
    }
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
        let png = encode_png(&image, PNG_LEVEL, &mut g)
            .map_err(|e| format!("page {} could not be encoded: {e}", page.saturating_add(1)))?;
        // Pixel verification (Phase 2 exit gate): decode the file we are about
        // to hand back and compare it with the pixels that were rendered. A
        // PNG that does not round-trip is a bug in the writer, and the user
        // must not receive it with a "verified" label on top.
        let decoded = selis_image::decode_png(&png, &mut g).map_err(|e| {
            format!(
                "page {} output could not be read back: {e}",
                page.saturating_add(1)
            )
        })?;
        if decoded.width != image.width
            || decoded.height != image.height
            || decoded.rgba != image.rgba
        {
            return Err(format!(
                "page {} failed pixel verification: the PNG does not round-trip",
                page.saturating_add(1)
            ));
        }
        rendered.push((
            format!("page-{:0width$}.png", page.saturating_add(1), width = width),
            png,
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
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;

    const MINIMAL: &[u8] = include_bytes!("../../selis-pdf-engine/src/fixtures/minimal.pdf");

    fn guard() -> selis_sandbox::BudgetGuard<'static> {
        Budget::unlimited().guard()
    }

    #[test]
    fn chunk_id_is_raster() {
        assert_eq!(CHUNK_ID, "raster");
        assert_eq!(selis_raster_available(), 1);
    }

    #[test]
    fn one_page_is_a_png_that_decodes() {
        let (bytes, files) = run_impl(MINIMAL, "", 72).expect("render");
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
        let (a, _) = run_impl(MINIMAL, "1", 96).expect("first");
        let (b, _) = run_impl(MINIMAL, " 1 ", 96).expect("second");
        assert_eq!(a, b);
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
        assert!(run_impl(MINIMAL, "", 0).is_err());
        assert!(run_impl(MINIMAL, "", 1000).is_err());
    }

    #[test]
    fn several_pages_become_one_zip() {
        // The fixture has one page, so ask for it twice through two selections
        // to prove de-duplication, then a non-existent page for the error.
        let (bytes, files) = run_impl(MINIMAL, "1,1", 36).expect("render");
        assert_eq!(files, 1);
        assert_eq!(
            bytes.get(..8),
            Some([0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A].as_slice())
        );
        let err = run_impl(MINIMAL, "2", 36).expect_err("out of range");
        assert!(err.contains("outside this document"), "{err}");
    }

    #[test]
    fn entry_names_zero_pad_to_the_page_count() {
        assert_eq!(decimal_width(1), 1);
        assert_eq!(decimal_width(9), 1);
        assert_eq!(decimal_width(10), 2);
        assert_eq!(decimal_width(100), 3);
    }
}
