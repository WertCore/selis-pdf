//! SL-5.FORM.07 — auto-detect candidate fields on a flat document.
//!
//! Many PDFs look like forms but carry no `/AcroForm`: the "fields" are just
//! drawn lines and boxes next to printed labels. This module reads the *page
//! content streams* and reports geometry that could be a field. It is a
//! heuristic and it is strictly read-only — it never creates a field, never
//! rewrites the document, and every candidate carries a confidence and the
//! reason it was suggested so a human can accept or discard it.
//!
//! The signals used here are deliberately conservative:
//!
//! * a short, very flat rectangle (or a single horizontal stroked line) near a
//!   text label suggests a **text** field;
//! * a small, roughly square rectangle suggests a **checkbox**;
//! * a nearby text label, when one is found, raises confidence and is reported
//!   so the caller can pre-fill a `/T`.
//!
//! Encoded streams are decoded through `selis-pdf-filter`; a stream whose
//! filter this crate cannot decode is skipped and recorded in
//! [`CandidateReport::notes`] rather than silently pretending the page was
//! empty.

use super::{dict_get, is_encrypted, open_doc, page_refs, FormError};
use selis_bytes::Bytes;
use selis_pdf_cos::copy::resolve_ref;
use selis_pdf_cos::{Obj, Ref};
use selis_pdf_filter::decode_stream;
use selis_sandbox::{Budget, BudgetGuard};
use serde::{Deserialize, Serialize};

/// The kind of field a geometric feature suggests.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CandidateKind {
    /// A horizontal rule/flat box next to a label: a probable text field.
    Text,
    /// A small square: a probable checkbox.
    Checkbox,
}

/// One read-only field candidate.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FieldCandidate {
    /// The 1-based page the geometry appears on.
    pub page: u32,
    /// What the geometry suggests.
    pub kind: CandidateKind,
    /// The suggested widget rectangle `[x0, y0, x1, y1]` in default user space.
    pub rect: [f64; 4],
    /// A value in `0.0..=1.0`. Higher means the geometry is more field-like;
    /// this is a heuristic score, not a probability.
    pub confidence: f64,
    /// A nearby text label, when one was found on the same page.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// Why this candidate was reported (in English, for a reviewer).
    pub reason: String,
}

/// The read-only result of a detection pass.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CandidateReport {
    /// `ok` when the pages were scanned.
    pub status: String,
    /// The document's page count.
    pub pages: u64,
    /// The candidates, ordered by page then descending confidence.
    pub candidates: Vec<FieldCandidate>,
    /// Honest notes about things that were skipped (e.g. an undecodable
    /// content stream) or limits that were hit.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<String>,
}

impl CandidateReport {
    /// The report as pretty JSON.
    ///
    /// # Errors
    ///
    /// [`FormError::Malformed`] when the report cannot be encoded (never for a
    /// report this crate built).
    pub fn to_json(&self) -> Result<Vec<u8>, FormError> {
        serde_json::to_vec_pretty(self)
            .map_err(|e| FormError::Malformed(format!("cannot encode the candidate report: {e}")))
    }
}

/// Maximum decoded bytes per content stream. A decompression bomb is cut off
/// here rather than in the caller's memory.
const DETECT_STREAM_LIMIT: u64 = 32 * 1024 * 1024;
/// A page with more geometry than this is treated as noise; the report says so.
const MAX_CANDIDATES: usize = 500;
/// How many tokens between budget ticks (keeps a hostile stream bounded).
const TICK_STRIDE: usize = 64;

/// Detect candidate form fields from the geometry in `src`'s page content
/// streams. Nothing is rewritten and no field is created.
///
/// # Budget
///
/// One document parse plus a bounded walk of each page's decoded content
/// streams; bytes and ticks are charged to `budget`. A hostile stream is
/// capped by [`DETECT_STREAM_LIMIT`] and [`MAX_CANDIDATES`].
///
/// # Malformed Input
///
/// An encrypted document is refused. A page whose content cannot be resolved,
/// or a stream whose filter cannot be decoded, is skipped with a note rather
/// than guessed at; a document that does not reparse is a typed [`FormError`].
///
/// # Errors
///
/// [`FormError::Encrypted`] for an encrypted document,
/// [`FormError::Malformed`]/[`FormError::Unresolvable`] for one this crate
/// cannot read.
pub fn detect_form_fields(
    src: &[u8],
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
) -> Result<CandidateReport, FormError> {
    let doc = open_doc(src, budget, g)?;
    if is_encrypted(&doc) {
        return Err(FormError::Encrypted);
    }
    let pages = page_refs(src, &doc, budget, g)?;
    let mut candidates: Vec<FieldCandidate> = Vec::new();
    let mut notes: Vec<String> = Vec::new();

    for (index, page_ref) in pages.iter().enumerate() {
        let page = u32::try_from(index).unwrap_or(u32::MAX).saturating_add(1);
        let content = page_content(src, &doc, *page_ref, page, budget, g, &mut notes)?;
        let before = candidates.len();
        let mut labels: Vec<(f64, f64, String)> = Vec::new();
        scan_content(&content, page, &mut candidates, &mut labels, g)?;
        apply_labels(page, &mut candidates, &labels);
        if candidates.len() >= MAX_CANDIDATES {
            notes.push(format!(
                "page {page}: stopped after {MAX_CANDIDATES} candidates; the rest were ignored"
            ));
            break;
        }
        if candidates.len() == before && content.is_empty() {
            notes.push(format!("page {page}: no decodable content stream"));
        }
    }

    // A nearby label raises confidence and supplies a suggested field name.
    for candidate in &mut candidates {
        if let Some(label) = candidate.label.take() {
            candidate.confidence = (candidate.confidence + 0.1).min(0.95);
            candidate.label = Some(label);
        }
    }
    candidates.sort_by(|a, b| {
        a.page.cmp(&b.page).then_with(|| {
            b.confidence
                .partial_cmp(&a.confidence)
                .unwrap_or(std::cmp::Ordering::Equal)
        })
    });

    Ok(CandidateReport {
        status: "ok".to_string(),
        pages: u64::try_from(pages.len()).unwrap_or(u64::MAX),
        candidates,
        notes,
    })
}

/// Concatenate the decoded content streams of one page.
#[allow(clippy::too_many_arguments)]
fn page_content(
    src: &[u8],
    doc: &selis_pdf_cos::Doc,
    page_ref: Ref,
    page: u32,
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
    notes: &mut Vec<String>,
) -> Result<Vec<u8>, FormError> {
    let page_obj = resolve_ref(src, doc, page_ref, budget, g)
        .map_err(|e| FormError::Unresolvable(format!("cannot resolve page {page}: {e}")))?;
    let Obj::Dict(pairs) = &page_obj else {
        notes.push(format!("page {page}: page object is not a dictionary"));
        return Ok(Vec::new());
    };
    let contents = match dict_get(pairs, b"Contents") {
        Some(Obj::Ref(reference)) => vec![*reference],
        Some(Obj::Array(items)) => items
            .iter()
            .filter_map(|item| match item {
                Obj::Ref(reference) => Some(*reference),
                _ => None,
            })
            .collect(),
        _ => return Ok(Vec::new()),
    };
    let mut out = Vec::new();
    for reference in contents {
        let stream = resolve_ref(src, doc, reference, budget, g).map_err(|e| {
            FormError::Unresolvable(format!("cannot resolve a content stream: {e}"))
        })?;
        let Obj::Stream { dict, data } = &stream else {
            continue;
        };
        let filter = dict_get(dict, b"Filter").and_then(|value| match value {
            Obj::Name(name) => Some(name.as_slice().to_vec()),
            Obj::Array(items) => items.first().and_then(|item| match item {
                Obj::Name(name) => Some(name.as_slice().to_vec()),
                _ => None,
            }),
            _ => None,
        });
        let decoded = match filter {
            None => data.as_slice().to_vec(),
            Some(filter) => {
                match decode_stream(
                    &Bytes::copy_from_slice(&filter),
                    None,
                    data.as_slice(),
                    DETECT_STREAM_LIMIT,
                    g,
                ) {
                    Ok(decoded) => decoded,
                    Err(_) => {
                        notes.push(format!(
                            "page {page}: skipped a content stream with an undecodable filter {}",
                            String::from_utf8_lossy(&filter)
                        ));
                        continue;
                    }
                }
            }
        };
        g.charge(
            selis_sandbox::Resource::Bytes,
            u64::try_from(decoded.len()).unwrap_or(u64::MAX),
        )
        .map_err(|e| FormError::Limit(format!("detection budget exhausted: {e}")))?;
        out.extend_from_slice(&decoded);
    }
    Ok(out)
}

/// A token from a content stream. Strings are unescaped enough for a label
/// suggestion; numbers and bare operator words drive the geometry.
#[derive(Debug, Clone)]
enum Tok {
    /// A numeric operand.
    Num(f64),
    /// A bare operator word (no leading `/`).
    Word(Vec<u8>),
    /// A literal or hex string operand.
    Str(Vec<u8>),
    /// A punctuation token (`[`, `]`, dictionaries, names) we ignore.
    Ignored,
}

/// Scan one content stream, appending candidates for `page`.
///
/// # Budget
///
/// Charged one tick per [`TICK_STRIDE`] tokens; allocations are bounded by the
/// decoded stream length and the candidate cap.
///
/// # Malformed Input
///
/// Unknown operators are ignored, unbalanced strings end at EOF, and an
/// operand of the wrong shape is skipped instead of guessed at.
fn scan_content(
    content: &[u8],
    page: u32,
    out: &mut Vec<FieldCandidate>,
    labels: &mut Vec<(f64, f64, String)>,
    g: &mut BudgetGuard<'_>,
) -> Result<(), FormError> {
    let tokens = tokenise(content, g)?;
    let mut operands: Vec<f64> = Vec::with_capacity(8);
    let mut path: Vec<(f64, f64)> = Vec::new();
    let mut text_x = 0.0f64;
    let mut text_y = 0.0f64;
    let mut line_x = 0.0f64;
    let mut line_y = 0.0f64;
    let mut last_str: Option<String> = None;

    for token in &tokens {
        match token {
            Tok::Num(number) => {
                operands.push(*number);
                if operands.len() > 16 {
                    operands.remove(0);
                }
            }
            Tok::Str(bytes) => {
                last_str = Some(String::from_utf8_lossy(bytes).trim().to_string());
            }
            Tok::Ignored => {}
            Tok::Word(word) => match word.as_slice() {
                b"re" => {
                    let values: Vec<f64> = std::mem::take(&mut operands);
                    if let [x, y, w, h] = values.as_slice() {
                        classify_box(*x, *y, *w, *h, page, last_str.as_deref(), out);
                    }
                }
                b"m" | b"l" => {
                    if operands.len() >= 2 {
                        let len = operands.len();
                        let x = operands.get(len.saturating_sub(2)).copied().unwrap_or(0.0);
                        let y = operands.get(len.saturating_sub(1)).copied().unwrap_or(0.0);
                        path.push((x, y));
                    }
                    clear_operands(&mut operands);
                }
                b"S" | b"s" | b"B" | b"b" | b"B*" | b"b*" => {
                    detect_rules(&path, page, last_str.as_deref(), out);
                    path.clear();
                    clear_operands(&mut operands);
                }
                b"f" | b"F" | b"f*" | b"n" => {
                    path.clear();
                    clear_operands(&mut operands);
                }
                b"Tm" => {
                    if operands.len() >= 6 {
                        let len = operands.len();
                        text_x = operands.get(len.saturating_sub(2)).copied().unwrap_or(0.0);
                        text_y = operands.get(len.saturating_sub(1)).copied().unwrap_or(0.0);
                        line_x = text_x;
                        line_y = text_y;
                    }
                    clear_operands(&mut operands);
                }
                b"Td" | b"TD" => {
                    if operands.len() >= 2 {
                        let len = operands.len();
                        let tx = operands.get(len.saturating_sub(2)).copied().unwrap_or(0.0);
                        let ty = operands.get(len.saturating_sub(1)).copied().unwrap_or(0.0);
                        line_x += tx;
                        line_y += ty;
                        text_x = line_x;
                        text_y = line_y;
                    }
                    clear_operands(&mut operands);
                }
                b"Tj" | b"'" => {
                    if let Some(label) = last_str.take() {
                        if !label.is_empty() {
                            labels.push((text_x, text_y, label));
                        }
                    }
                    clear_operands(&mut operands);
                }
                _ => {}
            },
        }
    }
    Ok(())
}

/// Drop operands that belonged to a consumed operator.
fn clear_operands(operands: &mut Vec<f64>) {
    operands.clear();
}

/// Tokenise a content stream into numbers, operators and strings.
fn tokenise(content: &[u8], g: &mut BudgetGuard<'_>) -> Result<Vec<Tok>, FormError> {
    let mut out = Vec::new();
    let mut i = 0usize;
    let len = content.len();
    while i < len {
        if i % TICK_STRIDE == 0 {
            g.tick()
                .map_err(|e| FormError::Limit(format!("detection budget exhausted: {e}")))?;
        }
        let Some(&byte) = content.get(i) else { break };
        if byte.is_ascii_whitespace() {
            i = i.saturating_add(1);
            continue;
        }
        if byte == b'%' {
            while let Some(&ch) = content.get(i) {
                if ch == b'\n' || ch == b'\r' {
                    break;
                }
                i = i.saturating_add(1);
            }
            continue;
        }
        match byte {
            b'(' => {
                let (bytes, next) = read_literal(content, i);
                out.push(Tok::Str(bytes));
                i = next;
            }
            b'<' if content.get(i.saturating_add(1)) == Some(&b'<') => {
                out.push(Tok::Ignored);
                i = i.saturating_add(2);
            }
            b'<' => {
                let (bytes, next) = read_hex(content, i);
                out.push(Tok::Str(bytes));
                i = next;
            }
            b'/' => {
                i = i.saturating_add(1);
                while let Some(&ch) = content.get(i) {
                    if ch.is_ascii_whitespace() || is_delimiter(ch) {
                        break;
                    }
                    i = i.saturating_add(1);
                }
                out.push(Tok::Ignored);
            }
            b'[' | b']' | b'>' => {
                out.push(Tok::Ignored);
                i = i.saturating_add(1);
            }
            _ => {
                let start = i;
                while let Some(&ch) = content.get(i) {
                    if ch.is_ascii_whitespace() || is_delimiter(ch) {
                        break;
                    }
                    i = i.saturating_add(1);
                }
                if i == start {
                    i = i.saturating_add(1);
                    continue;
                }
                let Some(raw) = content.get(start..i) else {
                    continue;
                };
                if let Ok(text) = std::str::from_utf8(raw) {
                    if let Ok(number) = text.parse::<f64>() {
                        out.push(Tok::Num(number));
                        continue;
                    }
                }
                out.push(Tok::Word(raw.to_vec()));
            }
        }
        if out.len() > 200_000 {
            return Err(FormError::Limit(
                "content stream has too many tokens to scan".to_string(),
            ));
        }
    }
    Ok(out)
}

/// True when a byte delimits a PDF token.
fn is_delimiter(byte: u8) -> bool {
    matches!(
        byte,
        b'(' | b')' | b'<' | b'>' | b'[' | b']' | b'{' | b'}' | b'/' | b'%'
    )
}

/// Read a balanced PDF literal string starting at `start`.
fn read_literal(content: &[u8], start: usize) -> (Vec<u8>, usize) {
    let mut out = Vec::new();
    let mut i = start.saturating_add(1);
    let mut depth = 1usize;
    while i < content.len() {
        let Some(&byte) = content.get(i) else { break };
        match byte {
            b'\\' => {
                let escaped = content.get(i.saturating_add(1)).copied().unwrap_or(0);
                i = i.saturating_add(2);
                match escaped {
                    b'n' => out.push(b'\n'),
                    b'r' => out.push(b'\r'),
                    b't' => out.push(b'\t'),
                    b'b' => out.push(0x08),
                    b'f' => out.push(0x0c),
                    other => out.push(other),
                }
            }
            b'(' => {
                depth = depth.saturating_add(1);
                out.push(b'(');
                i = i.saturating_add(1);
            }
            b')' => {
                depth = depth.saturating_sub(1);
                i = i.saturating_add(1);
                if depth == 0 {
                    break;
                }
                out.push(b')');
            }
            other => {
                out.push(other);
                i = i.saturating_add(1);
            }
        }
    }
    (out, i)
}

/// Read a PDF hex string starting at `start` (up to `>` or EOF).
fn read_hex(content: &[u8], start: usize) -> (Vec<u8>, usize) {
    let mut nibbles: Vec<u8> = Vec::new();
    let mut i = start.saturating_add(1);
    while i < content.len() {
        let Some(&byte) = content.get(i) else { break };
        if byte == b'>' {
            break;
        }
        if byte.is_ascii_hexdigit() {
            nibbles.push(byte);
        }
        i = i.saturating_add(1);
    }
    let mut out = Vec::new();
    let mut pair: Vec<u8> = Vec::with_capacity(2);
    for nibble in nibbles {
        pair.push(nibble);
        if pair.len() == 2 {
            let text = std::str::from_utf8(&pair).unwrap_or("00");
            if let Ok(byte) = u8::from_str_radix(text, 16) {
                out.push(byte);
            }
            pair.clear();
        }
    }
    (out, i.saturating_add(1))
}

/// Classify a `re` rectangle as a text field or a checkbox.
fn classify_box(
    x: f64,
    y: f64,
    w: f64,
    h: f64,
    page: u32,
    label: Option<&str>,
    out: &mut Vec<FieldCandidate>,
) {
    if !x.is_finite() || !y.is_finite() || !w.is_finite() || !h.is_finite() || w <= 0.0 || h <= 0.0
    {
        return;
    }
    let width = w.abs();
    let height = h.abs();
    let (kind, confidence, reason) = if height <= 4.0 && width >= 30.0 && width >= height * 10.0 {
        (
            CandidateKind::Text,
            if height <= 2.0 { 0.82f64 } else { 0.74f64 },
            "a short horizontal rule suggests a text entry line",
        )
    } else if (8.0..=40.0).contains(&width) && (8.0..=40.0).contains(&height) {
        let ratio = width / height;
        if (0.6..=1.6).contains(&ratio) {
            (
                CandidateKind::Checkbox,
                if (ratio - 1.0).abs() <= 0.25 {
                    0.72f64
                } else {
                    0.64f64
                },
                "a small, roughly square box suggests a checkbox",
            )
        } else {
            return;
        }
    } else {
        return;
    };
    push_candidate(
        out,
        FieldCandidate {
            page,
            kind,
            rect: [x, y, x + width, y + height],
            confidence,
            label: label.filter(|s| !s.is_empty()).map(str::to_string),
            reason: reason.to_string(),
        },
    );
}

/// Detect long horizontal segments in a stroked path as text rules.
fn detect_rules(
    path: &[(f64, f64)],
    page: u32,
    label: Option<&str>,
    out: &mut Vec<FieldCandidate>,
) {
    for pair in path.windows(2) {
        let Some((x0, y0)) = pair.first() else {
            continue;
        };
        let Some((x1, y1)) = pair.get(1) else {
            continue;
        };
        if (y0 - y1).abs() > 0.5 {
            continue;
        }
        let low = x0.min(*x1);
        let high = x0.max(*x1);
        let length = high - low;
        if length >= 30.0 {
            push_candidate(
                out,
                FieldCandidate {
                    page,
                    kind: CandidateKind::Text,
                    rect: [low, *y0 - 1.0, high, *y0 + 1.0],
                    confidence: 0.62,
                    label: label.filter(|s| !s.is_empty()).map(str::to_string),
                    reason: "a long horizontal stroked line suggests a text entry line".to_string(),
                },
            );
        }
    }
}

/// Add a candidate unless an almost identical one is already present.
fn push_candidate(out: &mut Vec<FieldCandidate>, candidate: FieldCandidate) {
    if out.len() >= MAX_CANDIDATES {
        return;
    }
    let duplicate = out.iter().any(|existing| {
        existing.page == candidate.page
            && existing
                .rect
                .iter()
                .zip(candidate.rect.iter())
                .all(|(a, b)| (a - b).abs() <= 2.0)
    });
    if !duplicate {
        out.push(candidate);
    }
}

/// Attach the nearest label above/overlapping each candidate rectangle. Labels
/// are gathered in one pass over the page, so a label drawn after a rule is
/// still matched to it.
fn apply_labels(page: u32, out: &mut [FieldCandidate], labels: &[(f64, f64, String)]) {
    for candidate in out.iter_mut() {
        if candidate.page != page || candidate.label.is_some() {
            continue;
        }
        let [x0, y0, x1, y1] = candidate.rect;
        let mut best: Option<(f64, String)> = None;
        for (lx, ly, text) in labels {
            if *ly >= y0 && *ly <= y1 + 24.0 && *lx >= x0 - 4.0 && *lx <= x1 + 4.0 {
                let distance = (*ly - y1).abs() + (*lx - x0).abs();
                if best.as_ref().is_none_or(|(d, _)| distance < *d) {
                    best = Some((distance, text.clone()));
                }
            }
        }
        if let Some((_, text)) = best {
            candidate.label = Some(text);
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing,
        clippy::arithmetic_side_effects,
        clippy::cast_possible_truncation,
        clippy::type_complexity
    )]

    use super::*;
    use crate::read_form_fields;

    /// A flat one-page PDF whose content draws a text rule, a checkbox square,
    /// a short line and a label above the rule.
    fn flat_with_geometry() -> Vec<u8> {
        let content = b"BT /F1 12 Tf 10 70 Td (Full name) Tj ET\n\
                        10 50 150 2 re S\n\
                        10 20 16 16 re S\n\
                        200 60 m 320 60 l S\n";
        let objects: &[(u32, Vec<u8>)] = &[
            (1, b"<< /Type /Catalog /Pages 2 0 R >>".to_vec()),
            (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec()),
            (
                3,
                b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << >> /Contents 4 0 R >>".to_vec(),
            ),
            (
                4,
                [
                    format!("<< /Length {} >>\nstream\n", content.len()).into_bytes(),
                    content.to_vec(),
                    b"\nendstream".to_vec(),
                ]
                .concat(),
            ),
        ];
        let max = objects.iter().map(|(num, _)| *num).max().unwrap_or(0);
        let mut out = Vec::new();
        out.extend_from_slice(b"%PDF-1.4\n");
        let mut offsets = vec![0u64; max as usize + 1];
        for (num, body) in objects {
            offsets[*num as usize] = out.len() as u64;
            out.extend_from_slice(format!("{num} 0 obj\n").as_bytes());
            out.extend_from_slice(body);
            out.extend_from_slice(b"\nendobj\n");
        }
        let xref_at = out.len() as u64;
        out.extend_from_slice(format!("xref\n0 {}\n", max + 1).as_bytes());
        out.extend_from_slice(b"0000000000 65535 f \n");
        for index in 1..=max {
            out.extend_from_slice(format!("{:010} 00000 n \n", offsets[index as usize]).as_bytes());
        }
        out.extend_from_slice(
            format!(
                "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref_at}\n%%EOF\n",
                max + 1
            )
            .as_bytes(),
        );
        out
    }

    fn detect(fixture: &[u8]) -> CandidateReport {
        let budget = Budget::unlimited();
        let mut g = budget.guard();
        detect_form_fields(fixture, &budget, &mut g).expect("detect")
    }

    #[test]
    fn detects_boxes_and_lines_as_read_only_candidates() {
        let fixture = flat_with_geometry();
        let report = detect(&fixture);
        assert_eq!(report.status, "ok");
        assert_eq!(report.pages, 1);
        assert!(
            report.candidates.len() >= 3,
            "candidates: {:?}",
            report.candidates
        );

        let checkbox = report
            .candidates
            .iter()
            .find(|c| c.kind == CandidateKind::Checkbox)
            .expect("a checkbox");
        assert_eq!(checkbox.rect, [10.0, 20.0, 26.0, 36.0]);
        assert!(checkbox.confidence >= 0.6 && checkbox.confidence <= 0.95);

        let texts: Vec<&FieldCandidate> = report
            .candidates
            .iter()
            .filter(|c| c.kind == CandidateKind::Text)
            .collect();
        assert!(texts.len() >= 2, "a flat box and a stroked line");
        assert!(
            texts.iter().any(|c| c.rect == [10.0, 50.0, 160.0, 52.0]),
            "the flat rectangle: {:?}",
            report.candidates
        );
        assert!(
            texts.iter().any(|c| c.rect == [200.0, 59.0, 320.0, 61.0]),
            "the stroked line: {:?}",
            report.candidates
        );
        // The label above the rule is attached with a confidence bump.
        assert!(
            texts
                .iter()
                .any(|c| c.label.as_deref() == Some("Full name") && c.confidence > 0.82),
            "label and confidence: {:?}",
            report.candidates
        );
    }

    #[test]
    fn detection_never_creates_a_form() {
        let fixture = flat_with_geometry();
        let before = fixture.clone();
        let report = detect(&fixture);
        assert!(!report.candidates.is_empty());
        // The input bytes are untouched (the API only borrows them).
        assert_eq!(fixture, before);
        // And a normal read sees no fields: detection is read-only.
        let budget = Budget::unlimited();
        let mut g = budget.guard();
        let fields = read_form_fields(&fixture, &budget, &mut g).expect("read");
        assert!(fields.fields.is_empty(), "no /AcroForm was created");
    }

    #[test]
    fn a_document_with_no_geometry_has_no_candidates() {
        let objects: &[(u32, &[u8])] = &[
            (1, b"<< /Type /Catalog /Pages 2 0 R >>"),
            (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
            (
                3,
                b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R >>",
            ),
            (4, b"<< /Length 0 >>\nstream\n\nendstream"),
        ];
        let max = objects.iter().map(|(num, _)| *num).max().unwrap_or(0);
        let mut out = Vec::new();
        out.extend_from_slice(b"%PDF-1.4\n");
        let mut offsets = vec![0u64; max as usize + 1];
        for (num, body) in objects {
            offsets[*num as usize] = out.len() as u64;
            out.extend_from_slice(format!("{num} 0 obj\n").as_bytes());
            out.extend_from_slice(body);
            out.extend_from_slice(b"\nendobj\n");
        }
        let xref_at = out.len() as u64;
        out.extend_from_slice(format!("xref\n0 {}\n", max + 1).as_bytes());
        out.extend_from_slice(b"0000000000 65535 f \n");
        for index in 1..=max {
            out.extend_from_slice(format!("{:010} 00000 n \n", offsets[index as usize]).as_bytes());
        }
        out.extend_from_slice(
            format!(
                "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref_at}\n%%EOF\n",
                max + 1
            )
            .as_bytes(),
        );
        let report = detect(&out);
        assert!(report.candidates.is_empty());
    }
}
