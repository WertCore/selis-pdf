//! Convert lazy chunk (SL-4.WASM.02).
//!
//! Separate `cdylib` from the core viewer/tools. The viewer never downloads
//! this module; it is fetched when the user invokes Markdown/HTML/CSV → PDF
//! conversion. This chunk speaks a small, self-contained ABI: one entry point
//! turns an input document into PDF bytes and reports the page count, plus an
//! error slot the caller reads on failure.

#![allow(unsafe_code)]

use std::alloc::{alloc, dealloc, Layout};
use std::sync::Mutex;

use selis_pdf_convert::{
    csv_to_pdf, docx_to_pdf, epub_to_pdf, html_to_pdf, latex_to_pdf, markdown_to_pdf, pptx_to_pdf,
    svg_to_pdf, xlsx_to_pdf, Options, PageSize,
};

use selis_sandbox::{Budget, CancelToken, FixedClock};

/// Chunk identifier (stable, matches the manifest).
pub const CHUNK_ID: &str = "convert";
/// Chunk version.
pub const CHUNK_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Input format selector.
pub const FORMAT_MARKDOWN: u32 = 0;
/// HTML input.
pub const FORMAT_HTML: u32 = 1;
/// CSV input.
pub const FORMAT_CSV: u32 = 2;
/// LaTeX input.
pub const FORMAT_LATEX: u32 = 3;
/// EPUB input (a binary ZIP package; the text path is bypassed).
pub const FORMAT_EPUB: u32 = 4;
/// SVG input (the documented subset in `selis-pdf-convert::svg`).
pub const FORMAT_SVG: u32 = 5;
/// Word input (a binary ZIP package; the text path is bypassed).
pub const FORMAT_DOCX: u32 = 6;
/// PowerPoint input (a binary ZIP package; the text path is bypassed).
pub const FORMAT_PPTX: u32 = 7;
/// Excel input (a binary ZIP package; the text path is bypassed).
pub const FORMAT_XLSX: u32 = 8;

/// Page size selector.
pub const PAGE_LETTER: u32 = 0;
/// A4 page size.
pub const PAGE_A4: u32 = 1;

/// The last error message. A `Mutex` rather than a `static mut`: the page calls
/// this module from one thread, but a shared reference to a mutable static is
/// undefined behaviour regardless of how it is used, and the compiler is right
/// to say so.
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

/// Allocate guest memory for an output buffer.
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
pub unsafe extern "C" fn selis_convert_chunk_id(out_len: *mut u32) -> *mut u8 {
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
///
/// The page writes the input document and the two output slots into this
/// memory, so the chunk has to hand out raw allocations: `selis_convert_run`
/// takes pointers, not a copy. Returns null on a zero or impossible request,
/// which the caller must treat as a failure rather than a valid pointer.
#[no_mangle]
pub extern "C" fn selis_convert_alloc(len: u32) -> *mut u8 {
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
/// `ptr`/`len` must identify a live allocation from this module, freed exactly once.
#[no_mangle]
pub unsafe extern "C" fn selis_convert_free(ptr: *mut u8, len: u32) {
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

/// Returns the last error message (if any) as guest memory; freed with
/// `selis_convert_free`. Writes its length to `out_len`.
///
/// # Safety
/// `out_len` must be null or point to a writable `u32` in this module's address space.
#[no_mangle]
pub unsafe extern "C" fn selis_convert_last_error(out_len: *mut u32) -> *mut u8 {
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

/// Capability query: which converters this chunk offers (bitmask).
/// Bit 0: Markdown, Bit 1: HTML, Bit 2: CSV, Bit 3: LaTeX, Bit 4: EPUB,
/// Bit 5: SVG, Bit 6: Word, Bit 7: PowerPoint, Bit 8: Excel.
#[no_mangle]
pub extern "C" fn selis_convert_capabilities() -> u32 {
    0b1_1111_1111
}

/// Availability probe.
#[no_mangle]
pub extern "C" fn selis_convert_available() -> u32 {
    1
}

/// Convert one input document into PDF bytes.
///
/// `format`: 0 = Markdown, 1 = HTML, 2 = CSV, 3 = LaTeX, 4 = EPUB, 5 = SVG,
/// 6 = Word, 7 = PowerPoint, 8 = Excel.
/// `page_size`: 0 = Letter, 1 = A4.
/// `title_ptr`/`title_len`: optional title (may be null/0).
///
/// Returns a pointer to the PDF bytes on success (free with
/// `selis_convert_free` using `*out_len`), or null on failure (read
/// `selis_convert_last_error`).
///
/// # Safety
/// `input_ptr`/`input_len` and `title_ptr`/`title_len` must describe live
/// buffers in this module's address space for the duration of the call, and
/// `out_len` must be null or point to a writable `u32`.
#[no_mangle]
pub unsafe extern "C" fn selis_convert_run(
    format: u32,
    input_ptr: *const u8,
    input_len: u32,
    page_size: u32,
    title_ptr: *const u8,
    title_len: u32,
    out_len: *mut u32,
    out_pages: *mut u32,
) -> *mut u8 {
    clear_error();
    if out_len.is_null() || out_pages.is_null() {
        set_error("missing output slot".to_string());
        return std::ptr::null_mut();
    }
    if input_ptr.is_null() || input_len == 0 {
        set_error("empty input".to_string());
        return std::ptr::null_mut();
    }
    let input = unsafe { std::slice::from_raw_parts(input_ptr, input_len as usize) };
    let text = match format {
        FORMAT_EPUB | FORMAT_DOCX | FORMAT_PPTX | FORMAT_XLSX => String::new(),
        _ => match std::str::from_utf8(input) {
            Ok(value) => value.to_string(),
            Err(_) => {
                set_error("input is not valid UTF-8".to_string());
                return std::ptr::null_mut();
            }
        },
    };
    let title = if title_ptr.is_null() || title_len == 0 {
        None
    } else {
        let slice = unsafe { std::slice::from_raw_parts(title_ptr, title_len as usize) };
        std::str::from_utf8(slice).ok().map(|s| s.to_string())
    };

    let ps = if page_size == PAGE_A4 {
        PageSize::A4
    } else {
        PageSize::Letter
    };

    let budget = Budget::unlimited();
    let mut guard = budget.guard_with(&FixedClock(0), CancelToken::new());

    let opts = Options {
        page_size: ps,
        title,
    };

    let bytes = match format {
        FORMAT_MARKDOWN => markdown_to_pdf(&text, &opts, &budget, &mut guard),
        FORMAT_HTML => html_to_pdf(&text, &opts, &budget, &mut guard),
        FORMAT_CSV => csv_to_pdf(&text, &opts, &budget, &mut guard),
        FORMAT_LATEX => latex_to_pdf(&text, &opts, &budget, &mut guard),
        FORMAT_EPUB => epub_to_pdf(input, &opts, &budget, &mut guard),
        FORMAT_SVG => svg_to_pdf(&text, &opts, &budget, &mut guard),
        FORMAT_DOCX => docx_to_pdf(input, &opts, &budget, &mut guard),
        FORMAT_PPTX => pptx_to_pdf(input, &opts, &budget, &mut guard),
        FORMAT_XLSX => xlsx_to_pdf(input, &opts, &budget, &mut guard),
        _ => {
            set_error("unknown format".to_string());
            return std::ptr::null_mut();
        }
    };

    match bytes {
        Ok(data) => {
            // Count pages by scanning the output for /Type /Page (rough but
            // cheap; the structural gate on the writer already validated).
            let page_count = count_pages(&data);
            let Ok(len_u32) = u32::try_from(data.len()) else {
                set_error("output too large".to_string());
                return std::ptr::null_mut();
            };
            let ptr = alloc_bytes(&data);
            if ptr.is_null() {
                set_error("out of memory".to_string());
                return std::ptr::null_mut();
            }
            unsafe {
                out_len.write(len_u32);
                out_pages.write(page_count);
            }
            ptr
        }
        Err(e) => {
            set_error(e.to_string());
            std::ptr::null_mut()
        }
    }
}

#[allow(clippy::arithmetic_side_effects, clippy::indexing_slicing)]
fn count_pages(data: &[u8]) -> u32 {
    // Count occurrences of "/Type /Page" (page objects, not /Pages).
    let needle = b"/Type /Page";
    let mut count = 0u32;
    let mut i = 0;
    while i + needle.len() <= data.len() {
        if &data[i..i + needle.len()] == needle {
            // Make sure it is not "/Type /Pages".
            let next = data.get(i + needle.len()).copied().unwrap_or(0);
            if next != b's' {
                count = count.saturating_add(1);
            }
            i += needle.len();
        } else {
            i += 1;
        }
    }
    if count == 0 {
        1
    } else {
        count
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn chunk_id_is_convert() {
        assert_eq!(CHUNK_ID, "convert");
    }
    #[test]
    fn caps_cover_all_current_formats() {
        assert_eq!(selis_convert_capabilities(), 0b1_1111_1111);
    }
}
