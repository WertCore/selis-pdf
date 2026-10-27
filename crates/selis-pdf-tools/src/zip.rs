//! A small, owned ZIP container for multi-output tools.
//!
//! The browser's split modes can produce one PDF per part. Returning a ZIP
//! lets the page offer a single download without adding a third-party archive
//! dependency or a second document path. Store (method 0) is deliberate: the
//! PDF writer already spends the CPU on the documents, and an uncompressed
//! container is simple enough to verify byte-for-byte in this crate.
//!
//! Entries are flat file names, UTF-8 flagged, and deterministic (fixed DOS
//! timestamp). The writer rejects absolute paths, separators, control
//! characters, and `..` markers so a bundle cannot turn into a directory
//! traversal when a user extracts it.

#![allow(
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation,
    clippy::indexing_slicing
)]

/// The CRC-32 table (IEEE polynomial), computed at compile time.
const CRC_TABLE: [u32; 256] = {
    let mut table = [0u32; 256];
    let mut i = 0;
    while i < 256 {
        let mut crc = i as u32;
        let mut bit = 0;
        while bit < 8 {
            crc = if crc & 1 != 0 {
                0xedb8_8320 ^ (crc >> 1)
            } else {
                crc >> 1
            };
            bit += 1;
        }
        table[i] = crc;
        i += 1;
    }
    table
};

fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xffff_ffffu32;
    for &byte in data {
        let index = ((crc ^ u32::from(byte)) & 0xff) as usize;
        crc = CRC_TABLE[index] ^ (crc >> 8);
    }
    !crc
}

fn push_u16(out: &mut Vec<u8>, value: u16) {
    out.extend_from_slice(&value.to_le_bytes());
}

fn push_u32(out: &mut Vec<u8>, value: u32) {
    out.extend_from_slice(&value.to_le_bytes());
}

fn checked_u32(value: usize, what: &str) -> Result<u32, String> {
    u32::try_from(value).map_err(|_| format!("ZIP {what} exceeds the 4 GiB format limit"))
}

fn checked_u16(value: usize, what: &str) -> Result<u16, String> {
    u16::try_from(value).map_err(|_| format!("ZIP {what} exceeds the 65535-entry format limit"))
}

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
    checked_u16(entries.len(), "entry count")?;

    let mut out: Vec<u8> = Vec::new();
    let mut central: Vec<u8> = Vec::new();

    for (name, data) in entries {
        validate_name(name)?;
        let name_bytes = name.as_bytes();
        let name_len = checked_u16(name_bytes.len(), "entry name length")?;
        let data_len = checked_u32(data.len(), "entry size")?;
        let offset = checked_u32(out.len(), "entry offset")?;
        let crc = crc32(data);

        // Local file header. Bit 11 marks the name as UTF-8; the DOS
        // timestamp is the epoch (1980-01-01 00:00:00) so equal inputs
        // produce equal bytes.
        push_u32(&mut out, 0x0403_4b50);
        push_u16(&mut out, 20);
        push_u16(&mut out, 0x0800);
        push_u16(&mut out, 0);
        push_u16(&mut out, 0);
        push_u16(&mut out, 0x0021);
        push_u32(&mut out, crc);
        push_u32(&mut out, data_len);
        push_u32(&mut out, data_len);
        push_u16(&mut out, name_len);
        push_u16(&mut out, 0);
        out.extend_from_slice(name_bytes);
        out.extend_from_slice(data);

        // Central directory header, filled with the same sizes and offset.
        push_u32(&mut central, 0x0201_4b50);
        push_u16(&mut central, 20);
        push_u16(&mut central, 20);
        push_u16(&mut central, 0x0800);
        push_u16(&mut central, 0);
        push_u16(&mut central, 0);
        push_u16(&mut central, 0x0021);
        push_u32(&mut central, crc);
        push_u32(&mut central, data_len);
        push_u32(&mut central, data_len);
        push_u16(&mut central, name_len);
        push_u16(&mut central, 0);
        push_u16(&mut central, 0);
        push_u16(&mut central, 0);
        push_u16(&mut central, 0);
        push_u32(&mut central, 0);
        push_u32(&mut central, offset);
        central.extend_from_slice(name_bytes);
    }

    let central_offset = checked_u32(out.len(), "central-directory offset")?;
    let central_size = checked_u32(central.len(), "central-directory size")?;
    let count = checked_u16(entries.len(), "entry count")?;
    out.extend_from_slice(&central);

    // End of central directory.
    push_u32(&mut out, 0x0605_4b50);
    push_u16(&mut out, 0);
    push_u16(&mut out, 0);
    push_u16(&mut out, count);
    push_u16(&mut out, count);
    push_u32(&mut out, central_size);
    push_u32(&mut out, central_offset);
    push_u16(&mut out, 0);
    Ok(out)
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
