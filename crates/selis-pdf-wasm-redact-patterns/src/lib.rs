//! The pattern-redaction lazy chunk (op 47).
//!
//! This is a separate `cdylib` from the viewer, tools, convert, raster, HTML,
//! security, sign, text and optimise chunks. It carries the engine + text
//! ladder that [`selis_pdf_tools::find_candidates`] and
//! [`selis_pdf_tools::apply_reviewed`] need, so the common tools page does not
//! pay for that weight: only the "Redact patterns" card fetches this module.
//!
//! # The mandatory review step is structural
//!
//! The two wire phases are deliberately separate. `scan` calls the read-only
//! `find_candidates` (whose signature has **no output path**) and returns the
//! masked [`selis_pdf_tools::PatternReview`] as a JSON report — never a PDF.
//! `apply` calls `apply_reviewed` with an explicit `approved_ids` list supplied
//! by the host *after* a human checked the review rows. There is no
//! scan-and-apply operation in this chunk: an apply request with no approvals,
//! with an unknown id, or with a review that does not match the input bytes is
//! refused and writes nothing.
//!
//! # Wire format
//!
//! All integers are little-endian. The request framing is the shared
//! `SLTS`/`SLTR` tools framing, so the host reuses `encodeToolRequest` and
//! `decodeToolResponse` from `protocol.ts` and only swaps the URL and the guest
//! export prefix.
//!
//! ```text
//! magic "SLTS" (u32)
//! version 1 (u32)
//! operation (u32: 47)
//! options_json_len (u32), options_json bytes
//!   {"phase":"scan","patterns":["ssn",...],"literal":...,"whole_word":...}
//!   or {"phase":"apply","review":{...},"approved_ids":["p0-l0-c0",...]}
//! file_count (u32)
//!   name_len (u32), name bytes
//!   data_len (u32), data bytes
//! ```
//!
//! A response is the usual `SLTR` status/message/outputs frame. On `scan` the
//! single output is a `.json` review; on `apply` it is the redacted `.pdf`.

// The raw-pointer ABI below is the only place unsafe code is permitted. Every
// operation, parser and business rule lives in `wire`, which forbids it.
#![allow(unsafe_code)]

use std::alloc::{alloc, dealloc, Layout};
use std::panic::{catch_unwind, AssertUnwindSafe};

mod wire;

/// Chunk identifier used by the asset manifest and the host protocol.
pub const CHUNK_ID: &str = "redact-patterns";
/// Request magic, `SLTS`.
const REQUEST_MAGIC: u32 = 0x5354_4C53;
/// Response magic, `SLTR`.
const RESPONSE_MAGIC: u32 = 0x5254_4C53;
/// Protocol version (shared with the tools chunk).
const VERSION: u32 = 1;

/// Return the chunk id as guest memory. The host reads `out_len` and frees the
/// pointer with [`selis_redact_patterns_free`].
///
/// # Safety
/// `out_len` must point to four writable bytes in guest memory.
#[no_mangle]
pub unsafe extern "C" fn selis_redact_patterns_chunk_id(out_len: *mut u32) -> *mut u8 {
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

/// Availability probe; this chunk is present once fetched.
#[no_mangle]
pub extern "C" fn selis_redact_patterns_available() -> u32 {
    1
}

/// Allocate `len` bytes of guest memory for the host to fill with a request.
///
/// Returns null when the allocation fails. The caller must free the pointer
/// with [`selis_redact_patterns_free`] and the same length.
#[no_mangle]
pub extern "C" fn selis_redact_patterns_alloc(len: u32) -> *mut u8 {
    allocate(len as usize)
}

/// Free a buffer previously returned by this module.
///
/// # Safety
/// `ptr` must have been returned by one of this module's exports with exactly
/// `len` bytes, and must not have been freed already.
#[no_mangle]
pub unsafe extern "C" fn selis_redact_patterns_free(ptr: *mut u8, len: u32) {
    if ptr.is_null() || len == 0 {
        return;
    }
    let Ok(layout) = Layout::from_size_align(len as usize, 1) else {
        return;
    };
    // SAFETY: the caller's contract.
    unsafe { dealloc(ptr, layout) };
}

/// Run one pattern-redaction operation (scan or apply).
///
/// The returned pointer is a response buffer; `out_len` receives its length.
/// The host must copy it out and free it with [`selis_redact_patterns_free`].
/// A null return means the request could not be processed at all.
///
/// # Safety
/// `request_ptr` must point to `request_len` readable bytes; `out_len` must
/// point to four writable bytes.
#[no_mangle]
pub unsafe extern "C" fn selis_redact_patterns_run(
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
    let response = catch_unwind(AssertUnwindSafe(|| wire::run_request(request)))
        .unwrap_or_else(|_| wire::build_response(1, b"pattern-redaction operation panicked", &[]));
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
    use wire::{parse_response, request};

    const SSN: &str = "123-45-6789";
    const CARD: &str = "4111 1111 1111 1111";
    const EMAIL: &str = "alice@example.com";

    fn object(num: u32, body: &str) -> (u32, Vec<u8>) {
        (num, body.as_bytes().to_vec())
    }

    fn stream(num: u32, data: &[u8]) -> (u32, Vec<u8>) {
        let mut out = format!("{num} 0 obj\n<< /Length {}>>\nstream\n", data.len()).into_bytes();
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

    /// Each PII token is its own show operation, which is what the low-level
    /// redactor removes (see `redact_patterns.rs`'s fixture rationale).
    fn fixture() -> Vec<u8> {
        let content = format!(
            "BT /F1 12 Tf \
              1 0 0 1 20 700 Tm (SSN) Tj 1 0 0 1 80 700 Tm ({SSN}) Tj \
              1 0 0 1 20 680 Tm (CARD) Tj 1 0 0 1 80 680 Tm ({CARD}) Tj \
              1 0 0 1 20 660 Tm (MAIL) Tj 1 0 0 1 80 660 Tm ({EMAIL}) Tj ET"
        );
        assemble(&[
            object(1, "1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n"),
            object(2, "2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n"),
            object(
                3,
                "3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 400 760] /Contents 4 0 R /Resources << /Font << /F1 5 0 R >> >> >>\nendobj\n",
            ),
            stream(4, content.as_bytes()),
            object(5, "5 0 obj\n<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>\nendobj\n"),
        ])
    }

    fn scan_options() -> Vec<u8> {
        br#"{"phase":"scan","patterns":["ssn","card","email"]}"#.to_vec()
    }

    fn run(bytes: &[u8]) -> Vec<u8> {
        wire::run_request(bytes)
    }

    /// Error frames carry a plain-text message, not JSON.
    fn error_text(response: &[u8]) -> String {
        let status = u32::from_le_bytes(response[4..8].try_into().expect("status"));
        assert_eq!(status, 1, "expected an error response");
        let len = u32::from_le_bytes(response[8..12].try_into().expect("len")) as usize;
        std::str::from_utf8(&response[12..12 + len])
            .expect("error utf8")
            .to_string()
    }

    #[test]
    fn chunk_id_is_redact_patterns_and_the_chunk_is_available() {
        assert_eq!(CHUNK_ID, "redact-patterns");
        assert_eq!(selis_redact_patterns_available(), 1);
    }

    #[test]
    fn op_47_scan_returns_a_masked_review_and_no_pdf() {
        let pdf = fixture();
        let response = run(&request(47, &scan_options(), &[("doc.pdf", &pdf)]));
        let (status, message, outputs) = parse_response(&response);
        assert_eq!(status, 0, "scan failed: {message}");
        assert_eq!(outputs.len(), 1);
        assert!(outputs[0].0.ends_with("-review.json"), "{}", outputs[0].0);
        assert_eq!(message["outputs"][0]["kind"], "report");
        let review = String::from_utf8(outputs[0].1.clone()).expect("review utf8");
        assert!(review.contains("***-**-6789"), "masked SSN");
        assert!(review.contains("a***@example.com"), "masked email");
        for secret in [SSN, CARD, EMAIL] {
            assert!(!review.contains(secret), "the review leaked {secret}");
            assert!(!String::from_utf8_lossy(&response).contains(secret));
        }
    }

    #[test]
    fn scan_then_apply_only_the_approved_ids_removes_them() {
        let pdf = fixture();
        let scan = run(&request(47, &scan_options(), &[("doc.pdf", &pdf)]));
        let (_, _, outputs) = parse_response(&scan);
        let review: serde_json::Value = serde_json::from_slice(&outputs[0].1).expect("review JSON");
        let ssn_id = review["candidates"][0]["id"]
            .as_str()
            .expect("id")
            .to_string();
        let options = serde_json::json!({
            "phase": "apply",
            "review": review,
            "approved_ids": [ssn_id],
        })
        .to_string();
        let response = run(&request(47, options.as_bytes(), &[("doc.pdf", &pdf)]));
        let (status, message, outputs) = parse_response(&response);
        assert_eq!(status, 0, "apply failed: {message}");
        assert_eq!(message["verdict"], "ok");
        assert_eq!(outputs.len(), 1);
        assert!(outputs[0].0.ends_with(".pdf"));
        assert_eq!(&outputs[0].1[0..5], b"%PDF-");
        let text = String::from_utf8_lossy(&outputs[0].1);
        assert!(!text.contains(SSN), "the approved SSN is gone");
        assert!(text.contains(CARD), "the unapproved card stays");
        assert!(text.contains(EMAIL), "the unapproved email stays");
    }

    #[test]
    fn apply_with_no_approval_is_refused_and_writes_nothing() {
        let pdf = fixture();
        let options = br#"{"phase":"apply","review":{"schema":1,"source":"in/0","source_len":1,"source_fnv":0,"candidates":[]},"approved_ids":[]}"#;
        let response = run(&request(47, options, &[("doc.pdf", &pdf)]));
        assert!(error_text(&response).contains("EMPTY_APPROVAL"));
        // No output files follow the status, message and zero output count.
        let len = u32::from_le_bytes(response[8..12].try_into().expect("len")) as usize;
        let count_pos = 12 + len;
        assert_eq!(
            u32::from_le_bytes(
                response[count_pos..count_pos + 4]
                    .try_into()
                    .expect("count")
            ),
            0
        );
    }

    #[test]
    fn a_bad_phase_an_unknown_op_and_a_bad_magic_are_refused() {
        let pdf = fixture();
        let bad_phase = br#"{"phase":"scan-and-apply","patterns":["ssn"]}"#;
        assert!(
            error_text(&run(&request(47, bad_phase, &[("doc.pdf", &pdf)])))
                .contains("unknown variant")
        );

        for op in [41, 42, 43, 44, 46] {
            let response = run(&request(op, &scan_options(), &[("doc.pdf", &pdf)]));
            assert!(
                error_text(&response).contains("unknown tool operation"),
                "op {op} must stay unknown here"
            );
        }
        let mut bytes = request(47, &scan_options(), &[("doc.pdf", &pdf)]);
        bytes[0] ^= 0xff;
        assert!(error_text(&run(&bytes)).contains("bad request magic"));
    }

    #[test]
    fn the_abi_moves_a_request_and_reports_the_chunk_id() {
        let pdf = fixture();
        let encoded = request(47, &scan_options(), &[("doc.pdf", &pdf)]);
        let mut out_len = 0u32;
        let response = unsafe {
            selis_redact_patterns_run(
                encoded.as_ptr(),
                u32::try_from(encoded.len()).unwrap(),
                &mut out_len,
            )
        };
        assert!(!response.is_null());
        let bytes = unsafe { std::slice::from_raw_parts(response, out_len as usize) }.to_vec();
        assert_eq!(bytes[0..4], RESPONSE_MAGIC.to_le_bytes());
        unsafe { selis_redact_patterns_free(response, out_len) };

        let mut id_len = 0u32;
        let id = unsafe { selis_redact_patterns_chunk_id(&mut id_len) };
        let bytes = unsafe { std::slice::from_raw_parts(id, id_len as usize) }.to_vec();
        assert_eq!(bytes, b"redact-patterns");
        unsafe { selis_redact_patterns_free(id, id_len) };
    }
}
