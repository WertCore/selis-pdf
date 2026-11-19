//! Safe request/response logic for the pattern-redaction chunk.
//!
//! This module forbids unsafe code: raw guest pointers never appear here, only
//! the copied request bytes and the response bytes. Every production function
//! states its work bound and how malformed input is refused, as the rest of the
//! tool layer does.

#![forbid(unsafe_code)]
#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing,
        clippy::arithmetic_side_effects
    )
)]

use selis_pdf_tools::{
    apply_reviewed, find_candidates, Pattern, PatternReview, ToolError, Verification,
};
use serde::Deserialize;

use crate::{REQUEST_MAGIC, RESPONSE_MAGIC, VERSION};

/// Maximum number of inputs in one request.
const MAX_FILES: u32 = 64;
/// Maximum input name length.
const MAX_NAME_BYTES: u32 = 128;
/// Maximum options JSON length.
const MAX_OPTIONS_BYTES: u32 = 64 * 1024;

/// The verification, the wire reports and the output files of one operation.
pub(crate) type RunOutput = (Verification, Vec<serde_json::Value>, Vec<(String, Vec<u8>)>);

/// The two phases of this chunk. There is deliberately no combined phase.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Phase {
    /// Read-only scan returning a masked review JSON.
    Scan,
    /// Apply explicitly approved ids from a reviewed document.
    Apply,
}

/// The built-in recognisers a scan may ask for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
enum PatternName {
    /// US SSN.
    Ssn,
    /// Luhn-valid card digit run.
    Card,
    /// Email address.
    Email,
}

impl PatternName {
    fn into_pattern(self) -> Pattern {
        match self {
            Self::Ssn => Pattern::Ssn,
            Self::Card => Pattern::Card,
            Self::Email => Pattern::Email,
        }
    }
}

/// The options block carried by the shared tools framing.
#[derive(Debug, Deserialize)]
struct Options {
    /// Which of the two separate operations to run.
    phase: Phase,
    /// Built-in recognisers (scan only).
    #[serde(default)]
    patterns: Vec<PatternName>,
    /// Caller-supplied literal to look for (scan only).
    #[serde(default)]
    literal: Option<String>,
    /// Require word boundaries around the literal (scan only).
    #[serde(default)]
    whole_word: bool,
    /// The reviewed JSON proposal (apply only).
    #[serde(default)]
    review: Option<serde_json::Value>,
    /// Explicitly approved candidate ids (apply only).
    #[serde(default)]
    approved_ids: Vec<String>,
}

/// Parse and validate one options block.
///
/// # Budget
///
/// One JSON parse, bounded by [`MAX_OPTIONS_BYTES`].
///
/// # Malformed Input
///
/// An unknown phase, an unrecognised pattern name, or a scan with no pattern
/// selected is refused. Apply-specific fields are checked by the apply phase.
///
/// # Errors
/// A typed error naming the bad field; no operation is run.
fn parse_options(bytes: &[u8]) -> Result<Options, ToolError> {
    if bytes.is_empty() {
        return Err(ToolError("missing operation options".to_string()));
    }
    let options: Options = serde_json::from_slice(bytes)
        .map_err(|e| ToolError(format!("invalid options JSON: {e}")))?;
    if options.phase == Phase::Scan
        && options.patterns.is_empty()
        && options.literal.as_deref().unwrap_or("").is_empty()
    {
        return Err(ToolError(
            "choose at least one pattern or literal to scan for".to_string(),
        ));
    }
    Ok(options)
}

/// Run the read-only scan and return the masked review as a JSON report.
///
/// # Budget
///
/// One text layer per page plus one linear scan per line, bounded by the
/// document's recovered text. The returned JSON is bounded by the number of
/// matches, which is bounded by the text length.
///
/// # Malformed Input
///
/// A document that cannot be opened, or a page whose text layer cannot be
/// built, is a typed error. Nothing is written: `find_candidates` has no output
/// path in its signature.
///
/// # Errors
/// A typed error from the scan or from JSON serialisation.
fn run_scan(stem: &str, bytes_in: u64, options: &Options) -> Result<RunOutput, ToolError> {
    let mut patterns: Vec<Pattern> = options.patterns.iter().map(|p| p.into_pattern()).collect();
    if let Some(text) = options.literal.as_deref() {
        if !text.is_empty() {
            patterns.push(Pattern::Literal {
                text: text.to_string(),
                whole_word: options.whole_word,
            });
        }
    }
    let review = find_candidates("in/0", &patterns)?;
    let json = review.to_json()?;
    let name = format!("{stem}-review.json");
    // No document is rewritten, so no preservation count is claimed. `pages`
    // is 0 and `checked` is empty on purpose: this is a read-only proposal.
    let verification = Verification {
        pages_in: None,
        pages_out: 0,
        annotations_in: None,
        annotations_out: 0,
        fields_in: None,
        fields_out: 0,
        ocgs_in: None,
        ocgs_out: 0,
        outline_entries_in: None,
        outline_entries_out: 0,
        embedded_files_in: None,
        embedded_files_out: 0,
        bytes_in,
        bytes_out: bytes_in,
        verdict: "review".to_string(),
        checked: Vec::new(),
        repair_mode: None,
        fonts_subset: None,
        glyphs_kept: None,
    };
    let reports = vec![serde_json::json!({
        "name": name,
        "kind": "report",
        "pages": 0,
        "bytes": json.len(),
        "verdict": "review",
    })];
    Ok((verification, reports, vec![(name, json)]))
}

/// Apply only the explicitly approved ids from a reviewed document.
///
/// # Budget
///
/// One text layer per approved page, one rewrite per such page and one fresh
/// parse and text layer for the read-back check (see `apply_reviewed`).
///
/// # Malformed Input
///
/// A missing or malformed review, an empty or unknown approval set, a changed
/// source, or a match that survives the rewrite are all typed errors that write
/// nothing. There is no path that treats "the review has candidates" as
/// approval.
///
/// # Errors
/// A typed error carrying the tool's `EMPTY_APPROVAL`, `UNKNOWN_ID`,
/// `REVIEW_MISMATCH`, `VERIFY_FAILED` or `IO` code; no output is committed.
fn run_apply(stem: &str, options: &Options) -> Result<RunOutput, ToolError> {
    if options.approved_ids.is_empty() {
        return Err(ToolError(
            "EMPTY_APPROVAL: no candidate ids were approved; nothing was applied".to_string(),
        ));
    }
    let review_value = options
        .review
        .as_ref()
        .ok_or_else(|| ToolError("apply needs the review JSON".to_string()))?;
    let review_bytes = serde_json::to_vec(review_value)
        .map_err(|e| ToolError(format!("cannot re-serialise the review: {e}")))?;
    let review = PatternReview::from_json(&review_bytes)?;
    let verification = apply_reviewed("in/0", &review, &options.approved_ids, "out.pdf")?;
    let bytes = selis_pdf_tools::take_output("out.pdf")
        .ok_or_else(|| ToolError("the verified output vanished from the session".to_string()))?;
    let name = format!("{stem}-redacted.pdf");
    let reports = vec![serde_json::json!({
        "name": name,
        "kind": "pdf",
        "pages": verification.pages_out,
        "bytes": bytes.len(),
        "verdict": verification.verdict,
        "overTarget": false,
    })];
    Ok((verification, reports, vec![(name, bytes)]))
}

/// Build one `SLTR` response frame.
pub(crate) fn build_response(
    status: u32,
    message: &[u8],
    outputs: &[(String, Vec<u8>)],
) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&RESPONSE_MAGIC.to_le_bytes());
    out.extend_from_slice(&status.to_le_bytes());
    put_bytes(&mut out, message);
    let count = u32::try_from(outputs.len()).unwrap_or(u32::MAX);
    out.extend_from_slice(&count.to_le_bytes());
    for (name, bytes) in outputs {
        put_bytes(&mut out, name.as_bytes());
        put_bytes(&mut out, bytes);
    }
    out
}

fn put_bytes(out: &mut Vec<u8>, bytes: &[u8]) {
    let len = u32::try_from(bytes.len()).unwrap_or(u32::MAX);
    out.extend_from_slice(&len.to_le_bytes());
    out.extend_from_slice(bytes);
}

/// Parse a framed request and dispatch its single operation.
///
/// # Budget
///
/// Linear in the request size plus the work of the chosen phase. The input
/// table is cleared on every exit path so no document bytes survive a request.
///
/// # Malformed Input
///
/// Bad magic, an unsupported version, an unknown op, malformed options, an
/// empty or oversized file list, an invalid name, or trailing bytes are all
/// typed errors. Only op 47 is accepted — tools/optimise/compare ops stay
/// unknown here, so a stale card cannot silently run the wrong module.
///
/// # Errors
/// A typed error; the caller turns it into an `SLTR` status 1 frame.
pub(crate) fn dispatch(request: &[u8]) -> Result<RunOutput, ToolError> {
    // The in-memory session table is process-wide. One request at a time, as
    // in the tools chunk; recover from a poisoned lock so one bad request
    // cannot brick the next one in the same browser page.
    static REQUEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _serial = REQUEST_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let _memory = selis_pdf_tools::MemorySession::activate();
    selis_pdf_tools::clear();
    let result = parse_and_run(request);
    selis_pdf_tools::clear();
    result
}

/// Run a parsed request. See [`dispatch`] for the bounds and refusal rules.
fn parse_and_run(request: &[u8]) -> Result<RunOutput, ToolError> {
    let mut cursor = Cursor::new(request);
    let magic = cursor
        .u32()
        .ok_or_else(|| ToolError("truncated request magic".to_string()))?;
    let version = cursor
        .u32()
        .ok_or_else(|| ToolError("truncated request version".to_string()))?;
    let op = cursor
        .u32()
        .ok_or_else(|| ToolError("truncated request operation".to_string()))?;
    if magic != REQUEST_MAGIC {
        return Err(ToolError("bad request magic".to_string()));
    }
    if version != VERSION {
        return Err(ToolError("unsupported tool protocol version".to_string()));
    }
    if op != 47 {
        return Err(ToolError("unknown tool operation".to_string()));
    }
    let options_bytes = cursor
        .bytes(MAX_OPTIONS_BYTES)
        .ok_or_else(|| ToolError("truncated options".to_string()))?;
    let options = parse_options(options_bytes)?;

    let file_count = cursor
        .u32()
        .ok_or_else(|| ToolError("truncated file count".to_string()))?;
    if file_count == 0 {
        return Err(ToolError("no input files".to_string()));
    }
    if file_count > MAX_FILES {
        return Err(ToolError("too many input files".to_string()));
    }
    let mut first_name = String::new();
    let mut first_len = 0usize;
    for index in 0..file_count {
        let name = cursor
            .bytes(MAX_NAME_BYTES)
            .ok_or_else(|| ToolError("truncated input name".to_string()))?;
        let name = std::str::from_utf8(name)
            .map_err(|_| ToolError("input name is not UTF-8".to_string()))?
            .to_string();
        let data = cursor
            .bytes(u32::MAX)
            .ok_or_else(|| ToolError("truncated input bytes".to_string()))?;
        if index == 0 {
            first_name = name;
            first_len = data.len();
        }
        // The host never controls the internal session key.
        selis_pdf_tools::insert_input(&format!("in/{index}"), data.to_vec());
    }
    if cursor.pos != request.len() {
        return Err(ToolError("trailing bytes in tool request".to_string()));
    }

    let stem = file_stem(&first_name);
    let held = u64::try_from(first_len).unwrap_or(u64::MAX);
    match options.phase {
        Phase::Scan => run_scan(&stem, held, &options),
        Phase::Apply => run_apply(&stem, &options),
    }
}

/// A safe cursor over a copied request.
struct Cursor<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> Cursor<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, pos: 0 }
    }

    fn u32(&mut self) -> Option<u32> {
        let end = self.pos.checked_add(4)?;
        let slice = self.bytes.get(self.pos..end)?;
        let array: [u8; 4] = slice.try_into().ok()?;
        self.pos = end;
        Some(u32::from_le_bytes(array))
    }

    fn bytes(&mut self, max: u32) -> Option<&'a [u8]> {
        let len = self.u32()?;
        if len > max {
            return None;
        }
        let end = self.pos.checked_add(len as usize)?;
        let slice = self.bytes.get(self.pos..end)?;
        self.pos = end;
        Some(slice)
    }
}

/// A safe download name derived from the user's first file name.
fn file_stem(name: &str) -> String {
    let base = name.rsplit(['/', '\\']).next().unwrap_or(name);
    let stem = base.rsplit_once('.').map_or(base, |(stem, _)| stem);
    let mut out = String::new();
    for ch in stem.chars().take(80) {
        if ch.is_alphanumeric() || ch == ' ' || ch == '-' || ch == '_' {
            out.push(ch);
        } else {
            out.push('_');
        }
    }
    let trimmed = out.trim_matches(|ch: char| ch == ' ' || ch == '_');
    if trimmed.is_empty() {
        "document".to_string()
    } else {
        trimmed.to_string()
    }
}

/// Encode a framed request for tests and for local verification.
#[cfg(test)]
pub(crate) fn request(op: u32, options: &[u8], files: &[(&str, &[u8])]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&REQUEST_MAGIC.to_le_bytes());
    out.extend_from_slice(&VERSION.to_le_bytes());
    out.extend_from_slice(&op.to_le_bytes());
    out.extend_from_slice(
        &u32::try_from(options.len())
            .unwrap_or(u32::MAX)
            .to_le_bytes(),
    );
    out.extend_from_slice(options);
    out.extend_from_slice(&u32::try_from(files.len()).unwrap_or(u32::MAX).to_le_bytes());
    for (name, bytes) in files {
        out.extend_from_slice(&u32::try_from(name.len()).unwrap_or(u32::MAX).to_le_bytes());
        out.extend_from_slice(name.as_bytes());
        out.extend_from_slice(&u32::try_from(bytes.len()).unwrap_or(u32::MAX).to_le_bytes());
        out.extend_from_slice(bytes);
    }
    out
}

/// Decode a framed response in tests, checking the framing and the reports.
#[cfg(test)]
pub(crate) fn parse_response(bytes: &[u8]) -> (u32, serde_json::Value, Vec<(String, Vec<u8>)>) {
    let magic = bytes.get(0..4).unwrap_or(&[]);
    assert_eq!(magic, RESPONSE_MAGIC.to_le_bytes());
    let status_bytes = bytes.get(4..8).unwrap_or(&[]);
    let status = u32::from_le_bytes(status_bytes.try_into().expect("status"));
    let mut pos = 8usize;
    let message_len = read_u32(bytes, &mut pos) as usize;
    let message_end = pos.saturating_add(message_len);
    let message: serde_json::Value =
        serde_json::from_slice(bytes.get(pos..message_end).expect("message"))
            .expect("message JSON");
    pos = message_end;
    let count = read_u32(bytes, &mut pos) as usize;
    let mut outputs = Vec::with_capacity(count);
    for _ in 0..count {
        let name_len = read_u32(bytes, &mut pos) as usize;
        let name_end = pos.saturating_add(name_len);
        let name = std::str::from_utf8(bytes.get(pos..name_end).expect("name"))
            .expect("name UTF-8")
            .to_string();
        pos = name_end;
        let data_len = read_u32(bytes, &mut pos) as usize;
        let data_end = pos.saturating_add(data_len);
        outputs.push((
            name,
            bytes.get(pos..data_end).expect("output bytes").to_vec(),
        ));
        pos = data_end;
    }
    assert_eq!(pos, bytes.len(), "trailing response bytes");
    (status, message, outputs)
}

#[cfg(test)]
fn read_u32(bytes: &[u8], pos: &mut usize) -> u32 {
    let end = pos.saturating_add(4);
    let slice: [u8; 4] = bytes
        .get(*pos..end)
        .expect("four bytes")
        .try_into()
        .expect("four bytes");
    *pos = end;
    u32::from_le_bytes(slice)
}

/// Run one request through the serialised in-memory session.
pub(crate) fn run_request(request: &[u8]) -> Vec<u8> {
    match dispatch(request) {
        Ok((verification, reports, outputs)) => {
            let mut message =
                serde_json::to_value(&verification).unwrap_or(serde_json::Value::Null);
            if let Some(object) = message.as_object_mut() {
                object.insert("outputs".to_string(), serde_json::Value::Array(reports));
            }
            let message = serde_json::to_vec(&message).unwrap_or_default();
            build_response(0, &message, &outputs)
        }
        Err(error) => build_response(1, error.to_string().as_bytes(), &[]),
    }
}
