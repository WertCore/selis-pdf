//! HTML lazy chunk: the selected pages → one positioned HTML document, on the device.
//!
//! A separate `cdylib` from the viewer, the tools chunk, the convert chunk and the raster
//! chunk. The tools page never downloads this module until the user asks for HTML; it is
//! fetched then and cached by the service worker. Like the chunks before it, it speaks a
//! pointer ABI and **declares no imports**: the page instantiates it with an empty import
//! object, which is the mechanical form of "this module cannot call out". All document
//! bytes arrive through [`selis_html_alloc`] and the answer leaves as a guest allocation;
//! nothing touches a network.
//!
//! What it writes is the engine's own extraction pipeline — the page's display list, the
//! visible-region gate, Unicode recovery, run/word/line assembly, reading order — rendered
//! by `selis-pdf-text`'s positioned emitter: one `<section>` per page carrying that page's
//! canvas size, one `<p>` per span at its projected position. The canvas is
//! `Session::page_view` at 72 DPI, i.e. one CSS pixel per PDF unit with `/Rotate` folded
//! into the transform — the same box the PDF-to-PNG card paints, so a span lands where that
//! card puts the ink.
//!
//! The document is read back before it is handed over. [`verify_positioned_html`] re-parses
//! the emitted bytes with the crate's own strict reader and demands the model it was built
//! from: the page count and numbers, both canvas sizes, the low-confidence flag, every
//! span's text and every projected edge and placement. A document that fails is reported as
//! a failure, never shipped under a verified label — the same rule the raster chunk applies
//! to a PNG or a TIFF.
//!
//! # What the output does not claim
//!
//! * **Colour.** The extraction model carries no resolved fill, so none is emitted.
//! * **Font fidelity.** One CSS fallback family; a page's own faces are not shipped.
//! * **Reflow.** This is a layout copy, not an article: spans sit where the page put them.
//! * **Separators between spans.** A span is a word and its text is the model's exactly,
//!   so the gap between two words is geometry rather than a character — which is what the
//!   JSON extraction format already ships, and a consumer that concatenates spans gets the
//!   same answer here.

#![allow(unsafe_code)]

use std::alloc::{alloc, dealloc, Layout};
use std::sync::Mutex;

use selis_geom::Matrix;
use selis_pdf_engine::{PageView, Session};
use selis_pdf_text::{
    apply_unicode_recovery, assemble, gather_glyphs, order_lines, structured, to_positioned_html,
    verify_positioned_html, HtmlPage, LineWithMcid, Structured, TextLine,
};
use selis_sandbox::{Budget, CancelToken, FixedClock, Surface};

/// Chunk identifier (stable, matches the manifest).
pub const CHUNK_ID: &str = "html";
/// Chunk version.
pub const CHUNK_VERSION: &str = env!("CARGO_PKG_VERSION");

/// The DPI the pages are laid out at.
///
/// Seventy-two is one CSS pixel per PDF unit, which is the canvas
/// [`Session::page_view`] the PDF-to-PNG card rasterises at its lowest setting. The
/// emitter scales with any DPI, so this is a choice about what the output *means* — a
/// document whose positions read back as the page's own points — not a quality knob, and
/// it is why the ABI carries no DPI argument.
const HTML_DPI: f64 = 72.0;

/// The longest document title the chunk accepts, in characters.
///
/// The title is the name the user's file has, which no filesystem allows anywhere near
/// this long. The bound exists for the caller that passes something else by mistake — a
/// whole document, say — and a truncated title is honest because the verification compares
/// the document against the title it was written with, not against the caller's request.
const MAX_TITLE_CHARS: usize = 200;

/// The last error message. A `Mutex`, not a `static mut`: a shared reference to a mutable
/// static is undefined behaviour even on a single thread.
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
pub unsafe extern "C" fn selis_html_chunk_id(out_len: *mut u32) -> *mut u8 {
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
pub extern "C" fn selis_html_alloc(len: u32) -> *mut u8 {
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
pub unsafe extern "C" fn selis_html_free(ptr: *mut u8, len: u32) {
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

/// Returns the last error message as guest memory, freed with [`selis_html_free`]. Writes
/// its length to `out_len`.
///
/// # Safety
/// `out_len` must be a writable 4-byte-aligned guest `u32` slot.
#[no_mangle]
pub unsafe extern "C" fn selis_html_last_error(out_len: *mut u32) -> *mut u8 {
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
pub extern "C" fn selis_html_available() -> u32 {
    1
}

/// Write the selected pages as one positioned HTML document.
///
/// `pages_ptr`/`pages_len` is a UTF-8 selection in the same 1-based syntax the page's other
/// tools use (`""` = every page, `"1-3,5"` = those four). `title_ptr`/`title_len` is the
/// document's `<title>`, usually the file's own name; a null or empty slice means no title.
/// The result is one `.html` file however many pages were selected — the pages are
/// `<section>`s inside it — and `out_pages` carries how many, so the page can tell the user
/// what the document holds. Free the result with [`selis_html_free`].
///
/// On failure the return value is null and [`selis_html_last_error`] carries the reason.
///
/// # Safety
/// All input slices must identify live guest allocations for the duration of the call, and
/// both output slots must be writable `u32`s.
#[no_mangle]
pub unsafe extern "C" fn selis_html_run(
    input_ptr: *const u8,
    input_len: u32,
    pages_ptr: *const u8,
    pages_len: u32,
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
    let selection = match guest_text(pages_ptr, pages_len) {
        Ok(text) => text,
        Err(message) => {
            set_error(message);
            return std::ptr::null_mut();
        }
    };
    let title = match guest_text(title_ptr, title_len) {
        Ok(text) => text,
        Err(message) => {
            set_error(message);
            return std::ptr::null_mut();
        }
    };
    match run_impl(input, selection, title) {
        Ok((bytes, pages)) => {
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
                out_pages.write(pages);
            }
            ptr
        }
        Err(message) => {
            set_error(message);
            std::ptr::null_mut()
        }
    }
}

/// A caller-supplied guest string: null or empty is the empty string.
///
/// # Safety
/// `ptr`/`len` must identify a live guest allocation for the duration of the call.
unsafe fn guest_text<'a>(ptr: *const u8, len: u32) -> Result<&'a str, String> {
    if ptr.is_null() || len == 0 {
        return Ok("");
    }
    let bytes = unsafe { std::slice::from_raw_parts(ptr, len as usize) };
    std::str::from_utf8(bytes).map_err(|_| "text is not valid UTF-8".to_string())
}

/// One page as the emitter needs it: its extracted text and the canvas that text sits on.
struct LaidOut {
    /// The 1-based page number the document will carry.
    number: u32,
    /// The canvas width in CSS pixels.
    width_px: u32,
    /// The canvas height in CSS pixels.
    height_px: u32,
    /// User space → canvas pixels.
    ctm: Matrix,
    /// The page's extraction model.
    model: Structured,
}

/// The safe body of [`selis_html_run`], unit-testable on the host.
fn run_impl(input: &[u8], selection: &str, title: &str) -> Result<(Vec<u8>, u32), String> {
    let budget = Budget::profile(Surface::Batch);
    let clock = FixedClock(0);
    let session = Session::open(input.to_vec(), &budget, &clock)
        .map_err(|e| format!("cannot open PDF: {e}"))?;
    let pages = parse_pages(selection, session.len())?;
    let title: String = title.chars().take(MAX_TITLE_CHARS).collect();
    // The structure tree's marked-content order, when the document has one: the same
    // reading-order source the CLI and the viewer's text layer use, so the HTML lists a
    // tagged page's lines the way the tagged page means them to be read.
    let mut walk = budget.guard_with(&clock, CancelToken::new());
    let mcid_order = session
        .mcid_order(&budget, &mut walk)
        .ok()
        .filter(|order| !order.is_empty());
    drop(walk);
    let mut laid: Vec<LaidOut> = Vec::with_capacity(pages.len());
    for &page in &pages {
        let mut g = budget.guard_with(&clock, CancelToken::new());
        let model = page_model(&session, &budget, &mut g, page, mcid_order.as_deref())?;
        drop(g);
        let label = page.saturating_add(1);
        let view = session
            .page_view(page, HTML_DPI)
            .ok_or_else(|| format!("page {label} has no box to lay text out on"))?;
        check_view_is_writable(&view, page)?;
        laid.push(LaidOut {
            number: u32::try_from(label).map_err(|_| "too many pages".to_string())?,
            width_px: view.width,
            height_px: view.height,
            ctm: view.ctm,
            model,
        });
    }
    // The emitter borrows the models, so every page is extracted and held before the first
    // byte is written. A page-sized document of text lines is small; the raster chunk's
    // per-page canvas discipline has no equivalent here because one HTML file is assembled
    // from all of its pages in one call.
    let pages_html: Vec<HtmlPage<'_>> = laid
        .iter()
        .map(|page| HtmlPage {
            number: page.number,
            width_px: page.width_px,
            height_px: page.height_px,
            ctm: page.ctm,
            content: &page.model,
        })
        .collect();
    let document = to_positioned_html(&pages_html, &title)
        .map_err(|e| format!("the HTML could not be written: {e}"))?;
    verify_positioned_html(&pages_html, &title, &document)
        .map_err(|e| format!("the HTML failed its own read-back: {e}"))?;
    let count = u32::try_from(pages_html.len()).map_err(|_| "too many pages".to_string())?;
    Ok((document.into_bytes(), count))
}

/// Refuse a page whose canvas cannot carry a box, before writing a document around it.
///
/// A `PageView` of zero width or height is what `/MediaBox [0 0 0 0]` becomes: the emitter
/// would happily describe a `<section>` of `0px` — the box would be self-consistent and
/// would verify — while showing none of the text it holds. The raster path refuses the same
/// page when it cannot build a canvas, so both surfaces answer it alike.
fn check_view_is_writable(view: &PageView, page: usize) -> Result<(), String> {
    if view.width > 0 && view.height > 0 {
        return Ok(());
    }
    Err(format!(
        "page {} has no area to place text on: its box is {}x{} px at 72 DPI, so its text \
         cannot be positioned",
        page.saturating_add(1),
        view.width,
        view.height
    ))
}

/// The page's extracted text as the emitter's model: its lines in reading order, one span
/// per run, and the SL-3.TEXT.10 low-confidence flag.
///
/// This is the same pipeline `selis extract` drives (`apps/cli/src/extract.rs`) and the
/// viewer's text layer drives (`crates/selis-pdf-wasm/src/worker.rs`): the shared gather and
/// Unicode-recovery helpers exist so the three surfaces cannot drift, and this is the third
/// caller they were written for.
///
/// # Errors
///
/// The page's display list, reading order or budget failing; all are reported with the page
/// that caused them.
fn page_model(
    session: &Session,
    budget: &Budget,
    g: &mut selis_sandbox::BudgetGuard<'_>,
    page: usize,
    mcid_order: Option<&[u32]>,
) -> Result<Structured, String> {
    let label = page.saturating_add(1);
    let dl = session
        .page_text_display_list(page, budget, g)
        .map_err(|e| format!("page {label} could not be read: {e}"))?;
    // The page's visible region gates extraction (SL-3.TEXT.26): a glyph the page clips
    // away is not text on this page, and must not be positioned in the document either.
    let visible = session.page_visible_box(page);
    let mut glyphs = gather_glyphs(&dl, visible);
    let drew_text = !glyphs.is_empty();
    apply_unicode_recovery(&mut glyphs, &mut |font, code| {
        session.text_unicode(page, font, code, budget, g)
    });
    let lines = assemble(glyphs);
    let mcid_lines = lines
        .into_iter()
        .map(|line| {
            let mcid = line
                .words
                .iter()
                .flat_map(|word| word.runs.iter())
                .find_map(|run| run.glyphs.first().and_then(|glyph| glyph.mcid));
            LineWithMcid { line, mcid }
        })
        .collect();
    let ordered = order_lines(mcid_lines, mcid_order, g)
        .map_err(|e| format!("page {label} could not be ordered: {e}"))?;
    let line_texts: Vec<String> = ordered.lines.iter().map(line_text).collect();
    let run_texts: Vec<Vec<String>> = ordered
        .lines
        .iter()
        .map(|line| {
            line.words
                .iter()
                .flat_map(|word| word.runs.iter())
                .map(|run| {
                    run.glyphs
                        .iter()
                        .filter_map(|glyph| char::from_u32(u32::from(glyph.code)))
                        .collect()
                })
                .collect()
        })
        .collect();
    let mut model = structured(&ordered.lines, &line_texts, &run_texts);
    // SL-3.TEXT.10: text was drawn but nothing is readable. The emitter turns the flag into
    // the marker line the other formats carry.
    model.low_confidence = drew_text && !line_texts.iter().any(|text| !text.trim().is_empty());
    Ok(model)
}

/// The recovered text of a line, with one space between words — the assembler strips the
/// space glyphs when it splits runs into words, so the separator has to be put back for the
/// line's own text (it stays geometry, not a character, inside each span).
fn line_text(line: &TextLine) -> String {
    let mut out = String::new();
    for (index, word) in line.words.iter().enumerate() {
        if index > 0 {
            out.push(' ');
        }
        for run in &word.runs {
            for glyph in &run.glyphs {
                if let Some(ch) = char::from_u32(u32::from(glyph.code)) {
                    out.push(ch);
                }
            }
        }
    }
    out
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

    /// Every span text in the document, in the order the document lists them.
    fn span_texts(document: &str) -> Vec<String> {
        let read = selis_pdf_text::read_positioned_html(document).expect("read back");
        read.pages
            .iter()
            .flat_map(|page| page.spans.iter())
            .map(|span| span.text.clone())
            .collect()
    }

    #[test]
    fn chunk_id_is_html() {
        assert_eq!(CHUNK_ID, "html");
        assert_eq!(selis_html_available(), 1);
    }

    #[test]
    fn a_text_page_becomes_a_document_that_carries_its_words() {
        let (bytes, pages) = run_impl(TEXT, "", "Report").expect("write");
        assert_eq!(pages, 1);
        let document = String::from_utf8(bytes).expect("utf-8");
        assert!(document.starts_with("<!doctype html>\n"), "{document}");
        assert!(
            document.contains("<section class=\"page\" data-page=\"1\""),
            "{document}"
        );
        let words = span_texts(&document);
        assert!(
            words.iter().any(|word| !word.trim().is_empty()),
            "the page carries no readable word: {words:?}"
        );
    }

    #[test]
    fn every_selected_page_is_a_section_numbered_as_the_user_selected_it() {
        let (bytes, pages) = run_impl(NAV, "", "Two").expect("write");
        assert_eq!(pages, 2);
        let document = String::from_utf8(bytes).expect("utf-8");
        assert!(document.contains("data-page=\"1\""), "{document}");
        assert!(document.contains("data-page=\"2\""), "{document}");
        let read = selis_pdf_text::read_positioned_html(&document).expect("read back");
        assert_eq!(read.pages.len(), 2);
        assert_eq!(read.title, "Two");

        // A selection of the second page alone still says so: the number is the page's,
        // not the position it happens to hold in the document.
        let (one, pages) = run_impl(NAV, "2", "Second").expect("write second");
        assert_eq!(pages, 1);
        let document = String::from_utf8(one).expect("utf-8");
        let read = selis_pdf_text::read_positioned_html(&document).expect("read back");
        assert_eq!(read.pages.len(), 1);
        assert_eq!(read.pages[0].number, 2);
    }

    #[test]
    fn a_page_with_no_text_is_a_section_with_no_spans_and_no_marker() {
        // `minimal.pdf` draws a rectangle and no glyphs, so nothing was recovered AND
        // nothing was drawn: SL-3.TEXT.10 keeps those two answers apart, and the document
        // must show the difference rather than mark a blank page as unreadable.
        let (bytes, pages) = run_impl(MINIMAL, "", "Blank").expect("write");
        assert_eq!(pages, 1);
        let document = String::from_utf8(bytes).expect("utf-8");
        assert_eq!(span_texts(&document), Vec::<String>::new());
        let read = selis_pdf_text::read_positioned_html(&document).expect("read back");
        assert!(
            !read.pages[0].low_confidence,
            "a blank page read as unreadable"
        );
        // The stylesheet always names the `p.low-confidence` rule; the marker paragraph is
        // what must be absent.
        assert!(!document.contains("[low-confidence:"), "{document}");
        assert!(document.contains("data-width=\"100\""), "{document}");
    }

    #[test]
    fn writing_the_same_selection_twice_is_byte_identical() {
        let (a, _) = run_impl(NAV, "1-2", "Same").expect("first");
        let (b, _) = run_impl(NAV, " 1 , 2 ", "Same").expect("second");
        assert_eq!(a, b);
    }

    #[test]
    fn the_canvas_is_the_pages_own_box_and_every_span_sits_inside_it() {
        // The claim the card makes: a span sits where the PDF-to-PNG card paints its ink,
        // because both are the same `PageView`. Read from the engine rather than hardcoded,
        // so the assertion says what is actually true of this document.
        let budget = Budget::unlimited();
        let clock = FixedClock(0);
        let session = Session::open(TEXT.to_vec(), &budget, &clock).expect("open");
        let view = session.page_view(0, HTML_DPI).expect("page view");
        assert!(
            view.width > 0 && view.height > 0,
            "the fixture has no canvas"
        );
        let (bytes, _) = run_impl(TEXT, "", "Box").expect("write");
        let document = String::from_utf8(bytes).expect("utf-8");
        let read = selis_pdf_text::read_positioned_html(&document).expect("read back");
        assert_eq!(read.pages[0].width_px, view.width);
        assert_eq!(read.pages[0].height_px, view.height);
        for span in &read.pages[0].spans {
            let [left, top, right, bottom] = span.box_px;
            assert!(
                left >= 0.0
                    && top >= 0.0
                    && right <= f64::from(view.width)
                    && bottom <= f64::from(view.height),
                "span {:?} at {left},{top},{right},{bottom} is outside the {}x{} canvas",
                span.text,
                view.width,
                view.height
            );
        }
    }

    #[test]
    fn markup_in_the_title_cannot_escape_its_element() {
        let (bytes, _) =
            run_impl(MINIMAL, "", r#"a<img src=x onerror="alert(1)">&b"#).expect("write");
        let document = String::from_utf8(bytes).expect("utf-8");
        assert!(!document.contains("<img"), "{document}");
        // The markup survives as inert *content*: the angle brackets are gone, so nothing
        // here can become an element or an attribute, and the reader still refuses any
        // document that says otherwise.
        assert!(
            document.contains(r#"<title>a&lt;img src=x onerror="alert(1)"&gt;&amp;b</title>"#),
            "{document}"
        );
        // The sanitised form is what the document carries, and what it is verified against.
        let read = selis_pdf_text::read_positioned_html(&document).expect("read back");
        assert_eq!(read.title, r#"a<img src=x onerror="alert(1)">&b"#);
    }

    #[test]
    fn a_long_title_is_truncated_and_still_verifies() {
        let long = "x".repeat(5_000);
        let (bytes, _) = run_impl(MINIMAL, "", &long).expect("write");
        let document = String::from_utf8(bytes).expect("utf-8");
        let read = selis_pdf_text::read_positioned_html(&document).expect("read back");
        assert_eq!(read.title.chars().count(), MAX_TITLE_CHARS);
    }

    #[test]
    fn a_control_character_in_the_title_is_not_a_new_line() {
        // The reader insists a `<title>` is one line, so a document-derived newline must
        // arrive as a character, never as a line break in the profile.
        let (bytes, _) = run_impl(MINIMAL, "", "one\ntwo\u{7}").expect("write");
        let document = String::from_utf8(bytes).expect("utf-8");
        let read = selis_pdf_text::read_positioned_html(&document).expect("read back");
        assert_eq!(read.title, "one\u{fffd}two\u{fffd}");
    }

    #[test]
    fn selection_parsing_is_one_based_and_ordered() {
        assert_eq!(parse_pages("", 3).unwrap(), vec![0, 1, 2]);
        assert_eq!(parse_pages("1-2,3", 3).unwrap(), vec![0, 1, 2]);
        assert_eq!(parse_pages("3,1", 3).unwrap(), vec![2, 0]);
        assert!(parse_pages("0", 3).is_err());
        assert!(parse_pages("4", 3).is_err());
        assert!(parse_pages("3-1", 3).is_err());
        assert!(parse_pages("x", 3).is_err());
    }

    #[test]
    fn a_page_outside_the_document_is_refused_before_anything_is_read() {
        let err = run_impl(NAV, "3", "No").expect_err("out of range");
        assert!(err.contains("outside this document"), "{err}");
    }

    #[test]
    fn a_page_with_no_area_is_refused_rather_than_written_as_a_zero_section() {
        let empty = page_pdf("0 0 0 0");
        let err = run_impl(&empty, "", "Zero").expect_err("no canvas");
        assert!(err.contains("no area to place text on"), "{err}");
    }

    #[test]
    fn a_document_that_is_not_a_pdf_is_refused() {
        let err = run_impl(b"not a pdf", "", "No").expect_err("not a pdf");
        assert!(err.contains("cannot open PDF"), "{err}");
    }

    #[test]
    fn the_abi_moves_bytes_through_guest_slots_and_reports_failure() {
        let mut out_len = 0u32;
        let mut out_pages = 0u32;
        let err_len = unsafe {
            selis_html_run(
                std::ptr::null(),
                0,
                std::ptr::null(),
                0,
                std::ptr::null(),
                0,
                &mut out_len,
                &mut out_pages,
            )
        };
        assert!(
            err_len.is_null(),
            "an empty input must not return a document"
        );
        assert_eq!(out_pages, 0);
        let message = unsafe { selis_html_last_error(&mut out_len) };
        assert!(!message.is_null());
        let bytes = unsafe { std::slice::from_raw_parts(message, out_len as usize) }.to_vec();
        assert!(
            String::from_utf8_lossy(&bytes).contains("empty input"),
            "{bytes:?}"
        );
        unsafe { selis_html_free(message, out_len) };

        let mut out_len = 0u32;
        let mut out_pages = 0u32;
        let title = b"Report";
        let document = unsafe {
            selis_html_run(
                TEXT.as_ptr(),
                u32::try_from(TEXT.len()).unwrap(),
                std::ptr::null(),
                0,
                title.as_ptr(),
                u32::try_from(title.len()).unwrap(),
                &mut out_len,
                &mut out_pages,
            )
        };
        assert!(!document.is_null(), "{}", last_error().unwrap_or_default());
        assert_eq!(out_pages, 1);
        let written = unsafe { std::slice::from_raw_parts(document, out_len as usize) }.to_vec();
        assert!(String::from_utf8_lossy(&written).starts_with("<!doctype html>\n"));
        unsafe { selis_html_free(document, out_len) };

        let mut id_len = 0u32;
        let id = unsafe { selis_html_chunk_id(&mut id_len) };
        let bytes = unsafe { std::slice::from_raw_parts(id, id_len as usize) }.to_vec();
        assert_eq!(bytes, b"html");
        unsafe { selis_html_free(id, id_len) };
    }

    /// A one-page document with the given `/MediaBox` and an empty content stream. Written
    /// by hand — the chunk has no business depending on the COS writer.
    fn page_pdf(box_: &str) -> Vec<u8> {
        let objects: [Vec<u8>; 4] = [
            b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n".to_vec(),
            b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n".to_vec(),
            format!(
                "3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [{box_}] \
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
}
