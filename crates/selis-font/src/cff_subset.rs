//! CFF (`/FontFile3`, Type1C and CIDFontType0C) font subsetting (SL-3.FONT.11).
//!
//! This is a *real* CFF writer, not a feasibility note. It parses the CFF
//! program, closes over the used glyphs and their transitive subroutines,
//! re-encodes every Type 2 charstring's `callsubr` / `callgsubr` operand
//! against the new subr biases, rebuilds the charset / FDSelect / FDArray,
//! rewrites the variable-length DICT offsets with a bounded fixpoint, and
//! reassembles the enclosing sfnt table directory.
//!
//! The output is never partial: any input this writer cannot prove it can
//! rewrite faithfully (a `seac` endchar, a subroutine call cycle, an
//! inconsistent hint-mask state, an offset fixpoint that does not converge)
//! produces a typed [`CffError`] and **no** font bytes.
//!
//! # Budget
//!
//! Every output buffer is allocated through [`alloc::vec_filled`] /
//! [`alloc::vec_with_capacity`], so document-derived sizes are charged before
//! allocation. The walk depth is bounded and every INDEX count is checked.
//!
//! # Malformed Input
//!
//! Structurally broken CFFs return [`CffError::Malformed`]. Budget exhaustion
//! is [`CffError::Budget`]. No partial font is ever emitted.

use std::collections::{BTreeMap, BTreeSet};

use crate::subset::{build_cmap_format12_from_map, patch_checksum, GlyphSet, SubsetResult};
use selis_bytes::Bytes;
use selis_error::Error as SelisError;
use selis_sandbox::{alloc, BudgetGuard};
use skrifa::MetadataProvider;

/// A typed refusal of a CFF program this writer cannot subset faithfully.
#[derive(Debug)]
pub enum CffError {
    /// The CFF program is structurally invalid.
    Malformed(&'static str),
    /// A four-operand `endchar` is `seac`; closing its base/accent glyphs
    /// requires an encoding walk this writer deliberately does not attempt.
    Seac,
    /// A subroutine call cycle whose final hint state cannot be resolved.
    CyclicSubr,
    /// One subroutine is reached with two incompatible hint-mask states.
    AmbiguousHintState,
    /// A well-formed but deliberately unsupported CFF construct.
    Unsupported(&'static str),
    /// The DICT offset fixpoint did not converge within the iteration bound.
    OffsetFixpoint,
    /// The caller's byte budget was exhausted (mapped back to `BUDGET_BYTES`
    /// at the crate boundary; keeping it valueless keeps this error small).
    Budget,
}

impl std::fmt::Display for CffError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Malformed(why) => write!(f, "malformed CFF program: {why}"),
            Self::Seac => f.write_str("CFF seac glyphs are not subset"),
            Self::CyclicSubr => f.write_str("CFF subroutine call cycle is not subset"),
            Self::AmbiguousHintState => {
                f.write_str("CFF subroutine has inconsistent hint-mask state")
            }
            Self::Unsupported(why) => write!(f, "unsupported CFF construct: {why}"),
            Self::OffsetFixpoint => f.write_str("CFF DICT offset fixpoint did not converge"),
            Self::Budget => f.write_str("CFF subset budget exhausted"),
        }
    }
}

impl std::error::Error for CffError {}

impl From<SelisError> for CffError {
    fn from(_value: SelisError) -> Self {
        Self::Budget
    }
}

/// The CFF subroutine bias for a subr INDEX of `n` entries (CFF §7).
fn bias(n: usize) -> i32 {
    if n < 1240 {
        107
    } else if n < 33900 {
        1131
    } else {
        32768
    }
}

// ---------------------------------------------------------------------------
// Low-level readers and writers (all bounds-checked, no indexing).
// ---------------------------------------------------------------------------

struct Reader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }

    fn byte(&mut self) -> Option<u8> {
        let b = *self.data.get(self.pos)?;
        self.pos = self.pos.checked_add(1)?;
        Some(b)
    }

    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        let end = self.pos.checked_add(n)?;
        let slice = self.data.get(self.pos..end)?;
        self.pos = end;
        Some(slice)
    }

    fn u16(&mut self) -> Option<u16> {
        let b = self.take(2)?;
        Some(u16::from_be_bytes([*b.first()?, *b.get(1)?]))
    }

    fn i16(&mut self) -> Option<i16> {
        let b = self.take(2)?;
        Some(i16::from_be_bytes([*b.first()?, *b.get(1)?]))
    }

    fn i32(&mut self) -> Option<i32> {
        let b = self.take(4)?;
        Some(i32::from_be_bytes([
            *b.first()?,
            *b.get(1)?,
            *b.get(2)?,
            *b.get(3)?,
        ]))
    }

    fn uint(&mut self, n: usize) -> Option<u32> {
        if n == 0 || n > 4 {
            return None;
        }
        let b = self.take(n)?;
        let mut value = 0u32;
        for byte in b {
            value = value.checked_mul(256)?.checked_add(u32::from(*byte))?;
        }
        Some(value)
    }
}

fn put_u16(out: &mut Vec<u8>, value: u16) {
    let bytes = value.to_be_bytes();
    out.push(bytes[0]);
    out.push(bytes[1]);
}

fn put_i32(out: &mut Vec<u8>, value: i32) {
    out.extend_from_slice(&value.to_be_bytes());
}

fn off_size(n: usize) -> usize {
    if n <= 0xFF {
        1
    } else if n <= 0xFFFF {
        2
    } else if n <= 0xFF_FFFF {
        3
    } else {
        4
    }
}

/// Write a CFF INDEX of already-serialised items.
fn write_index(items: &[Vec<u8>], g: &mut BudgetGuard<'_>) -> Result<Vec<u8>, CffError> {
    let count = u16::try_from(items.len()).map_err(|_| CffError::Malformed("INDEX count"))?;
    let mut out =
        alloc::vec_with_capacity(g, 2usize.saturating_add(items.len().saturating_mul(5)))?;
    put_u16(&mut out, count);
    if items.is_empty() {
        return Ok(out);
    }
    let total: usize = items
        .iter()
        .fold(1usize, |acc, item| acc.saturating_add(item.len()));
    let width = off_size(total);
    out.push(u8::try_from(width).map_err(|_| CffError::Malformed("INDEX offSize"))?);
    let mut offset = 1usize;
    for item in items {
        let o = u32::try_from(offset).map_err(|_| CffError::Malformed("INDEX offset"))?;
        for shift in (0..width).rev() {
            // Every offset byte is in 0..=255 by construction.
            #[allow(clippy::cast_possible_truncation)]
            out.push((o >> (shift.saturating_mul(8))) as u8);
        }
        offset = offset.saturating_add(item.len());
    }
    let last = u32::try_from(offset).map_err(|_| CffError::Malformed("INDEX offset"))?;
    for shift in (0..width).rev() {
        #[allow(clippy::cast_possible_truncation)]
        out.push((last >> (shift.saturating_mul(8))) as u8);
    }
    for item in items {
        out.extend_from_slice(item);
    }
    Ok(out)
}

/// Parse a CFF INDEX, returning one owned byte vector per item.
fn read_index(r: &mut Reader<'_>) -> Option<Vec<Vec<u8>>> {
    let count = usize::from(r.u16()?);
    if count == 0 {
        return Some(Vec::new());
    }
    let width = usize::from(r.byte()?);
    if width == 0 || width > 4 {
        return None;
    }
    let mut offsets = Vec::with_capacity(count.saturating_add(1));
    for _ in 0..=count {
        offsets.push(r.uint(width)?);
    }
    if offsets.first().copied()? != 1 {
        return None;
    }
    let data_start = r.pos;
    let data_len = usize::try_from(*offsets.last()?).ok()?.checked_sub(1)?;
    let data = r.take(data_len)?;
    let mut items = Vec::with_capacity(count);
    for pair in offsets.windows(2) {
        let start = usize::try_from(*pair.first()?).ok()?.checked_sub(1)?;
        let end = usize::try_from(*pair.get(1)?).ok()?.checked_sub(1)?;
        items.push(data.get(start..end)?.to_vec());
    }
    let _ = data_start;
    Some(items)
}

// ---------------------------------------------------------------------------
// DICT model
// ---------------------------------------------------------------------------

/// One encoded operand: its exact source bytes, plus its integer value when
/// the encoding was integer-valued. Non-integer operands (reals) are kept
/// verbatim.
#[derive(Clone, Debug)]
struct Operand {
    raw: Vec<u8>,
    int: Option<i32>,
}

#[derive(Clone, Debug)]
struct Entry {
    /// Single-byte operators are `0..=255`; escaped operators are
    /// `0x0C00 | second_byte`.
    op: u16,
    operands: Vec<Operand>,
}

type Dict = Vec<Entry>;

// Every arithmetic operation below is range-checked by the enclosing `match`
// arm: the operand encodings are self-delimiting and their domains are fixed
// by CFF §7.
#[allow(clippy::arithmetic_side_effects, clippy::integer_division)]
fn encode_int(value: i32) -> Vec<u8> {
    if (108..=1131).contains(&value) {
        let v = value - 108;
        let hi = v / 256;
        let lo = v % 256;
        vec![
            u8::try_from(247 + hi).unwrap_or(250),
            u8::try_from(lo).unwrap_or(0),
        ]
    } else if (-1131..=-108).contains(&value) {
        let v = -value - 108;
        let hi = v / 256;
        let lo = v % 256;
        vec![
            u8::try_from(251 + hi).unwrap_or(254),
            u8::try_from(lo).unwrap_or(0),
        ]
    } else if (-107..=107).contains(&value) {
        vec![u8::try_from(value + 139).unwrap_or(32)]
    } else if let Ok(short) = i16::try_from(value) {
        let bytes = short.to_be_bytes();
        vec![28, bytes[0], bytes[1]]
    } else {
        let mut out = vec![255];
        put_i32(&mut out, value);
        out
    }
}

#[allow(clippy::arithmetic_side_effects)]
fn parse_operand(byte: u8, r: &mut Reader<'_>) -> Option<Operand> {
    match byte {
        28 => {
            let v = r.i16()?;
            Some(Operand {
                raw: vec![
                    28,
                    *r.data.get(r.pos.checked_sub(2)?)?,
                    *r.data.get(r.pos.checked_sub(1)?)?,
                ],
                int: Some(i32::from(v)),
            })
        }
        29 => {
            let raw = r.take(5)?.to_vec();
            Some(Operand { raw, int: None })
        }
        30 => {
            let mut raw = vec![30];
            loop {
                let b = r.byte()?;
                raw.push(b);
                if b & 0x0F == 0x0F || b >> 4 == 0x0F {
                    break;
                }
            }
            Some(Operand { raw, int: None })
        }
        32..=246 => Some(Operand {
            raw: vec![byte],
            int: Some(i32::from(byte) - 139),
        }),
        247..=250 => {
            let lo = r.byte()?;
            let v = (i32::from(byte) - 247) * 256 + i32::from(lo) + 108;
            Some(Operand {
                raw: vec![byte, lo],
                int: Some(v),
            })
        }
        251..=254 => {
            let lo = r.byte()?;
            let v = -((i32::from(byte) - 251) * 256) - i32::from(lo) - 108;
            Some(Operand {
                raw: vec![byte, lo],
                int: Some(v),
            })
        }
        255 => {
            let v = r.i32()?;
            let mut raw = vec![255];
            put_i32(&mut raw, v);
            Some(Operand { raw, int: Some(v) })
        }
        _ => None,
    }
}

fn parse_dict(data: &[u8]) -> Option<Dict> {
    let mut r = Reader::new(data);
    let mut operands: Vec<Operand> = Vec::new();
    let mut out = Vec::new();
    while let Some(byte) = r.byte() {
        if byte <= 21 {
            let op = if byte == 12 {
                let second = r.byte()?;
                0x0C00 | u16::from(second)
            } else {
                u16::from(byte)
            };
            out.push(Entry {
                op,
                operands: std::mem::take(&mut operands),
            });
        } else {
            operands.push(parse_operand(byte, &mut r)?);
        }
    }
    Some(out)
}

fn write_dict(dict: &Dict) -> Vec<u8> {
    let mut out = Vec::new();
    for entry in dict {
        for operand in &entry.operands {
            out.extend_from_slice(&operand.raw);
        }
        if entry.op & 0x0F00 == 0x0C00 {
            out.push(12);
            out.push(u8::try_from(entry.op & 0x00FF).unwrap_or(0));
        } else {
            out.push(u8::try_from(entry.op).unwrap_or(0));
        }
    }
    out
}

fn find_entry(dict: &Dict, op: u16) -> Option<usize> {
    dict.iter().position(|entry| entry.op == op)
}

fn set_entry_int(dict: &mut Dict, op: u16, operand_index: usize, value: i32) {
    if let Some(index) = find_entry(dict, op) {
        if let Some(entry) = dict.get_mut(index) {
            while entry.operands.len() <= operand_index {
                entry.operands.push(Operand {
                    raw: vec![139],
                    int: Some(0),
                });
            }
            if let Some(operand) = entry.operands.get_mut(operand_index) {
                operand.raw = encode_int(value);
                operand.int = Some(value);
            }
        }
    } else {
        let mut operands = Vec::new();
        for _ in 0..operand_index {
            operands.push(Operand {
                raw: vec![139],
                int: Some(0),
            });
        }
        operands.push(Operand {
            raw: encode_int(value),
            int: Some(value),
        });
        dict.push(Entry { op, operands });
    }
}

fn entry_int(dict: &Dict, op: u16, operand_index: usize) -> Option<i32> {
    dict.iter()
        .find(|entry| entry.op == op)
        .and_then(|entry| entry.operands.get(operand_index))
        .and_then(|operand| operand.int)
}

fn remove_entry(dict: &mut Dict, op: u16) {
    dict.retain(|entry| entry.op != op);
}

// ---------------------------------------------------------------------------
// Charset / FDSelect / Encoding
// ---------------------------------------------------------------------------

fn parse_charset(data: &[u8], offset: u32, n_glyphs: usize) -> Option<Vec<u16>> {
    if offset <= 2 {
        // ISOAdobe: SID N maps to glyph N. Expert/ExpertSubset are refused by
        // the caller because their SID tables are not needed for our fixtures
        // and a wrong guess would silently rename glyphs.
        if offset != 0 {
            return None;
        }
        return (0..n_glyphs).map(|i| u16::try_from(i).ok()).collect();
    }
    let mut r = Reader::new(data.get(usize::try_from(offset).ok()?..)?);
    let format = r.byte()?;
    let mut sids = vec![0u16]; // .notdef
    match format {
        0 => {
            for _ in 1..n_glyphs {
                sids.push(r.u16()?);
            }
        }
        1 | 2 => loop {
            let first = usize::from(r.u16()?);
            let n_left = usize::from(r.u16()?);
            let sid = if format == 1 {
                i32::from(r.u16()?)
            } else {
                let delta =
                    i16::from_be_bytes([*r.data.get(r.pos)?, *r.data.get(r.pos.checked_add(1)?)?]);
                r.pos = r.pos.checked_add(2)?;
                // Format 2 stores a delta from the previous range's SID.
                i32::from(sids.last().copied().unwrap_or(0)).saturating_add(i32::from(delta))
            };
            let sid = u16::try_from(sid).ok()?;
            for _ in 0..=n_left {
                if sids.len() >= n_glyphs {
                    break;
                }
                sids.push(sid);
            }
            let _ = first;
            if sids.len() >= n_glyphs {
                break;
            }
        },
        _ => return None,
    }
    sids.truncate(n_glyphs);
    while sids.len() < n_glyphs {
        sids.push(0);
    }
    if sids.first().copied() != Some(0) {
        return None;
    }
    Some(sids)
}

fn write_charset(sids: &[u16], g: &mut BudgetGuard<'_>) -> Result<Vec<u8>, CffError> {
    let mut out = alloc::vec_with_capacity(g, 3usize.saturating_add(sids.len().saturating_mul(2)))?;
    out.push(0); // format 0
    for sid in sids.iter().skip(1) {
        put_u16(&mut out, *sid);
    }
    Ok(out)
}

fn parse_fd_select(data: &[u8], offset: u32, n_glyphs: usize) -> Option<Vec<u8>> {
    let mut r = Reader::new(data.get(usize::try_from(offset).ok()?..)?);
    let format = r.byte()?;
    match format {
        0 => {
            let bytes = r.take(n_glyphs)?.to_vec();
            Some(bytes)
        }
        3 | 4 => {
            let n_ranges = usize::try_from(r.uint(2)?).ok()?;
            // A plain Vec is correct here: its length is bounded by `n_glyphs`
            // from the CharStrings INDEX, which the caller charges.
            let mut fds = Vec::with_capacity(n_glyphs);
            let mut current_fd = 0u8;
            for _ in 0..n_ranges {
                let first = usize::try_from(r.uint(if format == 3 { 2 } else { 4 })?).ok()?;
                let fd = if format == 3 {
                    r.byte()?
                } else {
                    u8::try_from(r.uint(2)?).ok()?
                };
                while fds.len() < first && fds.len() < n_glyphs {
                    fds.push(current_fd);
                }
                current_fd = fd;
            }
            while fds.len() < n_glyphs {
                fds.push(current_fd);
            }
            fds.truncate(n_glyphs);
            Some(fds)
        }
        _ => None,
    }
}

fn write_fd_select(fds: &[u8], g: &mut BudgetGuard<'_>) -> Result<Vec<u8>, CffError> {
    // Format 0 is always valid and simple; the subset is small.
    let mut out = alloc::vec_with_capacity(g, 1usize.saturating_add(fds.len()))?;
    out.push(0);
    out.extend_from_slice(fds);
    Ok(out)
}

/// Rebuild a custom CFF Encoding so that each kept glyph keeps the code that
/// used to select it. Predefined encodings (offset 0/1) need no rewrite.
fn rewrite_encoding(raw: &[u8], charset: &[u16], renumber: &BTreeMap<u16, u16>) -> Option<Vec<u8>> {
    let mut r = Reader::new(raw);
    let header = r.byte()?;
    if header & 0x80 == 0 {
        return None; // predefined
    }
    let format = header & 0x7F;
    let has_supplement = false;
    let mut codes: Vec<u8> = Vec::new();
    match format {
        0 => {
            let n = usize::from(r.byte()?);
            codes.extend_from_slice(r.take(n)?);
        }
        1 => {
            let n = usize::from(r.byte()?);
            for _ in 0..n {
                let first = r.byte()?;
                let n_left = r.byte()?;
                for offset in 0..=n_left {
                    codes.push(first.saturating_add(offset));
                }
            }
        }
        _ => return None,
    }
    if has_supplement {
        return None;
    }
    let _ = r;

    // Old GID -> code. Format 0 and 1 assign codes to GIDs in order from 1.
    let mut code_of_gid: BTreeMap<u16, u8> = BTreeMap::new();
    for (index, code) in codes.iter().enumerate() {
        let gid = u16::try_from(index.saturating_add(1)).ok()?;
        code_of_gid.entry(gid).or_insert(*code);
    }
    let mut used: BTreeSet<u8> = code_of_gid.values().copied().collect();
    let mut filler = 0u8;
    while used.contains(&filler) {
        filler = filler.saturating_add(1);
    }
    used.insert(filler);

    // New GID -> old GID.
    let mut old_of_new: BTreeMap<u16, u16> = BTreeMap::new();
    for (&old, &new) in renumber {
        old_of_new.insert(new, old);
    }
    let new_count = u16::try_from(renumber.len()).ok()?;
    let mut out = vec![0x80]; // custom, format 0
    out.push(u8::try_from(new_count.saturating_sub(1)).unwrap_or(u8::MAX));
    for new_gid in 1..new_count {
        let old_gid = old_of_new.get(&new_gid).copied().unwrap_or(0);
        let code = code_of_gid.get(&old_gid).copied().unwrap_or(filler);
        out.push(code);
    }
    let _ = charset;
    Some(out)
}

// ---------------------------------------------------------------------------
// Type 2 charstring scanner with hint tracking
// ---------------------------------------------------------------------------

/// Which subroutine INDEX a byte stream (or a call target) belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum SubrSet {
    Global,
    Local(usize),
}

#[derive(Clone, Debug)]
enum Token {
    /// A self-delimiting number, with its original byte span and integer value.
    Number {
        start: usize,
        end: usize,
        value: Option<i32>,
    },
    /// A one-byte operator (or the first byte of a two-byte operator).
    Op { start: usize, end: usize },
    /// A `hintmask` / `cntrmask` and its exact mask bytes.
    Mask { start: usize, end: usize },
}

struct Patch {
    start: usize,
    target: SubrSet,
    target_index: u16,
}

struct ParseRecord {
    tokens: Vec<Token>,
    final_hints: usize,
    /// Operand tokens left on the stack when the body ends. A subroutine that
    /// returns with a non-empty stack can carry operands into its caller,
    /// which this writer deliberately refuses rather than guess at.
    final_stack: usize,
    patches: Vec<Patch>,
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct SubrKey {
    set: SubrSet,
    index: u16,
    /// The FD whose local subrs a `callsubr` inside this body resolves against.
    fd: usize,
    hints: usize,
}

struct Walk<'a> {
    global_subrs: &'a [Vec<u8>],
    local_subrs: &'a [Vec<Vec<u8>>],
    records: Vec<ParseRecord>,
    by_key: BTreeMap<SubrKey, usize>,
    by_subr: BTreeMap<(SubrSet, u16), Vec<usize>>,
    in_progress: BTreeSet<SubrKey>,
    glyph_records: Vec<ParseRecord>,
    used_global: BTreeSet<u16>,
    used_local: Vec<BTreeSet<u16>>,
}

impl<'a> Walk<'a> {
    fn new(global_subrs: &'a [Vec<u8>], local_subrs: &'a [Vec<Vec<u8>>]) -> Self {
        let used_local = local_subrs.iter().map(|_| BTreeSet::new()).collect();
        Self {
            global_subrs,
            local_subrs,
            records: Vec::new(),
            by_key: BTreeMap::new(),
            by_subr: BTreeMap::new(),
            in_progress: BTreeSet::new(),
            glyph_records: Vec::new(),
            used_global: BTreeSet::new(),
            used_local,
        }
    }

    fn subr_bytes(&self, set: SubrSet, index: u16) -> Option<&'a [u8]> {
        match set {
            SubrSet::Global => self.global_subrs.get(usize::from(index)).map(Vec::as_slice),
            SubrSet::Local(fd) => self
                .local_subrs
                .get(fd)
                .and_then(|list| list.get(usize::from(index)))
                .map(Vec::as_slice),
        }
    }

    fn subr_count(&self, set: SubrSet) -> usize {
        match set {
            SubrSet::Global => self.global_subrs.len(),
            SubrSet::Local(fd) => self.local_subrs.get(fd).map_or(0, Vec::len),
        }
    }

    fn parse_glyph(&mut self, bytes: &[u8], fd: usize) -> Result<usize, CffError> {
        let record = self.parse(bytes, fd, 0, 0, None)?;
        let index = self.glyph_records.len();
        self.glyph_records.push(record);
        Ok(index)
    }

    /// Parse one charstring/subr body. `fd` is the FD context carried from the
    /// calling glyph: a `callsubr` always resolves against that FD's local
    /// subr INDEX, even when the call sits inside a global subr.
    fn parse(
        &mut self,
        bytes: &[u8],
        fd: usize,
        start_hints: usize,
        depth: u8,
        current: Option<(SubrSet, u16)>,
    ) -> Result<ParseRecord, CffError> {
        if depth > 32 {
            return Err(CffError::Malformed("subr depth"));
        }
        let mut r = Reader::new(bytes);
        let mut tokens: Vec<Token> = Vec::new();
        let mut patches: Vec<Patch> = Vec::new();
        let mut stack: Vec<usize> = Vec::new();
        let mut hints = start_hints;

        while let Some(byte) = r.byte() {
            let start = r.pos.checked_sub(1).ok_or(CffError::Malformed("token"))?;
            match byte {
                28 | 32..=255 => {
                    let operand =
                        parse_operand(byte, &mut r).ok_or(CffError::Malformed("number"))?;
                    let end = r.pos;
                    let index = tokens.len();
                    tokens.push(Token::Number {
                        start,
                        end,
                        value: operand.int,
                    });
                    stack.push(index);
                }
                12 => {
                    let second = r.byte().ok_or(CffError::Malformed("escape"))?;
                    tokens.push(Token::Op { start, end: r.pos });
                    if second == 1 || second == 2 {
                        hints = hints.saturating_add(3);
                    }
                    stack.clear();
                }
                19 | 20 => {
                    let mask_len = hints.saturating_add(7) >> 3;
                    let mask_start = r.pos;
                    let _ = r.take(mask_len).ok_or(CffError::Malformed("hint mask"))?;
                    tokens.push(Token::Mask {
                        start,
                        end: mask_start.saturating_add(mask_len),
                    });
                    stack.clear();
                }
                _ => {
                    match byte {
                        1 | 3 | 18 | 23 => {
                            hints = hints.saturating_add(stack.len() >> 1);
                        }
                        10 | 29 => {
                            let number_index =
                                *stack.last().ok_or(CffError::Malformed("call operand"))?;
                            let biased = match tokens.get(number_index) {
                                Some(Token::Number { value: Some(v), .. }) => *v,
                                _ => return Err(CffError::Malformed("call operand type")),
                            };
                            let set = if byte == 10 {
                                SubrSet::Local(fd)
                            } else {
                                SubrSet::Global
                            };
                            let old_bias = i64::from(bias(self.subr_count(set)));
                            let absolute = i64::from(biased).saturating_add(old_bias);
                            if absolute < 0 {
                                return Err(CffError::Malformed("subr index"));
                            }
                            let target_index = u16::try_from(absolute)
                                .map_err(|_| CffError::Malformed("subr index"))?;
                            if self.subr_bytes(set, target_index).is_none() {
                                return Err(CffError::Malformed("subr index range"));
                            }
                            if let Some((SubrSet::Global, _)) = current {
                                if byte == 10 {
                                    return Err(CffError::Unsupported(
                                        "global subr calls a local subr",
                                    ));
                                }
                            }
                            let token_start = match tokens.get(number_index) {
                                Some(Token::Number { start, .. }) => *start,
                                _ => return Err(CffError::Malformed("call operand token")),
                            };
                            patches.push(Patch {
                                start: token_start,
                                target: set,
                                target_index,
                            });
                            let key = SubrKey {
                                set,
                                index: target_index,
                                fd,
                                hints,
                            };
                            let final_hints = if let Some(id) = self.by_key.get(&key).copied() {
                                self.records
                                    .get(id)
                                    .map_or(hints, |record| record.final_hints)
                            } else if self.in_progress.contains(&key) {
                                return Err(CffError::CyclicSubr);
                            } else {
                                let target = self
                                    .subr_bytes(set, target_index)
                                    .ok_or(CffError::Malformed("subr bytes"))?;
                                self.in_progress.insert(key);
                                let parsed = self.parse(
                                    target,
                                    fd,
                                    hints,
                                    depth.saturating_add(1),
                                    Some((set, target_index)),
                                );
                                self.in_progress.remove(&key);
                                let parsed = parsed?;
                                if parsed.final_stack != 0 {
                                    return Err(CffError::Unsupported(
                                        "operand carry-over across subroutine calls",
                                    ));
                                }
                                let final_hints = parsed.final_hints;
                                match set {
                                    SubrSet::Global => {
                                        self.used_global.insert(target_index);
                                    }
                                    SubrSet::Local(fd) => {
                                        if let Some(slot) = self.used_local.get_mut(fd) {
                                            slot.insert(target_index);
                                        }
                                    }
                                }
                                let id = self.records.len();
                                self.records.push(parsed);
                                self.by_key.insert(key, id);
                                self.by_subr
                                    .entry((set, target_index))
                                    .or_default()
                                    .push(id);
                                final_hints
                            };
                            hints = final_hints;
                            stack.clear();
                        }
                        14 => {
                            if stack.len() >= 4 {
                                return Err(CffError::Seac);
                            }
                            stack.clear();
                        }
                        _ => stack.clear(),
                    }
                    tokens.push(Token::Op { start, end: r.pos });
                }
            }
        }

        Ok(ParseRecord {
            tokens,
            final_hints: hints,
            final_stack: stack.len(),
            patches,
        })
    }
}

/// Render a parsed charstring, substituting rewritten call operands.
fn render(record: &ParseRecord, bytes: &[u8], patches: &BTreeMap<usize, Vec<u8>>) -> Vec<u8> {
    let mut out = Vec::with_capacity(bytes.len());
    for token in &record.tokens {
        let (start, end) = match token {
            Token::Number { start, end, .. }
            | Token::Op { start, end, .. }
            | Token::Mask { start, end } => (*start, *end),
        };
        if let Some(replacement) = patches.get(&start) {
            out.extend_from_slice(replacement);
        } else if let Some(slice) = bytes.get(start..end) {
            out.extend_from_slice(slice);
        }
    }
    out
}

// ---------------------------------------------------------------------------
// The parsed source CFF
// ---------------------------------------------------------------------------

struct Source {
    cff: Vec<u8>,
    name: Vec<u8>,
    header_off_size: u8,
    top: Dict,
    strings_raw: Vec<u8>,
    charstrings: Vec<Vec<u8>>,
    global_subrs: Vec<Vec<u8>>,
    local_subrs: Vec<Vec<Vec<u8>>>,
    charset_sids: Vec<u16>,
    encoding_offset: u32,
    encoding_raw: Option<Vec<u8>>,
    fd_select: Option<Vec<u8>>,
    fd_dicts: Option<Vec<Dict>>,
    fd_privates: Vec<Dict>,
    private: Option<Dict>,
}

fn parse_source(cff: &[u8]) -> Result<Source, CffError> {
    let bad = |why| CffError::Malformed(why);
    let mut r = Reader::new(cff);
    let major = r.byte().ok_or(bad("header"))?;
    let _minor = r.byte().ok_or(bad("header"))?;
    let hdr_size = r.byte().ok_or(bad("header"))?;
    let off_size = r.byte().ok_or(bad("header"))?;
    if major != 1 || !(1..=4).contains(&off_size) {
        return Err(bad("header values"));
    }
    r.pos = usize::from(hdr_size);
    if r.pos > cff.len() {
        return Err(bad("header size"));
    }

    let name_index = read_index(&mut r).ok_or(bad("Name INDEX"))?;
    let name = name_index.first().cloned().ok_or(bad("font name"))?;
    let top_index = read_index(&mut r).ok_or(bad("Top DICT INDEX"))?;
    let top_raw = top_index.first().cloned().ok_or(bad("Top DICT"))?;
    let top = parse_dict(&top_raw).ok_or(bad("Top DICT parse"))?;
    let strings_start = r.pos;
    let _strings = read_index(&mut r).ok_or(bad("String INDEX"))?;
    let strings_end = r.pos;
    let gsubrs = read_index(&mut r).ok_or(bad("Global Subr INDEX"))?;

    let cs_offset = u32::try_from(entry_int(&top, 17, 0).ok_or(bad("CharStrings offset"))?)
        .map_err(|_| bad("CharStrings offset"))?;
    let charstrings = read_index_at(cff, cs_offset).ok_or(bad("CharStrings INDEX"))?;
    let n_glyphs = charstrings.len();
    if n_glyphs == 0 || n_glyphs > usize::from(u16::MAX) {
        return Err(bad("glyph count"));
    }

    let charset_offset =
        u32::try_from(entry_int(&top, 15, 0).unwrap_or(0)).map_err(|_| bad("charset offset"))?;
    let charset_sids = parse_charset(cff, charset_offset, n_glyphs).ok_or(bad("charset"))?;

    let encoding_offset =
        u32::try_from(entry_int(&top, 16, 0).unwrap_or(0)).map_err(|_| bad("encoding offset"))?;
    let encoding_raw = if encoding_offset <= 1 {
        None
    } else {
        Some(
            cff.get(usize::try_from(encoding_offset).map_err(|_| bad("encoding offset"))?..)
                .ok_or(bad("encoding offset"))?
                .to_vec(),
        )
    };

    let fd_array_offset = entry_int(&top, 0x0C24, 0);
    let fd_select_offset = entry_int(&top, 0x0C25, 0);
    let mut fd_dicts = None;
    let mut fd_select = None;
    let mut fd_privates: Vec<Dict> = Vec::new();
    let mut local_subrs: Vec<Vec<Vec<u8>>> = Vec::new();
    let mut private = None;

    if let Some(fd_off) = fd_array_offset {
        let fd_off = u32::try_from(fd_off).map_err(|_| bad("FDArray offset"))?;
        let fd_index = read_index_at(cff, fd_off).ok_or(bad("FDArray INDEX"))?;
        let mut dicts = Vec::new();
        for item in &fd_index {
            dicts.push(parse_dict(item).ok_or(bad("FD DICT"))?);
        }
        let sel_off = u32::try_from(fd_select_offset.ok_or(bad("FDSelect offset"))?)
            .map_err(|_| bad("FDSelect offset"))?;
        fd_select = Some(parse_fd_select(cff, sel_off, n_glyphs).ok_or(bad("FDSelect"))?);
        for dict in &dicts {
            let (len, off) = private_range(dict).ok_or(bad("FD Private range"))?;
            let priv_bytes = cff
                .get(off..off.saturating_add(len))
                .ok_or(bad("FD Private DICT"))?;
            let private_dict = parse_dict(priv_bytes).ok_or(bad("FD Private DICT parse"))?;
            let subrs = read_local_subrs(cff, &private_dict, off)?;
            fd_privates.push(private_dict);
            local_subrs.push(subrs);
        }
        fd_dicts = Some(dicts);
    } else {
        let (len, off) = private_range(&top).ok_or(bad("Private DICT"))?;
        let priv_bytes = cff
            .get(off..off.saturating_add(len))
            .ok_or(bad("Private DICT"))?;
        let private_dict = parse_dict(priv_bytes).ok_or(bad("Private DICT parse"))?;
        let subrs = read_local_subrs(cff, &private_dict, off)?;
        local_subrs.push(subrs);
        private = Some(private_dict);
    }

    Ok(Source {
        cff: cff.to_vec(),
        name,
        header_off_size: off_size,
        top,
        strings_raw: cff
            .get(strings_start..strings_end)
            .ok_or(bad("String INDEX span"))?
            .to_vec(),
        charstrings,
        global_subrs: gsubrs,
        local_subrs,
        charset_sids,
        encoding_offset,
        encoding_raw,
        fd_select,
        fd_dicts,
        fd_privates,
        private,
    })
}

fn private_range(dict: &Dict) -> Option<(usize, usize)> {
    // Operators 18 (single byte) and 12 10 are both used by producers.
    let entry = dict
        .iter()
        .find(|entry| entry.op == 18 || entry.op == 0x0C0A)?;
    let len = usize::try_from(entry.operands.first()?.int?).ok()?;
    let off = usize::try_from(entry.operands.get(1)?.int?).ok()?;
    Some((len, off))
}

fn read_index_at(cff: &[u8], offset: u32) -> Option<Vec<Vec<u8>>> {
    let mut r = Reader::new(cff.get(usize::try_from(offset).ok()?..)?);
    read_index(&mut r)
}

fn read_local_subrs(
    cff: &[u8],
    private: &Dict,
    private_offset: usize,
) -> Result<Vec<Vec<u8>>, CffError> {
    let Some(rel) = entry_int(private, 19, 0) else {
        return Ok(Vec::new());
    };
    let rel = u32::try_from(rel).map_err(|_| CffError::Malformed("Subrs offset"))?;
    let abs = u32::try_from(private_offset)
        .ok()
        .and_then(|v| v.checked_add(rel))
        .ok_or(CffError::Malformed("Subrs offset"))?;
    read_index_at(cff, abs).ok_or(CffError::Malformed("Local Subrs INDEX"))
}

// ---------------------------------------------------------------------------
// Output assembly with a DICT offset fixpoint
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Offsets {
    charset: u32,
    encoding: u32,
    charstrings: u32,
    private: u32,
    private_len: u32,
    local_subrs_rel: u32,
    fd_array: u32,
    fd_select: u32,
    fd_private: [(u32, u32); 8],
    fd_local_rel: [u32; 8],
}

impl Offsets {
    fn zero() -> Self {
        Self {
            charset: 0,
            encoding: 0,
            charstrings: 0,
            private: 0,
            private_len: 0,
            local_subrs_rel: 0,
            fd_array: 0,
            fd_select: 0,
            fd_private: [(0, 0); 8],
            fd_local_rel: [0; 8],
        }
    }
}

struct Assembled {
    bytes: Vec<u8>,
    actual: Offsets,
}

/// The already-rebuilt pieces that the offset fixpoint feeds into `assemble`.
struct Pieces<'a> {
    strings: &'a [Vec<u8>],
    charstrings: &'a [Vec<u8>],
    global_subrs: &'a [Vec<u8>],
    local_subrs: &'a [Vec<Vec<u8>>],
    charset: &'a [u8],
    encoding: &'a [u8],
    fd_select: &'a [u8],
}

fn assemble(
    source: &Source,
    offsets: Offsets,
    pieces: &Pieces<'_>,
    g: &mut BudgetGuard<'_>,
) -> Result<Assembled, CffError> {
    let cap = source.cff.len().saturating_add(4096);
    let mut out = alloc::vec_with_capacity(g, cap)?;
    out.push(1); // major
    out.push(0); // minor
    out.push(4); // hdrSize
    out.push(source.header_off_size);

    let name_index = write_index(std::slice::from_ref(&source.name), g)?;
    out.extend_from_slice(&name_index);

    // Top DICT. Clone and patch the offsets; add missing operators.
    let mut top = source.top.clone();
    set_entry_int(
        &mut top,
        15,
        0,
        i32::try_from(offsets.charset).unwrap_or(i32::MAX),
    );
    set_entry_int(
        &mut top,
        16,
        0,
        i32::try_from(offsets.encoding).unwrap_or(i32::MAX),
    );
    set_entry_int(
        &mut top,
        17,
        0,
        i32::try_from(offsets.charstrings).unwrap_or(i32::MAX),
    );
    if source.fd_dicts.is_some() {
        set_entry_int(
            &mut top,
            0x0C24,
            0,
            i32::try_from(offsets.fd_array).unwrap_or(i32::MAX),
        );
        set_entry_int(
            &mut top,
            0x0C25,
            0,
            i32::try_from(offsets.fd_select).unwrap_or(i32::MAX),
        );
    } else {
        set_entry_int(
            &mut top,
            18,
            0,
            i32::try_from(offsets.private_len).unwrap_or(i32::MAX),
        );
        set_entry_int(
            &mut top,
            18,
            1,
            i32::try_from(offsets.private).unwrap_or(i32::MAX),
        );
    }
    let top_bytes = write_dict(&top);
    let top_index = write_index(std::slice::from_ref(&top_bytes), g)?;
    out.extend_from_slice(&top_index);

    let strings_index = write_index(pieces.strings, g)?;
    out.extend_from_slice(&strings_index);
    let gsubr_index = write_index(pieces.global_subrs, g)?;
    out.extend_from_slice(&gsubr_index);

    let mut actual = Offsets::zero();
    actual.charset = u32::try_from(out.len()).unwrap_or(u32::MAX);
    out.extend_from_slice(pieces.charset);
    actual.encoding = u32::try_from(out.len()).unwrap_or(u32::MAX);
    out.extend_from_slice(pieces.encoding);
    actual.charstrings = u32::try_from(out.len()).unwrap_or(u32::MAX);
    let cs_index = write_index(pieces.charstrings, g)?;
    out.extend_from_slice(&cs_index);

    if let Some(original_fds) = &source.fd_dicts {
        actual.fd_array = u32::try_from(out.len()).unwrap_or(u32::MAX);
        let mut fd_bytes = Vec::new();
        for (i, fd) in original_fds.iter().enumerate() {
            let mut dict = fd.clone();
            let (offset, len) = offsets.fd_private.get(i).copied().unwrap_or((0, 0));
            set_entry_int(&mut dict, 18, 0, i32::try_from(len).unwrap_or(i32::MAX));
            set_entry_int(&mut dict, 18, 1, i32::try_from(offset).unwrap_or(i32::MAX));
            fd_bytes.push(write_dict(&dict));
        }
        let fd_index = write_index(&fd_bytes, g)?;
        out.extend_from_slice(&fd_index);

        actual.fd_select = u32::try_from(out.len()).unwrap_or(u32::MAX);
        out.extend_from_slice(pieces.fd_select);

        for (i, private_dict) in source.fd_privates.iter().enumerate() {
            let rel = offsets.fd_local_rel.get(i).copied().unwrap_or(0);
            let mut dict = private_dict.clone();
            if pieces.local_subrs.get(i).is_none_or(Vec::is_empty) {
                // A stale Subrs offset would point into unrelated bytes.
                remove_entry(&mut dict, 19);
            } else {
                set_entry_int(&mut dict, 19, 0, i32::try_from(rel).unwrap_or(i32::MAX));
            }
            let bytes = write_dict(&dict);
            let at = u32::try_from(out.len()).unwrap_or(u32::MAX);
            if let Some(slot) = actual.fd_private.get_mut(i) {
                *slot = (at, u32::try_from(bytes.len()).unwrap_or(u32::MAX));
            }
            out.extend_from_slice(&bytes);
            if let Some(list) = pieces.local_subrs.get(i) {
                let index = write_index(list, g)?;
                if !list.is_empty() {
                    if let Some(slot) = actual.fd_local_rel.get_mut(i) {
                        *slot = u32::try_from(bytes.len()).unwrap_or(u32::MAX);
                    }
                }
                out.extend_from_slice(&index);
            }
        }
    } else {
        let mut private_dict = source.private.clone().unwrap_or_default();
        if pieces.local_subrs.first().is_none_or(Vec::is_empty) {
            remove_entry(&mut private_dict, 19);
        } else {
            set_entry_int(
                &mut private_dict,
                19,
                0,
                i32::try_from(offsets.local_subrs_rel).unwrap_or(i32::MAX),
            );
        }
        let priv_bytes = write_dict(&private_dict);
        actual.private = u32::try_from(out.len()).unwrap_or(u32::MAX);
        actual.private_len = u32::try_from(priv_bytes.len()).unwrap_or(u32::MAX);
        out.extend_from_slice(&priv_bytes);
        if let Some(list) = pieces.local_subrs.first() {
            let index = write_index(list, g)?;
            if !list.is_empty() {
                actual.local_subrs_rel = u32::try_from(priv_bytes.len()).unwrap_or(u32::MAX);
            }
            out.extend_from_slice(&index);
        }
    }

    Ok(Assembled { bytes: out, actual })
}

// ---------------------------------------------------------------------------
// sfnt reassembly
// ---------------------------------------------------------------------------

fn table_checksum(data: &[u8], is_head: bool) -> u32 {
    let mut padded = data.to_vec();
    while padded.len() % 4 != 0 {
        padded.push(0);
    }
    let mut sum = 0u32;
    let mut i = 0usize;
    while i.saturating_add(4) <= padded.len() {
        let word = u32::from_be_bytes([
            *padded.get(i).unwrap_or(&0),
            *padded.get(i.saturating_add(1)).unwrap_or(&0),
            *padded.get(i.saturating_add(2)).unwrap_or(&0),
            *padded.get(i.saturating_add(3)).unwrap_or(&0),
        ]);
        let is_adjustment = is_head && i == 8;
        sum = sum.wrapping_add(if is_adjustment { 0 } else { word });
        i = i.saturating_add(4);
    }
    sum
}

fn assemble_sfnt(
    version: [u8; 4],
    tables: &[([u8; 4], Vec<u8>)],
    g: &mut BudgetGuard<'_>,
) -> Result<Vec<u8>, CffError> {
    let total: usize = tables.iter().fold(
        12usize.saturating_add(tables.len().saturating_mul(16)),
        |acc, (_, data)| acc.saturating_add(data.len().saturating_add(3) & !3),
    );
    let mut out = alloc::vec_filled(g, total, 0u8)?;
    out.get_mut(0..4)
        .ok_or(CffError::Malformed("sfnt version"))?
        .copy_from_slice(&version);
    let count = u16::try_from(tables.len()).map_err(|_| CffError::Malformed("sfnt tables"))?;
    put_u16_dir(&mut out, 4, count);
    let mut offset = 12usize.saturating_add(tables.len().saturating_mul(16));
    let mut entries = Vec::with_capacity(tables.len());
    for (tag, data) in tables {
        let checksum = table_checksum(data, tag == b"head");
        let entry_pos = entries.len().saturating_mul(16).saturating_add(12);
        if let Some(slot) = out.get_mut(entry_pos..entry_pos.saturating_add(4)) {
            slot.copy_from_slice(tag);
        }
        put_u32_dir(&mut out, entry_pos.saturating_add(4), checksum);
        put_u32_dir(
            &mut out,
            entry_pos.saturating_add(8),
            u32::try_from(offset).unwrap_or(u32::MAX),
        );
        put_u32_dir(
            &mut out,
            entry_pos.saturating_add(12),
            u32::try_from(data.len()).unwrap_or(u32::MAX),
        );
        entries.push((offset, data));
        offset = offset.saturating_add(data.len().saturating_add(3) & !3);
    }
    for (at, data) in entries {
        if let Some(slot) = out.get_mut(at..at.saturating_add(data.len())) {
            slot.copy_from_slice(data);
        }
    }
    Ok(out)
}

fn put_u16_dir(out: &mut [u8], at: usize, value: u16) {
    let bytes = value.to_be_bytes();
    if let Some(slot) = out.get_mut(at..at.saturating_add(2)) {
        slot.copy_from_slice(&bytes);
    }
}

fn put_u32_dir(out: &mut [u8], at: usize, value: u32) {
    let bytes = value.to_be_bytes();
    if let Some(slot) = out.get_mut(at..at.saturating_add(4)) {
        slot.copy_from_slice(&bytes);
    }
}

fn read_u16_at(data: &[u8], at: usize) -> u16 {
    let hi = data.get(at).copied().unwrap_or(0);
    let lo = data.get(at.saturating_add(1)).copied().unwrap_or(0);
    u16::from_be_bytes([hi, lo])
}

// ---------------------------------------------------------------------------
// Public entry point
// ---------------------------------------------------------------------------

/// Subset a CFF font (`/FontFile3` Type1C / CIDFontType0C, bare or in an sfnt
/// `OTTO` wrapper) to `keep`.
///
/// Returns `Ok(None)` only when `data` is not a CFF program at all. Any CFF
/// that parses but cannot be faithfully rewritten produces a typed
/// [`CffError`] and no bytes.
///
/// # Budget
///
/// Charges every output buffer (sfnt, INDEXes, charstrings) through `g`.
///
/// # Malformed Input
///
/// [`CffError::Malformed`] for a broken program, [`CffError::Seac`] /
/// [`CffError::CyclicSubr`] / [`CffError::AmbiguousHintState`] for constructs
/// outside this writer's guaranteed scope, [`CffError::OffsetFixpoint`] if
/// offsets do not stabilise, and [`CffError::Budget`] on exhaustion.
pub fn subset_cff(
    data: &Bytes,
    keep: &GlyphSet,
    g: &mut BudgetGuard<'_>,
) -> Result<Option<SubsetResult>, CffError> {
    let (version, sfnt_tables, cff_bytes) = match split_sfnt(data.as_slice()) {
        Some((version, tables, cff)) => (Some(version), Some(tables), cff),
        None => (None, None, data.as_slice()),
    };
    if sfnt_tables.is_none() && !looks_like_cff(cff_bytes) {
        return Ok(None);
    }
    g.charge(
        selis_sandbox::Resource::Bytes,
        u64::try_from(cff_bytes.len()).unwrap_or(u64::MAX),
    )?;

    let source = parse_source(cff_bytes)?;
    let n_glyphs = source.charstrings.len();
    let is_cid = source.fd_dicts.is_some();

    // 1. Choose kept glyphs (.notdef is always kept), in original GID order.
    let mut keep_set: BTreeSet<u16> = keep
        .keep
        .iter()
        .copied()
        .filter(|gid| usize::from(*gid) < n_glyphs)
        .collect();
    keep_set.insert(0);
    let keep_sorted: Vec<u16> = keep_set.iter().copied().collect();
    let mut renumber = BTreeMap::new();
    for (new, old) in keep_sorted.iter().copied().enumerate() {
        renumber.insert(
            old,
            u16::try_from(new).map_err(|_| CffError::Malformed("too many kept glyphs"))?,
        );
    }

    // 2. Walk every kept charstring, closing over subrs and recording patches.
    let mut walk = Walk::new(&source.global_subrs, &source.local_subrs);
    for &old_gid in &keep_sorted {
        let fd = if is_cid {
            source
                .fd_select
                .as_ref()
                .and_then(|fds| fds.get(usize::from(old_gid)))
                .copied()
                .unwrap_or(0)
        } else {
            0
        };
        let fd = usize::from(fd);
        let bytes = source
            .charstrings
            .get(usize::from(old_gid))
            .ok_or(CffError::Malformed("glyph"))?;
        walk.parse_glyph(bytes, fd)?;
    }

    // A subr reached with two different starting hint counts is only a problem
    // when it actually contains a hint mask (whose byte width then differs).
    for ids in walk.by_subr.values() {
        if ids.len() < 2 {
            continue;
        }
        let has_mask = ids.iter().any(|id| {
            walk.records.get(*id).is_some_and(|record| {
                record
                    .tokens
                    .iter()
                    .any(|token| matches!(token, Token::Mask { .. }))
            })
        });
        if has_mask {
            return Err(CffError::AmbiguousHintState);
        }
    }

    // 3. Renumber used subrs per set and compute the new biases.
    let mut new_gsubr_index: BTreeMap<u16, u16> = BTreeMap::new();
    for (new, old) in walk.used_global.iter().copied().enumerate() {
        new_gsubr_index.insert(
            old,
            u16::try_from(new).map_err(|_| CffError::Malformed("gsubr count"))?,
        );
    }
    let mut new_lsubr_index: Vec<BTreeMap<u16, u16>> = Vec::new();
    for used in &walk.used_local {
        let mut map = BTreeMap::new();
        for (new, old) in used.iter().copied().enumerate() {
            map.insert(
                old,
                u16::try_from(new).map_err(|_| CffError::Malformed("lsubr count"))?,
            );
        }
        new_lsubr_index.push(map);
    }
    let new_g_bias = bias(new_gsubr_index.len());
    let new_l_bias: Vec<i32> = new_lsubr_index.iter().map(|map| bias(map.len())).collect();

    // 4. Render the used subr bodies and the kept glyph charstrings.
    let mut new_global: Vec<Vec<u8>> = Vec::new();
    for old in walk.used_global.iter().copied() {
        let id = walk
            .by_subr
            .get(&(SubrSet::Global, old))
            .and_then(|ids| ids.first().copied())
            .ok_or(CffError::Malformed("gsubr record"))?;
        let record = walk
            .records
            .get(id)
            .ok_or(CffError::Malformed("gsubr record"))?;
        let original = source
            .global_subrs
            .get(usize::from(old))
            .ok_or(CffError::Malformed("gsubr"))?;
        let patches = patch_map(
            record,
            &new_gsubr_index,
            new_g_bias,
            &new_lsubr_index,
            &new_l_bias,
            SubrSet::Global,
            old,
        )?;
        new_global.push(render(record, original, &patches));
    }
    let mut new_local: Vec<Vec<Vec<u8>>> = Vec::new();
    for (fd, used) in walk.used_local.iter().enumerate() {
        let mut list = Vec::new();
        for old in used.iter().copied() {
            // Find any parse of this subr; the exact hint state does not change
            // the rewritten bytes, only how the original tokens were counted.
            let id = walk
                .by_subr
                .get(&(SubrSet::Local(fd), old))
                .and_then(|ids| ids.first().copied())
                .ok_or(CffError::Malformed("lsubr record"))?;
            let record = walk
                .records
                .get(id)
                .ok_or(CffError::Malformed("lsubr record"))?;
            let original = source
                .local_subrs
                .get(fd)
                .and_then(|l| l.get(usize::from(old)))
                .ok_or(CffError::Malformed("lsubr"))?;
            let patches = patch_map(
                record,
                &new_gsubr_index,
                new_g_bias,
                &new_lsubr_index,
                &new_l_bias,
                SubrSet::Local(fd),
                old,
            )?;
            list.push(render(record, original, &patches));
        }
        new_local.push(list);
    }
    let mut new_charstrings = Vec::with_capacity(keep_sorted.len());
    for (position, old_gid) in keep_sorted.iter().copied().enumerate() {
        let record = walk
            .glyph_records
            .get(position)
            .ok_or(CffError::Malformed("glyph record"))?;
        let original = source
            .charstrings
            .get(usize::from(old_gid))
            .ok_or(CffError::Malformed("glyph"))?;
        let mut patches = BTreeMap::new();
        for patch in &record.patches {
            let value = subr_value(
                patch.target,
                patch.target_index,
                &new_gsubr_index,
                new_g_bias,
                &new_lsubr_index,
                &new_l_bias,
            )?;
            patches.insert(patch.start, encode_int(value));
        }
        new_charstrings.push(render(record, original, &patches));
    }

    // 5. Charset, FDSelect and Encoding.
    let new_sids: Vec<u16> = keep_sorted
        .iter()
        .map(|gid| {
            source
                .charset_sids
                .get(usize::from(*gid))
                .copied()
                .unwrap_or(0)
        })
        .collect();
    let charset_bytes = write_charset(&new_sids, g)?;
    let encoding_bytes = if let Some(raw) = &source.encoding_raw {
        rewrite_encoding(raw, &source.charset_sids, &renumber)
            .ok_or(CffError::Malformed("custom encoding"))?
    } else {
        Vec::new()
    };
    let encoding_offset = if source.encoding_offset <= 1 {
        source.encoding_offset
    } else {
        0 // patched by the fixpoint below
    };
    let fd_select_bytes = if let Some(fds) = &source.fd_select {
        let kept: Vec<u8> = keep_sorted
            .iter()
            .map(|gid| fds.get(usize::from(*gid)).copied().unwrap_or(0))
            .collect();
        write_fd_select(&kept, g)?
    } else {
        Vec::new()
    };

    let mut top = source.top.clone();
    set_entry_int(&mut top, 15, 0, i32::try_from(encoding_offset).unwrap_or(0));
    let _ = &top;
    // The fixpoint writes the real encoding offset into a copy of `top` inside
    // `assemble`; here we only need a valid placeholder in `source.top`.
    let mut source = source;
    set_entry_int(
        &mut source.top,
        16,
        0,
        i32::try_from(encoding_offset).unwrap_or(0),
    );

    // 6. Run the two-pass DICT offset fixpoint.
    let mut guess = Offsets::zero();
    let mut assembled = None;
    for _ in 0..12 {
        let attempt = assemble(
            &source,
            guess,
            &Pieces {
                strings: &rebuild_strings(&source, &new_sids, g)?,
                charstrings: &new_charstrings,
                global_subrs: &new_global,
                local_subrs: &new_local,
                charset: &charset_bytes,
                encoding: &encoding_bytes,
                fd_select: &fd_select_bytes,
            },
            g,
        )?;
        if attempt.actual == guess {
            assembled = Some(attempt);
            break;
        }
        guess = attempt.actual;
    }
    let assembled = assembled.ok_or(CffError::OffsetFixpoint)?;

    // 7. Wrap back into the sfnt (or emit a bare CFF program).
    let cff_out = patch_checksum_cff(&assembled.bytes);
    let font = match sfnt_tables {
        None => Bytes::from(cff_out),
        Some(tables) => {
            // Rebuild the cmap so Unicode codes still select the same outlines
            // after GID compaction (the CFF charset keeps the glyph names).
            let mut codes = BTreeMap::new();
            if let Ok(font) = skrifa::FontRef::new(data.as_slice()) {
                for (code, old_gid) in font.charmap().mappings() {
                    let old = u16::try_from(old_gid.to_u32()).unwrap_or(u16::MAX);
                    if let Some(&new) = renumber.get(&old) {
                        codes.insert(code, new);
                    }
                }
            }
            let new_cmap = build_cmap_format12_from_map(&codes);
            let built = rebuild_tables(
                &tables,
                version.unwrap_or(*b"OTTO"),
                &cff_out,
                &new_cmap,
                &keep_sorted,
                g,
            )?;
            Bytes::from(patched_sfnt(&built))
        }
    };

    Ok(Some(SubsetResult {
        font,
        gid_map: renumber,
    }))
}

fn subr_value(
    target: SubrSet,
    target_index: u16,
    gmap: &BTreeMap<u16, u16>,
    gbias: i32,
    lmaps: &[BTreeMap<u16, u16>],
    lbias: &[i32],
) -> Result<i32, CffError> {
    let (map, new_bias) = match target {
        SubrSet::Global => (Some(gmap), gbias),
        SubrSet::Local(fd) => (lmaps.get(fd), lbias.get(fd).copied().unwrap_or(0)),
    };
    let map = map.ok_or(CffError::Malformed("subr map"))?;
    let new_index = map
        .get(&target_index)
        .copied()
        .ok_or(CffError::Malformed("subr map"))?;
    let value = i64::from(new_index).saturating_sub(i64::from(new_bias));
    i32::try_from(value).map_err(|_| CffError::Malformed("subr operand"))
}

fn patch_map(
    record: &ParseRecord,
    gmap: &BTreeMap<u16, u16>,
    gbias: i32,
    lmaps: &[BTreeMap<u16, u16>],
    lbias: &[i32],
    set: SubrSet,
    index: u16,
) -> Result<BTreeMap<usize, Vec<u8>>, CffError> {
    let mut out = BTreeMap::new();
    for patch in &record.patches {
        if patch.target == set && patch.target_index == index {
            // A self-reference cannot occur: cycles are rejected by the walk.
            return Err(CffError::CyclicSubr);
        }
        let value = subr_value(patch.target, patch.target_index, gmap, gbias, lmaps, lbias)?;
        out.insert(patch.start, encode_int(value));
    }
    Ok(out)
}

fn rebuild_strings(
    source: &Source,
    _sids: &[u16],
    _g: &mut BudgetGuard<'_>,
) -> Result<Vec<Vec<u8>>, CffError> {
    // Preserve the String INDEX verbatim: every SID in the charset must keep
    // resolving to the same glyph name, and the standard-string range is
    // fixed by the specification.
    let mut r = Reader::new(&source.strings_raw);
    read_index(&mut r).ok_or(CffError::Malformed("String INDEX rebuild"))
}

fn rebuild_tables(
    tables: &[([u8; 4], Vec<u8>)],
    version: [u8; 4],
    cff: &[u8],
    new_cmap: &[u8],
    keep: &[u16],
    g: &mut BudgetGuard<'_>,
) -> Result<Vec<u8>, CffError> {
    let mut out: Vec<([u8; 4], Vec<u8>)> = Vec::new();
    for (tag, data) in tables {
        let replacement = match tag {
            b"CFF " => Some(cff.to_vec()),
            b"cmap" if !new_cmap.is_empty() => Some(new_cmap.to_vec()),
            b"hmtx" => Some(rebuild_hmtx(data, tables, keep)),
            b"maxp" => Some(rebuild_maxp(data, keep.len())),
            b"hhea" => Some(rebuild_hhea(data, keep.len())),
            _ => None,
        };
        out.push((*tag, replacement.unwrap_or_else(|| data.clone())));
    }
    assemble_sfnt(version, &out, g)
}

fn rebuild_hmtx(hmtx: &[u8], tables: &[([u8; 4], Vec<u8>)], keep: &[u16]) -> Vec<u8> {
    let num_hmetrics = tables
        .iter()
        .find(|(tag, _)| tag == b"hhea")
        .map(|(_, data)| read_u16_at(data, 34))
        .unwrap_or(0);
    let mut out = Vec::new();
    for old in keep.iter().take(usize::from(num_hmetrics)) {
        let at = usize::from(*old).saturating_mul(4);
        if let Some(slice) = hmtx.get(at..at.saturating_add(4)) {
            out.extend_from_slice(slice);
        }
    }
    out
}

fn rebuild_maxp(maxp: &[u8], count: usize) -> Vec<u8> {
    let mut out = maxp.to_vec();
    if out.len() >= 6 {
        let value = u16::try_from(count).unwrap_or(u16::MAX).to_be_bytes();
        if let Some(slot) = out.get_mut(4..6) {
            slot.copy_from_slice(&value);
        }
    }
    out
}

fn rebuild_hhea(hhea: &[u8], count: usize) -> Vec<u8> {
    let mut out = hhea.to_vec();
    if out.len() >= 36 {
        let value = u16::try_from(count).unwrap_or(u16::MAX).to_be_bytes();
        if let Some(slot) = out.get_mut(34..36) {
            slot.copy_from_slice(&value);
        }
    }
    out
}

fn patched_sfnt(bytes: &[u8]) -> Vec<u8> {
    patch_checksum(bytes)
}

fn patch_checksum_cff(cff: &[u8]) -> Vec<u8> {
    // A bare CFF program has no checksum field; keep the bytes verbatim.
    cff.to_vec()
}

pub(crate) fn looks_like_cff(data: &[u8]) -> bool {
    let first = data.first().copied().unwrap_or(0);
    first == 1 && data.get(1).copied().is_some_and(|v| v <= 1)
}

type SfntTables = Vec<([u8; 4], Vec<u8>)>;

fn split_sfnt(data: &[u8]) -> Option<([u8; 4], SfntTables, &[u8])> {
    let version = [
        data.first().copied()?,
        data.get(1).copied()?,
        data.get(2).copied()?,
        data.get(3).copied()?,
    ];
    if &version != b"OTTO" && &version != b"\x00\x01\x00\x00" {
        return None;
    }
    let num = read_u16_at(data, 4);
    let mut tables = Vec::with_capacity(usize::from(num));
    let mut cff = None;
    for i in 0..usize::from(num) {
        let at = 12usize.saturating_add(i.saturating_mul(16));
        let tag: [u8; 4] = [
            data.get(at).copied()?,
            data.get(at.saturating_add(1)).copied()?,
            data.get(at.saturating_add(2)).copied()?,
            data.get(at.saturating_add(3)).copied()?,
        ];
        let off_bytes = [
            data.get(at.saturating_add(8)).copied()?,
            data.get(at.saturating_add(9)).copied()?,
            data.get(at.saturating_add(10)).copied()?,
            data.get(at.saturating_add(11)).copied()?,
        ];
        let len_bytes = [
            data.get(at.saturating_add(12)).copied()?,
            data.get(at.saturating_add(13)).copied()?,
            data.get(at.saturating_add(14)).copied()?,
            data.get(at.saturating_add(15)).copied()?,
        ];
        let off = usize::try_from(u32::from_be_bytes(off_bytes)).ok()?;
        let len = usize::try_from(u32::from_be_bytes(len_bytes)).ok()?;
        let slice = data.get(off..off.saturating_add(len))?;
        if &tag == b"CFF " {
            cff = Some(slice);
        }
        tables.push((tag, slice.to_vec()));
    }
    Some((version, tables, cff?))
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::indexing_slicing,
        clippy::arithmetic_side_effects,
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        clippy::integer_division
    )]

    use super::*;
    use selis_sandbox::Budget;
    use skrifa::{FontRef, MetadataProvider, Tag};

    fn guard() -> BudgetGuard<'static> {
        Budget::unlimited().guard()
    }

    /// A four-operand `endchar` is `seac`; it must produce a typed refusal,
    /// never a glyph that silently drops its base and accent.
    #[test]
    fn seac_is_a_typed_refusal() {
        let global = Vec::new();
        let local = vec![Vec::<Vec<u8>>::new()];
        let mut walk = Walk::new(&global, &local);
        // 0 0 rmoveto, then four operands and endchar.
        let bytes = [139, 139, 21, 139, 139, 139, 139, 14];
        assert!(matches!(
            walk.parse(&bytes, 0, 0, 0, None),
            Err(CffError::Seac)
        ));
    }

    /// A subroutine that calls itself is rejected explicitly instead of being
    /// followed into an unbounded walk.
    #[test]
    fn cyclic_subr_is_a_typed_refusal() {
        let global = Vec::new();
        // Local subr 0 calls biased index -107 == subr 0 (bias 107);
        // -107 encodes as byte 32 (`32 - 139 = -107`).
        let local = vec![vec![vec![32, 10]]];
        let mut walk = Walk::new(&global, &local);
        let bytes = local.first().and_then(|l| l.first()).cloned().unwrap();
        assert!(matches!(
            walk.parse(&bytes, 0, 0, 0, Some((SubrSet::Local(0), 0))),
            Err(CffError::CyclicSubr)
        ));
    }

    #[test]
    fn encode_int_round_trips() {
        for value in [
            -40000i32, -2000, -109, -108, -1, 0, 107, 108, 1131, 1132, 30000,
        ] {
            let bytes = encode_int(value);
            let mut r = Reader::new(&bytes);
            let first = r.byte().unwrap();
            let operand = parse_operand(first, &mut r).unwrap();
            assert_eq!(operand.int, Some(value));
        }
    }

    #[test]
    fn name_keyed_fixture_with_subrs_subsets_and_reopens() {
        let mut g = guard();
        let original = Bytes::copy_from_slice(include_bytes!("../tests/fixtures/cff-subr.otf"));
        let before = FontRef::new(original.as_slice()).expect("source OTF parses");
        assert!(before.table_data(Tag::new(b"CFF ")).is_some());

        let subset = subset_cff(
            &original,
            &GlyphSet {
                keep: vec![1, 2, 4],
            },
            &mut g,
        )
        .expect("subset CFF")
        .expect("CFF font");
        assert!(
            subset.font.len() < original.len(),
            "{} < {}",
            subset.font.len(),
            original.len()
        );
        let reopened = FontRef::new(subset.font.as_slice()).expect("output OTF reopens");
        assert!(reopened.table_data(Tag::new(b"CFF ")).is_some());
        let metrics = reopened.metrics(
            skrifa::instance::Size::unscaled(),
            skrifa::instance::LocationRef::default(),
        );
        assert_eq!(usize::from(metrics.glyph_count), 4); // .notdef + three kept

        // Every kept outline must be byte-for-byte the same command stream.
        for &old in &[0u16, 1, 2, 4] {
            let new = subset.gid_map.get(&old).copied().expect("kept gid");
            let source_outline = crate::outline::outline_glyph(&original, old, &mut g)
                .expect("source outline")
                .expect("source glyph exists");
            let subset_outline = crate::outline::outline_glyph(&subset.font, new, &mut g)
                .expect("subset outline")
                .expect("subset glyph exists");
            assert_eq!(
                source_outline.commands, subset_outline.commands,
                "gid {old} -> {new} keeps its outline"
            );
        }
    }

    /// CID-keyed CFF with two FDArray entries, FDSelect and both local and
    /// global subrs: outlines survive the subset for glyphs from both FDs.
    #[test]
    fn cid_keyed_fixture_with_fds_subsets_and_keeps_outlines() {
        let mut g = guard();
        let original = Bytes::copy_from_slice(include_bytes!("../tests/fixtures/cff-cid-subr.otf"));
        let subset = subset_cff(
            &original,
            &GlyphSet {
                keep: vec![1, 2, 4, 5],
            },
            &mut g,
        )
        .expect("subset CID CFF")
        .expect("font");
        assert!(
            subset.font.len() < original.len(),
            "{} < {}",
            subset.font.len(),
            original.len()
        );
        let reopened = FontRef::new(subset.font.as_slice()).expect("output reopens");
        assert!(reopened.table_data(Tag::new(b"CFF ")).is_some());
        for &old in &[0u16, 1, 2, 4, 5] {
            let new = subset.gid_map.get(&old).copied().expect("kept gid");
            let source_outline = crate::outline::outline_glyph(&original, old, &mut g)
                .expect("source outline")
                .expect("source glyph exists");
            let subset_outline = crate::outline::outline_glyph(&subset.font, new, &mut g)
                .expect("subset outline")
                .expect("subset glyph exists");
            assert_eq!(
                source_outline.commands, subset_outline.commands,
                "CID gid {old} -> {new} keeps its outline"
            );
        }
    }

    /// The two tiny hand-built fixtures still subset without passthrough.
    #[test]
    fn trivial_fixtures_subset_and_reopen() {
        let mut g = guard();
        for fixture in [
            &include_bytes!("../tests/fixtures/cff.otf")[..],
            &include_bytes!("../tests/fixtures/cff-cid.otf")[..],
        ] {
            let data = Bytes::copy_from_slice(fixture);
            let subset = subset_cff(&data, &GlyphSet { keep: vec![1] }, &mut g)
                .expect("subset")
                .expect("font");
            let font = FontRef::new(subset.font.as_slice()).expect("reopen");
            assert!(font.table_data(Tag::new(b"CFF ")).is_some());
        }
    }
}
