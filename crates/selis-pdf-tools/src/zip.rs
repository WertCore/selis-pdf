//! A small, owned ZIP container for multi-output tools.
//!
//! The browser's split modes can produce one PDF per part. Returning a ZIP
//! lets the page offer a single download without adding a third-party archive
//! dependency or a second document path. Store (method 0) is deliberate: the
//! PDF writer already spends the CPU on the documents, and an uncompressed
//! container is simple enough to verify byte-for-byte in this crate.
//!
//! The container itself lives in `selis-archive`, the same writer the EPUB,
//! Word, PowerPoint and Excel packages use. This module owns only the rule that
//! makes a bundle safe: flat file names, no separators, no `..` marker and no
//! control characters, so a bundle cannot turn into a directory traversal when
//! a user extracts it.

#![allow(
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation,
    clippy::indexing_slicing
)]

#[cfg(test)]
use selis_archive::crc32;

fn validate_name(name: &str) -> Result<(), String> {
    if name.is_empty() {
        return Err("ZIP entry name is empty".to_string());
    }
    if name.len() > 512 {
        return Err("ZIP entry name is too long".to_string());
    }
    if name.contains('/') || name.contains('\\') {
        return Err("ZIP entry names must be flat file names".to_string());
    }
    if name == "." || name == ".." || name.contains("..") {
        return Err("ZIP entry name contains a parent-directory marker".to_string());
    }
    if name
        .bytes()
        .any(|byte| byte < 0x20 || byte == 0x7f || byte == b':')
    {
        return Err("ZIP entry name contains an unsafe character".to_string());
    }
    Ok(())
}

/// Bundle `entries` into a store-method ZIP.
///
/// The returned bytes are a complete archive; each entry is copied once into
/// the central-directory-backed container. Names are validated before any
/// bytes are written.
pub(crate) fn store_bundle(entries: &[(&str, &[u8])]) -> Result<Vec<u8>, String> {
    if entries.is_empty() {
        return Err("a ZIP bundle needs at least one file".to_string());
    }
    if entries.len() > usize::from(u16::MAX) {
        return Err("ZIP entry count exceeds the 65535-entry format limit".to_string());
    }
    for (name, data) in entries {
        validate_name(name)?;
        if u32::try_from(data.len()).is_err() {
            return Err("ZIP entry size exceeds the 4 GiB format limit".to_string());
        }
    }
    Ok(selis_archive::store_zip(entries))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]

    use super::*;

    fn read_u16(bytes: &[u8], offset: usize) -> u16 {
        u16::from_le_bytes([bytes[offset], bytes[offset + 1]])
    }

    fn read_u32(bytes: &[u8], offset: usize) -> u32 {
        u32::from_le_bytes([
            bytes[offset],
            bytes[offset + 1],
            bytes[offset + 2],
            bytes[offset + 3],
        ])
    }

    #[test]
    fn crc32_matches_the_standard_check_value() {
        assert_eq!(crc32(b"123456789"), 0xcbf4_3926);
        assert_eq!(crc32(b""), 0);
    }

    #[test]
    fn bundles_flat_files_with_a_central_directory() {
        let entries: Vec<(&str, &[u8])> = vec![("one.pdf", b"%PDF-one"), ("two.pdf", b"%PDF-two")];
        let bytes = store_bundle(&entries).expect("bundle");
        assert_eq!(read_u32(&bytes, 0), 0x0403_4b50);
        assert_eq!(read_u16(&bytes, 6), 0x0800, "UTF-8 flag");
        assert_eq!(read_u16(&bytes, 12), 0x0021, "deterministic DOS date");

        let first_name_len = usize::from(read_u16(&bytes, 26));
        let first_size = usize::try_from(read_u32(&bytes, 18)).unwrap();
        let first_offset = 30 + first_name_len;
        assert_eq!(&bytes[30..first_offset], b"one.pdf");
        assert_eq!(&bytes[first_offset..first_offset + first_size], b"%PDF-one");

        let second_local = first_offset + first_size;
        assert_eq!(read_u32(&bytes, second_local), 0x0403_4b50);
        let second_name_len = usize::from(read_u16(&bytes, second_local + 26));
        let second_size = usize::try_from(read_u32(&bytes, second_local + 18)).unwrap();
        let second_offset = second_local + 30 + second_name_len;
        assert_eq!(
            &bytes[second_offset..second_offset + second_size],
            b"%PDF-two"
        );

        let central_offset = second_offset + second_size;
        assert_eq!(read_u32(&bytes, central_offset), 0x0201_4b50);
        assert_eq!(read_u16(&bytes, central_offset + 10), 0, "store method");
        assert_eq!(
            read_u32(&bytes, central_offset + 42),
            u32::try_from(0).unwrap(),
            "first local-header offset"
        );

        let second_central =
            central_offset + 46 + usize::from(read_u16(&bytes, central_offset + 28));
        let eocd = second_central + 46 + usize::from(read_u16(&bytes, second_central + 28));
        assert_eq!(read_u32(&bytes, eocd), 0x0605_4b50);
        assert_eq!(read_u16(&bytes, eocd + 10), 2, "entry count");
        assert_eq!(
            read_u32(&bytes, eocd + 16),
            u32::try_from(central_offset).unwrap()
        );
    }

    #[test]
    fn equal_entries_produce_identical_archives() {
        let entries: Vec<(&str, &[u8])> = vec![("one.pdf", b"%PDF-one")];
        assert_eq!(
            store_bundle(&entries).expect("first"),
            store_bundle(&entries).expect("second")
        );
    }

    #[test]
    fn rejects_unsafe_names_and_empty_bundles() {
        assert!(store_bundle(&[]).is_err());
        for name in [
            "",
            "../escape.pdf",
            "a/b.pdf",
            "a\\b.pdf",
            "a:b.pdf",
            "a\nb.pdf",
        ] {
            assert!(
                store_bundle(&[(name, b"x")]).is_err(),
                "{name:?} should be rejected"
            );
        }
    }
}
