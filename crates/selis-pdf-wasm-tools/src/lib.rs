//! The browser tools chunk.
//!
//! This is a separate `cdylib` from the viewer. The viewer never downloads
//! it; the tools page fetches it on first use. It exposes one binary
//! request/response entry point over the in-memory backend of
//! [`selis_pdf_tools`], so no document byte is ever passed to a filesystem,
//! a network API, or a third-party implementation.
//!
//! # Wire format
//!
//! All integers are little-endian. A request is:
//!
//! ```text
//! magic "SLTS" (u32)
//! version 1 (u32)
//! operation (u32)
//! options_json_len (u32), options_json bytes
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
//! The host allocates request and length buffers with [`selis_tools_alloc`],
//! calls [`selis_tools_run`], copies the response out, and frees both with
//! [`selis_tools_free`].

#![allow(unsafe_code)]

use std::alloc::{alloc, dealloc, Layout};
use std::panic::{catch_unwind, AssertUnwindSafe};

use selis_pdf_tools::{ToolError, Verification};

/// Chunk identifier used by the asset manifest.
pub const CHUNK_ID: &str = "tools";
/// Request magic, `SLTS`.
const REQUEST_MAGIC: u32 = 0x5354_4C53;
/// Response magic, `SLTR`.
const RESPONSE_MAGIC: u32 = 0x5254_4C53;
/// Protocol version.
const VERSION: u32 = 1;
/// Maximum number of inputs in one request.
const MAX_FILES: u32 = 64;
/// Maximum input name length.
const MAX_NAME_BYTES: u32 = 128;
/// Maximum options JSON length.
const MAX_OPTIONS_BYTES: u32 = 64 * 1024;

/// Operations understood by this chunk.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Op {
    Merge,
    Split,
    Rotate,
    Delete,
    Reorder,
    SetMetadata,
}

impl Op {
    fn from_wire(value: u32) -> Option<Self> {
        match value {
            1 => Some(Self::Merge),
            2 => Some(Self::Split),
            3 => Some(Self::Rotate),
            4 => Some(Self::Delete),
            5 => Some(Self::Reorder),
            6 => Some(Self::SetMetadata),
            _ => None,
        }
    }
}

/// Return the chunk id as guest memory. The host reads `out_len` and frees
/// the pointer with [`selis_tools_free`].
///
/// # Safety
///
/// `out_len` must point to four writable bytes in guest memory.
#[no_mangle]
pub unsafe extern "C" fn selis_tools_chunk_id(out_len: *mut u32) -> *mut u8 {
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

/// Bit 0 merge, 1 split, 2 rotate, 3 delete, 4 reorder, 5 metadata.
#[no_mangle]
pub extern "C" fn selis_tools_capabilities() -> u32 {
    0b11_1111
}

/// Allocate `len` bytes of guest memory. The host writes a request into it.
///
/// Returns null when the allocation fails. The caller must free the pointer
/// with [`selis_tools_free`] and the same length.
#[no_mangle]
pub extern "C" fn selis_tools_alloc(len: u32) -> *mut u8 {
    allocate(len as usize)
}

/// Free a buffer previously returned by [`selis_tools_alloc`],
/// [`selis_tools_chunk_id`], or [`selis_tools_run`].
///
/// # Safety
///
/// `ptr` must have been returned by one of those functions with exactly
/// `len` bytes, and must not have been freed already.
#[no_mangle]
pub unsafe extern "C" fn selis_tools_free(ptr: *mut u8, len: u32) {
    if ptr.is_null() || len == 0 {
        return;
    }
    let Ok(layout) = Layout::from_size_align(len as usize, 1) else {
        return;
    };
    // SAFETY: the caller's contract.
    unsafe { dealloc(ptr, layout) };
}

/// Run one tool operation.
///
/// The returned pointer is a response buffer; `out_len` receives its length.
/// The host must copy it out and free it with [`selis_tools_free`]. A null
/// return means the request could not be processed at all.
///
/// # Safety
///
/// `request_ptr` must point to `request_len` readable bytes; `out_len` must
/// point to four writable bytes.
#[no_mangle]
pub unsafe extern "C" fn selis_tools_run(
    request_ptr: *const u8,
    request_len: u32,
    out_len: *mut u32,
) -> *mut u8 {
    if out_len.is_null() {
        return std::ptr::null_mut();
    }
    if request_ptr.is_null() {
        return std::ptr::null_mut();
    }
    let Ok(request_len) = usize::try_from(request_len) else {
        return std::ptr::null_mut();
    };
    let request = unsafe { std::slice::from_raw_parts(request_ptr, request_len) };
    let response = catch_unwind(AssertUnwindSafe(|| run_request(request))).unwrap_or_else(|_| {
        let message = b"tool operation panicked";
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
        Ok((verification, name, bytes)) => {
            let message = serde_json::to_vec(&verification).unwrap_or_default();
            build_response(0, &message, &[(name, bytes)])
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

fn parse_and_run(request: &[u8]) -> Result<(Verification, String, Vec<u8>), ToolError> {
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
        // The host never controls the internal session key: a requested name
        // cannot collide with an earlier run or escape a table.
        let data = cursor
            .bytes(u32::MAX)
            .ok_or_else(|| ToolError("truncated input bytes".to_string()))?;
        selis_pdf_tools::insert_input(&format!("in/{index}"), data.to_vec());
    }
    if cursor.pos != request.len() {
        return Err(ToolError("trailing bytes in tool request".to_string()));
    }

    const OUTPUT: &str = "out.pdf";
    let verification = match op {
        Op::Merge => {
            let inputs: Vec<String> = (0..file_count).map(|i| format!("in/{i}")).collect();
            selis_pdf_tools::merge(&inputs, OUTPUT)?
        }
        Op::Split => {
            let first = usize::try_from(
                option_u64(&options, "from")
                    .ok_or_else(|| ToolError("split needs a from page".to_string()))?,
            )
            .map_err(|_| ToolError("split from is out of range".to_string()))?;
            let last = usize::try_from(
                option_u64(&options, "to")
                    .ok_or_else(|| ToolError("split needs a to page".to_string()))?,
            )
            .map_err(|_| ToolError("split to is out of range".to_string()))?;
            selis_pdf_tools::split("in/0", first, last, OUTPUT)?
        }
        Op::Rotate => {
            let angle = option_i64(&options, "angle")
                .ok_or_else(|| ToolError("rotate needs an angle".to_string()))?;
            let pages = option_str(&options, "pages");
            selis_pdf_tools::rotate("in/0", angle, pages.as_deref(), OUTPUT)?
        }
        Op::Delete => {
            let pages = option_str(&options, "pages")
                .ok_or_else(|| ToolError("delete needs a page selection".to_string()))?;
            selis_pdf_tools::delete("in/0", &pages, OUTPUT)?
        }
        Op::Reorder => {
            let order = option_str(&options, "order")
                .ok_or_else(|| ToolError("reorder needs a page order".to_string()))?;
            selis_pdf_tools::reorder("in/0", &order, OUTPUT)?
        }
        Op::SetMetadata => {
            let fields = option_fields(&options)?;
            let refs: Vec<(&str, &str)> = fields
                .iter()
                .map(|(key, value)| (key.as_str(), value.as_str()))
                .collect();
            selis_pdf_tools::set_metadata("in/0", &refs, OUTPUT)?
        }
    };

    let bytes = selis_pdf_tools::take_output(OUTPUT)
        .ok_or_else(|| ToolError("operation produced no output".to_string()))?;
    let stem = {
        let stem = file_stem(&first_name);
        if stem.is_empty() {
            "document".to_string()
        } else {
            stem
        }
    };
    let output_name = match op {
        Op::Merge => "merged.pdf".to_string(),
        Op::Split => format!("{stem}-pages.pdf"),
        Op::Rotate => format!("{stem}-rotated.pdf"),
        Op::Delete => format!("{stem}-trimmed.pdf"),
        Op::Reorder => format!("{stem}-reordered.pdf"),
        Op::SetMetadata => format!("{stem}-metadata.pdf"),
    };
    Ok((verification, output_name, bytes))
}

fn file_stem(name: &str) -> String {
    let base = name.rsplit(['/', '\\']).next().unwrap_or(name);
    match base.rsplit_once('.') {
        Some((stem, _)) => stem.to_string(),
        None => base.to_string(),
    }
}

fn option_u64(options: &serde_json::Value, key: &str) -> Option<u64> {
    options.get(key)?.as_u64()
}

fn option_i64(options: &serde_json::Value, key: &str) -> Option<i64> {
    options.get(key)?.as_i64()
}

fn option_str(options: &serde_json::Value, key: &str) -> Option<String> {
    options.get(key)?.as_str().map(ToString::to_string)
}

fn option_fields(options: &serde_json::Value) -> Result<Vec<(String, String)>, ToolError> {
    let object = options
        .get("fields")
        .and_then(|value| value.as_object())
        .ok_or_else(|| ToolError("metadata needs a fields object".to_string()))?;
    let mut fields = Vec::with_capacity(object.len());
    for (key, value) in object {
        let value = value
            .as_str()
            .ok_or_else(|| ToolError("metadata field values must be strings".to_string()))?;
        fields.push((key.clone(), value.to_string()));
    }
    Ok(fields)
}

#[cfg(test)]
mod tests {
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

    #[test]
    fn capabilities_cover_the_six_operations() {
        assert_eq!(selis_tools_capabilities(), 0b11_1111);
    }

    #[test]
    fn rejects_bad_magic_without_panicking() {
        let mut bytes = request(1, b"", &[]);
        bytes[0] ^= 0xFF;
        let response = run_request(&bytes);
        assert_eq!(response[0..4], RESPONSE_MAGIC.to_le_bytes());
        assert_eq!(response[4..8], 1u32.to_le_bytes());
    }

    #[test]
    fn merge_of_two_minimal_documents_succeeds() {
        let one = minimal_pdf();
        let request = request(1, b"", &[("a.pdf", &one), ("b.pdf", &one)]);
        let response = run_request(&request);
        assert_eq!(response[4..8], 0u32.to_le_bytes(), "merge should pass");
        let message = &response[8..];
        assert!(!message.is_empty());
    }

    fn minimal_pdf() -> Vec<u8> {
        let objects: [&[u8]; 4] = [
            b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n",
            b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n",
            b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] >>\nendobj\n",
            b"4 0 obj\n<< /Length 0 >>\nstream\n\nendstream\nendobj\n",
        ];
        let mut out = b"%PDF-1.4\n".to_vec();
        let mut offsets = Vec::new();
        for object in objects {
            offsets.push(out.len());
            out.extend_from_slice(object);
        }
        let xref = out.len();
        let size = offsets.len() + 1;
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
