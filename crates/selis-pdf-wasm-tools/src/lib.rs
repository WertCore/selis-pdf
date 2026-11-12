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
//! `{name, kind, pages, bytes, verdict}` record per output. `kind` is `pdf`
//! for a rewritten document, `zip` for a bundle, `file` for an extracted
//! attachment, and `report` for the metadata JSON export. Multi-output split
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
    ImageToPdf,
    Zip,
    ImageWatermark,
    Outline,
    MetadataReport,
    Batch,
    FormFields,
    FillForms,
    Fingerprint,
    Privacy,
    EditText,
    Redact,
    Annotate,
    Annotations,
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
            23 => Some(Self::ImageToPdf),
            24 => Some(Self::Zip),
            25 => Some(Self::ImageWatermark),
            26 => Some(Self::Outline),
            27 => Some(Self::MetadataReport),
            28 => Some(Self::Batch),
            29 => Some(Self::FormFields),
            30 => Some(Self::FillForms),
            31 => Some(Self::Fingerprint),
            32 => Some(Self::Privacy),
            33 => Some(Self::EditText),
            34 => Some(Self::Redact),
            35 => Some(Self::Annotate),
            36 => Some(Self::Annotations),
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
/// attachments, 21 repair, 22 image to PDF, 23 ZIP, 24 image watermark,
/// 25 outline editing, 26 metadata report, 27 batch queue, 28 form fields,
/// 29 fill forms, 30 fingerprint, 31 privacy scan. The 32 bits of the mask are
/// full; operation 33 (edit text) has no bit of its own. The mask is purely
/// informational — no consumer gates on it, and the op dispatch matches on the
/// wire code directly — so the 33rd op is reached without widening the mask.
#[no_mangle]
pub extern "C" fn selis_tools_capabilities() -> u32 {
    u32::MAX
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
        Ok((verification, reports, outputs, batch)) => {
            let mut message =
                serde_json::to_value(&verification).unwrap_or(serde_json::Value::Null);
            if let Some(object) = message.as_object_mut() {
                object.insert("outputs".to_string(), serde_json::Value::Array(reports));
                if let Some(batch) = batch {
                    object.insert("batch".to_string(), batch);
                }
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

/// Aggregate verification, per-output JSON records, the wire files, and — for
/// a run over several documents — the per-file rows that say what happened to
/// each one. Only the batch queue fills that last field: every other operation
/// has one input document, so its rows would repeat the single result.
type RunOutput = (
    Verification,
    Vec<serde_json::Value>,
    Vec<(String, Vec<u8>)>,
    Option<serde_json::Value>,
);

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
    // The Zip operation bundles the selected files by their user-visible names,
    // so it needs the bytes here rather than re-reading the session store.
    let mut input_files: Vec<(String, Vec<u8>)> = Vec::new();
    for index in 0..file_count {
        let name = cursor
            .bytes(MAX_NAME_BYTES)
            .ok_or_else(|| ToolError("truncated input name".to_string()))?;
        let name = std::str::from_utf8(name)
            .map_err(|_| ToolError("input name is not UTF-8".to_string()))?
            .to_string();
        input_names.push(name.clone());
        if index == 0 {
            first_name = name.clone();
        }
        // The host never controls the internal session key: a requested name
        // cannot collide with an earlier run or escape a table.
        let data = cursor
            .bytes(u32::MAX)
            .ok_or_else(|| ToolError("truncated input bytes".to_string()))?;
        if index == 0 {
            first_input_len = data.len();
        }
        input_files.push((name.clone(), data.to_vec()));
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
        Op::Compress => {
            let level = match option_str(&options, "level").as_deref() {
                None => None,
                Some(name) => Some(
                    selis_pdf_tools::CompressionLevel::from_name(name).ok_or_else(|| {
                        ToolError(format!(
                            "compress level must be less, recommended or extreme (got {name})"
                        ))
                    })?,
                ),
            };
            let target_kib = option_u64(&options, "targetKiB");
            let compress_options = selis_pdf_tools::CompressOptions { level, target_kib };
            let (report, verification) =
                selis_pdf_tools::compress_file_with_options("in/0", "out.pdf", &compress_options)?;
            let over_target = target_kib.is_some_and(|kib| {
                u64::try_from(report.out_bytes).unwrap_or(u64::MAX) > kib.saturating_mul(1024)
            });
            vec![Produced {
                output: "out.pdf".to_string(),
                name: format!("{stem}-compressed.pdf"),
                verification,
                over_target,
            }]
        }
        Op::Attachments => return run_attachments(&stem, first_input_len),
        Op::Annotate => {
            let annotate_options: selis_pdf_tools::AnnotateOptions = parse_options(&options)?;
            vec![Produced {
                output: "out.pdf".to_string(),
                name: format!("{stem}-annotated.pdf"),
                verification: selis_pdf_tools::annotate("in/0", &annotate_options, "out.pdf")?,
                over_target: false,
            }]
        }
        Op::Annotations => return run_annotations(&options, &stem, first_input_len),
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
        Op::ImageToPdf => {
            let page_size = match option_str(&options, "page_size") {
                Some(value) => selis_pdf_tools::ImagePageSize::parse(&value)?,
                None => selis_pdf_tools::ImagePageSize::Fit,
            };
            let margin = option_f64(&options, "margin").unwrap_or(36.0);
            let inputs: Vec<String> = (0..file_count).map(|i| format!("in/{i}")).collect();
            vec![Produced {
                output: "out.pdf".to_string(),
                name: format!("{stem}-images.pdf"),
                verification: selis_pdf_tools::image_to_pdf(
                    &inputs,
                    selis_pdf_tools::ImageToPdfOptions { page_size, margin },
                    "out.pdf",
                )?,
                over_target: false,
            }]
        }
        Op::ImageWatermark => {
            // The image is the second input file, not a path: the browser has
            // no filesystem, so the page hands the bytes over like any other
            // input and the session store resolves them here.
            if file_count < 2 {
                return Err(ToolError(
                    "an image watermark needs the PDF and the image".to_string(),
                ));
            }
            let mut options: selis_pdf_tools::overlay::ImageWatermarkOptions =
                parse_options(&options)?;
            options.image_path = "in/1".to_string();
            vec![Produced {
                output: "out.pdf".to_string(),
                name: format!("{stem}-image-watermarked.pdf"),
                verification: selis_pdf_tools::overlay::add_image_watermark(
                    "in/0", &options, "out.pdf",
                )?,
                over_target: false,
            }]
        }
        Op::Outline => {
            let options: selis_pdf_tools::OutlineOptions = parse_options(&options)?;
            vec![Produced {
                output: "out.pdf".to_string(),
                name: format!("{stem}-outline.pdf"),
                verification: selis_pdf_tools::set_outline("in/0", &options, "out.pdf")?,
                over_target: false,
            }]
        }
        Op::MetadataReport => return run_metadata_report(&stem, first_input_len),
        Op::Fingerprint => return run_fingerprint(&stem, first_input_len),
        Op::Privacy => return run_privacy(&stem, first_input_len),
        Op::Batch => return run_batch_op(&input_files, &options),
        Op::FormFields => return run_form_fields(&stem, first_input_len),
        Op::FillForms => {
            let options: selis_pdf_tools::FillFormsOptions = parse_options(&options)?;
            vec![Produced {
                output: "out.pdf".to_string(),
                name: format!("{stem}-filled.pdf"),
                verification: selis_pdf_tools::fill_forms("in/0", &options, "out.pdf")?,
                over_target: false,
            }]
        }
        Op::EditText => {
            let options: selis_pdf_tools::EditTextOptions = parse_options(&options)?;
            vec![Produced {
                output: "out.pdf".to_string(),
                name: format!("{stem}-edited.pdf"),
                verification: selis_pdf_tools::edit_text(
                    "in/0",
                    options.page,
                    &options.find,
                    &options.replace,
                    "out.pdf",
                )?,
                over_target: false,
            }]
        }
        Op::Redact => {
            // The page is zero-based, like every other page selection in this
            // chunk, and the regions are in the page's own user space.
            let page = option_u64(&options, "page")
                .ok_or_else(|| ToolError("a redaction needs a page".to_string()))?;
            let page = usize::try_from(page)
                .map_err(|_| ToolError("the redaction's page is out of range".to_string()))?;
            let rects = option_rects(&options)?;
            vec![Produced {
                output: "out.pdf".to_string(),
                name: format!("{stem}-redacted.pdf"),
                verification: selis_pdf_tools::redact_page("in/0", page, &rects, "out.pdf")?,
                over_target: false,
            }]
        }
        Op::Zip => {
            let entries: Vec<(&str, &[u8])> = input_files
                .iter()
                .map(|(name, bytes)| (name.as_str(), bytes.as_slice()))
                .collect();
            let zip = selis_pdf_tools::bundle_zip(&entries)?;
            let name = format!("{stem}.zip");
            selis_pdf_tools::write_output("out.zip", &zip)?;
            vec![Produced {
                output: "out.zip".to_string(),
                name: name.clone(),
                verification: selis_pdf_tools::Verification {
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
                    bytes_in: 0,
                    bytes_out: u64::try_from(zip.len()).unwrap_or(u64::MAX),
                    verdict: "ok".to_string(),
                    checked: vec![],
                    repair_mode: None,
                },
                over_target: false,
            }]
        }
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
            "kind": if part.name.ends_with(".zip") { "zip" } else { "pdf" },
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

    Ok((verification, reports, outputs, None))
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

    Ok((verification, reports, outputs, None))
}

/// Read a document's metadata and structure into a JSON report.
///
/// Nothing is rewritten: the only output is the report itself, so the
/// verification counts describe the document as it stands and the size row
/// deliberately shows the same number twice. An encrypted document reports
/// `encrypted` and no counts, because every dictionary it would show is still
/// ciphertext without the file key.
fn run_metadata_report(stem: &str, bytes_in: usize) -> Result<RunOutput, ToolError> {
    let report = selis_pdf_tools::metadata_report("in/0")?;
    let json = report.to_json()?;
    let readable = report.status == "ok";
    let counts = report.counts;
    let held = u64::try_from(bytes_in).unwrap_or(u64::MAX);
    let verification = Verification {
        pages_in: report.pages,
        pages_out: report.pages.unwrap_or(0),
        annotations_in: readable.then_some(counts.annotations),
        annotations_out: counts.annotations,
        fields_in: readable.then_some(counts.form_fields),
        fields_out: counts.form_fields,
        ocgs_in: readable.then_some(counts.layers),
        ocgs_out: counts.layers,
        outline_entries_in: readable.then_some(counts.outline_entries),
        outline_entries_out: counts.outline_entries,
        embedded_files_in: readable.then_some(counts.embedded_files),
        embedded_files_out: counts.embedded_files,
        bytes_in: held,
        bytes_out: held,
        verdict: if readable { "read" } else { "encrypted" }.to_string(),
        checked: if readable {
            vec![
                "pages",
                "annotations",
                "fields",
                "ocgs",
                "outline_entries",
                "embedded_files",
            ]
        } else {
            vec!["metadata"]
        },
        repair_mode: None,
    };
    let name = format!("{stem}-metadata.json");
    let reports = vec![serde_json::json!({
        "name": name,
        "kind": "report",
        "pages": 0,
        "bytes": json.len(),
        "verdict": verification.verdict,
    })];
    Ok((verification, reports, vec![(name, json)], None))
}

/// Report the document's structural fingerprint as JSON.
///
/// Nothing is rewritten: like the metadata report the output is the report
/// itself, never a PDF, so no preview is offered. An encrypted document reports
/// only the envelope and the trailer `/ID`.
fn run_fingerprint(stem: &str, bytes_in: usize) -> Result<RunOutput, ToolError> {
    let report = selis_pdf_tools::fingerprint_report("in/0")?;
    let json = report.to_json()?;
    let readable = report.status == "ok";
    let held = u64::try_from(bytes_in).unwrap_or(u64::MAX);
    let verification = Verification {
        pages_in: report.pages,
        pages_out: report.pages.unwrap_or(0),
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
        bytes_in: held,
        bytes_out: held,
        verdict: if readable { "read" } else { "encrypted" }.to_string(),
        checked: if readable {
            vec!["identifiers", "fonts", "images"]
        } else {
            vec!["identifiers"]
        },
        repair_mode: None,
    };
    let name = format!("{stem}-fingerprint.json");
    let reports = vec![serde_json::json!({
        "name": name,
        "kind": "report",
        "pages": 0,
        "bytes": json.len(),
        "verdict": verification.verdict,
    })];
    Ok((verification, reports, vec![(name, json)], None))
}

/// Scan the document for privacy risks as JSON.
///
/// Nothing is rewritten and nothing is auto-fixed: like the other report ops the
/// output is the report itself, never a PDF, so no preview is offered. An
/// encrypted document reports that it could not be scanned.
fn run_privacy(stem: &str, bytes_in: usize) -> Result<RunOutput, ToolError> {
    let report = selis_pdf_tools::privacy_report("in/0")?;
    let json = report.to_json()?;
    let readable = report.status == "ok";
    let held = u64::try_from(bytes_in).unwrap_or(u64::MAX);
    let verification = Verification {
        pages_in: report.pages,
        pages_out: report.pages.unwrap_or(0),
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
        bytes_in: held,
        bytes_out: held,
        verdict: if readable { "read" } else { "encrypted" }.to_string(),
        checked: vec!["privacy"],
        repair_mode: None,
    };
    let name = format!("{stem}-privacy.json");
    let reports = vec![serde_json::json!({
        "name": name,
        "kind": "report",
        "pages": 0,
        "bytes": json.len(),
        "verdict": verification.verdict,
    })];
    Ok((verification, reports, vec![(name, json)], None))
}

/// Read the document's AcroForm fields into a JSON report.
///
/// Nothing is rewritten: the output is the report itself, so the counts
/// describe the form as it stands. The browser card uses this to render one
/// control per field before the user confirms a fill; a document without a
/// form produces an empty list rather than an error, because "this PDF has no
/// fillable fields" is a fact the page can show.
fn run_form_fields(stem: &str, bytes_in: usize) -> Result<RunOutput, ToolError> {
    let report = selis_pdf_tools::form_fields("in/0")?;
    let json = report.to_json()?;
    let count = u64::try_from(report.fields.len()).unwrap_or(u64::MAX);
    let pages = report.pages;
    let held = u64::try_from(bytes_in).unwrap_or(u64::MAX);
    let verification = Verification {
        pages_in: pages,
        pages_out: pages.unwrap_or(0),
        annotations_in: None,
        annotations_out: 0,
        fields_in: Some(count),
        fields_out: count,
        ocgs_in: None,
        ocgs_out: 0,
        outline_entries_in: None,
        outline_entries_out: 0,
        embedded_files_in: None,
        embedded_files_out: 0,
        bytes_in: held,
        bytes_out: held,
        verdict: "read".to_string(),
        checked: Vec::new(),
        repair_mode: None,
    };
    let name = format!("{stem}-fields.json");
    let reports = vec![serde_json::json!({
        "name": name,
        "kind": "report",
        "pages": 0,
        "bytes": json.len(),
        "verdict": verification.verdict,
    })];
    Ok((verification, reports, vec![(name, json)], None))
}


/// Read the annotations a page carries, as a JSON report.
///
/// The report is a READ-ONLY view: it changes nothing and writes nothing.
fn run_annotations(
	options: &serde_json::Value,
	stem: &str,
	bytes_in: usize,
) -> Result<RunOutput, ToolError> {
	let page = option_u64(options, "page").unwrap_or(0);
	let page = u32::try_from(page).unwrap_or(u32::MAX);
	let json = selis_pdf_tools::annotations("in/0", page)?;
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
		bytes_in: u64::try_from(bytes_in).unwrap_or(u64::MAX),
		bytes_out: u64::try_from(json.len()).unwrap_or(u64::MAX),
		verdict: "read".to_string(),
		checked: Vec::new(),
		repair_mode: None,
	};
	let name = format!("{stem}-annotations.json");
	let reports = vec![serde_json::json!({
		"name": name,
		"kind": "report",
		"pages": 0,
		"bytes": json.len(),
		"verdict": verification.verdict,
	})];
	Ok((
		verification,
		reports,
		vec![(name, json.into_bytes())],
		None,
	))
}

/// Run one tool across every selected document, each file on its own.
///
/// The queue is the only operation whose result is not a single document, so
/// it carries a `batch` object beside the aggregate: one row per file the user
/// picked, in the order they picked them, saying whether that file produced a
/// result or why it did not. A failing file never cancels the others and never
/// contributes bytes to their outputs — the engine handed each row only its own
/// store key.
///
/// Every download is named from its **own** input, because a queue of twelve
/// files whose results are all named after the first is twelve files nobody can
/// tell apart afterwards.
fn run_batch_op(
    files: &[(String, Vec<u8>)],
    options: &serde_json::Value,
) -> Result<RunOutput, ToolError> {
    let requested = option_str(options, "tool")
        .ok_or_else(|| ToolError("batch needs the tool to run".to_string()))?;
    let tool = selis_pdf_tools::BatchTool::from_name(&requested)
        .ok_or_else(|| ToolError(format!("unknown batch tool `{requested}`")))?;
    let jobs: Vec<selis_pdf_tools::BatchJob> = (0..files.len())
        .map(|index| {
            selis_pdf_tools::BatchJob::new(&format!("in/{index}"), &format!("out/{index}"))
        })
        .collect();
    let outcome = selis_pdf_tools::run_batch(tool, &jobs)?;

    let mut taken: Vec<String> = Vec::with_capacity(files.len());
    let mut reports: Vec<serde_json::Value> = Vec::new();
    let mut outputs: Vec<(String, Vec<u8>)> = Vec::new();
    let mut rows: Vec<serde_json::Value> = Vec::new();
    let mut verifications: Vec<Verification> = Vec::new();
    for ((input_name, _), file) in files.iter().zip(&outcome.files) {
        let produced = match (&file.verification, &file.output) {
            (Some(verification), Some(key)) => Some((verification, key)),
            _ => None,
        };
        let Some((verification, key)) = produced else {
            rows.push(serde_json::json!({
                "input": input_name,
                "name": serde_json::Value::Null,
                "status": "failed",
                "error": file.error,
            }));
            continue;
        };
        let base = format!("{}-{}", file_stem(input_name), tool.suffix());
        let name = unique_name(&mut taken, &base, ".pdf");
        let bytes = selis_pdf_tools::take_output(key)
            .ok_or_else(|| ToolError("a verified result vanished from the session".to_string()))?;
        let held = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
        reports.push(serde_json::json!({
            "name": name,
            "kind": "pdf",
            "pages": verification.pages_out,
            "bytes": bytes.len(),
            "verdict": verification.verdict,
        }));
        rows.push(serde_json::json!({
            "input": input_name,
            "name": name,
            "status": "ok",
            "pages": verification.pages_out,
            "bytes": held,
            "verdict": verification.verdict,
        }));
        outputs.push((name, bytes));
        verifications.push(verification.clone());
    }

    if verifications.is_empty() {
        // Nothing succeeded, so there is nothing to download and no result to
        // dress up. The first file's own error is the most useful thing to say.
        let reason = outcome
            .files
            .iter()
            .find_map(|file| file.error.as_deref())
            .unwrap_or("no document could be processed");
        return Err(ToolError(format!(
            "batch: none of the {} files could be processed — {reason}",
            outcome.files.len()
        )));
    }

    if outputs.len() > 1 {
        let entries: Vec<(&str, &[u8])> = outputs
            .iter()
            .map(|(name, bytes)| (name.as_str(), bytes.as_slice()))
            .collect();
        let zip = selis_pdf_tools::bundle_zip(&entries)?;
        let base = format!(
            "{}-batch",
            file_stem(
                &files
                    .first()
                    .map(|(name, _)| name.clone())
                    .unwrap_or_default()
            )
        );
        let zip_name = unique_name(&mut taken, &base, ".zip");
        reports.push(serde_json::json!({
            "name": zip_name,
            "kind": "zip",
            "pages": 0,
            "bytes": zip.len(),
            "verdict": "bundle",
        }));
        outputs.push((zip_name, zip));
    }

    let first = verifications
        .first()
        .ok_or_else(|| ToolError("batch produced no verification".to_string()))?;
    let sum_in = |pick: fn(&Verification) -> Option<u64>| -> Option<u64> {
        if verifications.iter().all(|row| pick(row).is_some()) {
            Some(verifications.iter().fold(0u64, |total, row| {
                total.saturating_add(pick(row).unwrap_or(0))
            }))
        } else {
            None
        }
    };
    let verification = Verification {
        pages_in: sum_in(|row| row.pages_in),
        pages_out: verifications
            .iter()
            .fold(0u64, |total, row| total.saturating_add(row.pages_out)),
        annotations_in: sum_in(|row| row.annotations_in),
        annotations_out: verifications
            .iter()
            .fold(0u64, |total, row| total.saturating_add(row.annotations_out)),
        fields_in: sum_in(|row| row.fields_in),
        fields_out: verifications
            .iter()
            .fold(0u64, |total, row| total.saturating_add(row.fields_out)),
        ocgs_in: sum_in(|row| row.ocgs_in),
        ocgs_out: verifications
            .iter()
            .fold(0u64, |total, row| total.saturating_add(row.ocgs_out)),
        outline_entries_in: sum_in(|row| row.outline_entries_in),
        outline_entries_out: verifications.iter().fold(0u64, |total, row| {
            total.saturating_add(row.outline_entries_out)
        }),
        embedded_files_in: sum_in(|row| row.embedded_files_in),
        embedded_files_out: verifications.iter().fold(0u64, |total, row| {
            total.saturating_add(row.embedded_files_out)
        }),
        // The queue read every file it was given, including the ones that
        // failed; only the documents it actually wrote count as output.
        bytes_in: files.iter().fold(0u64, |total, (_, bytes)| {
            total.saturating_add(u64::try_from(bytes.len()).unwrap_or(u64::MAX))
        }),
        bytes_out: verifications
            .iter()
            .fold(0u64, |total, row| total.saturating_add(row.bytes_out)),
        verdict: if outcome.failed() == 0 {
            "ok".to_string()
        } else {
            "partial".to_string()
        },
        // Only a promise every successful file kept is a promise the batch made.
        checked: first
            .checked
            .iter()
            .filter(|label| verifications.iter().all(|row| row.checked.contains(label)))
            .copied()
            .collect(),
        repair_mode: None,
    };

    let batch = serde_json::json!({
        "tool": outcome.tool,
        "total": outcome.files.len(),
        "ok": outcome.ok(),
        "failed": outcome.failed(),
        "files": rows,
    });
    Ok((verification, reports, outputs, Some(batch)))
}

/// A download name no earlier row of this run has taken.
fn unique_name(taken: &mut Vec<String>, base: &str, extension: &str) -> String {
    let mut candidate = format!("{base}{extension}");
    let mut suffix = 2u32;
    while taken
        .iter()
        .any(|name| name.eq_ignore_ascii_case(&candidate))
    {
        candidate = format!("{base}-{suffix}{extension}");
        suffix = suffix.saturating_add(1);
    }
    taken.push(candidate.clone());
    candidate
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

/// The redaction regions, as `(x, y, width, height)` in the page's user space.
///
/// Every region is four numbers, and a region that is not finite or is
/// degenerate is refused rather than quietly dropped: a redaction that covers
/// nothing is a redaction the reader believes they made and did not.
fn option_rects(options: &serde_json::Value) -> Result<Vec<(f64, f64, f64, f64)>, ToolError> {
    let array = options
        .get("rects")
        .and_then(|value| value.as_array())
        .ok_or_else(|| ToolError("a redaction needs a rects array".to_string()))?;
    if array.is_empty() {
        return Err(ToolError(
            "a redaction needs at least one region".to_string(),
        ));
    }
    let mut rects = Vec::with_capacity(array.len());
    for region in array {
        let parts = region
            .as_array()
            .ok_or_else(|| ToolError("a redaction region must be four numbers".to_string()))?;
        if parts.len() != 4 {
            return Err(ToolError(
                "a redaction region must be x, y, width, height".to_string(),
            ));
        }
        let mut values: Vec<f64> = Vec::with_capacity(4);
        for part in parts {
            let value = part.as_f64().ok_or_else(|| {
                ToolError("a redaction region's numbers must be numbers".to_string())
            })?;
            if !value.is_finite() {
                return Err(ToolError(
                    "a redaction region's numbers must be finite".to_string(),
                ));
            }
            values.push(value);
        }
        let x = values.first().copied().unwrap_or(0.0);
        let y = values.get(1).copied().unwrap_or(0.0);
        let width = values.get(2).copied().unwrap_or(0.0);
        let height = values.get(3).copied().unwrap_or(0.0);
        if width <= 0.0 || height <= 0.0 {
            return Err(ToolError(
                "a redaction region must have a positive width and height".to_string(),
            ));
        }
        rects.push((x, y, width, height));
    }
    Ok(rects)
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

    /// One upright page with a text field, a checkbox and a two-widget radio
    /// group — the smallest document the form ops can edit.
    fn form_pdf() -> Vec<u8> {
        let objects: &[(u32, &[u8])] = &[
            (1, b"<< /Type /Catalog /Pages 2 0 R /AcroForm 5 0 R >>"),
            (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
            (
                3,
                b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] /Annots [6 0 R 7 0 R 10 0 R 11 0 R] >>",
            ),
            (
                5,
                b"<< /Fields [6 0 R 7 0 R 9 0 R] /DA (/Helv 0 Tf 0 g) >>",
            ),
            (
                6,
                b"<< /FT /Tx /T (Name) /V (Old) /Subtype /Widget /Rect [10 10 90 30] /P 3 0 R >>",
            ),
            (
                7,
                b"<< /FT /Btn /T (Agree) /V /Off /Subtype /Widget /Rect [10 40 30 60] /P 3 0 R /AP << /N << /Off 8 0 R /Yes 8 0 R >> >> >>",
            ),
            (
                8,
                b"<< /Type /XObject /Subtype /Form /BBox [0 0 20 20] /Length 0 >>\nstream\n\nendstream",
            ),
            (9, b"<< /FT /Btn /Ff 32768 /T (Size) /V /Small /Kids [10 0 R 11 0 R] >>"),
            (
                10,
                b"<< /Subtype /Widget /Parent 9 0 R /Rect [10 70 25 85] /P 3 0 R /AP << /N << /Off 8 0 R /Small 8 0 R >> >> >>",
            ),
            (
                11,
                b"<< /Subtype /Widget /Parent 9 0 R /Rect [40 70 55 85] /P 3 0 R /AP << /N << /Off 8 0 R /Large 8 0 R >> >> >>",
            ),
        ];
        let max = objects.iter().map(|(num, _)| *num).max().unwrap_or(0);
        let mut out = b"%PDF-1.4\n".to_vec();
        let mut offsets = vec![0usize; max as usize + 1];
        for (num, body) in objects {
            offsets[*num as usize] = out.len();
            out.extend_from_slice(format!("{num} 0 obj\n").as_bytes());
            out.extend_from_slice(body);
            out.extend_from_slice(b"\nendobj\n");
        }
        let xref = out.len();
        let size = max as usize + 1;
        out.extend_from_slice(format!("xref\n0 {size}\n").as_bytes());
        out.extend_from_slice(b"0000000000 65535 f \n");
        for offset in &offsets[1..] {
            out.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
        }
        out.extend_from_slice(
            format!("trailer\n<< /Size {size} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n")
                .as_bytes(),
        );
        out
    }

    #[test]
    fn capabilities_cover_every_operation() {
        // Ops 1-32: the twenty-three object-graph tools, Zip, the image
        // watermark, outline editing, the read-only metadata report, the
        // batch queue, the AcroForm read/fill pair, the structural fingerprint
        // and the privacy scan. Op N sets bit N-1, so 32 ops fill the u32.
        assert_eq!(selis_tools_capabilities(), u32::MAX);
    }

    #[test]
    fn form_fields_and_fill_forms_are_dispatched() {
        let pdf = form_pdf();
        // The read op: one JSON report, no document written.
        let response = run_request(&request(29, br#"{}"#, &[("doc.pdf", &pdf)]));
        let (status, message, outputs) = parse_response(&response);
        assert_eq!(status, 0, "form-fields failed: {message}");
        assert_eq!(outputs.len(), 1);
        assert_eq!(outputs[0].0, "doc-fields.json");
        let json: serde_json::Value = serde_json::from_slice(&outputs[0].1).expect("fields json");
        let names: Vec<&str> = json["fields"]
            .as_array()
            .expect("fields")
            .iter()
            .filter_map(|field| field["name"].as_str())
            .collect();
        assert_eq!(names, ["Name", "Agree", "Size"]);
        assert_eq!(message["fields_out"], 3);
        let reports = message["outputs"].as_array().expect("reports");
        assert_eq!(reports[0]["kind"], "report");
        assert_eq!(reports[0]["verdict"], "read");

        // The write op: the filled document carries the values, and the
        // report says a PDF came out.
        let response = run_request(&request(
            30,
            br#"{"fields":[{"name":"Name","value":"Ada"},{"name":"Agree","value":"Yes"},{"name":"Size","value":"Large"}]}"#,
            &[("doc.pdf", &pdf)],
        ));
        let (status, message, outputs) = parse_response(&response);
        assert_eq!(status, 0, "fill-forms failed: {message}");
        assert_eq!(outputs.len(), 1);
        assert_eq!(outputs[0].0, "doc-filled.pdf");
        let text = String::from_utf8_lossy(&outputs[0].1);
        assert!(text.contains("(Ada)"), "filled value written");
        assert!(text.contains("/V /Large"), "filled radio state written");
        assert!(text.contains("/NeedAppearances true"), "appearances asked");
        assert_eq!(message["verdict"], "ok");
        assert_eq!(message["fields_out"], 5);
        let reports = message["outputs"].as_array().expect("reports");
        assert_eq!(reports[0]["kind"], "pdf");
    }

    #[test]
    fn fill_forms_refuses_an_unknown_field() {
        let pdf = form_pdf();
        let response = run_request(&request(
            30,
            br#"{"fields":[{"name":"Nope","value":"x"}]}"#,
            &[("doc.pdf", &pdf)],
        ));
        // A refusal goes down the plain-text error path, not the JSON one, so
        // walk the response by hand to its output count.
        let status = u32::from_le_bytes(response[4..8].try_into().expect("status"));
        assert_eq!(status, 1, "a refused fill signals an error status");
        let message_len =
            u32::from_le_bytes(response[8..12].try_into().expect("message len")) as usize;
        let outputs_offset = 12usize.saturating_add(message_len);
        let outputs = u32::from_le_bytes(
            response[outputs_offset..outputs_offset + 4]
                .try_into()
                .expect("output count"),
        );
        assert_eq!(outputs, 0, "a refused fill produces no downloadable file");
        assert!(
            response.windows(4).any(|window| window == b"Nope"),
            "the refusal names the field"
        );
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

        // Op 36 is the annotations report, so the probe moves past it.
        let response = run_request(&request(37, b"", &[("doc.pdf", &pdf)]));
        assert!(error_text(&response).contains("unknown tool operation"));
    }

    #[test]
    fn metadata_report_operation_is_dispatched() {
        let pdf = metadata_pdf();
        let response = run_request(&request(27, b"", &[("doc.pdf", &pdf)]));
        let (status, message, outputs) = parse_response(&response);
        assert_eq!(status, 0, "metadata report failed: {message}");
        assert_eq!(message["verdict"], "read");
        assert_eq!(message["outputs"][0]["kind"], "report");
        assert_eq!(outputs[0].0, "doc-metadata.json");
        let report: serde_json::Value =
            serde_json::from_slice(&outputs[0].1).expect("the report is JSON");
        assert_eq!(report["status"], "ok");
        assert_eq!(report["pdf_version"], "1.4");
        assert_eq!(report["encrypted"], false);
        assert_eq!(report["revisions"], 1);
        assert_eq!(report["pages"], 1);
        assert_eq!(report["page_size"]["width"], 100.0);
        assert_eq!(report["info"][0]["key"], "Author");
        assert_eq!(report["info"][0]["value"], "Selis");
        assert_eq!(report["info"][1]["key"], "Title");
        assert_eq!(report["info"][1]["value"], "A fixture");
        assert_eq!(report["xmp"]["present"], true);
        assert_eq!(report["xmp"]["decoded"], true);
        assert!(report["xmp"]["text"]
            .as_str()
            .unwrap_or_default()
            .contains("xmpmeta"));
        assert_eq!(report["counts"]["annotations"], 0);
        assert_eq!(report["counts"]["embedded_files"], 0);
    }

    #[test]
    fn fingerprint_operation_is_dispatched() {
        let pdf = metadata_pdf();
        let response = run_request(&request(31, b"", &[("doc.pdf", &pdf)]));
        let (status, message, outputs) = parse_response(&response);
        assert_eq!(status, 0, "fingerprint failed: {message}");
        assert_eq!(message["verdict"], "read");
        assert_eq!(message["outputs"][0]["kind"], "report");
        assert_eq!(outputs[0].0, "doc-fingerprint.json");
        let report: serde_json::Value =
            serde_json::from_slice(&outputs[0].1).expect("the report is JSON");
        assert_eq!(report["status"], "ok");
        assert_eq!(report["pdf_version"], "1.4");
        assert!(
            report["ids"].is_object(),
            "the trailer /ID pair is reported"
        );
        assert!(report["fonts"].is_array(), "fonts are an array");
        assert!(report["images"].is_array(), "images are an array");
    }

    #[test]
    fn privacy_operation_is_dispatched() {
        let pdf = metadata_pdf();
        let response = run_request(&request(32, b"", &[("doc.pdf", &pdf)]));
        let (status, message, outputs) = parse_response(&response);
        assert_eq!(status, 0, "privacy scan failed: {message}");
        assert_eq!(message["verdict"], "read");
        assert_eq!(message["outputs"][0]["kind"], "report");
        assert_eq!(outputs[0].0, "doc-privacy.json");
        let report: serde_json::Value =
            serde_json::from_slice(&outputs[0].1).expect("the report is JSON");
        assert_eq!(report["status"], "ok");
        assert_eq!(report["schema"], "selis-privacy/1");
        assert!(report["findings"].is_array(), "findings are an array");
        assert!(report["risk"].is_string(), "an overall risk is reported");
    }

    /// The queue is the one operation whose answer is not a single document, so
    /// the response carries a row per file: what happened to each one, in the
    /// order the user picked them, with the failure isolated to its own row.
    #[test]
    fn batch_operation_runs_one_tool_over_every_file_and_keeps_the_good_ones() {
        let first = multipage_pdf(2);
        let second = multipage_pdf(3);
        let response = run_request(&request(
            28,
            br#"{"tool":"compress"}"#,
            &[
                ("alpha.pdf", &first),
                ("beta.pdf", &second),
                ("broken.pdf", b"this is not a PDF"),
            ],
        ));
        let (status, message, outputs) = parse_response(&response);
        assert_eq!(status, 0, "batch failed: {message}");
        assert_eq!(message["verdict"], "partial", "one file did not run");
        assert_eq!(message["batch"]["tool"], "compress");
        assert_eq!(message["batch"]["total"], 3);
        assert_eq!(message["batch"]["ok"], 2);
        assert_eq!(message["batch"]["failed"], 1);

        // Each result is measured from its own document: two pages and three
        // pages, never a merged or averaged count across the queue.
        assert_eq!(message["outputs"][0]["name"], "alpha-compressed.pdf");
        assert_eq!(message["outputs"][0]["pages"], 2);
        assert_eq!(message["outputs"][1]["name"], "beta-compressed.pdf");
        assert_eq!(message["outputs"][1]["pages"], 3);
        assert_eq!(message["outputs"][2]["kind"], "zip");
        assert_eq!(outputs.len(), 3, "two PDFs and the bundle");
        assert_eq!(outputs[0].0, "alpha-compressed.pdf");
        assert_eq!(&outputs[0].1[0..5], b"%PDF-");
        assert_eq!(&outputs[2].1[0..2], b"PK", "the bundle is a real ZIP");

        let rows = message["batch"]["files"].as_array().expect("rows");
        assert_eq!(rows.len(), 3, "one row per picked file");
        assert_eq!(rows[0]["status"], "ok");
        assert_eq!(rows[0]["input"], "alpha.pdf");
        assert_eq!(rows[1]["status"], "ok");
        assert_eq!(rows[2]["status"], "failed");
        assert_eq!(rows[2]["input"], "broken.pdf");
        assert!(
            rows[2]["name"].is_null(),
            "a failed file has no download to name"
        );
        assert!(
            rows[2]["error"]
                .as_str()
                .unwrap_or_default()
                .contains("in/2"),
            "the row names the file that failed: {}",
            rows[2]["error"]
        );
        assert_eq!(message["pages_out"], 5, "2 + 3 pages, not 2 × 3 files");
    }

    /// Two files with the same name are two downloads, not one overwritten
    /// result: each carries its own content.
    #[test]
    fn batch_downloads_are_named_after_their_own_file_and_never_share_a_name() {
        let two = multipage_pdf(2);
        let three = multipage_pdf(3);
        let response = run_request(&request(
            28,
            br#"{"tool":"strip-metadata"}"#,
            &[("report.pdf", &two), ("report.pdf", &three)],
        ));
        let (status, message, outputs) = parse_response(&response);
        assert_eq!(status, 0, "batch failed: {message}");
        assert_eq!(outputs[0].0, "report-clean.pdf");
        assert_eq!(outputs[1].0, "report-clean-2.pdf");
        assert_eq!(message["outputs"][0]["pages"], 2);
        assert_eq!(message["outputs"][1]["pages"], 3);
        assert_eq!(outputs[2].0, "report-batch.zip");
        let names: Vec<&str> = outputs.iter().map(|(name, _)| name.as_str()).collect();
        let unique: std::collections::HashSet<&str> = names.iter().copied().collect();
        assert_eq!(unique.len(), names.len(), "{names:?}");
    }

    /// A queue where nothing worked is an error, not a green run with an empty
    /// download list: the page must say what went wrong rather than show zero.
    #[test]
    fn a_batch_where_every_file_fails_reports_the_first_reason() {
        let response = run_request(&request(
            28,
            br#"{"tool":"compress"}"#,
            &[("a.bin", b"not a PDF"), ("b.bin", b"also not a PDF")],
        ));
        let text = error_text(&response);
        assert!(text.contains("none of the 2 files"), "{text}");
        assert!(text.contains("in/0"), "the first reason is quoted: {text}");
    }

    /// The tool is chosen, never guessed: an unknown or absent name is refused
    /// before a single document is opened, so a stale page cannot run a
    /// different tool over the user's files.
    #[test]
    fn a_batch_refuses_an_unknown_or_missing_tool_name() {
        let pdf = multipage_pdf(1);
        let response = run_request(&request(28, br#"{"tool":"protect"}"#, &[("doc.pdf", &pdf)]));
        assert!(
            error_text(&response).contains("unknown batch tool `protect`"),
            "encryption is not a browser batch tool"
        );
        let response = run_request(&request(28, b"{}", &[("doc.pdf", &pdf)]));
        assert!(error_text(&response).contains("needs the tool"));
    }

    #[test]
    fn metadata_report_leaves_the_document_bytes_alone() {
        let pdf = metadata_pdf();
        let response = run_request(&request(27, b"", &[("doc.pdf", &pdf)]));
        let (status, message, outputs) = parse_response(&response);
        assert_eq!(status, 0, "metadata report failed: {message}");
        // The verification size row is the document, not the report: a read
        // cannot make the input larger or smaller.
        assert_eq!(
            message["bytes_in"],
            u64::try_from(pdf.len()).unwrap_or(u64::MAX)
        );
        assert_eq!(message["bytes_out"], message["bytes_in"]);
        assert_eq!(&outputs[0].1[0..1], b"{", "the output is JSON, not a PDF");
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

    /// A 1x1 RGBA PNG (colour type 6) used by the image leg. The bytes are a
    /// fixed encoder output, so the test does not depend on an image crate
    /// feature being enabled in this build.
    const ONE_PNG: [u8; 120] = [
        0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44,
        0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1f,
        0x15, 0xc4, 0x89, 0x00, 0x00, 0x00, 0x01, 0x73, 0x52, 0x47, 0x42, 0x00, 0xae, 0xce, 0x1c,
        0xe9, 0x00, 0x00, 0x00, 0x04, 0x67, 0x41, 0x4d, 0x41, 0x00, 0x00, 0xb1, 0x8f, 0x0b, 0xfc,
        0x61, 0x05, 0x00, 0x00, 0x00, 0x09, 0x70, 0x48, 0x59, 0x73, 0x00, 0x00, 0x0e, 0xc3, 0x00,
        0x00, 0x0e, 0xc3, 0x01, 0xc7, 0x6f, 0xa8, 0x64, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x44, 0x41,
        0x54, 0x18, 0x57, 0x63, 0xe0, 0x12, 0x91, 0xfb, 0x0f, 0x00, 0x01, 0xa4, 0x01, 0x3c, 0xbc,
        0x4e, 0xa8, 0xf7, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4e, 0x44, 0xae, 0x42, 0x60, 0x82,
    ];

    /// A 2x2 RGB PNG, built here so the test owns its bytes.
    fn watermark_png() -> Vec<u8> {
        let mut png = vec![0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
        let mut push_chunk = |kind: &[u8; 4], data: &[u8]| {
            png.extend_from_slice(&u32::try_from(data.len()).unwrap().to_be_bytes());
            let start = png.len();
            png.extend_from_slice(kind);
            png.extend_from_slice(data);
            let mut crc = 0xffff_ffffu32;
            for byte in &png[start..] {
                crc ^= u32::from(*byte);
                for _ in 0..8 {
                    let mask = (crc & 1).wrapping_neg();
                    crc = (crc >> 1) ^ (0xedb8_8320 & mask);
                }
            }
            png.extend_from_slice(&(!crc).to_be_bytes());
        };
        let mut ihdr = Vec::new();
        ihdr.extend_from_slice(&2u32.to_be_bytes());
        ihdr.extend_from_slice(&2u32.to_be_bytes());
        ihdr.extend_from_slice(&[8, 2, 0, 0, 0]);
        push_chunk(b"IHDR", &ihdr);
        let scanlines = vec![0u8, 255, 0, 0, 0, 0, 255, 0, 0, 0, 255, 255, 0, 0];
        // A zlib stream of stored (uncompressed) deflate blocks: no dependency
        // on the filter crate, and still a valid PNG for the decoder.
        let mut zlib = vec![0x78, 0x01];
        let mut remaining = scanlines.as_slice();
        loop {
            let take = remaining.len().min(0xffff);
            let last = take == remaining.len();
            zlib.push(u8::from(last));
            zlib.extend_from_slice(&u16::try_from(take).unwrap().to_le_bytes());
            zlib.extend_from_slice(&(!u16::try_from(take).unwrap()).to_le_bytes());
            zlib.extend_from_slice(&remaining[..take]);
            remaining = &remaining[take..];
            if last {
                break;
            }
        }
        let (mut a, mut b) = (1u32, 0u32);
        for byte in &scanlines {
            a = (a + u32::from(*byte)) % 65521;
            b = (b + a) % 65521;
        }
        zlib.extend_from_slice(&((b << 16) | a).to_be_bytes());
        push_chunk(b"IDAT", &zlib);
        push_chunk(b"IEND", &[]);
        png
    }
    #[test]
    fn image_watermark_operation_is_dispatched() {
        let pdf = multipage_pdf(2);
        let png = watermark_png();
        let response = run_request(&request(
            25,
            br#"{"opacity":1.0,"scale":0.5,"pages":"1"}"#,
            &[("doc.pdf", &pdf), ("mark.png", &png)],
        ));
        let (status, message, outputs) = parse_response(&response);
        assert_eq!(status, 0, "image watermark failed: {message}");
        assert_eq!(message["pages_out"], 2);
        assert_eq!(outputs[0].0, "doc-image-watermarked.pdf");
        let text = String::from_utf8_lossy(&outputs[0].1).to_string();
        assert_eq!(text.matches("/Subtype /Image").count(), 1);
        assert!(text.contains("/ColorSpace /DeviceRGB"));

        // The image is required: a PDF on its own is an error, not a silent
        // no-op that returns the input unchanged.
        let response = run_request(&request(25, b"{}", &[("doc.pdf", &pdf)]));
        assert!(error_text(&response).contains("needs the PDF and the image"));
    }

    /// A one-page PDF whose content stream draws one line of text, so a
    /// redaction has something to remove.
    fn text_pdf() -> Vec<u8> {
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
    fn redact_operation_is_dispatched() {
        let pdf = text_pdf();
        let response = run_request(&request(
            34,
            br#"{"page":0,"rects":[[10,150,160,24]]}"#,
            &[("doc.pdf", &pdf)],
        ));
        let (status, message, outputs) = parse_response(&response);
        assert_eq!(status, 0, "redact failed: {message}");
        assert_eq!(message["pages_out"], 1);
        assert_eq!(outputs[0].0, "doc-redacted.pdf");
        let text = String::from_utf8_lossy(&outputs[0].1).to_string();
        assert!(
            !text.contains("Invoice total"),
            "the text under the region is removed from the file, not just covered"
        );
        assert!(
            text.contains(" re") || text.contains("0 0 0 rg"),
            "an opaque box is drawn over the region"
        );

        // A page outside the document is refused.
        let response = run_request(&request(
            34,
            br#"{"page":9,"rects":[[0,0,10,10]]}"#,
            &[("doc.pdf", &pdf)],
        ));
        assert!(error_text(&response).contains("out of range"));

        // A region with no size is refused: a redaction that covers nothing is
        // a redaction the reader believes they made.
        let response = run_request(&request(
            34,
            br#"{"page":0,"rects":[[0,0,0,0]]}"#,
            &[("doc.pdf", &pdf)],
        ));
        assert!(error_text(&response).contains("positive width and height"));
    }

    #[test]
    fn outline_operation_is_dispatched() {
        let pdf = multipage_pdf(3);
        let response = run_request(&request(
            26,
            br#"{"entries":[{"title":"Chapter 1","page":1,"level":0},{"title":"Section","page":2,"level":1},{"title":"Chapter 2","page":3,"level":0}]}"#,
            &[("doc.pdf", &pdf)],
        ));
        let (status, message, outputs) = parse_response(&response);
        assert_eq!(status, 0, "outline failed: {message}");
        assert_eq!(message["pages_out"], 3);
        assert_eq!(message["outline_entries_out"], 3);
        assert_eq!(outputs[0].0, "doc-outline.pdf");
        let text = String::from_utf8_lossy(&outputs[0].1).to_string();
        assert!(text.contains("/Type /Outlines"));
        assert!(text.contains("(Chapter 1)"));
        assert!(text.contains("(Section)"));
        assert!(text.contains("(Chapter 2)"));

        // An out-of-range page is a typed error, not a silent clamp.
        let response = run_request(&request(
            26,
            br#"{"entries":[{"title":"Past the end","page":9,"level":0}]}"#,
            &[("doc.pdf", &pdf)],
        ));
        assert!(error_text(&response).contains("document has 3"));
    }

    #[test]
    fn image_to_pdf_operation_is_dispatched() {
        let response = run_request(&request(
            23,
            br#"{"page_size":"a4","margin":18}"#,
            &[("photo.png", &ONE_PNG), ("second.png", &ONE_PNG)],
        ));
        let (status, message, outputs) = parse_response(&response);
        assert_eq!(status, 0, "image to pdf failed: {message}");
        assert_eq!(message["pages_out"], 2);
        assert_eq!(outputs[0].0, "photo-images.pdf");
        assert_eq!(&outputs[0].1[0..5], b"%PDF-");
        let text = String::from_utf8_lossy(&outputs[0].1);
        assert!(text.contains("/Subtype /Image"));
        assert!(text.contains("/Filter /FlateDecode"));
        assert!(text.contains("595.276"), "the A4 page width is written");
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
        let single = String::from_utf8_lossy(&outputs[0].1)
            .matches("(CONFIDENTIAL) Tj")
            .count();

        let response = run_request(&request(
            8,
            br#"{"text":"CONFIDENTIAL","opacity":0.3,"tiled":true}"#,
            &[("doc.pdf", &pdf)],
        ));
        let (status, message, outputs) = parse_response(&response);
        assert_eq!(status, 0, "tiled watermark failed: {message}");
        let tiled = String::from_utf8_lossy(&outputs[0].1)
            .matches("(CONFIDENTIAL) Tj")
            .count();
        assert!(
            tiled > single,
            "the tiled option must repeat the line (single = {single}, tiled = {tiled})"
        );

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
    fn compress_lossy_level_shrinks_a_scanned_pdf() {
        let pdf = scanned_pdf();
        let in_len = pdf.len();
        let response = run_request(&request(
            11,
            br#"{"level":"recommended"}"#,
            &[("scan.pdf", &pdf)],
        ));
        let (status, message, outputs) = parse_response(&response);
        assert_eq!(status, 0, "compress failed: {message}");
        assert_eq!(message["verdict"], "ok");
        assert_eq!(outputs[0].0, "scan-compressed.pdf");
        let out_len = outputs[0].1.len();
        assert!(out_len < in_len, "lossy output is smaller");
        assert_eq!(message["outputs"][0]["overTarget"], false);
        let text = String::from_utf8_lossy(&outputs[0].1);
        assert!(text.contains("DCTDecode"), "image became a JPEG stream");
    }

    #[test]
    fn compress_over_target_is_reported() {
        let pdf = scanned_pdf();
        let response = run_request(&request(
            11,
            br#"{"level":"extreme","targetKiB":1}"#,
            &[("scan.pdf", &pdf)],
        ));
        let (status, message, outputs) = parse_response(&response);
        assert_eq!(status, 0, "compress failed: {message}");
        assert_eq!(message["outputs"][0]["overTarget"], true);
        let out_len = outputs[0].1.len();
        let in_len = pdf.len();
        assert!(
            out_len < in_len,
            "an unreachable target still never grows the file"
        );
    }

    #[test]
    fn compress_rejects_an_unknown_level() {
        let pdf = multipage_pdf(1);
        let response = run_request(&request(11, br#"{"level":"bogus"}"#, &[("doc.pdf", &pdf)]));
        assert!(
            error_text(&response).contains("level"),
            "the typed error names the level"
        );
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

    /// A one-page document carrying an `/Info` dictionary and an XMP packet:
    /// the two places a metadata report reads its text from.
    fn metadata_pdf() -> Vec<u8> {
        let xmp: &[u8] = br#"<?xpacket begin="" id="W5M0MpCehiHzreSzNTczkc9d"?><x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"><rdf:Description rdf:about=""/></rdf:RDF></x:xmpmeta><?xpacket end="w"?>""#;
        let packet = format!(
            "5 0 obj\n<< /Subtype /XML /Length {} >>\nstream\n",
            xmp.len()
        );
        let mut metadata = packet.into_bytes();
        metadata.extend_from_slice(xmp);
        metadata.extend_from_slice(b"\nendstream\nendobj\n");
        let objects: Vec<Vec<u8>> = vec![
            b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R /Metadata 5 0 R >>\nendobj\n".to_vec(),
            b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n".to_vec(),
            b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] >>\nendobj\n".to_vec(),
            b"4 0 obj\n<< /Title (A fixture) /Author (Selis) >>\nendobj\n".to_vec(),
            metadata,
        ];
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
            format!(
                "trailer\n<< /Size {size} /Root 1 0 R /Info 4 0 R >>\nstartxref\n{xref}\n%%EOF\n"
            )
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

    /// A single-page scanned-style PDF: a raw RGB noise image XObject that
    /// Flate cannot shrink but a low-quality JPEG can.
    fn scanned_pdf() -> Vec<u8> {
        let mut samples = Vec::with_capacity(200 * 150 * 3);
        let mut state = 0x1234_5678u32;
        for _ in 0..(200 * 150 * 3) {
            state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            samples.push(u8::try_from(state >> 24).expect("high byte fits u8"));
        }
        let content = b"q 200 0 0 150 0 0 cm /Im0 Do Q\n";
        let mut objects: Vec<Vec<u8>> = vec![
            b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n".to_vec(),
            b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n".to_vec(),
            b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 150] /Resources << /XObject << /Im0 5 0 R >> >> /Contents 4 0 R >>\nendobj\n".to_vec(),
        ];
        let mut obj4 = format!("4 0 obj\n<< /Length {} >>\nstream\n", content.len()).into_bytes();
        obj4.extend_from_slice(content);
        obj4.extend_from_slice(b"\nendstream\nendobj\n");
        objects.push(obj4);
        let mut obj5 = format!(
            "5 0 obj\n<< /Type /XObject /Subtype /Image /Width 200 /Height 150 /ColorSpace /DeviceRGB /BitsPerComponent 8 /Length {} >>\nstream\n",
            samples.len()
        )
        .into_bytes();
        obj5.extend_from_slice(&samples);
        obj5.extend_from_slice(b"\nendstream\nendobj\n");
        objects.push(obj5);

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
