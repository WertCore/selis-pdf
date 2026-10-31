//! Convert lazy chunk (SL-4.WASM.02).
//!
//! Separate `cdylib` from the core viewer/tools. The viewer never downloads
//! this module; it is fetched when the user invokes Markdown/HTML/CSV → PDF
//! conversion. This chunk speaks a small, self-contained ABI: one entry point
//! turns an input document into PDF bytes and reports the page count, plus an
//! error slot the caller reads on failure.

#![allow(unsafe_code)]

use std::alloc::{alloc, dealloc, Layout};

use selis_bytes::Bytes;
use selis_error::Result;
use selis_pdf_convert::{html_to_pdf, markdown_to_pdf, Options, PageSize};
use selis_pdf_cos::doc_writer::DocumentBuilder;
use selis_pdf_cos::{Obj, Ref};
use selis_sandbox::{Budget, BudgetGuard, CancelToken, FixedClock};

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

/// Page size selector.
pub const PAGE_LETTER: u32 = 0;
/// A4 page size.
pub const PAGE_A4: u32 = 1;

static mut LAST_ERROR: Option<String> = None;

fn set_error(message: String) {
    // SAFETY: single-threaded wasm; no reentrancy across the boundary.
    unsafe { LAST_ERROR = Some(message) };
}

fn clear_error() {
    unsafe { LAST_ERROR = None };
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
#[no_mangle]
pub unsafe extern "C" fn selis_convert_last_error(out_len: *mut u32) -> *mut u8 {
    if out_len.is_null() {
        return std::ptr::null_mut();
    }
    let message = unsafe { LAST_ERROR.clone() }.unwrap_or_default();
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
/// Bit 0: Markdown, Bit 1: HTML, Bit 2: CSV.
#[no_mangle]
pub extern "C" fn selis_convert_capabilities() -> u32 {
    0b111
}

/// Availability probe.
#[no_mangle]
pub extern "C" fn selis_convert_available() -> u32 {
    1
}

/// Convert one input document into PDF bytes.
///
/// `format`: 0 = Markdown, 1 = HTML, 2 = CSV.
/// `page_size`: 0 = Letter, 1 = A4.
/// `title_ptr`/`title_len`: optional title (may be null/0).
///
/// Returns a pointer to the PDF bytes on success (free with
/// `selis_convert_free` using `*out_len`), or null on failure (read
/// `selis_convert_last_error`).
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
    let text = match std::str::from_utf8(input) {
        Ok(value) => value,
        Err(_) => {
            set_error("input is not valid UTF-8".to_string());
            return std::ptr::null_mut();
        }
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
        FORMAT_MARKDOWN => markdown_to_pdf(text, &opts, &budget, &mut guard),
        FORMAT_HTML => html_to_pdf(text, &opts, &budget, &mut guard),
        FORMAT_CSV => csv_to_pdf(text, &opts, &budget, &mut guard),
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

/// A minimal CSV → PDF table renderer. Splits on commas (with simple quoted-
/// field support), sizes columns evenly, and paginates with a repeated header
/// row. No claim of spreadsheet fidelity: this is a table dump, nothing more.
fn csv_to_pdf(
    input: &str,
    opts: &Options,
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
) -> Result<Vec<u8>> {
    use selis_pdf_cos::doc_writer::ContentBuilder;

    let rows = parse_csv(input);
    let cols = rows.iter().map(Vec::len).max().unwrap_or(0);
    if cols == 0 {
        // Empty input: emit a single blank page.
        let (page_w, page_h) = opts.page_size.dims();
        let mut b = DocumentBuilder::new();
        b.add_page(page_w, page_h, &[]);
        return b.write(budget, g);
    }

    let (page_w, page_h) = opts.page_size.dims();
    let margin = 54.0;
    let avail_w = page_w - 2.0 * margin;
    let col_w = avail_w / (cols as f64);
    let row_h = 14.0;
    let font_size = 9.0;

    let mut b = DocumentBuilder::new();
    if let Some(title) = &opts.title {
        if !title.is_empty() {
            b.set_info(b"Title", title);
        }
    }

    // Font resources: Helvetica + Helvetica-Bold.
    let f_regular = take_num(&mut b);
    b.add_object(
        f_regular,
        Obj::Dict(vec![
            (bytes(b"Type"), Obj::Name(bytes(b"Font"))),
            (bytes(b"Subtype"), Obj::Name(bytes(b"Type1"))),
            (bytes(b"BaseFont"), Obj::Name(bytes(b"Helvetica"))),
            (bytes(b"Encoding"), Obj::Name(bytes(b"WinAnsiEncoding"))),
        ]),
    );
    let f_bold = take_num(&mut b);
    b.add_object(
        f_bold,
        Obj::Dict(vec![
            (bytes(b"Type"), Obj::Name(bytes(b"Font"))),
            (bytes(b"Subtype"), Obj::Name(bytes(b"Type1"))),
            (bytes(b"BaseFont"), Obj::Name(bytes(b"Helvetica-Bold"))),
            (bytes(b"Encoding"), Obj::Name(bytes(b"WinAnsiEncoding"))),
        ]),
    );
    let res_num = take_num(&mut b);
    b.add_object(
        res_num,
        Obj::Dict(vec![(
            bytes(b"Font"),
            Obj::Dict(vec![
                (bytes(b"F1"), Obj::Ref(Ref::new(f_regular, 0))),
                (bytes(b"F2"), Obj::Ref(Ref::new(f_bold, 0))),
            ]),
        )]),
    );
    let res_ref = Ref::new(res_num, 0);

    // We'll accumulate lines into a list of (is_header, cells) and paginate.
    let mut pages: Vec<Vec<(bool, Vec<String>)>> = Vec::new();
    let mut current: Vec<(bool, Vec<String>)> = Vec::new();
    let rows_per_page = ((page_h - 2.0 * margin) / row_h).floor() as usize;
    let rows_per_page = if rows_per_page == 0 { 1 } else { rows_per_page };
    for (i, row) in rows.iter().enumerate() {
        let is_header = i == 0;
        if current.len() >= rows_per_page && !current.is_empty() {
            pages.push(std::mem::take(&mut current));
            if i > 0 {
                current.push((true, rows[0].clone()));
            }
        }
        current.push((is_header, row.clone()));
    }
    if !current.is_empty() {
        pages.push(current);
    }
    if pages.is_empty() {
        pages.push(Vec::new());
    }

    // Now render pages.
    for page in &pages {
        let mut cb = ContentBuilder::new();
        let mut yy = page_h - margin;
        for (is_header, cells) in page {
            for (ci, cell) in cells.iter().enumerate() {
                let x = margin + (ci as f64) * col_w + 2.0;
                let font = if *is_header { "F2" } else { "F1" };
                // truncate
                let max_chars = ((col_w - 4.0) / (font_size * 0.5)).max(1.0) as usize;
                let display: String = cell.chars().take(max_chars).collect();
                cb.begin_text()
                    .set_font(font, font_size)
                    .set_text_matrix(x, yy)
                    .show_text(&display)
                    .end_text();
            }
            yy -= row_h;
        }
        let data = cb.to_bytes();
        let content_num = take_num(&mut b);
        b.add_object(
            content_num,
            Obj::Stream {
                dict: vec![(
                    bytes(b"Length"),
                    Obj::Int(i64::try_from(data.len()).unwrap_or(i64::MAX)),
                )],
                data: Bytes::from(data),
            },
        );
        b.add_page_with(page_w, page_h, &[Ref::new(content_num, 0)], Some(res_ref));
    }

    g.charge(selis_sandbox::Resource::Objects, 10)?;
    b.write(budget, g)
}

fn parse_csv(input: &str) -> Vec<Vec<String>> {
    let mut rows: Vec<Vec<String>> = Vec::new();
    for line in input.lines() {
        if line.trim().is_empty() {
            continue;
        }
        rows.push(split_csv_line(line));
    }
    rows
}

fn split_csv_line(line: &str) -> Vec<String> {
    let mut fields = Vec::new();
    let mut current = String::new();
    let mut in_quotes = false;
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        if in_quotes {
            if c == '"' {
                if chars.peek() == Some(&'"') {
                    current.push('"');
                    chars.next();
                } else {
                    in_quotes = false;
                }
            } else {
                current.push(c);
            }
        } else if c == '"' {
            in_quotes = true;
        } else if c == ',' {
            fields.push(std::mem::take(&mut current));
        } else {
            current.push(c);
        }
    }
    fields.push(current);
    fields
}

fn take_num(b: &mut DocumentBuilder) -> u32 {
    let n = *b.next_num_mut();
    *b.next_num_mut() = n.saturating_add(1);
    n
}

fn bytes(v: &[u8]) -> selis_bytes::Bytes {
    selis_bytes::Bytes::copy_from_slice(v)
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
        assert_eq!(selis_convert_capabilities(), 0b111);
    }
}
