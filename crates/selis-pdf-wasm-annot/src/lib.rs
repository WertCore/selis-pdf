//! The browser's annotation chunk (SL-5.ANNOT.02/03).
//!
//! A separate `cdylib` from the all-purpose tools chunk. Appearance
//! regeneration and multi-stroke ink were documented as typed gaps rather than
//! widening the tools chunk under budget pressure: `Annotate` (op 35) there
//! still carries only the legacy single-stroke `points` form. Both capabilities
//! now live here, in their own on-demand module, so a reader who never
//! regenerates an appearance or draws a multi-stroke mark never downloads this
//! code. Like every other chunk it has **no imports** (ADR-P0011): no
//! wasm-bindgen, no network, no filesystem. Document bytes arrive as an
//! attachment and stay on device.
//!
//! # Wire format (identical to the tools chunk)
//!
//! Request: magic `SLTS`, version 1, op (u32), options JSON, file count, then
//! each `{name, bytes}`. The one file is the PDF. Response: magic `SLTR`,
//! status, message (verification JSON with an `annotation` report on success,
//! a plain error string on failure), output count, then each `{name, bytes}`.
//!
//! Every write goes through `selis-pdf-annot`: an incremental revision whose
//! original bytes are a verbatim prefix, re-opened and checked before it is
//! returned, and the shared WRITE.05 gate in `selis-pdf-tools` before it is
//! committed to the in-memory session. Every refusal is a typed error.

#![allow(unsafe_code)]

use std::alloc::{alloc, dealloc, Layout};
use std::panic::{catch_unwind, AssertUnwindSafe};

use selis_pdf_annot::{AnnotKind, AnnotSpec};
use selis_pdf_tools::{clear, take_output, write_gate, MemorySession, Verification};
use selis_sandbox::{shell_clock, Budget};
use serde_json::Value;

/// Chunk identifier used by the asset manifest and the host loader.
pub const CHUNK_ID: &str = "annot";
const REQUEST_MAGIC: u32 = 0x5354_4C53;
const RESPONSE_MAGIC: u32 = 0x5254_4C53;
const VERSION: u32 = 1;
const MAX_FILES: u32 = 64;
const MAX_NAME_BYTES: u32 = 128;
const MAX_OPTIONS_BYTES: u32 = 64 * 1024;

/// The ops this chunk serves. The numbers continue the tools protocol's table
/// after the content chunk's 48..=62 range so the host's typed `ToolOp`/op-code
/// map stays the single source of truth.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Op {
    RegenerateAppearances,
    InkMultiStroke,
}

impl Op {
    fn from_wire(value: u32) -> Option<Self> {
        match value {
            63 => Some(Self::RegenerateAppearances),
            64 => Some(Self::InkMultiStroke),
            _ => None,
        }
    }

    /// The short output-file label for this op.
    fn label(self) -> &'static str {
        match self {
            Self::RegenerateAppearances => "appearances",
            Self::InkMultiStroke => "ink",
        }
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

    fn bytes(&mut self, max: u32) -> Option<&'a [u8]> {
        let len = self.u32()?;
        if len > max {
            return None;
        }
        let len = usize::try_from(len).ok()?;
        let end = self.pos.checked_add(len)?;
        let slice = self.bytes.get(self.pos..end)?;
        self.pos = end;
        Some(slice)
    }
}

fn put_bytes(out: &mut Vec<u8>, bytes: &[u8]) {
    let len = u32::try_from(bytes.len()).unwrap_or(u32::MAX);
    out.extend_from_slice(&len.to_le_bytes());
    out.extend_from_slice(bytes);
}

/// Build an `SLTR` response. `message` is verification JSON on success and a
/// plain error string on failure; `outputs` are `(name, bytes)` pairs.
fn build_response(status: u32, message: &[u8], outputs: &[(String, Vec<u8>)]) -> Vec<u8> {
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

/// Decide one request. Safe (no raw pointers), so it is the test seam too.
pub fn run_request(request: &[u8]) -> Vec<u8> {
    // One request at a time, as in the tools and content chunks. Recover from a
    // poisoned lock so a caught panic cannot poison the next browser operation.
    static REQUEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _serial = REQUEST_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let _memory = MemorySession::activate();
    clear();
    let result = parse_and_run(request);
    clear();
    match result {
        Ok((verification, name, bytes, annotation)) => {
            let mut message =
                serde_json::to_value(&verification).unwrap_or(serde_json::Value::Null);
            if let Some(object) = message.as_object_mut() {
                object.insert("annotation".to_string(), annotation);
                object.insert(
                    "outputs".to_string(),
                    serde_json::json!([{
                        "name": name,
                        "kind": "pdf",
                        "pages": verification.pages_out,
                        "bytes": bytes.len(),
                        "verdict": verification.verdict,
                        "overTarget": false,
                    }]),
                );
            }
            let message = serde_json::to_vec(&message).unwrap_or_default();
            build_response(0, &message, &[(name, bytes)])
        }
        Err(error) => build_response(1, error.as_bytes(), &[]),
    }
}

fn parse_and_run(request: &[u8]) -> Result<(Verification, String, Vec<u8>, Value), String> {
    let mut cursor = Cursor::new(request);
    let magic = cursor.u32().ok_or("truncated request magic")?;
    let version = cursor.u32().ok_or("truncated request version")?;
    let op_code = cursor.u32().ok_or("truncated operation")?;
    if magic != REQUEST_MAGIC {
        return Err("bad request magic".to_string());
    }
    if version != VERSION {
        return Err("unsupported protocol version".to_string());
    }
    let op = Op::from_wire(op_code).ok_or("unknown annotation operation")?;
    let options_bytes = cursor.bytes(MAX_OPTIONS_BYTES).ok_or("truncated options")?;
    let options: Value = if options_bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(options_bytes).map_err(|e| format!("invalid options JSON: {e}"))?
    };
    let file_count = cursor.u32().ok_or("truncated file count")?;
    if file_count == 0 || file_count > MAX_FILES {
        return Err("expected one PDF".to_string());
    }
    let mut files: Vec<Vec<u8>> = Vec::new();
    for _ in 0..file_count {
        let name = cursor.bytes(MAX_NAME_BYTES).ok_or("truncated input name")?;
        if std::str::from_utf8(name).is_err() {
            return Err("input name is not UTF-8".to_string());
        }
        let data = cursor.bytes(u32::MAX).ok_or("truncated input bytes")?;
        files.push(data.to_vec());
    }
    if cursor.pos != request.len() {
        return Err("trailing bytes in request".to_string());
    }
    if files.len() != 1 {
        return Err("the annotation ops take exactly one PDF".to_string());
    }
    let pdf = files.first().ok_or("no input document")?;
    if pdf.is_empty() {
        return Err("the input document is empty".to_string());
    }

    let budget = Budget::unlimited();
    let clock = shell_clock();
    let mut g = selis_pdf_tools::runtime::cli_guard(&budget, &clock);
    let (bytes, annotation) = match op {
        Op::RegenerateAppearances => {
            let (bytes, report) = selis_pdf_annot::regenerate_appearances(pdf, &budget, &mut g)
                .map_err(|e| e.to_string())?;
            let annotation = serde_json::json!({
                "kind": "appearances",
                "pages": report.pages,
                "annotations": report.annotations,
                "rebuilt": report.regenerated,
                "need_appearances": report.need_appearances,
            });
            (bytes, annotation)
        }
        Op::InkMultiStroke => {
            let spec = ink_spec(&options)?;
            let (bytes, report) = selis_pdf_annot::annotate(pdf, &spec, &budget, &mut g)
                .map_err(|e| e.to_string())?;
            let annotation = serde_json::json!({
                "kind": "ink",
                "page": report.page,
                "strokes": spec.paths.len(),
                "annots": report.annots,
            });
            (bytes, annotation)
        }
    };
    let verification = commit(pdf, &bytes, &budget, &mut g)?;
    let bytes = take_output("out.pdf").ok_or("operation produced no output")?;
    let name = format!("document-{}.pdf", op.label());
    Ok((verification, name, bytes, annotation))
}

/// Build the ink spec from caller-supplied strokes. The page is 0-based on the
/// wire (the editor's own convention); the engine's `AnnotSpec` is 1-based.
fn ink_spec(options: &Value) -> Result<AnnotSpec, String> {
    let page = u32_option(options, "page")?;
    let strokes = strokes_option(options)?;
    if strokes.is_empty() {
        return Err("an ink annotation needs at least one stroke".to_string());
    }
    let width = f64_option(options, "width", 1.0)?;
    if !width.is_finite() || width < 0.0 {
        return Err("a stroke width must be a finite, non-negative number".to_string());
    }
    let colour = colour_option(options)?;
    let contents = string_option(options, "contents");
    let author = string_option(options, "author");
    if contents.chars().count() > 4_000 {
        return Err("the comment is too long".to_string());
    }
    if author.chars().count() > 256 {
        return Err("the author name is too long".to_string());
    }

    // A zero rectangle is an honest placeholder: the engine derives the real
    // /InkList rectangle from the strokes, grown by half the stroke width.
    let mut spec = AnnotSpec::new(AnnotKind::Ink, page.saturating_add(1), [0.0; 4], colour);
    spec.paths = strokes;
    spec.width = width;
    spec.contents = contents;
    spec.author = author;
    Ok(spec)
}

fn u32_option(options: &Value, key: &str) -> Result<u32, String> {
    let raw = options
        .get(key)
        .and_then(Value::as_u64)
        .ok_or_else(|| format!("{key} must be a non-negative integer"))?;
    u32::try_from(raw).map_err(|_| format!("{key} is too large"))
}

fn f64_option(options: &Value, key: &str, default: f64) -> Result<f64, String> {
    match options.get(key) {
        None | Some(Value::Null) => Ok(default),
        Some(value) => value
            .as_f64()
            .ok_or_else(|| format!("{key} must be a number")),
    }
}

fn colour_option(options: &Value) -> Result<[f64; 3], String> {
    let array = options
        .get("colour")
        .and_then(Value::as_array)
        .ok_or("colour must be an r,g,b array")?;
    if array.len() != 3 {
        return Err("colour must be exactly three numbers".to_string());
    }
    let mut out = [0.0; 3];
    for (channel, value) in out.iter_mut().zip(array) {
        let number = value.as_f64().ok_or("colour has a non-numeric channel")?;
        if !number.is_finite() || !(0.0..=1.0).contains(&number) {
            return Err("colour channels must be finite numbers in 0..=1".to_string());
        }
        *channel = number;
    }
    Ok(out)
}

fn strokes_option(options: &Value) -> Result<Vec<Vec<(f64, f64)>>, String> {
    let raw = options
        .get("strokes")
        .and_then(Value::as_array)
        .ok_or("strokes must be an array of strokes")?;
    let mut strokes = Vec::new();
    for stroke in raw {
        let points = stroke
            .as_array()
            .ok_or("each stroke must be an array of points")?;
        if points.len() < 2 {
            return Err("each stroke needs two or more points".to_string());
        }
        let mut line = Vec::new();
        for point in points {
            let pair = point.as_array().ok_or("each point must be x,y")?;
            if pair.len() != 2 {
                return Err("each point must be exactly x,y".to_string());
            }
            let x = pair
                .first()
                .and_then(Value::as_f64)
                .ok_or("a point has a non-numeric x")?;
            let y = pair
                .get(1)
                .and_then(Value::as_f64)
                .ok_or("a point has a non-numeric y")?;
            if !x.is_finite() || !y.is_finite() {
                return Err("a point is not finite".to_string());
            }
            line.push((x, y));
        }
        strokes.push(line);
    }
    Ok(strokes)
}

fn string_option(options: &Value, key: &str) -> String {
    options
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// Run the shared WRITE.05 gate and build the verification line, exactly as
/// the native CLI's annotation commands do. `write_gate::write_verified` stores
/// the result under `out.pdf` in the live in-memory session.
fn commit(
    src: &[u8],
    bytes: &[u8],
    budget: &Budget,
    g: &mut selis_sandbox::BudgetGuard<'_>,
) -> Result<Verification, String> {
    let observed = selis_pdf_cos::verify::survey(src, budget, g)
        .map_err(|e| format!("input survey failed: {e}"))?;
    let expected = write_gate::expectations_from(&observed, &write_gate::PRESERVE_ON_APPEND);
    let verdict = write_gate::write_verified(bytes, "out.pdf", &expected, budget, g)
        .map_err(|e| format!("write verification failed: {e}"))?;
    Ok(Verification::from_gate(
        Some(&observed),
        &verdict,
        &expected,
        u64::try_from(src.len()).unwrap_or(u64::MAX),
        u64::try_from(bytes.len()).unwrap_or(u64::MAX),
    ))
}

/// Return the chunk id as guest memory, freed with `selis_annot_free`.
///
/// # Safety
/// `out_len` must point to four writable bytes.
#[no_mangle]
pub unsafe extern "C" fn selis_annot_chunk_id(out_len: *mut u32) -> *mut u8 {
    if out_len.is_null() {
        return std::ptr::null_mut();
    }
    let bytes = CHUNK_ID.as_bytes();
    let Ok(len) = u32::try_from(bytes.len()) else {
        return std::ptr::null_mut();
    };
    let ptr = allocate(bytes.len());
    if ptr.is_null() {
        return ptr;
    }
    std::ptr::copy_nonoverlapping(bytes.as_ptr(), ptr, bytes.len());
    unsafe { out_len.write(len) };
    ptr
}

/// Allocate `len` guest bytes for a request or the length out-word.
#[no_mangle]
pub extern "C" fn selis_annot_alloc(len: u32) -> *mut u8 {
    allocate(len as usize)
}

/// # Safety
/// `ptr`/`len` must identify one live allocation from this chunk, freed once.
#[no_mangle]
pub unsafe extern "C" fn selis_annot_free(ptr: *mut u8, len: u32) {
    if ptr.is_null() || len == 0 {
        return;
    }
    let Ok(len) = usize::try_from(len) else {
        return;
    };
    let Ok(layout) = Layout::from_size_align(len, 1) else {
        return;
    };
    unsafe { dealloc(ptr, layout) };
}

/// Run one op. The response pointer is freed with `selis_annot_free`.
///
/// # Safety
/// `request_ptr` must be readable for `request_len` bytes; `out_len` writable.
#[no_mangle]
pub unsafe extern "C" fn selis_annot_run(
    request_ptr: *const u8,
    request_len: u32,
    out_len: *mut u32,
) -> *mut u8 {
    if out_len.is_null() || request_ptr.is_null() {
        return std::ptr::null_mut();
    }
    let Ok(request_len) = usize::try_from(request_len) else {
        return std::ptr::null_mut();
    };
    let request = unsafe { std::slice::from_raw_parts(request_ptr, request_len) };
    let response = catch_unwind(AssertUnwindSafe(|| run_request(request)))
        .unwrap_or_else(|_| build_response(1, b"annotation operation panicked", &[]));
    let Ok(len) = u32::try_from(response.len()) else {
        return std::ptr::null_mut();
    };
    let ptr = allocate(response.len());
    if ptr.is_null() {
        return ptr;
    }
    std::ptr::copy_nonoverlapping(response.as_ptr(), ptr, response.len());
    unsafe { out_len.write(len) };
    ptr
}

/// How many ops this chunk implements (codes 63 and 64).
#[no_mangle]
pub extern "C" fn selis_annot_capabilities() -> u32 {
    2
}

fn allocate(len: usize) -> *mut u8 {
    if len == 0 {
        return std::ptr::null_mut();
    }
    let Ok(layout) = Layout::from_size_align(len, 1) else {
        return std::ptr::null_mut();
    };
    unsafe { alloc(layout) }
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::arithmetic_side_effects,
        clippy::expect_used,
        clippy::indexing_slicing,
        clippy::unwrap_used
    )]

    use super::*;

    fn request(op: u32, options: &[u8], files: &[(&str, &[u8])]) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&REQUEST_MAGIC.to_le_bytes());
        out.extend_from_slice(&VERSION.to_le_bytes());
        out.extend_from_slice(&op.to_le_bytes());
        put_bytes(&mut out, options);
        out.extend_from_slice(&u32::try_from(files.len()).expect("count").to_le_bytes());
        for (name, data) in files {
            put_bytes(&mut out, name.as_bytes());
            put_bytes(&mut out, data);
        }
        out
    }

    fn parse(bytes: &[u8]) -> (u32, Value, Vec<(String, Vec<u8>)>) {
        assert_eq!(bytes[0..4], RESPONSE_MAGIC.to_le_bytes());
        let status = u32::from_le_bytes(bytes[4..8].try_into().expect("status"));
        let mut pos = 8usize;
        let read_u32 = |pos: &mut usize| -> u32 {
            let end = pos.saturating_add(4);
            let value = u32::from_le_bytes(bytes[*pos..end].try_into().expect("u32"));
            *pos = end;
            value
        };
        let message_len = usize::try_from(read_u32(&mut pos)).expect("len");
        let message: Value = serde_json::from_slice(&bytes[pos..pos + message_len]).expect("json");
        pos += message_len;
        let count = usize::try_from(read_u32(&mut pos)).expect("count");
        let mut outputs = Vec::new();
        for _ in 0..count {
            let name_len = usize::try_from(read_u32(&mut pos)).expect("name len");
            let name = String::from_utf8(bytes[pos..pos + name_len].to_vec()).expect("name");
            pos += name_len;
            let data_len = usize::try_from(read_u32(&mut pos)).expect("data len");
            outputs.push((name, bytes[pos..pos + data_len].to_vec()));
            pos += data_len;
        }
        (status, message, outputs)
    }

    /// A one-page PDF whose sole markup annotation carries no `/AP` stream.
    fn pdf_with_stale_markup() -> Vec<u8> {
        let objects = vec![
            "1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n".to_string(),
            "2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n".to_string(),
            "3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Annots [5 0 R] >>\nendobj\n".to_string(),
            "4 0 obj\n<< /Length 0 >>\nstream\n\nendstream\nendobj\n".to_string(),
            "5 0 obj\n<< /Type /Annot /Subtype /Highlight /Rect [10 10 40 30] /QuadPoints [10 10 40 10 40 30 10 30] /C [1 0.92 0.23] >>\nendobj\n".to_string(),
        ];
        let mut out = b"%PDF-1.4\n".to_vec();
        let mut offsets = Vec::new();
        for object in &objects {
            offsets.push(out.len());
            out.extend_from_slice(object.as_bytes());
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

    fn simple_pdf() -> Vec<u8> {
        let objects = vec![
            "1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n".to_string(),
            "2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n".to_string(),
            "3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] >>\nendobj\n"
                .to_string(),
        ];
        let mut out = b"%PDF-1.4\n".to_vec();
        let mut offsets = Vec::new();
        for object in &objects {
            offsets.push(out.len());
            out.extend_from_slice(object.as_bytes());
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
    fn chunk_id_is_annot() {
        assert_eq!(CHUNK_ID, "annot");
        assert_eq!(selis_annot_capabilities(), 2);
    }

    #[test]
    fn regenerate_appearances_is_dispatched_and_preserves_the_prefix() {
        let pdf = pdf_with_stale_markup();
        let (status, message, outputs) =
            parse(&run_request(&request(63, br#"{}"#, &[("doc.pdf", &pdf)])));
        assert_eq!(status, 0, "regenerate failed: {message}");
        assert_eq!(outputs[0].0, "document-appearances.pdf");
        assert!(outputs[0].1.starts_with(&pdf), "incremental append");
        assert_eq!(message["annotation"]["kind"], "appearances");
        assert!(
            message["annotation"]["rebuilt"].as_u64().unwrap_or(0) >= 1,
            "message: {message}"
        );
        assert_eq!(message["annotation"]["need_appearances"], false);
        assert_eq!(message["outputs"][0]["kind"], "pdf");
    }

    #[test]
    fn multi_stroke_ink_is_written_with_every_stroke() {
        let pdf = simple_pdf();
        let options = br#"{"page":0,"strokes":[[[10,10],[20,20],[30,10]],[[40,40],[50,50]]],"width":2,"colour":[0,0,1],"contents":"two strokes"}"#;
        let (status, message, outputs) =
            parse(&run_request(&request(64, options, &[("doc.pdf", &pdf)])));
        assert_eq!(status, 0, "ink failed: {message}");
        assert_eq!(outputs[0].0, "document-ink.pdf");
        assert!(outputs[0].1.starts_with(&pdf), "incremental append");
        assert!(String::from_utf8_lossy(&outputs[0].1).contains("InkList"));
        assert_eq!(message["annotation"]["kind"], "ink");
        assert_eq!(message["annotation"]["strokes"], 2);
    }

    fn failure(bytes: &[u8]) -> (u32, String) {
        let status = u32::from_le_bytes(bytes[4..8].try_into().expect("status"));
        let len = u32::from_le_bytes(bytes[8..12].try_into().expect("len")) as usize;
        let message = String::from_utf8_lossy(&bytes[12..12 + len]).into_owned();
        (status, message)
    }

    #[test]
    fn refuses_unknown_ops_and_malformed_ink() {
        let pdf = simple_pdf();
        let (status, message) = failure(&run_request(&request(1, b"{}", &[("doc.pdf", &pdf)])));
        assert_eq!(status, 1);
        assert!(
            message.contains("unknown annotation operation"),
            "{message}"
        );

        let (status, message) = failure(&run_request(&request(
            64,
            br#"{"page":0,"strokes":[[[10,10]]],"width":2,"colour":[0,0,1]}"#,
            &[("doc.pdf", &pdf)],
        )));
        assert_eq!(status, 1, "a one-point stroke must be refused");
        assert!(message.contains("two or more points"), "{message}");
    }

    #[test]
    fn an_out_of_gamut_colour_and_missing_strokes_are_refused() {
        let pdf = simple_pdf();
        for (options, cause) in [
            (
                br#"{"page":0,"strokes":[[[1,1],[2,2]]],"colour":[0,0,2]}"#.as_slice(),
                "0..=1",
            ),
            (
                br#"{"page":0,"width":2,"colour":[0,0,1]}"#.as_slice(),
                "strokes",
            ),
        ] {
            let (status, message) =
                failure(&run_request(&request(64, options, &[("doc.pdf", &pdf)])));
            assert_eq!(status, 1, "refusal expected: {options:?}");
            assert!(message.contains(cause), "{message}");
        }
    }
}
