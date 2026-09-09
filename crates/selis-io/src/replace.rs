//! The atomic save-commit primitive (ADR-P0037).
//!
//! A save is atomic only if the final swap cannot tear: the destination is
//! either the old file or the new file, never a mixture. The temp file is
//! fsynced by the sink *before* calling [`atomic_replace`]; this module owns
//! the swap itself.
//!
//! Windows uses `MoveFileExW(MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH)`
//! — the documented replace operation, with the metadata write flushed.
//! POSIX uses `rename(2)`, which the kernel guarantees to be atomic over an
//! existing destination. `std::fs::rename` is deliberately not used: its
//! Windows backing flag set is not contractual, and the contract is the point
//! (see the design note `29-WRITE05-06-DESIGN-NOTE.md §2.4`).

// The one `unsafe` block below (MoveFileExW FFI) is allowlisted for this crate
// under 03-CONVENTIONS.md §2 / xtask/unsafe-allow.toml (ADR-P0037).
#![cfg_attr(windows, allow(unsafe_code))]

use std::path::Path;

use selis_error::{Code, Error};

/// Atomically replace `dest` with `temp`, which must be complete and fsynced.
///
/// On success `dest` holds `temp`'s contents and `temp` no longer exists. The
/// original `dest` is either fully preserved (failure) or fully replaced
/// (success) — never a mixture.
///
/// # Errors
///
/// `IO_READ_FAILED` (the sink-layer generic I/O code) wrapping the OS error.
/// The commonest real-world failure is a Windows sharing violation — another
/// process (antivirus, indexer, backup) holds the destination open. The
/// original file is untouched in that case; callers surface it as a typed
/// error and may retry.
pub fn atomic_replace(temp: &Path, dest: &Path) -> Result<(), Error> {
    #[cfg(windows)]
    {
        replace_windows(temp, dest)
    }
    #[cfg(not(windows))]
    {
        std::fs::rename(temp, dest).map_err(|e| io_error("rename", temp, dest, e))
    }
}

#[cfg(windows)]
fn replace_windows(temp: &Path, dest: &Path) -> Result<(), Error> {
    use std::os::windows::ffi::OsStrExt;

    // MOVEFILE_REPLACE_EXISTING: overwrite the destination even if it exists.
    // MOVEFILE_WRITE_THROUGH: the rename is written to disk before returning,
    // closing the only remaining visibility window on NTFS.
    const MOVEFILE_REPLACE_EXISTING: u32 = 0x1;
    const MOVEFILE_WRITE_THROUGH: u32 = 0x8;

    #[link(name = "kernel32")]
    extern "system" {
        fn MoveFileExW(
            lp_existing_filename: *const u16,
            lp_new_filename: *const u16,
            dw_flags: u32,
        ) -> i32;
    }

    let temp_w: Vec<u16> = temp
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let dest_w: Vec<u16> = dest
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();

    // SAFETY: both pointers are null-terminated UTF-16 path buffers owned by
    // this call and live across the FFI invocation; MoveFileExW only reads
    // them. The kernel upholds the replace semantics documented above; the
    // invariant "temp is complete and fsynced" is the caller's (FileSink
    // `finish`), enforced by this module's only-public contract.
    let ok = unsafe {
        MoveFileExW(
            temp_w.as_ptr(),
            dest_w.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    };
    if ok == 0 {
        let e = std::io::Error::last_os_error();
        return Err(io_error("MoveFileExW", temp, dest, e));
    }
    Ok(())
}

fn io_error(op: &str, temp: &Path, dest: &Path, e: std::io::Error) -> Error {
    let mut ctx = selis_error::Ctx::new();
    ctx.detail = Some(format!(
        "{op} {} -> {}: {e}",
        temp.display(),
        dest.display()
    ));
    Error::with(Code::IoReadFailed, ctx)
}

#[cfg(all(test, unix))]
mod tests {
    use super::atomic_replace;

    // The replace-over-existing semantics on POSIX rename(2); on Windows the
    // same path goes through MoveFileExW(REPLACE_EXISTING) and is exercised by
    // the FileSink tests (which run on the dev host) plus the WRITE.06 kill
    // harness, so the cross-platform behaviour is covered where the platform
    // is available.
    #[test]
    fn replace_overwrites_existing_destination() {
        let dir = std::env::temp_dir().join("selis-atomic-replace-test");
        std::fs::create_dir_all(&dir).unwrap();
        let dest = dir.join("dest.pdf");
        let temp = dir.join("dest.pdf.tmp");
        std::fs::write(&dest, b"OLD").unwrap();
        std::fs::write(&temp, b"NEW-DATA").unwrap();

        atomic_replace(&temp, &dest).unwrap();

        assert_eq!(std::fs::read(&dest).unwrap(), b"NEW-DATA");
        assert!(!temp.exists(), "temp is consumed by the replace");
        std::fs::remove_file(&dest).unwrap();
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn replace_error_is_typed_and_original_untouched() {
        let dir = std::env::temp_dir().join("selis-atomic-replace-missing");
        std::fs::create_dir_all(&dir).unwrap();
        let dest = dir.join("keep.pdf");
        let temp = dir.join("does-not-exist.tmp");
        std::fs::write(&dest, b"ORIGINAL").unwrap();

        let err = atomic_replace(&temp, &dest).unwrap_err();

        assert_eq!(err.code(), Code::IoReadFailed);
        assert_eq!(std::fs::read(&dest).unwrap(), b"ORIGINAL");
        std::fs::remove_file(&dest).unwrap();
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
