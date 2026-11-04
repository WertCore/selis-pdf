//! The browser signing chunk.
//!
//! This is a separate `cdylib` from the viewer, the plain tools chunk and the
//! security chunk. The tools page never downloads it until someone opens a
//! signing or certificate card, and it is fetched then and cached by the
//! service worker. Like the chunks before it it speaks a pointer ABI and
//! **declares no imports**: the page instantiates it with an empty import
//! object, which is the mechanical form of "this module cannot call out".
//! Document bytes arrive as request payloads and the answer leaves as a guest
//! allocation; nothing touches a network and nothing touches a filesystem —
//! every path in a request is a key into the in-memory tool session.
//!
//! What it runs is [`selis_pdf_tools::sign`] — the same operations the
//! `selis stamp`, `selis sign` and `selis signatures` commands run, byte for
//! byte, including the append-only writer, the detached CMS signer, the
//! signature report and the structural verification gate.
//!
//! # Wire format
//!
//! All integers are little-endian. A request is:
//!
//! ```text
//! magic "SLSG" (u32)
//! version 1 (u32)
//! operation (u32: 1 stamp, 2 sign, 3 report)
//! options_json_len (u32), options_json bytes
//! file_count (u32)          — exactly 1
//!   name_len (u32), name bytes
//!   data_len (u32), data bytes
//! ```
//!
//! A response is:
//!
//! ```text
//! magic "SLRG" (u32)
//! status (u32: 0 ok, 1 error)
//! message_len (u32), message bytes (verification/report JSON, or error text)
//! output_count (u32)
//!   name_len (u32), name bytes
//!   data_len (u32), data bytes
//! ```
//!
//! The report operation produces no output; `output_count` is 0 and the
//! message is the report itself. Stamp and sign produce exactly one PDF and
//! the message carries the verification object plus the operation's own
//! report (the page it stamped, the field it signed, the digest).
//!
//! # Where the private key comes from
//!
//! The page reads the user's key file **in the tab** and puts the PEM text in
//! the options JSON. This module never invents a key, never stores one, and
//! never echoes one back — not in a verification object, not in an error
//! message. The request buffer holds the key while the call runs; the page
//! zeroes that buffer as soon as the call returns.
//!
//! # What is not claimed
//!
//! Signing proves the document has not changed since the signature was made.
//! It does not claim the signer is trusted: trust is the report's chain
//! verdict, which ends at anchors the caller supplied and always records
//! `revocation_checked: false` because this build has no network to check a
//! revocation list.

#![allow(unsafe_code)]

use std::alloc::{alloc, dealloc, Layout};
use std::panic::{catch_unwind, AssertUnwindSafe};

use selis_pdf_tools::sign::{self, DigitalOptions, VisualOptions};
use selis_pdf_tools::ToolError;

/// Chunk identifier used by the asset manifest.
pub const CHUNK_ID: &str = "sign";
/// Request magic, `SLSG`.
const REQUEST_MAGIC: u32 = 0x4753_4C53;
/// Response magic, `SLRG`.
const RESPONSE_MAGIC: u32 = 0x4752_4C53;
/// Protocol version.
const VERSION: u32 = 1;
/// A signing operation takes exactly one document.
const REQUIRED_FILES: u32 = 1;
/// Maximum input name length.
const MAX_NAME_BYTES: u32 = 128;
/// Maximum options JSON length. Larger than the other chunks because the
/// options carry a PEM private key and its certificate chain.
const MAX_OPTIONS_BYTES: u32 = 256 * 1024;
/// The session key the input document is held under. Chosen here, never by
/// the caller, so a requested name cannot collide with an earlier run.
const SESSION_KEY: &str = "in/0";

/// Operations understood by this chunk.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Op {
    Stamp,
    Sign,
    Report,
}

impl Op {
    fn from_wire(value: u32) -> Option<Self> {
        match value {
            1 => Some(Self::Stamp),
            2 => Some(Self::Sign),
            3 => Some(Self::Report),
            _ => None,
        }
    }

    /// The name the refusal messages use when an operation is mis-called.
    fn name(self) -> &'static str {
        match self {
            Self::Stamp => "stamp",
            Self::Sign => "sign",
            Self::Report => "signature report",
        }
    }
}

/// Return the chunk id as guest memory. The host reads `out_len` and frees the
/// pointer with [`selis_sign_free`].
///
/// # Safety
///
/// `out_len` must point to four writable bytes in guest memory.
#[no_mangle]
pub unsafe extern "C" fn selis_sign_chunk_id(out_len: *mut u32) -> *mut u8 {
    if out_len.is_null() {
        return std::ptr::null_mut();
    }
    let bytes = CHUNK_ID.as_bytes();
    let Some(len) = u32::try_from(bytes.len()).ok() else {
        return std::ptr::null_mut();
    };
    let ptr = allocate(len as usize);
    if ptr.is_null() {
        return ptr;
    }
    std::ptr::copy_nonoverlapping(bytes.as_ptr(), ptr, bytes.len());
    // SAFETY: the caller promised four writable bytes.
    unsafe { out_len.write(len) };
    ptr
}

/// Bit 0 stamp, bit 1 certificate sign, bit 2 signature report.
#[no_mangle]
pub extern "C" fn selis_sign_capabilities() -> u32 {
    0b111
}

/// Allocate `len` bytes of guest memory. The host writes a request into it.
///
/// Returns null when the allocation fails. The caller must free the pointer
/// with [`selis_sign_free`] and the same length.
#[no_mangle]
pub extern "C" fn selis_sign_alloc(len: u32) -> *mut u8 {
    allocate(len as usize)
}

/// Free a buffer previously returned by [`selis_sign_alloc`],
/// [`selis_sign_chunk_id`], or [`selis_sign_run`].
///
/// # Safety
///
/// `ptr` must have been returned by one of those functions with exactly `len`
/// bytes, and must not have been freed already.
#[no_mangle]
pub unsafe extern "C" fn selis_sign_free(ptr: *mut u8, len: u32) {
    if ptr.is_null() || len == 0 {
        return;
    }
    let Ok(layout) = Layout::from_size_align(len as usize, 1) else {
        return;
    };
    // SAFETY: the caller's contract.
    unsafe { dealloc(ptr, layout) };
}

/// Run one signing operation.
///
/// The returned pointer is a response buffer; `out_len` receives its length.
/// The host must copy it out and free it with [`selis_sign_free`]. A null
/// return means the request could not be processed at all.
///
/// # Safety
///
/// `request_ptr` must point to `request_len` readable bytes; `out_len` must
/// point to four writable bytes.
#[no_mangle]
pub unsafe extern "C" fn selis_sign_run(
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
    // bytes. The guest is single-threaded, and the slice does not outlive the
    // call.
    let request = unsafe { std::slice::from_raw_parts(request_ptr, request_len) };
    let response = catch_unwind(AssertUnwindSafe(|| run_request(request))).unwrap_or_else(|_| {
        let message = b"signing operation panicked";
        build_response(1, message, &[])
    });
    let Ok(len) = u32::try_from(response.len()) else {
        return std::ptr::null_mut();
    };
    let ptr = allocate(response.len());
    if ptr.is_null() {
        return ptr;
    }
    std::ptr::copy_nonoverlapping(response.as_ptr(), ptr, response.len());
    // SAFETY: `out_len` was checked non-null above, and the caller promised it
    // points to four writable bytes.
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

fn run_request(request: &[u8]) -> Vec<u8> {
    // The session table is process-wide, and `clear()` at the edges means two
    // concurrent requests would erase each other's inputs. The page runs one
    // request at a time; the lock makes the native test binary obey that too.
    // Recover from a poisoned lock: a caught panic must not poison the next
    // browser operation.
    static REQUEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _serial = REQUEST_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    // A workspace test unifies the CLI's `native-fs` feature into this crate.
    // The guard keeps this request on the in-memory backend regardless, so the
    // browser protocol is tested against the browser I/O boundary.
    let _memory = selis_pdf_tools::MemorySession::activate();
    // A failed operation must never leave a previous result reachable.
    selis_pdf_tools::clear();
    let result = parse_and_run(request);
    selis_pdf_tools::clear();
    match result {
        Ok((message, output)) => {
            let message = serde_json::to_vec(&message).unwrap_or_default();
            match output {
                Some((name, bytes)) => build_response(0, &message, &[(name, bytes)]),
                None => build_response(0, &message, &[]),
            }
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

/// One accepted request: the operation, its options, and the document's
/// user-visible name (the output is named after it).
struct Call<'a> {
    op: Op,
    options: &'a serde_json::Value,
    stem: String,
}

fn parse_and_run(
    request: &[u8],
) -> Result<(serde_json::Value, Option<(String, Vec<u8>)>), ToolError> {
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
        return Err(ToolError(
            "unsupported signing protocol version".to_string(),
        ));
    }
    let op = Op::from_wire(op).ok_or_else(|| ToolError("unknown signing operation".to_string()))?;

    let options = cursor
        .bytes(MAX_OPTIONS_BYTES)
        .ok_or_else(|| ToolError("truncated options".to_string()))?;
    let options: serde_json::Value = if options.is_empty() {
        serde_json::Value::Null
    } else {
        serde_json::from_slice(options)
            .map_err(|e| ToolError(format!("invalid options JSON: {e}")))?
    };

    let file_count = cursor
        .u32()
        .ok_or_else(|| ToolError("truncated file count".to_string()))?;
    if file_count != REQUIRED_FILES {
        return Err(ToolError(format!(
            "{} needs exactly one document (got {file_count})",
            op.name()
        )));
    }
    let name = cursor
        .bytes(MAX_NAME_BYTES)
        .ok_or_else(|| ToolError("truncated input name".to_string()))?;
    let name = std::str::from_utf8(name)
        .map_err(|_| ToolError("input name is not UTF-8".to_string()))?
        .to_string();
    let data = cursor
        .bytes(u32::MAX)
        .ok_or_else(|| ToolError("truncated input bytes".to_string()))?;
    if cursor.pos != request.len() {
        return Err(ToolError("trailing bytes in signing request".to_string()));
    }
    // The host never controls the internal session key: a requested name cannot
    // collide with an earlier run or escape a table.
    selis_pdf_tools::insert_input(SESSION_KEY, data.to_vec());
    let stem = file_stem(&name);

    let call = Call {
        op,
        options: &options,
        stem,
    };
    // The engine reports failures against the path it was handed, which here is
    // the session key. A reader cannot act on `in/0`; the document's own name is
    // what the CLI prints, so the two surfaces say the same sentence.
    run_operation(&call).map_err(|error| ToolError(error.0.replace(SESSION_KEY, &name)))
}

/// Run one operation and package its JSON with any output bytes.
fn run_operation(
    call: &Call<'_>,
) -> Result<(serde_json::Value, Option<(String, Vec<u8>)>), ToolError> {
    match call.op {
        Op::Stamp => {
            let options: VisualOptions = serde_json::from_value(call.options.clone())
                .map_err(|e| ToolError(format!("invalid stamp options: {e}")))?;
            let verification = sign::stamp_pdf(SESSION_KEY, &options, "out.pdf")?;
            let mut message = to_message(&verification)?;
            if let Some(object) = message.as_object_mut() {
                object.insert("operation".to_string(), serde_json::json!(call.op.name()));
                object.insert("page".to_string(), serde_json::json!(options.page));
                object.insert("lines".to_string(), serde_json::json!(options.lines.len()));
                object.insert(
                    "strokes".to_string(),
                    serde_json::json!(options.strokes.len()),
                );
            }
            let bytes = take_output()?;
            let name = format!("{}-stamped.pdf", call.stem);
            attach_output(&mut message, &name, &bytes);
            Ok((message, Some((name, bytes))))
        }
        Op::Sign => {
            let options: DigitalOptions = serde_json::from_value(call.options.clone())
                .map_err(|e| ToolError(format!("invalid signing options: {e}")))?;
            let verification = sign::sign_pdf(SESSION_KEY, &options, "out.pdf")?;
            let mut message = to_message(&verification)?;
            if let Some(object) = message.as_object_mut() {
                object.insert("operation".to_string(), serde_json::json!(call.op.name()));
                object.insert("field".to_string(), serde_json::json!(options.field_name));
                object.insert("page".to_string(), serde_json::json!(options.page));
                object.insert(
                    "subFilter".to_string(),
                    serde_json::json!("adbe.pkcs7.detached"),
                );
            }
            let bytes = take_output()?;
            let name = format!("{}-signed.pdf", call.stem);
            attach_output(&mut message, &name, &bytes);
            Ok((message, Some((name, bytes))))
        }
        Op::Report => {
            let anchors = anchors(call.options)?;
            let now = call
                .options
                .get("now")
                .and_then(serde_json::Value::as_i64)
                .ok_or_else(|| {
                    ToolError(
                        "the signature report needs `now`: Unix seconds from the page".to_string(),
                    )
                })?;
            let report = sign::signature_report(SESSION_KEY, &anchors, now)?;
            let mut message = serde_json::to_value(&report)
                .map_err(|e| ToolError(format!("could not serialize the report: {e}")))?;
            if let Some(object) = message.as_object_mut() {
                object.insert("operation".to_string(), serde_json::json!(call.op.name()));
                object.insert("anchorCount".to_string(), serde_json::json!(anchors.len()));
            }
            Ok((message, None))
        }
    }
}

/// The verification object as the response's base JSON.
fn to_message(
    verification: &selis_pdf_tools::Verification,
) -> Result<serde_json::Value, ToolError> {
    serde_json::to_value(verification)
        .map_err(|e| ToolError(format!("could not serialize the verification: {e}")))
}

/// Add the one output record every file-producing operation reports.
fn attach_output(message: &mut serde_json::Value, name: &str, bytes: &[u8]) {
    let pages = message
        .get("pages_out")
        .cloned()
        .unwrap_or(serde_json::Value::Null);
    let verdict = message
        .get("verdict")
        .cloned()
        .unwrap_or(serde_json::Value::Null);
    if let Some(object) = message.as_object_mut() {
        object.insert(
            "outputs".to_string(),
            serde_json::json!([{
                "name": name,
                "kind": "pdf",
                "pages": pages,
                "bytes": bytes.len(),
                "verdict": verdict,
            }]),
        );
    }
}

fn take_output() -> Result<Vec<u8>, ToolError> {
    selis_pdf_tools::take_output("out.pdf")
        .ok_or_else(|| ToolError("the operation produced no output".to_string()))
}

/// The trust anchors the page handed over, as DER certificates.
///
/// Each entry is PEM (`-----BEGIN CERTIFICATE-----`) or bare base64; an entry
/// that is neither is refused rather than silently skipped, because dropping
/// an anchor would turn "trusted" into "untrusted" without saying so.
fn anchors(options: &serde_json::Value) -> Result<Vec<Vec<u8>>, ToolError> {
    let Some(items) = options.get("anchors").and_then(|value| value.as_array()) else {
        return Ok(Vec::new());
    };
    let mut out = Vec::new();
    for (index, item) in items.iter().enumerate() {
        let text = item
            .as_str()
            .ok_or_else(|| ToolError(format!("anchor {index} is not a string")))?;
        let der = selis_crypto::cert::pem_body(text, "CERTIFICATE")
            .or_else(|| selis_crypto::cert::base64_decode(text))
            .ok_or_else(|| {
                ToolError(format!(
                    "anchor {index} is not a PEM or base64 X.509 certificate"
                ))
            })?;
        let budget = selis_sandbox::Budget::profile(selis_sandbox::Surface::Fuzz);
        let mut g = budget.guard();
        selis_crypto::cert::parse(&der, &mut g).map_err(|_| {
            ToolError(format!(
                "anchor {index} is not a certificate this build can read"
            ))
        })?;
        out.push(der);
    }
    Ok(out)
}

/// The download name's stem: the user's file name without its extension.
fn file_stem(name: &str) -> String {
    let base = name.rsplit(['/', '\\']).next().unwrap_or(name);
    match base.rfind('.') {
        Some(index) if index > 0 => base.get(..index).unwrap_or(base).to_string(),
        _ => base.to_string(),
    }
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::arithmetic_side_effects,
        clippy::expect_used,
        clippy::indexing_slicing,
        clippy::panic,
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

    fn read_u32(bytes: &[u8], pos: &mut usize) -> u32 {
        let end = pos.saturating_add(4);
        let value = u32::from_le_bytes(bytes[*pos..end].try_into().expect("u32"));
        *pos = end;
        value
    }

    /// `(status, message JSON or error text, outputs)`.
    fn parse_response(bytes: &[u8]) -> (u32, serde_json::Value, Vec<(String, Vec<u8>)>) {
        assert_eq!(bytes[0..4], RESPONSE_MAGIC.to_le_bytes());
        let status = u32::from_le_bytes(bytes[4..8].try_into().expect("status"));
        let mut pos = 8usize;
        let message_len = usize::try_from(read_u32(bytes, &mut pos)).expect("message len");
        let message_end = pos.saturating_add(message_len);
        let message_text = String::from_utf8_lossy(&bytes[pos..message_end]).to_string();
        pos = message_end;
        let message: serde_json::Value = if status == 0 {
            serde_json::from_str(&message_text).expect("message JSON")
        } else {
            serde_json::Value::String(message_text)
        };
        let count = usize::try_from(read_u32(bytes, &mut pos)).expect("output count");
        let mut outputs = Vec::new();
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
        let (status, message, _) = parse_response(response);
        assert_eq!(status, 1, "expected an error response");
        message.as_str().unwrap_or_default().to_string()
    }

    /// A plain (unencrypted) `pages`-page document.
    fn plain_pdf(pages: usize) -> Vec<u8> {
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

    const STAMP: &[u8] = br#"{
        "page": 1,
        "rect": [10.0, 10.0, 90.0, 40.0],
        "lines": [{"text": "Approved", "size": 11.0}],
        "strokes": [],
        "color": [0.0, 0.0, 0.0],
        "strokeWidth": 1.0,
        "fontSize": 11.0,
        "border": true
    }"#;

    #[test]
    fn capabilities_cover_every_operation() {
        // Ops 1-3: stamp, certificate sign, signature report.
        assert_eq!(selis_sign_capabilities(), 0b111);
    }

    #[test]
    fn request_validation_rejects_a_bad_envelope() {
        let pdf = plain_pdf(1);
        let response = run_request(&request(9, b"", &[("doc.pdf", &pdf)]));
        assert!(error_text(&response).contains("unknown signing operation"));

        let mut bad = request(1, b"", &[("doc.pdf", &pdf)]);
        bad[0..4].copy_from_slice(b"XXXX");
        assert!(error_text(&run_request(&bad)).contains("bad request magic"));

        let response = run_request(&request(1, b"", &[("a.pdf", &pdf), ("b.pdf", &pdf)]));
        assert!(error_text(&response).contains("exactly one document"));
    }

    #[test]
    fn stamp_writes_one_verified_pdf_over_the_wire() {
        let pdf = plain_pdf(2);
        let response = run_request(&request(1, STAMP, &[("my report.pdf", &pdf)]));
        let (status, message, outputs) = parse_response(&response);
        assert_eq!(status, 0, "stamp failed: {message}");
        assert_eq!(message["operation"], "stamp");
        assert_eq!(message["pages_out"], 2);
        assert_eq!(message["page"], 1);
        assert_eq!(message["lines"], 1);
        assert_eq!(message["outputs"][0]["kind"], "pdf");
        assert_eq!(outputs[0].0, "my report-stamped.pdf");
        let text = String::from_utf8_lossy(&outputs[0].1);
        assert!(text.contains("/Approved") || text.contains("(Approved) Tj"));
        assert!(text.contains("%%EOF"));
        assert!(
            text.find("%%EOF").unwrap() < text.rfind("%%EOF").unwrap(),
            "the stamp is an appended revision, not a rewrite"
        );
    }

    #[test]
    fn the_report_has_no_output_and_needs_a_clock() {
        let pdf = plain_pdf(1);
        let response = run_request(&request(3, b"{}", &[("doc.pdf", &pdf)]));
        assert!(error_text(&response).contains("now"));

        let response = run_request(&request(3, br#"{"now":1793793600}"#, &[("doc.pdf", &pdf)]));
        let (status, message, outputs) = parse_response(&response);
        assert_eq!(status, 0, "report failed: {message}");
        assert_eq!(message["operation"], "signature report");
        assert_eq!(message["signatures"], serde_json::json!([]));
        assert_eq!(message["revocationChecked"], false);
        assert!(outputs.is_empty(), "a report writes nothing");
    }

    #[test]
    fn a_bad_anchor_is_refused_not_dropped() {
        let pdf = plain_pdf(1);
        let response = run_request(&request(
            3,
            br#"{"now":1793793600,"anchors":["not a certificate"]}"#,
            &[("doc.pdf", &pdf)],
        ));
        assert!(error_text(&response).contains("anchor 0"));
    }

    #[test]
    fn an_invalid_stamp_option_is_named() {
        let pdf = plain_pdf(1);
        let response = run_request(&request(1, br#"{"page":1}"#, &[("doc.pdf", &pdf)]));
        assert!(error_text(&response).contains("invalid stamp options"));
    }
}
