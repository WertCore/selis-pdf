//! Damaged-file reconstruction (SL-1.COS.06).
//!
//! When the xref is unusable, scan the whole file for `N G obj` patterns,
//! rebuild an index, recover the trailer by finding a `/Root`, and reconstruct
//! page-tree order. Bounded by the budget; reports a `Reconstructed` deviation.
//!
//! The reconstruction **never writes to the user's file** (ADR-P0007,
//! 01-ARCHITECTURE.md §13): it only builds an in-memory index.
//!
//! This is where competitors differentiate on "opens files Acrobat rejects",
//! and it is the single most fuzz-sensitive code in the codebase.

use std::collections::BTreeMap;

use selis_error::{err, Code, Result};
use selis_sandbox::{Budget, BudgetGuard};

use crate::obj::Obj;
use crate::revision::Doc;
use crate::xref::XrefEntry;

/// The result of a reconstruction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reconstruction {
    /// The rebuilt index (object number → byte offset of its `N G obj`).
    pub index: BTreeMap<u32, u64>,
    /// The highest object number seen (drives the `/Size` guess).
    pub size: u64,
}

/// Scan the whole buffer for `N G obj` headers and rebuild an index.
///
/// # Budget
///
/// Scans every byte; the object budget bounds the number of headers recorded.
///
/// # Malformed Input
///
/// Returns `XREF_UNRECOVERABLE` when no `N G obj` header is found.
pub fn reconstruct_index(
    src: &[u8],
    _budget: &Budget,
    g: &mut BudgetGuard<'_>,
) -> Result<Reconstruction> {
    let mut index: BTreeMap<u32, u64> = BTreeMap::new();
    let mut i = 0usize;
    let mut size = 0u64;
    let mut steps = 0u32;

    while i < src.len() && steps < 1_000_000 {
        steps = steps.saturating_add(1);
        g.tick()?;
        // Look for `N G obj` — an object header.
        let tail = src.get(i..).unwrap_or(&[]);
        let Some(rel) = find_obj_header(tail) else {
            break;
        };
        let pos = i.saturating_add(rel);
        // Parse the numbers from the header.
        let header = src.get(pos..).unwrap_or(&[]);
        match read_obj_header(header) {
            Ok((num, _gen, next)) => {
                g.charge_one(selis_sandbox::Resource::Objects)?;
                let num = u32::try_from(num).unwrap_or(u32::MAX);
                index.insert(num, pos as u64);
                size = size.max(u64::from(num));
                // Jump straight past the header. `next` is at least the width of
                // ` obj`, so this always advances; the previous byte-at-a-time
                // crawl re-discovered the same header O(n) times and exhausted
                // both the object budget and the wall clock (SL-1.ROB.01).
                i = pos.saturating_add(next.max(1));
            }
            // A malformed header: skip past its start and keep scanning rather
            // than aborting the whole reconstruction.
            Err(_) => {
                i = pos.saturating_add(1);
            }
        }
    }

    if index.is_empty() {
        return Err(err!(
            Code::XrefUnrecoverable,
            during = "reconstruct",
            detail = "no object headers"
        ));
    }

    Ok(Reconstruction {
        index,
        size: size.saturating_add(1),
    })
}

/// Find the next `N G obj` header in `src`, returning its relative offset.
fn find_obj_header(src: &[u8]) -> Option<usize> {
    let mut i = 0usize;
    while i.saturating_add(6) <= src.len() {
        if src.get(i..).is_some_and(|rest| rest.starts_with(b" obj")) {
            // Walk back from `i` over the generation and object numbers.
            let mut j = i;
            // Skip the space before "obj" (already verified by starts_with).
            let mut gen_digits = 0usize;
            while j > 0 {
                j = j.wrapping_sub(1);
                if src.get(j).is_some_and(|b| b.is_ascii_digit()) {
                    gen_digits = gen_digits.saturating_add(1);
                } else {
                    break;
                }
            }
            if gen_digits == 0 {
                i = i.saturating_add(1);
                continue;
            }
            // j now points at the space before the generation number.
            if src.get(j) != Some(&b' ') {
                i = i.saturating_add(1);
                continue;
            }
            // Walk back over the object number.
            let mut num_digits = 0usize;
            while j > 0 {
                j = j.wrapping_sub(1);
                if src.get(j).is_some_and(|b| b.is_ascii_digit()) {
                    num_digits = num_digits.saturating_add(1);
                } else {
                    break;
                }
            }
            if num_digits == 0 {
                i = i.saturating_add(1);
                continue;
            }
            // `j` points at a digit only when the number begins at byte 0;
            // otherwise it points at the byte before the number.
            let start = if src.get(j).is_some_and(|b| b.is_ascii_digit()) {
                j
            } else {
                j.saturating_add(1)
            };
            return Some(start);
        }
        i = i.saturating_add(1);
    }
    None
}

/// Parse `num gen obj` at the start of `src`, returning `(num, gen, next_pos)`.
fn read_obj_header(src: &[u8]) -> Result<(i64, i64, usize)> {
    let p = 0usize;
    let (num, p) = read_int(src, p)?;
    let (gen, p) = read_int(src, p)?;
    // Expect ` obj`.
    let tail = src.get(p..p.saturating_add(4)).unwrap_or(&[]);
    if tail != b" obj" {
        return Err(err!(
            Code::XrefUnrecoverable,
            during = "reconstruct",
            at = p as u64
        ));
    }
    Ok((num, gen, p.saturating_add(4)))
}

/// Recover the trailer by locating a `/Root` reference anywhere in the file.
///
/// Scans for the bytes `<<` … `/Root <num> <gen> R` and returns the reference.
///
/// # Budget
///
/// Single pass, no heap allocation beyond the returned object.
///
/// # Malformed Input
///
/// `TRAILER_MISSING_ROOT` when no `/Root` is found.
pub fn recover_root(src: &[u8]) -> Result<Obj> {
    Ok(recover_root_full(src)?.0)
}

/// [`recover_root`], also returning the byte offset of the `/Root` value when
/// it is an inline dictionary (which cannot be referenced by number — the
/// caller synthesises an index entry pointing at those bytes).
fn recover_root_full(src: &[u8]) -> Result<(Obj, Option<u64>)> {
    // A cheap scan: look for `/Root` followed by a reference.
    let mut i = 0usize;
    while i.saturating_add(5) <= src.len() {
        if src.get(i..).is_some_and(|rest| rest.starts_with(b"/Root")) {
            let after = i.saturating_add(5);
            let mut p = after;
            while src
                .get(p)
                .is_some_and(|b| matches!(b, b' ' | b'\t' | b'\n' | b'\r'))
            {
                p = p.saturating_add(1);
            }
            if src.get(p) == Some(&b'<') && src.get(p.saturating_add(1)) == Some(&b'<') {
                // An inline catalog dict: `<< /Pages <num> <gen> R >>`. Extract
                // the /Pages pair and resolve the value bytes later (a dict
                // cannot be an xref target). Trailing whitespace tolerated.
                if let Some((pages, voff)) = inline_dict_pages(src, p) {
                    let mut pairs: Vec<(selis_bytes::Bytes, Obj)> = vec![(
                        selis_bytes::Bytes::copy_from_slice(b"Type"),
                        Obj::Name(selis_bytes::Bytes::copy_from_slice(b"Catalog")),
                    )];
                    pairs.push((
                        selis_bytes::Bytes::copy_from_slice(b"Pages"),
                        Obj::Ref(pages),
                    ));
                    return Ok((Obj::Dict(pairs), Some(voff)));
                }
            }
            if let Ok((num, next)) = read_int(src, p) {
                let p2 = next;
                if let Ok((gen, next2)) = read_int(src, p2) {
                    let after_ref = next2;
                    if src
                        .get(after_ref)
                        .is_some_and(|b| matches!(b, b' ' | b'\t' | b'\n' | b'\r'))
                    {
                        let r = after_ref.saturating_add(1);
                        if src.get(r..r.saturating_add(1)) == Some(b"R") {
                            return Ok((
                                Obj::Ref(crate::obj::Ref::new(
                                    u32::try_from(num).unwrap_or(u32::MAX),
                                    u16::try_from(gen).unwrap_or(u16::MAX),
                                )),
                                None,
                            ));
                        }
                    }
                }
            }
        }
        i = i.saturating_add(1);
    }
    Err(err!(
        Code::TrailerMissingRoot,
        during = "recover-root",
        detail = "no /Root found"
    ))
}

/// Extract `/Pages <num> <gen> R` from an inline `/Root << ... >>` dict,
/// returning the reference and the byte offset of the dict's opening `<<`.
fn inline_dict_pages(src: &[u8], start: usize) -> Option<(crate::obj::Ref, u64)> {
    // Track nesting to find the matching `>>`.
    let mut depth = 0u32;
    let mut p = start;
    let mut pages_ref: Option<crate::obj::Ref> = None;
    while p < src.len() {
        if src.get(p..p.saturating_add(2)) == Some(b"<<") {
            depth = depth.saturating_add(1);
            p = p.saturating_add(2);
        } else if src.get(p..p.saturating_add(2)) == Some(b">>") {
            if depth == 0 {
                break;
            }
            depth = depth.saturating_sub(1);
            p = p.saturating_add(2);
        } else if src.get(p..).is_some_and(|rest| rest.starts_with(b"/Pages")) {
            let mut q = p.saturating_add(6);
            while src
                .get(q)
                .is_some_and(|b| matches!(b, b' ' | b'\t' | b'\n' | b'\r'))
            {
                q = q.saturating_add(1);
            }
            if let Ok((num, n2)) = read_int(src, q) {
                if let Ok((gen, n3)) = read_int(src, n2) {
                    let r = n3;
                    if src
                        .get(r)
                        .is_some_and(|b| matches!(b, b' ' | b'\t' | b'\n' | b'\r'))
                    {
                        let rr = r.saturating_add(1);
                        if src.get(rr..rr.saturating_add(1)) == Some(b"R") {
                            pages_ref = Some(crate::obj::Ref::new(
                                u32::try_from(num).unwrap_or(u32::MAX),
                                u16::try_from(gen).unwrap_or(u16::MAX),
                            ));
                        }
                    }
                }
            }
            p = q;
        } else {
            p = p.saturating_add(1);
        }
    }
    pages_ref.map(|r| (r, u64::try_from(start).unwrap_or(0)))
}

/// Reconstruct a full document from a damaged file.
///
/// # Budget
///
/// As [`reconstruct_index`].
///
/// # Malformed Input
///
/// `XREF_UNRECOVERABLE` when no object headers exist; `TRAILER_MISSING_ROOT`
/// when no `/Root` can be recovered.
pub fn reconstruct(
    src: &[u8],
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
) -> Result<(Doc, crate::Deviation)> {
    let rec = reconstruct_index(src, budget, g)?;
    let (root, inline_offset) = recover_root_full(src)?;
    // Build a single-revision document whose index is the reconstruction.
    let mut entries = BTreeMap::new();
    for (num, offset) in &rec.index {
        entries.insert(
            *num,
            XrefEntry::InUse {
                offset: *offset,
                gen: 0,
            },
        );
    }
    // An inline `/Root << ... >>` dict cannot be referenced by number:
    // synthesize an index entry pointing at its value bytes so the catalog
    // resolves from there (SL-1.ROB.01).
    let root = match (root, inline_offset) {
        (Obj::Dict(_), Some(off)) => {
            let synthetic = entries
                .keys()
                .next_back()
                .copied()
                .unwrap_or(0)
                .saturating_add(1);
            entries.insert(
                synthetic,
                XrefEntry::InUse {
                    offset: off,
                    gen: 0,
                },
            );
            Obj::Ref(crate::obj::Ref::new(synthetic, 0))
        }
        (other, _) => other,
    };
    let trailer = vec![(selis_bytes::Bytes::copy_from_slice(b"Root"), root)];
    let doc = Doc::from_single_revision(entries, trailer);
    let deviation = crate::Deviation::ReconstructedIndex { offset: 0 };
    Ok((doc, deviation))
}

fn read_int(src: &[u8], mut p: usize) -> Result<(i64, usize)> {
    // Skip whitespace so `1 0 obj` parses as three fields.
    while src
        .get(p)
        .is_some_and(|b| matches!(b, b' ' | b'\t' | b'\n' | b'\r'))
    {
        p = p.saturating_add(1);
    }
    let mut v: i64 = 0;
    let mut seen = false;
    while let Some(&b) = src.get(p) {
        if b.is_ascii_digit() {
            seen = true;
            v = v
                .checked_mul(10)
                .and_then(|x| x.checked_add(i64::from(b.wrapping_sub(b'0'))))
                .ok_or_else(|| {
                    err!(
                        Code::LexNumberOverflow,
                        during = "reconstruct",
                        at = p as u64
                    )
                })?;
            p = p.saturating_add(1);
        } else {
            break;
        }
    }
    if !seen {
        return Err(err!(
            Code::XrefUnrecoverable,
            during = "reconstruct",
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
        clippy::cast_sign_loss
    )]

    use super::*;
    use selis_sandbox::{CancelToken, FixedClock};

    fn guard() -> BudgetGuard<'static> {
        Budget::unlimited().guard_with(&FixedClock(0), CancelToken::new())
    }

    /// A file with no valid xref: just object bodies.
    fn damaged_file() -> Vec<u8> {
        b"%PDF-1.4\n1 0 obj\n<< /Type /Catalog /Root 1 0 R >>\nendobj\n2 0 obj\n<< /Pages 3 0 R >>\nendobj\n3 0 obj\n[ ]\nendobj\n".to_vec()
    }

    #[test]
    fn reconstruct_finds_object_headers() {
        let pdf = damaged_file();
        let budget = Budget::unlimited();
        let mut g = guard();
        let rec = reconstruct_index(&pdf, &budget, &mut g).expect("reconstruct");
        assert!(rec.index.contains_key(&1));
        assert!(rec.index.contains_key(&2));
        assert!(rec.index.contains_key(&3));
        assert!(rec.size >= 3);
    }

    #[test]
    fn no_headers_is_xref_unrecoverable() {
        let pdf = b"%PDF-1.4\njust some bytes\n%%EOF\n".to_vec();
        let budget = Budget::unlimited();
        let mut g = guard();
        let e = reconstruct_index(&pdf, &budget, &mut g).expect_err("unrecoverable");
        assert_eq!(e.code(), Code::XrefUnrecoverable);
    }

    #[test]
    fn recover_root_finds_a_reference() {
        let pdf: &[u8] = b"%PDF-1.4\n5 0 obj\n<< /Root 1 0 R >>\nendobj\n";
        let root = recover_root(pdf).expect("root");
        assert_eq!(root, Obj::Ref(crate::obj::Ref::new(1, 0)));
    }

    #[test]
    fn reconstruct_produces_a_doc_and_deviation() {
        let pdf = damaged_file();
        let budget = Budget::unlimited();
        let mut g = guard();
        let (doc, dev) = reconstruct(&pdf, &budget, &mut g).expect("reconstruct");
        assert_eq!(doc.len(), 1);
        assert!(matches!(dev, crate::Deviation::ReconstructedIndex { .. }));
    }
}
