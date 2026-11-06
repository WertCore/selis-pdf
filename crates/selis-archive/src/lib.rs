//! The ZIP container every package writer shares.
//!
//! EPUB output, Word, PowerPoint and Excel documents, and the browser's own
//! multi-file bundles are all ZIP containers. Keeping one writer and one reader
//! in a foundation crate means a container bug is fixed once, the same input
//! produces the same bytes in every product, and a desktop build reads a package
//! through exactly the code the browser chunk writes it with.
//!
//! Writing is deliberately unambitious: stored entries, a fixed date stamp,
//! entries in caller order. That is what OCF requires for `mimetype`, and it is
//! what makes the output reproducible. Reading accepts stored and deflated
//! entries, because a package a reader supplies was not written by this crate.

#![forbid(unsafe_code)]
#![allow(
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation
)]

/// Stored (no compression).
const METHOD_STORE: u16 = 0;
/// Deflate.
const METHOD_DEFLATE: u16 = 8;

/// DOS date for 1980-01-01, the earliest a ZIP can express.
const DOS_DATE: u16 = 0x0021;

const LOCAL_SIG: u32 = 0x0403_4b50;
const CENTRAL_SIG: u32 = 0x0201_4b50;
const EOCD_SIG: u32 = 0x0605_4b50;

/// The largest default limit for one entry's expanded size, so a hostile
/// archive cannot turn a small upload into an unbounded allocation.
pub const MAX_ENTRY_BYTES: usize = 64 * 1024 * 1024;

/// CRC-32 (IEEE), computed rather than tabulated: one table is 1 KiB of rodata
/// a WASM chunk would carry for every reader that never sees a ZIP.
#[must_use]
pub fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for byte in data {
        crc ^= u32::from(*byte);
        for _ in 0..8 {
            let mask = (crc & 1).wrapping_neg();
            crc = (crc >> 1) ^ (0xEDB8_8320 & mask);
        }
    }
    !crc
}

/// Write a stored-entry ZIP: local headers, a central directory, one end record.
///
/// No compression, no data descriptor, no ZIP64. Names may contain `/` (a
/// package's own directory structure); a caller that accepts names from outside
/// its crate must validate them itself.
///
/// # Output Guarantees
///
/// The same entries in the same order always produce the same bytes.
#[must_use]
pub fn store_zip(entries: &[(&str, &[u8])]) -> Vec<u8> {
    let mut out: Vec<u8> = Vec::new();
    let mut central: Vec<u8> = Vec::new();
    for (name, data) in entries {
        let offset = u32::try_from(out.len()).unwrap_or(u32::MAX);
        let size = u32::try_from(data.len()).unwrap_or(u32::MAX);
        let crc = crc32(data);
        let name_len = u16::try_from(name.len()).unwrap_or(u16::MAX);
        out.extend_from_slice(&LOCAL_SIG.to_le_bytes());
        out.extend_from_slice(&20u16.to_le_bytes());
        out.extend_from_slice(&0x0800u16.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        out.extend_from_slice(&DOS_DATE.to_le_bytes());
        out.extend_from_slice(&crc.to_le_bytes());
        out.extend_from_slice(&size.to_le_bytes());
        out.extend_from_slice(&size.to_le_bytes());
        out.extend_from_slice(&name_len.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        out.extend_from_slice(name.as_bytes());
        out.extend_from_slice(data);

        central.extend_from_slice(&CENTRAL_SIG.to_le_bytes());
        central.extend_from_slice(&20u16.to_le_bytes());
        central.extend_from_slice(&20u16.to_le_bytes());
        central.extend_from_slice(&0x0800u16.to_le_bytes());
        central.extend_from_slice(&0u16.to_le_bytes());
        central.extend_from_slice(&0u16.to_le_bytes());
        central.extend_from_slice(&DOS_DATE.to_le_bytes());
        central.extend_from_slice(&crc.to_le_bytes());
        central.extend_from_slice(&size.to_le_bytes());
        central.extend_from_slice(&size.to_le_bytes());
        central.extend_from_slice(&name_len.to_le_bytes());
        central.extend_from_slice(&0u16.to_le_bytes());
        central.extend_from_slice(&0u16.to_le_bytes());
        central.extend_from_slice(&0u16.to_le_bytes());
        central.extend_from_slice(&0u16.to_le_bytes());
        central.extend_from_slice(&0u32.to_le_bytes());
        central.extend_from_slice(&offset.to_le_bytes());
        central.extend_from_slice(name.as_bytes());
    }
    let cd_offset = u32::try_from(out.len()).unwrap_or(u32::MAX);
    let cd_size = u32::try_from(central.len()).unwrap_or(u32::MAX);
    out.extend_from_slice(&central);
    let count = u16::try_from(entries.len()).unwrap_or(u16::MAX);
    out.extend_from_slice(&EOCD_SIG.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(&count.to_le_bytes());
    out.extend_from_slice(&count.to_le_bytes());
    out.extend_from_slice(&cd_size.to_le_bytes());
    out.extend_from_slice(&cd_offset.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out
}

fn zip_u16(bytes: &[u8], at: usize) -> Option<u16> {
    let slice = bytes.get(at..at.checked_add(2)?)?;
    let mut raw = [0u8; 2];
    raw.copy_from_slice(slice);
    Some(u16::from_le_bytes(raw))
}

fn zip_u32(bytes: &[u8], at: usize) -> Option<u32> {
    let slice = bytes.get(at..at.checked_add(4)?)?;
    let mut raw = [0u8; 4];
    raw.copy_from_slice(slice);
    Some(u32::from_le_bytes(raw))
}

/// Decompress a raw deflate stream, bounded by [`MAX_ENTRY_BYTES`].
fn inflate(data: &[u8], want: usize) -> Result<Vec<u8>, String> {
    if want > MAX_ENTRY_BYTES {
        return Err("an archive entry is larger than this build accepts".to_string());
    }
    let out = miniz_oxide::inflate::decompress_to_vec(data)
        .map_err(|_| "a deflated archive entry could not be decompressed".to_string())?;
    if out.len() != want {
        return Err("an archive entry does not match the size it declares".to_string());
    }
    Ok(out)
}

/// Read the entries of a ZIP, in central-directory order.
///
/// Stored and deflated entries are accepted; ZIP64, encryption and every other
/// method are refused by name rather than misread. Each entry's CRC-32 is
/// checked, so a damaged package is a failed read instead of a shorter book.
///
/// # Errors
///
/// A sentence naming the reason the package cannot be read.
pub fn read_zip(bytes: &[u8]) -> Result<Vec<(String, Vec<u8>)>, String> {
    let truncated = || "the archive is truncated".to_string();
    let mut eocd: Option<usize> = None;
    let floor = bytes.len().saturating_sub(65_557);
    let mut at = bytes.len().saturating_sub(22);
    loop {
        if at < floor {
            break;
        }
        if zip_u32(bytes, at) == Some(EOCD_SIG) {
            eocd = Some(at);
            break;
        }
        match at.checked_sub(1) {
            Some(next) => at = next,
            None => break,
        }
    }
    let eocd = eocd.ok_or_else(|| "the archive has no end-of-directory record".to_string())?;
    let count = u32::from(
        zip_u16(bytes, eocd.checked_add(10).ok_or_else(truncated)?).ok_or_else(truncated)?,
    );
    let cd_at =
        zip_u32(bytes, eocd.checked_add(16).ok_or_else(truncated)?).ok_or_else(truncated)?;
    let mut cursor = usize::try_from(cd_at).map_err(|_| truncated())?;
    let mut entries: Vec<(String, Vec<u8>)> = Vec::new();
    for _ in 0..count {
        if zip_u32(bytes, cursor) != Some(CENTRAL_SIG) {
            return Err("the central directory is malformed".to_string());
        }
        let field = |offset: usize| cursor.checked_add(offset).ok_or_else(truncated);
        let method = zip_u16(bytes, field(10)?).ok_or_else(truncated)?;
        let crc = zip_u32(bytes, field(16)?).ok_or_else(truncated)?;
        let compressed = zip_u32(bytes, field(20)?).ok_or_else(truncated)?;
        let size = zip_u32(bytes, field(24)?).ok_or_else(truncated)?;
        let name_len = usize::from(zip_u16(bytes, field(28)?).ok_or_else(truncated)?);
        let extra_len = usize::from(zip_u16(bytes, field(30)?).ok_or_else(truncated)?);
        let comment_len = usize::from(zip_u16(bytes, field(32)?).ok_or_else(truncated)?);
        let local = usize::try_from(zip_u32(bytes, field(42)?).ok_or_else(truncated)?)
            .map_err(|_| truncated())?;
        let name_at = cursor.checked_add(46).ok_or_else(truncated)?;
        let name_end = name_at.checked_add(name_len).ok_or_else(truncated)?;
        let name = String::from_utf8_lossy(bytes.get(name_at..name_end).ok_or_else(truncated)?)
            .into_owned();
        cursor = name_end
            .checked_add(extra_len)
            .and_then(|at| at.checked_add(comment_len))
            .ok_or_else(truncated)?;
        if zip_u32(bytes, local) != Some(LOCAL_SIG) {
            return Err(format!("{name}'s local header is missing"));
        }
        // Local header: name length at +26, extra length at +28, data at +30.
        let local_name = local.checked_add(26).ok_or_else(truncated)?;
        let local_extra = local.checked_add(28).ok_or_else(truncated)?;
        let data_at = local
            .checked_add(30)
            .and_then(|at| at.checked_add(usize::from(zip_u16(bytes, local_name)?)))
            .and_then(|at| at.checked_add(usize::from(zip_u16(bytes, local_extra)?)))
            .ok_or_else(truncated)?;
        let compressed = usize::try_from(compressed).map_err(|_| truncated())?;
        let size = usize::try_from(size).map_err(|_| truncated())?;
        let raw = bytes
            .get(data_at..data_at.checked_add(compressed).ok_or_else(truncated)?)
            .ok_or_else(truncated)?;
        let data = match method {
            METHOD_STORE => {
                if compressed != size {
                    return Err(format!("{name} declares inconsistent sizes"));
                }
                raw.to_vec()
            }
            METHOD_DEFLATE => inflate(raw, size)?,
            other => {
                return Err(format!(
                    "{name} uses unsupported compression method {other}; only stored and deflated entries are accepted"
                ))
            }
        };
        if crc32(&data) != crc {
            return Err(format!("{name} failed its checksum"));
        }
        entries.push((name, data));
    }
    Ok(entries)
}

/// Read an archive and return one named entry as UTF-8 text.
///
/// # Errors
///
/// As [`read_zip`], plus a missing or non-UTF-8 entry.
pub fn read_text_entry(bytes: &[u8], name: &str) -> Result<String, String> {
    let entries = read_zip(bytes)?;
    let (_, data) = entries
        .into_iter()
        .find(|(entry, _)| entry == name)
        .ok_or_else(|| format!("{name} is missing"))?;
    String::from_utf8(data).map_err(|_| format!("{name} is not UTF-8"))
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing,
        clippy::arithmetic_side_effects
    )]

    use super::*;

    #[test]
    fn crc32_matches_the_standard_check_value() {
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
    }

    #[test]
    fn stored_entries_round_trip() {
        let zip = store_zip(&[("a.txt", b"hello"), ("dir/b.bin", &[0u8, 1, 2])]);
        let entries = read_zip(&zip).expect("read back");
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0], ("a.txt".to_string(), b"hello".to_vec()));
        assert_eq!(entries[1], ("dir/b.bin".to_string(), vec![0u8, 1, 2]));
    }

    #[test]
    fn a_flipped_byte_fails_the_checksum() {
        let mut zip = store_zip(&[("a.txt", b"hello")]);
        // The stored bytes start after the 30-byte local header and the name.
        zip[35] ^= 0xFF;
        let err = read_zip(&zip).expect_err("damaged");
        assert!(err.contains("checksum"), "{err}");
    }

    #[test]
    fn deterministic_bytes() {
        let first = store_zip(&[("a", b"1"), ("b", b"2")]);
        let second = store_zip(&[("a", b"1"), ("b", b"2")]);
        assert_eq!(first, second);
    }
}
