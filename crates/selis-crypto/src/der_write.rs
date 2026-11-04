//! DER writer for the structures this crate *emits*.
//!
//! The reader in [`crate::der`] is deliberately narrow: it accepts exactly the
//! CMS subset a PDF recipient blob needs and rejects everything else. The
//! writer here is the mirror image — it emits only what a PDF signature needs
//! (RFC 5652 `SignedData`, RFC 5280 `AlgorithmIdentifier` shapes and the
//! X.509 `Name` values they carry), in definite, minimal-length DER.
//!
//! Separating write from read matters for the same reason the reader is
//! hand-rolled: the output is checked by the reader, so a bug in one side
//! cannot hide in the other. Every constructor returns an owned `Vec<u8>`,
//! because the caller assembles a tree bottom-up and the encoder has no
//! borrow of the document.
//!
//! Tag numbers are single-byte and below 31; the high-tag-number form is not
//! emitted (compare [`crate::der::Tag`], which does not accept it either).

/// Universal constructed SEQUENCE (`0x30`).
pub const SEQUENCE: u8 = 0x30;
/// Universal constructed SET (`0x31`) — also used for `SET OF`.
pub const SET: u8 = 0x31;
/// Universal primitive INTEGER (`0x02`).
pub const INTEGER: u8 = 0x02;
/// Universal primitive OBJECT IDENTIFIER (`0x06`).
pub const OID: u8 = 0x06;
/// Universal primitive OCTET STRING (`0x04`).
pub const OCTET_STRING: u8 = 0x04;
/// Universal primitive BIT STRING (`0x03`).
pub const BIT_STRING: u8 = 0x03;
/// Universal primitive NULL (`0x05`).
pub const NULL: u8 = 0x05;
/// Universal primitive UTF8String (`0x0C`).
pub const UTF8_STRING: u8 = 0x0C;
/// Universal primitive UTCTime (`0x17`).
pub const UTC_TIME: u8 = 0x17;
/// Universal primitive GeneralizedTime (`0x18`).
pub const GENERALIZED_TIME: u8 = 0x18;
/// Universal primitive BOOLEAN (`0x01`).
pub const BOOLEAN: u8 = 0x01;

/// Wrap `content` in a definite-length TLV with `tag`.
///
/// Lengths use the minimal encoding: short form below 128, otherwise the
/// fewest long-form octets that hold the value (DER, not BER).
#[must_use]
pub fn tlv(tag: u8, content: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(content.len().saturating_add(6));
    out.push(tag);
    push_length(&mut out, content.len());
    out.extend_from_slice(content);
    out
}

/// Concatenate already-encoded children, as a SEQUENCE.
#[must_use]
pub fn sequence(children: &[Vec<u8>]) -> Vec<u8> {
    tlv(SEQUENCE, &concat(children))
}

/// Concatenate already-encoded children, as a SET.
#[must_use]
pub fn set(children: &[Vec<u8>]) -> Vec<u8> {
    tlv(SET, &concat(children))
}

/// A context-specific constructed tag `[n]` wrapping `content` (explicit
/// tagging).
#[must_use]
pub fn explicit(number: u8, content: &[u8]) -> Vec<u8> {
    tlv(0xA0 | number, content)
}

/// A context-specific *implicit* tag `[n]` over `content` — the element's own
/// tag is replaced, as CMS does for `[0] IMPLICIT SignatureValue` and
/// `[1] IMPLICIT SignedData.certificates`.
#[must_use]
pub fn implicit(number: u8, constructed: bool, content: &[u8]) -> Vec<u8> {
    let tag = if constructed { 0xA0 } else { 0x80 } | number;
    tlv(tag, content)
}

/// DER OBJECT IDENTIFIER from its arc list.
///
/// # Panics
///
/// The arcs come from constants in this crate; an empty list or a first arc
/// outside the two-arc packing is a programmer error, not input. Both checks
/// are debug assertions, so release builds encode whatever they were given
/// rather than panicking on a constant.
#[must_use]
pub fn oid(arcs: &[u64]) -> Vec<u8> {
    debug_assert!(arcs.len() >= 2, "an OID needs at least two arcs");
    let mut body = Vec::with_capacity(arcs.len());
    let first = arcs.first().copied().unwrap_or(0);
    let second = arcs.get(1).copied().unwrap_or(0);
    debug_assert!(first <= 2 && (first < 2 || second < 40), "OID arc packing");
    push_base128(&mut body, first.saturating_mul(40).saturating_add(second));
    for arc in arcs.iter().skip(2) {
        push_base128(&mut body, *arc);
    }
    tlv(OID, &body)
}

/// DER INTEGER from an unsigned magnitude, minimal and non-negative.
#[must_use]
pub fn unsigned_integer(value: &[u8]) -> Vec<u8> {
    let first = value.iter().position(|b| *b != 0).unwrap_or(value.len());
    let trimmed = value.get(first..).unwrap_or(&[]);
    let mut body = Vec::with_capacity(trimmed.len().saturating_add(1));
    // A leading bit set would read as negative: prepend one zero octet.
    if trimmed.first().is_some_and(|b| b & 0x80 != 0) || trimmed.is_empty() {
        body.push(0);
    }
    body.extend_from_slice(trimmed);
    tlv(INTEGER, &body)
}

/// DER INTEGER from a small signed value.
#[must_use]
pub fn integer(value: i64) -> Vec<u8> {
    let bytes = value.to_be_bytes();
    let mut start = 0usize;
    // Strip redundant sign extension octets, keeping the sign bit.
    while start < bytes.len().saturating_sub(1) {
        let current = bytes.get(start).copied().unwrap_or(0);
        let next = bytes.get(start.saturating_add(1)).copied().unwrap_or(0);
        let redundant =
            (current == 0x00 && next & 0x80 == 0) || (current == 0xFF && next & 0x80 != 0);
        if !redundant {
            break;
        }
        start = start.saturating_add(1);
    }
    tlv(INTEGER, bytes.get(start..).unwrap_or(&bytes))
}

/// DER OCTET STRING.
#[must_use]
pub fn octet_string(value: &[u8]) -> Vec<u8> {
    tlv(OCTET_STRING, value)
}

/// DER BIT STRING with no unused bits.
#[must_use]
pub fn bit_string(value: &[u8]) -> Vec<u8> {
    let mut body = Vec::with_capacity(value.len().saturating_add(1));
    body.push(0);
    body.extend_from_slice(value);
    tlv(BIT_STRING, &body)
}

/// DER BOOLEAN.
#[must_use]
pub fn boolean(value: bool) -> Vec<u8> {
    tlv(BOOLEAN, &[if value { 0xFF } else { 0x00 }])
}

/// DER NULL.
#[must_use]
pub fn null() -> Vec<u8> {
    tlv(NULL, &[])
}

/// DER UTF8String.
#[must_use]
pub fn utf8_string(value: &str) -> Vec<u8> {
    tlv(UTF8_STRING, value.as_bytes())
}

/// Concatenate encoded children.
#[must_use]
pub fn concat(children: &[Vec<u8>]) -> Vec<u8> {
    let total = children.iter().map(Vec::len).sum();
    let mut out = Vec::with_capacity(total);
    for child in children {
        out.extend_from_slice(child);
    }
    out
}

fn push_length(out: &mut Vec<u8>, len: usize) {
    if len < 0x80 {
        out.push(u8::try_from(len).unwrap_or(0x7F));
        return;
    }
    let bytes = len.to_be_bytes();
    let first = bytes.iter().position(|b| *b != 0).unwrap_or(bytes.len());
    let significant = bytes.get(first..).unwrap_or(&[]);
    let count = u8::try_from(significant.len()).unwrap_or(4);
    out.push(0x80 | count);
    out.extend_from_slice(significant);
}

fn push_base128(out: &mut Vec<u8>, value: u64) {
    let mut stack = [0u8; 10];
    let mut len = 0usize;
    let mut rest = value;
    loop {
        stack[len] = u8::try_from(rest & 0x7F).unwrap_or(0);
        len = len.saturating_add(1);
        rest >>= 7;
        if rest == 0 {
            break;
        }
    }
    for index in (0..len).rev() {
        let mut byte = stack.get(index).copied().unwrap_or(0);
        if index != 0 {
            byte |= 0x80;
        }
        out.push(byte);
    }
}

/// Encode a UTC timestamp as DER `UTCTime` (1950–2049) or `GeneralizedTime`.
///
/// `stamp` is `YYYYMMDDHHMMSSZ` — the form a caller derives from its own
/// clock. The core never reads a clock: a signature's signing time is an
/// assertion by the caller, so it is passed in like every other input.
///
/// # Errors
///
/// Returns `None` when the stamp is not fourteen digits plus `Z`. A caller
/// that cannot name the moment must say so instead of inventing one.
#[must_use]
pub fn signing_time(stamp: &str) -> Option<Vec<u8>> {
    let digits = stamp.strip_suffix('Z')?;
    if digits.len() != 14 || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let year: u32 = digits.get(0..4)?.parse().ok()?;
    if (1950..=2049).contains(&year) {
        // UTCTime drops the century but keeps the trailing Z.
        let body = digits.get(2..)?;
        Some(tlv(UTC_TIME, format!("{body}Z").as_bytes()))
    } else {
        Some(tlv(GENERALIZED_TIME, stamp.as_bytes()))
    }
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
    fn lengths_are_minimal_and_definite() {
        assert_eq!(tlv(SEQUENCE, &[0u8; 3])[1], 3);
        let long = tlv(SEQUENCE, &[0u8; 200]);
        assert_eq!(long[1], 0x81);
        assert_eq!(long[2], 200);
        let longer = tlv(SEQUENCE, &[0u8; 300]);
        assert_eq!(longer[1], 0x82);
        assert_eq!(longer[2], 1);
        assert_eq!(longer[3], 44);
    }

    #[test]
    fn oid_round_trips_through_the_reader() {
        use crate::der::oid_arcs;
        let arcs = [1u64, 2, 840, 113549, 1, 7, 2];
        let encoded = oid(&arcs);
        assert_eq!(encoded[0], OID);
        assert_eq!(oid_arcs(&encoded[2..]).as_deref(), Some(&arcs[..]));
    }

    #[test]
    fn integers_strip_padding_and_keep_the_sign_bit() {
        // 255 needs the zero octet: 0xFF alone would read as -1.
        assert_eq!(
            unsigned_integer(&[0x00, 0xFF]),
            vec![0x02, 0x02, 0x00, 0xFF]
        );
        assert_eq!(unsigned_integer(&[0x00, 0x7F]), vec![0x02, 0x01, 0x7F]);
        // A high bit set gains a zero so the value stays positive.
        assert_eq!(unsigned_integer(&[0x80]), vec![0x02, 0x02, 0x00, 0x80]);
        assert_eq!(integer(0), vec![0x02, 0x01, 0x00]);
        assert_eq!(integer(-1), vec![0x02, 0x01, 0xFF]);
        assert_eq!(integer(127), vec![0x02, 0x01, 0x7F]);
        assert_eq!(integer(128), vec![0x02, 0x02, 0x00, 0x80]);
        assert_eq!(integer(256), vec![0x02, 0x02, 0x01, 0x00]);
    }

    #[test]
    fn signing_time_picks_utc_or_generalized() {
        assert_eq!(
            signing_time("20261104120000Z").as_deref(),
            Some(
                &[
                    UTC_TIME, 13, b'2', b'6', b'1', b'1', b'0', b'4', b'1', b'2', b'0', b'0', b'0',
                    b'0', b'Z'
                ][..]
            )
        );
        assert_eq!(
            signing_time("20501104120000Z").map(|v| v[0]),
            Some(GENERALIZED_TIME)
        );
        assert!(signing_time("2026-11-04T12:00:00Z").is_none());
        assert!(signing_time("2026110412000Z").is_none());
    }

    #[test]
    fn context_tags_are_constructed_or_primitive() {
        assert_eq!(explicit(0, &[0x02, 0x01, 0x00])[0], 0xA0);
        assert_eq!(implicit(1, true, &[])[0], 0xA1);
        assert_eq!(implicit(0, false, &[0x01])[0], 0x80);
    }
}
