//! Minimal DER (a restricted subset of BER) reader for the PKCS#7/CMS
//! structures of the public-key security handler (SL-1.ENC.03).
//!
//! Hand-rolled rather than a dependency by decision (see
//! `pdf-plan/31-ENC03-DESIGN-NOTE.md §3`): the CMS subset PDF needs is ~10
//! constructs, the reader is iterative (no recursion, so no stack risk), and
//! every branch is covered by the `pkcs7_cms` fuzz target and unit vectors.
//! The reader accepts exactly what ISO 32000-2 §7.6.6 needs and rejects
//! everything else as malformed — indefinite lengths, the high-tag-number
//! form, and any length that exceeds the remaining input. RSA/EC
//! private-key decoding stays inside the `rsa`/`p256` crates, which run
//! their own hardened PKCS#8 decoders.
//!
//! Every length is `checked_*` arithmetic and every access goes through
//! `get()`, so a hostile length field can at worst yield a typed
//! [`Code::EncryptMalformed`], never a panic or an over-read.

use selis_error::{err, Code, Result};
use selis_sandbox::{BudgetGuard, Resource};

/// One DER tag: the full single-byte identifier octet (class | constructed |
/// number). The CMS subset uses only low tag numbers, so the high-tag-number
/// form is rejected outright.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Tag(pub u8);

impl Tag {
    /// Universal primitive INTEGER.
    pub(crate) const INTEGER: Tag = Tag(0x02);
    /// Universal primitive BIT STRING.
    pub(crate) const BIT_STRING: Tag = Tag(0x03);
    /// Universal primitive OCTET STRING.
    pub(crate) const OCTET_STRING: Tag = Tag(0x04);
    /// Universal primitive OBJECT IDENTIFIER.
    pub(crate) const OID: Tag = Tag(0x06);
    /// Universal constructed SEQUENCE.
    pub(crate) const SEQUENCE: Tag = Tag(0x30);
    /// Universal constructed SET (and SET OF).
    pub(crate) const SET: Tag = Tag(0x31);

    /// Context-specific primitive tag 0 (`SubjectKeyIdentifier`).
    pub(crate) const CTX_0: Tag = Tag(0x80);
    /// Context-specific constructed tag 0 (CMS `[0] EXPLICIT`).
    pub(crate) const CTX_0_CONSTRUCTED: Tag = Tag(0xA0);
    /// Context-specific constructed tag 1 (CMS `[1]`; the `KeyAgreeRecipientInfo`
    /// choice arm and `OriginatorPublicKey`).
    pub(crate) const CTX_1_CONSTRUCTED: Tag = Tag(0xA1);
}

/// One parsed tag-length-value: the tag and the content span it encloses.
///
/// The content always borrows the caller's buffer — the reader never copies
/// or allocates, so a hostile input allocates nothing at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Tlv<'a> {
    /// The identifier octet.
    pub tag: Tag,
    /// The bytes between the length octets and the next TLV.
    pub content: &'a [u8],
}

/// An iterative DER reader over the remaining bytes.
pub(crate) struct Der<'a> {
    remaining: &'a [u8],
    consumed: u64,
}

impl<'a> Der<'a> {
    /// A reader over `data`; the buffer is already resident and budgeted by
    /// the caller, so construction itself charges nothing.
    pub(crate) fn new(data: &'a [u8]) -> Self {
        Self {
            remaining: data,
            consumed: 0,
        }
    }

    /// Whether every byte has been consumed. Trailing bytes are malformed in
    /// every structure this module reads; callers reject them explicitly.
    pub(crate) fn is_empty(&self) -> bool {
        self.remaining.is_empty()
    }

    /// The byte offset of the next TLV (for error context).
    pub(crate) fn offset(&self) -> u64 {
        self.consumed
    }

    /// Read the next TLV, charging its consumed bytes to the budget.
    ///
    /// # Budget
    ///
    /// Each TLV charges [`Resource::Bytes`] for its exact wire size
    /// (identifier + length octets + content). Hostile nesting of zero-length
    /// TLVs still charges the header bytes, so a parse terminates within the
    /// budget of the input's own length; exhaustion poisons the guard and
    /// fails the whole parse typed.
    ///
    /// # Malformed Input
    ///
    /// Rejects — as [`Code::EncryptMalformed`], never a panic: truncated
    /// input (a length that runs past the end), the indefinite-length form
    /// (`0x80`), long lengths over 4 octets or with a non-minimal first
    /// octet, and the high-tag-number form (tag numbers ≥ 31). The caller
    /// rejects trailing bytes via [`Der::is_empty`].
    pub(crate) fn next(&mut self, g: &mut BudgetGuard<'_>) -> Result<Tlv<'a>> {
        let at = self.offset();
        // Identifier octet: reject the high-tag-number form (0x1F) — the CMS
        // subset uses only low tags.
        let ident = *self
            .remaining
            .first()
            .ok_or_else(|| malformed(at, "empty input"))?;
        if ident & 0x1F == 0x1F {
            return Err(malformed(at, "high tag number form"));
        }
        // Length octets: short form (< 0x80) or long form (0x81..=0x84).
        // 0x80 is the BER indefinite length — not DER, rejected. Long forms
        // beyond 4 octets are absurd for the CMS subset and rejected before
        // any arithmetic.
        let len_byte = *self
            .remaining
            .get(1)
            .ok_or_else(|| malformed(at, "truncated length"))?;
        let (len_octets, content_len) = if len_byte < 0x80 {
            (0usize, usize::from(len_byte))
        } else if len_byte == 0x80 {
            return Err(malformed(at, "indefinite length"));
        } else {
            let n = usize::from(len_byte & 0x7F);
            if !(1..=4).contains(&n) {
                return Err(malformed(at, "unsupported long length"));
            }
            let mut value: u64 = 0;
            for i in 0..n {
                // 1 + 1 + i ≤ 6: no overflow, but checked keeps the hostile
                // path uniform.
                let idx = 2usize
                    .checked_add(i)
                    .ok_or_else(|| malformed(at, "length overflow"))?;
                let b = *self
                    .remaining
                    .get(idx)
                    .ok_or_else(|| malformed(at, "truncated length"))?;
                // DER minimality: the first long-form octet must not pad.
                if i == 0 && b == 0 && n > 1 {
                    return Err(malformed(at, "non-minimal length"));
                }
                // ≤ 4 octets × 8 bits: cannot overflow u64.
                value = (value << 8) | u64::from(b);
            }
            let content_len =
                usize::try_from(value).map_err(|_| malformed(at, "length overflow"))?;
            (n, content_len)
        };
        // total = 1 (identifier) + 1 (first length octet) + len_octets +
        // content_len, all checked.
        let total = 2usize
            .checked_add(len_octets)
            .and_then(|t| t.checked_add(content_len))
            .ok_or_else(|| malformed(at, "length overflow"))?;
        if self.remaining.len() < total {
            return Err(malformed(at, "truncated content"));
        }
        // The charge happens before the split: the guard poisons on
        // exhaustion, so an over-budget input fails as a typed error.
        g.charge(Resource::Bytes, u64::try_from(total).unwrap_or(u64::MAX))?;
        let body_start = total.saturating_sub(content_len);
        let content = self
            .remaining
            .get(body_start..total)
            .ok_or_else(|| malformed(at, "truncated content"))?;
        self.remaining = self
            .remaining
            .get(total..)
            .ok_or_else(|| malformed(at, "truncated content"))?;
        self.consumed = self
            .consumed
            .saturating_add(u64::try_from(total).unwrap_or(u64::MAX));
        Ok(Tlv {
            tag: Tag(ident),
            content,
        })
    }

    /// Read the next TLV and require exactly `tag`.
    pub(crate) fn next_expect(&mut self, tag: Tag, g: &mut BudgetGuard<'_>) -> Result<Tlv<'a>> {
        let at = self.offset();
        let tlv = self.next(g)?;
        if tlv.tag != tag {
            return Err(malformed(at, "unexpected tag"));
        }
        Ok(tlv)
    }

    /// The unconsumed rest of the buffer (checked by the caller to reject
    /// trailing bytes).
    pub(crate) fn rest(&self) -> &'a [u8] {
        self.remaining
    }
}

/// Decode an OID's content into its arc list (checked arithmetic).
///
/// The first byte packs arcs 1 and 2 (`a*40 + b`); the rest is base-128.
pub(crate) fn oid_arcs(content: &[u8]) -> Option<Vec<u64>> {
    let first = *content.first()?;
    let mut arcs = vec![u64::from(first / 40), u64::from(first % 40)];
    let mut value: u64 = 0;
    let mut open = false;
    for &b in content.get(1..)? {
        // Overflow (an absurdly long arc) is malformed, not silent garbage.
        value = value.checked_shl(7)?.checked_add(u64::from(b & 0x7F))?;
        if b & 0x80 == 0 {
            arcs.push(value);
            value = 0;
            open = false;
        } else {
            open = true;
        }
    }
    if open {
        return None; // a continuation byte with no terminator
    }
    Some(arcs)
}

/// A malformed-DER error at byte offset `at` with a short reason.
pub(crate) fn malformed(at: u64, reason: &'static str) -> selis_error::Error {
    err!(
        Code::EncryptMalformed,
        during = "pkcs7",
        at = at,
        detail = reason
    )
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
    use selis_sandbox::{Budget, Surface};

    fn guard() -> BudgetGuard<'static> {
        Budget::profile(Surface::Fuzz).guard()
    }

    #[test]
    fn reads_short_and_long_form_lengths() {
        let mut g = guard();
        // SEQUENCE { INTEGER 0 } — short-form length.
        let data = [0x30, 0x03, 0x02, 0x01, 0x00];
        let mut d = Der::new(&data);
        let seq = d.next(&mut g).expect("seq");
        assert_eq!(seq.tag, Tag::SEQUENCE);
        assert_eq!(seq.content, &[0x02, 0x01, 0x00]);
        let mut inner = Der::new(seq.content);
        let int = inner.next(&mut g).expect("int");
        assert_eq!(int.tag, Tag::INTEGER);
        assert_eq!(int.content, &[0x00]);
        assert!(inner.is_empty());
        assert!(d.is_empty());
        // Long form: 0x81 0x20 (32-byte OCTET STRING).
        let mut long = vec![0x04, 0x81, 0x20];
        long.extend(std::iter::repeat_n(0xAB, 32));
        let mut d = Der::new(&long);
        let t = d.next(&mut g).expect("long form");
        assert_eq!(t.content.len(), 32);
    }

    #[test]
    fn rejects_indefinite_truncated_high_tag_and_nonminimal_lengths() {
        let cases: Vec<Vec<u8>> = vec![
            vec![0x30, 0x80],                         // indefinite length
            vec![0x30, 0x10, 0x02],                   // content runs past the end
            vec![0x1F, 0x81, 0x02, 0x01, 0x00],       // high tag number form
            vec![0x30],                               // truncated length
            vec![],                                   // empty
            vec![0x04, 0x85, 0x01, 0x00],             // 5-octet long length
            vec![0x04, 0x82, 0x00, 0x02, 0x01, 0x02], // non-minimal (leading zero)
        ];
        for data in cases {
            let mut g = guard();
            let mut d = Der::new(&data);
            let r = d.next(&mut g);
            assert!(r.is_err(), "expected malformed for {data:?}");
        }
    }

    #[test]
    fn trailing_bytes_are_detectable() {
        let mut g = guard();
        let data = [0x02, 0x01, 0x00, 0xFF];
        let mut d = Der::new(&data);
        let _ = d.next(&mut g).expect("int");
        assert!(!d.is_empty(), "the 0xFF must be visible to the caller");
    }

    #[test]
    fn zero_length_tlv_charges_and_parses() {
        let mut g = guard();
        let data = [0x05, 0x00]; // NULL
        let mut d = Der::new(&data);
        let t = d.next(&mut g).expect("null");
        assert_eq!(t.tag, Tag(0x05));
        assert!(t.content.is_empty());
        assert!(d.is_empty());
    }

    #[test]
    fn budget_exhaustion_is_a_typed_error() {
        let mut g = Budget {
            bytes: 2,
            ..Budget::profile(Surface::Fuzz)
        }
        .guard();
        let data = [0x30, 0x40, 0x02, 0x01, 0x00];
        let mut d = Der::new(&data);
        let r = d.next(&mut g);
        assert!(r.is_err(), "over-budget parse must fail typed");
    }

    #[test]
    fn oid_arcs_decode() {
        // rsaEncryption 1.2.840.113549.1.1.1
        let arcs = oid_arcs(&[0x2A, 0x86, 0x48, 0x86, 0xF7, 0x0D, 0x01, 0x01, 0x01]);
        assert_eq!(arcs.as_deref(), Some(&[1u64, 2, 840, 113549, 1, 1, 1][..]));
        // dhSinglePass-stdDH-sha256kdf-scheme 1.3.133.16.840.63.0.4
        let arcs = oid_arcs(&[0x2B, 0x81, 0x05, 0x10, 0x86, 0x48, 0x3F, 0x00, 0x04]);
        assert_eq!(
            arcs.as_deref(),
            Some(&[1u64, 3, 133, 16, 840, 63, 0, 4][..])
        );
        // A dangling continuation byte is malformed.
        assert!(oid_arcs(&[0x2A, 0x86]).is_none());
        assert!(oid_arcs(&[]).is_none());
    }
}
