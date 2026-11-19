//! SL-5.REDACT.05 — pattern-based bulk redaction with a mandatory review step.
//!
//! This module is the *safe* half of "bulk redact PII". It never applies
//! anything on its own:
//!
//! * [`find_candidates`] is a read-only scan. It has no output path in its
//!   signature, so it cannot write a document even by accident. It returns a
//!   [`PatternReview`] whose candidate text is **masked** — the full secret is
//!   never serialized into the review.
//! * [`apply_reviewed`] takes an explicit `approved_ids` slice. There is no
//!   code path that treats "review contains candidates" as approval: an empty
//!   set is an error, an unknown id is an error, and only the named regions
//!   are passed to the existing redaction tool.
//!
//! The read-back check (ADR-P0023) re-opens the produced bytes with a fresh
//! parser and a fresh text layer, then asserts every approved candidate's
//! matched text is gone from both the extracted text and the raw bytes. A
//! survivor is a typed error and the output is left untouched. The
//! `verifier_is_not_vacuous` test proves the check fails on an unredacted
//! document.
//!
//! The pattern model is deliberately small and honest: no regex crate is
//! pulled in (ADR-P0021). There are three built-in recognizers (US SSN,
//! Luhn-valid payment-card digit runs, and RFC-ish email addresses) plus
//! caller-supplied **literal** strings for the "names from a list" case. Every
//! match is found in text the engine already recovered, with per-character
//! geometry from `selis-pdf-text`; nothing is guessed from a raw byte pattern
//! that is not also present in the extracted text.
//!
//! ## Coverage and its limits
//!
//! The library and the CLI ship this operation; there is no WASM protocol op
//! or web card yet, so the browser surface is deliberately deferred (and no
//! web checkbox is claimed). The low-level redactor removes a **whole show
//! operation** whose text origin falls in the region — it cannot excise a
//! substring from a longer `Tj`. When a pattern match sits inside a longer
//! show operation, the read-back check therefore fails loudly with
//! `VERIFY_FAILED` and no file is written rather than pretending the content
//! was removed. Documents that place each PII token in its own show operation
//! (a normal PDF layout) redact exactly that token.

#![forbid(unsafe_code)]

use selis_pdf_engine::Session;
use selis_pdf_text::{LayerChar, PageTextLayer};
use selis_sandbox::{Budget, BudgetGuard, Clock};
use serde::{Deserialize, Serialize};

use crate::{CliError, ToolResult, Verification};

/// The schema version of [`PatternReview`]. A reader must refuse a review
/// written by a different version rather than guess at changed field meanings.
pub const REVIEW_SCHEMA: u32 = 1;

/// Padding added around a matched character's quad, in points.
///
/// Redaction removes text whose glyph box intersects the region. A sub-point
/// pad keeps a match whose quad ends exactly on a glyph boundary from being
/// missed without swallowing the neighbouring word.
pub const DEFAULT_PADDING: f64 = 0.6;

/// The pattern families this scan understands.
///
/// `Literal` carries its own matching rule so the review can say *how* the
/// name was found without storing the name itself.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Pattern {
    /// A US Social Security number, `NNN-NN-NNNN`, with the usual invalid
    /// group checks (no all-zero area, group, or serial).
    Ssn,
    /// A 13–19 digit run that passes the Luhn check. Interior single spaces
    /// or hyphens are part of the match so the covering region includes them.
    Card,
    /// A `local@domain` address with a dotted domain.
    Email,
    /// A caller-supplied literal string.
    Literal {
        /// The text to find. This is a *query*, not a match: it is not written
        /// into the review's candidate text.
        text: String,
        /// When true, require a word boundary on each side of the match.
        whole_word: bool,
    },
}

impl Pattern {
    /// The stable, secret-free kind label used in the review.
    #[must_use]
    pub fn kind(&self) -> PatternKind {
        match self {
            Self::Ssn => PatternKind::Ssn,
            Self::Card => PatternKind::Card,
            Self::Email => PatternKind::Email,
            Self::Literal { .. } => PatternKind::Literal,
        }
    }
}

/// The secret-free label of a [`Pattern`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PatternKind {
    /// See [`Pattern::Ssn`].
    Ssn,
    /// See [`Pattern::Card`].
    Card,
    /// See [`Pattern::Email`].
    Email,
    /// See [`Pattern::Literal`].
    Literal,
}

/// Options for a scan.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ScanOptions {
    /// Points of padding around each matched quad. Must be finite and
    /// non-negative; other values are rejected by [`find_candidates_with`].
    pub padding: f64,
}

impl Default for ScanOptions {
    fn default() -> Self {
        Self {
            padding: DEFAULT_PADDING,
        }
    }
}

/// One candidate a reviewer may approve.
///
/// The `start`/`end` pair is a UTF-16 code-unit range into the page layer's
/// line text — the same indexing the text layer's `chars` uses. Keeping it
/// (rather than a serialized copy of the secret) lets `apply_reviewed`
/// re-derive the exact matched bytes from the *input* at apply time, so the
/// review never has to carry them.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Candidate {
    /// Stable, location-derived id: `p<page>-l<line>-c<start>`.
    pub id: String,
    /// Zero-based page index.
    pub page: usize,
    /// Line index within the page's reading-ordered text layer.
    pub line: usize,
    /// UTF-16 code-unit start of the match in that line's text.
    pub start: usize,
    /// UTF-16 code-unit end (exclusive) of the match.
    pub end: usize,
    /// Which recognizer found it.
    pub pattern: PatternKind,
    /// A masked rendering of the match, safe to show in a review UI.
    pub masked: String,
    /// The padded redaction region in PDF user space: `[x, y, width, height]`.
    pub region: [f64; 4],
}

/// The read-only result of a scan, and the only input [`apply_reviewed`]
/// accepts besides the explicit approval list.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PatternReview {
    /// Schema version; [`REVIEW_SCHEMA`].
    pub schema: u32,
    /// The path/key the review was built from.
    pub source: String,
    /// Length of the source bytes, as a cheap whole-file change check.
    pub source_len: u64,
    /// FNV-1a of the source bytes (a non-cryptographic integrity aid, not a
    /// security boundary — see [`apply_reviewed`]'s semantic re-derivation).
    pub source_fnv: u64,
    /// Candidates in reading order. No candidate is approved by default.
    pub candidates: Vec<Candidate>,
}

impl PatternReview {
    /// Serialize the review as pretty JSON.
    ///
    /// # Errors
    ///
    /// A typed error if serialization fails (it cannot for this type, but the
    /// boundary stays honest for callers that add fields later).
    pub fn to_json(&self) -> ToolResult<Vec<u8>> {
        serde_json::to_vec_pretty(self)
            .map_err(|e| CliError(format!("cannot serialise review: {e}")))
    }

    /// Parse a review written by [`Self::to_json`].
    ///
    /// # Errors
    ///
    /// A typed error naming the parse fault; a wrong schema version is
    /// rejected as well.
    pub fn from_json(bytes: &[u8]) -> ToolResult<Self> {
        let review: Self = serde_json::from_slice(bytes)
            .map_err(|e| CliError(format!("cannot parse review: {e}")))?;
        if review.schema != REVIEW_SCHEMA {
            return Err(CliError(format!(
                "unsupported review schema {} (expected {REVIEW_SCHEMA})",
                review.schema
            )));
        }
        Ok(review)
    }
}

/// A typed failure from the pattern-redaction path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PatternRedactError {
    /// No candidate was explicitly approved.
    EmptyApproval,
    /// An approval named an id that is not in the review.
    UnknownId(String),
    /// The review's schema, source, or candidate geometry no longer matches
    /// the input. Nothing is applied.
    ReviewMismatch(String),
    /// The read-back check found a redacted match in the output.
    VerifyFailed(String),
    /// A structural I/O or parse fault.
    Io(String),
}

impl PatternRedactError {
    /// A stable machine-readable code for this failure.
    #[must_use]
    pub fn code(&self) -> &'static str {
        match self {
            Self::EmptyApproval => "EMPTY_APPROVAL",
            Self::UnknownId(_) => "UNKNOWN_ID",
            Self::ReviewMismatch(_) => "REVIEW_MISMATCH",
            Self::VerifyFailed(_) => "VERIFY_FAILED",
            Self::Io(_) => "IO",
        }
    }
}

impl std::fmt::Display for PatternRedactError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}: {}",
            self.code(),
            match self {
                Self::EmptyApproval =>
                    "no candidate ids were approved; nothing was applied".to_string(),
                Self::UnknownId(id) => format!("unknown candidate id {id:?}"),
                Self::ReviewMismatch(why) => format!("review does not match the input: {why}"),
                Self::VerifyFailed(why) => format!("redaction verification failed: {why}"),
                Self::Io(why) => why.clone(),
            }
        )
    }
}

impl std::error::Error for PatternRedactError {}

impl From<PatternRedactError> for CliError {
    fn from(value: PatternRedactError) -> Self {
        CliError(value.to_string())
    }
}

/// A needle re-derived from the input at apply time, used only for read-back.
#[derive(Debug, Clone)]
struct ProofNeedle {
    id: String,
    page: usize,
    text: String,
}

// ---------------------------------------------------------------------------
// Read-only scan
// ---------------------------------------------------------------------------

/// Scan every page for the given patterns and return a masked review.
///
/// This function is strictly read-only: its signature has no output path and
/// it never calls a writer or a committer. Its result is a *proposal*; no
/// candidate in it is approved.
///
/// # Budget
///
/// Linear in the document's recovered text plus one text layer per page. Each
/// layer holds one [`LayerChar`] per UTF-16 code unit of that page's text,
/// which is a constant factor on the display list the engine already built.
/// Recognition is linear in the length of each line; no quadratic backtracking
/// is used.
///
/// # Malformed Input
///
/// A document that cannot be opened, or a page whose text layer cannot be
/// built, yields a typed error. A line with no recovered text contributes no
/// candidates. A non-finite glyph quad is skipped when computing a region,
/// so a corrupt geometry entry can never produce a NaN rectangle.
///
/// # Errors
///
/// A typed [`CliError`] naming the input and the page that could not be read.
pub fn find_candidates(path: &str, patterns: &[Pattern]) -> ToolResult<PatternReview> {
    find_candidates_with(path, patterns, &ScanOptions::default())
}

/// Scan with explicit geometry options. See [`find_candidates`].
///
/// # Budget
///
/// As [`find_candidates`].
///
/// # Malformed Input
///
/// A non-finite or negative `padding` is refused rather than silently
/// producing invalid rectangles.
///
/// # Errors
///
/// As [`find_candidates`], plus an invalid-padding error.
pub fn find_candidates_with(
    path: &str,
    patterns: &[Pattern],
    options: &ScanOptions,
) -> ToolResult<PatternReview> {
    if !options.padding.is_finite() || options.padding < 0.0 {
        return Err(CliError(
            "padding must be a finite, non-negative number of points".to_string(),
        ));
    }
    let src = crate::read_file(path)?;
    let budget = Budget::unlimited();
    let clock = crate::shell_clock();
    let session = Session::open(src.clone(), &budget, &clock)
        .map_err(|e| CliError(format!("{path}: cannot open PDF: {e}")))?;
    let mut candidates = Vec::new();
    for page in 0..session.len() {
        let mut g = crate::runtime::cli_guard(&budget, &clock);
        let layer = page_text_layer(&session, page, &budget, &mut g)
            .map_err(|e| CliError(format!("{path}: page {page}: {e}")))?;
        scan_layer(&layer, patterns, options.padding, &mut candidates);
    }
    Ok(PatternReview {
        schema: REVIEW_SCHEMA,
        source: path.to_string(),
        source_len: u64::try_from(src.len()).unwrap_or(u64::MAX),
        source_fnv: fnv1a(&src),
        candidates,
    })
}

/// Build one page's text layer through the same ladder `selis extract` and
/// the viewer's text layer use: display list → visible gate → Unicode
/// recovery → assembly → reading order → `page_layer`.
///
/// # Budget
///
/// One advance-width table per page, keyed by glyph code, plus the layer.
///
/// # Malformed Input
///
/// A page outside the document, or a display list that cannot be interpreted,
/// yields a typed error rather than an empty layer that would silently
/// produce no candidates.
fn page_text_layer(
    session: &Session,
    page: usize,
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
) -> ToolResult<PageTextLayer> {
    let Some(size) = session.page_size(page) else {
        return Err(CliError(format!("page {page} is outside the document")));
    };
    let (lines, line_texts, low_confidence) = crate::text::page_lines(session, page, budget, g)?;
    let mut widths = std::collections::HashMap::new();
    for line in &lines {
        for word in &line.words {
            for run in &word.runs {
                for glyph in &run.glyphs {
                    let _ = widths.entry(glyph.code).or_insert(glyph.advance);
                }
            }
        }
    }
    let width_of = |code: u16| widths.get(&code).copied().unwrap_or(0.0);
    let mut layer = selis_pdf_text::page_layer(page, size, &lines, &line_texts, &width_of);
    layer.low_confidence = low_confidence;
    Ok(layer)
}

/// Append every candidate found on one page layer.
///
/// # Budget
///
/// Linear in the page's UTF-16 code units and the number of patterns.
///
/// # Malformed Input
///
/// A match whose character range is out of bounds, or whose quad is entirely
/// non-finite, is skipped rather than producing a broken region.
fn scan_layer(layer: &PageTextLayer, patterns: &[Pattern], padding: f64, out: &mut Vec<Candidate>) {
    for (line_index, line) in layer.lines.iter().enumerate() {
        let units: Vec<u16> = line.text.encode_utf16().collect();
        let mut found: Vec<(usize, usize, PatternKind)> = Vec::new();
        for pattern in patterns {
            let ranges = match pattern {
                Pattern::Ssn => find_ssn(&units),
                Pattern::Card => find_card(&units),
                Pattern::Email => find_email(&units),
                Pattern::Literal { text, whole_word } => {
                    let needle: Vec<u16> = text.encode_utf16().collect();
                    find_literal(&units, &needle, *whole_word)
                }
            };
            let kind = pattern.kind();
            for (start, end) in ranges {
                found.push((start, end, kind));
            }
        }
        // Longest match at a given start wins; ties go to the earlier pattern
        // in the caller's list. Overlapping matches are dropped so one secret
        // is one candidate with one id.
        found.sort_by(|a, b| {
            a.0.cmp(&b.0)
                .then_with(|| b.1.cmp(&a.1))
                .then_with(|| a.2.cmp_kind(b.2))
        });
        let mut last_end = 0usize;
        for (start, end, kind) in found {
            if start < last_end || end <= start {
                continue;
            }
            let Some(region) = region_for(&line.chars, start, end, padding) else {
                continue;
            };
            let Some(raw_units) = units.get(start..end) else {
                continue;
            };
            let raw = String::from_utf16_lossy(raw_units);
            if raw.trim().is_empty() {
                continue;
            }
            out.push(Candidate {
                id: format!("p{}-l{}-c{}", layer.page, line_index, start),
                page: layer.page,
                line: line_index,
                start,
                end,
                pattern: kind,
                masked: mask(kind, &raw),
                region,
            });
            last_end = end;
        }
    }
}

impl PatternKind {
    /// A deterministic total order used only to break scan ties.
    fn cmp_kind(self, other: Self) -> std::cmp::Ordering {
        fn rank(kind: PatternKind) -> u8 {
            match kind {
                PatternKind::Literal => 0,
                PatternKind::Ssn => 1,
                PatternKind::Card => 2,
                PatternKind::Email => 3,
            }
        }
        rank(self).cmp(&rank(other))
    }
}

/// Union the glyph quads of the matched UTF-16 code units plus padding.
///
/// # Budget
///
/// Linear in the number of matched code units.
///
/// # Malformed Input
///
/// Non-finite quads are ignored. If no finite quad remains, there is no
/// region and the candidate is skipped.
#[allow(clippy::arithmetic_side_effects)] // simple point geometry on finite f64s
fn region_for(chars: &[LayerChar], start: usize, end: usize, padding: f64) -> Option<[f64; 4]> {
    let slice = chars.get(start..end)?;
    let mut x0 = f64::INFINITY;
    let mut y0 = f64::INFINITY;
    let mut x1 = f64::NEG_INFINITY;
    let mut y1 = f64::NEG_INFINITY;
    for lc in slice {
        let r = &lc.rect;
        if !r.x0.is_finite() || !r.y0.is_finite() || !r.x1.is_finite() || !r.y1.is_finite() {
            continue;
        }
        x0 = x0.min(r.x0);
        y0 = y0.min(r.y0);
        x1 = x1.max(r.x1);
        y1 = y1.max(r.y1);
    }
    if !x0.is_finite() {
        return None;
    }
    let x0 = x0 - padding;
    let y0 = y0 - padding;
    let x1 = x1 + padding;
    let y1 = y1 + padding;
    Some([x0, y0, x1 - x0, y1 - y0])
}

// ---------------------------------------------------------------------------
// Recognizers (UTF-16 code-unit based, so indices map straight to LayerChar)
// ---------------------------------------------------------------------------

fn at(units: &[u16], index: usize) -> u16 {
    units.get(index).copied().unwrap_or(0)
}

fn is_digit(unit: u16) -> bool {
    unit >= u16::from(b'0') && unit <= u16::from(b'9')
}

/// US SSN: `NNN-NN-NNNN` with the standard all-zero-group exclusions.
///
/// # Budget
///
/// One 11-unit sliding window per line.
///
/// # Malformed Input
///
/// Short lines simply yield no match.
fn find_ssn(units: &[u16]) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let zero = u16::from(b'0');
    let dash = u16::from(b'-');
    for (i, w) in units.windows(11).enumerate() {
        let d = |k: usize| w.get(k).copied().unwrap_or(0);
        let shaped = is_digit(d(0))
            && is_digit(d(1))
            && is_digit(d(2))
            && d(3) == dash
            && is_digit(d(4))
            && is_digit(d(5))
            && d(6) == dash
            && is_digit(d(7))
            && is_digit(d(8))
            && is_digit(d(9))
            && is_digit(d(10));
        if !shaped {
            continue;
        }
        let area_zero = d(0) == zero && d(1) == zero && d(2) == zero;
        let group_zero = d(4) == zero && d(5) == zero;
        let serial_zero = d(7) == zero && d(8) == zero && d(9) == zero && d(10) == zero;
        if !(area_zero || group_zero || serial_zero) {
            out.push((i, i.saturating_add(11)));
        }
    }
    out
}

/// Luhn check over ASCII digit code units.
///
/// # Budget
///
/// Linear in the digit run.
///
/// # Malformed Input
///
/// Non-digit units are treated as zero; callers only pass digit runs.
fn luhn_ok(digits: &[u16]) -> bool {
    if digits.len() < 13 || digits.len() > 19 {
        return false;
    }
    let zero = u16::from(b'0');
    let even_len = (digits.len() & 1) == 0;
    let mut sum = 0u32;
    for (idx, &unit) in digits.iter().enumerate() {
        let mut value = u32::from(unit.saturating_sub(zero));
        let double = ((idx & 1) == 0) == even_len;
        if double {
            value = value.saturating_mul(2);
            if value > 9 {
                value = value.saturating_sub(9);
            }
        }
        sum = sum.saturating_add(value);
    }
    sum.checked_rem(10) == Some(0)
}

/// Payment-card digit runs (13–19 digits) that pass Luhn.
///
/// # Budget
///
/// A single left-to-right pass; a rejected run is skipped in full so a long
/// digit string cannot induce quadratic re-testing.
///
/// # Malformed Input
///
/// Only single spaces or hyphens followed by a digit are absorbed; a run of
/// separators, or a trailing separator, ends the match, so punctuation can
/// never be silently included in a redaction region.
fn find_card(units: &[u16]) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let space = u16::from(b' ');
    let dash = u16::from(b'-');
    let mut i = 0usize;
    while i < units.len() {
        if !is_digit(at(units, i)) {
            i = i.saturating_add(1);
            continue;
        }
        let start = i;
        let mut end = i;
        let mut digits = Vec::new();
        let mut separators = 0usize;
        loop {
            let unit = at(units, end);
            if is_digit(unit) {
                digits.push(unit);
                end = end.saturating_add(1);
                separators = 0;
            } else if (unit == space || unit == dash) && !digits.is_empty() {
                let next = at(units, end.saturating_add(1));
                if !is_digit(next) || separators >= 1 {
                    break;
                }
                separators = separators.saturating_add(1);
                end = end.saturating_add(1);
            } else {
                break;
            }
        }
        if luhn_ok(&digits) {
            out.push((start, end));
            i = end;
        } else {
            // Longest-run-wins: a nested card inside a failed run is not
            // reported. This keeps the scan bounded on hostile input.
            i = end.max(start.saturating_add(1));
        }
    }
    out
}

fn is_local_char(unit: u16) -> bool {
    unit == u16::from(b'.')
        || unit == u16::from(b'_')
        || unit == u16::from(b'%')
        || unit == u16::from(b'+')
        || unit == u16::from(b'-')
        || is_digit(unit)
        || (unit >= u16::from(b'a') && unit <= u16::from(b'z'))
        || (unit >= u16::from(b'A') && unit <= u16::from(b'Z'))
}

fn is_domain_char(unit: u16) -> bool {
    unit == u16::from(b'.')
        || unit == u16::from(b'-')
        || is_digit(unit)
        || (unit >= u16::from(b'a') && unit <= u16::from(b'z'))
        || (unit >= u16::from(b'A') && unit <= u16::from(b'Z'))
}

/// `local@domain` with a dotted domain and no leading/trailing dot.
///
/// # Budget
///
/// One pass over `@` positions, with backward and forward walks bounded by
/// the line length.
///
/// # Malformed Input
///
/// An `@` with no local part, no domain, or an undotted domain is ignored.
fn find_email(units: &[u16]) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let at_sign = u16::from(b'@');
    let dot = u16::from(b'.');
    for (at_pos, &unit) in units.iter().enumerate() {
        if unit != at_sign {
            continue;
        }
        let mut start = at_pos;
        while start > 0 {
            let prev = at(units, start.saturating_sub(1));
            if is_local_char(prev) {
                start = start.saturating_sub(1);
            } else {
                break;
            }
        }
        let mut end = at_pos.saturating_add(1);
        while end < units.len() {
            let current = at(units, end);
            if is_domain_char(current) {
                end = end.saturating_add(1);
            } else {
                break;
            }
        }
        if start == at_pos || end <= at_pos.saturating_add(2) {
            continue;
        }
        let domain = units
            .iter()
            .skip(at_pos.saturating_add(1))
            .take(end.saturating_sub(at_pos).saturating_sub(1));
        let dotted = domain.clone().any(|&u| u == dot)
            && at(units, at_pos.saturating_add(1)) != dot
            && at(units, end.saturating_sub(1)) != dot;
        if dotted && at(units, start) != dot && at(units, at_pos.saturating_sub(1)) != dot {
            out.push((start, end));
        }
    }
    out
}

/// Literal substring (optionally whole-word) search.
///
/// # Budget
///
/// One sliding comparison of `needle.len()` units per position.
///
/// # Malformed Input
///
/// An empty needle yields no matches — redacting "everything" is never
/// implied by an empty literal.
fn find_literal(units: &[u16], needle: &[u16], whole_word: bool) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    if needle.is_empty() {
        return out;
    }
    let last = units.len().saturating_sub(needle.len());
    for start in 0..=last {
        if !units
            .iter()
            .copied()
            .skip(start)
            .take(needle.len())
            .eq(needle.iter().copied())
        {
            continue;
        }
        if whole_word && !word_boundary(units, start, needle.len()) {
            continue;
        }
        out.push((start, start.saturating_add(needle.len())));
    }
    out
}

/// Whether the units immediately before and after a range are not word
/// characters.
///
/// # Budget
///
/// Two unit lookups.
///
/// # Malformed Input
///
/// Out-of-range positions are treated as boundaries.
fn word_boundary(units: &[u16], start: usize, len: usize) -> bool {
    let before = if start == 0 {
        true
    } else {
        !is_word_unit(at(units, start.saturating_sub(1)))
    };
    let after_index = start.saturating_add(len);
    let after = !is_word_unit(at(units, after_index));
    before && after
}

fn is_word_unit(unit: u16) -> bool {
    is_digit(unit)
        || (unit >= u16::from(b'a') && unit <= u16::from(b'z'))
        || (unit >= u16::from(b'A') && unit <= u16::from(b'Z'))
        || unit == u16::from(b'_')
}

// ---------------------------------------------------------------------------
// Masking — the review never carries a full secret
// ---------------------------------------------------------------------------

/// Render a match in a form safe for a review JSON.
///
/// # Budget
///
/// Linear in the matched text.
///
/// # Malformed Input
///
/// An empty match yields an empty mask; no branch can leak the raw string.
fn mask(kind: PatternKind, raw: &str) -> String {
    match kind {
        PatternKind::Ssn | PatternKind::Card => mask_keep_last_digits(raw, 4),
        PatternKind::Email => mask_email(raw),
        PatternKind::Literal => raw
            .chars()
            .map(|c| if c.is_whitespace() { c } else { '*' })
            .collect(),
    }
}

/// Keep separators and the last `keep` digits; star the rest.
///
/// # Budget
///
/// Linear in the input.
///
/// # Malformed Input
///
/// `keep` above the digit count masks nothing, which is safe.
fn mask_keep_last_digits(raw: &str, keep: usize) -> String {
    let chars: Vec<char> = raw.chars().collect();
    let total = chars.iter().filter(|c| c.is_ascii_digit()).count();
    let threshold = total.saturating_sub(keep);
    let mut seen = 0usize;
    let mut out = String::with_capacity(raw.len());
    for &c in &chars {
        if c.is_ascii_digit() {
            let index = seen;
            seen = seen.saturating_add(1);
            out.push(if index < threshold { '*' } else { c });
        } else {
            out.push(c);
        }
    }
    out
}

/// Keep the first local character and the domain; star the rest of the local
/// part. The local part is the identifying secret; the domain is kept so a
/// reviewer can still tell what was matched.
///
/// # Budget
///
/// Linear in the input.
///
/// # Malformed Input
///
/// A string with no `@` is fully starred.
fn mask_email(raw: &str) -> String {
    let Some(at) = raw.find('@') else {
        return "*".repeat(raw.chars().count());
    };
    let (local, domain) = raw.split_at(at);
    let first = local
        .chars()
        .next()
        .map_or(String::new(), |c| c.to_string());
    format!("{first}***{domain}")
}

/// Deterministic, dependency-free FNV-1a over bytes.
///
/// # Budget
///
/// One pass over the input.
///
/// # Malformed Input
///
/// None; any byte sequence has a fingerprint.
fn fnv1a(bytes: &[u8]) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    for &byte in bytes {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

// ---------------------------------------------------------------------------
// Apply + read-back verification
// ---------------------------------------------------------------------------

/// Apply ONLY the explicitly approved candidate ids.
///
/// The review is a proposal, never an instruction: `approved_ids` must be
/// non-empty and every id must exist in the review. The candidate regions are
/// re-derived from the *input* at apply time and checked against the review
/// (so a review for a different or changed document is refused), then passed
/// to the existing [`crate::redact_page`] tool. No approval means no write.
///
/// # Budget
///
/// One text layer per page named in the approval set, one rewrite pass per
/// such page, and one fresh parse plus text layer for the read-back check.
/// All allocations are bounded by the document size.
///
/// # Malformed Input
///
/// An empty or unknown approval set, a wrong schema, a changed source, or a
/// candidate whose re-derived text/region disagree with the review are all
/// typed errors that write nothing. A redacted match that survives in the
/// output's extracted text or raw bytes is a typed error and the output is
/// left untouched.
///
/// # Errors
///
/// A typed [`CliError`] carrying a `PatternRedactError` code. On any error no
/// output file is committed.
pub fn apply_reviewed(
    path: &str,
    review: &PatternReview,
    approved_ids: &[String],
    output: &str,
) -> ToolResult<Verification> {
    apply_reviewed_inner(path, review, approved_ids, output).map_err(CliError::from)
}

fn apply_reviewed_inner(
    path: &str,
    review: &PatternReview,
    approved_ids: &[String],
    output: &str,
) -> Result<Verification, PatternRedactError> {
    if approved_ids.is_empty() {
        return Err(PatternRedactError::EmptyApproval);
    }
    if path == output {
        return Err(PatternRedactError::Io(
            "input and output must be different paths".to_string(),
        ));
    }
    if review.schema != REVIEW_SCHEMA {
        return Err(PatternRedactError::ReviewMismatch(format!(
            "schema {} != {REVIEW_SCHEMA}",
            review.schema
        )));
    }
    let src = crate::read_file(path).map_err(|e| PatternRedactError::Io(e.0))?;
    if u64::try_from(src.len()).unwrap_or(u64::MAX) != review.source_len
        || fnv1a(&src) != review.source_fnv
    {
        return Err(PatternRedactError::ReviewMismatch(
            "the input bytes differ from the reviewed source".to_string(),
        ));
    }

    let budget = Budget::unlimited();
    let clock = crate::shell_clock();

    // Resolve each approved id to a freshly re-derived needle and region.
    let mut needles: Vec<ProofNeedle> = Vec::new();
    let mut by_page: std::collections::BTreeMap<usize, Vec<[f64; 4]>> =
        std::collections::BTreeMap::new();
    let session = Session::open(src.clone(), &budget, &clock)
        .map_err(|e| PatternRedactError::Io(format!("{path}: cannot open PDF: {e}")))?;
    let mut layers: std::collections::BTreeMap<usize, PageTextLayer> =
        std::collections::BTreeMap::new();

    let mut seen_ids: std::collections::BTreeSet<&str> = std::collections::BTreeSet::new();
    for id in approved_ids {
        if !seen_ids.insert(id.as_str()) {
            continue;
        }
        let candidate = review
            .candidates
            .iter()
            .find(|c| c.id == *id)
            .ok_or_else(|| PatternRedactError::UnknownId(id.clone()))?;
        if candidate.page >= session.len() {
            return Err(PatternRedactError::ReviewMismatch(format!(
                "candidate {id} names page {} outside the document",
                candidate.page
            )));
        }
        let layer = match layers.entry(candidate.page) {
            std::collections::btree_map::Entry::Vacant(entry) => {
                let mut g = crate::runtime::cli_guard(&budget, &clock);
                let built = page_text_layer(&session, candidate.page, &budget, &mut g)
                    .map_err(|e| PatternRedactError::Io(format!("page {}: {e}", candidate.page)))?;
                entry.insert(built)
            }
            std::collections::btree_map::Entry::Occupied(entry) => entry.into_mut(),
        };
        let Some(line) = layer.lines.get(candidate.line) else {
            return Err(PatternRedactError::ReviewMismatch(format!(
                "candidate {id} names a line that no longer exists"
            )));
        };
        let units: Vec<u16> = line.text.encode_utf16().collect();
        let Some(slice) = units.get(candidate.start..candidate.end) else {
            return Err(PatternRedactError::ReviewMismatch(format!(
                "candidate {id} has an out-of-range text range"
            )));
        };
        let raw = String::from_utf16_lossy(slice);
        if raw.trim().is_empty() || mask(candidate.pattern, &raw) != candidate.masked {
            return Err(PatternRedactError::ReviewMismatch(format!(
                "candidate {id} text no longer matches the review"
            )));
        }
        let region = region_for(&line.chars, candidate.start, candidate.end, DEFAULT_PADDING)
            .ok_or_else(|| {
                PatternRedactError::ReviewMismatch(format!("candidate {id} has no finite region"))
            })?;
        if !regions_close(region, candidate.region) {
            return Err(PatternRedactError::ReviewMismatch(format!(
                "candidate {id} region no longer matches the review"
            )));
        }
        needles.push(ProofNeedle {
            id: id.clone(),
            page: candidate.page,
            text: raw,
        });
        by_page
            .entry(candidate.page)
            .or_default()
            .push(candidate.region);
    }
    drop(session);
    drop(layers);

    // Apply through the existing redaction tool, one rewrite per page. The
    // intermediates live in the tool's path backend (real temp files on the
    // CLI, the memory table in a browser session) and are removed on every
    // exit path.
    let work = WorkArea::new()?;
    let mut current = path.to_string();
    let mut last_verification: Option<Verification> = None;
    for (index, (page, rects)) in by_page.iter().enumerate() {
        let next = work.key(index);
        let tuples: Vec<(f64, f64, f64, f64)> = rects
            .iter()
            .map(|r| {
                (
                    r.first().copied().unwrap_or(0.0),
                    r.get(1).copied().unwrap_or(0.0),
                    r.get(2).copied().unwrap_or(0.0),
                    r.get(3).copied().unwrap_or(0.0),
                )
            })
            .collect();
        let verification = crate::redact_page(&current, *page, &tuples, &next)
            .map_err(|e| PatternRedactError::Io(e.0))?;
        last_verification = Some(verification);
        current = next;
    }

    // Read-back BEFORE committing to `output`.
    let out_bytes = crate::read_file(&current).map_err(|e| PatternRedactError::Io(e.0))?;
    verify_no_survivors(&out_bytes, &needles, &budget, &clock)?;

    // Only now is the destination touched.
    crate::write_gate::atomic_write(&out_bytes, output, &crate::runtime::token())
        .map_err(|e| PatternRedactError::Io(e.0))?;
    drop(work);
    // The counts are the redaction gate's own measurements of the output we
    // just verified and committed; reporting `Verification::copied` here would
    // falsely claim nothing was rewritten.
    last_verification.ok_or_else(|| PatternRedactError::Io("no candidate was applied".to_string()))
}

/// Two padded regions agree when every component is within a point.
///
/// # Budget
///
/// Four comparisons.
///
/// # Malformed Input
///
/// Non-finite stored values never compare equal.
fn regions_close(a: [f64; 4], b: [f64; 4]) -> bool {
    a.iter()
        .zip(b.iter())
        .all(|(x, y)| (x - y).abs() <= 1e-3 && x.is_finite() && y.is_finite())
}

/// Read-back verification: the matched text of every approved candidate must
/// be absent from both the fresh text layer and the raw output bytes.
///
/// # Budget
///
/// One fresh parse and one text layer per page named by a needle, plus one
/// raw-byte window scan per needle. No state from the writer is consulted.
///
/// # Malformed Input
///
/// An output that does not re-open, or that has no pages, is a verification
/// failure. A missing needle is a pass; the caller decides which needles are
/// required.
///
/// # Errors
///
/// [`PatternRedactError::VerifyFailed`] naming the candidate id and the
/// surviving medium, or an IO error for an unreadable output.
fn verify_no_survivors(
    bytes: &[u8],
    needles: &[ProofNeedle],
    budget: &Budget,
    clock: &impl Clock,
) -> Result<(), PatternRedactError> {
    let session = Session::open(bytes.to_vec(), budget, clock)
        .map_err(|e| PatternRedactError::Io(format!("cannot re-open output: {e}")))?;
    if session.is_empty() {
        return Err(PatternRedactError::VerifyFailed(
            "the output has no pages".to_string(),
        ));
    }
    let mut layers: std::collections::BTreeMap<usize, PageTextLayer> =
        std::collections::BTreeMap::new();
    for needle in needles {
        if needle.page >= session.len() {
            return Err(PatternRedactError::VerifyFailed(format!(
                "candidate {} names a page outside the output",
                needle.id
            )));
        }
        let layer = match layers.entry(needle.page) {
            std::collections::btree_map::Entry::Vacant(entry) => {
                let mut g = crate::runtime::cli_guard(budget, clock);
                let built =
                    page_text_layer(&session, needle.page, budget, &mut g).map_err(|e| {
                        PatternRedactError::Io(format!("read-back page {}: {e}", needle.page))
                    })?;
                entry.insert(built)
            }
            std::collections::btree_map::Entry::Occupied(entry) => entry.into_mut(),
        };
        let text = layer.text();
        if text.contains(&needle.text) {
            return Err(PatternRedactError::VerifyFailed(format!(
                "candidate {} survived in extracted text",
                needle.id
            )));
        }
        let raw = needle.text.as_bytes();
        if !raw.is_empty() && bytes.windows(raw.len()).any(|w| w == raw) {
            return Err(PatternRedactError::VerifyFailed(format!(
                "candidate {} survived in raw bytes",
                needle.id
            )));
        }
    }
    Ok(())
}

/// Process-wide scratch counter so parallel applies never share a directory.
#[cfg(feature = "native-fs")]
static SCRATCH_SEQ: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// Intermediate path backend for the per-page rewrite chain.
struct WorkArea {
    #[cfg(feature = "native-fs")]
    dir: Option<std::path::PathBuf>,
    #[cfg(not(feature = "native-fs"))]
    prefix: String,
    counter: std::sync::atomic::AtomicUsize,
}

impl WorkArea {
    /// Create a unique scratch namespace.
    ///
    /// # Budget
    ///
    /// O(1) besides one directory creation on the CLI.
    ///
    /// # Malformed Input
    ///
    /// A temp directory that cannot be created is a typed IO error in memory
    /// mode it is just a key prefix.
    fn new() -> Result<Self, PatternRedactError> {
        #[cfg(feature = "native-fs")]
        {
            if crate::memory::active() {
                return Ok(Self {
                    dir: None,
                    counter: std::sync::atomic::AtomicUsize::new(0),
                });
            }
            let seq = SCRATCH_SEQ.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let dir = std::env::temp_dir().join(format!(
                "selis-redact-patterns-{}-{seq}",
                std::process::id()
            ));
            std::fs::create_dir_all(&dir)
                .map_err(|e| PatternRedactError::Io(format!("cannot create scratch dir: {e}")))?;
            Ok(Self {
                dir: Some(dir),
                counter: std::sync::atomic::AtomicUsize::new(0),
            })
        }
        #[cfg(not(feature = "native-fs"))]
        {
            Ok(Self {
                prefix: format!("__selis_redact_patterns_{}", std::process::id()),
                counter: std::sync::atomic::AtomicUsize::new(0),
            })
        }
    }

    /// The path/key for rewrite step `index`.
    ///
    /// # Budget
    ///
    /// O(1).
    ///
    /// # Malformed Input
    ///
    /// None.
    fn key(&self, index: usize) -> String {
        let n = self
            .counter
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        #[cfg(feature = "native-fs")]
        {
            match &self.dir {
                Some(dir) => dir
                    .join(format!("step-{index}-{n}.pdf"))
                    .to_string_lossy()
                    .into_owned(),
                None => format!(
                    "__selis_redact_patterns::{}::{}::{}",
                    std::process::id(),
                    index,
                    n
                ),
            }
        }
        #[cfg(not(feature = "native-fs"))]
        {
            let _ = &self.prefix;
            format!(
                "__selis_redact_patterns::{}::{}::{}",
                std::process::id(),
                index,
                n
            )
        }
    }
}

#[cfg(feature = "native-fs")]
impl Drop for WorkArea {
    fn drop(&mut self) {
        if let Some(dir) = &self.dir {
            let _ = std::fs::remove_dir_all(dir);
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::indexing_slicing,
        clippy::arithmetic_side_effects,
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic
    )]

    use super::*;

    const SSN: &str = "123-45-6789";
    const CARD: &str = "4111 1111 1111 1111";
    const EMAIL: &str = "alice@example.com";
    const NAME: &str = "Jane Doe";

    fn object(num: u32, body: &str) -> (u32, Vec<u8>) {
        (num, body.as_bytes().to_vec())
    }

    fn stream(num: u32, extra: &str, data: &[u8]) -> (u32, Vec<u8>) {
        let mut out =
            format!("{num} 0 obj\n<< /Length {} {extra}>>\nstream\n", data.len()).into_bytes();
        out.extend_from_slice(data);
        out.extend_from_slice(b"\nendstream\nendobj\n");
        (num, out)
    }

    fn assemble(objects: &[(u32, Vec<u8>)]) -> Vec<u8> {
        let mut out = b"%PDF-1.4\n".to_vec();
        let mut offsets: Vec<(u32, usize)> = Vec::new();
        for (num, body) in objects {
            offsets.push((*num, out.len()));
            out.extend_from_slice(body);
        }
        let xref = out.len();
        let size = objects.iter().map(|(n, _)| *n).max().unwrap_or(0) + 1;
        let by_num: std::collections::HashMap<u32, usize> = offsets.into_iter().collect();
        out.extend_from_slice(format!("xref\n0 {size}\n").as_bytes());
        out.extend_from_slice(b"0000000000 65535 f \n");
        for num in 1..size {
            let off = by_num.get(&num).copied().unwrap_or(0);
            out.extend_from_slice(format!("{off:010} 00000 n \n").as_bytes());
        }
        out.extend_from_slice(
            format!("trailer\n<< /Size {size} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n")
                .as_bytes(),
        );
        out
    }

    /// One page with four lines of recoverable PII text at known positions.
    ///
    /// Each PII token is its own show operation (its own `Tm` origin). That is
    /// a normal PDF layout — the adversarial corpus uses the same "text split
    /// across runs" shape — and it matters because the low-level redactor
    /// drops a whole show operation when its *origin* falls in the region. A
    /// token sharing one `Tj` with its label could not be removed without
    /// also removing the label; the read-back check would then fail loudly
    /// (see `a_substring_inside_one_show_operation_fails_loudly`).
    fn fixture() -> Vec<u8> {
        let content = format!(
            "BT /F1 12 Tf \
             1 0 0 1 20 700 Tm (SSN) Tj 1 0 0 1 80 700 Tm ({SSN}) Tj \
             1 0 0 1 20 680 Tm (CARD) Tj 1 0 0 1 80 680 Tm ({CARD}) Tj \
             1 0 0 1 20 660 Tm (MAIL) Tj 1 0 0 1 80 660 Tm ({EMAIL}) Tj \
             1 0 0 1 20 640 Tm (NAME) Tj 1 0 0 1 80 640 Tm ({NAME}) Tj ET"
        );
        assemble(&[
            object(1, "1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n"),
            object(2, "2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n"),
            object(
                3,
                "3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 400 760] /Contents 4 0 R /Resources << /Font << /F1 5 0 R >> >> >>\nendobj\n",
            ),
            stream(4, "", content.as_bytes()),
            object(5, "5 0 obj\n<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>\nendobj\n"),
        ])
    }

    struct Fixture {
        dir: std::path::PathBuf,
        src: String,
    }

    impl Fixture {
        fn new(tag: &str) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "selis-redact-patterns-test-{}-{tag}",
                std::process::id()
            ));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).expect("create temp dir");
            let src = dir.join("in.pdf");
            std::fs::write(&src, fixture()).expect("write fixture");
            Self {
                dir,
                src: src.to_string_lossy().into_owned(),
            }
        }

        fn out(&self, name: &str) -> String {
            self.dir.join(name).to_string_lossy().into_owned()
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    fn all_patterns() -> Vec<Pattern> {
        vec![
            Pattern::Ssn,
            Pattern::Card,
            Pattern::Email,
            Pattern::Literal {
                text: NAME.to_string(),
                whole_word: true,
            },
        ]
    }

    fn extracted(path: &str) -> String {
        crate::extract_text(path, None).expect("extract").text
    }

    fn ids(review: &PatternReview, kind: PatternKind) -> Vec<String> {
        review
            .candidates
            .iter()
            .filter(|c| c.pattern == kind)
            .map(|c| c.id.clone())
            .collect()
    }

    #[test]
    fn scan_finds_all_four_kinds_with_correct_regions() {
        let f = Fixture::new("scan");
        let review = find_candidates(&f.src, &all_patterns()).expect("scan");
        assert_eq!(review.candidates.len(), 4, "{review:#?}");
        let kinds: Vec<PatternKind> = review.candidates.iter().map(|c| c.pattern).collect();
        assert_eq!(
            kinds,
            vec![
                PatternKind::Ssn,
                PatternKind::Card,
                PatternKind::Email,
                PatternKind::Literal
            ]
        );
        for c in &review.candidates {
            assert_eq!(c.page, 0);
            assert!(c.start < c.end);
            assert!(
                c.region[2] > 0.0 && c.region[3] > 0.0,
                "degenerate region: {c:?}"
            );
        }
        // Reading order: each secret's y band descends down the page.
        assert!(review.candidates[0].region[1] > review.candidates[1].region[1]);
        assert!(review.candidates[1].region[1] > review.candidates[2].region[1]);
        assert!(review.candidates[2].region[1] > review.candidates[3].region[1]);
        // Raw text really is in the layer at the region (the scan is not
        // guessing from raw bytes).
        let budget = Budget::unlimited();
        let clock = crate::shell_clock();
        let session = Session::open(std::fs::read(&f.src).unwrap(), &budget, &clock).unwrap();
        let mut g = crate::runtime::cli_guard(&budget, &clock);
        let layer = page_text_layer(&session, 0, &budget, &mut g).unwrap();
        for c in &review.candidates {
            let line = &layer.lines[c.line];
            let units: Vec<u16> = line.text.encode_utf16().collect();
            let raw = String::from_utf16_lossy(&units[c.start..c.end]);
            assert_eq!(
                raw,
                match c.pattern {
                    PatternKind::Ssn => SSN,
                    PatternKind::Card => CARD,
                    PatternKind::Email => EMAIL,
                    PatternKind::Literal => NAME,
                }
            );
        }
    }

    #[test]
    fn review_masks_the_secret_and_never_serializes_it() {
        let f = Fixture::new("mask");
        let review = find_candidates(&f.src, &all_patterns()).unwrap();
        let masked: Vec<&str> = review
            .candidates
            .iter()
            .map(|c| c.masked.as_str())
            .collect();
        assert_eq!(masked[0], "***-**-6789");
        assert!(masked[1].contains("1111") && masked[1].starts_with('*'));
        assert!(masked[2].starts_with("a***@") && masked[2].ends_with("example.com"));
        assert_eq!(masked[3], "**** ***");
        for secret in [SSN, CARD, EMAIL, NAME] {
            assert!(
                !masked.iter().any(|m| m.contains(secret)),
                "leaked {secret}"
            );
        }
        let json = String::from_utf8(review.to_json().unwrap()).unwrap();
        for secret in [SSN, CARD, EMAIL, NAME] {
            assert!(!json.contains(secret), "review JSON leaked {secret}");
        }
    }

    #[test]
    fn applying_an_approved_subset_removes_only_those_matches() {
        let f = Fixture::new("apply");
        let review = find_candidates(&f.src, &all_patterns()).unwrap();
        let mut approved = ids(&review, PatternKind::Ssn);
        approved.extend(ids(&review, PatternKind::Literal));
        let out = f.out("subset.pdf");
        let verification = apply_reviewed(&f.src, &review, &approved, &out).expect("apply");
        assert!(verdict_ok(&verification));

        let text = extracted(&out);
        assert!(!text.contains(SSN), "approved SSN survived:\n{text}");
        assert!(!text.contains(NAME), "approved name survived:\n{text}");
        assert!(text.contains(CARD), "unapproved card was removed:\n{text}");
        assert!(
            text.contains(EMAIL),
            "unapproved email was removed:\n{text}"
        );

        let bytes = std::fs::read(&out).unwrap();
        assert!(!contains(&bytes, SSN), "SSN survived in raw bytes");
        assert!(!contains(&bytes, NAME), "name survived in raw bytes");
        assert!(
            contains(&bytes, CARD),
            "unapproved card lost from raw bytes"
        );
        assert!(
            contains(&bytes, EMAIL),
            "unapproved email lost from raw bytes"
        );
    }

    #[test]
    fn verifier_is_not_vacuous() {
        let f = Fixture::new("control");
        let review = find_candidates(&f.src, &all_patterns()).unwrap();
        let needle = ProofNeedle {
            id: ids(&review, PatternKind::Ssn)[0].clone(),
            page: 0,
            text: SSN.to_string(),
        };
        let budget = Budget::unlimited();
        let clock = crate::shell_clock();

        // Control 1: the untouched input still contains the secret, so the
        // verifier MUST reject it. If this passes, the check is a no-op.
        let original = std::fs::read(&f.src).unwrap();
        let err = verify_no_survivors(&original, std::slice::from_ref(&needle), &budget, &clock)
            .expect_err("verifier must catch an unredacted secret");
        assert_eq!(err.code(), "VERIFY_FAILED");

        // Control 2: fully redacted output passes the same check.
        let out = f.out("control.pdf");
        let all: Vec<String> = review.candidates.iter().map(|c| c.id.clone()).collect();
        apply_reviewed(&f.src, &review, &all, &out).expect("apply all");
        let redacted = std::fs::read(&out).unwrap();
        verify_no_survivors(&redacted, &[needle], &budget, &clock)
            .expect("correctly redacted output must pass");
    }

    #[test]
    fn empty_and_unknown_approval_ids_refuse_and_write_nothing() {
        let f = Fixture::new("refuse");
        let review = find_candidates(&f.src, &all_patterns()).unwrap();
        let out = f.out("refused.pdf");

        let err = apply_reviewed(&f.src, &review, &[], &out).unwrap_err();
        assert_eq!(err.0, PatternRedactError::EmptyApproval.to_string());
        assert!(!std::path::Path::new(&out).exists());

        let err = apply_reviewed(&f.src, &review, &["p0-l0-c999".to_string()], &out).unwrap_err();
        assert!(err.0.contains("UNKNOWN_ID"), "{err}");
        assert!(!std::path::Path::new(&out).exists());
    }

    #[test]
    fn a_tampered_review_is_refused() {
        let f = Fixture::new("tamper");
        let mut review = find_candidates(&f.src, &all_patterns()).unwrap();
        if let Some(c) = review
            .candidates
            .iter_mut()
            .find(|c| c.pattern == PatternKind::Ssn)
        {
            c.masked = "totally different".to_string();
        }
        let out = f.out("tampered.pdf");
        let ids = ids(&review, PatternKind::Ssn);
        let err = apply_reviewed(&f.src, &review, &ids, &out).unwrap_err();
        assert!(err.0.contains("REVIEW_MISMATCH"), "{err}");
        assert!(!std::path::Path::new(&out).exists());
    }

    #[test]
    fn a_review_for_a_different_file_is_refused() {
        let f = Fixture::new("mismatch");
        let review = find_candidates(&f.src, &all_patterns()).unwrap();
        let other = f.out("other.pdf");
        let mut changed = fixture();
        changed.extend_from_slice(b"\n% trailing bytes change the digest\n");
        std::fs::write(&other, changed).unwrap();
        let out = f.out("mismatch-out.pdf");
        let ids = ids(&review, PatternKind::Ssn);
        let err = apply_reviewed(&other, &review, &ids, &out).unwrap_err();
        assert!(err.0.contains("REVIEW_MISMATCH"), "{err}");
    }

    #[test]
    fn recognizer_edge_cases() {
        let u: Vec<u16> = "x000-00-0000 y123-45-6789".encode_utf16().collect();
        assert_eq!(find_ssn(&u), vec![(14, 25)]);
        // A Luhn-invalid 16-digit run is not a card.
        let bad: Vec<u16> = "1234567812345678".encode_utf16().collect();
        assert!(find_card(&bad).is_empty());
        // A Luhn-valid run is.
        let good: Vec<u16> = "4111111111111111".encode_utf16().collect();
        assert_eq!(find_card(&good), vec![(0, 16)]);
        // Whole-word literal does not match inside a longer word.
        let text: Vec<u16> = "Jane Doe Janes Doe".encode_utf16().collect();
        let needle: Vec<u16> = "Jane".encode_utf16().collect();
        assert_eq!(find_literal(&text, &needle, true), vec![(0, 4)]);
        assert_eq!(find_literal(&text, &needle, false), vec![(0, 4), (9, 13)]);
    }

    /// A page where the SSN shares one show operation with its label. The
    /// low-level redactor can only drop a whole show op, so a region covering
    /// just the SSN cannot remove it.
    fn embedded_ssn_fixture() -> Vec<u8> {
        let content = format!("BT /F1 12 Tf 20 700 Td (SSN {SSN}) Tj ET");
        assemble(&[
            object(1, "1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n"),
            object(2, "2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n"),
            object(
                3,
                "3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 400 760] /Contents 4 0 R /Resources << /Font << /F1 5 0 R >> >> >>\nendobj\n",
            ),
            stream(4, "", content.as_bytes()),
            object(5, "5 0 obj\n<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>\nendobj\n"),
        ])
    }

    #[test]
    fn a_substring_inside_one_show_operation_fails_loudly() {
        // ADR-P0023: a redaction that cannot actually remove the content must
        // fail, never produce a file that looks redacted but still leaks.
        let f = Fixture::new("embedded");
        std::fs::write(&f.src, embedded_ssn_fixture()).unwrap();
        let review = find_candidates(&f.src, &[Pattern::Ssn]).unwrap();
        assert_eq!(review.candidates.len(), 1);
        let out = f.out("embedded-out.pdf");
        let ids = ids(&review, PatternKind::Ssn);
        let err = apply_reviewed(&f.src, &review, &ids, &out).unwrap_err();
        assert!(err.0.contains("VERIFY_FAILED"), "{err}");
        assert!(
            !std::path::Path::new(&out).exists(),
            "no file on verify failure"
        );
    }

    fn contains(haystack: &[u8], needle: &str) -> bool {
        let n = needle.as_bytes();
        !n.is_empty() && haystack.windows(n.len()).any(|w| w == n)
    }

    fn verdict_ok(v: &Verification) -> bool {
        v.verdict == "ok"
    }
}
