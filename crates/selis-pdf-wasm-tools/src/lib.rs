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
//! On success the message JSON also carries an `outputs` array with one
//! `{name, kind, pages, bytes, verdict}` record per output. Multi-output split
//! appends a deterministic ZIP after the PDF parts; its kind is `zip` and its
//! page count is zero.
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
    Mix,
    Split,
    Rotate,
    Delete,
    Reorder,
    SetMetadata,
    PageNumbers,
    Watermark,
    HeaderFooter,
    Bates,
    Compress,
    Attachments,
    Crop,
    Flip,
    PageLabels,
    StripMetadata,
    Resize,
    Nup,
    Attach,
    Detach,
    Repair,
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
            7 => Some(Self::PageNumbers),
            8 => Some(Self::Watermark),
            9 => Some(Self::HeaderFooter),
            10 => Some(Self::Bates),
            11 => Some(Self::Compress),
            12 => Some(Self::Attachments),
            13 => Some(Self::Mix),
            14 => Some(Self::Crop),
            15 => Some(Self::Flip),
            16 => Some(Self::PageLabels),
            17 => Some(Self::StripMetadata),
            18 => Some(Self::Resize),
            19 => Some(Self::Nup),
            20 => Some(Self::Attach),
            21 => Some(Self::Detach),
            22 => Some(Self::Repair),
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

/// Bit 0 merge, 1 split, 2 rotate, 3 delete, 4 reorder, 5 metadata,
/// 6 page numbers, 7 watermark, 8 header/footer, 9 Bates, 10 compress,
/// 11 extract attachments, 12 mix pages, 13 crop, 14 flip, 15 page labels,
/// 16 strip metadata, 17 resize, 18 N-up, 19 attach files, 20 remove
/// attachments, 21 repair.
#[no_mangle]
pub extern "C" fn selis_tools_capabilities() -> u32 {
    0b11_1111_1111_1111_1111_1111
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
    // SAFETY: the caller promised `request_ptr` is readable for `request_len`
    // bytes. The guest is single-threaded, and the slice does not outlive the
    // call.
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

struct Produced {
    output: String,
    name: String,
    verification: Verification,
    over_target: bool,
}

/// Aggregate verification, per-output JSON records, and the wire files.
type RunOutput = (Verification, Vec<serde_json::Value>, Vec<(String, Vec<u8>)>);

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
    let mut first_input_len = 0usize;
    let mut input_names: Vec<String> = Vec::new();
    for index in 0..file_count {
        let name = cursor
            .bytes(MAX_NAME_BYTES)
            .ok_or_else(|| ToolError("truncated input name".to_string()))?;
        let name = std::str::from_utf8(name)
            .map_err(|_| ToolError("input name is not UTF-8".to_string()))?
            .to_string();
        input_names.push(name.clone());
        if index == 0 {
            first_name = name;
        }
        // The host never controls the internal session key: a requested name
        // cannot collide with an earlier run or escape a table.
        let data = cursor
            .bytes(u32::MAX)
            .ok_or_else(|| ToolError("truncated input bytes".to_string()))?;
        if index == 0 {
            first_input_len = data.len();
        }
        selis_pdf_tools::insert_input(&format!("in/{index}"), data.to_vec());
    }
    if cursor.pos != request.len() {
        return Err(ToolError("trailing bytes in tool request".to_string()));
    }

    let stem = file_stem(&first_name);
    let produced = match op {
        Op::Merge => {
            let inputs: Vec<String> = (0..file_count).map(|i| format!("in/{i}")).collect();
            vec![Produced {
                output: "out.pdf".to_string(),
                name: "merged.pdf".to_string(),
                verification: selis_pdf_tools::merge(&inputs, "out.pdf")?,
                over_target: false,
            }]
        }
        Op::Mix => {
            let inputs: Vec<String> = (0..file_count).map(|i| format!("in/{i}")).collect();
            vec![Produced {
                output: "out.pdf".to_string(),
                name: format!("{stem}-mixed.pdf"),
                verification: selis_pdf_tools::mix(&inputs, "out.pdf")?,
                over_target: false,
            }]
        }
        Op::Split => run_split(&options, &stem)?,
        Op::Rotate => {
            let angle = option_i64(&options, "angle")
                .ok_or_else(|| ToolError("rotate needs an angle".to_string()))?;
            let pages = option_str(&options, "pages");
            vec![Produced {
                output: "out.pdf".to_string(),
                name: format!("{stem}-rotated.pdf"),
                verification: selis_pdf_tools::rotate("in/0", angle, pages.as_deref(), "out.pdf")?,
                over_target: false,
            }]
        }
        Op::Delete => {
            let pages = option_str(&options, "pages")
                .ok_or_else(|| ToolError("delete needs a page selection".to_string()))?;
            vec![Produced {
                output: "out.pdf".to_string(),
                name: format!("{stem}-trimmed.pdf"),
                verification: selis_pdf_tools::delete("in/0", &pages, "out.pdf")?,
                over_target: false,
            }]
        }
        Op::Reorder => run_organize(&options, &stem)?,
        Op::SetMetadata => {
            let fields = option_fields(&options)?;
            let refs: Vec<(&str, &str)> = fields
                .iter()
                .map(|(key, value)| (key.as_str(), value.as_str()))
                .collect();
            vec![Produced {
                output: "out.pdf".to_string(),
                name: format!("{stem}-metadata.pdf"),
                verification: selis_pdf_tools::set_metadata("in/0", &refs, "out.pdf")?,
                over_target: false,
            }]
        }
        Op::PageNumbers => {
            let options: selis_pdf_tools::overlay::PageNumberOptions = parse_options(&options)?;
            vec![Produced {
                output: "out.pdf".to_string(),
                name: format!("{stem}-numbered.pdf"),
                verification: selis_pdf_tools::overlay::add_page_numbers(
                    "in/0", &options, "out.pdf",
                )?,
                over_target: false,
            }]
        }
        Op::Watermark => {
            let options: selis_pdf_tools::overlay::WatermarkOptions = parse_options(&options)?;
            vec![Produced {
                output: "out.pdf".to_string(),
                name: format!("{stem}-watermarked.pdf"),
                verification: selis_pdf_tools::overlay::add_watermark("in/0", &options, "out.pdf")?,
                over_target: false,
            }]
        }
        Op::HeaderFooter => {
            let options: selis_pdf_tools::overlay::HeaderFooterOptions = parse_options(&options)?;
            vec![Produced {
                output: "out.pdf".to_string(),
                name: format!("{stem}-header-footer.pdf"),
                verification: selis_pdf_tools::overlay::add_header_footer(
                    "in/0", &options, "out.pdf",
                )?,
                over_target: false,
            }]
        }
        Op::Bates => {
            let options: selis_pdf_tools::overlay::BatesOptions = parse_options(&options)?;
            vec![Produced {
                output: "out.pdf".to_string(),
                name: format!("{stem}-bates.pdf"),
                verification: selis_pdf_tools::overlay::add_bates("in/0", &options, "out.pdf")?,
                over_target: false,
            }]
        }
        Op::Compress => vec![Produced {
            output: "out.pdf".to_string(),
            name: format!("{stem}-compressed.pdf"),
            verification: selis_pdf_tools::compress("in/0", "out.pdf")?,
            over_target: false,
        }],
        Op::Attachments => return run_attachments(&stem, first_input_len),
        Op::Crop => {
            let pages = option_str(&options, "pages");
            let margins = selis_pdf_tools::CropMargins {
                left: option_f64(&options, "left").unwrap_or(0.0),
                top: option_f64(&options, "top").unwrap_or(0.0),
                right: option_f64(&options, "right").unwrap_or(0.0),
                bottom: option_f64(&options, "bottom").unwrap_or(0.0),
            };
            vec![Produced {
                output: "out.pdf".to_string(),
                name: format!("{stem}-cropped.pdf"),
                verification: selis_pdf_tools::crop("in/0", margins, pages.as_deref(), "out.pdf")?,
                over_target: false,
            }]
        }
        Op::Flip => {
            let axis = match option_str(&options, "axis").as_deref() {
                None | Some("horizontal") => selis_pdf_tools::FlipAxis::Horizontal,
                Some("vertical") => selis_pdf_tools::FlipAxis::Vertical,
                Some(other) => {
                    return Err(ToolError(format!(
                        "flip axis must be horizontal or vertical (got {other})"
                    )))
                }
            };
            let pages = option_str(&options, "pages");
            vec![Produced {
                output: "out.pdf".to_string(),
                name: format!("{stem}-flipped.pdf"),
                verification: selis_pdf_tools::flip("in/0", axis, pages.as_deref(), "out.pdf")?,
                over_target: false,
            }]
        }
        Op::PageLabels => {
            let style = match option_str(&options, "style").as_deref() {
                None | Some("D") => selis_pdf_tools::PageLabelStyle::Decimal,
                Some("r") => selis_pdf_tools::PageLabelStyle::LowerRoman,
                Some("R") => selis_pdf_tools::PageLabelStyle::UpperRoman,
                Some("a") => selis_pdf_tools::PageLabelStyle::LowerLetters,
                Some("A") => selis_pdf_tools::PageLabelStyle::UpperLetters,
                Some("None") => selis_pdf_tools::PageLabelStyle::None,
                Some(other) => {
                    return Err(ToolError(format!(
                        "page label style must be D, r, R, a, A, or None (got {other})"
                    )))
                }
            };
            let start = option_u64(&options, "start").unwrap_or(1);
            let start = u32::try_from(start).map_err(|_| {
                ToolError("page label start is outside the supported range".to_string())
            })?;
            let pages = option_str(&options, "pages");
            vec![Produced {
                output: "out.pdf".to_string(),
                name: format!("{stem}-labeled.pdf"),
                verification: selis_pdf_tools::page_labels(
                    "in/0",
                    &selis_pdf_tools::PageLabelOptions {
                        style,
                        start,
                        prefix: option_str(&options, "prefix").unwrap_or_default(),
                    },
                    pages.as_deref(),
                    "out.pdf",
                )?,
                over_target: false,
            }]
        }
        Op::StripMetadata => {
            vec![Produced {
                output: "out.pdf".to_string(),
                name: format!("{stem}-metadata-removed.pdf"),
                verification: selis_pdf_tools::strip_metadata("in/0", "out.pdf")?,
                over_target: false,
            }]
        }
        Op::Resize => {
            let width = option_f64(&options, "width")
                .ok_or_else(|| ToolError("resize needs a width".to_string()))?;
            let height = option_f64(&options, "height")
                .ok_or_else(|| ToolError("resize needs a height".to_string()))?;
            let fit = options
                .get("fit")
                .and_then(|value| value.as_bool())
                .unwrap_or(true);
            let pages = option_str(&options, "pages");
            vec![Produced {
                output: "out.pdf".to_string(),
                name: format!("{stem}-resized.pdf"),
                verification: selis_pdf_tools::resize(
                    "in/0",
                    selis_pdf_tools::ResizeOptions { width, height, fit },
                    pages.as_deref(),
                    "out.pdf",
                )?,
                over_target: false,
            }]
        }
        Op::Nup => {
            let per_sheet = option_u64(&options, "per_sheet")
                .ok_or_else(|| ToolError("N-up needs a per_sheet".to_string()))?;
            let per_sheet = usize::try_from(per_sheet)
                .map_err(|_| ToolError("N-up per_sheet is out of range".to_string()))?;
            vec![Produced {
                output: "out.pdf".to_string(),
                name: format!("{stem}-n-up.pdf"),
                verification: selis_pdf_tools::nup(
                    "in/0",
                    selis_pdf_tools::NupOptions { per_sheet },
                    "out.pdf",
                )?,
                over_target: false,
            }]
        }
        Op::Attach => {
            if file_count < 2 {
                return Err(ToolError(
                    "attach needs a PDF and at least one file to embed".to_string(),
                ));
            }
            let count = usize::try_from(file_count).unwrap_or(0);
            let files: Vec<(String, String)> = (1..count)
                .map(|index| {
                    let name = input_names.get(index).cloned().unwrap_or_default();
                    (name, format!("in/{index}"))
                })
                .collect();
            vec![Produced {
                output: "out.pdf".to_string(),
                name: format!("{stem}-attached.pdf"),
                verification: selis_pdf_tools::attachments::attach_session(
                    "in/0", &files, "out.pdf",
                )?,
                over_target: false,
            }]
        }
        Op::Detach => vec![Produced {
            output: "out.pdf".to_string(),
            name: format!("{stem}-detached.pdf"),
            verification: selis_pdf_tools::attachments::detach("in/0", "out.pdf")?,
            over_target: false,
        }],
        Op::Repair => vec![Produced {
            output: "out.pdf".to_string(),
            name: format!("{stem}-repaired.pdf"),
            verification: selis_pdf_tools::repair("in/0", "out.pdf")?,
            over_target: false,
        }],
    };

    let verifications: Vec<Verification> = produced
        .iter()
        .map(|part| part.verification.clone())
        .collect();
    let verification = aggregate_verifications(&verifications)?;

    let mut reports = Vec::with_capacity(produced.len().saturating_add(1));
    let mut outputs = Vec::with_capacity(produced.len().saturating_add(1));
    for part in &produced {
        let bytes = selis_pdf_tools::take_output(&part.output)
            .ok_or_else(|| ToolError("operation produced no output".to_string()))?;
        reports.push(serde_json::json!({
            "name": part.name,
            "kind": "pdf",
            "pages": part.verification.pages_out,
            "bytes": bytes.len(),
            "verdict": part.verification.verdict,
            "overTarget": part.over_target,
        }));
        outputs.push((part.name.clone(), bytes));
    }

    if op == Op::Split && produced.len() > 1 {
        let entries: Vec<(&str, &[u8])> = outputs
            .iter()
            .map(|(name, bytes)| (name.as_str(), bytes.as_slice()))
            .collect();
        let zip = selis_pdf_tools::bundle_zip(&entries)?;
        let zip_name = format!("{stem}-split.zip");
        reports.push(serde_json::json!({
            "name": zip_name,
            "kind": "zip",
            "pages": 0,
            "bytes": zip.len(),
            "verdict": "bundle",
        }));
        outputs.push((zip_name, zip));
    }

    Ok((verification, reports, outputs))
}

/// Extract embedded files into ordinary wire outputs. One attachment yields
/// one file; two or more also yield a deterministic ZIP. The result metadata
/// marks these outputs as `file`, so the page never treats an attachment as a
/// PDF to preview.
fn run_attachments(stem: &str, bytes_in: usize) -> Result<RunOutput, ToolError> {
    let attachments = selis_pdf_tools::attachments::extract("in/0")?;
    let count = attachments.len();
    let total = attachments.iter().fold(0u64, |sum, attachment| {
        sum.saturating_add(u64::try_from(attachment.bytes.len()).unwrap_or(u64::MAX))
    });

    let zip = if count > 1 {
        let entries: Vec<(&str, &[u8])> = attachments
            .iter()
            .map(|attachment| (attachment.name.as_str(), attachment.bytes.as_slice()))
            .collect();
        let base = format!("{stem}-attachments");
        let mut zip_name = format!("{base}.zip");
        let mut suffix = 2u32;
        while attachments
            .iter()
            .any(|attachment| attachment.name.eq_ignore_ascii_case(&zip_name))
        {
            zip_name = format!("{base}-{suffix}.zip");
            suffix = suffix.saturating_add(1);
        }
        Some((zip_name, selis_pdf_tools::bundle_zip(&entries)?))
    } else {
        None
    };

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
        embedded_files_in: Some(u64::try_from(count).unwrap_or(u64::MAX)),
        embedded_files_out: u64::try_from(count).unwrap_or(u64::MAX),
        bytes_in: u64::try_from(bytes_in).unwrap_or(u64::MAX),
        bytes_out: total,
        verdict: "ok".to_string(),
        checked: vec!["embedded_files"],
        repair_mode: None,
    };

    let extra = if zip.is_some() { 1 } else { 0 };
    let mut reports = Vec::with_capacity(count.saturating_add(extra));
    let mut outputs = Vec::with_capacity(count.saturating_add(extra));
    for attachment in attachments {
        let bytes = attachment.bytes.len();
        reports.push(serde_json::json!({
            "name": attachment.name,
            "kind": "file",
            "pages": 0,
            "bytes": bytes,
            "verdict": "extracted",
        }));
        outputs.push((attachment.name, attachment.bytes));
    }
    if let Some((name, bytes)) = zip {
        reports.push(serde_json::json!({
            "name": name,
            "kind": "zip",
            "pages": 0,
            "bytes": bytes.len(),
            "verdict": "bundle",
        }));
        outputs.push((name, bytes));
    }

    Ok((verification, reports, outputs))
}

/// Page organization: the existing reorder operation plus duplicate and
/// insert-blank modes. Keeping one wire op means the capability mask and card
/// count do not change when a mode is added.
fn run_organize(options: &serde_json::Value, stem: &str) -> Result<Vec<Produced>, ToolError> {
    let mode = option_str(options, "mode").unwrap_or_else(|| "reorder".to_string());
    match mode.as_str() {
        "reorder" => {
            let order = option_str(options, "order")
                .ok_or_else(|| ToolError("reorder needs a page order".to_string()))?;
            Ok(vec![Produced {
                output: "out.pdf".to_string(),
                name: format!("{stem}-reordered.pdf"),
                verification: selis_pdf_tools::reorder("in/0", &order, "out.pdf")?,
                over_target: false,
            }])
        }
        "duplicate" => {
            let pages = option_str(options, "pages")
                .ok_or_else(|| ToolError("duplicate needs a page selection".to_string()))?;
            Ok(vec![Produced {
                output: "out.pdf".to_string(),
                name: format!("{stem}-duplicated.pdf"),
                verification: selis_pdf_tools::duplicate_pages("in/0", &pages, "out.pdf")?,
                over_target: false,
            }])
        }
        "blank" => {
            let after =
                match option_u64(options, "after") {
                    Some(value) => Some(usize::try_from(value).map_err(|_| {
                        ToolError("blank insert position is out of range".to_string())
                    })?),
                    None => None,
                };
            let count = usize::try_from(
                option_u64(options, "count")
                    .ok_or_else(|| ToolError("blank insert needs a page count".to_string()))?,
            )
            .map_err(|_| ToolError("blank page count is out of range".to_string()))?;
            Ok(vec![Produced {
                output: "out.pdf".to_string(),
                name: format!("{stem}-blanks.pdf"),
                verification: selis_pdf_tools::insert_blank_pages("in/0", after, count, "out.pdf")?,
                over_target: false,
            }])
        }
        other => Err(ToolError(format!("unknown organize mode `{other}`"))),
    }
}

fn run_split(options: &serde_json::Value, stem: &str) -> Result<Vec<Produced>, ToolError> {
    let mode = option_str(options, "mode").unwrap_or_else(|| "range".to_string());
    match mode.as_str() {
        "range" => {
            let first = usize::try_from(
                option_u64(options, "from")
                    .ok_or_else(|| ToolError("split needs a from page".to_string()))?,
            )
            .map_err(|_| ToolError("split from is out of range".to_string()))?;
            let last = match option_u64(options, "to") {
                Some(value) => usize::try_from(value)
                    .map_err(|_| ToolError("split to is out of range".to_string()))?,
                None => usize::MAX,
            };
            Ok(vec![Produced {
                output: "out.pdf".to_string(),
                name: format!("{stem}-pages.pdf"),
                verification: selis_pdf_tools::split("in/0", first, last, "out.pdf")?,
                over_target: false,
            }])
        }
        "every" => {
            let every = usize::try_from(
                option_u64(options, "every")
                    .ok_or_else(|| ToolError("split every needs a page interval".to_string()))?,
            )
            .map_err(|_| ToolError("split interval is out of range".to_string()))?;
            let parts = selis_pdf_tools::split_every("in/0", every, "out/")?;
            Ok(named_parts(parts, stem, "pages"))
        }
        "half" => {
            let parts = selis_pdf_tools::split_half("in/0", "out/")?;
            Ok(named_parts(parts, stem, "pages"))
        }
        "size" => {
            let target_bytes = option_u64(options, "targetBytes")
                .ok_or_else(|| ToolError("split by size needs a target in bytes".to_string()))?;
            let parts = selis_pdf_tools::split_by_size("in/0", target_bytes, "out/")?;
            Ok(named_parts(parts, stem, "size"))
        }
        "bookmarks" => {
            let parts = selis_pdf_tools::split_by_bookmarks("in/0", "out/")?;
            Ok(named_parts(parts, stem, "bookmarks"))
        }
        "extract" => {
            let pages = option_str(options, "pages")
                .ok_or_else(|| ToolError("extract needs a page selection".to_string()))?;
            Ok(vec![Produced {
                output: "out.pdf".to_string(),
                name: format!("{stem}-extracted.pdf"),
                verification: selis_pdf_tools::extract_pages("in/0", &pages, "out.pdf")?,
                over_target: false,
            }])
        }
        other => Err(ToolError(format!("unknown split mode `{other}`"))),
    }
}

/// Give each backend split part a bounded, human-readable download name.
fn named_parts(parts: Vec<selis_pdf_tools::SplitPart>, stem: &str, label: &str) -> Vec<Produced> {
    parts
        .into_iter()
        .map(|part| Produced {
            name: format!(
                "{stem}-{label}-{}-{}.pdf",
                part.first.saturating_add(1),
                part.last.saturating_add(1)
            ),
            output: part.output,
            verification: part.verification,
            over_target: part.over_target,
        })
        .collect()
}

fn aggregate_verifications(parts: &[Verification]) -> Result<Verification, ToolError> {
    let first = parts
        .first()
        .ok_or_else(|| ToolError("operation produced no verification".to_string()))?;
    if parts.len() == 1 {
        return Ok(first.clone());
    }
    let mut checked = Vec::new();
    for label in &first.checked {
        if (*label == "pages" || *label == "annotations")
            && parts.iter().all(|part| part.checked.contains(label))
        {
            checked.push(*label);
        }
    }
    Ok(Verification {
        pages_in: first.pages_in,
        pages_out: parts
            .iter()
            .fold(0u64, |total, part| total.saturating_add(part.pages_out)),
        annotations_in: first.annotations_in,
        annotations_out: parts.iter().fold(0u64, |total, part| {
            total.saturating_add(part.annotations_out)
        }),
        // Per-part counts for these structures are not additive (each part
        // carries its own catalog-level copy). The aggregate only claims the
        // counts that are meaningful across the whole split.
        fields_in: None,
        fields_out: 0,
        ocgs_in: None,
        ocgs_out: 0,
        outline_entries_in: None,
        outline_entries_out: 0,
        embedded_files_in: None,
        embedded_files_out: 0,
        bytes_in: first.bytes_in,
        bytes_out: parts
            .iter()
            .fold(0u64, |total, part| total.saturating_add(part.bytes_out)),
        verdict: if parts.iter().all(|part| part.verdict == "ok") {
            "ok".to_string()
        } else {
            "failed".to_string()
        },
        checked,
        repair_mode: first.repair_mode.clone(),
    })
}

/// A bounded, single-component download stem.
///
/// Dots are replaced rather than preserved so a name such as `a..b.pdf`
/// cannot become a parent-directory marker inside the ZIP. Unicode letters
/// survive; separators, control characters, and punctuation become `_`.
fn file_stem(name: &str) -> String {
    let base = name.rsplit(['/', '\\']).next().unwrap_or(name);
    let stem = match base.rsplit_once('.') {
        Some((stem, _)) => stem,
        None => base,
    };
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

fn parse_options<T>(options: &serde_json::Value) -> Result<T, ToolError>
where
    T: serde::de::DeserializeOwned,
{
    serde_json::from_value(options.clone())
        .map_err(|e| ToolError(format!("invalid tool options: {e}")))
}

fn option_u64(options: &serde_json::Value, key: &str) -> Option<u64> {
    options.get(key)?.as_u64()
}

fn option_i64(options: &serde_json::Value, key: &str) -> Option<i64> {
    options.get(key)?.as_i64()
}

fn option_f64(options: &serde_json::Value, key: &str) -> Option<f64> {
    options.get(key)?.as_f64()
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
    #![allow(
        clippy::arithmetic_side_effects,
        clippy::expect_used,
        clippy::indexing_slicing
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

    #[test]
    fn capabilities_cover_every_operation() {
        assert_eq!(selis_tools_capabilities(), 0b11_1111_1111_1111_1111_1111);
    }

    #[test]
    fn crop_and_flip_operations_are_dispatched() {
        let pdf = multipage_pdf(2);
        let response = run_request(&request(
            14,
            br#"{"left":10,"top":5,"right":10,"bottom":5,"pages":"0"}"#,
            &[("doc.pdf", &pdf)],
        ));
        let (status, message, outputs) = parse_response(&response);
        assert_eq!(status, 0, "crop failed: {message}");
        assert_eq!(message["pages_out"], 2);
        assert_eq!(outputs[0].0, "doc-cropped.pdf");
        assert_eq!(&outputs[0].1[0..5], b"%PDF-");

        let response = run_request(&request(
            15,
            br#"{"axis":"vertical","pages":"1"}"#,
            &[("doc.pdf", &pdf)],
        ));
        let (status, message, outputs) = parse_response(&response);
        assert_eq!(status, 0, "flip failed: {message}");
        assert_eq!(message["pages_out"], 2);
        assert_eq!(outputs[0].0, "doc-flipped.pdf");

        let response = run_request(&request(
            15,
            br#"{"axis":"diagonal"}"#,
            &[("doc.pdf", &pdf)],
        ));
        assert!(error_text(&response).contains("horizontal or vertical"));
    }

    #[test]
    fn page_labels_operation_is_dispatched() {
        let pdf = multipage_pdf(3);
        let response = run_request(&request(
            16,
            br#"{"style":"R","start":4,"prefix":"A-","pages":"0,2"}"#,
            &[("doc.pdf", &pdf)],
        ));
        let (status, message, outputs) = parse_response(&response);
        assert_eq!(status, 0, "page labels failed: {message}");
        assert_eq!(message["pages_out"], 3);
        assert_eq!(outputs[0].0, "doc-labeled.pdf");
        assert_eq!(&outputs[0].1[0..5], b"%PDF-");

        let response = run_request(&request(16, br#"{"style":"roman"}"#, &[("doc.pdf", &pdf)]));
        assert!(error_text(&response).contains("page label style"));
    }

    #[test]
    fn strip_metadata_operation_is_dispatched() {
        let pdf = multipage_pdf(2);
        let response = run_request(&request(17, b"", &[("doc.pdf", &pdf)]));
        let (status, message, outputs) = parse_response(&response);
        assert_eq!(status, 0, "strip metadata failed: {message}");
        assert_eq!(message["pages_out"], 2);
        assert_eq!(outputs[0].0, "doc-metadata-removed.pdf");
        assert_eq!(&outputs[0].1[0..5], b"%PDF-");
        assert!(
            !String::from_utf8_lossy(&outputs[0].1).contains("/Metadata"),
            "the XMP stream is removed"
        );

        let response = run_request(&request(23, b"", &[("doc.pdf", &pdf)]));
        assert!(error_text(&response).contains("unknown tool operation"));
    }

    #[test]
    fn attach_and_detach_operations_are_dispatched() {
        let pdf = multipage_pdf(3);
        let note = b"local note bytes".to_vec();
        let response = run_request(&request(
            20,
            b"",
            &[("report.pdf", &pdf), ("note.txt", &note)],
        ));
        let (status, message, outputs) = parse_response(&response);
        assert_eq!(status, 0, "attach failed: {message}");
        assert_eq!(message["pages_out"], 3);
        assert_eq!(message["embedded_files_out"], 1);
        assert_eq!(outputs[0].0, "report-attached.pdf");
        assert_eq!(&outputs[0].1[0..5], b"%PDF-");
        let attached = String::from_utf8_lossy(&outputs[0].1);
        assert!(attached.contains("/EmbeddedFiles"), "the tree is written");
        assert!(attached.contains("note.txt"), "the file name is written");

        let response = run_request(&request(21, b"", &[("report-attached.pdf", &outputs[0].1)]));
        let (status, message, outputs) = parse_response(&response);
        assert_eq!(status, 0, "detach failed: {message}");
        assert_eq!(message["pages_out"], 3);
        assert_eq!(message["embedded_files_out"], 0);
        assert_eq!(outputs[0].0, "report-attached-detached.pdf");
        assert!(
            !String::from_utf8_lossy(&outputs[0].1).contains("/EmbeddedFiles"),
            "the embedded-file tree is gone"
        );

        let response = run_request(&request(20, b"", &[("report.pdf", &pdf)]));
        assert!(error_text(&response).contains("at least one file"));
    }

    #[test]
    fn repair_operation_is_dispatched() {
        let mut pdf = multipage_pdf(3);
        // Destroy the cross-reference keyword, not just its offset: the parser
        // tolerates a mis-aimed startxref, so only a genuinely unreadable
        // table forces the reconstruction path.
        let at = pdf
            .windows(4)
            .position(|window| window == b"xref".as_slice())
            .expect("xref keyword");
        pdf[at..at + 4].copy_from_slice(b"zzzz");
        let response = run_request(&request(22, b"", &[("broken.pdf", &pdf)]));
        let (status, message, outputs) = parse_response(&response);
        assert_eq!(status, 0, "repair failed: {message}");
        assert_eq!(message["pages_out"], 3);
        assert_eq!(message["repair_mode"], "reconstructed index");
        assert_eq!(outputs[0].0, "broken-repaired.pdf");
        assert_eq!(&outputs[0].1[0..5], b"%PDF-");
    }

    #[test]
    fn resize_operation_is_dispatched() {
        let pdf = multipage_pdf(3);
        let response = run_request(&request(
            18,
            br#"{"width":300,"height":400,"fit":true,"pages":"0"}"#,
            &[("doc.pdf", &pdf)],
        ));
        let (status, message, outputs) = parse_response(&response);
        assert_eq!(status, 0, "resize failed: {message}");
        assert_eq!(message["pages_out"], 3);
        assert_eq!(outputs[0].0, "doc-resized.pdf");
        assert_eq!(&outputs[0].1[0..5], b"%PDF-");
        let text = String::from_utf8_lossy(&outputs[0].1);
        assert!(
            text.contains("300.000"),
            "the target width is written: {text}"
        );
        assert!(
            text.contains("400.000"),
            "the target height is written: {text}"
        );

        let response = run_request(&request(18, br#"{"width":300}"#, &[("doc.pdf", &pdf)]));
        assert!(error_text(&response).contains("height"));
    }

    #[test]
    fn nup_operation_is_dispatched() {
        let pdf = content_pdf(3);
        let response = run_request(&request(19, br#"{"per_sheet":2}"#, &[("doc.pdf", &pdf)]));
        let (status, message, outputs) = parse_response(&response);
        assert_eq!(status, 0, "N-up failed: {message}");
        assert_eq!(message["pages_in"], 3);
        assert_eq!(message["pages_out"], 2);
        assert_eq!(outputs[0].0, "doc-n-up.pdf");
        assert_eq!(&outputs[0].1[0..5], b"%PDF-");
        let text = String::from_utf8_lossy(&outputs[0].1);
        assert!(
            text.contains("/Subtype /Form"),
            "the placed pages are forms: {text}"
        );

        let response = run_request(&request(19, br#"{"per_sheet":3}"#, &[("doc.pdf", &pdf)]));
        assert!(error_text(&response).contains("2, 4, 6, or 9"));
    }

    #[test]
    fn page_number_operation_stamps_the_selected_pages() {
        let pdf = multipage_pdf(3);
        let response = run_request(&request(
            7,
            br#"{"pages":"1,3","start":5,"format":"fraction","position":"bottom_center"}"#,
            &[("doc.pdf", &pdf)],
        ));
        let (status, message, outputs) = parse_response(&response);
        assert_eq!(status, 0, "page numbers failed: {message}");
        assert_eq!(message["pages_in"], 3);
        assert_eq!(message["pages_out"], 3);
        assert_eq!(outputs[0].0, "doc-numbered.pdf");
        assert_eq!(&outputs[0].1[0..5], b"%PDF-");
    }

    #[test]
    fn watermark_and_bates_operations_are_dispatched() {
        let pdf = multipage_pdf(2);
        let response = run_request(&request(
            8,
            br#"{"text":"CONFIDENTIAL","opacity":0.3,"pages":"2"}"#,
            &[("doc.pdf", &pdf)],
        ));
        let (status, message, outputs) = parse_response(&response);
        assert_eq!(status, 0, "watermark failed: {message}");
        assert_eq!(outputs[0].0, "doc-watermarked.pdf");

        let response = run_request(&request(
            10,
            br#"{"prefix":"ABC-","start":9,"digits":4,"pages":"1-2"}"#,
            &[("doc.pdf", &pdf)],
        ));
        let (status, message, outputs) = parse_response(&response);
        assert_eq!(status, 0, "bates failed: {message}");
        assert_eq!(outputs[0].0, "doc-bates.pdf");
    }

    #[test]
    fn header_footer_operation_requires_at_least_one_slot() {
        let pdf = multipage_pdf(1);
        let response = run_request(&request(
            9,
            br#"{"header":{"left":"Page {page} of {pages}"}}"#,
            &[("doc.pdf", &pdf)],
        ));
        let (status, message, outputs) = parse_response(&response);
        assert_eq!(status, 0, "header/footer failed: {message}");
        assert_eq!(outputs[0].0, "doc-header-footer.pdf");

        let response = run_request(&request(9, b"{}", &[("doc.pdf", &pdf)]));
        assert!(error_text(&response).contains("at least one"));
    }

    #[test]
    fn compress_operation_returns_a_verified_pdf() {
        let pdf = multipage_pdf(1);
        let response = run_request(&request(11, b"{}", &[("doc.pdf", &pdf)]));
        let (status, message, outputs) = parse_response(&response);
        assert_eq!(status, 0, "compress failed: {message}");
        assert_eq!(message["verdict"], "ok");
        assert_eq!(outputs[0].0, "doc-compressed.pdf");
        assert_eq!(&outputs[0].1[0..5], b"%PDF-");
    }

    #[test]
    fn attachments_operation_returns_files_and_a_zip() {
        let pdf = attachment_pdf();
        let response = run_request(&request(12, b"", &[("bundle.pdf", &pdf)]));
        let (status, message, outputs) = parse_response(&response);
        assert_eq!(status, 0, "attachments failed: {message}");
        assert_eq!(message["embedded_files_in"], 2);
        assert_eq!(message["embedded_files_out"], 2);
        assert!(message.get("pages_in").is_none());
        assert_eq!(message["verdict"], "ok");
        assert_eq!(outputs.len(), 3);

        let notes = outputs
            .iter()
            .find(|(name, _)| name == "notes.txt")
            .expect("notes attachment");
        assert_eq!(notes.1, b"hello attachment");
        let evil = outputs
            .iter()
            .find(|(name, _)| name == "evil.txt")
            .expect("sanitized attachment name");
        assert_eq!(evil.1, b"safe name");
        let zip = outputs
            .iter()
            .find(|(name, _)| name == "bundle-attachments.zip")
            .expect("attachment bundle");
        assert_eq!(&zip.1[0..4], b"PK\x03\x04");

        let reports = message["outputs"].as_array().expect("output reports");
        assert_eq!(reports.len(), 3);
        assert!(reports
            .iter()
            .filter(|report| report["kind"] == "file")
            .all(|report| report["pages"] == 0));
        assert_eq!(
            reports
                .iter()
                .filter(|report| report["kind"] == "zip")
                .count(),
            1
        );
    }

    #[test]
    fn attachment_zip_name_does_not_collide_with_a_file() {
        let pdf = attachment_pdf_named("bundle-attachments.zip", "notes.txt");
        let response = run_request(&request(12, b"", &[("bundle.pdf", &pdf)]));
        let (status, message, outputs) = parse_response(&response);
        assert_eq!(status, 0, "attachments failed: {message}");
        assert!(outputs
            .iter()
            .any(|(name, _)| name == "bundle-attachments-2.zip"));
        assert_eq!(
            outputs
                .iter()
                .filter(|(name, _)| name == "bundle-attachments.zip")
                .count(),
            1
        );
    }

    #[test]
    fn mix_operation_interleaves_and_reports_the_combined_page_count() {
        let a = multipage_pdf(2);
        let b = multipage_pdf(3);
        let response = run_request(&request(13, b"", &[("a.pdf", &a), ("b.pdf", &b)]));
        let (status, message, outputs) = parse_response(&response);
        assert_eq!(status, 0, "mix failed: {message}");
        assert_eq!(outputs.len(), 1);
        assert_eq!(outputs[0].0, "a-mixed.pdf");
        assert_eq!(message["pages_in"], 5);
        assert_eq!(message["pages_out"], 5);
        assert_eq!(message["verdict"], "ok");
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

    fn attachment_pdf() -> Vec<u8> {
        attachment_pdf_named("../../evil.txt", "notes.txt")
    }

    fn attachment_pdf_named(first: &str, second: &str) -> Vec<u8> {
        let mut objects: Vec<Vec<u8>> = vec![
            b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R /Names 4 0 R >>\nendobj\n".to_vec(),
            b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n".to_vec(),
            b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] >>\nendobj\n"
                .to_vec(),
            b"4 0 obj\n<< /EmbeddedFiles 5 0 R >>\nendobj\n".to_vec(),
            format!("5 0 obj\n<< /Names [({first}) 6 0 R ({second}) 7 0 R] >>\nendobj\n")
                .into_bytes(),
            format!(
                "6 0 obj\n<< /Type /Filespec /UF ({first}) /F ({first}) /EF << /F 8 0 R >> >>\nendobj\n"
            )
            .into_bytes(),
            format!(
                "7 0 obj\n<< /Type /Filespec /UF ({second}) /F ({second}) /EF << /F 9 0 R >> >>\nendobj\n"
            )
            .into_bytes(),
        ];
        objects.push(stream_object(8, b"safe name"));
        objects.push(stream_object(9, b"hello attachment"));
        let mut out = b"%PDF-1.4\n".to_vec();
        let mut offsets = Vec::new();
        for object in &objects {
            offsets.push(out.len());
            out.extend_from_slice(object);
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

    fn stream_object(number: usize, data: &[u8]) -> Vec<u8> {
        let mut out = format!(
            "{number} 0 obj\n<< /Type /EmbeddedFile /Length {} >>\nstream\n",
            data.len()
        )
        .into_bytes();
        out.extend_from_slice(data);
        out.extend_from_slice(b"\nendstream\nendobj\n");
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

    fn content_pdf(pages: usize) -> Vec<u8> {
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
                "{} 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Contents {} 0 R >>\nendobj\n",
                index.saturating_add(3),
                index.saturating_add(3).saturating_add(pages),
            ));
        }
        let stream = "q 1 0 0 1 0 0 cm Q";
        for index in 0..pages {
            objects.push(format!(
                "{} 0 obj\n<< /Length {} >>\nstream\n{stream}\nendstream\nendobj\n",
                index.saturating_add(3).saturating_add(pages),
                stream.len()
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

    fn split_request(options: &str, pages: usize) -> Vec<u8> {
        let pdf = multipage_pdf(pages);
        request(2, options.as_bytes(), &[("doc.pdf", &pdf)])
    }

    fn organize_request(options: &str, pages: usize) -> Vec<u8> {
        let pdf = multipage_pdf(pages);
        request(5, options.as_bytes(), &[("doc.pdf", &pdf)])
    }

    #[test]
    fn organize_modes_duplicate_and_insert_blanks() {
        let (status, message, outputs) = parse_response(&run_request(&organize_request(
            r#"{"mode":"reorder","order":"2,0,1"}"#,
            3,
        )));
        assert_eq!(status, 0, "reorder failed: {message}");
        assert_eq!(message["pages_out"], 3);
        assert_eq!(outputs[0].0, "doc-reordered.pdf");
        assert_eq!(&outputs[0].1[0..5], b"%PDF-");

        let (status, message, outputs) = parse_response(&run_request(&organize_request(
            r#"{"mode":"duplicate","pages":"1"}"#,
            3,
        )));
        assert_eq!(status, 0, "duplicate failed: {message}");
        assert_eq!(message["pages_in"], 3);
        assert_eq!(message["pages_out"], 4);
        assert_eq!(outputs[0].0, "doc-duplicated.pdf");
        assert_eq!(&outputs[0].1[0..5], b"%PDF-");

        let (status, message, outputs) = parse_response(&run_request(&organize_request(
            r#"{"mode":"blank","after":1,"count":2}"#,
            3,
        )));
        assert_eq!(status, 0, "blank insert failed: {message}");
        assert_eq!(message["pages_in"], 3);
        assert_eq!(message["pages_out"], 5);
        assert_eq!(outputs[0].0, "doc-blanks.pdf");
        assert_eq!(&outputs[0].1[0..5], b"%PDF-");
    }

    #[test]
    fn organize_blank_mode_supports_start_and_append() {
        let (status, message, _) = parse_response(&run_request(&organize_request(
            r#"{"mode":"blank","after":0,"count":1}"#,
            3,
        )));
        assert_eq!(status, 0, "blank at start failed: {message}");
        assert_eq!(message["pages_out"], 4);

        let (status, message, _) = parse_response(&run_request(&organize_request(
            r#"{"mode":"blank","count":1}"#,
            3,
        )));
        assert_eq!(status, 0, "blank append failed: {message}");
        assert_eq!(message["pages_out"], 4);
    }

    #[test]
    fn organize_rejects_unknown_modes_and_bad_requests() {
        let response = run_request(&organize_request(r#"{"mode":"shuffle"}"#, 3));
        assert!(error_text(&response).contains("unknown organize mode"));

        let response = run_request(&organize_request(r#"{"mode":"duplicate"}"#, 3));
        assert!(error_text(&response).contains("page selection"));

        let response = run_request(&organize_request(r#"{"mode":"blank","count":0}"#, 3));
        assert!(error_text(&response).contains("at least one"));
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

    #[test]
    fn split_every_returns_named_parts_and_a_zip() {
        let response = run_request(&split_request(r#"{"mode":"every","every":2}"#, 5));
        let (status, message, outputs) = parse_response(&response);
        assert_eq!(status, 0, "split every failed: {message}");
        assert_eq!(outputs.len(), 4, "three PDFs and one ZIP");
        let reports = message["outputs"].as_array().expect("outputs metadata");
        assert_eq!(reports.len(), 4);
        assert_eq!(
            reports
                .iter()
                .map(|report| report["kind"].as_str().unwrap_or(""))
                .collect::<Vec<_>>(),
            vec!["pdf", "pdf", "pdf", "zip"]
        );
        assert_eq!(
            reports
                .iter()
                .map(|report| report["pages"].as_u64().unwrap_or(999))
                .collect::<Vec<_>>(),
            vec![2, 2, 1, 0]
        );
        assert_eq!(outputs[0].0, "doc-pages-1-2.pdf");
        assert_eq!(outputs[1].0, "doc-pages-3-4.pdf");
        assert_eq!(outputs[2].0, "doc-pages-5-5.pdf");
        assert_eq!(outputs[3].0, "doc-split.zip");
        assert_eq!(&outputs[0].1[0..5], b"%PDF-");
        assert_eq!(&outputs[3].1[0..4], b"PK\x03\x04");
        assert_eq!(message["pages_in"], 5);
        assert_eq!(message["pages_out"], 5);
        assert_eq!(message["verdict"], "ok");
        assert_eq!(
            message["checked"],
            serde_json::json!(["pages", "annotations"])
        );
    }

    #[test]
    fn split_half_and_range_without_to_are_both_bounded() {
        let (status, message, outputs) =
            parse_response(&run_request(&split_request(r#"{"mode":"half"}"#, 5)));
        assert_eq!(status, 0, "split half failed: {message}");
        assert_eq!(outputs.len(), 3, "two PDFs and one ZIP");
        let reports = message["outputs"].as_array().expect("reports");
        assert_eq!(reports[0]["pages"], 2);
        assert_eq!(reports[1]["pages"], 3);
        assert_eq!(reports[2]["kind"], "zip");

        let (status, message, outputs) = parse_response(&run_request(&split_request(
            r#"{"mode":"range","from":2}"#,
            5,
        )));
        assert_eq!(status, 0, "range split failed: {message}");
        assert_eq!(outputs.len(), 1, "range mode produces one PDF");
        assert_eq!(outputs[0].0, "doc-pages.pdf");
        assert_eq!(message["pages_out"], 3);
        assert_eq!(message["outputs"][0]["kind"], "pdf");
    }

    #[test]
    fn size_split_flags_parts_that_cannot_meet_the_target() {
        let response = run_request(&split_request(r#"{"mode":"size","targetBytes":1}"#, 2));
        let (status, message, outputs) = parse_response(&response);
        assert_eq!(status, 0, "size split failed: {message}");
        assert_eq!(outputs.len(), 3, "two PDFs and one ZIP");
        let reports = message["outputs"].as_array().expect("reports");
        assert_eq!(reports[0]["overTarget"], true);
        assert_eq!(reports[1]["overTarget"], true);
        assert_eq!(reports[0]["pages"], 1);
        assert_eq!(reports[1]["pages"], 1);
        assert_eq!(reports[2]["kind"], "zip");
        assert_eq!(outputs[0].0, "doc-size-1-1.pdf");
        assert_eq!(outputs[1].0, "doc-size-2-2.pdf");

        let response = run_request(&split_request(r#"{"mode":"size"}"#, 2));
        assert!(
            error_text(&response).contains("target"),
            "missing target is reported"
        );
        let response = run_request(&split_request(r#"{"mode":"size","targetBytes":0}"#, 2));
        assert!(
            error_text(&response).contains("at least one byte"),
            "a zero target is refused"
        );
    }

    #[test]
    fn bookmarks_split_uses_the_fixture_outline() {
        let fixture: &[u8] = include_bytes!("../../../corpus/fixtures/tagged_structure.pdf");
        let response = run_request(&request(
            2,
            br#"{"mode":"bookmarks"}"#,
            &[("doc.pdf", fixture)],
        ));
        let (status, message, outputs) = parse_response(&response);
        assert_eq!(status, 0, "bookmark split failed: {message}");
        assert_eq!(outputs.len(), 3, "two PDFs and one ZIP");
        assert_eq!(outputs[0].0, "doc-bookmarks-1-1.pdf");
        assert_eq!(outputs[1].0, "doc-bookmarks-2-2.pdf");
        assert_eq!(outputs[2].0, "doc-split.zip");
        assert_eq!(message["pages_in"], 2);
        assert_eq!(message["pages_out"], 2);
        assert_eq!(message["outputs"][0]["pages"], 1);
        assert_eq!(message["outputs"][1]["pages"], 1);
        assert_eq!(message["outputs"][0]["overTarget"], false);

        let response = run_request(&split_request(r#"{"mode":"bookmarks"}"#, 2));
        assert!(
            error_text(&response).contains("no bookmarks"),
            "a document without bookmarks is refused"
        );
    }

    #[test]
    fn extract_mode_returns_one_pdf_in_document_order() {
        let response = run_request(&split_request(r#"{"mode":"extract","pages":"4,1-2"}"#, 6));
        let (status, message, outputs) = parse_response(&response);
        assert_eq!(status, 0, "extract failed: {message}");
        assert_eq!(outputs.len(), 1);
        assert_eq!(outputs[0].0, "doc-extracted.pdf");
        assert_eq!(message["pages_in"], 6);
        assert_eq!(message["pages_out"], 3);
        assert_eq!(message["outputs"][0]["pages"], 3);
    }

    #[test]
    fn split_every_reports_the_part_limit() {
        let response = run_request(&split_request(r#"{"mode":"every","every":1}"#, 65));
        let status = u32::from_le_bytes(response[4..8].try_into().expect("status"));
        assert_eq!(status, 1);
        let message_len =
            usize::try_from(u32::from_le_bytes(response[8..12].try_into().expect("len")))
                .expect("message length");
        let message = std::str::from_utf8(&response[12..12 + message_len]).expect("error text");
        assert!(
            message.contains("64"),
            "error should name the limit: {message}"
        );
    }
}
