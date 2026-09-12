//! CID font width resolution (SL-3.FONT.07) — ISO 32000-2:2020 §9.7.4.3.
//!
//! A CIDFont carries its widths in the `/W` (and `/W2` for vertical) arrays
//! rather than a simple `/Widths` array (the structure deferred from
//! SL-3.FONT.01). The `/W` array alternates CIDs and width arrays:
//!
//! ```text
//! /W [ c0 [w0 w1 ...] c1 c2 c3 [w0 w1 w2] ]
//! ```
//!
//! * a CID immediately followed by an array gives widths for the CIDs
//!   `c..=c+n-1`;
//! * a run of CIDs followed by an array gives those CIDs the widths in
//!   order.
//!
//! `/DW` is the default width for any CID with no explicit entry.
//!
//! This module is COS-free: the caller (which holds the COS edge) resolves
//! the `/W` array into [`CidWidthEntry`]s.

use std::collections::BTreeMap;

use selis_bytes::Bytes;

/// How a CID maps to a glyph id in the embedded font program
/// (ISO 32000-2:2020 §9.7.4.2, `/CIDToGIDMap`).
#[derive(Debug, Clone, PartialEq, Default)]
pub enum CidToGid {
    /// `/CIDToGIDMap /Identity` — or absent, which the spec defines as
    /// identity: the CID *is* the glyph id in the embedded program.
    #[default]
    Identity,
    /// A stream `/CIDToGIDMap`: 2-byte big-endian glyph ids, one per CID.
    /// A short stream is a deviation: CIDs past its end have no glyph.
    Table(Bytes),
}

/// The glyph id for `cid`, or `None` when a stream map has no entry for it
/// (a deviation — the caller skips the glyph, never fails the page).
#[must_use]
pub fn map_cid(map: &CidToGid, cid: u16) -> Option<u16> {
    match map {
        CidToGid::Identity => Some(cid),
        CidToGid::Table(bytes) => {
            let i = usize::from(cid).checked_mul(2)?;
            let pair = bytes.as_slice().get(i..i.saturating_add(2))?;
            let (hi, lo) = (pair.first().copied(), pair.get(1).copied());
            match (hi, lo) {
                (Some(h), Some(l)) => Some(u16::from_be_bytes([h, l])),
                _ => None,
            }
        }
    }
}

/// One element of a `/W` (or `/W2`) array: a CID, or a width array.
#[derive(Debug, Clone, PartialEq)]
pub enum CidWidthEntry {
    /// A CID (the first number of a range, or part of a run).
    Cid(u32),
    /// A width array (glyph-space widths, 1000 units per em).
    Widths(Vec<f64>),
}

/// Resolved CID widths: explicit entries plus a default.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct CidWidths {
    /// CID → width.
    map: BTreeMap<u32, f64>,
    /// The default width (`/DW`).
    default: f64,
}

impl CidWidths {
    /// The width for a CID (the explicit entry, else `/DW`).
    #[must_use]
    pub fn width(&self, cid: u32) -> f64 {
        self.map.get(&cid).copied().unwrap_or(self.default)
    }

    /// The default width.
    #[must_use]
    pub fn default_width(&self) -> f64 {
        self.default
    }
}

/// Resolve a `/W` array (plus `/DW`) into CID widths.
///
/// # Malformed Input
///
/// A width array that is shorter than the run it serves is a deviation: the
/// missing trailing CIDs fall back to `/DW`, never an error.
#[must_use]
pub fn resolve_cid_widths(entries: &[CidWidthEntry], default: f64) -> CidWidths {
    let mut map = BTreeMap::new();
    let mut pending: Vec<u32> = Vec::new();
    let mut i = 0usize;
    while i < entries.len() {
        match entries.get(i) {
            Some(CidWidthEntry::Cid(c)) => {
                // A CID immediately followed by an array starts a range only
                // when there is no pending run of CIDs awaiting widths.
                let next_is_widths = matches!(
                    entries.get(i.saturating_add(1)),
                    Some(CidWidthEntry::Widths(_))
                );
                if next_is_widths && pending.is_empty() {
                    if let Some(CidWidthEntry::Widths(widths)) = entries.get(i.saturating_add(1)) {
                        for (k, w) in widths.iter().enumerate() {
                            let k = u32::try_from(k).unwrap_or(u32::MAX);
                            map.insert(c.saturating_add(k), *w);
                        }
                    }
                    i = i.saturating_add(2);
                } else {
                    pending.push(*c);
                    i = i.saturating_add(1);
                }
            }
            Some(CidWidthEntry::Widths(widths)) => {
                // Widths for the pending run of CIDs.
                for (k, w) in widths.iter().enumerate() {
                    if let Some(cid) = pending.get(k).copied() {
                        map.insert(cid, *w);
                    }
                }
                pending.clear();
                i = i.saturating_add(1);
            }
            None => break,
        }
    }
    CidWidths { map, default }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn range_form() {
        // /W [ 55 [500 600 700] ] → CID 55=500, 56=600, 57=700.
        let widths = resolve_cid_widths(
            &[
                CidWidthEntry::Cid(55),
                CidWidthEntry::Widths(vec![500.0, 600.0, 700.0]),
            ],
            1000.0,
        );
        assert_eq!(widths.width(55), 500.0);
        assert_eq!(widths.width(56), 600.0);
        assert_eq!(widths.width(57), 700.0);
        assert_eq!(widths.width(58), 1000.0); // default
    }

    #[test]
    fn run_form() {
        // /W [ 72 75 80 [700 750 800] ] → 72=700, 75=750, 80=800.
        let widths = resolve_cid_widths(
            &[
                CidWidthEntry::Cid(72),
                CidWidthEntry::Cid(75),
                CidWidthEntry::Cid(80),
                CidWidthEntry::Widths(vec![700.0, 750.0, 800.0]),
            ],
            0.0,
        );
        assert_eq!(widths.width(72), 700.0);
        assert_eq!(widths.width(75), 750.0);
        assert_eq!(widths.width(80), 800.0);
        assert_eq!(widths.width(81), 0.0);
    }

    #[test]
    fn mixed_forms() {
        // /W [ 0 [500] 5 10 [700 800] ] → 0=500, 5=700, 10=800.
        let widths = resolve_cid_widths(
            &[
                CidWidthEntry::Cid(0),
                CidWidthEntry::Widths(vec![500.0]),
                CidWidthEntry::Cid(5),
                CidWidthEntry::Cid(10),
                CidWidthEntry::Widths(vec![700.0, 800.0]),
            ],
            999.0,
        );
        assert_eq!(widths.width(0), 500.0);
        assert_eq!(widths.width(5), 700.0);
        assert_eq!(widths.width(10), 800.0);
        assert_eq!(widths.width(6), 999.0);
    }

    #[test]
    fn short_array_is_a_deviation() {
        // A run of 3 CIDs with only 2 widths: the third falls back to /DW.
        let widths = resolve_cid_widths(
            &[
                CidWidthEntry::Cid(1),
                CidWidthEntry::Cid(2),
                CidWidthEntry::Cid(3),
                CidWidthEntry::Widths(vec![100.0, 200.0]),
            ],
            50.0,
        );
        assert_eq!(widths.width(1), 100.0);
        assert_eq!(widths.width(2), 200.0);
        assert_eq!(widths.width(3), 50.0);
    }

    #[test]
    fn empty_is_all_defaults() {
        let widths = resolve_cid_widths(&[], 42.0);
        assert_eq!(widths.width(0), 42.0);
        assert_eq!(widths.default_width(), 42.0);
    }

    #[test]
    fn identity_maps_cid_to_itself() {
        assert_eq!(map_cid(&CidToGid::Identity, 0), Some(0));
        assert_eq!(map_cid(&CidToGid::Identity, 56), Some(56));
        assert_eq!(map_cid(&CidToGid::Identity, u16::MAX), Some(u16::MAX));
        // Absent is identity (the spec default).
        assert_eq!(map_cid(&CidToGid::default(), 7), Some(7));
    }

    #[test]
    fn table_maps_per_cid_big_endian() {
        // CIDs 0..=2 → GIDs 10, 0x0102, 0xFFFF.
        let table = CidToGid::Table(Bytes::copy_from_slice(&[0, 10, 1, 2, 255, 255]));
        assert_eq!(map_cid(&table, 0), Some(10));
        assert_eq!(map_cid(&table, 1), Some(0x0102));
        assert_eq!(map_cid(&table, 2), Some(0xFFFF));
    }

    #[test]
    fn short_table_is_a_deviation_not_an_error() {
        let table = CidToGid::Table(Bytes::copy_from_slice(&[0, 10, 1]));
        assert_eq!(map_cid(&table, 0), Some(10));
        // CID 1's pair is truncated: no glyph, not a panic.
        assert_eq!(map_cid(&table, 1), None);
        assert_eq!(map_cid(&table, 60000), None);
    }
}
