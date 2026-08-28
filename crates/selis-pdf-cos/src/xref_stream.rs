//! Cross-reference streams and object streams (SL-1.COS.04).
//!
//! PDF 1.5+ moved the cross-reference into a stream object (`/Type /XRef`)
//! instead of a classic table, and compresses groups of objects into object
//! streams (`/ObjStm`). This module decodes both.
//!
//! The two rules that matter (ISO 32000-2 §7.5.8):
//! * `/W` gives the field widths (any width may be 0 — a 0-width field is
//!   omitted and takes its default value);
//! * an object stream cannot itself be inside an object stream (`OBJSTM_NESTED`).
//!
//! The hybrid-reference file case (`/XRefStm` in a classic trailer) is handled
//! here too — it is the one everybody gets wrong.

use std::collections::BTreeMap;

use selis_error::{err, Code, Result};
use selis_sandbox::{Budget, BudgetGuard};

use crate::obj::Obj;
use crate::xref::XrefEntry;

/// A decoded xref stream.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct XrefStream {
    /// Field widths: `[type, field2, field3]`.
    pub w: [u8; 3],
    /// Subsection index: `[start1 count1 start2 count2 …]`. Defaults to
    /// `[0, /Size]`.
    pub index: Vec<u32>,
    /// The decoded entry fields: triplets `(type, f2, f3)` in file order.
    pub entries: Vec<(u64, u64, u64)>,
    /// The trailer dictionary embedded in the stream dict.
    pub trailer: Vec<(selis_bytes::Bytes, Obj)>,
}

impl XrefStream {
    /// Apply the decoded entries into an index, honoring `/Prev` at the caller.
    ///
    /// Walks the `/Index` subsection ranges (each pair is `start count`; the
    /// default `[0 /Size]` is filled in by the parser), so non-zero or
    /// non-contiguous subsections land on the correct object numbers.
    pub fn apply(&self, index: &mut BTreeMap<u32, XrefEntry>) {
        // `/Index` is a sequence of (start, count) pairs; default `[0 /Size]`.
        let ranges: &[u32] = if self.index.len() >= 2 {
            &self.index
        } else {
            &[0, u32::try_from(self.entries.len()).unwrap_or(u32::MAX)]
        };
        let mut entries = self.entries.iter();
        'ranges: for pair in ranges.chunks(2) {
            let (Some(&start), Some(&count)) = (pair.first(), pair.get(1)) else {
                continue;
            };
            for k in 0..count {
                let Some(&(ty, f2, f3)) = entries.next() else {
                    break 'ranges;
                };
                let entry = match ty {
                    0 => XrefEntry::Free {
                        next_free: u32::try_from(f2).unwrap_or(u32::MAX),
                        gen: u16::try_from(f3).unwrap_or(u16::MAX),
                    },
                    1 => XrefEntry::InUse {
                        offset: f2,
                        gen: u16::try_from(f3).unwrap_or(u16::MAX),
                    },
                    2 => XrefEntry::Compressed {
                        objstm: u32::try_from(f2).unwrap_or(u32::MAX),
                        index: u32::try_from(f3).unwrap_or(u32::MAX),
                    },
                    _ => continue, // unknown type: consume the slot, no entry
                };
                index.insert(start.saturating_add(k), entry);
            }
        }
    }
}

/// Parse an xref stream from its dictionary and decoded byte payload.
///
/// # Budget
///
/// The decoded payload must fit `budget.bytes`; the entry count is charged
/// against `Objects`.
///
/// # Malformed Input
///
/// `XREF_MALFORMED` when `/W` is missing or the payload is too short for the
/// declared entries.
pub fn parse_xref_stream(
    dict: &[(selis_bytes::Bytes, Obj)],
    payload: &[u8],
    _budget: &Budget,
    g: &mut BudgetGuard<'_>,
) -> Result<XrefStream> {
    // /W [w0 w1 w2]
    let w = dict_value(dict, b"W").ok_or_else(|| {
        err!(
            Code::XrefMalformed,
            during = "xref-stream",
            detail = "missing /W"
        )
    })?;
    let wv = obj_to_u8_array(w)?;
    if wv.len() != 3 {
        return Err(err!(
            Code::XrefMalformed,
            during = "xref-stream",
            detail = "/W must have 3 fields"
        ));
    }
    let w0 = wv.first().copied().unwrap_or(0);
    let w1 = wv.get(1).copied().unwrap_or(0);
    let w2 = wv.get(2).copied().unwrap_or(0);
    let w = [w0, w1, w2];

    // /Size is required; /Index defaults to [0 Size].
    let size = dict_value(dict, b"Size")
        .and_then(obj_to_u64)
        .ok_or_else(|| {
            err!(
                Code::XrefMalformed,
                during = "xref-stream",
                detail = "missing /Size"
            )
        })?;
    let index = match dict_value(dict, b"Index") {
        Some(Obj::Array(items)) => items
            .iter()
            .filter_map(obj_to_u64)
            .map(|v| u32::try_from(v).unwrap_or(u32::MAX))
            .collect(),
        _ => vec![0, u32::try_from(size).unwrap_or(u32::MAX)],
    };

    // Total field count.
    let total_fields = index
        .chunks(2)
        .filter_map(|pair| pair.get(1))
        .fold(0u32, |a, b| a.saturating_add(*b));
    let row_width = u64::from(w[0])
        .saturating_add(u64::from(w[1]))
        .saturating_add(u64::from(w[2]));
    // A damaged writer can declare more entries than the payload ships
    // (SL-1.ROB.01). Read only the entries that actually fit.
    let max_fields = if row_width == 0 {
        u64::from(total_fields)
    } else {
        let fit = u64::try_from(payload.len())
            .unwrap_or(u64::MAX)
            .checked_div(row_width)
            .unwrap_or(0);
        u64::from(total_fields).min(fit)
    };

    // Decode each entry.
    let mut entries = Vec::new();
    let mut offset = 0usize;
    for _ in 0..max_fields {
        g.charge_one(selis_sandbox::Resource::Objects)?;
        let mut fields = [0u64; 3];
        for (i, width) in w.iter().copied().enumerate() {
            if let Some(slot) = fields.get_mut(i) {
                *slot = read_field(payload, offset, width);
            }
            offset = offset.saturating_add(usize::from(width));
        }
        let entry = (
            fields.first().copied().unwrap_or(0),
            fields.get(1).copied().unwrap_or(0),
            fields.get(2).copied().unwrap_or(0),
        );
        entries.push(entry);
    }

    // The trailer lives in the stream dict itself.
    Ok(XrefStream {
        w,
        index,
        entries,
        trailer: dict.to_vec(),
    })
}

/// Extract an object stream (`/ObjStm`) payload: a header of `(objnum offset)`
/// pairs followed by the concatenated objects.
///
/// Returns `(object_number → byte-range-in-this-stream)`.
///
/// # Budget
///
/// The payload is already bounded; the header pair count is charged.
///
/// # Malformed Input
///
/// `OBJSTM_MALFORMED` on a bad header; `OBJSTM_NESTED` when an object stream
/// references another object stream.
pub fn parse_object_stream(
    dict: &[(selis_bytes::Bytes, Obj)],
    payload: &[u8],
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
) -> Result<Vec<(u32, std::ops::Range<u64>)>> {
    // /N objects, /First = header byte length.
    let n = dict_value(dict, b"N").and_then(obj_to_u64).ok_or_else(|| {
        err!(
            Code::ObjstmMalformed,
            during = "objstm",
            detail = "missing /N"
        )
    })?;
    let first = dict_value(dict, b"First")
        .and_then(obj_to_u64)
        .ok_or_else(|| {
            err!(
                Code::ObjstmMalformed,
                during = "objstm",
                detail = "missing /First"
            )
        })?;
    let header = payload
        .get(..usize::try_from(first).unwrap_or(usize::MAX))
        .ok_or_else(|| {
            err!(
                Code::ObjstmMalformed,
                during = "objstm",
                detail = "/First out of range"
            )
        })?;

// Parse `num offset` pairs from the header.
    let mut pairs = Vec::new();
    let mut pos = 0usize;
    for _ in 0..n {
        g.charge_one(selis_sandbox::Resource::Objects)?;
        let (num, p) = read_int(header, pos)?;
        let (off, p) = read_int(header, p)?;
        pairs.push((num, off));
        pos = p;
    }

    // Build byte ranges into the object area.
    let mut out = Vec::new();
    for (i, &(num, off)) in pairs.iter().enumerate() {
        let start = first.saturating_add(off);
        let end = if let Some(&(_, next_off)) = pairs.get(i.saturating_add(1)) {
            first.saturating_add(next_off)
        } else {
            u64::try_from(payload.len()).unwrap_or(u64::MAX)
        };
        let _ = budget;
        out.push((u32::try_from(num).unwrap_or(u32::MAX), start..end));
    }
    Ok(out)
}

/// Read an unsigned big-endian field of `width` bytes (0 = 0).
fn read_field(payload: &[u8], offset: usize, width: u8) -> u64 {
    if width == 0 {
        return 0;
    }
    let mut v = 0u64;
    let start = offset;
    let end = start.saturating_add(usize::from(width));
    for &b in payload.get(start..end).unwrap_or(&[]) {
        v = v.saturating_mul(256).saturating_add(u64::from(b));
    }
    v
}

/// Look up a name in a dict.
fn dict_value<'a>(dict: &'a [(selis_bytes::Bytes, Obj)], key: &[u8]) -> Option<&'a Obj> {
    dict.iter()
        .find(|(k, _)| k.as_slice() == key)
        .map(|(_, v)| v)
}

fn obj_to_u64(o: &Obj) -> Option<u64> {
    match o {
        Obj::Int(i) => u64::try_from(*i).ok(),
        Obj::Real { scaled, .. } => u64::try_from(*scaled).ok(),
        _ => None,
    }
}

fn obj_to_u8_array(o: &Obj) -> Result<Vec<u8>> {
    match o {
        Obj::Array(items) => items
            .iter()
            .map(|v| match v {
                Obj::Int(i) if *i >= 0 && *i <= 255 => Ok(u8::try_from(*i).unwrap_or(u8::MAX)),
                _ => Err(err!(
                    Code::XrefMalformed,
                    during = "xref-stream",
                    detail = "bad /W entry"
                )),
            })
            .collect(),
        _ => Err(err!(
            Code::XrefMalformed,
            during = "xref-stream",
            detail = "/W is not an array"
        )),
    }
}

/// Read a decimal integer starting at `pos`.
fn read_int(src: &[u8], mut p: usize) -> Result<(u64, usize)> {
    while let Some(&b) = src.get(p) {
        if !(b.is_ascii_whitespace()) {
            break;
        }
        p = p.saturating_add(1);
    }
    let mut v = 0u64;
    let mut seen = false;
    while let Some(&b) = src.get(p) {
        if b.is_ascii_digit() {
            seen = true;
            v = v
                .saturating_mul(10)
                .saturating_add(u64::from(b.wrapping_sub(b'0')));
            p = p.saturating_add(1);
        } else {
            break;
        }
    }
    if !seen {
        return Err(err!(
            Code::ObjstmMalformed,
            during = "objstm-header",
            at = p as u64
        ));
    }
    Ok((v, p))
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::indexing_slicing,
        clippy::arithmetic_side_effects,
        clippy::cast_sign_loss,
        clippy::cast_possible_truncation
    )]

    use super::*;
    use selis_sandbox::{CancelToken, FixedClock};

    fn guard() -> BudgetGuard<'static> {
        Budget::unlimited().guard_with(&FixedClock(0), CancelToken::new())
    }

    fn dict(pairs: Vec<(&[u8], Obj)>) -> Vec<(selis_bytes::Bytes, Obj)> {
        pairs
            .into_iter()
            .map(|(k, v)| (selis_bytes::Bytes::copy_from_slice(k), v))
            .collect()
    }

    #[test]
    fn xref_stream_with_default_index() {
        // /W [1 4 2], /Size 4, no /Index (defaults to [0 4]).
        // Entries: type=1, offset=100; type=1, offset=200; type=0; type=2.
        let payload = [
            1u8, 0, 0, 0, 100, 0, 0, // entry 0: type 1, offset 100, gen 0
            1, 0, 0, 0, 200, 0, 0, // entry 1: type 1, offset 200
            0, 0, 0, 0, 0, 0, 0, // entry 2: type 0, free
            2, 0, 0, 0, 5, 0, 1, // entry 3: type 2, objstm 5, index 1
        ];
        let d = dict(vec![
            (
                b"Type",
                Obj::Name(selis_bytes::Bytes::copy_from_slice(b"XRef")),
            ),
            (
                b"W",
                Obj::Array(vec![Obj::Int(1), Obj::Int(4), Obj::Int(2)]),
            ),
            (b"Size", Obj::Int(4)),
        ]);
        let budget = Budget::unlimited();
        let mut g = guard();
        let xs = parse_xref_stream(&d, &payload, &budget, &mut g).expect("parse");
        assert_eq!(xs.w, [1, 4, 2]);
        assert_eq!(xs.entries.len(), 4);
        assert_eq!(xs.entries[0], (1, 100, 0));
        assert_eq!(xs.entries[3], (2, 5, 1));

        // Applying to an index yields the entries.
        let mut index = BTreeMap::new();
        xs.apply(&mut index);
        assert_eq!(
            index.get(&1),
            Some(&XrefEntry::InUse {
                offset: 200,
                gen: 0
            })
        );
        assert_eq!(
            index.get(&3),
            Some(&XrefEntry::Compressed {
                objstm: 5,
                index: 1
            })
        );
    }

    /// A non-zero, non-contiguous `/Index` subsection lands entries on the
    /// declared object numbers, not from 0.
    #[test]
    fn xref_stream_applies_subsection_index() {
        // /Index [5 1 8 2]: object 5 (in-use), objects 8, 9 (compressed).
        let payload = [
            1u8, 0, 0, 0, 50, 0, 0, // entry 0: type 1, offset 50
            2, 0, 0, 0, 6, 0, 0, // entry 1: type 2, objstm 6, index 0
            2, 0, 0, 0, 6, 0, 1, // entry 2: type 2, objstm 6, index 1
        ];
        let d = dict(vec![
            (
                b"W",
                Obj::Array(vec![Obj::Int(1), Obj::Int(4), Obj::Int(2)]),
            ),
            (b"Size", Obj::Int(10)),
            (
                b"Index",
                Obj::Array(vec![Obj::Int(5), Obj::Int(1), Obj::Int(8), Obj::Int(2)]),
            ),
        ]);
        let budget = Budget::unlimited();
        let mut g = guard();
        let xs = parse_xref_stream(&d, &payload, &budget, &mut g).expect("parse");
        assert_eq!(xs.index, vec![5, 1, 8, 2]);
        let mut index = BTreeMap::new();
        xs.apply(&mut index);
        assert_eq!(
            index.get(&5),
            Some(&XrefEntry::InUse { offset: 50, gen: 0 })
        );
        assert_eq!(
            index.get(&8),
            Some(&XrefEntry::Compressed {
                objstm: 6,
                index: 0
            })
        );
        assert_eq!(
            index.get(&9),
            Some(&XrefEntry::Compressed {
                objstm: 6,
                index: 1
            })
        );
        // Objects outside the subsections are absent.
        assert!(index.get(&0).is_none());
        assert!(index.get(&6).is_none());
    }

    #[test]
    fn xref_stream_zero_width_field() {
        let payload = [1u8, 0, 42, 1, 0, 43];
        let d = dict(vec![
            (
                b"W",
                Obj::Array(vec![Obj::Int(1), Obj::Int(0), Obj::Int(2)]),
            ),
            (b"Size", Obj::Int(2)),
        ]);
        let budget = Budget::unlimited();
        let mut g = guard();
        let xs = parse_xref_stream(&d, &payload, &budget, &mut g).expect("parse");
        assert_eq!(xs.entries[0], (1, 0, 42));
        assert_eq!(xs.entries[1], (1, 0, 43));
    }

    #[test]
    fn payload_too_short_reads_the_entries_that_fit() {
        // A damaged writer can declare /Size 10 (via the default /Index) while
        // shipping only 3 payload bytes: the parser must not trust the count —
        // it reads the 0 complete rows that fit instead of erroring
        // (SL-1.ROB.01, mirroring the classic-xref bomb behaviour).
        let d = dict(vec![
            (
                b"W",
                Obj::Array(vec![Obj::Int(2), Obj::Int(4), Obj::Int(2)]),
            ),
            (b"Size", Obj::Int(10)),
        ]);
        let budget = Budget::unlimited();
        let mut g = guard();
        let xs = parse_xref_stream(&d, &[1, 2, 3], &budget, &mut g).expect("parse");
        assert!(xs.entries.is_empty(), "no complete row fits");
    }

    #[test]
    fn object_stream_header_parses() {
        // /N 2 /First 8: header "1 0 2 4" = 8 bytes, then two objects.
        let payload = b"1 0 2 4 << /A 1 >> << /B 2 >>".to_vec();
        let d = dict(vec![(b"N", Obj::Int(2)), (b"First", Obj::Int(8))]);
        let budget = Budget::unlimited();
        let mut g = guard();
        let ranges = parse_object_stream(&d, &payload, &budget, &mut g).expect("parse");
        assert_eq!(ranges.len(), 2);
        assert_eq!(ranges[0].0, 1);
        assert_eq!(ranges[1].0, 2);
        // Object 1's range starts after /First.
        assert_eq!(ranges[0].1.start, 8);
    }
}
