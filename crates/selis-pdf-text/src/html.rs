//! Positioned, self-contained HTML — written, read back and verified here.
//!
//! `PDF → HTML` needs a page, not a paragraph dump. The shape this module replaces — one
//! `<p data-bbox>` per recovered line, no canvas, no position, and a page range exported as
//! several `<html>` roots concatenated into one file — is text wearing HTML clothes. This
//! module writes what the raster cards paint:
//!
//! * [`to_positioned_html`] emits **one** document: one `<section class="page">` per
//!   selected page, sized with the caller's device canvas, holding one absolutely
//!   positioned `<p>` per measured span.
//! * [`read_positioned_html`] parses exactly the profile that emitter writes. It is
//!   deliberately *not* an HTML parser: anything outside the tag and attribute whitelist
//!   is refused with the line and the reason, which is what turns "no external reference
//!   can reach whoever opens this file" into a checked property instead of a hope.
//! * [`verify_positioned_html`] re-reads the bytes and compares them against the model
//!   they were written from, span for span — the doctrine `selis-image`'s `tiff` module
//!   applies to a multi-page container: the writer only owns the claim because it also
//!   owns the reader.
//!
//! # What the document claims
//!
//! The canvas is the page's `/MediaBox` with `/Rotate` folded into the transform at one
//! CSS pixel per PDF unit — the same `PageView` the PNG card rasterises at 72 DPI — so a
//! span lands where that card paints it. Only *recovered* text is positioned, so the
//! accuracy ceiling is the extraction measurement in `21-TESTING-AND-ORACLES.md` and not
//! this emitter's, and a page that drew text and recovered none carries
//! [`LOW_CONFIDENCE_MARKER`] exactly as the text, JSON and Markdown formats do.
//!
//! # What it does not claim
//!
//! * **Reflow.** This is a layout copy, not a reading-order article: spans stay in
//!   extraction order inside their page.
//! * **Colour.** The extraction model carries no resolved fill, so no colour assertion is
//!   possible and none is made.
//! * **Faces.** One CSS fallback family serves every span. A real face wider than the
//!   measured box overflows it, which is why the measured box is emitted as `data-box`
//!   and the position is that box's own top-left.
//! * **Control characters.** A newline inside a recovered span has no representation in a
//!   line-oriented profile, so it reaches the document as U+FFFD — the mapping a byte
//!   decoder uses for an invalid byte. Verification compares that sanitised form.
//! * **Geometry beyond the page.** Text whose matrices project to a non-finite or absurd
//!   device coordinate saturates at [`MAX_COORDINATE_PX`] instead of failing, as
//!   [`selis_geom::Fixed`] saturates a document-derived float. The words survive; only an
//!   already-off-page position degrades, and the raster path clips that same span away.

use selis_error::{err, Code, Error, Result};
use selis_geom::Matrix;

use crate::export::{Span, Structured, LOW_CONFIDENCE_MARKER};

/// The stylesheet. Every byte of it comes from this module and the reader compares the
/// `<style>` block line for line against it, so `@import` and `url()` cannot appear in a
/// document that passes [`read_positioned_html`] — and that block is the only place
/// either one could mean anything.
const STYLE_CSS: &str = "\
html{margin:0;padding:0;background:#e8e8e8}
body{margin:0;padding:16px;font-family:sans-serif;color:#000}
main{display:block}
section.page{position:relative;box-sizing:border-box;margin:0 auto 16px;background:#fff;overflow:hidden}
p.s{position:absolute;margin:0;padding:0;white-space:pre;line-height:1}
p.low-confidence{position:static;margin:8px;font-style:italic}";

/// The largest coordinate this profile writes, in CSS pixels. A page is at most 32 767
/// PDF units wide (ISO 32000-1 §7.1.2.3), so at one pixel per unit the real ceiling sits
/// three orders of magnitude below this; the bound exists so a document with a runaway
/// `cm` still produces finite, valid CSS rather than `NaNpx`.
pub const MAX_COORDINATE_PX: f64 = 10_000_000.0;

/// How far a re-read position may sit from the model's, in CSS pixels. Positions are
/// written with two decimals, so the round trip loses at most half of that; the bound
/// leaves the last digit free to differ without calling a faithful document broken, and is
/// tight enough that a nudged edge is caught.
pub const MAX_POSITION_ERROR_PX: f64 = 0.01;

/// The most integer digits the reader accepts in a coordinate. [`MAX_COORDINATE_PX`]
/// needs eight, so the extra three are headroom rather than a second clamp.
const MAX_DIGITS: usize = 11;

/// The literal a span line starts with, through the opening quote of `data-box`.
const SPAN_OPEN: &str = "<p class=\"s\" data-box=\"";
/// Between `data-box` and the inline position.
const SPAN_MID: &str = "\" style=\"left:";
/// Between `left` and `top`.
const SPAN_TOP: &str = "px;top:";
/// Between `top` and `font-size`.
const SPAN_SIZE: &str = "px;font-size:";
/// Out of the attributes and into the text node.
const SPAN_CLOSE: &str = "px\">";
/// What closes a span, and the marker line.
const SPAN_END: &str = "</p>";
/// The line a low-confidence page carries after its spans.
const LOW_OPEN: &str = "<p class=\"low-confidence\">";
/// The literal a page section starts with, through the opening quote of `data-page`.
const SECTION_OPEN: &str = "<section class=\"page\" data-page=\"";
/// Between the page number and the canvas width.
const SECTION_WIDTH: &str = "\" data-width=\"";
/// Between the canvas width and the canvas height.
const SECTION_HEIGHT: &str = "\" data-height=\"";
/// Between the height attribute and the inline canvas.
const SECTION_STYLE: &str = "\" style=\"width:";
/// Between the inline width and the inline height.
const SECTION_TAIL: &str = "px;height:";
/// What closes a section's opening line.
const SECTION_CLOSE: &str = "px\">";

/// One page of the model, ready to be laid into a document.
///
/// The canvas arrives already resolved — `width_px` × `height_px`, `/Rotate` applied —
/// because that pair is what the caller rasterises at: the HTML page and the PNG page of
/// one PDF page are the same rectangle by construction, not by two layers agreeing on it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct HtmlPage<'a> {
    /// The page's number as the user selected it: 1-based in every shipped surface.
    pub number: u32,
    /// The canvas width in CSS pixels.
    pub width_px: u32,
    /// The canvas height in CSS pixels.
    pub height_px: u32,
    /// The transform from PDF user space into those pixels: a `PageView`'s `ctm`, at any
    /// DPI. `IDENTITY` is not a page view and would place every span off the canvas.
    pub ctm: Matrix,
    /// The page's extracted text: its spans in reading order, and the SL-3.TEXT.10 flag.
    pub content: &'a Structured,
}

/// A span as it was re-read from emitted bytes.
#[derive(Debug, Clone, PartialEq)]
pub struct ReadSpan {
    /// The sanitised text — what the model's text becomes once it may live in HTML.
    pub text: String,
    /// The box's left edge, in CSS pixels.
    pub left_px: f64,
    /// The box's top edge, in CSS pixels.
    pub top_px: f64,
    /// The font size, in CSS pixels: the model's size scaled by the transform's vertical
    /// magnitude, so 12 pt at 72 DPI reads back as `12.00`.
    pub font_size_px: f64,
    /// The projected box: `[left, top, right, bottom]` in CSS pixels.
    pub box_px: [f64; 4],
}

/// One `<section>` as it was re-read.
#[derive(Debug, Clone, PartialEq)]
pub struct ReadPage {
    /// The `data-page` value.
    pub number: u32,
    /// The `data-width` value.
    pub width_px: u32,
    /// The `data-height` value.
    pub height_px: u32,
    /// Whether the page carries the low-confidence marker.
    pub low_confidence: bool,
    /// The spans, in document order.
    pub spans: Vec<ReadSpan>,
}

/// A re-read document.
#[derive(Debug, Clone, PartialEq)]
pub struct ReadDocument {
    /// The `<title>` text, sanitised the way a span's text is.
    pub title: String,
    /// The pages, in document order.
    pub pages: Vec<ReadPage>,
}

/// Emit one positioned HTML document.
///
/// `Err` only when a page's canvas is empty: a zero-area page is a document whose box
/// cannot be measured, and writing a `<section>` of `0px` for it would produce a file that
/// verifies perfectly while showing none of the text the model holds. The raster path
/// refuses the same page with the same `detail = "page has zero area"`, so both surfaces
/// answer it alike.
///
/// An empty `pages` slice is a valid, empty document: the selection decides that nothing
/// was asked for, not the emitter.
pub fn to_positioned_html(pages: &[HtmlPage<'_>], title: &str) -> Result<String> {
    let mut out = String::from(
        "<!doctype html>\n<html lang=\"en\">\n<head>\n<meta charset=\"utf-8\">\n<title>",
    );
    out.push_str(&escape(&carry(title)));
    out.push_str("</title>\n<style>\n");
    out.push_str(STYLE_CSS);
    out.push_str("\n</style>\n</head>\n<body>\n<main>\n");
    for page in pages {
        if page.width_px == 0 || page.height_px == 0 {
            return Err(err!(
                Code::ObjUnexpected,
                page = page.number,
                during = "html-export",
                detail = "page has zero area"
            ));
        }
        let width = page.width_px.to_string();
        let height = page.height_px.to_string();
        out.push_str(&format!(
            "{SECTION_OPEN}{number}{SECTION_WIDTH}{width}{SECTION_HEIGHT}{height}{SECTION_STYLE}{width}{SECTION_TAIL}{height}{SECTION_CLOSE}\n",
            number = page.number,
        ));
        for span in page.content.lines.iter().flat_map(|line| line.spans.iter()) {
            out.push_str(&span_markup(span, page.ctm));
        }
        if page.content.low_confidence {
            out.push_str(LOW_OPEN);
            out.push_str(&escape(&carry(LOW_CONFIDENCE_MARKER)));
            out.push_str(SPAN_END);
            out.push('\n');
        }
        out.push_str("</section>\n");
    }
    out.push_str("</main>\n</body>\n</html>\n");
    Ok(out)
}

/// Re-read exactly the profile [`to_positioned_html`] writes.
///
/// A document outside the profile is a `VERIFY_FAILED` naming the line and the reason.
/// Read-back is the reader's only job today, and "these bytes do not match the model they
/// claim to carry" is the honest code for that; a future HTML *import* path would need its
/// own code and a real parser.
pub fn read_positioned_html(document: &str) -> Result<ReadDocument> {
    let lines: Vec<&str> = document.split('\n').collect();
    let mut at = 0usize;
    for wanted in [
        "<!doctype html>",
        "<html lang=\"en\">",
        "<head>",
        "<meta charset=\"utf-8\">",
    ] {
        expect(&lines, &mut at, wanted)?;
    }
    let title = read_title(&lines, &mut at)?;
    expect(&lines, &mut at, "<style>")?;
    for wanted in STYLE_CSS.split('\n') {
        expect(&lines, &mut at, wanted)?;
    }
    expect(&lines, &mut at, "</style>")?;
    expect(&lines, &mut at, "</head>")?;
    expect(&lines, &mut at, "<body>")?;
    expect(&lines, &mut at, "<main>")?;

    let mut pages = Vec::new();
    while lines
        .get(at)
        .is_some_and(|line| line.starts_with(SECTION_OPEN))
    {
        pages.push(read_section(&lines, &mut at)?);
    }

    expect(&lines, &mut at, "</main>")?;
    expect(&lines, &mut at, "</body>")?;
    expect(&lines, &mut at, "</html>")?;
    // The document ends with a newline, so the split leaves exactly one empty element
    // after it. Anything else is trailing content this emitter never writes.
    if lines.get(at).copied() != Some("") || lines.get(at.saturating_add(1)).is_some() {
        return Err(breaks(at, "content after </html>"));
    }
    Ok(ReadDocument { title, pages })
}

/// Prove the bytes carry the model.
///
/// Per page it compares the page number, both canvas dimensions, the low-confidence flag,
/// the span count, every span's text, and every projected edge and placement within
/// [`MAX_POSITION_ERROR_PX`]. It re-reads rather than string-compares, so a faithful
/// document written with different whitespace would still pass — while a nudged box, a
/// dropped span, a reordered page, an added section or an injected element does not.
pub fn verify_positioned_html(pages: &[HtmlPage<'_>], title: &str, document: &str) -> Result<()> {
    let got = read_positioned_html(document)?;
    if got.pages.len() != pages.len() {
        return Err(err!(
            Code::VerifyFailed,
            during = "html-verify",
            detail = format!(
                "the document has {} page sections, the model has {}",
                got.pages.len(),
                pages.len()
            )
        ));
    }
    if got.title != carry(title) {
        return Err(err!(
            Code::VerifyFailed,
            during = "html-verify",
            detail = "the title is not the one the document was written with"
        ));
    }
    for (model, read) in pages.iter().zip(got.pages.iter()) {
        if read.number != model.number {
            return Err(err!(
                Code::VerifyFailed,
                page = model.number,
                during = "html-verify",
                detail = format!("the section says data-page {}", read.number)
            ));
        }
        if read.width_px != model.width_px || read.height_px != model.height_px {
            return Err(err!(
                Code::VerifyFailed,
                page = model.number,
                during = "html-verify",
                detail = format!(
                    "the canvas reads {}x{}, the model is {}x{}",
                    read.width_px, read.height_px, model.width_px, model.height_px
                )
            ));
        }
        if read.low_confidence != model.content.low_confidence {
            return Err(err!(
                Code::VerifyFailed,
                page = model.number,
                during = "html-verify",
                detail = "the low-confidence marker is not where the model puts it"
            ));
        }
        let expected: Vec<ReadSpan> = model
            .content
            .lines
            .iter()
            .flat_map(|line| line.spans.iter())
            .map(|span| project(span, model.ctm))
            .collect();
        if read.spans.len() != expected.len() {
            return Err(err!(
                Code::VerifyFailed,
                page = model.number,
                during = "html-verify",
                detail = format!(
                    "the section carries {} spans, the model has {}",
                    read.spans.len(),
                    expected.len()
                )
            ));
        }
        for (want, have) in expected.iter().zip(read.spans.iter()) {
            if have.text != want.text {
                return Err(err!(
                    Code::VerifyFailed,
                    page = model.number,
                    during = "html-verify",
                    detail = "a span's text did not survive the round trip"
                ));
            }
            let [have_x0, have_y0, have_x1, have_y1] = have.box_px;
            let [want_x0, want_y0, want_x1, want_y1] = want.box_px;
            for (edge, (have_v, want_v)) in [
                ("box left", (have_x0, want_x0)),
                ("box top", (have_y0, want_y0)),
                ("box right", (have_x1, want_x1)),
                ("box bottom", (have_y1, want_y1)),
                ("placement", (have.left_px, want.left_px)),
                ("offset", (have.top_px, want.top_px)),
                ("size", (have.font_size_px, want.font_size_px)),
            ] {
                if (have_v - want_v).abs() > MAX_POSITION_ERROR_PX {
                    return Err(err!(
                        Code::VerifyFailed,
                        page = model.number,
                        during = "html-verify",
                        detail =
                            format!("a span's {edge} reads {have_v}, the model projects {want_v}")
                    ));
                }
            }
        }
    }
    Ok(())
}

/// The model span as the values the document is required to carry.
fn project(span: &Span, ctm: Matrix) -> ReadSpan {
    let r = span.bbox.transform(ctm);
    let box_px = [
        clamp_px(r.x0),
        clamp_px(r.y0),
        clamp_px(r.x1),
        clamp_px(r.y1),
    ];
    let [left, top, _, _] = box_px;
    ReadSpan {
        text: carry(&span.text),
        left_px: left,
        top_px: top,
        font_size_px: font_size_px(span, ctm),
        box_px,
    }
}

/// The one span line: the projected box as data, the placement and size as CSS.
fn span_markup(span: &Span, ctm: Matrix) -> String {
    let model = project(span, ctm);
    let [x0, y0, x1, y1] = model.box_px;
    format!(
        "{SPAN_OPEN}{x0},{y0},{x1},{y1}{SPAN_MID}{left}{SPAN_TOP}{top}{SPAN_SIZE}{size}{SPAN_CLOSE}{text}{SPAN_END}\n",
        x0 = num(x0),
        y0 = num(y0),
        x1 = num(x1),
        y1 = num(y1),
        left = num(model.left_px),
        top = num(model.top_px),
        size = num(model.font_size_px),
        text = escape(&model.text),
    )
}

/// The em height in device pixels: the model's size times the transform's vertical
/// magnitude — `1.0` for the unrotated 72 DPI view, the DPI scale otherwise, and
/// unchanged by a quarter-turn, which swaps the axes without changing the length.
fn font_size_px(span: &Span, ctm: Matrix) -> f64 {
    clamp_px(span.size * ctm.c.hypot(ctm.d))
}

/// A document-derived pixel coordinate, made safe to write.
fn clamp_px(v: f64) -> f64 {
    if v.is_finite() {
        v.clamp(-MAX_COORDINATE_PX, MAX_COORDINATE_PX)
    } else {
        // Only a document's own matrices can produce this. It saturates away from `NaN`
        // so the span's text still reaches the page.
        MAX_COORDINATE_PX
    }
}

/// A coordinate as the profile writes it: always two decimals.
fn num(v: f64) -> String {
    format!("{:.2}", clamp_px(v))
}

/// The text as it may live in this profile: control characters replaced by U+FFFD. This
/// is the form verification compares against, so the sanitisation is a stated part of the
/// claim rather than a silent loss.
fn carry(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if c.is_control() {
            out.push('\u{FFFD}');
        } else {
            out.push(c);
        }
    }
    out
}

/// The three entities this profile uses, and the only three the reader accepts.
///
/// Nothing document-derived ever reaches an attribute value, so no quote needs escaping
/// and no attribute can be broken out of: `&`, `<` and `>` cover the whole surface.
fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            c => out.push(c),
        }
    }
    out
}

/// The exact inverse of [`escape`]. A bare `&` or an unknown entity is not something this
/// emitter writes, so it is a refusal rather than a literal.
fn unescape(s: &str) -> Result<String> {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(hash) = rest.find('&') {
        let (plain, after) = rest.split_at(hash);
        out.push_str(plain);
        rest = if let Some(tail) = after.strip_prefix("&amp;") {
            out.push('&');
            tail
        } else if let Some(tail) = after.strip_prefix("&lt;") {
            out.push('<');
            tail
        } else if let Some(tail) = after.strip_prefix("&gt;") {
            out.push('>');
            tail
        } else {
            return Err(breaks(0, "an entity this profile does not write"));
        };
    }
    out.push_str(rest);
    Ok(out)
}

/// Read the `<title>` line.
fn read_title(lines: &[&str], at: &mut usize) -> Result<String> {
    let line = *lines
        .get(*at)
        .ok_or_else(|| breaks(*at, "the document ends before </title>"))?;
    let inner = line
        .strip_prefix("<title>")
        .and_then(|rest| rest.strip_suffix("</title>"))
        .ok_or_else(|| breaks(*at, "expected a single-line <title>"))?;
    *at = at.saturating_add(1);
    unescape(inner)
}

/// Read one `<section>` and everything it holds.
fn read_section(lines: &[&str], at: &mut usize) -> Result<ReadPage> {
    let open_at = *at;
    let line = *lines
        .get(open_at)
        .ok_or_else(|| breaks(open_at, "the document ends inside a page"))?;
    let rest = line
        .strip_prefix(SECTION_OPEN)
        .ok_or_else(|| breaks(open_at, "the section does not open the profile's way"))?;
    let (number, rest) = int_field(rest, SECTION_WIDTH, open_at, "page number")?;
    let (width, rest) = int_field(rest, SECTION_HEIGHT, open_at, "canvas width")?;
    let (height, rest) = int_field(rest, SECTION_STYLE, open_at, "canvas height")?;
    let (inline_width, rest) = int_field(rest, SECTION_TAIL, open_at, "inline canvas width")?;
    let (inline_height, tail) = int_field(rest, SECTION_CLOSE, open_at, "inline canvas height")?;
    if !tail.is_empty() {
        return Err(breaks(open_at, "an attribute this profile does not write"));
    }
    if inline_width != width || inline_height != height {
        return Err(breaks(
            open_at,
            "the section's attributes disagree with its own style",
        ));
    }
    *at = at.saturating_add(1);

    let mut spans = Vec::new();
    let mut low_confidence = false;
    loop {
        let Some(line) = lines.get(*at).copied() else {
            return Err(breaks(*at, "the document ends inside a page"));
        };
        *at = at.saturating_add(1);
        if line == "</section>" {
            break;
        }
        if let Some(rest) = line.strip_prefix(LOW_OPEN) {
            if low_confidence {
                return Err(breaks(*at, "a page carries the marker twice"));
            }
            let inner = rest
                .strip_suffix(SPAN_END)
                .ok_or_else(|| breaks(*at, "the marker line does not close"))?;
            if inner != escape(LOW_CONFIDENCE_MARKER) {
                return Err(breaks(*at, "the marker says something else"));
            }
            low_confidence = true;
            continue;
        }
        if let Some(rest) = line.strip_prefix(SPAN_OPEN) {
            spans.push(read_span(rest, *at)?);
            continue;
        }
        return Err(breaks(*at, "an element this profile does not write"));
    }
    Ok(ReadPage {
        number,
        width_px: width,
        height_px: height,
        low_confidence,
        spans,
    })
}

/// Read one span line: its four box edges, its three lengths, its text.
fn read_span(rest: &str, at: usize) -> Result<ReadSpan> {
    let (box_part, rest) = rest
        .split_once(SPAN_MID)
        .ok_or_else(|| breaks(at, "the box attribute does not close"))?;
    let edges: Vec<f64> = box_part
        .split(',')
        .map(|part| coord(part, at))
        .collect::<Result<Vec<f64>>>()?;
    let box_px: [f64; 4] =
        <[f64; 4]>::try_from(edges).map_err(|_| breaks(at, "a box must carry four edges"))?;
    let (left, rest) = coord_field(rest, SPAN_TOP, at, "left")?;
    let (top, rest) = coord_field(rest, SPAN_SIZE, at, "top")?;
    let (size, content) = coord_field(rest, SPAN_CLOSE, at, "font size")?;
    let inner = content
        .strip_suffix(SPAN_END)
        .ok_or_else(|| breaks(at, "the span does not close"))?;
    Ok(ReadSpan {
        text: unescape(inner)?,
        left_px: left,
        top_px: top,
        font_size_px: size,
        box_px,
    })
}

/// Split `PREFIX<int>TERMINATOR` off a section line: the value, then everything after the
/// terminator literal. Each attribute's name doubles as the previous value's terminator,
/// which is what keeps `612px;height:792px">` from being read as one long number.
fn int_field<'a>(rest: &'a str, terminator: &str, at: usize, what: &str) -> Result<(u32, &'a str)> {
    let (digits, tail) = rest
        .split_once(terminator)
        .ok_or_else(|| breaks(at, &format!("an unterminated {what}")))?;
    let value: u32 = digits
        .parse()
        .map_err(|_| breaks(at, &format!("a malformed {what}")))?;
    if digits != value.to_string() {
        // Canonical or refused: `07` names the same page as `7`, and a profile that
        // accepted both could not tell a written file from an edited one.
        return Err(breaks(at, &format!("a non-canonical {what}")));
    }
    Ok((value, tail))
}

/// Split `PREFIX<coord>LITERAL` off the front of a span's style, as [`int_field`] does
/// for an attribute.
fn coord_field<'a>(rest: &'a str, literal: &str, at: usize, what: &str) -> Result<(f64, &'a str)> {
    let (part, tail) = rest
        .split_once(literal)
        .ok_or_else(|| breaks(at, &format!("the {what} is not in the profile")))?;
    Ok((coord(part, at)?, tail))
}

/// One coordinate as written: an optional sign, at least one integer digit, at most
/// [`MAX_DIGITS`], a dot and exactly two decimals.
fn coord(part: &str, at: usize) -> Result<f64> {
    let unsigned = part.strip_prefix('-').unwrap_or(part);
    let (whole, frac) = unsigned
        .split_once('.')
        .ok_or_else(|| breaks(at, "a coordinate without a decimal point"))?;
    if whole.is_empty()
        || whole.len() > MAX_DIGITS
        || frac.len() != 2
        || !whole.bytes().all(|b| b.is_ascii_digit())
        || !frac.bytes().all(|b| b.is_ascii_digit())
    {
        return Err(breaks(at, "a coordinate outside the written shape"));
    }
    part.parse::<f64>()
        .map_err(|_| breaks(at, "a coordinate that will not parse"))
}

/// A profile violation, naming the line and what is wrong with it.
fn breaks(line: usize, why: &str) -> Error {
    err!(
        Code::VerifyFailed,
        during = "html-read",
        detail = format!("line {}: {why}", line.saturating_add(1))
    )
}

/// Consume one expected literal line.
fn expect(lines: &[&str], at: &mut usize, wanted: &str) -> Result<()> {
    match lines.get(*at).copied() {
        Some(line) if line == wanted => {
            *at = at.saturating_add(1);
            Ok(())
        }
        Some(line) => Err(breaks(
            *at,
            &format!("expected `{wanted}`, found `{}`", short(line)),
        )),
        None => Err(breaks(*at, &format!("expected `{wanted}`, found the end"))),
    }
}

/// A diagnostic-safe excerpt of an offending line.
fn short(line: &str) -> &str {
    line.get(..80).unwrap_or(line)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::indexing_slicing, clippy::arithmetic_side_effects)]

    use super::*;
    use crate::export::Line;
    use selis_geom::Rect;

    /// A 12 pt span on a 612×792 page, in user space.
    fn span(text: &str, x0: f64, y0: f64, x1: f64, y1: f64) -> Span {
        Span {
            text: text.to_string(),
            font: "F1".to_string(),
            size: 12.0,
            bbox: Rect::new(x0, y0, x1, y1),
        }
    }

    fn content(spans: Vec<Span>) -> Structured {
        Structured {
            lines: vec![Line {
                spans,
                bbox: Rect::new(0.0, 0.0, 100.0, 12.0),
            }],
            low_confidence: false,
        }
    }

    /// The unrotated 72 DPI view of a 612×792 `MediaBox`: one pixel per unit, y down.
    fn view_72() -> Matrix {
        Matrix::new(1.0, 0.0, 0.0, -1.0, 0.0, 792.0)
    }

    fn page<'a>(number: u32, model: &'a Structured) -> HtmlPage<'a> {
        HtmlPage {
            number,
            width_px: 612,
            height_px: 792,
            ctm: view_72(),
            content: model,
        }
    }

    /// The one span of a one-span page.
    fn only_span(model: &Structured) -> &Span {
        model
            .lines
            .first()
            .and_then(|line| line.spans.first())
            .unwrap()
    }

    fn first_span(document: &str) -> ReadSpan {
        read_positioned_html(document)
            .and_then(|read| {
                read.pages
                    .into_iter()
                    .next()
                    .and_then(|got| got.spans.into_iter().next())
                    .ok_or_else(|| breaks(0, "no span"))
            })
            .unwrap()
    }

    #[test]
    fn one_document_one_root_for_every_selected_page() {
        let a = content(vec![span("Hello", 72.0, 700.0, 300.0, 712.0)]);
        let b = content(vec![]);
        let model = [page(1, &a), page(2, &b)];
        let document = to_positioned_html(&model, "Booklet").unwrap();
        assert_eq!(document.matches("<html").count(), 1);
        assert_eq!(document.matches("</html>").count(), 1);
        assert_eq!(document.matches("<section").count(), 2);
        assert!(document.contains("data-page=\"2\""));
        assert!(document.contains("<title>Booklet</title>"));
        assert!(document.ends_with("</html>\n"));
        verify_positioned_html(&model, "Booklet", &document).unwrap();
    }

    #[test]
    fn a_span_lands_where_the_png_card_paints_it() {
        let a = content(vec![span("Hello", 72.0, 700.0, 300.0, 712.0)]);
        let document = to_positioned_html(&[page(1, &a)], "t").unwrap();
        // Device space is y-down: 792 − 712 = 80 is the top edge, 792 − 700 = 92 the bottom.
        assert!(
            document.contains("data-box=\"72.00,80.00,300.00,92.00\""),
            "{document}"
        );
        assert!(document.contains("left:72.00px;top:80.00px;font-size:12.00px"));
        let got = first_span(&document);
        assert_eq!(got.box_px, [72.0, 80.0, 300.0, 92.0]);
        assert_eq!([got.left_px, got.top_px], [72.0, 80.0]);
        assert_eq!(got.font_size_px, 12.0);
        assert_eq!(got.text, "Hello");
    }

    #[test]
    fn rotation_moves_the_box_through_the_same_transform_the_raster_uses() {
        // `rot_matrix(Clockwise90, 612, 792)` over the flipped page box, which is what a
        // `/Rotate` 90 page's `PageView::ctm` holds.
        let ctm = Matrix::new(0.0, 1.0, -1.0, 0.0, 792.0, 0.0);
        let a = content(vec![span("Rot", 72.0, 700.0, 300.0, 712.0)]);
        let model = [HtmlPage {
            number: 1,
            width_px: 792,
            height_px: 612,
            ctm,
            content: &a,
        }];
        let document = to_positioned_html(&model, "t").unwrap();
        assert!(
            document.contains("data-box=\"80.00,72.00,92.00,300.00\""),
            "{document}"
        );
        assert!(document.contains("left:80.00px;top:72.00px;font-size:12.00px"));
        verify_positioned_html(&model, "t", &document).unwrap();
    }

    #[test]
    fn dpi_scales_the_size_as_well_as_the_box() {
        // The same page at 144 DPI: two pixels per unit.
        let ctm = Matrix::new(2.0, 0.0, 0.0, -2.0, 0.0, 1584.0);
        let a = content(vec![span("Big", 72.0, 700.0, 300.0, 712.0)]);
        let model = [HtmlPage {
            number: 1,
            width_px: 1224,
            height_px: 1584,
            ctm,
            content: &a,
        }];
        let document = to_positioned_html(&model, "t").unwrap();
        assert!(document.contains("font-size:24.00px"));
        assert!(
            document.contains("data-box=\"144.00,160.00,600.00,184.00\""),
            "{document}"
        );
        verify_positioned_html(&model, "t", &document).unwrap();
    }

    #[test]
    fn text_is_escaped_and_control_characters_become_replacements() {
        let a = content(vec![span("a&b<c>d\u{7}e\nf", 0.0, 0.0, 40.0, 12.0)]);
        let model = [page(1, &a)];
        let document = to_positioned_html(&model, "t").unwrap();
        let line = document
            .split('\n')
            .find(|l| l.starts_with(SPAN_OPEN))
            .unwrap_or_default()
            .to_string();
        assert!(
            line.contains(">a&amp;b&lt;c&gt;d\u{FFFD}e\u{FFFD}f</p>"),
            "{line}"
        );
        assert_eq!(
            line.matches("</p>").count(),
            1,
            "text cannot close its own span"
        );
        verify_positioned_html(&model, "t", &document).unwrap();
        assert_eq!(first_span(&document).text, "a&b<c>d\u{FFFD}e\u{FFFD}f");
    }

    #[test]
    fn the_low_confidence_marker_rides_once_per_page() {
        let mut blank = content(vec![]);
        blank.low_confidence = true;
        let model = [page(1, &blank)];
        let document = to_positioned_html(&model, "t").unwrap();
        assert_eq!(document.matches(LOW_OPEN).count(), 1);
        assert!(read_positioned_html(&document)
            .unwrap()
            .pages
            .first()
            .is_some_and(|got| got.low_confidence));
        verify_positioned_html(&model, "t", &document).unwrap();

        // Recovered text and unrecoverable text both reach the page.
        let mut mixed = content(vec![span("Some", 0.0, 0.0, 30.0, 12.0)]);
        mixed.low_confidence = true;
        let both = to_positioned_html(&[page(1, &mixed)], "t").unwrap();
        assert!(both.contains(">Some</p>"));
        assert!(both.contains(LOW_OPEN));
        verify_positioned_html(&[page(1, &mixed)], "t", &both).unwrap();
    }

    #[test]
    fn a_zero_area_page_is_refused_rather_than_written() {
        let a = content(vec![span("Hello", 72.0, 700.0, 300.0, 712.0)]);
        let flat = HtmlPage {
            number: 3,
            width_px: 0,
            height_px: 792,
            ctm: view_72(),
            content: &a,
        };
        let error = to_positioned_html(&[flat], "t").unwrap_err();
        assert_eq!(error.code(), Code::ObjUnexpected);
        assert_eq!(error.ctx().page, Some(3));
        assert_eq!(error.ctx().during, Some("html-export"));
    }

    #[test]
    fn runaway_coordinates_saturate_and_the_text_survives() {
        let wild = Structured {
            lines: vec![Line {
                spans: vec![
                    span("widow", 0.0, 0.0, f64::INFINITY, 12.0),
                    span("void", f64::NAN, 0.0, 10.0, 12.0),
                    span("giant", 0.0, 0.0, 1.0e30, 12.0),
                ],
                bbox: Rect::new(0.0, 0.0, 1.0e30, 12.0),
            }],
            low_confidence: false,
        };
        let model = [page(1, &wild)];
        let document = to_positioned_html(&model, "t").unwrap();
        assert!(
            !document.contains("NaN") && !document.contains("inf"),
            "{document}"
        );
        // The infinity saturates into `data-box`; `Rect::transform` drops a NaN corner
        // the way the raster path's min/max does, so that span keeps its other edge.
        assert!(document.contains("10000000.00"), "{document}");
        for word in ["widow", "void", "giant"] {
            assert!(document.contains(&format!(">{word}</p>")), "{word}");
        }
        verify_positioned_html(&model, "t", &document).unwrap();
    }

    #[test]
    fn a_plain_html_paragraph_dump_is_not_this_profile() {
        // The shape this module replaces: no doctype, no canvas, one `<p>` per line.
        let sketch =
            "<html><body>\n  <p data-bbox=\"0.00,0.00,50.00,12.00\">Hello</p>\n</body></html>\n";
        let error = read_positioned_html(sketch).unwrap_err();
        assert_eq!(error.code(), Code::VerifyFailed);
        assert!(error
            .ctx()
            .detail
            .as_deref()
            .is_some_and(|d| d.contains("<!doctype html>")));
    }

    #[test]
    fn an_injected_element_is_refused() {
        let a = content(vec![span("Hello", 72.0, 700.0, 300.0, 712.0)]);
        let document = to_positioned_html(&[page(1, &a)], "t").unwrap();
        for smuggled in [
            "<img src=\"https://evil.invalid/x.gif\">",
            "<script>fetch(\"//evil.invalid\")</script>",
            "<link rel=\"stylesheet\" href=\"https://evil.invalid\">",
            "<a href=\"https://evil.invalid\">x</a>",
            "<style>@import url(https://evil.invalid);</style>",
            "<section class=\"page\" data-page=\"1\" data-width=\"612\" data-height=\"792\" style=\"width:612px;height:792px\" onmouseover=\"steal()\">",
        ] {
            let tampered = document.replace("<main>\n", &format!("<main>\n{smuggled}\n"));
            assert_ne!(tampered, document, "the fixture did not change");
            assert!(read_positioned_html(&tampered).is_err(), "{smuggled}");
        }
    }

    #[test]
    fn a_mutated_box_text_or_page_fails_verification() {
        let a = content(vec![span("Hello", 72.0, 700.0, 300.0, 712.0)]);
        let model = [page(1, &a)];
        let document = to_positioned_html(&model, "t").unwrap();
        let line = span_markup(only_span(&a), view_72());
        for tampered in [
            document.replace("data-box=\"72.00", "data-box=\"73.00"),
            document.replace(">Hello<", ">Help<"),
            document.replace(&line, ""),
            document.replace(
                "</main>",
                "<section class=\"page\" data-page=\"2\" data-width=\"612\" data-height=\"792\" style=\"width:612px;height:792px\">\n</section>\n</main>",
            ),
            document.replace("left:72.00px", "left:72.50px"),
            document.replace("font-size:12.00px", "font-size:96.00px"),
        ] {
            assert_ne!(tampered, document, "the fixture did not change");
            assert!(
                verify_positioned_html(&model, "t", &tampered).is_err(),
                "a tamper passed: {tampered}"
            );
        }
        assert!(verify_positioned_html(&model, "Other", &document).is_err());
        verify_positioned_html(&model, "t", &document).unwrap();
    }

    #[test]
    fn numbers_must_be_written_the_way_they_are_written() {
        let a = content(vec![span("Hello", 72.0, 700.0, 300.0, 712.0)]);
        let document = to_positioned_html(&[page(1, &a)], "t").unwrap();
        for tampered in [
            document.replace("left:72.00px", "left:72.0px"),
            document.replace("left:72.00px", "left:72px"),
            document.replace("left:72.00px", "left:7.2e1px"),
            document.replace("left:72.00px", "left: 72.00px"),
            document.replace("font-size:12.00px", "font-size:12.000px"),
            document.replace("80.00,300.00", "80.00, 300.00"),
            document.replace("data-page=\"1\"", "data-page=\"01\""),
        ] {
            assert_ne!(tampered, document, "the fixture did not change");
            assert!(read_positioned_html(&tampered).is_err(), "{tampered}");
        }
    }

    #[test]
    fn an_empty_selection_is_a_valid_empty_document() {
        let document = to_positioned_html(&[], "nothing").unwrap();
        assert!(document.contains("<main>\n</main>"));
        let read = read_positioned_html(&document).unwrap();
        assert!(read.pages.is_empty());
        assert_eq!(read.title, "nothing");
        verify_positioned_html(&[], "nothing", &document).unwrap();
    }

    #[test]
    fn a_title_carrying_markup_cannot_open_a_tag() {
        let sneaky = "</title><script>alert(1)</script>";
        let document = to_positioned_html(&[], sneaky).unwrap();
        assert_eq!(document.matches("<title>").count(), 1);
        assert!(!document.contains("<script>"));
        verify_positioned_html(&[], sneaky, &document).unwrap();
        assert_eq!(read_positioned_html(&document).unwrap().title, sneaky);
    }

    #[test]
    fn escapes_are_the_only_entities_the_reader_knows() {
        assert_eq!(unescape("a&amp;b&lt;c&gt;d").unwrap(), "a&b<c>d");
        assert!(unescape("a & b").is_err());
        assert!(unescape("&nbsp;").is_err());
    }
}
