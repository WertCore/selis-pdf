//! The browser's first-class content-editing chunk (SL-5.TEXTEDIT.05/06/07/08).
//!
//! A separate `cdylib` from the all-purpose tools chunk. The add-content and
//! object-edit engines link font matching and image decoding; keeping them in
//! their own on-demand module keeps the tools chunk at its published size and
//! means a reader who never adds content never downloads this code. Like the
//! tools chunk it has **no imports** (ADR-P0011): no wasm-bindgen, no network,
//! no filesystem. Document bytes arrive as an attachment and stay on device.
//!
//! # Wire format (identical to the tools chunk)
//!
//! Request: magic `SLTS`, version 1, op (u32), options JSON, file count, then
//! each `{name, bytes}`. The first file is the PDF; `add_image` and
//! `replace_image` take the image as the second file, never as base64 in JSON.
//! Response: magic `SLTR`, status, message (verification JSON on success),
//! output count, then each `{name, bytes}`.

#![allow(unsafe_code)]

use std::alloc::{alloc, dealloc, Layout};
use std::panic::{catch_unwind, AssertUnwindSafe};

use selis_pdf_tools::{clear, insert_input, take_output, MemorySession, Verification};
use serde_json::Value;

/// Chunk identifier used by the asset manifest and the host loader.
pub const CHUNK_ID: &str = "content";
const REQUEST_MAGIC: u32 = 0x5354_4C53;
const RESPONSE_MAGIC: u32 = 0x5254_4C53;
const VERSION: u32 = 1;
const MAX_FILES: u32 = 64;
const MAX_NAME_BYTES: u32 = 128;
const MAX_OPTIONS_BYTES: u32 = 64 * 1024;

/// The ops this chunk serves. The numbers match the tools protocol's 48..=62
/// range so the host's typed `ToolOp`/op-code table is shared.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Op {
    AddTextBox,
    AddImage,
    AddShape,
    AddLink,
    AddHeaderFooter,
    AddWatermark,
    AddBates,
    AddBackground,
    ListImageObjects,
    ListVectorObjects,
    MoveObject,
    ResizeObject,
    DeleteObject,
    ReplaceImage,
    ExtractImage,
}

impl Op {
    fn from_wire(value: u32) -> Option<Self> {
        match value {
            48 => Some(Self::AddTextBox),
            49 => Some(Self::AddImage),
            50 => Some(Self::AddShape),
            51 => Some(Self::AddLink),
            52 => Some(Self::AddHeaderFooter),
            53 => Some(Self::AddWatermark),
            54 => Some(Self::AddBates),
            55 => Some(Self::AddBackground),
            56 => Some(Self::ListImageObjects),
            57 => Some(Self::ListVectorObjects),
            58 => Some(Self::MoveObject),
            59 => Some(Self::ResizeObject),
            60 => Some(Self::DeleteObject),
            61 => Some(Self::ReplaceImage),
            62 => Some(Self::ExtractImage),
            _ => None,
        }
    }

    /// The bridge op name and the short output-file label.
    fn bridge(self) -> (&'static str, &'static str) {
        match self {
            Self::AddTextBox => ("add_text_box", "text-box"),
            Self::AddImage => ("add_image", "image"),
            Self::AddShape => ("add_shape", "shape"),
            Self::AddLink => ("add_link", "link"),
            Self::AddHeaderFooter => ("add_header_footer", "header-footer"),
            Self::AddWatermark => ("add_watermark", "watermark"),
            Self::AddBates => ("add_bates_numbering", "bates"),
            Self::AddBackground => ("add_background", "background"),
            Self::ListImageObjects => ("list_image_objects", "images"),
            Self::ListVectorObjects => ("list_vector_objects", "vectors"),
            Self::MoveObject => ("move_object", "moved"),
            Self::ResizeObject => ("resize_object", "resized"),
            Self::DeleteObject => ("delete_object", "object-deleted"),
            Self::ReplaceImage => ("replace_image", "image-replaced"),
            Self::ExtractImage => ("extract_image", "extracted-image"),
        }
    }

    fn needs_image(self) -> bool {
        matches!(self, Self::AddImage | Self::ReplaceImage)
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

/// Build a `SLTR` response. `message` is verification JSON on success and a
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
    // The session table is process-wide; one request at a time, as in the tools
    // chunk. Recover from a poisoned lock so a caught panic cannot poison the
    // next browser operation.
    static REQUEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _serial = REQUEST_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let _memory = MemorySession::activate();
    clear();
    let result = parse_and_run(request);
    clear();
    match result {
        Ok((verification, name, bytes)) => {
            let mut message =
                serde_json::to_value(&verification).unwrap_or(serde_json::Value::Null);
            let kind = if name.ends_with(".json") {
                "report"
            } else if name.ends_with(".png") || name.ends_with(".jpg") {
                "file"
            } else {
                "pdf"
            };
            let pages = if kind == "pdf" {
                verification.pages_out
            } else {
                0
            };
            if let Some(object) = message.as_object_mut() {
                object.insert(
                    "outputs".to_string(),
                    serde_json::json!([{
                        "name": name,
                        "kind": kind,
                        "pages": pages,
                        "bytes": bytes.len(),
                        "verdict": verification.verdict,
                        "overTarget": false,
                    }]),
                );
            }
            let message = serde_json::to_vec(&message).unwrap_or_default();
            build_response(0, &message, &[(name, bytes)])
        }
        Err(error) => build_response(1, error.to_string().as_bytes(), &[]),
    }
}

fn parse_and_run(request: &[u8]) -> Result<(Verification, String, Vec<u8>), String> {
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
    let op = Op::from_wire(op_code).ok_or("unknown content operation")?;
    let options_bytes = cursor.bytes(MAX_OPTIONS_BYTES).ok_or("truncated options")?;
    let options: Value = if options_bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(options_bytes).map_err(|e| format!("invalid options JSON: {e}"))?
    };
    let file_count = cursor.u32().ok_or("truncated file count")?;
    if file_count == 0 || file_count > MAX_FILES {
        return Err("expected one PDF and at most one image".to_string());
    }
    let mut files: Vec<Vec<u8>> = Vec::new();
    for index in 0..file_count {
        let _ = index;
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
    let pdf = files.first().ok_or("no input document")?;
    insert_input("in/0", pdf.clone());
    let image = if op.needs_image() {
        Some(
            files
                .get(1)
                .ok_or("the op needs an image as the second input")?
                .as_slice(),
        )
    } else {
        None
    };
    let (op_name, label) = op.bridge();
    let result = selis_pdf_tools::content_edit::run(op_name, &options, "in/0", "out.bin", image)
        .map_err(|e| e.to_string())?;
    let bytes = take_output("out.bin").ok_or("operation produced no output")?;
    let name = match result.kind {
        selis_pdf_tools::content_edit::ArtifactKind::Pdf => {
            format!("{stem}-{label}.pdf", stem = stem_of())
        }
        selis_pdf_tools::content_edit::ArtifactKind::Report => {
            format!("{stem}-{label}.json", stem = stem_of())
        }
        selis_pdf_tools::content_edit::ArtifactKind::File => {
            format!("{stem}-{label}.png", stem = stem_of())
        }
    };
    Ok((result.verification, name, bytes))
}

fn stem_of() -> &'static str {
    "document"
}

/// Return the chunk id as guest memory, freed with `selis_content_free`.
///
/// # Safety
/// `out_len` must point to four writable bytes.
#[no_mangle]
pub unsafe extern "C" fn selis_content_chunk_id(out_len: *mut u32) -> *mut u8 {
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
pub extern "C" fn selis_content_alloc(len: u32) -> *mut u8 {
    allocate(len as usize)
}

/// # Safety
/// `ptr`/`len` must identify one live allocation from this chunk, freed once.
#[no_mangle]
pub unsafe extern "C" fn selis_content_free(ptr: *mut u8, len: u32) {
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

/// Run one op. The response pointer is freed with `selis_content_free`.
///
/// # Safety
/// `request_ptr` must be readable for `request_len` bytes; `out_len` writable.
#[no_mangle]
pub unsafe extern "C" fn selis_content_run(
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
        .unwrap_or_else(|_| build_response(1, b"content operation panicked", &[]));
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

/// How many ops this chunk implements (the fifteen codes 48..=62).
#[no_mangle]
pub extern "C" fn selis_content_capabilities() -> u32 {
    15
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

    fn simple_pdf() -> Vec<u8> {
        let stream = "BT /F1 12 Tf 20 160 Td (Invoice total: 900 USD) Tj ET";
        let objects = vec![
            "1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n".to_string(),
            "2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n".to_string(),
            "3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Contents 4 0 R >>\nendobj\n".to_string(),
            format!(
                "4 0 obj\n<< /Length {} >>\nstream\n{stream}\nendstream\nendobj\n",
                stream.len()
            ),
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
    fn chunk_id_is_content() {
        assert_eq!(CHUNK_ID, "content");
    }

    #[test]
    fn add_text_box_is_dispatched_and_preserves_the_prefix() {
        let pdf = simple_pdf();
        let (status, message, outputs) = parse(&run_request(&request(
            48,
            br#"{"page":0,"rect":[10,10,120,40],"text":"First class","font":"Helvetica","font_size":11,"color":[0,0,0]}"#,
            &[("doc.pdf", &pdf)],
        )));
        assert_eq!(status, 0, "add_text_box failed: {message}");
        assert_eq!(outputs[0].0, "document-text-box.pdf");
        assert!(outputs[0].1.starts_with(&pdf));
        assert!(String::from_utf8_lossy(&outputs[0].1).contains("First class"));
        assert_eq!(message["outputs"][0]["kind"], "pdf");
    }

    #[test]
    fn furniture_ops_are_dispatched() {
        let pdf = simple_pdf();
        for (op, options, suffix) in [
            (
                54_u32,
                r#"{"pattern":"CASE-000001","start":7,"font":"Helvetica","font_size":9,"color":[0,0,0]}"#,
                "bates",
            ),
            (
                55_u32,
                r#"{"page":0,"rect":[0,0,200,200],"color":[245,245,240]}"#,
                "background",
            ),
        ] {
            let (status, message, outputs) = parse(&run_request(&request(
                op,
                options.as_bytes(),
                &[("doc.pdf", &pdf)],
            )));
            assert_eq!(status, 0, "{suffix} failed: {message}");
            assert_eq!(outputs[0].0, format!("document-{suffix}.pdf"));
            assert!(outputs[0].1.starts_with(&pdf));
        }
    }

    #[test]
    fn object_ops_are_dispatched() {
        // A page whose content paints a vector path and draws an image.
        let content = b"q 100 0 0 80 20 30 cm /Im0 Do Q 0 1 0 rg 5 5 40 20 re f\n";
        let objects = vec![
            "1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n".to_string(),
            "2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n".to_string(),
            "3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Resources << /XObject << /Im0 5 0 R >> >> /Contents 4 0 R >>\nendobj\n".to_string(),
            format!("4 0 obj\n<< /Length {} >>\nstream\n{}\nendstream\nendobj\n", content.len(), String::from_utf8_lossy(content)),
            "5 0 obj\n<< /Type /XObject /Subtype /Image /Width 2 /Height 2 /ColorSpace /DeviceRGB /BitsPerComponent 8 /Length 12 >>\nstream\n\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\nendstream\nendobj\n".to_string(),
        ];
        let mut pdf = b"%PDF-1.4\n".to_vec();
        let mut offsets = Vec::new();
        for object in &objects {
            offsets.push(pdf.len());
            pdf.extend_from_slice(object.as_bytes());
        }
        let xref = pdf.len();
        let size = offsets.len().saturating_add(1);
        pdf.extend_from_slice(format!("xref\n0 {size}\n").as_bytes());
        pdf.extend_from_slice(b"0000000000 65535 f \n");
        for offset in &offsets {
            pdf.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
        }
        pdf.extend_from_slice(
            format!("trailer\n<< /Size {size} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n")
                .as_bytes(),
        );

        let (status, message, outputs) = parse(&run_request(&request(
            56,
            br#"{"page":0}"#,
            &[("doc.pdf", &pdf)],
        )));
        assert_eq!(status, 0, "list_objects failed: {message}");
        let report: Value = serde_json::from_slice(&outputs[0].1).expect("report JSON");
        let objects = report["objects"].as_array().expect("objects");
        assert_eq!(objects.len(), 1, "one image object is reported");
        let id = &objects[0]["id"];
        let name = id["name"].as_str().expect("name");
        let index = id["op_index"].as_u64().expect("index");

        let options = format!(
            "{{\"page\":0,\"id\":{{\"name\":\"{name}\",\"op_index\":{index}}},\"dx\":2,\"dy\":3}}"
        );
        let (status, message, outputs) = parse(&run_request(&request(
            58,
            options.as_bytes(),
            &[("doc.pdf", &pdf)],
        )));
        assert_eq!(status, 0, "move_object failed: {message}");
        assert_eq!(outputs[0].0, "document-moved.pdf");
        assert!(
            outputs[0].1.starts_with(&pdf),
            "the move is an incremental append"
        );
    }

    #[test]
    fn refuses_an_unknown_op_and_a_missing_image() {
        let pdf = simple_pdf();
        let bytes = run_request(&request(1, b"{}", &[("doc.pdf", &pdf)]));
        let status = u32::from_le_bytes(bytes[4..8].try_into().expect("status"));
        assert_eq!(status, 1, "the content chunk refuses tools-chunk ops");

        let bytes = run_request(&request(
            49,
            br#"{"page":0,"rect":[0,0,10,10]}"#,
            &[("doc.pdf", &pdf)],
        ));
        let text = String::from_utf8_lossy(&bytes);
        assert!(
            text.contains("second input"),
            "the refusal names the cause: {text}"
        );
    }
}
