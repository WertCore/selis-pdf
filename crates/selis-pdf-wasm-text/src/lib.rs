//! Text lazy chunk: extraction with its quality report, tables, split at a text
//! boundary, and reflow — all on the device (Phase 3).
//!
//! A separate `cdylib` from the viewer, the tools, convert, raster, HTML,
//! security and sign chunks. The tools page downloads it on demand and the
//! service worker caches it. Like every chunk before it, it declares **no
//! imports**: the page instantiates it with an empty import object, which is the
//! mechanical form of "this module cannot call out".
//!
//! # Why a chunk of its own
//!
//! Text extraction links the engine's display list, Unicode recovery, line
//! assembly and reading order. The plain tools chunk deliberately does not link
//! the engine — that is what keeps it under half a megabyte — so the text
//! surfaces ride here, behind the `text` feature of `selis-pdf-tools`. The
//! membrane between the two is the feature flag: the tools chunk cannot reach
//! this code and this chunk cannot reach the writer.
//!
//! # Response format
//!
//! ```text
//! magic "SLTX" (u32)
//! status (u32: 0 ok, 1 error)
//! message (u32 length + bytes; the failure sentence, or empty)
//! output_count (u32)
//!   per output: name (len+bytes), kind (len+bytes), data (len+bytes)
//! ```
//!
//! The single output that is not a PDF is the point: a text export produces a
//! `.txt` and a quality `.json` in one call, so the page neither runs the
//! document twice nor reconstructs the export itself.

#![allow(unsafe_code)]

use std::alloc::{alloc, dealloc, Layout};
use std::sync::Mutex;

/// Chunk identifier used by the asset manifest.
pub const CHUNK_ID: &str = "text";
/// Chunk version.
pub const CHUNK_VERSION: &str = env!("CARGO_PKG_VERSION");
/// Response magic, `SLTX`.
const RESPONSE_MAGIC: u32 = 0x5854_4C53;
/// The most outputs one run may return (a split's parts plus its report).
const MAX_OUTPUTS: usize = 65;

/// One output the framed response carries: download name, kind tag and bytes.
type Output = (String, &'static str, Vec<u8>);

/// Every output one run produced, in the order the card offers them.
type Outputs = Vec<Output>;

/// Operations understood by this chunk.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Op {
    /// Text with a separator per page, plus the quality report.
    Text,
    /// Table reconstruction as CSV, plus the table report.
    Tables,
    /// Split at every page whose text matches, bundled as one ZIP.
    Split,
    /// Reading-order HTML article.
    Reflow,
    /// The same article as an EPUB 3 package.
    Epub,
    /// The same article as a Word document.
    Word,
    /// One slide per page with positioned text boxes.
    Slides,
    /// Reconstructed tables as an Excel workbook.
    Workbook,
}

impl Op {
    fn from_wire(value: u32) -> Option<Self> {
        match value {
            1 => Some(Self::Text),
            2 => Some(Self::Tables),
            3 => Some(Self::Split),
            4 => Some(Self::Reflow),
            5 => Some(Self::Epub),
            6 => Some(Self::Word),
            7 => Some(Self::Slides),
            8 => Some(Self::Workbook),
            _ => None,
        }
    }
}

/// The last error message.
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
    // SAFETY: the layout has a non-zero size, as `alloc` requires.
    let ptr = unsafe { alloc(layout) };
    if ptr.is_null() {
        return std::ptr::null_mut();
    }
    // SAFETY: `ptr` holds `data.len()` writable bytes.
    unsafe { std::ptr::copy_nonoverlapping(data.as_ptr(), ptr, data.len()) };
    ptr
}

/// Write one length-prefixed block.
fn put_block(out: &mut Vec<u8>, bytes: &[u8]) -> Result<(), String> {
    let len = u32::try_from(bytes.len()).map_err(|_| "output too large".to_string())?;
    out.extend_from_slice(&len.to_le_bytes());
    out.extend_from_slice(bytes);
    Ok(())
}

/// Frame one successful run: every output named and typed.
fn frame(outputs: &[Output]) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    out.extend_from_slice(&RESPONSE_MAGIC.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    put_block(&mut out, b"")?;
    let count = u32::try_from(outputs.len()).map_err(|_| "too many outputs".to_string())?;
    out.extend_from_slice(&count.to_le_bytes());
    for (name, kind, data) in outputs {
        put_block(&mut out, name.as_bytes())?;
        put_block(&mut out, kind.as_bytes())?;
        put_block(&mut out, data)?;
    }
    Ok(out)
}

/// Frame one failure: the reason, and no output.
fn frame_error(message: &str) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&RESPONSE_MAGIC.to_le_bytes());
    out.extend_from_slice(&1u32.to_le_bytes());
    let payload = message.as_bytes();
    let len = u32::try_from(payload.len()).unwrap_or(0);
    out.extend_from_slice(&len.to_le_bytes());
    out.extend_from_slice(payload);
    out.extend_from_slice(&0u32.to_le_bytes());
    out
}

/// The chunk id as guest memory. The host reads `out_len` and frees the pointer
/// with [`selis_text_free`].
///
/// # Safety
/// `out_len` must point to four writable bytes in guest memory.
#[no_mangle]
pub unsafe extern "C" fn selis_text_chunk_id(out_len: *mut u32) -> *mut u8 {
    if out_len.is_null() {
        return std::ptr::null_mut();
    }
    let bytes = CHUNK_ID.as_bytes();
    let Ok(len) = u32::try_from(bytes.len()) else {
        return std::ptr::null_mut();
    };
    let ptr = alloc_bytes(bytes);
    if ptr.is_null() {
        return ptr;
    }
    // SAFETY: the caller promised four writable bytes.
    unsafe { out_len.write(len) };
    ptr
}

/// Allocate `len` bytes for the host to fill with a request.
#[no_mangle]
pub extern "C" fn selis_text_alloc(len: u32) -> *mut u8 {
    let Ok(size) = usize::try_from(len) else {
        return std::ptr::null_mut();
    };
    if size == 0 {
        return std::ptr::null_mut();
    }
    let Ok(layout) = Layout::from_size_align(size, 1) else {
        return std::ptr::null_mut();
    };
    // SAFETY: the layout has a non-zero size.
    unsafe { alloc(layout) }
}

/// # Safety
/// `ptr`/`len` must identify a live allocation from this module, freed once.
#[no_mangle]
pub unsafe extern "C" fn selis_text_free(ptr: *mut u8, len: u32) {
    if ptr.is_null() || len == 0 {
        return;
    }
    let Ok(size) = usize::try_from(len) else {
        return;
    };
    let Ok(layout) = Layout::from_size_align(size, 1) else {
        return;
    };
    // SAFETY: the caller's contract.
    unsafe { dealloc(ptr, layout) };
}

/// The last error message as guest memory, freed with [`selis_text_free`].
///
/// # Safety
/// `out_len` must point to four writable bytes in guest memory.
#[no_mangle]
pub unsafe extern "C" fn selis_text_last_error(out_len: *mut u32) -> *mut u8 {
    if out_len.is_null() {
        return std::ptr::null_mut();
    }
    let message = last_error().unwrap_or_default();
    if message.is_empty() {
        // SAFETY: the caller promised four writable bytes.
        unsafe { out_len.write(0) };
        return std::ptr::null_mut();
    }
    let bytes = message.as_bytes();
    let Ok(len) = u32::try_from(bytes.len()) else {
        // SAFETY: as above.
        unsafe { out_len.write(0) };
        return std::ptr::null_mut();
    };
    let ptr = alloc_bytes(bytes);
    if ptr.is_null() {
        // SAFETY: as above.
        unsafe { out_len.write(0) };
        return std::ptr::null_mut();
    }
    // SAFETY: as above.
    unsafe { out_len.write(len) };
    ptr
}

/// Availability probe.
#[no_mangle]
pub extern "C" fn selis_text_available() -> u32 {
    1
}

/// Run one text operation.
///
/// `op` is 1 text, 2 tables, 3 split, 4 reflow. `options_ptr`/`options_len` is a
/// JSON object: `{"pages":"1-3","title":"Report","find":"Chapter"}` — every key
/// is optional, and `pages` is the same 1-based selection the other cards use
/// (`""` means every page).
///
/// # Safety
/// All input slices must identify live guest allocations for the duration of the
/// call, and `out_len` must be a writable `u32`.
#[no_mangle]
pub unsafe extern "C" fn selis_text_run(
    input_ptr: *const u8,
    input_len: u32,
    op: u32,
    options_ptr: *const u8,
    options_len: u32,
    out_len: *mut u32,
) -> *mut u8 {
    clear_error();
    if out_len.is_null() {
        set_error("missing output slot".to_string());
        return std::ptr::null_mut();
    }
    if input_ptr.is_null() || input_len == 0 {
        set_error("empty input".to_string());
        return std::ptr::null_mut();
    }
    // SAFETY: the caller promised `input_len` readable bytes.
    let input = unsafe { std::slice::from_raw_parts(input_ptr, input_len as usize) };
    let options = if options_ptr.is_null() || options_len == 0 {
        String::new()
    } else {
        // SAFETY: as above for the options slice.
        let bytes = unsafe { std::slice::from_raw_parts(options_ptr, options_len as usize) };
        match std::str::from_utf8(bytes) {
            Ok(text) => text.to_string(),
            Err(_) => {
                set_error("options are not valid UTF-8".to_string());
                return std::ptr::null_mut();
            }
        }
    };
    let response = match Op::from_wire(op) {
        Some(op) => match run_impl(input, op, &options) {
            Ok((outputs, _pages)) => frame(&outputs).unwrap_or_else(|e| frame_error(&e)),
            Err(message) => frame_error(&message),
        },
        None => frame_error("unknown text operation"),
    };
    let Ok(len) = u32::try_from(response.len()) else {
        set_error("response too large".to_string());
        return std::ptr::null_mut();
    };
    let ptr = alloc_bytes(&response);
    if ptr.is_null() {
        set_error("out of memory".to_string());
        return std::ptr::null_mut();
    }
    // SAFETY: the caller promised four writable bytes.
    unsafe { out_len.write(len) };
    ptr
}

/// One option from the JSON object, decoded by hand.
///
/// The chunk has no serde dependency on this path — the options are two strings
/// and a page selection — and a hand-written reader keeps the surface visible:
/// exactly these keys, exactly these types.
fn option(json: &str, key: &str) -> Option<String> {
    let needle = format!("\"{key}\"");
    let start = json.find(&needle)?;
    let rest = json.get(start.saturating_add(needle.len())..)?;
    let rest = rest.trim_start().strip_prefix(':')?.trim_start();
    let rest = rest.strip_prefix('"')?;
    let mut out = String::new();
    let mut chars = rest.chars();
    while let Some(ch) = chars.next() {
        match ch {
            '"' => return Some(out),
            '\\' => match chars.next() {
                Some('n') => out.push('\n'),
                Some('t') => out.push('\t'),
                Some('r') => out.push('\r'),
                Some('"') => out.push('"'),
                Some('\\') => out.push('\\'),
                _ => return None,
            },
            ch => out.push(ch),
        }
    }
    None
}

/// A file stem without its extension, safe to use in a download name.
fn file_stem(name: &str) -> String {
    let base = name.rsplit(['/', '\\']).next().unwrap_or(name);
    let stem = base.rsplit_once('.').map_or(base, |(stem, _)| stem);
    let cleaned: String = stem
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
                ch
            } else {
                '-'
            }
        })
        .collect();
    if cleaned.is_empty() {
        "document".to_string()
    } else {
        cleaned
    }
}

/// A sane output stem: the page passes the picked file's name, but the chunk
/// does not trust it to be free of separators.
fn page_selection(options: &str) -> Option<String> {
    option(options, "pages").filter(|pages| !pages.trim().is_empty())
}

/// Parse a `["...", "..."]` string array from one option.
///
/// Only the shapes a small hand-written reader can honestly guarantee: the
/// array for `slideImages` holds standard base64 (A–Z, a–z, 0–9, `+`, `/`,
/// `=`), which contains no quotes or commas, so trimming and quote-stripping
/// cannot mis-split a payload.
fn option_strings(json: &str, key: &str) -> Option<Vec<String>> {
    let needle = format!("\"{key}\"");
    let start = json.find(&needle)?;
    let rest = json.get(start.saturating_add(needle.len())..)?;
    let rest = rest.trim_start().strip_prefix(':')?.trim_start();
    let rest = rest.strip_prefix('[')?.trim_start();
    let close = rest.find(']')?;
    let inner = &rest[..close];
    let inner = inner.trim();
    if inner.is_empty() {
        return Some(Vec::new());
    }
    let mut out = Vec::new();
    for token in inner.split(',') {
        let token = token.trim();
        let value = token
            .strip_prefix('"')
            .and_then(|t| t.strip_suffix('"'))
            .unwrap_or(token);
        out.push(value.to_string());
    }
    Some(out)
}

#[allow(clippy::arithmetic_side_effects)]
fn base64_value(byte: u8) -> Option<u8> {
    match byte {
        b'A'..=b'Z' => Some(byte - b'A'),
        b'a'..=b'z' => Some(byte - b'a' + 26),
        b'0'..=b'9' => Some(byte - b'0' + 52),
        b'+' => Some(62),
        b'/' => Some(63),
        b'=' => Some(64),
        _ => None,
    }
}

/// Decode standard base64 (with padding) as the JS `btoa` produces it.
///
/// # Errors
///
/// `None` on a length that is not a multiple of four, padding in the wrong
/// chunk, or an alphabet byte.
#[allow(
    clippy::arithmetic_side_effects,
    clippy::integer_division,
    clippy::indexing_slicing
)]
fn decode_base64(input: &str) -> Option<Vec<u8>> {
    let bytes: Vec<u8> = input
        .bytes()
        .filter(|byte| !byte.is_ascii_whitespace())
        .collect();
    if bytes.len() % 4 != 0 {
        return None;
    }
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len() / 4 * 3);
    for chunk in bytes.chunks(4) {
        let mut values = [0u8; 4];
        let mut padding = 0usize;
        for (index, &byte) in chunk.iter().enumerate() {
            let value = base64_value(byte)?;
            if value == 64 {
                // Padding is only honest in the final two positions.
                if index < 2 {
                    return None;
                }
                padding = padding.saturating_add(1);
                values[index] = 0;
            } else if padding > 0 {
                // A data byte after a padding byte makes the chunk malformed.
                return None;
            } else {
                values[index] = value;
            }
        }
        if padding > 2 {
            return None;
        }
        let triple = u32::from(values[0]) << 18
            | u32::from(values[1]) << 12
            | u32::from(values[2]) << 6
            | u32::from(values[3]);
        out.push(((triple >> 16) & 0xff) as u8);
        if padding < 2 {
            out.push(((triple >> 8) & 0xff) as u8);
        }
        if padding < 1 {
            out.push((triple & 0xff) as u8);
        }
    }
    Some(out)
}

/// The safe body of [`selis_text_run`], unit-testable on the host.
fn run_impl(input: &[u8], op: Op, options: &str) -> Result<(Outputs, u32), String> {
    // The in-memory backend, for the duration of this call.
    //
    // In the browser this chunk is built without `selis-pdf-tools/native-fs`,
    // so the tools layer reads `in/0` out of the session table and there is
    // nothing to say. A WORKSPACE build unifies the CLI's `native-fs` feature
    // into this crate, and then the same read goes to the filesystem, where
    // `in/0` is not a path — the tools layer's own guard exists for exactly
    // this, and the tools chunk activates it for the same reason.
    let _memory = selis_pdf_tools::MemorySession::activate();
    let stem =
        option(options, "name").map_or_else(|| "document".to_string(), |name| file_stem(&name));
    selis_pdf_tools::clear();
    selis_pdf_tools::insert_input("in/0", input.to_vec());
    let pages = page_selection(options);
    match op {
        Op::Text => {
            let export = selis_pdf_tools::extract_text("in/0", pages.as_deref())
                .map_err(|e| e.to_string())?;
            let pages_out = u32::try_from(export.quality.pages).unwrap_or(u32::MAX);
            Ok((
                vec![
                    (format!("{stem}.txt"), "text", export.text.into_bytes()),
                    (
                        format!("{stem}-quality.json"),
                        "report",
                        export.quality.to_json().into_bytes(),
                    ),
                ],
                pages_out,
            ))
        }
        Op::Tables => {
            let export = selis_pdf_tools::extract_tables("in/0", pages.as_deref())
                .map_err(|e| e.to_string())?;
            Ok((
                vec![
                    (
                        format!("{stem}-tables.csv"),
                        "tables",
                        export.csv.into_bytes(),
                    ),
                    (
                        format!("{stem}-tables.json"),
                        "report",
                        export.json.into_bytes(),
                    ),
                ],
                0,
            ))
        }
        Op::Split => {
            let needle = option(options, "find").unwrap_or_default();
            let split = selis_pdf_tools::split_by_text("in/0", &needle, "part-")
                .map_err(|e| e.to_string())?;
            if split.parts.len() > MAX_OUTPUTS.saturating_sub(1) {
                return Err("too many parts".to_string());
            }
            let mut entries: Vec<(String, Vec<u8>)> = Vec::with_capacity(split.parts.len());
            for (index, part) in split.parts.iter().enumerate() {
                let bytes = selis_pdf_tools::take_output(&part.output)
                    .ok_or_else(|| "a verified part vanished from the session".to_string())?;
                entries.push((format!("part-{}.pdf", index.saturating_add(1)), bytes));
            }
            let refs: Vec<(&str, &[u8])> = entries
                .iter()
                .map(|(name, bytes)| (name.as_str(), bytes.as_slice()))
                .collect();
            let zip = selis_pdf_tools::bundle_zip(&refs).map_err(|e| e.to_string())?;
            let pages_out = u32::try_from(split.parts.len()).unwrap_or(u32::MAX);
            Ok((
                vec![
                    (format!("{stem}-split.zip"), "zip", zip),
                    (
                        format!("{stem}-split.json"),
                        "report",
                        split.json.into_bytes(),
                    ),
                ],
                pages_out,
            ))
        }
        Op::Reflow => {
            let title = option(options, "title").unwrap_or_else(|| stem.clone());
            let export = selis_pdf_tools::reflow_html("in/0", pages.as_deref(), &title)
                .map_err(|e| e.to_string())?;
            let pages_out = u32::try_from(export.pages).unwrap_or(u32::MAX);
            Ok((
                vec![(format!("{stem}.html"), "html", export.html.into_bytes())],
                pages_out,
            ))
        }
        Op::Epub => {
            let title = option(options, "title").unwrap_or_else(|| stem.clone());
            let export = selis_pdf_tools::reflow_epub("in/0", pages.as_deref(), &title)
                .map_err(|e| e.to_string())?;
            let pages_out = u32::try_from(export.pages).unwrap_or(u32::MAX);
            Ok((
                vec![(format!("{stem}.epub"), "file", export.epub)],
                pages_out,
            ))
        }
        Op::Word => {
            let title = option(options, "title").unwrap_or_else(|| stem.clone());
            let export = selis_pdf_tools::extract_docx("in/0", pages.as_deref(), &title)
                .map_err(|e| e.to_string())?;
            let pages_out = u32::try_from(export.pages).unwrap_or(u32::MAX);
            Ok((
                vec![(format!("{stem}.docx"), "file", export.bytes)],
                pages_out,
            ))
        }
        Op::Slides => {
            let title = option(options, "title").unwrap_or_else(|| stem.clone());
            // The deck carries the page rendered by the caller when one
            // supplied it — `slideImages` holds one base64 PNG per selected
            // page, in selection order. A deck the caller could not render
            // keeps the positioned-text-boxes facsimile instead.
            let images = match option_strings(options, "slideImages") {
                None => None,
                Some(decoded) => {
                    let mut out: Vec<Vec<u8>> = Vec::with_capacity(decoded.len());
                    for (index, encoded) in decoded.iter().enumerate() {
                        let bytes = decode_base64(encoded)
                            .ok_or_else(|| format!("slideImages[{index}] is not valid base64"))?;
                        out.push(bytes);
                    }
                    Some(out)
                }
            };
            let export = selis_pdf_tools::extract_pptx_with_images(
                "in/0",
                pages.as_deref(),
                &title,
                images.as_deref(),
            )
            .map_err(|e| e.to_string())?;
            let pages_out = u32::try_from(export.pages).unwrap_or(u32::MAX);
            Ok((
                vec![(format!("{stem}.pptx"), "file", export.bytes)],
                pages_out,
            ))
        }
        Op::Workbook => {
            let title = option(options, "title").unwrap_or_else(|| stem.clone());
            let export = selis_pdf_tools::extract_xlsx("in/0", pages.as_deref(), &title)
                .map_err(|e| e.to_string())?;
            let pages_out = u32::try_from(export.tables).unwrap_or(u32::MAX);
            Ok((
                vec![(format!("{stem}.xlsx"), "file", export.bytes)],
                pages_out,
            ))
        }
    }
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

    const TEXT: &[u8] = include_bytes!("../../selis-pdf-engine/src/fixtures/text.pdf");
    const NAV: &[u8] = include_bytes!("../../selis-pdf-engine/src/fixtures/nav.pdf");

    /// The tools layer keeps its inputs in one process-wide store, so tests
    /// that drive it must not overlap: `clear` in one test would empty the
    /// document the next one just inserted. Every test body takes this lock.
    static TEST_LOCK: Mutex<()> = Mutex::new(());

    fn serial<T>(body: impl FnOnce() -> T) -> T {
        let guard = TEST_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let out = body();
        drop(guard);
        out
    }

    fn run(input: &[u8], op: Op, options: &str) -> (Outputs, u32) {
        serial(|| run_impl(input, op, options).expect("run"))
    }

    fn run_err(input: &[u8], op: Op, options: &str) -> String {
        serial(|| run_impl(input, op, options).expect_err("refused"))
    }

    #[test]
    fn chunk_id_is_text() {
        assert_eq!(CHUNK_ID, "text");
        assert_eq!(selis_text_available(), 1);
    }

    #[test]
    fn a_text_export_carries_its_separator_and_its_quality_report() {
        let (outputs, pages) = run(TEXT, Op::Text, r#"{"name":"report.pdf"}"#);
        assert_eq!(pages, 1);
        assert_eq!(outputs.len(), 2);
        let text = String::from_utf8(outputs[0].2.clone()).expect("utf-8");
        assert!(text.starts_with("--- page 1 of 1 ---\n"), "{text}");
        assert!(!text.trim().is_empty());
        assert_eq!(outputs[0].0, "report.txt");
        assert_eq!(outputs[0].1, "text");
        let quality = String::from_utf8(outputs[1].2.clone()).expect("utf-8");
        assert!(quality.contains("\"schema\": \"selis-extract-quality/1\""));
        assert!(quality.contains("\"page\": 1"));
        assert_eq!(outputs[1].0, "report-quality.json");
    }

    #[test]
    fn the_quality_report_says_how_many_pages_it_read() {
        let (outputs, pages) = run(NAV, Op::Text, "{}");
        assert_eq!(pages, 2);
        let text = String::from_utf8(outputs[0].2.clone()).expect("utf-8");
        assert!(text.contains("--- page 1 of 2 ---"), "{text}");
        assert!(text.contains("--- page 2 of 2 ---"), "{text}");
    }

    #[test]
    fn a_page_outside_the_document_is_refused() {
        let err = run_err(NAV, Op::Text, r#"{"pages":"3"}"#);
        assert!(err.contains("outside this document"), "{err}");
    }

    #[test]
    fn a_selection_numbers_the_pages_it_exported() {
        let (outputs, pages) = run(NAV, Op::Text, r#"{"pages":"2"}"#);
        assert_eq!(pages, 1);
        let text = String::from_utf8(outputs[0].2.clone()).expect("utf-8");
        assert!(text.starts_with("--- page 2 of 1 ---"), "{text}");
    }

    #[test]
    fn tables_are_read_from_alignment_and_reported_with_confidence() {
        let pdf = table_pdf();
        let (text, _) = run(&pdf, Op::Text, "{}");
        let extracted = String::from_utf8(text[0].2.clone()).expect("utf-8");
        let (outputs, _) = run(&pdf, Op::Tables, r#"{"name":"ledger.pdf"}"#);
        assert_eq!(outputs.len(), 2, "text={extracted:?}");
        let csv = String::from_utf8(outputs[0].2.clone()).expect("utf-8");
        let report = String::from_utf8(outputs[1].2.clone()).expect("utf-8");
        assert!(
            csv.contains("Name,Amount"),
            "csv={csv:?} report={report:?} text={extracted:?}"
        );
        assert!(
            csv.contains("Ada,1200"),
            "csv={csv:?} report={report:?} text={extracted:?}"
        );
        assert!(
            report.contains("\"schema\": \"selis-tables/1\""),
            "{report}"
        );
        assert!(report.contains("\"tables\": 1"), "{report}");
    }

    #[test]
    fn a_document_with_no_table_says_zero_rather_than_failing() {
        let (outputs, _) = run(TEXT, Op::Tables, "{}");
        let report = String::from_utf8(outputs[1].2.clone()).expect("utf-8");
        assert!(report.contains("\"tables\": 0"), "{report}");
        assert_eq!(String::from_utf8(outputs[0].2.clone()).expect("utf-8"), "");
    }

    #[test]
    fn split_by_text_bundles_the_parts_it_made() {
        let pdf = chapter_pdf();
        let (outputs, pages) = run(&pdf, Op::Split, r#"{"find":"Chapter 2"}"#);
        assert_eq!(pages, 2);
        assert_eq!(outputs.len(), 2);
        let zip = &outputs[0].2;
        assert!(zip.starts_with(b"PK\x03\x04\x14"), "not a zip");
        let report = String::from_utf8(outputs[1].2.clone()).expect("utf-8");
        assert!(
            report.contains("\"schema\": \"selis-split-text/1\""),
            "{report}"
        );
        assert!(report.contains("\"matched_pages\": [2]"), "{report}");
    }

    #[test]
    fn a_needle_that_matches_nothing_is_refused_by_name() {
        let err = run_err(&chapter_pdf(), Op::Split, r#"{"find":"Appendix Z"}"#);
        assert!(err.contains("no page contains"), "{err}");
    }

    #[test]
    fn reflow_writes_an_article_that_carries_its_words() {
        let (outputs, pages) = run(TEXT, Op::Reflow, r#"{"title":"Report"}"#);
        assert_eq!(pages, 1);
        let html = String::from_utf8(outputs[0].2.clone()).expect("utf-8");
        assert!(html.starts_with("<!doctype html>"), "{html}");
        assert!(html.contains("<article>"), "{html}");
        assert!(
            html.contains("<section class=\"page\" data-page=\"1\">"),
            "{html}"
        );
        assert_eq!(outputs[0].1, "html");
    }

    #[test]
    fn the_abi_moves_a_request_and_reports_a_failure() {
        let mut out_len = 0u32;
        let empty =
            unsafe { selis_text_run(std::ptr::null(), 0, 1, std::ptr::null(), 0, &mut out_len) };
        assert!(empty.is_null());
        let message = unsafe { selis_text_last_error(&mut out_len) };
        assert!(!message.is_null());
        let bytes = unsafe { std::slice::from_raw_parts(message, out_len as usize) }.to_vec();
        assert!(String::from_utf8_lossy(&bytes).contains("empty input"));
        unsafe { selis_text_free(message, out_len) };

        let options = br#"{"name":"report.pdf"}"#;
        let mut out_len = 0u32;
        let response = unsafe {
            selis_text_run(
                TEXT.as_ptr(),
                u32::try_from(TEXT.len()).unwrap(),
                1,
                options.as_ptr(),
                u32::try_from(options.len()).unwrap(),
                &mut out_len,
            )
        };
        assert!(!response.is_null(), "{}", last_error().unwrap_or_default());
        let bytes = unsafe { std::slice::from_raw_parts(response, out_len as usize) }.to_vec();
        assert!(bytes.starts_with(b"SLTX"));
        unsafe { selis_text_free(response, out_len) };

        let mut id_len = 0u32;
        let id = unsafe { selis_text_chunk_id(&mut id_len) };
        let bytes = unsafe { std::slice::from_raw_parts(id, id_len as usize) }.to_vec();
        assert_eq!(bytes, b"text");
        unsafe { selis_text_free(id, id_len) };
    }

    /// Two pages, each with one line of text.
    fn chapter_pdf() -> Vec<u8> {
        text_pdf(&["Chapter 1", "Chapter 2"])
    }

    /// One page holding a two-column, two-row table.
    fn table_pdf() -> Vec<u8> {
        let content = "BT /F1 10 Tf 72 700 Td (Name) Tj 228 0 Td (Amount) Tj ET\n\
                       BT /F1 10 Tf 72 686 Td (Ada) Tj 228 0 Td (1200) Tj ET\n";
        build_pdf(&[content])
    }

    fn text_pdf(lines: &[&str]) -> Vec<u8> {
        let contents: Vec<String> = lines
            .iter()
            .map(|line| format!("BT /F1 24 Tf 72 700 Td ({line}) Tj ET\n"))
            .collect();
        let refs: Vec<&str> = contents.iter().map(String::as_str).collect();
        build_pdf(&refs)
    }

    /// A PDF with one page per content stream, one Helvetica resource.
    fn build_pdf(contents: &[&str]) -> Vec<u8> {
        let page_count = contents.len();
        let font_object = page_count.saturating_mul(2).saturating_add(3);
        let mut objects: Vec<Vec<u8>> = Vec::new();
        objects.push(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n".to_vec());
        let kids: String = (0..page_count)
            .map(|index| format!("{} 0 R", index.saturating_mul(2).saturating_add(3)))
            .collect::<Vec<String>>()
            .join(" ");
        objects.push(
            format!("2 0 obj\n<< /Type /Pages /Kids [{kids}] /Count {page_count} >>\nendobj\n")
                .into_bytes(),
        );
        for (index, content) in contents.iter().enumerate() {
            let page = index.saturating_mul(2).saturating_add(3);
            let body = index.saturating_mul(2).saturating_add(4);
            objects.push(
                format!(
                    "{page} 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
                     /Resources << /Font << /F1 {font_object} 0 R >> >> /Contents {body} 0 R >>\nendobj\n"
                )
                .into_bytes(),
            );
            objects.push(
                format!(
                    "{body} 0 obj\n<< /Length {} >>\nstream\n{content}endstream\nendobj\n",
                    content.len()
                )
                .into_bytes(),
            );
        }
        objects.push(
            format!(
                "{font_object} 0 obj\n<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica \
                 /Encoding /WinAnsiEncoding >>\nendobj\n"
            )
            .into_bytes(),
        );
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
    fn epub_package_is_readable() {
        // `run` already serialises on `TEST_LOCK`; taking it here as well would
        // deadlock on a non-reentrant mutex and stall the whole binary.
        let (outputs, pages) = run(TEXT, Op::Epub, r#"{"name":"book.pdf","title":"Book"}"#);
        assert_eq!(pages, 1);
        let (name, kind, bytes) = &outputs[0];
        assert_eq!(name, "book.epub");
        assert_eq!(*kind, "file");
        assert!(bytes.starts_with(b"PK\x03\x04"));
        // The one ordering rule an OCF reader enforces: mimetype first, stored,
        // and its exact bytes at offset 30 (immediately after the local header).
        assert_eq!(&bytes[30..38], b"mimetype");
        let has = |needle: &[u8]| bytes.windows(needle.len()).any(|w| w == needle);
        assert!(has(b"application/epub+zip"));
        assert!(has(b"OEBPS/text/page-1.xhtml"));
        assert!(!has(b"OEBPS/text/page-2.xhtml"));
    }

    #[test]
    fn word_package_carries_the_article() {
        let (outputs, pages) = run(TEXT, Op::Word, r#"{"name":"report.pdf","title":"Report"}"#);
        assert_eq!(pages, 1);
        let (name, kind, bytes) = &outputs[0];
        assert_eq!(name, "report.docx");
        assert_eq!(*kind, "file");
        assert!(bytes.starts_with(b"PK\x03\x04"), "not a zip");
        let has = |needle: &[u8]| bytes.windows(needle.len()).any(|w| w == needle);
        assert!(has(b"word/document.xml"));
        assert!(has(b"word/styles.xml"));
    }

    #[test]
    fn slides_are_one_per_page() {
        let (outputs, pages) = run(NAV, Op::Slides, r#"{"name":"deck.pdf","title":"Deck"}"#);
        assert_eq!(pages, 2);
        let (name, kind, bytes) = &outputs[0];
        assert_eq!(name, "deck.pptx");
        assert_eq!(*kind, "file");
        assert!(bytes.starts_with(b"PK\x03\x04"), "not a zip");
        let has = |needle: &[u8]| bytes.windows(needle.len()).any(|w| w == needle);
        assert!(has(b"ppt/slides/slide1.xml"));
        assert!(has(b"ppt/slides/slide2.xml"));
        assert!(!has(b"ppt/slides/slide3.xml"));
    }

    #[test]
    fn slides_embed_the_caller_supplied_page_images() {
        // A tiny fake PNG: the embedder does not decode it, the package just
        // carries it. The needle is the media part's own file name, which
        // appears in the archive's local and central headers.
        let fake_png = "iVBORw0KGgoAAAANSUhEUg=="; // "\x89PNG\r\n\x1a\n\0IEND"
        let options = format!(
            r#"{{"name":"deck.pdf","title":"Deck","slideImages":["{fake_png}","{fake_png}"]}}"#
        );
        let (outputs, pages) = run(NAV, Op::Slides, &options);
        assert_eq!(pages, 2);
        let (name, kind, bytes) = &outputs[0];
        assert_eq!(name, "deck.pptx");
        assert_eq!(*kind, "file");
        let has = |needle: &[u8]| bytes.windows(needle.len()).any(|w| w == needle);
        assert!(has(b"ppt/media/slideImage1.png"));
        assert!(has(b"ppt/media/slideImage2.png"));
        assert!(has(b"r:embed=\"rId2\""));
    }

    #[test]
    fn the_workbook_carries_the_reconstructed_table() {
        let (outputs, tables) = run(
            &table_pdf(),
            Op::Workbook,
            r#"{"name":"ledger.pdf","title":"Ledger"}"#,
        );
        assert_eq!(tables, 1);
        let (name, kind, bytes) = &outputs[0];
        assert_eq!(name, "ledger.xlsx");
        assert_eq!(*kind, "file");
        assert!(bytes.starts_with(b"PK\x03\x04"), "not a zip");
        let has = |needle: &[u8]| bytes.windows(needle.len()).any(|w| w == needle);
        assert!(has(b"xl/workbook.xml"));
        assert!(has(b"xl/worksheets/sheet2.xml"));
        assert!(has(b"Ada"));
    }
}
