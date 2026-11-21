//! The browser's signature-logo chunk.
//!
//! A separate `cdylib` from the plain signing chunk. SIGN.07 measured that
//! linking the PNG/JPEG decoder into `selis-pdf-wasm-sign` cost +29 877 B
//! brotli and the `/AP /N` read-back a further +1 812 B — both past that
//! chunk's 2% baseline gate. This module carries exactly those two features,
//! fetched only when the signer actually picks a logo. Everything else about
//! signing (the CMS byte range, the detached signature, the signature report)
//! stays in `selis-pdf-wasm-sign` and is unchanged.
//!
//! Like every tools-protocol chunk it declares **no host imports** (ADR-P0011):
//! the page instantiates it with an empty import object, which is the
//! mechanical form of "this module cannot call out". The document, the logo
//! and the key all arrive as request payloads; nothing touches a network or a
//! filesystem, and key bytes are never echoed back.
//!
//! # Wire format
//!
//! All integers are little-endian. A request is:
//!
//! ```text
//! magic "SLLG" (u32)
//! version 1 (u32)
//! operation (u32: 1 sign-with-logo, 2 verify-appearance)
//! options_json_len (u32), options_json bytes
//! file_count (u32) — 2 for sign-with-logo, 2 for verify-appearance
//!   name_len (u32), name bytes
//!   data_len (u32), data bytes
//! ```
//!
//! For `sign-with-logo` file 0 is the PDF and file 1 is the caller's PNG/JPEG
//! logo. For `verify-appearance` file 0 is a produced PDF and file 1 is the
//! expected `/AP /N` stream. A response is:
//!
//! ```text
//! magic "SLRL" (u32)
//! status (u32: 0 ok, 1 error)
//! message_len (u32), message bytes (JSON on success, error text on failure)
//! output_count (u32)
//!   name_len (u32), name bytes
//!   data_len (u32), data bytes
//! ```
//!
//! `sign-with-logo` returns one signed PDF; `verify-appearance` returns none.

// The five `extern "C"` functions at the bottom are the whole unsafe surface:
// a pointer ABI is the only import-free way for a page to hand bytes to a
// `cdylib`. All parsing, decision and engine code lives in `protocol`, which
// forbids unsafe outright.
#![allow(unsafe_code)]

use std::alloc::{alloc, dealloc, Layout};

use protocol::run_request;

/// Chunk identifier used by the asset manifest and the host loader.
pub const CHUNK_ID: &str = "sign-logo";

/// Return the chunk id as guest memory, freed with [`selis_sign_logo_free`].
///
/// # Safety
///
/// `out_len` must point to four writable bytes in guest memory.
#[no_mangle]
pub unsafe extern "C" fn selis_sign_logo_chunk_id(out_len: *mut u32) -> *mut u8 {
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
    // SAFETY: the caller promised four writable bytes.
    unsafe { out_len.write(len) };
    ptr
}

/// Bit 0: sign with a logo. Bit 1: `/AP /N` read-back verification.
#[no_mangle]
pub extern "C" fn selis_sign_logo_capabilities() -> u32 {
    0b11
}

/// Allocate `len` guest bytes for a request or the length out-word.
#[no_mangle]
pub extern "C" fn selis_sign_logo_alloc(len: u32) -> *mut u8 {
    allocate(len as usize)
}

/// Free one allocation returned by this chunk, exactly once.
///
/// # Safety
/// `ptr`/`len` must identify one live allocation from this chunk.
#[no_mangle]
pub unsafe extern "C" fn selis_sign_logo_free(ptr: *mut u8, len: u32) {
    if ptr.is_null() || len == 0 {
        return;
    }
    let Ok(len) = usize::try_from(len) else {
        return;
    };
    let Ok(layout) = Layout::from_size_align(len, 1) else {
        return;
    };
    // SAFETY: the caller's contract.
    unsafe { dealloc(ptr, layout) };
}

/// Run one operation. The response pointer is freed with
/// [`selis_sign_logo_free`].
///
/// # Safety
/// `request_ptr` must be readable for `request_len` bytes; `out_len` writable.
#[no_mangle]
pub unsafe extern "C" fn selis_sign_logo_run(
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
    // bytes. The guest is single-threaded and the slice does not outlive the
    // call.
    let request = unsafe { std::slice::from_raw_parts(request_ptr, request_len) };
    let response = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| run_request(request)))
        .unwrap_or_else(|_| build_error(b"signature logo operation panicked"));
    let Ok(len) = u32::try_from(response.len()) else {
        return std::ptr::null_mut();
    };
    let ptr = allocate(response.len());
    if ptr.is_null() {
        return ptr;
    }
    std::ptr::copy_nonoverlapping(response.as_ptr(), ptr, response.len());
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

fn build_error(message: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&protocol::RESPONSE_MAGIC.to_le_bytes());
    out.extend_from_slice(&1u32.to_le_bytes());
    protocol::put_bytes(&mut out, message);
    out.extend_from_slice(&0u32.to_le_bytes());
    out
}

/// All wire parsing and all engine calls. No raw pointers, no unsafe.
mod protocol {
    #![forbid(unsafe_code)]

    use selis_pdf_tools::{clear, insert_input, take_output, MemorySession, Verification};
    use serde_json::Value;

    /// Request magic, `SLLG`.
    pub(super) const RESPONSE_MAGIC: u32 = 0x4C52_4C53;
    const REQUEST_MAGIC: u32 = 0x474C_4C53;
    const VERSION: u32 = 1;
    const MAX_FILES: u32 = 2;
    const MAX_NAME_BYTES: u32 = 128;
    /// The options carry a PEM key and its certificate chain.
    const MAX_OPTIONS_BYTES: u32 = 256 * 1024;
    /// The session key is chosen here, never by the caller.
    const SESSION_KEY: &str = "in/0";

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum Op {
        SignWithLogo,
        VerifyAppearance,
    }

    impl Op {
        fn from_wire(value: u32) -> Option<Self> {
            match value {
                1 => Some(Self::SignWithLogo),
                2 => Some(Self::VerifyAppearance),
                _ => None,
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

    pub(super) fn put_bytes(out: &mut Vec<u8>, bytes: &[u8]) {
        let len = u32::try_from(bytes.len()).unwrap_or(u32::MAX);
        out.extend_from_slice(&len.to_le_bytes());
        out.extend_from_slice(bytes);
    }

    /// One produced file, named for the download. `None` for read-only ops.
    type Output = Option<(String, Vec<u8>)>;
    /// A decoded request's result: its JSON message and at most one file.
    type RunResult = Result<(Value, Output), String>;

    fn response(status: u32, message: &[u8], outputs: &[(String, Vec<u8>)]) -> Vec<u8> {
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
    pub(crate) fn run_request(request: &[u8]) -> Vec<u8> {
        static REQUEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let _serial = REQUEST_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let _memory = MemorySession::activate();
        clear();
        let result = parse_and_run(request);
        clear();
        match result {
            Ok((message, output)) => {
                let message = serde_json::to_vec(&message).unwrap_or_default();
                match output {
                    Some((name, bytes)) => response(0, &message, &[(name, bytes)]),
                    None => response(0, &message, &[]),
                }
            }
            Err(error) => response(1, error.as_bytes(), &[]),
        }
    }

    fn parse_and_run(request: &[u8]) -> RunResult {
        let mut cursor = Cursor::new(request);
        if cursor.u32().ok_or("truncated request magic")? != REQUEST_MAGIC {
            return Err("bad signature-logo request magic".to_string());
        }
        if cursor.u32().ok_or("truncated request version")? != VERSION {
            return Err("unsupported signature-logo protocol version".to_string());
        }
        let op = Op::from_wire(cursor.u32().ok_or("truncated operation")?)
            .ok_or("unknown signature-logo operation")?;
        let options_bytes = cursor.bytes(MAX_OPTIONS_BYTES).ok_or("truncated options")?;
        let options: Value = if options_bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(options_bytes)
                .map_err(|e| format!("invalid options JSON: {e}"))?
        };

        let file_count = cursor.u32().ok_or("truncated file count")?;
        if file_count != MAX_FILES {
            return Err("this operation needs exactly two files".to_string());
        }
        let mut files: Vec<Vec<u8>> = Vec::new();
        for _ in 0..file_count {
            let name = cursor.bytes(MAX_NAME_BYTES).ok_or("truncated input name")?;
            if std::str::from_utf8(name).is_err() {
                return Err("an input name is not UTF-8".to_string());
            }
            let data = cursor.bytes(u32::MAX).ok_or("truncated input bytes")?;
            files.push(data.to_vec());
        }
        if cursor.pos != request.len() {
            return Err("trailing bytes in request".to_string());
        }
        let pdf = files.first().ok_or("no input document")?;
        let second = files.get(1).ok_or("no second input")?;
        insert_input(SESSION_KEY, pdf.clone());

        match op {
            Op::SignWithLogo => sign_with_logo(&options, second),
            Op::VerifyAppearance => verify_appearance(&options, pdf, second),
        }
    }

    /// Decode the caller's PNG/JPEG in the guest, sign with it, and prove the
    /// written `/AP /N` before the document is handed out.
    fn sign_with_logo(options: &Value, logo: &[u8]) -> RunResult {
        if logo.is_empty() {
            return Err("the logo has no image bytes; choose a PNG or JPEG file".to_string());
        }
        let params: selis_pdf_tools::DigitalOptions = serde_json::from_value(options.clone())
            .map_err(|e| format!("invalid signing options: {e}"))?;
        let field = params.field_name.clone();
        let page = params.page;
        let verification: Verification =
            selis_pdf_tools::sign_pdf_with_encoded_logo(SESSION_KEY, &params, logo, "out.pdf")
                .map_err(|e| e.0.replace(SESSION_KEY, "document"))?;
        let bytes = take_output("out.pdf").ok_or("the operation produced no output")?;
        let name = "document-signed-logo.pdf".to_string();
        let mut message = serde_json::to_value(&verification)
            .map_err(|e| format!("could not serialize the verification: {e}"))?;
        if let Some(object) = message.as_object_mut() {
            object.insert("operation".to_string(), Value::from("sign-logo"));
            object.insert("field".to_string(), Value::from(field));
            object.insert("page".to_string(), Value::from(page));
            object.insert("subFilter".to_string(), Value::from("adbe.pkcs7.detached"));
            object.insert("hasLogo".to_string(), Value::from(true));
            object.insert("appearanceVerified".to_string(), Value::from(true));
            object.insert(
                "outputs".to_string(),
                serde_json::json!([{
                    "name": name,
                    "kind": "pdf",
                    "pages": verification.pages_out,
                    "bytes": bytes.len(),
                    "verdict": verification.verdict,
                }]),
            );
        }
        Ok((message, Some((name, bytes))))
    }

    /// The read-only op: re-open the produced PDF and compare its `/AP /N`
    /// stream to the bytes the caller says were written.
    fn verify_appearance(options: &Value, pdf: &[u8], expected: &[u8]) -> RunResult {
        let field = options
            .get("fieldName")
            .and_then(Value::as_str)
            .ok_or("the read-back needs a fieldName")?
            .to_string();
        let has_logo = options
            .get("hasLogo")
            .and_then(Value::as_bool)
            .unwrap_or(true);
        if expected.is_empty() {
            return Err("the read-back needs an expected /AP /N stream".to_string());
        }
        selis_pdf_tools::verify_appearance_bytes(pdf, &field, expected, has_logo)
            .map_err(|e| e.0)?;
        let message = serde_json::json!({
            "operation": "verify-appearance",
            "field": field,
            "verified": true,
            "hasLogo": has_logo,
            "appearanceBytes": expected.len(),
        });
        Ok((message, None))
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
            let value = u32::from_le_bytes(bytes[*pos..*pos + 4].try_into().expect("u32"));
            *pos += 4;
            value
        }

        /// `(status, message, outputs)`.
        fn parse_response(bytes: &[u8]) -> (u32, Value, Vec<(String, Vec<u8>)>) {
            assert_eq!(bytes[0..4], RESPONSE_MAGIC.to_le_bytes());
            let status = u32::from_le_bytes(bytes[4..8].try_into().expect("status"));
            let mut pos = 8usize;
            let len = usize::try_from(read_u32(bytes, &mut pos)).expect("len");
            let text = String::from_utf8_lossy(&bytes[pos..pos + len]).to_string();
            pos += len;
            let message = if status == 0 {
                serde_json::from_str(&text).expect("message JSON")
            } else {
                Value::from(text)
            };
            let count = usize::try_from(read_u32(bytes, &mut pos)).expect("count");
            let mut outputs = Vec::new();
            for _ in 0..count {
                let name_len = usize::try_from(read_u32(bytes, &mut pos)).expect("name len");
                let name = String::from_utf8(bytes[pos..pos + name_len].to_vec()).expect("name");
                pos += name_len;
                let data_len = usize::try_from(read_u32(bytes, &mut pos)).expect("data len");
                outputs.push((name, bytes[pos..pos + data_len].to_vec()));
                pos += data_len;
            }
            assert_eq!(pos, bytes.len(), "trailing response bytes");
            (status, message, outputs)
        }

        fn error_text(response: &[u8]) -> String {
            let (status, message, _) = parse_response(response);
            assert_eq!(status, 1, "expected an error response");
            message.as_str().unwrap_or_default().to_string()
        }

        fn plain_pdf() -> Vec<u8> {
            let objects = vec![
                "1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n".to_string(),
                "2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n".to_string(),
                "3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 400 400] >>\nendobj\n"
                    .to_string(),
            ];
            let mut out = b"%PDF-1.4\n".to_vec();
            let mut offsets = Vec::new();
            for object in &objects {
                offsets.push(out.len());
                out.extend_from_slice(object.as_bytes());
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

        #[test]
        fn a_request_rejects_a_wrong_magic_or_unknown_op() {
            let mut bad = request(1, b"{}", &[("in.pdf", b"%PDF-1.4")]);
            bad[0] = 0;
            assert!(error_text(&run_request(&bad)).contains("magic"));
            assert!(error_text(&run_request(&request(9, b"{}", &[]))).contains("operation"));
        }

        #[test]
        fn the_readback_op_needs_two_files_a_field_and_an_expected_stream() {
            let pdf = plain_pdf();
            let response = run_request(&request(2, b"{}", &[("in.pdf", &pdf), ("ap", b"q 1 0 0")]));
            assert!(error_text(&response).contains("fieldName"));
            let options = br#"{"fieldName":"Signature1","hasLogo":true}"#;
            let response = run_request(&request(2, options, &[("in.pdf", &pdf), ("ap", b"")]));
            assert!(error_text(&response).contains("expected"));
            assert!(
                error_text(&run_request(&request(2, options, &[("in.pdf", &pdf)])))
                    .contains("two files")
            );
        }

        #[test]
        fn an_empty_or_undecodable_logo_is_refused_before_a_write() {
            let pdf = plain_pdf();
            let options = br#"{"page":1,"rect":[8,8,248,100],"fieldName":"Signature1","signerName":"Ada","signingTime":"20261104120000Z","keyPem":"x","certificatesPem":[]}"#;
            let response =
                run_request(&request(1, options, &[("in.pdf", &pdf), ("logo.png", b"")]));
            assert!(error_text(&response).contains("no image bytes"));
            let response = run_request(&request(
                1,
                options,
                &[("in.pdf", &pdf), ("logo.png", b"not an image")],
            ));
            let message = error_text(&response);
            assert!(
                message.contains("PNG") || message.contains("JPEG") || message.contains("decode"),
                "{message}"
            );
        }

        #[test]
        fn a_valid_request_produces_a_signed_document_and_a_structured_report() {
            const LEAF: &str = include_str!("../../selis-pdf-sign/tests/fixtures/leaf.crt");
            const ROOT: &str = include_str!("../../selis-pdf-sign/tests/fixtures/root.crt");
            const KEY: &str = include_str!("../../selis-pdf-sign/tests/fixtures/leaf.key");
            let pdf = plain_pdf();
            let options = serde_json::json!({
                "page": 1,
                "rect": [8.0, 8.0, 248.0, 100.0],
                "fieldName": "Signature1",
                "signerName": "Ada Lovelace",
                "signingTime": "20261104120000Z",
                "keyPem": KEY,
                "certificatesPem": [LEAF, ROOT],
            });
            let options = serde_json::to_vec(&options).expect("options");
            // A 2x2 PNG built by the same workspace decoder the chunk links.
            let budget = selis_sandbox::Budget::unlimited();
            let mut guard = budget.guard();
            let image = selis_image::Image {
                width: 2,
                height: 2,
                rgba: vec![
                    255, 0, 0, 255, 0, 0, 255, 255, 0, 255, 0, 255, 255, 255, 0, 255,
                ],
            };
            let logo = selis_image::encode_png(&image, 6, &mut guard).expect("png");
            let response = run_request(&request(
                1,
                &options,
                &[("in.pdf", &pdf), ("logo.png", &logo)],
            ));
            let (status, message, outputs) = parse_response(&response);
            assert_eq!(status, 0, "{message}");
            assert_eq!(message["operation"], "sign-logo");
            assert_eq!(message["hasLogo"], true);
            assert_eq!(message["appearanceVerified"], true);
            assert_eq!(message["outputs"][0]["kind"], "pdf");
            assert_eq!(outputs.len(), 1);
            assert!(outputs[0].1.starts_with(&pdf), "incremental revision");
            assert!(outputs[0].1.len() > pdf.len(), "a revision was appended");
            assert_eq!(message["field"], "Signature1");
        }
    }
}
