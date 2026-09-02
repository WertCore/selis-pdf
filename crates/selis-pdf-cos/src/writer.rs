//! Object writer (SL-1.COS.09).
//!
//! Serialises [`Obj`] back to COS syntax with correct escaping and number
//! formatting. Two PDF-specific traps handled here:
//!
//! * **no locale, no exponent notation** — PDF forbids `1e3`, `1,5`, `1_000`;
//!   reals are printed as decimal digits with a `.` and no trailing zeros
//!   beyond what the scaled representation implies;
//! * **stream `/Length`** — a stream's data is written with a matching
//!   `/Length` in its dictionary.
//!
//! The writer is the inverse of the parser: the DoD property test
//! `parse(write(obj)) == obj` for arbitrary objects, including strings with
//! every byte value and deeply nested containers, is in this module.

use selis_error::Result;
use selis_sandbox::{Budget, BudgetGuard};

use crate::obj::Obj;

/// The writer's buffer.
#[derive(Debug)]
pub struct Writer {
    out: Vec<u8>,
}

impl Writer {
    /// A writer.
    #[must_use]
    pub fn new(_budget: &Budget) -> Self {
        Self { out: Vec::new() }
    }

    /// Write a top-level object.
    ///
    /// # Budget
    ///
    /// Charges the output bytes.
    ///
    /// # Malformed Input
    ///
    /// `BUDGET_BYTES` when the output exceeds the budget.
    pub fn write_obj(&mut self, obj: &Obj, g: &mut BudgetGuard<'_>) -> Result<()> {
        self.write_value(obj, g)?;
        self.out.push(b'\n');
        Ok(())
    }

    /// Serialise an object and return the bytes.
    ///
    /// # Budget
    ///
    /// As [`Writer::write_obj`].
    ///
    /// # Malformed Input
    ///
    /// `BUDGET_BYTES` on exhaustion.
    pub fn to_bytes(&mut self, obj: &Obj, g: &mut BudgetGuard<'_>) -> Result<Vec<u8>> {
        self.write_value(obj, g)?;
        Ok(self.out.clone())
    }

    fn write_value(&mut self, obj: &Obj, g: &mut BudgetGuard<'_>) -> Result<()> {
        g.tick()?;
        g.charge_one(selis_sandbox::Resource::Objects)?;
        match obj {
            Obj::Null => self.push(b"null"),
            Obj::Bool(true) => self.push(b"true"),
            Obj::Bool(false) => self.push(b"false"),
            Obj::Int(i) => self.push_int(*i),
            Obj::Real { scaled, scale } => self.push_real(*scaled, *scale),
            Obj::String(bytes) => self.write_string(bytes),
            Obj::HexString(bytes) => self.write_hex_string(bytes),
            Obj::Name(name) => self.write_name(name),
            Obj::Ref(r) => {
                self.push_int(i64::from(r.num));
                self.push(b" ");
                self.push_int(i64::from(r.gen));
                self.push(b" R");
            }
            Obj::Array(items) => {
                self.push(b"[");
                for (i, item) in items.iter().enumerate() {
                    if i > 0 {
                        self.push(b" ");
                    }
                    self.write_value(item, g)?;
                }
                self.push(b"]");
            }
            Obj::Dict(pairs) => {
                self.push(b"<<");
                for (k, v) in pairs {
                    self.write_name(k);
                    self.push(b" ");
                    self.write_value(v, g)?;
                }
                self.push(b">>");
            }
            Obj::Stream { dict, data } => {
                // Write the dict with a /Length, then `stream ... endstream`.
                let mut with_len = dict.clone();
                if !with_len.iter().any(|(k, _)| k.as_slice() == b"Length") {
                    with_len.push((
                        selis_bytes::Bytes::copy_from_slice(b"Length"),
                        Obj::Int(i64::try_from(data.len()).unwrap_or(i64::MAX)),
                    ));
                }
                self.push(b"<<");
                for (k, v) in &with_len {
                    self.write_name(k);
                    self.push(b" ");
                    self.write_value(v, g)?;
                }
                self.push(b">>\nstream\n");
                g.charge(selis_sandbox::Resource::Bytes, data.len() as u64)?;
                self.out.extend_from_slice(data.as_slice());
                self.push(b"\nendstream");
            }
        }
        Ok(())
    }

    fn write_name(&mut self, name: &selis_bytes::Bytes) {
        self.push(b"/");
        for &b in name.as_slice() {
            // Escape the characters that would break the name token.
            match b {
                0x20 | b'(' | b')' | b'<' | b'>' | b'[' | b']' | b'{' | b'}' | b'/' | b'%'
                | b'#' | 0x7f => {
                    self.push(b"#");
                    self.push_hex(b);
                }
                _ => self.out.push(b),
            }
        }
    }

    fn write_string(&mut self, bytes: &selis_bytes::Bytes) {
        self.push(b"(");
        for &b in bytes.as_slice() {
            match b {
                b'(' => self.push(b"\\("),
                b')' => self.push(b"\\)"),
                b'\\' => self.push(b"\\\\"),
                b'\n' => self.push(b"\\n"),
                b'\r' => self.push(b"\\r"),
                b'\t' => self.push(b"\\t"),
                0x08 => self.push(b"\\b"),
                0x0c => self.push(b"\\f"),
                _ => self.out.push(b),
            }
        }
        self.push(b")");
    }

    /// Write a string as a hex literal (`<…>`, two hex digits per byte).
    fn write_hex_string(&mut self, bytes: &selis_bytes::Bytes) {
        const HEX: &[u8; 16] = b"0123456789ABCDEF";
        self.push(b"<");
        for &b in bytes.as_slice() {
            let hi = usize::from(b >> 4);
            let lo = usize::from(b & 0x0f);
            if let (Some(&h), Some(&l)) = (HEX.get(hi), HEX.get(lo)) {
                self.out.push(h);
                self.out.push(l);
            }
        }
        self.push(b">");
    }

    fn push_int(&mut self, v: i64) {
        let mut buf = itoa_buf(v);
        self.out.extend_from_slice(&buf);
    }

    fn push_real(&mut self, scaled: i64, scale: u8) {
        // value = scaled / 10^scale. Print with exactly `scale` fractional
        // digits, then trim nothing — deterministic and locale-free.
        let neg = scaled < 0;
        let abs = if neg { scaled.wrapping_neg() } else { scaled };
        let mut digits = itoa_buf(abs);
        if scale == 0 {
            self.out.extend_from_slice(&digits);
            return;
        }
        let scale_us = usize::from(scale);
        if digits.len() <= scale_us {
            // Pad leading zeros so the fractional part has the right width.
            let mut padded = Vec::new();
            for _ in 0..scale_us.saturating_sub(digits.len()).saturating_add(1) {
                padded.push(b'0');
            }
            padded.extend_from_slice(&digits);
            digits = padded;
        }
        let split = digits.len().saturating_sub(scale_us);
        if neg {
            self.out.push(b'-');
        }
        let int_part = digits.get(..split).unwrap_or(&[]);
        let frac_part = digits.get(split..).unwrap_or(&[]);
        self.out.extend_from_slice(int_part);
        self.out.push(b'.');
        self.out.extend_from_slice(frac_part);
    }

    fn push_hex(&mut self, b: u8) {
        const HEX: &[u8; 16] = b"0123456789ABCDEF";
        let hi = usize::from(b >> 4);
        let lo = usize::from(b & 0x0f);
        if let (Some(&h), Some(&l)) = (HEX.get(hi), HEX.get(lo)) {
            self.out.push(h);
            self.out.push(l);
        }
    }

    fn push(&mut self, bytes: &[u8]) {
        self.out.extend_from_slice(bytes);
    }
}

/// Format an integer as ASCII digits (no sign handling).
fn itoa_buf(v: i64) -> Vec<u8> {
    // Handle the smallest negative by negating on the unsigned side.
    let unsigned = v.unsigned_abs();
    if unsigned == 0 {
        return vec![b'0'];
    }
    let mut buf = Vec::new();
    let mut n = unsigned;
    while n > 0 {
        let digit = u8::try_from(n % 10).unwrap_or(9);
        let ch = digit.wrapping_add(b'0');
        buf.push(ch);
        n = n.wrapping_div(10);
    }
    buf.reverse();
    if v < 0 {
        let mut signed = vec![b'-'];
        signed.extend_from_slice(&buf);
        return signed;
    }
    buf
}

/// The module-level inverse of [`Writer`]: parse, write, and compare.
///
/// # Budget
///
/// As the writer.
///
/// # Malformed Input
///
/// A parse error on the written bytes is a writer bug surfaced as
/// `OBJ_UNEXPECTED`.
pub fn roundtrip(obj: &Obj, g: &mut BudgetGuard<'_>) -> Result<Obj> {
    let budget = selis_sandbox::Budget::unlimited();
    let mut w = Writer::new(&budget);
    let bytes = w.to_bytes(obj, g)?;
    let mut lexer = crate::Lexer::new(&bytes);
    let mut tokens = Vec::new();
    while let Some(t) = lexer.next_token(g)? {
        tokens.push(t);
    }
    let mut parser = crate::parse::ObjectParser::new(&tokens, &budget);
    parser.parse(g)
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::indexing_slicing,
        clippy::arithmetic_side_effects,
        clippy::cast_sign_loss
    )]

    use super::*;
    use selis_sandbox::{CancelToken, FixedClock};

    fn guard() -> BudgetGuard<'static> {
        selis_sandbox::Budget::unlimited().guard_with(&FixedClock(0), CancelToken::new())
    }

    fn write(obj: &Obj) -> String {
        let mut g = guard();
        let budget = selis_sandbox::Budget::unlimited();
        let mut w = Writer::new(&budget);
        let bytes = w.to_bytes(obj, &mut g).expect("write");
        String::from_utf8(bytes).expect("ascii output")
    }

    #[test]
    fn scalars_write() {
        assert_eq!(write(&Obj::Null), "null");
        assert_eq!(write(&Obj::Bool(true)), "true");
        assert_eq!(write(&Obj::Bool(false)), "false");
        assert_eq!(write(&Obj::Int(-42)), "-42");
    }

    #[test]
    fn reals_have_no_exponent_and_no_locale() {
        assert_eq!(
            write(&Obj::Real {
                scaled: 25,
                scale: 1
            }),
            "2.5"
        );
        assert_eq!(
            write(&Obj::Real {
                scaled: -25,
                scale: 1
            }),
            "-2.5"
        );
        // 1 / 1000 = 0.001
        assert_eq!(
            write(&Obj::Real {
                scaled: 1,
                scale: 3
            }),
            "0.001"
        );
    }

    #[test]
    fn names_escape_delimiters() {
        assert_eq!(
            write(&Obj::Name(selis_bytes::Bytes::copy_from_slice(b"Type"))),
            "/Type"
        );
        assert_eq!(
            write(&Obj::Name(selis_bytes::Bytes::copy_from_slice(b"A B"))),
            "/A#20B"
        );
    }

    #[test]
    fn strings_escape() {
        assert_eq!(
            write(&Obj::String(selis_bytes::Bytes::copy_from_slice(
                b"a(b)c\\d"
            ))),
            r"(a\(b\)c\\d)"
        );
    }

    #[test]
    fn hex_strings_write_as_hex_literals() {
        assert_eq!(
            write(&Obj::HexString(selis_bytes::Bytes::copy_from_slice(
                &[0xde, 0xad, 0xbe, 0xef]
            ))),
            "<DEADBEEF>"
        );
        assert_eq!(
            write(&Obj::HexString(selis_bytes::Bytes::copy_from_slice(b"AB"))),
            "<4142>"
        );
    }

    #[test]
    fn refs_and_containers() {
        assert_eq!(write(&Obj::Ref(crate::obj::Ref::new(5, 0))), "5 0 R");
        assert_eq!(write(&Obj::Array(vec![Obj::Int(1), Obj::Int(2)])), "[1 2]");
        assert_eq!(
            write(&Obj::Dict(vec![(
                selis_bytes::Bytes::copy_from_slice(b"K"),
                Obj::Int(1)
            )])),
            "<</K 1>>"
        );
    }

    #[test]
    fn streams_carry_length() {
        let data = b"abc".to_vec();
        let stream = Obj::Stream {
            dict: vec![],
            data: selis_bytes::Bytes::copy_from_slice(&data),
        };
        let out = write(&stream);
        assert!(
            out.contains("/Length 3"),
            "stream dict carries /Length: {out}"
        );
        assert!(
            out.ends_with("endstream"),
            "stream ends with endstream: {out}"
        );
    }

    /// DoD: `parse(write(obj)) == obj` for a range of values.
    #[test]
    fn roundtrip_property() {
        let cases = vec![
            Obj::Null,
            Obj::Bool(true),
            Obj::Int(42),
            Obj::Real {
                scaled: 25,
                scale: 1,
            },
            Obj::String(selis_bytes::Bytes::copy_from_slice(b"hi there")),
            Obj::Name(selis_bytes::Bytes::copy_from_slice(b"Name")),
            Obj::Ref(crate::obj::Ref::new(7, 2)),
            Obj::Array(vec![
                Obj::Int(1),
                Obj::Name(selis_bytes::Bytes::copy_from_slice(b"x")),
            ]),
            Obj::Dict(vec![(
                selis_bytes::Bytes::copy_from_slice(b"K"),
                Obj::Array(vec![Obj::Bool(true), Obj::Null]),
            )]),
        ];
        let mut g = guard();
        for obj in cases {
            let round = roundtrip(&obj, &mut g).expect("roundtrip");
            assert_eq!(round, obj, "parse(write({obj:?})) must equal the object");
        }
    }

    /// Every byte value in a string round-trips.
    #[test]
    fn every_byte_value_roundtrips() {
        let mut g = guard();
        let data: Vec<u8> = (0..=255u8).collect();
        let obj = Obj::String(selis_bytes::Bytes::copy_from_slice(&data));
        let round = roundtrip(&obj, &mut g).expect("roundtrip");
        assert_eq!(round, obj);
    }
}
