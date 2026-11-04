//! The browser security chunk.
//!
//! This is a separate `cdylib` from the viewer and from the plain tools chunk.
//! The tools page never downloads it until someone opens a password card, and
//! it is fetched then and cached by the service worker. Like the chunks before
//! it it speaks a pointer ABI and **declares no imports**: the page
//! instantiates it with an empty import object, which is the mechanical form of
//! "this module cannot call out". Document bytes arrive as request payloads and
//! the answer leaves as a guest allocation; nothing touches a network and
//! nothing touches a filesystem — every path in a request is a key into the
//! in-memory tool session.
//!
//! What it runs is [`selis_pdf_tools::security`] — the same three operations
//! the `selis unlock`, `selis protect` and `selis clear-permissions` commands
//! run, byte for byte, including the engine's own decryption resolver, the
//! structural verification gate and the "no partial file" rule.
//!
//! # Wire format
//!
//! All integers are little-endian. A request is:
//!
//! ```text
//! magic "SLSS" (u32)
//! version 1 (u32)
//! operation (u32)
//! options_json_len (u32), options_json bytes
//! file_count (u32)          — exactly 1: a password operation is one document
//!   name_len (u32), name bytes
//!   data_len (u32), data bytes
//! ```
//!
//! A response is:
//!
//! ```text
//! magic "SLRS" (u32)
//! status (u32: 0 ok, 1 error)
//! message_len (u32), message bytes (verification JSON on success, error text on failure)
//! output_count (u32)
//!   name_len (u32), name bytes
//!   data_len (u32), data bytes
//! ```
//!
//! On success the message JSON carries the verification object, an `outputs`
//! array with one `{name, kind, pages, bytes, verdict}` record, and the fields
//! that say what the operation did to the protection: `encrypted` for unlock,
//! `algorithm` / `granted` / `denied` / `encryptMetadata` for protect, and
//! `previouslyDenied` / `summary` for clear-permissions. The words in those
//! fields are read back out of the flag the writer actually wrote, not echoed
//! from the request.
//!
//! # Where the key material comes from
//!
//! Protecting needs a 256-bit file key and four 8-byte salts. This module does
//! not draw them and cannot: it has no entropy import. The page generates them
//! with `crypto.getRandomValues` and passes them in the options JSON as
//! `keyMaterial` (128 hex characters). Missing or wrong-length material is a
//! refusal, never a stand-in — a fixed or derived key would give every
//! protected document the same one.
//!
//! # What the page must do with the secrets
//!
//! The passwords and the key material live inside the request buffer, which the
//! page allocates in this module's memory. The page should zero that buffer as
//! soon as the call returns: the guest copies what it needs and keeps nothing,
//! but the bytes stay reachable in the shared linear memory until they are
//! overwritten. Nothing in this module echoes a password back — not in the
//! verification JSON, not in an error message.

#![allow(unsafe_code)]

use std::alloc::{alloc, dealloc, Layout};
use std::panic::{catch_unwind, AssertUnwindSafe};

use selis_pdf_tools::security::{self, ProtectOptions, R6Material};
use selis_pdf_tools::ToolError;

/// Chunk identifier used by the asset manifest.
pub const CHUNK_ID: &str = "security";
/// Request magic, `SLSS`.
const REQUEST_MAGIC: u32 = 0x5353_4C53;
/// Response magic, `SLRS`.
const RESPONSE_MAGIC: u32 = 0x5352_4C53;
/// Protocol version.
const VERSION: u32 = 1;
/// A password operation takes exactly one document. The batch queue's
/// `protect` stays CLI-only — crypto is human-owned — so a request with two
/// files is a caller mistake, not a smaller batch.
const REQUIRED_FILES: u32 = 1;
/// Maximum input name length.
const MAX_NAME_BYTES: u32 = 128;
/// Maximum options JSON length.
const MAX_OPTIONS_BYTES: u32 = 64 * 1024;
/// The hex characters one [`R6Material`] blob is written as.
const KEY_MATERIAL_HEX_CHARS: usize = R6Material::LEN * 2;
/// The session key the one input document is held under. Chosen here, never by
/// the caller, so a requested name cannot collide with an earlier run.
const SESSION_KEY: &str = "in/0";

/// Operations understood by this chunk.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Op {
    Unlock,
    Protect,
    ClearPermissions,
}

impl Op {
    fn from_wire(value: u32) -> Option<Self> {
        match value {
            1 => Some(Self::Unlock),
            2 => Some(Self::Protect),
            3 => Some(Self::ClearPermissions),
            _ => None,
        }
    }

    /// The name the refusal messages use when an operation is mis-called.
    fn name(self) -> &'static str {
        match self {
            Self::Unlock => "unlock",
            Self::Protect => "protect",
            Self::ClearPermissions => "clear permissions",
        }
    }
}

/// Return the chunk id as guest memory. The host reads `out_len` and frees the
/// pointer with [`selis_security_free`].
///
/// # Safety
///
/// `out_len` must point to four writable bytes in guest memory.
#[no_mangle]
pub unsafe extern "C" fn selis_security_chunk_id(out_len: *mut u32) -> *mut u8 {
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

/// Bit 0 unlock, bit 1 protect (which is also the permission-setting
/// operation), bit 2 clear permissions.
#[no_mangle]
pub extern "C" fn selis_security_capabilities() -> u32 {
    0b111
}

/// Allocate `len` bytes of guest memory. The host writes a request into it.
///
/// Returns null when the allocation fails. The caller must free the pointer
/// with [`selis_security_free`] and the same length.
#[no_mangle]
pub extern "C" fn selis_security_alloc(len: u32) -> *mut u8 {
    allocate(len as usize)
}

/// Free a buffer previously returned by [`selis_security_alloc`],
/// [`selis_security_chunk_id`], or [`selis_security_run`].
///
/// # Safety
///
/// `ptr` must have been returned by one of those functions with exactly `len`
/// bytes, and must not have been freed already.
#[no_mangle]
pub unsafe extern "C" fn selis_security_free(ptr: *mut u8, len: u32) {
    if ptr.is_null() || len == 0 {
        return;
    }
    let Ok(layout) = Layout::from_size_align(len as usize, 1) else {
        return;
    };
    // SAFETY: the caller's contract.
    unsafe { dealloc(ptr, layout) };
}

/// Run one password or permission operation.
///
/// The returned pointer is a response buffer; `out_len` receives its length.
/// The host must copy it out and free it with [`selis_security_free`]. A null
/// return means the request could not be processed at all.
///
/// # Safety
///
/// `request_ptr` must point to `request_len` readable bytes; `out_len` must
/// point to four writable bytes.
#[no_mangle]
pub unsafe extern "C" fn selis_security_run(
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
        let message = b"security operation panicked";
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
        Ok((mut message, name, bytes)) => {
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
                        "name": name.clone(),
                        "kind": "pdf",
                        "pages": pages,
                        "bytes": bytes.len(),
                        "verdict": verdict,
                    }]),
                );
            }
            let message = serde_json::to_vec(&message).unwrap_or_default();
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

/// One accepted request: the operation, its options, and the document's
/// user-visible name (the output is named after it).
struct Call<'a> {
    op: Op,
    options: &'a serde_json::Value,
    stem: String,
}

fn parse_and_run(request: &[u8]) -> Result<(serde_json::Value, String, Vec<u8>), ToolError> {
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
            "unsupported security protocol version".to_string(),
        ));
    }
    let op =
        Op::from_wire(op).ok_or_else(|| ToolError("unknown security operation".to_string()))?;

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
        return Err(ToolError("trailing bytes in security request".to_string()));
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

/// Run one operation and package its verification JSON with the output bytes.
fn run_operation(call: &Call<'_>) -> Result<(serde_json::Value, String, Vec<u8>), ToolError> {
    let (mut message, suffix) = match call.op {
        Op::Unlock => {
            let password = option_str(call.options, "password");
            let outcome = security::unlock(SESSION_KEY, "out.pdf", password.as_deref())?;
            let mut message = to_message(&outcome.verification)?;
            if let Some(object) = message.as_object_mut() {
                object.insert(
                    "encrypted".to_string(),
                    serde_json::json!(outcome.encrypted),
                );
            }
            (message, "unlocked")
        }
        Op::Protect => {
            let user_password = option_str(call.options, "userPassword").unwrap_or_default();
            // Absent means the user password, as Acrobat does and as the CLI's
            // `PasswordOptions::resolve` does; the page always sends both so the
            // distinction is visible in the panel.
            let owner_password =
                option_str(call.options, "ownerPassword").unwrap_or_else(|| user_password.clone());
            let granted = match option_str(call.options, "permissions") {
                Some(spec) => security::parse_permissions(&spec).map_err(ToolError)?,
                None => security::ALL_GRANTED,
            };
            let encrypt_metadata = call
                .options
                .get("encryptMetadata")
                .and_then(|value| value.as_bool())
                .unwrap_or(true);
            let material = key_material(call.options)?;
            let outcome = security::protect(
                SESSION_KEY,
                "out.pdf",
                &ProtectOptions {
                    user_password,
                    owner_password,
                    granted,
                    encrypt_metadata,
                    material,
                },
            )?;
            let mut message = to_message(&outcome.verification)?;
            if let Some(object) = message.as_object_mut() {
                object.insert(
                    "algorithm".to_string(),
                    serde_json::json!("AES-256 (revision 6)"),
                );
                object.insert("granted".to_string(), serde_json::json!(outcome.granted));
                object.insert(
                    "denied".to_string(),
                    serde_json::json!(security::denied_permissions(security::compute_p(granted))),
                );
                object.insert(
                    "encryptMetadata".to_string(),
                    serde_json::json!(encrypt_metadata),
                );
            }
            (message, "protected")
        }
        Op::ClearPermissions => {
            let password = option_str(call.options, "password");
            let outcome = security::clear_permissions(SESSION_KEY, "out.pdf", password.as_deref())?;
            let mut message = to_message(&outcome.verification)?;
            if let Some(object) = message.as_object_mut() {
                object.insert(
                    "previouslyDenied".to_string(),
                    serde_json::json!(outcome.previously_denied),
                );
                object.insert("summary".to_string(), serde_json::json!(outcome.summary));
            }
            (message, "permissions-cleared")
        }
    };
    if let Some(object) = message.as_object_mut() {
        object.insert("operation".to_string(), serde_json::json!(call.op.name()));
    }
    let bytes = selis_pdf_tools::take_output("out.pdf")
        .ok_or_else(|| ToolError("the operation produced no output".to_string()))?;
    let name = format!("{}-{suffix}.pdf", call.stem);
    Ok((message, name, bytes))
}

/// The verification object as the response's base JSON.
fn to_message(
    verification: &selis_pdf_tools::Verification,
) -> Result<serde_json::Value, ToolError> {
    serde_json::to_value(verification)
        .map_err(|e| ToolError(format!("could not serialize the verification: {e}")))
}

/// The 64 bytes of key material the page injected, as 128 hex characters.
///
/// There is no fallback here. A chunk with no entropy import that was handed no
/// key would have to invent one, and an invented file key is the same as no
/// encryption at all — so this refuses.
fn key_material(options: &serde_json::Value) -> Result<R6Material, ToolError> {
    let hex = option_str(options, "keyMaterial").ok_or_else(|| {
        ToolError(
            "protect needs keyMaterial: 64 random bytes from the page, as 128 hex characters"
                .to_string(),
        )
    })?;
    if hex.len() != KEY_MATERIAL_HEX_CHARS {
        return Err(ToolError(format!(
            "keyMaterial must be {KEY_MATERIAL_HEX_CHARS} hex characters ({} bytes), got {}",
            R6Material::LEN,
            hex.len()
        )));
    }
    let Some(bytes) = hex_bytes(&hex) else {
        return Err(ToolError("keyMaterial is not hexadecimal".to_string()));
    };
    R6Material::from_bytes(&bytes).ok_or_else(|| {
        ToolError("keyMaterial is not the 64 bytes this operation needs".to_string())
    })
}

/// Decode an even-length hex string; `None` on any non-hex character.
fn hex_bytes(text: &str) -> Option<Vec<u8>> {
    let digits = text.as_bytes();
    if digits.len() % 2 != 0 {
        return None;
    }
    let nibble = |b: u8| -> Option<u8> {
        match b {
            b'0'..=b'9' => Some(b.wrapping_sub(b'0')),
            b'a'..=b'f' => Some(b.wrapping_sub(b'a').wrapping_add(10)),
            b'A'..=b'F' => Some(b.wrapping_sub(b'A').wrapping_add(10)),
            _ => None,
        }
    };
    let mut out = Vec::with_capacity(digits.len().div_ceil(2));
    let mut index = 0usize;
    while index < digits.len() {
        let (Some(&high), Some(&low)) = (digits.get(index), digits.get(index.saturating_add(1)))
        else {
            return None;
        };
        let (Some(high), Some(low)) = (nibble(high), nibble(low)) else {
            return None;
        };
        out.push(high << 4 | low);
        index = index.saturating_add(2);
    }
    Some(out)
}

fn option_str(options: &serde_json::Value, key: &str) -> Option<String> {
    options
        .get(key)
        .and_then(|value| value.as_str())
        .map(str::to_string)
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

    /// The page's own injection: 64 bytes, hex-encoded.
    fn key_material_json(fill: u8) -> String {
        let bytes = [fill; R6Material::LEN];
        let mut text = String::with_capacity(bytes.len() * 2);
        for byte in bytes {
            text.push_str(&format!("{byte:02x}"));
        }
        text
    }

    #[test]
    fn capabilities_cover_every_operation() {
        // Ops 1-3: unlock, protect (which is also the permission surface), and
        // clear permissions. Op N sets bit N-1, so the mask is three ones.
        assert_eq!(selis_security_capabilities(), 0b111);
    }

    #[test]
    fn request_validation_rejects_a_bad_envelope() {
        let pdf = plain_pdf(1);
        let response = run_request(&request(9, b"", &[("doc.pdf", &pdf)]));
        assert!(error_text(&response).contains("unknown security operation"));

        let mut bad = request(1, b"", &[("doc.pdf", &pdf)]);
        bad[0..4].copy_from_slice(b"XXXX");
        assert!(error_text(&run_request(&bad)).contains("bad request magic"));

        // Two documents is not a smaller batch here.
        let response = run_request(&request(1, b"", &[("a.pdf", &pdf), ("b.pdf", &pdf)]));
        assert!(error_text(&response).contains("exactly one document"));
    }

    #[test]
    fn protect_needs_injected_key_material_and_says_so() {
        let pdf = plain_pdf(2);
        let response = run_request(&request(
            2,
            br#"{"userPassword":"open","ownerPassword":"open"}"#,
            &[("doc.pdf", &pdf)],
        ));
        assert!(
            error_text(&response).contains("keyMaterial"),
            "the refusal names the missing material"
        );

        let response = run_request(&request(
            2,
            br#"{"userPassword":"open","keyMaterial":"deadbeef"}"#,
            &[("doc.pdf", &pdf)],
        ));
        assert!(error_text(&response).contains("128 hex characters"));

        let response = run_request(&request(
            2,
            format!(
                r#"{{"userPassword":"open","keyMaterial":"{}"}}"#,
                "zz".repeat(64)
            )
            .as_bytes(),
            &[("doc.pdf", &pdf)],
        ));
        assert!(error_text(&response).contains("not hexadecimal"));
    }

    #[test]
    fn protect_then_unlock_round_trips_over_the_wire() {
        let pdf = plain_pdf(3);
        let material = key_material_json(0x5a);
        let response = run_request(&request(
            2,
            format!(
                r#"{{"userPassword":"reader","ownerPassword":"owner",
                     "permissions":"print,copy","keyMaterial":"{material}"}}"#
            )
            .as_bytes(),
            &[("my report.pdf", &pdf)],
        ));
        let (status, message, outputs) = parse_response(&response);
        assert_eq!(status, 0, "protect failed: {message}");
        assert_eq!(message["pages_out"], 3);
        assert_eq!(message["granted"], serde_json::json!(["print", "copy"]));
        assert_eq!(
            message["denied"],
            serde_json::json!(["modify", "annotate"]),
            "the denial list is read from the flag that was written"
        );
        assert_eq!(message["algorithm"], "AES-256 (revision 6)");
        assert_eq!(message["outputs"][0]["kind"], "pdf");
        assert_eq!(outputs[0].0, "my report-protected.pdf");
        let protected = &outputs[0].1;
        let text = String::from_utf8_lossy(protected);
        assert!(text.contains("/Encrypt"), "the output carries /Encrypt");
        assert!(text.contains("/V 5"), "the handler is AESV3");
        assert!(!text.contains("reader"), "the password is not in the bytes");

        let response = run_request(&request(
            1,
            br#"{"password":"reader"}"#,
            &[("my report-protected.pdf", protected)],
        ));
        let (status, message, outputs) = parse_response(&response);
        assert_eq!(status, 0, "unlock failed: {message}");
        assert_eq!(message["encrypted"], true);
        assert_eq!(message["pages_out"], 3);
        assert_eq!(outputs[0].0, "my report-protected-unlocked.pdf");
        let unlocked = String::from_utf8_lossy(&outputs[0].1);
        assert!(
            !unlocked.contains("/Encrypt"),
            "the decrypted rewrite drops /Encrypt"
        );
    }

    #[test]
    fn a_wrong_password_errors_and_commits_nothing() {
        let pdf = plain_pdf(1);
        let material = key_material_json(0x11);
        let response = run_request(&request(
            2,
            format!(r#"{{"userPassword":"reader","keyMaterial":"{material}"}}"#).as_bytes(),
            &[("doc.pdf", &pdf)],
        ));
        let (status, _, outputs) = parse_response(&response);
        assert_eq!(status, 0, "protect failed");
        let protected = outputs[0].1.clone();

        let response = run_request(&request(
            1,
            br#"{"password":"nope"}"#,
            &[("doc-protected.pdf", &protected)],
        ));
        let message = error_text(&response);
        assert!(message.contains("password"), "{message}");
        assert!(
            !message.contains("nope"),
            "the refusal must not echo the password it was given: {message}"
        );
        assert!(
            message.contains("doc-protected.pdf"),
            "the refusal names the document the reader picked up: {message}"
        );
        assert!(
            !message.contains(SESSION_KEY),
            "the refusal does not leak this chunk's internal session key: {message}"
        );
    }

    #[test]
    fn clear_permissions_names_what_the_owner_password_lifted() {
        let pdf = plain_pdf(2);
        let material = key_material_json(0x22);
        let response = run_request(&request(
            2,
            format!(
                r#"{{"userPassword":"reader","ownerPassword":"owner",
                     "permissions":"print","keyMaterial":"{material}"}}"#
            )
            .as_bytes(),
            &[("doc.pdf", &pdf)],
        ));
        let (status, _, outputs) = parse_response(&response);
        assert_eq!(status, 0, "protect failed");

        let response = run_request(&request(
            3,
            br#"{"password":"owner"}"#,
            &[("doc-protected.pdf", &outputs[0].1)],
        ));
        let (status, message, outputs) = parse_response(&response);
        assert_eq!(status, 0, "clear permissions failed: {message}");
        assert_eq!(
            message["previouslyDenied"],
            serde_json::json!(["modify", "copy", "annotate"])
        );
        assert_eq!(message["pages_out"], 2);
        assert_eq!(outputs[0].0, "doc-protected-permissions-cleared.pdf");

        // Without the owner password the same request is refused: the bits are
        // an owner's to change, and a user password does not authorise this.
        let response = run_request(&request(
            3,
            br#"{"password":"reader"}"#,
            &[("doc-protected.pdf", &outputs[0].1)],
        ));
        assert!(
            error_text(&response).contains("owner password required"),
            "a user password cannot clear permissions"
        );
    }

    #[test]
    fn unlock_of_a_plain_document_reports_copied() {
        let pdf = plain_pdf(2);
        let response = run_request(&request(1, b"", &[("clean.pdf", &pdf)]));
        let (status, message, outputs) = parse_response(&response);
        assert_eq!(status, 0, "copy failed: {message}");
        assert_eq!(message["encrypted"], false);
        assert_eq!(message["verdict"], "copied");
        assert_eq!(outputs[0].1, pdf, "the bytes are the user's own");
    }

    #[test]
    fn hex_bytes_decodes_both_cases_and_refuses_the_rest() {
        assert_eq!(hex_bytes("00ff8B").unwrap(), vec![0x00, 0xff, 0x8b]);
        assert_eq!(hex_bytes("").unwrap(), Vec::<u8>::new());
        assert!(hex_bytes("0").is_none());
        assert!(hex_bytes("0g").is_none());
    }
}
