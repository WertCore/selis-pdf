//! CMap parsing (SL-3.FONT.07) — ISO 32000-2:2020 §9.7.5.
//!
//! A CMap maps character codes to CIDs (for a CIDFont's `/Encoding`) or to
//! Unicode (a `/ToUnicode` CMap, consumed by text extraction in SL-3.TEXT.02).
//! This module owns the parser and the code → CID resolution. The CMap
//! language is a restricted PostScript; the operators handled:
//!
//! * `begincidrange` / `endcidrange` — `<start> <end> <cid>` triples
//!   (§9.7.5.1);
//! * `begincidchar` / `endcidchar` — `<code> <cid>` pairs;
//! * `beginnotdefrange` / `endnotdefrange` — recorded but not used for
//!   mapping;
//! * `/WMode n def` — the writing mode (0 horizontal, 1 vertical);
//! * `usecmap /Name` — a reference to another CMap, surfaced for the caller
//!   to resolve.
//!
//! Numbers may be decimal or hexadecimal (`<00FF>`). A malformed CMap is a
//! deviation — `Ok(None)` — never an error.

use selis_error::{err, Code, Result};
use selis_sandbox::BudgetGuard;

/// One `begincidrange` entry: codes `first..=last` map to the CIDs
/// `cid..=cid + (last - first)`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CidRange {
    /// The first character code.
    pub first: u32,
    /// The last character code.
    pub last: u32,
    /// The CID of `first`.
    pub cid: u16,
}

/// A parsed CMap.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct CMap {
    /// `begincidrange` ranges.
    pub cid_ranges: Vec<CidRange>,
    /// `begincidchar` single mappings.
    pub cid_chars: Vec<(u32, u16)>,
    /// `beginnotdefrange` ranges (informational).
    pub notdef_ranges: Vec<CidRange>,
    /// `beginbfchar` single code → Unicode mappings.
    pub bf_chars: Vec<(u32, u32)>,
    /// `beginbfrange` code range → Unicode base mappings.
    pub bf_ranges: Vec<BfRange>,
    /// The writing mode (`/WMode`): 0 horizontal, 1 vertical.
    pub wmode: u8,
    /// A `usecmap /Name` reference, if any.
    pub uses: Option<String>,
}

/// One `beginbfrange` entry: codes `first..=last` map to the Unicodes
/// `unicode..=unicode + (last - first)`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BfRange {
    /// The first character code.
    pub first: u32,
    /// The last character code.
    pub last: u32,
    /// The Unicode of `first`.
    pub unicode: u32,
}

impl CMap {
    /// The CID for a character code, or `None`.
    ///
    /// Single `begincidchar` mappings win; otherwise the enclosing
    /// `begincidrange`.
    #[must_use]
    pub fn code_to_cid(&self, code: u32) -> Option<u16> {
        if let Some((_, cid)) = self.cid_chars.iter().find(|(c, _)| *c == code) {
            return Some(*cid);
        }
        for r in &self.cid_ranges {
            if code >= r.first && code <= r.last {
                let delta = u16::try_from(code.saturating_sub(r.first)).unwrap_or(u16::MAX);
                return Some(r.cid.saturating_add(delta));
            }
        }
        None
    }

    /// The Unicode for a character code, from a `/ToUnicode` CMap
    /// (`beginbfchar`/`beginbfrange`), or `None`.
    ///
    /// Single `beginbfchar` mappings win; otherwise the enclosing
    /// `beginbfrange`.
    #[must_use]
    pub fn unicode_map(&self, code: u32) -> Option<u32> {
        if let Some((_, uni)) = self.bf_chars.iter().find(|(c, _)| *c == code) {
            return Some(*uni);
        }
        for r in &self.bf_ranges {
            if code >= r.first && code <= r.last {
                return Some(r.unicode.saturating_add(code.saturating_sub(r.first)));
            }
        }
        None
    }
}

/// A CMap numeric value as a `u32`, clamping negatives and truncating
/// fractions (CMap numbers are non-negative integers in practice).
fn num_to_u32(v: f64) -> u32 {
    if !v.is_finite() || v < 0.0 {
        return 0;
    }
    let t = v.trunc();
    if t >= u32::MAX as f64 {
        return u32::MAX;
    }
    // t is finite, non-negative, and below u32::MAX — exact in range.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    {
        t as u32
    }
}

/// A CMap numeric value as a `u16` (CID), with the same clamping.
fn num_to_u16(v: f64) -> u16 {
    if !v.is_finite() || v < 0.0 {
        return 0;
    }
    let t = v.trunc();
    if t >= u16::MAX as f64 {
        return u16::MAX;
    }
    // t is finite, non-negative, and below u16::MAX — exact in range.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    {
        t as u16
    }
}

/// Parse a CMap stream.
///
/// # Budget
///
/// Charges the CMap bytes; the mapping tables are derived from the stream.
///
/// # Malformed Input
///
/// `BUDGET_BYTES` when exhausted; a stream that cannot be parsed yields
/// `Ok(None)` — a deviation.
pub fn parse_cmap(data: &[u8], g: &mut BudgetGuard<'_>) -> Result<Option<CMap>> {
    g.charge(
        selis_sandbox::Resource::Bytes,
        u64::try_from(data.len()).unwrap_or(u64::MAX),
    )?;
    let mut cmap = CMap::default();
    let mut tok = Tokenizer::new(data);
    // Skip to the body, collecting `/WMode` and `usecmap`.
    while let Some(t) = tok.next() {
        match t {
            Token::Name(b"WMode") => {
                if let Some(Token::Number(n)) = tok.next() {
                    cmap.wmode = if n >= 1.0 { 1 } else { 0 };
                }
            }
            Token::Raw(b"usecmap") => {
                if let Some(Token::Name(name)) = tok.next() {
                    cmap.uses = Some(String::from_utf8_lossy(name).to_string());
                }
            }
            Token::Raw(b"begincidrange") => {
                collect_triples(&mut tok, &mut cmap.cid_ranges)?;
            }
            Token::Raw(b"begincidchar") => {
                while let Some(Token::Number(start)) = tok.next() {
                    let Some(Token::Number(cid)) = tok.next() else {
                        return Ok(None);
                    };
                    cmap.cid_chars.push((num_to_u32(start), num_to_u16(cid)));
                }
            }
            Token::Raw(b"beginnotdefrange") => {
                collect_triples(&mut tok, &mut cmap.notdef_ranges)?;
            }
            Token::Raw(b"beginbfchar") => {
                while let Some(Token::Number(start)) = tok.next() {
                    let Some(Token::Number(uni)) = tok.next() else {
                        return Ok(None);
                    };
                    cmap.bf_chars.push((num_to_u32(start), num_to_u32(uni)));
                }
            }
            Token::Raw(b"beginbfrange") => {
                collect_bfrange(&mut tok, &mut cmap.bf_ranges)?;
            }
            _ => {}
        }
    }
    if cmap.cid_ranges.is_empty()
        && cmap.cid_chars.is_empty()
        && cmap.bf_chars.is_empty()
        && cmap.bf_ranges.is_empty()
    {
        return Ok(None); // no mapping: not a CMap
    }
    Ok(Some(cmap))
}

/// Collect `start end unicode` triples until `endbfrange`.
///
/// The explicit-destination array form (`<start> <end> [u1 u2 ...]`) is
/// skipped; a `/ToUnicode` with that form falls back to `None` for the codes.
fn collect_bfrange(tok: &mut Tokenizer<'_>, out: &mut Vec<BfRange>) -> Result<()> {
    loop {
        let Some(Token::Number(start)) = tok.next() else {
            return Ok(()); // endbfrange or end of stream
        };
        let Some(Token::Number(last)) = tok.next() else {
            return Ok(());
        };
        let Some(Token::Number(uni)) = tok.next() else {
            return Ok(());
        };
        out.push(BfRange {
            first: num_to_u32(start),
            last: num_to_u32(last),
            unicode: num_to_u32(uni),
        });
    }
}

/// Collect `start end cid` triples until `endcidrange`.
fn collect_triples(tok: &mut Tokenizer<'_>, out: &mut Vec<CidRange>) -> Result<()> {
    loop {
        let Some(Token::Number(start)) = tok.next() else {
            return Ok(()); // endcidrange or end of stream
        };
        let Some(Token::Number(last)) = tok.next() else {
            return Ok(());
        };
        let Some(Token::Number(cid)) = tok.next() else {
            return Ok(());
        };
        out.push(CidRange {
            first: num_to_u32(start),
            last: num_to_u32(last),
            cid: num_to_u16(cid),
        });
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Token<'a> {
    /// A `/name` or operator name.
    Name(&'a [u8]),
    /// A bare word (`begincidrange`, `endcidrange`, `def`, `usecmap`).
    Raw(&'a [u8]),
    /// A decimal or hexadecimal number.
    Number(f64),
}

/// A minimal CMap tokenizer (whitespace- and delimiter-separated).
struct Tokenizer<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Tokenizer<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }

    fn next(&mut self) -> Option<Token<'a>> {
        self.skip_space();
        let start = self.pos;
        let c = self.data.get(self.pos).copied()?;
        if c == b'/' {
            self.pos = self.pos.saturating_add(1);
            let name_start = self.pos;
            self.skip_word();
            return Some(Token::Name(self.data.get(name_start..self.pos)?));
        }
        if c == b'<' {
            // Hex number `<00FF>`.
            self.pos = self.pos.saturating_add(1);
            let hex_start = self.pos;
            while self.data.get(self.pos).copied().is_some_and(|b| b != b'>') {
                self.pos = self.pos.saturating_add(1);
            }
            if self.data.get(self.pos).copied()? != b'>' {
                return None;
            }
            self.pos = self.pos.saturating_add(1); // skip '>'
            let hex = self.data.get(hex_start..self.pos.saturating_sub(1))?;
            let text = std::str::from_utf8(hex).ok()?;
            let value = u32::from_str_radix(text, 16).ok()?;
            return Some(Token::Number(f64::from(value)));
        }
        if c.is_ascii_digit() || c == b'-' || c == b'+' || c == b'.' {
            self.skip_number();
            let text = self.data.get(start..self.pos)?;
            let value = std::str::from_utf8(text).ok()?.parse().ok()?;
            return Some(Token::Number(value));
        }
        self.skip_word();
        let word = self.data.get(start..self.pos)?;
        Some(Token::Raw(word))
    }

    fn skip_space(&mut self) {
        while self
            .data
            .get(self.pos)
            .copied()
            .is_some_and(|b| b.is_ascii_whitespace())
        {
            self.pos = self.pos.saturating_add(1);
        }
    }

    fn skip_word(&mut self) {
        while self
            .data
            .get(self.pos)
            .copied()
            .is_some_and(|b| !b.is_ascii_whitespace() && !matches!(b, b'/' | b'<' | b'>'))
        {
            self.pos = self.pos.saturating_add(1);
        }
    }

    fn skip_number(&mut self) {
        while self
            .data
            .get(self.pos)
            .copied()
            .is_some_and(|b| b.is_ascii_digit() || matches!(b, b'.' | b'-' | b'+' | b'e' | b'E'))
        {
            self.pos = self.pos.saturating_add(1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use selis_sandbox::Budget;

    fn guard() -> BudgetGuard<'static> {
        Budget::unlimited().guard()
    }

    #[test]
    fn cidrange_mapping() {
        let mut g = guard();
        let src = b"/CMapName /Adobe-Identity-UCS def\n1 begincodespacerange\n<0000> <FFFF>\nendcodespacerange\n1 begincidrange\n<0000> <005D> 1\nendcidrange\n";
        let cmap = parse_cmap(src, &mut g).expect("parse").expect("cmap");
        assert_eq!(cmap.code_to_cid(0x0000), Some(1));
        assert_eq!(cmap.code_to_cid(0x005D), Some(1 + 0x5D));
        assert_eq!(cmap.code_to_cid(0x005E), None);
        assert_eq!(cmap.wmode, 0);
    }

    #[test]
    fn cidchar_and_wmode() {
        let mut g = guard();
        let src = b"/WMode 1 def\n2 begincidchar\n<0020> 1\n<0021> 2\nendcidchar\n";
        let cmap = parse_cmap(src, &mut g).expect("parse").expect("cmap");
        assert_eq!(cmap.wmode, 1);
        assert_eq!(cmap.code_to_cid(0x20), Some(1));
        assert_eq!(cmap.code_to_cid(0x21), Some(2));
    }

    #[test]
    fn decimal_numbers_and_usecmap() {
        let mut g = guard();
        let src = b"/WMode 0 def\n1 begincidrange\n65 90 101\nendcidrange\nusecmap /Another def\n";
        let cmap = parse_cmap(src, &mut g).expect("parse").expect("cmap");
        assert_eq!(cmap.code_to_cid(65), Some(101));
        assert_eq!(cmap.code_to_cid(90), Some(101 + 25));
        assert_eq!(cmap.uses.as_deref(), Some("Another"));
    }

    #[test]
    fn malformed_cmap_is_a_deviation() {
        let mut g = guard();
        // No mapping operators at all.
        assert!(parse_cmap(b"/CMapName /X def\n", &mut g)
            .expect("parse")
            .is_none());
        // Garbage.
        assert!(parse_cmap(b"not a cmap at all", &mut g)
            .expect("parse")
            .is_none());
    }

    #[test]
    fn notdef_ranges_are_recorded() {
        let mut g = guard();
        let src = b"1 beginnotdefrange\n<0000> <001F> 0\nendnotdefrange\n1 begincidrange\n<0020> <0020> 5\nendcidrange\n";
        let cmap = parse_cmap(src, &mut g).expect("parse").expect("cmap");
        assert_eq!(cmap.notdef_ranges.len(), 1);
        assert_eq!(cmap.code_to_cid(0x20), Some(5));
    }
}
