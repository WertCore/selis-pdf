//! The optimise lazy chunk: the two heavyweight document operations that used
//! to live in the tools chunk, moved out so the common tools page does not pay
//! for them (font subset, op 41; linearise, op 43).
//!
//! This is a separate `cdylib` from the viewer, the tools, convert, raster,
//! HTML, security, sign and text chunks. Its wire protocol is the same
//! import-free `SLTS`/`SLTR` request/response framing as the tools chunk, so
//! the host can encode and decode with the same functions and only swaps the
//! URL (and the export prefix) for these two operations. Op codes stay stable:
//! 41 is `subset_fonts`, 43 is `linearize`, and 42 remains the unknown sentinel
//! so persisted references to either code keep working.
//!
//! # Why a chunk of its own
//!
//! The TrueType subsetter and the linearisation writer carry the document-wide
//! glyph walk, the CIDToGIDMap rewrite, the Annex F hint tables and the
//! two-xref layout. Together they were roughly a fifth of the tools chunk's
//! brotli weight, yet the tools page only downloads this code when someone
//! actually picks "Subset fonts" or "Linearize". Keeping form-fidelity work in
//! the tools chunk and the optimise engine here keeps both budgets honest.
//!
//! # Wire format
//!
//! All integers are little-endian. A request is:
//!
//! ```text
//! magic "SLTS" (u32)
//! version 1 (u32)
//! operation (u32: 41 subset fonts, 43 linearize)
//! options_json_len (u32), options_json bytes (accepted, ignored by these ops)
//! file_count (u32)
//!   name_len (u32), name bytes
//!   data_len (u32), data bytes
//! ```
//!
//! A response is:
//!
//! ```text
//! magic "SLTR" (u32)
//! status (u32: 0 ok, 1 error)
//! message_len (u32), message bytes (verification JSON on success, error on failure)
//! output_count (u32)
//!   name_len (u32), name bytes
//!   data_len (u32), data bytes
//! ```
//!
//! The host allocates request and length buffers with [`selis_optimize_alloc`],
//! calls [`selis_optimize_run`], copies the response out, and frees both with
//! [`selis_optimize_free`]. The module declares no imports, so the page can
//! instantiate it with an empty import object.

#![allow(unsafe_code)]

use std::alloc::{alloc, dealloc, Layout};
use std::panic::{catch_unwind, AssertUnwindSafe};

use selis_pdf_tools::{ToolError, Verification};

/// Chunk identifier used by the asset manifest and the host protocol.
pub const CHUNK_ID: &str = "optimize";
/// Request magic, `SLTS`.
const REQUEST_MAGIC: u32 = 0x5354_4C53;
/// Response magic, `SLTR`.
const RESPONSE_MAGIC: u32 = 0x5254_4C53;
/// Protocol version (shared with the tools chunk).
const VERSION: u32 = 1;
/// Maximum number of inputs in one request.
const MAX_FILES: u32 = 64;
/// Maximum input name length.
const MAX_NAME_BYTES: u32 = 128;
/// Maximum options JSON length.
const MAX_OPTIONS_BYTES: u32 = 64 * 1024;

/// Operations understood by this chunk. The numbers are the stable host op
/// codes; they are deliberately not re-numbered from one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Op {
    /// Replace embedded TrueType programs with subsets (op 41).
    SubsetFonts,
    /// Rewrite the document into the fast-web-view layout (op 43).
    Linearize,
}

impl Op {
    fn from_wire(value: u32) -> Option<Self> {
        match value {
            41 => Some(Self::SubsetFonts),
            43 => Some(Self::Linearize),
            _ => None,
        }
    }
}

/// Return the chunk id as guest memory. The host reads `out_len` and frees the
/// pointer with [`selis_optimize_free`].
///
/// # Safety
///
/// `out_len` must point to four writable bytes in guest memory.
#[no_mangle]
pub unsafe extern "C" fn selis_optimize_chunk_id(out_len: *mut u32) -> *mut u8 {
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
    // SAFETY: `allocate` returned `bytes.len()` writable bytes.
    unsafe { std::ptr::copy_nonoverlapping(bytes.as_ptr(), ptr, bytes.len()) };
    // SAFETY: the caller promised four writable bytes.
    unsafe { out_len.write(len) };
    ptr
}

/// Availability probe; the optimise chunk is always present once fetched.
#[no_mangle]
pub extern "C" fn selis_optimize_available() -> u32 {
    1
}

/// Allocate `len` bytes of guest memory for the host to fill with a request.
///
/// Returns null when the allocation fails. The caller must free the pointer
/// with [`selis_optimize_free`] and the same length.
#[no_mangle]
pub extern "C" fn selis_optimize_alloc(len: u32) -> *mut u8 {
    allocate(len as usize)
}

/// Free a buffer previously returned by this module.
///
/// # Safety
///
/// `ptr` must have been returned by one of this module's exports with exactly
/// `len` bytes, and must not have been freed already.
#[no_mangle]
pub unsafe extern "C" fn selis_optimize_free(ptr: *mut u8, len: u32) {
    if ptr.is_null() || len == 0 {
        return;
    }
    let Ok(layout) = Layout::from_size_align(len as usize, 1) else {
        return;
    };
    // SAFETY: the caller's contract.
    unsafe { dealloc(ptr, layout) };
}

/// Run one optimise operation.
///
/// The returned pointer is a response buffer; `out_len` receives its length.
/// The host must copy it out and free it with [`selis_optimize_free`]. A null
/// return means the request could not be processed at all.
///
/// # Safety
///
/// `request_ptr` must point to `request_len` readable bytes; `out_len` must
/// point to four writable bytes.
#[no_mangle]
pub unsafe extern "C" fn selis_optimize_run(
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
    // SAFETY: the caller promised `request_ptr` is readable for `request_len`
    // bytes. The guest is single-threaded; the slice does not outlive the call.
    let request = unsafe { std::slice::from_raw_parts(request_ptr, request_len) };
    let response = catch_unwind(AssertUnwindSafe(|| run_request(request))).unwrap_or_else(|_| {
        let message = b"optimise operation panicked";
        build_response(1, message, &[])
    });
    let Ok(len) = u32::try_from(response.len()) else {
        return std::ptr::null_mut();
    };
    let ptr = allocate(response.len());
    if ptr.is_null() {
        return ptr;
    }
    // SAFETY: `allocate` returned `response.len()` writable bytes.
    unsafe { std::ptr::copy_nonoverlapping(response.as_ptr(), ptr, response.len()) };
    // SAFETY: `out_len` was checked non-null above.
    unsafe { out_len.write(len) };
    ptr
}

fn allocate(len: usize) -> *mut u8 {
    if len == 0 {
        return std::ptr::null_mut();
    }
    let Ok(layout) = Layout::from_size_align(len, 1) else {
        return std::ptr::null_mut();
    };
    // SAFETY: layout has a non-zero size.
    unsafe { alloc(layout) }
}

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

fn put_bytes(out: &mut Vec<u8>, bytes: &[u8]) {
    let len = u32::try_from(bytes.len()).unwrap_or(u32::MAX);
    out.extend_from_slice(&len.to_le_bytes());
    out.extend_from_slice(bytes);
}

/// The verification, the wire files, and the per-output report described by
/// the tools protocol's `outputs` array.
type RunOutput = (Verification, Vec<serde_json::Value>, Vec<(String, Vec<u8>)>);

fn run_request(request: &[u8]) -> Vec<u8> {
    // The in-memory session table is process-wide. One request at a time, just
    // as in the tools chunk; recover from a poisoned lock so one bad request
    // cannot brick the next one in the same browser page.
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
    let op = Op::from_wire(op).ok_or_else(|| ToolError("unknown tool operation".to_string()))?;

    // The options block is part of the shared framing. Neither op needs an
    // option today, but it is still bounded and parsed so a malformed request
    // is refused rather than silently accepted.
    let options = cursor
        .bytes(MAX_OPTIONS_BYTES)
        .ok_or_else(|| ToolError("truncated options".to_string()))?;
    if !options.is_empty() {
        serde_json::from_slice::<serde_json::Value>(options)
            .map_err(|e| ToolError(format!("invalid options JSON: {e}")))?;
    }

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
        // The host never controls the internal session key.
        selis_pdf_tools::insert_input(&format!("in/{index}"), data.to_vec());
    }
    if cursor.pos != request.len() {
        return Err(ToolError("trailing bytes in tool request".to_string()));
    }

    let stem = file_stem(&first_name);
    let (name, verification) = match op {
        Op::SubsetFonts => {
            let (_report, verification) = selis_pdf_tools::subset_fonts("in/0", "out.pdf")?;
            (format!("{stem}-subset.pdf"), verification)
        }
        Op::Linearize => {
            let (_report, verification) = selis_pdf_tools::linearize_file("in/0", "out.pdf")?;
            (format!("{stem}-linearized.pdf"), verification)
        }
    };
    let bytes = selis_pdf_tools::take_output("out.pdf")
        .ok_or_else(|| ToolError("the verified output vanished from the session".to_string()))?;
    let report = serde_json::json!({
        "name": name,
        "kind": "pdf",
        "pages": verification.pages_out,
        "bytes": bytes.len(),
        "verdict": verification.verdict,
        "overTarget": false,
    });
    Ok((verification, vec![report], vec![(name, bytes)]))
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

    const TTF: &[u8] = include_bytes!("../../../assets/fonts/LiberationSans-Regular.ttf");

    /// Tests drive one process-wide session table, so they must not overlap.
    static TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn serial<T>(body: impl FnOnce() -> T) -> T {
        let guard = TEST_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let out = body();
        drop(guard);
        out
    }

    fn request(op: u32, options: &[u8], files: &[(&str, &[u8])]) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&REQUEST_MAGIC.to_le_bytes());
        out.extend_from_slice(&VERSION.to_le_bytes());
        out.extend_from_slice(&op.to_le_bytes());
        out.extend_from_slice(&u32::try_from(options.len()).unwrap().to_le_bytes());
        out.extend_from_slice(options);
        out.extend_from_slice(&u32::try_from(files.len()).unwrap().to_le_bytes());
        for (name, bytes) in files {
            out.extend_from_slice(&u32::try_from(name.len()).unwrap().to_le_bytes());
            out.extend_from_slice(name.as_bytes());
            out.extend_from_slice(&u32::try_from(bytes.len()).unwrap().to_le_bytes());
            out.extend_from_slice(bytes);
        }
        out
    }

    fn run(bytes: &[u8]) -> Vec<u8> {
        serial(|| run_request(bytes))
    }

    fn read_u32(bytes: &[u8], pos: &mut usize) -> u32 {
        let end = pos.saturating_add(4);
        let slice: [u8; 4] = bytes[*pos..end].try_into().expect("four bytes");
        *pos = end;
        u32::from_le_bytes(slice)
    }

    fn parse_response(bytes: &[u8]) -> (u32, serde_json::Value, Vec<(String, Vec<u8>)>) {
        assert_eq!(bytes[0..4], RESPONSE_MAGIC.to_le_bytes());
        let status = u32::from_le_bytes(bytes[4..8].try_into().expect("status"));
        let mut pos = 8usize;
        let message_len = usize::try_from(read_u32(bytes, &mut pos)).expect("message len");
        let message_end = pos.saturating_add(message_len);
        let message: serde_json::Value =
            serde_json::from_slice(&bytes[pos..message_end]).expect("message JSON");
        pos = message_end;
        let count = usize::try_from(read_u32(bytes, &mut pos)).expect("output count");
        let mut outputs = Vec::with_capacity(count);
        for _ in 0..count {
            let name_len = usize::try_from(read_u32(bytes, &mut pos)).expect("name len");
            let name_end = pos.saturating_add(name_len);
            let name = std::str::from_utf8(&bytes[pos..name_end])
                .expect("name UTF-8")
                .to_string();
            pos = name_end;
            let data_len = usize::try_from(read_u32(bytes, &mut pos)).expect("data len");
            let data_end = pos.saturating_add(data_len);
            outputs.push((name, bytes[pos..data_end].to_vec()));
            pos = data_end;
        }
        assert_eq!(pos, bytes.len(), "trailing response bytes");
        (status, message, outputs)
    }

    fn error_text(response: &[u8]) -> String {
        let status = u32::from_le_bytes(response[4..8].try_into().expect("status"));
        assert_eq!(status, 1, "expected an error response");
        let message_len =
            usize::try_from(u32::from_le_bytes(response[8..12].try_into().expect("len")))
                .expect("message length");
        std::str::from_utf8(&response[12..12 + message_len])
            .expect("error text")
            .to_string()
    }

    /// One page with a whole embedded Liberation Sans program and a short
    /// WinAnsi string: the subsetter must shrink this substantially.
    fn embedded_font_pdf() -> Vec<u8> {
        let content = b"BT /F1 24 Tf 72 700 Td (Hello, fonts!) Tj ET\n";
        let mut objects: Vec<Vec<u8>> = Vec::new();
        objects.push(b"<< /Type /Catalog /Pages 2 0 R >>".to_vec());
        objects.push(b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec());
        objects.push(b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 300 200] /Resources << /Font << /F1 5 0 R >> >> /Contents 4 0 R >>".to_vec());
        let mut obj4 = format!("<< /Length {} >>\nstream\n", content.len()).into_bytes();
        obj4.extend_from_slice(content);
        obj4.extend_from_slice(b"\nendstream");
        objects.push(obj4);
        objects.push(b"<< /Type /Font /Subtype /TrueType /BaseFont /ABCDEE+LiberationSans /Encoding /WinAnsiEncoding /FontDescriptor << /FontName /ABCDEE+LiberationSans /Flags 4 /FontFile2 6 0 R >> >>".to_vec());
        let mut obj6 = format!(
            "<< /Length {} /Length1 {} >>\nstream\n",
            TTF.len(),
            TTF.len()
        )
        .into_bytes();
        obj6.extend_from_slice(TTF);
        obj6.extend_from_slice(b"\nendstream");
        objects.push(obj6);

        let mut out = b"%PDF-1.4\n".to_vec();
        let mut offsets = Vec::new();
        for object in &objects {
            offsets.push(out.len());
            out.extend_from_slice(format!("{} 0 obj\n", offsets.len()).as_bytes());
            out.extend_from_slice(object);
            out.extend_from_slice(b"\nendobj\n");
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

    fn multipage_pdf(pages: usize) -> Vec<u8> {
        let kids = (0..pages)
            .map(|index| format!("{} 0 R", index.saturating_add(3)))
            .collect::<Vec<_>>()
            .join(" ");
        let mut objects = vec![
            "1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n".to_string(),
            format!("2 0 obj\n<< /Type /Pages /Kids [{kids}] /Count {pages} >>\nendobj\n"),
        ];
        for index in 0..pages {
            objects.push(format!(
                "{} 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] >>\nendobj\n",
                index.saturating_add(3)
            ));
        }
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
    fn chunk_id_is_optimize_and_the_chunk_is_available() {
        assert_eq!(CHUNK_ID, "optimize");
        assert_eq!(selis_optimize_available(), 1);
    }

    #[test]
    fn op_41_subset_fonts_returns_a_smaller_verified_pdf() {
        let pdf = embedded_font_pdf();
        let in_len = pdf.len();
        let response = run(&request(41, b"{}", &[("doc.pdf", &pdf)]));
        let (status, message, outputs) = parse_response(&response);
        assert_eq!(status, 0, "subset fonts failed: {message}");
        assert_eq!(message["verdict"], "ok");
        assert_eq!(outputs[0].0, "doc-subset.pdf");
        assert_eq!(&outputs[0].1[0..5], b"%PDF-");
        assert!(
            outputs[0].1.len() < in_len,
            "subset {} < {} bytes",
            outputs[0].1.len(),
            in_len
        );
    }

    #[test]
    fn op_43_linearize_returns_a_verified_fast_web_view_pdf() {
        let pdf = multipage_pdf(2);
        let response = run(&request(43, b"", &[("doc.pdf", &pdf)]));
        let (status, message, outputs) = parse_response(&response);
        assert_eq!(status, 0, "linearize failed: {message}");
        assert_eq!(message["verdict"], "ok");
        assert_eq!(message["pages_out"], 2);
        assert_eq!(outputs[0].0, "doc-linearized.pdf");
        assert_eq!(&outputs[0].1[0..5], b"%PDF-");
    }

    #[test]
    fn op_42_and_tools_ops_are_not_dispatched_here() {
        let pdf = multipage_pdf(1);
        for op in [1, 42] {
            let response = run(&request(op, b"", &[("doc.pdf", &pdf)]));
            assert!(
                error_text(&response).contains("unknown tool operation"),
                "op {op} must stay unknown to the optimise chunk"
            );
        }
    }

    #[test]
    fn a_bad_magic_and_an_empty_file_list_are_refused() {
        let pdf = multipage_pdf(1);
        let mut bytes = request(41, b"", &[("doc.pdf", &pdf)]);
        bytes[0] ^= 0xff;
        assert!(error_text(&run(&bytes)).contains("bad request magic"));
        assert!(error_text(&run(&request(43, b"", &[]))).contains("no input files"));
    }

    #[test]
    fn the_abi_moves_a_request_and_reports_the_chunk_id() {
        let pdf = multipage_pdf(1);
        let encoded = request(43, b"", &[("doc.pdf", &pdf)]);
        let mut out_len = 0u32;
        let response = unsafe {
            selis_optimize_run(
                encoded.as_ptr(),
                u32::try_from(encoded.len()).unwrap(),
                &mut out_len,
            )
        };
        assert!(!response.is_null());
        let bytes = unsafe { std::slice::from_raw_parts(response, out_len as usize) }.to_vec();
        assert_eq!(bytes[0..4], RESPONSE_MAGIC.to_le_bytes());
        unsafe { selis_optimize_free(response, out_len) };

        let mut id_len = 0u32;
        let id = unsafe { selis_optimize_chunk_id(&mut id_len) };
        let bytes = unsafe { std::slice::from_raw_parts(id, id_len as usize) }.to_vec();
        assert_eq!(bytes, b"optimize");
        unsafe { selis_optimize_free(id, id_len) };
    }
}
