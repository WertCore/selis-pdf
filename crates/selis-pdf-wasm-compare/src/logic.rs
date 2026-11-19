#![forbid(unsafe_code)]
//! The pure request logic of the compare chunk.
//!
//! Kept separate from the C ABI in `lib.rs` so this half of the crate literally
//! forbids `unsafe`: it only parses bounded bytes, calls into the in-memory
//! tools session, and builds a bounded response. The raw-pointer boundary lives
//! in `lib.rs` and is the only place `unsafe` appears.

use selis_pdf_tools::{ToolError, Verification};

/// Chunk identifier used by the asset manifest and the host protocol.
pub(crate) const CHUNK_ID: &str = "compare";
/// Request magic, `SLTS` (shared with the tools and optimise chunks).
pub(crate) const REQUEST_MAGIC: u32 = 0x5354_4C53;
/// Response magic, `SLTR` (shared with the tools and optimise chunks).
pub(crate) const RESPONSE_MAGIC: u32 = 0x5254_4C53;
/// Protocol version (shared with the tools chunk).
pub(crate) const VERSION: u32 = 1;
/// Maximum number of inputs in one request. Compare uses exactly two.
const MAX_FILES: u32 = 64;
/// Maximum input name length.
const MAX_NAME_BYTES: u32 = 128;
/// Maximum options JSON length.
const MAX_OPTIONS_BYTES: u32 = 64 * 1024;
/// The render resolution used when the host sends no explicit DPI.
const DEFAULT_DPI: u32 = 72;
/// Highest DPI the core comparison accepts (mirrored here for a clear error).
const MAX_DPI: u32 = 600;

/// The single operation this chunk exports. The number is the stable host op
/// code, deliberately not re-numbered from one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Op {
    /// Compare two documents, page by page, pixels and whole-document text.
    Compare,
}

impl Op {
    /// Parse the stable wire code.
    pub(crate) fn from_wire(value: u32) -> Option<Self> {
        match value {
            46 => Some(Self::Compare),
            _ => None,
        }
    }
}

/// The verification, the per-output JSON record, and the wire files described
/// by the tools protocol's `outputs` array.
pub(crate) type RunOutput = (Verification, Vec<serde_json::Value>, Vec<(String, Vec<u8>)>);

/// Build the `SLTR` response buffer.
///
/// # Budget
///
/// Linear in the message and output sizes. Every length is written as a
/// checked `u32`; an oversized value becomes `u32::MAX` rather than truncating
/// into a smaller field.
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

/// Run one compare request against the process-wide in-memory session.
///
/// A lock serialises requests (the session table is cleared at both edges),
/// exactly as in the tools and optimise chunks. A poisoned lock is recovered
/// so one bad request cannot brick the next one in the same browser page.
pub(crate) fn run_request(request: &[u8]) -> Vec<u8> {
    static REQUEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _serial = REQUEST_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    // A workspace test unifies the CLI's `native-fs` feature into this crate.
    // The guard keeps this request on the in-memory backend regardless.
    let _memory = selis_pdf_tools::MemorySession::activate();
    selis_pdf_tools::clear();
    let result = parse_and_run(request);
    selis_pdf_tools::clear();
    match result {
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

    /// # Malformed Input
    ///
    /// A length above `max`, or one that would run past the request, is
    /// refused. No slice is returned for an out-of-bounds block.
    fn bytes(&mut self, max: u32) -> Option<&'a [u8]> {
        let len = self.u32()?;
        if len > max {
            return None;
        }
        let end = self.pos.checked_add(usize::try_from(len).ok()?)?;
        let slice = self.bytes.get(self.pos..end)?;
        self.pos = end;
        Some(slice)
    }
}

/// Validate and run a decoded request.
///
/// # Budget
///
/// At most [`MAX_FILES`] inputs, each name at most [`MAX_NAME_BYTES`] and each
/// options block at most [`MAX_OPTIONS_BYTES`]. Compare needs exactly two
/// inputs; a different count is refused before any document is opened.
///
/// # Malformed Input
///
/// Bad magic, an unsupported version or op, a truncated block, non-UTF-8 names,
/// invalid options JSON, a non-positive or out-of-range DPI, a wrong file count,
/// and trailing bytes are all typed errors. Nothing is read from a real path:
/// the host only ever supplies in-memory keys `in/0` and `in/1`.
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
    let _op = Op::from_wire(op).ok_or_else(|| ToolError("unknown tool operation".to_string()))?;

    let options = cursor
        .bytes(MAX_OPTIONS_BYTES)
        .ok_or_else(|| ToolError("truncated options".to_string()))?;
    let dpi = parse_dpi(options)?;

    let file_count = cursor
        .u32()
        .ok_or_else(|| ToolError("truncated file count".to_string()))?;
    if file_count == 0 {
        return Err(ToolError("no input files".to_string()));
    }
    if file_count > MAX_FILES {
        return Err(ToolError("too many input files".to_string()));
    }
    if file_count != 2 {
        return Err(ToolError("compare needs exactly two PDFs".to_string()));
    }

    let mut first_name = String::new();
    let mut bytes_in: u64 = 0;
    for index in 0..file_count {
        let name = cursor
            .bytes(MAX_NAME_BYTES)
            .ok_or_else(|| ToolError("truncated input name".to_string()))?;
        let name = std::str::from_utf8(name)
            .map_err(|_| ToolError("input name is not UTF-8".to_string()))?
            .to_string();
        if index == 0 {
            first_name = name;
        }
        let data = cursor
            .bytes(u32::MAX)
            .ok_or_else(|| ToolError("truncated input bytes".to_string()))?;
        bytes_in = bytes_in.saturating_add(u64::try_from(data.len()).unwrap_or(u64::MAX));
        // The host never controls the internal session key.
        selis_pdf_tools::insert_input(&format!("in/{index}"), data.to_vec());
    }
    if cursor.pos != request.len() {
        return Err(ToolError("trailing bytes in tool request".to_string()));
    }

    let report = selis_pdf_tools::compare_pdfs("in/0", "in/1", dpi)?;
    let json = report.to_json()?;
    let stem = file_stem(&first_name);
    let name = format!("{stem}-compare.json");
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
        bytes_out: u64::try_from(json.len()).unwrap_or(u64::MAX),
        verdict: "read".to_string(),
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
        "verdict": verification.verdict,
    })];
    Ok((verification, reports, vec![(name, json)]))
}

/// Read the optional render DPI from the shared options block.
///
/// # Malformed Input
///
/// An empty block means the default. A non-object, a non-integer `dpi`, or a
/// value outside 1..=600 is refused with a typed error rather than clamped, so
/// the caller learns that its request was wrong.
fn parse_dpi(options: &[u8]) -> Result<u32, ToolError> {
    if options.is_empty() {
        return Ok(DEFAULT_DPI);
    }
    let value: serde_json::Value = serde_json::from_slice(options)
        .map_err(|e| ToolError(format!("invalid options JSON: {e}")))?;
    let Some(object) = value.as_object() else {
        return Err(ToolError("compare options must be an object".to_string()));
    };
    let Some(raw) = object.get("dpi") else {
        return Ok(DEFAULT_DPI);
    };
    let Some(number) = raw.as_u64() else {
        return Err(ToolError("dpi must be a whole number".to_string()));
    };
    let dpi = u32::try_from(number)
        .map_err(|_| ToolError(format!("dpi must be between 1 and {MAX_DPI}")))?;
    if dpi == 0 || dpi > MAX_DPI {
        return Err(ToolError(format!("dpi must be between 1 and {MAX_DPI}")));
    }
    Ok(dpi)
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
