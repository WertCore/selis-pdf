//! The compare lazy chunk: a render-and-text diff of two PDFs, moved out of the
//! tools and text chunks so neither common page pays for the other engine half.
//!
//! This is a separate `cdylib` from the viewer, tools, optimise, text and raster
//! chunks. Its wire protocol is the same import-free `SLTS`/`SLTR`
//! request/response framing as the tools chunk, so the host can encode and
//! decode with the same functions and only swaps the URL, the export prefix and
//! the op code. Compare is the only two-input tool-protocol operation: the
//! request carries `in/0` and `in/1`, and op 46 returns a JSON report as a
//! downloadable `kind: "report"` output rather than a rewritten document.
//!
//! # Why a chunk of its own
//!
//! `selis_pdf_tools::compare_pdfs` links both the tiny-skia raster pipeline (to
//! render every page of both documents at the same DPI) and the text ladder (to
//! compare the extracted text). The tools chunk is deliberately engine-free; a
//! probe that added compare to the text chunk measured 320 596 B → 574 705 B
//! brotli, over its 400 000 B budget and far past the 2% baseline gate. A new
//! import-free chunk with its own budget keeps that weight behind one explicit
//! card action.
//!
//! # Wire format
//!
//! All integers are little-endian. A request is:
//!
//! ```text
//! magic "SLTS" (u32)
//! version 1 (u32)
//! operation (u32: 46 compare)
//! options_json_len (u32), options_json bytes (`{"dpi":72}` or `{}`)
//! file_count (u32: exactly 2)
//!   name_len (u32), name bytes
//!   data_len (u32), data bytes
//! ```
//!
//! A response is:
//!
//! ```text
//! magic "SLTR" (u32)
//! status (u32: 0 ok, 1 error)
//! message_len (u32), message bytes (verification JSON + outputs on success)
//! output_count (u32)
//!   name_len (u32), name bytes
//!   data_len (u32), data bytes
//! ```
//!
//! The host allocates request and length buffers with [`selis_compare_alloc`],
//! calls [`selis_compare_run`], copies the response out, and frees both with
//! [`selis_compare_free`]. The module declares no imports, so the page can
//! instantiate it with an empty import object.

// The C ABI at the boundary needs raw pointers. All request parsing, dispatch
// and response building lives in `logic`, which literally forbids `unsafe`.
#![allow(unsafe_code)]

mod logic;

use std::alloc::{alloc, dealloc, Layout};
use std::panic::{catch_unwind, AssertUnwindSafe};

use logic::{build_response, run_request, CHUNK_ID};
#[cfg(test)]
use logic::{REQUEST_MAGIC, RESPONSE_MAGIC, VERSION};

/// Return the chunk id as guest memory. The host reads `out_len` and frees the
/// pointer with [`selis_compare_free`].
///
/// # Safety
///
/// `out_len` must point to four writable bytes in guest memory.
#[no_mangle]
pub unsafe extern "C" fn selis_compare_chunk_id(out_len: *mut u32) -> *mut u8 {
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

/// Availability probe; the compare chunk is always present once fetched.
#[no_mangle]
pub extern "C" fn selis_compare_available() -> u32 {
    1
}

/// Allocate `len` bytes of guest memory for the host to fill with a request.
///
/// Returns null when the allocation fails. The caller must free the pointer
/// with [`selis_compare_free`] and the same length.
#[no_mangle]
pub extern "C" fn selis_compare_alloc(len: u32) -> *mut u8 {
    allocate(usize::try_from(len).unwrap_or(usize::MAX))
}

/// Free a buffer previously returned by this module.
///
/// # Safety
///
/// `ptr` must have been returned by one of this module's exports with exactly
/// `len` bytes, and must not have been freed already.
#[no_mangle]
pub unsafe extern "C" fn selis_compare_free(ptr: *mut u8, len: u32) {
    if ptr.is_null() || len == 0 {
        return;
    }
    let Ok(layout) = Layout::from_size_align(usize::try_from(len).unwrap_or(usize::MAX), 1) else {
        return;
    };
    // SAFETY: the caller's contract.
    unsafe { dealloc(ptr, layout) };
}

/// Run one compare operation.
///
/// The returned pointer is a response buffer; `out_len` receives its length.
/// The host must copy it out and free it with [`selis_compare_free`]. A null
/// return means the request could not be processed at all.
///
/// # Safety
///
/// `request_ptr` must point to `request_len` readable bytes; `out_len` must
/// point to four writable bytes.
#[no_mangle]
pub unsafe extern "C" fn selis_compare_run(
    request_ptr: *const u8,
    request_len: u32,
    out_len: *mut u32,
) -> *mut u8 {
    if out_len.is_null() || request_ptr.is_null() {
        return std::ptr::null_mut();
    }
    let request_len = usize::try_from(request_len).unwrap_or(usize::MAX);
    // SAFETY: the caller promised `request_ptr` is readable for `request_len`
    // bytes. The guest is single-threaded; the slice does not outlive the call.
    let request = unsafe { std::slice::from_raw_parts(request_ptr, request_len) };
    let response = catch_unwind(AssertUnwindSafe(|| run_request(request))).unwrap_or_else(|_| {
        let message = b"compare operation panicked";
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

    fn build_xref(objects: Vec<Vec<u8>>) -> Vec<u8> {
        let mut out = b"%PDF-1.7\n".to_vec();
        let mut offsets = Vec::with_capacity(objects.len());
        for object in objects {
            offsets.push(out.len());
            out.extend_from_slice(&object);
        }
        let xref = out.len();
        let size = offsets.len() + 1;
        out.extend_from_slice(format!("xref\n0 {size}\n").as_bytes());
        out.extend_from_slice(b"0000000000 65535 f \n");
        for offset in offsets {
            out.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
        }
        out.extend_from_slice(
            format!("trailer\n<< /Size {size} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n")
                .as_bytes(),
        );
        out
    }

    fn blank_pdf(pages: usize) -> Vec<u8> {
        let mut objects = vec![b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n".to_vec()];
        let kids: Vec<String> = (0..pages).map(|i| format!("{} 0 R", 3 + i)).collect();
        objects.push(
            format!(
                "2 0 obj\n<< /Type /Pages /Kids [{}] /Count {pages} >>\nendobj\n",
                kids.join(" ")
            )
            .into_bytes(),
        );
        for i in 0..pages {
            objects.push(
                format!(
                    "{} 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] >>\nendobj\n",
                    3 + i
                )
                .into_bytes(),
            );
        }
        build_xref(objects)
    }

    fn black_pdf() -> Vec<u8> {
        let content = b"0 0 0 rg\n0 0 200 200 re\nf\n";
        let mut stream = format!("4 0 obj\n<< /Length {} >>\nstream\n", content.len()).into_bytes();
        stream.extend_from_slice(content);
        stream.extend_from_slice(b"\nendstream\nendobj\n");
        build_xref(vec![
            b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n".to_vec(),
            b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n".to_vec(),
            b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Contents 4 0 R >>\nendobj\n".to_vec(),
            stream,
        ])
    }

    #[test]
    fn chunk_id_is_compare_and_the_chunk_is_available() {
        assert_eq!(CHUNK_ID, "compare");
        assert_eq!(selis_compare_available(), 1);
    }

    #[test]
    fn op_46_compares_two_identical_documents_and_returns_a_json_report() {
        let a = blank_pdf(1);
        let b = blank_pdf(1);
        let response = run(&request(
            46,
            b"{\"dpi\":72}",
            &[("a.pdf", &a), ("b.pdf", &b)],
        ));
        let (status, message, outputs) = parse_response(&response);
        assert_eq!(status, 0, "compare failed: {message}");
        assert_eq!(message["verdict"], "read");
        assert_eq!(outputs.len(), 1);
        assert_eq!(outputs[0].0, "a-compare.json");
        assert_eq!(message["outputs"][0]["kind"], "report");
        let report: serde_json::Value = serde_json::from_slice(&outputs[0].1).expect("report JSON");
        assert_eq!(report["schema"], "selis-compare/1");
        assert_eq!(report["status"], "ok");
        assert_eq!(report["identical"], true);
        assert_eq!(report["text_identical"], true);
        assert_eq!(report["pages_a"], 1);
        assert_eq!(report["pages_b"], 1);
        assert_eq!(report["pages"][0]["present_in"], "both");
    }

    #[test]
    fn a_pixel_difference_is_reported() {
        let a = blank_pdf(1);
        let b = black_pdf();
        let response = run(&request(46, b"", &[("a.pdf", &a), ("b.pdf", &b)]));
        let (status, _message, outputs) = parse_response(&response);
        assert_eq!(status, 0);
        let report: serde_json::Value = serde_json::from_slice(&outputs[0].1).expect("report JSON");
        assert_eq!(report["identical"], false);
        assert_eq!(report["pages"][0]["pixels_identical"], false);
        assert!(
            report["pages"][0]["diff_fraction"]
                .as_f64()
                .unwrap_or_default()
                > 0.5
        );
    }

    #[test]
    fn a_different_page_count_is_reported() {
        let a = blank_pdf(1);
        let b = blank_pdf(2);
        let response = run(&request(46, b"", &[("a.pdf", &a), ("b.pdf", &b)]));
        let (status, _message, outputs) = parse_response(&response);
        assert_eq!(status, 0);
        let report: serde_json::Value = serde_json::from_slice(&outputs[0].1).expect("report JSON");
        assert_eq!(report["pages_a"], 1);
        assert_eq!(report["pages_b"], 2);
        assert_eq!(report["pages"][1]["present_in"], "b_only");
    }

    #[test]
    fn tools_ops_are_not_dispatched_here() {
        let a = blank_pdf(1);
        for op in [1, 41, 44, 45] {
            let response = run(&request(op, b"", &[("a.pdf", &a), ("b.pdf", &a)]));
            assert!(
                error_text(&response).contains("unknown tool operation"),
                "op {op} must stay unknown to the compare chunk"
            );
        }
    }

    #[test]
    fn malformed_requests_are_refused() {
        let a = blank_pdf(1);
        let mut bytes = request(46, b"", &[("a.pdf", &a), ("b.pdf", &a)]);
        bytes[0] ^= 0xff;
        assert!(error_text(&run(&bytes)).contains("bad request magic"));
        assert!(error_text(&run(&request(46, b"", &[("a.pdf", &a)]))).contains("exactly two PDFs"));
        assert!(error_text(&run(&request(
            46,
            b"{\"dpi\":0}",
            &[("a.pdf", &a), ("b.pdf", &a)]
        )))
        .contains("dpi must be between"));
        assert!(error_text(&run(&request(
            46,
            b"{\"dpi\":\"high\"}",
            &[("a.pdf", &a), ("b.pdf", &a)]
        )))
        .contains("whole number"));
    }

    #[test]
    fn the_abi_moves_a_request_and_reports_the_chunk_id() {
        let a = blank_pdf(1);
        let encoded = request(46, b"", &[("a.pdf", &a), ("a2.pdf", &a)]);
        let mut out_len = 0u32;
        let response = unsafe {
            selis_compare_run(
                encoded.as_ptr(),
                u32::try_from(encoded.len()).unwrap(),
                &mut out_len,
            )
        };
        assert!(!response.is_null());
        let bytes = unsafe { std::slice::from_raw_parts(response, out_len as usize) }.to_vec();
        assert_eq!(bytes[0..4], RESPONSE_MAGIC.to_le_bytes());
        unsafe { selis_compare_free(response, out_len) };

        let mut id_len = 0u32;
        let id = unsafe { selis_compare_chunk_id(&mut id_len) };
        let bytes = unsafe { std::slice::from_raw_parts(id, id_len as usize) }.to_vec();
        assert_eq!(bytes, b"compare");
        unsafe { selis_compare_free(id, id_len) };
    }
}
