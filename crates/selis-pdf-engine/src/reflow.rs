//! Model-based text edit + re-layout + re-emit (SL-5.TEXTEDIT.02).
//!
//! This is the orchestration half of the largest Phase-5 text-edit gap. It
//! turns a `(page, paragraph id, run text, replacement)` request into a byte
//! region edit:
//!
//! 1. reconstruct the page's paragraph model with confidence
//!    ([`Session::infer_paragraphs`](crate::Session::infer_paragraphs)) and
//!    **refuse** a paragraph below the provisional threshold, pointing the
//!    caller at the box-level fallback (`selis_pdf_edit::edit_box`);
//! 2. locate the exact byte span of the paragraph's show-text operators in the
//!    decoded content stream;
//! 3. re-shape / re-break the replacement text with the page font's own metrics
//!    and UAX #14 line opportunities ([`selis_shape::line_breaks`]);
//! 4. emit new show-text operators for only that region and, when the edit
//!    needs a glyph the embedded subset lacks, extend the subset with
//!    [`selis_font::subset::add_glyph`] and widen the font dictionary;
//! 5. hand the region + font objects to
//!    [`selis_pdf_edit::reflow_regions`], which splices the bytes and saves an
//!    incremental update with the original bytes a verbatim prefix.
//!
//! # What is deliberately out of scope (honest gaps)
//!
//! * **Simple, single-byte fonts only.** Two-byte CID / `/ToUnicode` fonts are
//!   refused with [`ReflowError::Invalid`] rather than silently mangled — the
//!   in-place path (`selis_pdf_edit::edit_text`) already handles their codes,
//!   but re-laying-out them out needs a CID-aware emitter that is not here.
//! * **Caller-supplied new glyph outlines.** A replacement character absent
//!   from the subset needs an outline to add. The caller supplies it as a
//!   [`NewGlyph`] (sourced from a matched face via
//!   [`selis_font::font_match`]); this slice does not itself fetch a substitute
//!   font program. If a face cannot be extended, the font-match disclosure is
//!   surfaced in the refusal — never a silent fallback.
//! * **Horizontal, top-to-bottom paragraphs**, matching the paragraph model's
//!   calibration. Vertical writing is not re-flowed here.
#![forbid(unsafe_code)]

use std::collections::BTreeMap;
use std::fmt;
use std::ops::Range;

use selis_bytes::Bytes;
use selis_font::model::{FontDict, FontFile, FontSubtype};
use selis_font::outline::OutlineCmd;
use selis_pdf_cos::copy::resolve_ref;
use selis_pdf_cos::{Obj, Ref};
use selis_pdf_edit::reflow_edit::MIN_REFLOW_CONFIDENCE;
use selis_pdf_edit::{
    ensure_reflow_confidence, reflow_regions, ContentRegion, ReflowEditError, ReflowMutation,
};
use selis_sandbox::{Budget, BudgetGuard, CancelToken, Clock};

use crate::Session;

/// A glyph the caller wants added to the embedded subset. `outline` must be a
/// simple (Move/Line/Close) outline in font units; curves are refused by the
/// subsetter rather than guessed. `advance` is in font units.
#[derive(Debug, Clone, PartialEq)]
pub struct NewGlyph {
    /// The character whose code (its Unicode scalar, for a simple font) maps to
    /// the new glyph.
    pub ch: char,
    /// A flattened simple outline.
    pub outline: Vec<OutlineCmd>,
    /// Advance width in font units.
    pub advance: u16,
}

/// Why a model-based reflow was refused.
#[derive(Debug, Clone, PartialEq)]
pub enum ReflowError {
    /// The paragraph's reconstruction confidence is too low: use the box path.
    LowConfidence {
        /// The confidence the model reported.
        confidence: f64,
        /// The provisional threshold that was not cleared.
        threshold: f64,
        /// The suggested fallback.
        suggestion: &'static str,
    },
    /// No paragraph with that index exists on the page.
    ParagraphNotFound(usize),
    /// The named run was not found in the paragraph's recovered text.
    RunNotFound(String),
    /// The page/font shape is outside this slice (e.g. a CID font, or a face
    /// that could not be extended — the message carries the font-match
    /// disclosure in the latter case).
    Invalid(String),
}

impl fmt::Display for ReflowError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::LowConfidence {
                confidence,
                threshold,
                suggestion,
            } => write!(
                f,
                "paragraph confidence {confidence:.3} < reflow threshold {threshold:.3}; {suggestion}"
            ),
            Self::ParagraphNotFound(n) => write!(f, "no paragraph {n} on the page"),
            Self::RunNotFound(s) => write!(f, "run {s:?} not found in the paragraph"),
            Self::Invalid(s) => f.write_str(s),
        }
    }
}

impl std::error::Error for ReflowError {}

/// What a model-based reflow produced.
#[derive(Debug, Clone, PartialEq)]
pub struct ReflowReport {
    /// The edited 0-based page.
    pub page: usize,
    /// The edited 0-based paragraph.
    pub paragraph: usize,
    /// The model's confidence in the paragraph reconstruction.
    pub confidence: f64,
    /// The decoded-content byte span that was replaced.
    pub changed_span: Range<usize>,
    /// How many lines the reflowed paragraph now occupies.
    pub lines_out: usize,
    /// Input size in bytes.
    pub bytes_in: u64,
    /// Output size in bytes.
    pub bytes_out: u64,
    /// Whether the embedded font program was extended.
    pub font_extended: bool,
}

/// Reflow one paragraph of `src` and return the incrementally-updated document.
///
/// `run_text` is located in the paragraph's recovered text and replaced once
/// with `replacement`. The affected paragraph's show-text operators are
/// re-emitted; every other byte of the content stream is unchanged, and the
/// original document bytes are a verbatim prefix of the output.
///
/// # Budget
///
/// The whole pipeline is charged to `g`: paragraph inference, content decode,
/// measurement, shaping and the incremental save. The decoded content stream
/// is bounded by the edit crate's 64 MiB cap.
///
/// # Malformed Input
///
/// A low-confidence paragraph, a missing paragraph/run, a CID font, an
/// unreadable font program or a region outside the content stream are typed
/// refusals — no bytes are produced. `new_glyphs` with a curved outline is
/// refused by the subsetter and surfaced as an [`ReflowError::Invalid`] with a
/// disclosure rather than a partial font.
///
/// # Errors
///
/// The [`ReflowError`] variants above. In particular a face that cannot carry
/// a needed character is disclosed in the [`ReflowError::Invalid`] message —
/// there is no silent face substitution (SL-5.TEXTEDIT.04).
pub fn reflow_paragraph(
    src: &[u8],
    page: usize,
    paragraph: usize,
    run_text: &str,
    replacement: &str,
    new_glyphs: &[NewGlyph],
    budget: &Budget,
    clock: &dyn Clock,
) -> Result<(Vec<u8>, ReflowReport), ReflowError> {
    if run_text.is_empty() {
        return Err(ReflowError::Invalid("the run text is empty".to_string()));
    }

    // 1. Model + confidence gate.
    let session = Session::open(src.to_vec(), budget, clock)
        .map_err(|e| ReflowError::Invalid(format!("cannot open: {e}")))?;
    let mut g = budget.guard_with(clock, CancelToken::new());
    let (paragraphs, confidence) = session
        .infer_paragraphs(page, budget, &mut g)
        .map_err(|e| ReflowError::Invalid(format!("cannot infer paragraphs: {e}")))?;
    if let Err(ReflowEditError::LowConfidence { .. }) = ensure_reflow_confidence(confidence.value) {
        return Err(ReflowError::LowConfidence {
            confidence: confidence.value,
            threshold: MIN_REFLOW_CONFIDENCE,
            suggestion: "use selis_pdf_edit::edit_box (SL-5.TEXTEDIT.03) instead",
        });
    }
    let para = paragraphs
        .get(paragraph)
        .ok_or(ReflowError::ParagraphNotFound(paragraph))?;
    let at = para
        .text
        .find(run_text)
        .ok_or_else(|| ReflowError::RunNotFound(run_text.to_string()))?;
    let mut new_text = String::with_capacity(para.text.len().saturating_add(replacement.len()));
    new_text.push_str(para.text.get(..at).unwrap_or(""));
    new_text.push_str(replacement);
    new_text.push_str(
        para.text
            .get(at.saturating_add(run_text.len())..)
            .unwrap_or(""),
    );

    // 2. Decode the content and locate the paragraph's show-text operators.
    let (decoded, page_dict, doc) = decode_page_content(src, page, budget, &mut g)?;
    let ops = scan_text_ops(&decoded);
    let window = locate_paragraph_ops(&ops, para.lines.len(), &para.text)
        .ok_or_else(|| ReflowError::Invalid("the paragraph's show-text operators were not found; a CID or re-encoded font may need the box path".to_string()))?;
    let first = ops
        .get(window.first)
        .ok_or_else(|| ReflowError::Invalid("the first show-text operator vanished".to_string()))?;
    let last = ops
        .get(window.last)
        .ok_or_else(|| ReflowError::Invalid("the last show-text operator vanished".to_string()))?;
    if is_cid_op(first) {
        return Err(ReflowError::Invalid(
            "model-based reflow of a two-byte CID / ToUnicode font is not supported; use the box path (SL-5.TEXTEDIT.03)".to_string(),
        ));
    }
    let span = first.block_start..last.block_end;
    if span.start > span.end || span.end > decoded.len() {
        return Err(ReflowError::Invalid(
            "the paragraph region is outside the content stream".to_string(),
        ));
    }

    // 3. Build a metric measurer for the paragraph's font and re-break lines.
    let font_name = first.font.clone();
    let measurer = measurer_for(
        src, &doc, &page_dict, &font_name, new_glyphs, budget, &mut g,
    )?;
    let target_width = measurer.advance(&first.text);
    let lines = wrap_text(&new_text, target_width, &measurer);
    if lines.is_empty() {
        return Err(ReflowError::Invalid(
            "the replacement text produced no lines".to_string(),
        ));
    }

    // 4. Re-emit only that region. Baselines follow the paragraph's own leading.
    let leading = if window.last > window.first {
        let y0 = first.y;
        let y1 = ops
            .get(window.first.saturating_add(1))
            .map(|op| op.y)
            .unwrap_or(y0);
        (y0 - y1).abs()
    } else {
        first.size
    };
    let leading = if leading.is_finite() && leading > 0.0 {
        leading
    } else {
        first.size
    };
    let mut emitted = Vec::new();
    for (index, line) in lines.iter().enumerate() {
        if index > 0 {
            emitted.push(b'\n');
        }
        let y = first.y - leading * (index as f64);
        let op = format!(
            "BT /{} {} Tf {} {} Td ({}) Tj ET",
            String::from_utf8_lossy(&font_name),
            format_num(first.size),
            format_num(first.x),
            format_num(y),
            escape_literal(line)
        );
        emitted.extend_from_slice(op.as_bytes());
    }

    // 5. Extend the embedded subset when new glyphs are needed.
    let mut mutations: Vec<ReflowMutation> = vec![ReflowMutation::Content(ContentRegion {
        span: span.clone(),
        replacement: emitted,
    })];
    let mut font_extended = false;
    if !new_glyphs.is_empty() {
        match extend_font(
            src, &doc, &page_dict, &font_name, new_glyphs, budget, &mut g,
        ) {
            Ok(Some(objects)) => {
                for (num, value) in objects {
                    mutations.push(ReflowMutation::SetObject { num, value });
                }
                font_extended = true;
            }
            Ok(None) | Err(_) => {
                // A face that cannot be extended must be disclosed, never
                // silently swapped. The font-match layer supplies the wording.
                let name = String::from_utf8_lossy(&font_name).into_owned();
                let needed: Vec<char> = new_glyphs.iter().map(|g| g.ch).collect();
                let original = selis_font::font_match::OriginalFont::embedded(&name, &[]);
                let sub = selis_font::font_match::match_substitute(&original, &needed, budget);
                return Err(ReflowError::Invalid(format!(
                    "the embedded face {name} could not be extended with the new glyphs; {}",
                    sub.disclosure
                )));
            }
        }
    }

    let page_u32 = u32::try_from(page).unwrap_or(u32::MAX);
    let (out, report) = reflow_regions(src, page_u32, &mutations, budget, &mut g)
        .map_err(|e| ReflowError::Invalid(format!("the byte-region edit failed: {e}")))?;
    Ok((
        out,
        ReflowReport {
            page,
            paragraph,
            confidence: confidence.value,
            changed_span: span,
            lines_out: lines.len(),
            bytes_in: report.bytes_in,
            bytes_out: report.bytes_out,
            font_extended,
        },
    ))
}

// ---------------------------------------------------------------------------
// Content scanning
// ---------------------------------------------------------------------------

/// One show-text operator, with the byte span of its `BT … ET` block.
#[derive(Debug, Clone)]
struct TextOp {
    block_start: usize,
    block_end: usize,
    text: String,
    font: Vec<u8>,
    size: f64,
    x: f64,
    y: f64,
}

/// The inclusive range of operator indices belonging to one paragraph.
struct OpWindow {
    first: usize,
    last: usize,
}

/// Scan a decoded content stream for simple show-text operators, tracking the
/// `BT … ET` block, active font/size and text position.
///
/// # Budget
///
/// Linear in the content length: one forward pass, one buffer of text ops.
///
/// # Malformed Input
///
/// Unbalanced delimiters and malformed numbers are skipped rather than indexed
/// or panicked on; a string with no operator contributes no text op.
fn scan_text_ops(content: &[u8]) -> Vec<TextOp> {
    let mut ops: Vec<TextOp> = Vec::new();
    let mut i = 0usize;
    let mut block_start = 0usize;
    let mut in_block = false;
    let mut last_name: Vec<u8> = Vec::new();
    let mut font: Vec<u8> = Vec::new();
    let mut size = 0.0f64;
    let mut x = 0.0f64;
    let mut y = 0.0f64;
    let mut numbers: Vec<f64> = Vec::new();
    let mut strings: Vec<String> = Vec::new();
    while i < content.len() {
        let b = content.get(i).copied().unwrap_or(0);
        match b {
            b'(' => {
                let (text, next) = read_literal(content, i);
                strings.push(text);
                i = next;
            }
            b'<' => {
                if content.get(i.saturating_add(1)).copied() == Some(b'<') {
                    i = i.saturating_add(2);
                    continue;
                }
                let (text, next) = read_hex_string(content, i);
                strings.push(text);
                i = next;
            }
            b'/' => {
                let (name, next) = read_name(content, i);
                last_name = name;
                i = next;
            }
            b'%' => {
                while i < content.len()
                    && !matches!(content.get(i).copied(), Some(b'\n') | Some(b'\r'))
                {
                    i = i.saturating_add(1);
                }
            }
            b'+' | b'-' | b'.' | b'0'..=b'9' => {
                let (value, next) = read_number(content, i);
                numbers.push(value);
                i = next;
            }
            b if is_token_start(b) => {
                let token_start = i;
                let (word, next) = read_word(content, i);
                i = next;
                match word.as_slice() {
                    b"BT" => {
                        block_start = token_start;
                        in_block = true;
                    }
                    b"ET" => {
                        if in_block {
                            if let Some(last) = ops.last_mut() {
                                last.block_end = i;
                            }
                        }
                        in_block = false;
                    }
                    b"Tf" => {
                        font = last_name.clone();
                        size = numbers.first().copied().unwrap_or(0.0);
                    }
                    b"Td" | b"TD" => {
                        x = numbers.first().copied().unwrap_or(x);
                        y = numbers.get(1).copied().unwrap_or(y);
                    }
                    b"Tj" | b"'" | b"\"" => {
                        if let Some(text) = strings.last() {
                            ops.push(TextOp {
                                block_start,
                                block_end: i,
                                text: text.clone(),
                                font: font.clone(),
                                size,
                                x,
                                y,
                            });
                        }
                    }
                    _ => {}
                }
                numbers.clear();
                strings.clear();
                last_name.clear();
            }
            _ => {
                i = i.saturating_add(1);
            }
        }
    }
    ops
}

fn is_token_start(b: u8) -> bool {
    !matches!(
        b,
        0x00..=0x20 | b'(' | b')' | b'<' | b'>' | b'[' | b']' | b'{' | b'}' | b'/' | b'%'
    )
}

fn read_word(content: &[u8], at: usize) -> (Vec<u8>, usize) {
    let mut i = at;
    let mut out = Vec::new();
    while i < content.len() && is_token_start(content.get(i).copied().unwrap_or(0)) {
        out.push(content.get(i).copied().unwrap_or(0));
        i = i.saturating_add(1);
    }
    (out, i)
}

fn read_name(content: &[u8], at: usize) -> (Vec<u8>, usize) {
    let mut i = at.saturating_add(1);
    let mut out = Vec::new();
    while i < content.len() && is_token_start(content.get(i).copied().unwrap_or(0)) {
        out.push(content.get(i).copied().unwrap_or(0));
        i = i.saturating_add(1);
    }
    (out, i)
}

fn read_number(content: &[u8], at: usize) -> (f64, usize) {
    let mut i = at;
    let mut out = Vec::new();
    while i < content.len()
        && matches!(
            content.get(i).copied(),
            Some(b'+') | Some(b'-') | Some(b'.') | Some(b'0'..=b'9')
        )
    {
        out.push(content.get(i).copied().unwrap_or(0));
        i = i.saturating_add(1);
    }
    let value = std::str::from_utf8(&out)
        .ok()
        .and_then(|s| s.parse::<f64>().ok())
        .filter(|v| v.is_finite())
        .unwrap_or(0.0);
    (value, i)
}

/// Read a literal `(…)` string, decoding the escapes the PDF spec defines.
fn read_literal(content: &[u8], at: usize) -> (String, usize) {
    let mut i = at.saturating_add(1);
    let mut depth = 1usize;
    let mut bytes = Vec::new();
    while i < content.len() && depth > 0 {
        let b = content.get(i).copied().unwrap_or(0);
        i = i.saturating_add(1);
        match b {
            b'(' => {
                depth = depth.saturating_add(1);
                bytes.push(b);
            }
            b')' => {
                depth = depth.saturating_sub(1);
                if depth > 0 {
                    bytes.push(b);
                }
            }
            b'\\' => {
                let escaped = content.get(i).copied().unwrap_or(0);
                i = i.saturating_add(1);
                match escaped {
                    b'n' => bytes.push(b'\n'),
                    b'r' => bytes.push(b'\r'),
                    b't' => bytes.push(b'\t'),
                    b'b' => bytes.push(0x08),
                    b'f' => bytes.push(0x0c),
                    b'\r' => {
                        if content.get(i).copied() == Some(b'\n') {
                            i = i.saturating_add(1);
                        }
                    }
                    b'\n' => {}
                    b'0'..=b'7' => {
                        let mut oct = u16::from(escaped.saturating_sub(b'0'));
                        for _ in 0..2 {
                            match content.get(i).copied() {
                                Some(d @ b'0'..=b'7') => {
                                    oct = oct
                                        .saturating_mul(8)
                                        .saturating_add(u16::from(d.saturating_sub(b'0')));
                                    i = i.saturating_add(1);
                                }
                                _ => break,
                            }
                        }
                        bytes.push(u8::try_from(oct).unwrap_or(0));
                    }
                    other => bytes.push(other),
                }
            }
            other => bytes.push(other),
        }
    }
    (latin1(&bytes), i)
}

fn read_hex_string(content: &[u8], at: usize) -> (String, usize) {
    let mut i = at.saturating_add(1);
    let mut high: Option<u8> = None;
    let mut bytes = Vec::new();
    while i < content.len() {
        let b = content.get(i).copied().unwrap_or(0);
        i = i.saturating_add(1);
        if b == b'>' {
            break;
        }
        let nibble = match b {
            b'0'..=b'9' => b.saturating_sub(b'0'),
            b'a'..=b'f' => b.saturating_sub(b'a').saturating_add(10),
            b'A'..=b'F' => b.saturating_sub(b'A').saturating_add(10),
            _ => continue,
        };
        if let Some(h) = high.take() {
            bytes.push((h << 4) | nibble);
        } else {
            high = Some(nibble);
        }
    }
    (latin1(&bytes), i)
}

fn latin1(bytes: &[u8]) -> String {
    bytes.iter().copied().map(char::from).collect()
}

/// Find a contiguous window of `count` ops whose joined text equals the
/// paragraph's recovered text.
fn locate_paragraph_ops(ops: &[TextOp], count: usize, text: &str) -> Option<OpWindow> {
    if count == 0 || ops.len() < count {
        return None;
    }
    let max_start = ops.len().saturating_sub(count);
    for start in 0..=max_start {
        let end = start.saturating_add(count);
        let mut joined = String::new();
        for op in ops.get(start..end)? {
            if !joined.is_empty() {
                joined.push(' ');
            }
            joined.push_str(&op.text);
        }
        if joined == text {
            return Some(OpWindow {
                first: start,
                last: end.saturating_sub(1),
            });
        }
    }
    None
}

fn is_cid_op(op: &TextOp) -> bool {
    op.text.chars().any(|c| (c as u32) > 0xFF)
}

fn escape_literal(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '(' => out.push_str("\\("),
            ')' => out.push_str("\\)"),
            other => out.push(other),
        }
    }
    out
}

fn format_num(value: f64) -> String {
    if !value.is_finite() {
        return "0".to_string();
    }
    let mut s = format!("{value:.3}");
    if s.contains('.') {
        while s.ends_with('0') {
            s.pop();
        }
        if s.ends_with('.') {
            s.pop();
        }
    }
    if s == "-0" {
        "0".to_string()
    } else {
        s
    }
}

// ---------------------------------------------------------------------------
// Content decode + COS helpers
// ---------------------------------------------------------------------------

/// The leaf `/Page` object reference at `index` in reading order. Depth-bounded
/// against a cycle in a hostile page tree.
fn leaf_page_ref(
    src: &[u8],
    doc: &selis_pdf_cos::Doc,
    index: usize,
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
) -> Result<Ref, ReflowError> {
    let root = doc
        .revisions()
        .last()
        .and_then(|r| r.root)
        .ok_or_else(|| ReflowError::Invalid("no /Root in the trailer".to_string()))?;
    let Obj::Dict(catalog) = resolve_ref(src, doc, root, budget, g)
        .map_err(|e| ReflowError::Invalid(format!("cannot read the catalog: {e}")))?
    else {
        return Err(ReflowError::Invalid(
            "the catalog is not a dictionary".to_string(),
        ));
    };
    let pages_ref = dict_get(&catalog, b"Pages")
        .and_then(obj_ref)
        .ok_or_else(|| ReflowError::Invalid("the catalog has no /Pages".to_string()))?;
    let mut found = Vec::new();
    collect_pages(src, doc, pages_ref, &mut found, 0, budget, g)?;
    found
        .get(index)
        .copied()
        .ok_or(ReflowError::ParagraphNotFound(index))
}

fn collect_pages(
    src: &[u8],
    doc: &selis_pdf_cos::Doc,
    node: Ref,
    out: &mut Vec<Ref>,
    depth: u32,
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
) -> Result<(), ReflowError> {
    if depth > 64 || out.len() > 100_000 {
        return Ok(());
    }
    let Obj::Dict(dict) = resolve_ref(src, doc, node, budget, g)
        .map_err(|e| ReflowError::Invalid(format!("cannot read a page node: {e}")))?
    else {
        return Ok(());
    };
    if dict_get(&dict, b"Type").and_then(|v| match v {
        Obj::Name(n) => Some(n.as_slice()),
        _ => None,
    }) == Some(b"Pages")
    {
        if let Some(Obj::Array(kids)) = dict_get(&dict, b"Kids") {
            for kid in kids {
                if let Some(r) = obj_ref(kid) {
                    collect_pages(src, doc, r, out, depth.saturating_add(1), budget, g)?;
                }
            }
        }
    } else {
        out.push(node);
    }
    Ok(())
}

/// A page's content-stream references, in draw order.
fn content_refs(page_dict: &[(Bytes, Obj)]) -> Option<Vec<Ref>> {
    match dict_get(page_dict, b"Contents")? {
        Obj::Ref(r) => Some(vec![*r]),
        Obj::Array(items) => {
            let mut out = Vec::with_capacity(items.len());
            for item in items {
                out.push(obj_ref(item)?);
            }
            if out.is_empty() {
                None
            } else {
                Some(out)
            }
        }
        _ => None,
    }
}

/// Decode a page's content streams into one operator run, as the edit layer
/// does (streams joined with a newline). Also returns the page dictionary and
/// the parsed document for the font-resolution step.
fn decode_page_content(
    src: &[u8],
    page: usize,
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
) -> Result<(Vec<u8>, Vec<(Bytes, Obj)>, selis_pdf_cos::Doc), ReflowError> {
    let startxref = selis_pdf_cos::xref::find_startxref(src, 4096).unwrap_or(0);
    let doc = selis_pdf_cos::parse_revisions(src, startxref, budget, g)
        .map_err(|e| ReflowError::Invalid(format!("cannot open: {e}")))?;
    let page_ref = leaf_page_ref(src, &doc, page, budget, g)?;
    let page_obj = resolve_ref(src, &doc, page_ref, budget, g)
        .map_err(|e| ReflowError::Invalid(format!("cannot read the page: {e}")))?;
    let Obj::Dict(page_dict) = page_obj else {
        return Err(ReflowError::Invalid(
            "the page is not a dictionary".to_string(),
        ));
    };
    let refs = content_refs(&page_dict)
        .ok_or_else(|| ReflowError::Invalid("the page has no content stream".to_string()))?;
    let mut decoded = Vec::new();
    for (index, r) in refs.iter().enumerate() {
        let obj = resolve_ref(src, &doc, *r, budget, g)
            .map_err(|e| ReflowError::Invalid(format!("cannot read content: {e}")))?;
        let Obj::Stream { dict, data } = obj else {
            return Err(ReflowError::Invalid("content is not a stream".to_string()));
        };
        let bytes = decode_stream(&dict, data.as_slice(), g)?;
        if index > 0 {
            decoded.push(b'\n');
        }
        decoded.extend_from_slice(&bytes);
    }
    Ok((decoded, page_dict, doc))
}

/// Decode one stream honouring `/Filter` (raw / FlateDecode only).
fn decode_stream(
    dict: &[(Bytes, Obj)],
    data: &[u8],
    g: &mut BudgetGuard<'_>,
) -> Result<Vec<u8>, ReflowError> {
    let flate = dict.iter().any(|(k, v)| {
        k.as_slice() == b"Filter"
            && matches!(v, Obj::Name(n) if n.as_slice() == b"FlateDecode" || n.as_slice() == b"Fl")
    });
    if flate {
        selis_pdf_filter::decode("FlateDecode", data, 64 * 1024 * 1024, g)
            .map_err(|e| ReflowError::Invalid(format!("cannot decode stream: {e}")))
    } else {
        Ok(data.to_vec())
    }
}

/// Look up a page font's object reference by resource name, resolving indirect
/// `/Resources` and `/Font` dictionaries.
fn font_ref(
    src: &[u8],
    doc: &selis_pdf_cos::Doc,
    page_dict: &[(Bytes, Obj)],
    name: &[u8],
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
) -> Option<Ref> {
    let resources = dict_get(page_dict, b"Resources")?;
    let resources = match resources {
        Obj::Ref(r) => resolve_ref(src, doc, *r, budget, g).ok()?,
        other => other.clone(),
    };
    let Obj::Dict(res) = &resources else {
        return None;
    };
    let fonts_value = dict_get(res, b"Font")?;
    let fonts = match fonts_value {
        Obj::Ref(r) => resolve_ref(src, doc, *r, budget, g).ok()?,
        other => other.clone(),
    };
    let Obj::Dict(fonts) = &fonts else {
        return None;
    };
    obj_ref(dict_get(fonts, name)?)
}

fn obj_ref(value: &Obj) -> Option<Ref> {
    match value {
        Obj::Ref(r) => Some(*r),
        _ => None,
    }
}

fn dict_get<'a>(dict: &'a [(Bytes, Obj)], key: &[u8]) -> Option<&'a Obj> {
    dict.iter()
        .find(|(k, _)| k.as_slice() == key)
        .map(|(_, v)| v)
}

fn set_dict_entry(dict: &mut Vec<(Bytes, Obj)>, key: &[u8], value: Obj) {
    if let Some((_, slot)) = dict.iter_mut().find(|(k, _)| k.as_slice() == key) {
        *slot = value;
    } else {
        dict.push((Bytes::copy_from_slice(key), value));
    }
}

// ---------------------------------------------------------------------------
// Measurement + wrapping
// ---------------------------------------------------------------------------

/// A per-code advance measurer built from the page font dictionary.
struct Measurer {
    resolved: selis_font::width::ResolvedFont,
    extra: BTreeMap<char, f64>,
}

impl Measurer {
    fn advance(&self, text: &str) -> f64 {
        text.chars()
            .map(|c| {
                self.extra
                    .get(&c)
                    .copied()
                    .unwrap_or_else(|| self.resolved.width(u32::from(c)))
            })
            .sum()
    }
}

fn measurer_for(
    src: &[u8],
    doc: &selis_pdf_cos::Doc,
    page_dict: &[(Bytes, Obj)],
    font_name: &[u8],
    new_glyphs: &[NewGlyph],
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
) -> Result<Measurer, ReflowError> {
    // A standard-14 font selected by name with no /Font resource (common in
    // simple fixtures) still has a real metric table — seed the model with the
    // name the content selected so measurement does not silently become zero.
    let mut fd = FontDict::simple(FontSubtype::Type1);
    fd.base_font = String::from_utf8_lossy(font_name).into_owned();
    if let Some(r) = font_ref(src, doc, page_dict, font_name, budget, g) {
        if let Ok(Obj::Dict(font_dict)) = resolve_ref(src, doc, r, budget, g) {
            if let Some(Obj::Name(base)) = dict_get(&font_dict, b"BaseFont") {
                fd.base_font = String::from_utf8_lossy(base.as_slice()).into_owned();
            }
            fd.subtype = match dict_get(&font_dict, b"Subtype").and_then(|v| match v {
                Obj::Name(n) => Some(n.as_slice()),
                _ => None,
            }) {
                Some(b"TrueType") => FontSubtype::TrueType,
                Some(b"Type0") | Some(b"CIDFontType0") | Some(b"CIDFontType2") => {
                    return Err(ReflowError::Invalid(
                        "model-based reflow of a composite/CID font is not supported".to_string(),
                    ));
                }
                _ => FontSubtype::Type1,
            };
            if let Some(Obj::Int(v)) = dict_get(&font_dict, b"FirstChar") {
                fd.first_char = u32::try_from(*v).unwrap_or(0);
            }
            if let Some(Obj::Int(v)) = dict_get(&font_dict, b"LastChar") {
                fd.last_char = u32::try_from(*v).unwrap_or(255);
            }
            if let Some(Obj::Array(items)) = dict_get(&font_dict, b"Widths") {
                fd.widths = Some(items.iter().map(|v| number_of(v).unwrap_or(0.0)).collect());
            }
            if let Some(desc_ref) = dict_get(&font_dict, b"FontDescriptor").and_then(obj_ref) {
                if let Ok(Obj::Dict(desc)) = resolve_ref(src, doc, desc_ref, budget, g) {
                    if let Some(ff_ref) = dict_get(&desc, b"FontFile2").and_then(obj_ref) {
                        if let Ok(Obj::Stream { dict, data }) =
                            resolve_ref(src, doc, ff_ref, budget, g)
                        {
                            let bytes = decode_stream(&dict, data.as_slice(), g)?;
                            fd.font_file = Some(FontFile::TrueType(Bytes::copy_from_slice(&bytes)));
                        }
                    }
                }
            }
        }
    }
    let resolved = selis_font::width::resolve(&fd, g)
        .map_err(|e| ReflowError::Invalid(format!("cannot resolve widths: {e}")))?;
    let mut extra = BTreeMap::new();
    for ng in new_glyphs {
        extra.insert(ng.ch, f64::from(ng.advance));
    }
    Ok(Measurer { resolved, extra })
}

/// A glyph-space width as a three-decimal PDF real. Widths are non-negative
/// and small; the narrowing is representable and saturates for non-finite input.
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn width_obj(w: f64) -> Obj {
    let v = (w * 1000.0).round();
    Obj::Real {
        scaled: if v.is_finite() { v as i64 } else { 0 },
        scale: 3,
    }
}

fn number_of(value: &Obj) -> Option<f64> {
    match value {
        Obj::Int(v) => Some(*v as f64),
        Obj::Real { scaled, scale } => Some(*scaled as f64 / 10f64.powi(*scale as i32)),
        _ => None,
    }
}

/// Greedy UAX #14 wrapping against a width budget.
fn wrap_text(text: &str, max_width: f64, m: &Measurer) -> Vec<String> {
    let breaks = selis_shape::line_breaks(text);
    let mut lines = Vec::new();
    let mut start = 0usize;
    let mut last_fit = 0usize;
    for br in &breaks {
        let at = br.byte;
        if at <= start || at > text.len() {
            continue;
        }
        let candidate = text.get(start..at).unwrap_or("");
        if m.advance(candidate) <= max_width || last_fit == start {
            last_fit = at;
            if br.mandatory {
                break;
            }
        } else {
            if let Some(line) = text.get(start..last_fit) {
                lines.push(line.trim_end().to_string());
            }
            start = last_fit;
            last_fit = at;
        }
    }
    if start < text.len() {
        if let Some(line) = text.get(start..) {
            lines.push(line.trim_end().to_string());
        }
    }
    lines
}

// ---------------------------------------------------------------------------
// Font-subset extension
// ---------------------------------------------------------------------------

/// Extend the page font's embedded subset with the new glyphs and widen its
/// `/Widths` + `/LastChar`. Returns the objects to redefine.
fn extend_font(
    src: &[u8],
    doc: &selis_pdf_cos::Doc,
    page_dict: &[(Bytes, Obj)],
    font_name: &[u8],
    new_glyphs: &[NewGlyph],
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
) -> Result<Option<Vec<(u32, Obj)>>, ReflowError> {
    let font_obj_ref = match font_ref(src, doc, page_dict, font_name, budget, g) {
        Some(r) => r,
        None => return Ok(None),
    };
    let Obj::Dict(mut font_dict) = resolve_ref(src, doc, font_obj_ref, budget, g)
        .map_err(|_| ReflowError::Invalid("cannot read the font dictionary".to_string()))?
    else {
        return Ok(None);
    };
    let desc_ref = match dict_get(&font_dict, b"FontDescriptor").and_then(obj_ref) {
        Some(r) => r,
        None => return Ok(None),
    };
    let Obj::Dict(mut desc) = resolve_ref(src, doc, desc_ref, budget, g)
        .map_err(|_| ReflowError::Invalid("cannot read the font descriptor".to_string()))?
    else {
        return Ok(None);
    };
    let ff_ref = match dict_get(&desc, b"FontFile2").and_then(obj_ref) {
        Some(r) => r,
        None => return Ok(None),
    };
    let Obj::Stream {
        dict: ff_dict,
        data: ff_data,
    } = resolve_ref(src, doc, ff_ref, budget, g)
        .map_err(|_| ReflowError::Invalid("cannot read the font program".to_string()))?
    else {
        return Ok(None);
    };
    let mut program = Bytes::copy_from_slice(&decode_stream(&ff_dict, ff_data.as_slice(), g)?);

    let first_char = dict_get(&font_dict, b"FirstChar")
        .and_then(|v| {
            if let Obj::Int(n) = v {
                u32::try_from(*n).ok()
            } else {
                None
            }
        })
        .unwrap_or(0);
    let last_char = dict_get(&font_dict, b"LastChar")
        .and_then(|v| {
            if let Obj::Int(n) = v {
                u32::try_from(*n).ok()
            } else {
                None
            }
        })
        .unwrap_or(255);
    let mut widths: Vec<f64> = dict_get(&font_dict, b"Widths")
        .and_then(|v| match v {
            Obj::Array(items) => Some(items.iter().map(|x| number_of(x).unwrap_or(0.0)).collect()),
            _ => None,
        })
        .unwrap_or_else(|| {
            vec![
                0.0;
                usize::try_from(last_char.saturating_sub(first_char).saturating_add(1)).unwrap_or(0)
            ]
        });

    let mut sorted = new_glyphs.to_vec();
    sorted.sort_by_key(|g| u32::from(g.ch));
    let mut changed = false;
    for ng in &sorted {
        let code = u32::from(ng.ch);
        let next = selis_font::subset::add_glyph(&program, code, &ng.outline, ng.advance, g)
            .map_err(|e| ReflowError::Invalid(format!("font extension failed: {e}")))?;
        match next {
            Some(bytes) => {
                program = bytes;
                changed = true;
            }
            None => return Ok(None),
        }
        // Widen /Widths so the new code has an advance in the PDF too.
        if code < first_char {
            continue;
        }
        let index = usize::try_from(code.saturating_sub(first_char)).unwrap_or(usize::MAX);
        if index < widths.len() {
            let slot = widths.get_mut(index);
            if let Some(slot) = slot {
                *slot = f64::from(ng.advance);
            }
        } else {
            while widths.len() <= index {
                widths.push(0.0);
            }
            if let Some(slot) = widths.get_mut(index) {
                *slot = f64::from(ng.advance);
            }
        }
    }
    if !changed {
        return Ok(None);
    }
    let new_last = widths
        .len()
        .checked_add(usize::try_from(first_char).unwrap_or(0))
        .and_then(|n| u32::try_from(n).ok())
        .and_then(|n| n.checked_sub(1))
        .unwrap_or(last_char)
        .max(last_char);

    // Rebuild the font program stream (raw, with a fresh /Length).
    let mut new_ff_dict: Vec<(Bytes, Obj)> = ff_dict
        .iter()
        .filter(|(k, _)| k.as_slice() != b"Length" && k.as_slice() != b"Filter")
        .cloned()
        .collect();
    new_ff_dict.push((
        Bytes::copy_from_slice(b"Length"),
        Obj::Int(i64::try_from(program.len()).unwrap_or(i64::MAX)),
    ));
    let new_ff = Obj::Stream {
        dict: new_ff_dict,
        data: program,
    };

    // Rebuild the descriptor's /FontFile2 reference (unchanged number) and the
    // font dict's /LastChar and /Widths.
    set_dict_entry(&mut desc, b"FontFile2", Obj::Ref(ff_ref));
    set_dict_entry(
        &mut font_dict,
        b"LastChar",
        Obj::Int(i64::try_from(new_last).unwrap_or(i64::MAX)),
    );
    set_dict_entry(
        &mut font_dict,
        b"Widths",
        Obj::Array(widths.into_iter().map(width_obj).collect()),
    );
    Ok(Some(vec![
        (ff_ref.num, new_ff),
        (desc_ref.num, Obj::Dict(desc)),
        (font_obj_ref.num, Obj::Dict(font_dict)),
    ]))
}
