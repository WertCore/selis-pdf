//! Compressed cross-reference path for the linearization writer.
//!
//! This is the write counterpart of `selis_pdf_cos::xref_stream`. It emits the
//! *same* linearized part order and Annex F hint tables as the classic path in
//! `linearize.rs`, but represents both xref sections as PDF 1.5 cross-reference
//! streams (`/Type /XRef`) and packs eligible non-stream objects into object
//! streams (`/ObjStm`).
//!
//! # Layout
//!
//! ```text
//! header + /Linearized dict (padded)          object 1
//! first-page XRef stream  <- startxref        its own object number
//! part 4 (catalog, direct)
//! part 6 (first page): direct objects, then this section's /ObjStm
//! part 7 (each later page): direct objects, then that section's /ObjStm
//! part 8 (shared), part 9 (remainder): same pattern
//! hint stream                                 the largest "classic" number
//! main XRef stream (all objects, /Prev-free)
//! startxref -> first-page XRef stream, %%EOF
//! ```
//!
//! The first-page xref stream carries `/Prev` -> main xref stream, so a reader
//! following `startxref` sees the first page immediately and resolves the rest
//! through the chain. A *hybrid* file would need `/XRefStm` in a classic
//! trailer; this path is pure xref-stream (both sections are streams), so
//! `/XRefStm` is neither needed nor emitted.
//!
//! # Hint-table offsets for compressed objects
//!
//! Annex F tables store byte offsets/lengths. A compressed object has no
//! `N 0 obj` header, so this writer uses a documented, self-consistent view:
//!
//! * direct object -> offset of its `N 0 obj`, length of that record;
//! * compressed object -> offset of the containing `/ObjStm` object. The
//!   container's whole byte length is charged to its *first* member and zero
//!   to the remaining members, so a section's summed lengths equal the real
//!   physical byte span from its first object to the next section.
//!
//! Grouping a section's compressible objects into one stream keeps those
//! "storage unit" offsets non-decreasing in layout order, so the Annex F
//! cumulative page offsets chain exactly. `tools/check_linearization.py`
//! reconstructs the same view from the xref and object streams and cross-checks
//! every hint field against it.

// This module serialises a byte-exact binary layout. Indexing and arithmetic
// are over vectors whose bounds are established while classifying parts, and
// over checked file offsets. The hostile-input boundary is the object graph,
// validated by `build_index`/`discover_page_tree` in the parent module before
// this code runs.
#![forbid(unsafe_code)]
#![allow(
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::unwrap_used,
    clippy::expect_used
)]

use std::collections::BTreeMap;

use selis_bytes::Bytes;
use selis_error::Result;
use selis_pdf_cos::Obj;
use selis_sandbox::{Budget, BudgetGuard, Resource};

use super::{
    build_hint_buffer, int, key, name_of, to_error, Guess, Layout, LinearizeError, LinearizeInput,
    Sizes, PREFIX_LEN,
};

/// `/W [1 4 3]`: 1-byte type, 4-byte offset/object number, 3-byte generation
/// or object-stream index. Row width is 8 bytes.
const W: [u8; 3] = [1, 4, 3];
const ROW_WIDTH: usize = 8;
/// Largest object number storable in the 3-byte field (`f2`/`f3`).
const MAX_FIELD: u32 = 0x00ff_ffff;

/// How one object number is addressed by an xref-stream entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Entry {
    /// Entry 0 / a free slot.
    Free,
    /// A direct `N 0 obj` record at this file offset.
    Direct(u64),
    /// Object `index` (0-based) inside object stream `objstm`.
    Compressed { objstm: u32, index: u32 },
}

impl Entry {
    /// Encode as one `/W [1 4 3]` row.
    fn encode(self, out: &mut Vec<u8>) {
        match self {
            Self::Free => out.extend_from_slice(&[0u8; ROW_WIDTH]),
            Self::Direct(off) => {
                // `validate` guarantees this fits before any encode happens.
                let wide = off as u32;
                out.push(1);
                out.extend_from_slice(&wide.to_be_bytes());
                out.extend_from_slice(&[0u8; 3]);
            }
            Self::Compressed { objstm, index } => {
                out.push(2);
                out.extend_from_slice(&objstm.to_be_bytes());
                out.extend_from_slice(&index.to_be_bytes()[1..]);
            }
        }
    }
}

/// Fail loudly rather than emit a silently truncated offset.
fn validate(entries: &BTreeMap<u32, Entry>) -> Result<()> {
    for entry in entries.values() {
        match *entry {
            Entry::Direct(off) if off > u64::from(u32::MAX) => {
                return Err(to_error(LinearizeError::UnsupportedLayout(
                    "file offset exceeds the 4-byte /W field",
                )));
            }
            Entry::Compressed { objstm, index } if objstm > MAX_FIELD || index > MAX_FIELD => {
                return Err(to_error(LinearizeError::UnsupportedLayout(
                    "object-stream reference exceeds the 3-byte /W field",
                )));
            }
            _ => {}
        }
    }
    Ok(())
}

/// A serialized object-stream record ready to be placed in the file.
struct BuiltObjStm {
    /// Object number of the `/ObjStm`.
    num: u32,
    /// Full `N 0 obj ... endobj` bytes.
    bytes: Vec<u8>,
    /// `(member object number, index, decoded value length)` per member; the
    /// length is informational only (hint tables use the storage-unit view).
    members: Vec<(u32, u32, u64)>,
    /// Filled once the record's file offset is known.
    offset: u64,
}

/// Decide whether an object must stay a direct object.
///
/// Stream objects, page/page-tree dictionaries and the catalog cannot be
/// packed into an object stream for a linearized file: the page dictionaries
/// carry the byte offsets the hint tables describe, and the catalog is the
/// trailer `/Root`. Everything else (resources, fonts, annotations, outlines,
/// info, ...) is eligible.
fn keep_direct(num: u32, obj: &Obj, root: super::Ref) -> bool {
    if num == root.num || matches!(obj, Obj::Stream { .. }) {
        return true;
    }
    matches!(
        name_of(obj, b"Type").as_deref(),
        Some(b"Page") | Some(b"Pages")
    )
}

/// Which contiguous layout region an object belongs to.
///
/// Region 0 is part 4 (catalog), regions `1..=page_count` are the page
/// sections, `page_count + 1` is part 8 (shared) and `page_count + 2` is part 9
/// (remainder). Each region > 0 gets at most one object stream.
fn assign_regions(layout: &Layout) -> Result<Vec<usize>> {
    let n = layout.new_objects.len();
    let mut group_of = vec![0usize; n];
    let mut cursor = layout.part4_len;
    for (gi, section) in layout.page_sections.iter().enumerate() {
        let end = cursor
            .checked_add(section.len())
            .filter(|end| *end <= n)
            .ok_or_else(|| to_error(LinearizeError::UnsupportedLayout("page section overflow")))?;
        for slot in group_of
            .get_mut(cursor..end)
            .ok_or_else(|| to_error(LinearizeError::UnsupportedLayout("page section range")))?
        {
            *slot = gi + 1;
        }
        cursor = end;
    }
    let shared = layout.page_sections.len() + 1;
    let end = cursor
        .checked_add(layout.part8.len())
        .filter(|end| *end <= n)
        .ok_or_else(|| to_error(LinearizeError::UnsupportedLayout("part 8 overflow")))?;
    for slot in group_of
        .get_mut(cursor..end)
        .ok_or_else(|| to_error(LinearizeError::UnsupportedLayout("part 8 range")))?
    {
        *slot = shared;
    }
    for slot in group_of
        .get_mut(end..)
        .ok_or_else(|| to_error(LinearizeError::UnsupportedLayout("part 9 range")))?
    {
        *slot = shared + 1;
    }
    Ok(group_of)
}

/// Serialize one region's compressible objects into an `/ObjStm`.
fn build_objstm(
    num: u32,
    members: &[(u32, Obj)],
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
) -> Result<BuiltObjStm> {
    if members.len() > MAX_FIELD as usize {
        return Err(to_error(LinearizeError::UnsupportedLayout(
            "object stream has too many members",
        )));
    }
    if num > MAX_FIELD {
        return Err(to_error(LinearizeError::UnsupportedLayout(
            "object stream number exceeds /W field",
        )));
    }

    // Body: each serialized object value followed by one separator byte so
    // adjacent tokens cannot merge. The separator belongs to the preceding
    // object's byte range, which the parser tolerates as trailing whitespace.
    let mut body = Vec::new();
    let mut offsets: Vec<u32> = Vec::with_capacity(members.len());
    let mut lengths: Vec<u64> = Vec::with_capacity(members.len());
    for (_, obj) in members {
        offsets.push(u32::try_from(body.len()).map_err(|_| {
            to_error(LinearizeError::UnsupportedLayout(
                "object stream payload too large",
            ))
        })?);
        let mut w = selis_pdf_cos::Writer::new(budget);
        let value = w.to_bytes(obj, g)?;
        lengths.push(value.len() as u64);
        body.extend_from_slice(&value);
        body.push(b' ');
    }
    let mut header = Vec::new();
    for ((objnum, _), off) in members.iter().zip(offsets.iter()) {
        header.extend_from_slice(format!("{objnum} {off} ").as_bytes());
    }
    // `/First` is the byte length of the (objnum offset)* header, not of the
    // object body that follows it.
    let first = u32::try_from(header.len()).map_err(|_| {
        to_error(LinearizeError::UnsupportedLayout(
            "object stream header offset too large",
        ))
    })?;
    let mut payload = header;
    payload.extend_from_slice(&body);

    let compressed = selis_pdf_filter::flate_encode(&payload, 6);
    g.charge(Resource::Bytes, payload.len() as u64)?;
    g.charge(Resource::Bytes, compressed.len() as u64)?;

    let obj = Obj::Stream {
        dict: vec![
            (key(b"Type"), Obj::Name(key(b"ObjStm"))),
            (key(b"N"), int(members.len() as u64)),
            (key(b"First"), int(u64::from(first))),
            (key(b"Filter"), Obj::Name(key(b"FlateDecode"))),
        ],
        data: Bytes::from(compressed),
    };
    let bytes = super::write_one_bytes(num, &obj, budget, g)?;
    let encoded = members
        .iter()
        .enumerate()
        .map(|(i, (objnum, _))| (*objnum, i as u32, lengths[i]))
        .collect();
    Ok(BuiltObjStm {
        num,
        bytes,
        members: encoded,
        offset: 0,
    })
}

/// Serialize the entry rows of an xref stream in object-number order.
fn xref_payload(entries: &BTreeMap<u32, Entry>) -> Vec<u8> {
    let mut out = Vec::with_capacity(entries.len() * ROW_WIDTH);
    for entry in entries.values() {
        entry.encode(&mut out);
    }
    out
}

/// Derive the `/Index` array (`start count ...`) from consecutive runs of keys.
fn xref_index(entries: &BTreeMap<u32, Entry>) -> Vec<u32> {
    let mut out = Vec::new();
    let mut keys = entries.keys().copied().peekable();
    while let Some(start) = keys.next() {
        let mut count = 1u32;
        while keys.peek().is_some_and(|next| *next == start + count) {
            keys.next();
            count = count.saturating_add(1);
        }
        out.push(start);
        out.push(count);
    }
    out
}

/// Build one xref-stream object and return its bytes plus its relative `/T`.
#[allow(clippy::too_many_arguments)]
fn build_xref_stream(
    num: u32,
    entries: &BTreeMap<u32, Entry>,
    size: u32,
    root: super::Ref,
    extra: &[(Vec<u8>, Obj)],
    prev: Option<u64>,
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
) -> Result<Vec<u8>> {
    if num > MAX_FIELD || size > MAX_FIELD.saturating_add(1) {
        return Err(to_error(LinearizeError::UnsupportedLayout(
            "xref stream number exceeds /W field",
        )));
    }
    validate(entries)?;
    // The payload is intentionally left unfiltered. A Flate-compressed xref
    // stream's byte length depends on the *values* of the offsets it stores,
    // which are exactly the forward-referenced quantities the fixed-point
    // layout is solving for. Keeping it unfiltered makes the section length a
    // function of the entry count only, so one pass is exact. Object streams
    // below are still Flate-compressed, which is where the size win is.
    let payload = xref_payload(entries);
    g.charge(Resource::Bytes, payload.len() as u64)?;

    let mut dict: Vec<(Bytes, Obj)> = vec![
        (key(b"Type"), Obj::Name(key(b"XRef"))),
        (key(b"Size"), int(u64::from(size))),
        (
            key(b"W"),
            Obj::Array(W.iter().map(|w| int(u64::from(*w))).collect()),
        ),
        (
            key(b"Index"),
            Obj::Array(
                xref_index(entries)
                    .into_iter()
                    .map(|v| int(u64::from(v)))
                    .collect(),
            ),
        ),
        (key(b"Root"), Obj::Ref(root)),
    ];
    for (k, v) in extra {
        dict.push((Bytes::copy_from_slice(k), v.clone()));
    }
    if let Some(prev) = prev {
        dict.push((key(b"Prev"), int(prev)));
    }
    let obj = Obj::Stream {
        dict,
        data: Bytes::from(payload),
    };
    super::write_one_bytes(num, &obj, budget, g)
}

/// Render the whole file in compressed mode. See the module docs for layout.
pub(super) fn render(
    _input: &LinearizeInput<'_>,
    layout: &Layout,
    sizes: &Sizes,
    guess: Guess,
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
) -> Result<(Vec<u8>, Guess)> {
    let first_count = layout.part4_len + layout.part6_len;
    let regions = assign_regions(layout)?;
    let ngroups = layout.page_sections.len().saturating_add(3);
    let mut groups: Vec<Vec<(u32, Obj)>> = vec![Vec::new(); ngroups];
    for (i, (num, obj)) in layout.new_objects.iter().enumerate() {
        let group = *regions
            .get(i)
            .ok_or_else(|| to_error(LinearizeError::UnsupportedLayout("region index")))?;
        let slot = groups
            .get_mut(group)
            .ok_or_else(|| to_error(LinearizeError::UnsupportedLayout("region slot")))?;
        slot.push((*num, obj.clone()));
    }

    // Assign numbers to the object streams, then the two xref streams. All
    // live past the largest "classic" number, so the part numbering and hint
    // tables are untouched.
    let mut objstms: Vec<Option<BuiltObjStm>> = (0..ngroups).map(|_| None).collect();
    let mut next_num = layout
        .hint_obj
        .checked_add(1)
        .ok_or_else(|| to_error(LinearizeError::OutOfRange))?;
    for group in 0..ngroups {
        let members: Vec<(u32, Obj)> = groups[group]
            .iter()
            .filter(|(num, obj)| !keep_direct(*num, obj, layout.root))
            .cloned()
            .collect();
        if members.is_empty() {
            continue;
        }
        let built = build_objstm(next_num, &members, budget, g)?;
        next_num = next_num
            .checked_add(1)
            .ok_or_else(|| to_error(LinearizeError::OutOfRange))?;
        objstms[group] = Some(built);
    }
    let first_xref_num = next_num;
    let main_xref_num = first_xref_num
        .checked_add(1)
        .ok_or_else(|| to_error(LinearizeError::OutOfRange))?;
    let size = main_xref_num
        .checked_add(1)
        .ok_or_else(|| to_error(LinearizeError::OutOfRange))?;
    if main_xref_num > MAX_FIELD {
        return Err(to_error(LinearizeError::UnsupportedLayout(
            "too many objects for compressed mode",
        )));
    }

    // Seed the first-page xref stream to measure its length. /Prev is the only
    // value-dependent integer; the fixed-point loop resolves its digit width.
    let mut seed_entries: BTreeMap<u32, Entry> = BTreeMap::new();
    seed_entries.insert(0, Entry::Free);
    for num in 1..=sizes.first_max {
        seed_entries.insert(num, Entry::Direct(0));
    }
    // The real first-page xref also carries this section's object streams.
    // The seed must have exactly the same entry count for its length to be a
    // valid layout guess.
    // The first-page xref section covers groups 0 (part 4) and 1 (part 6), so
    // only their object streams appear there. The seed must have exactly the
    // same entry count for its length to be a valid layout guess.
    for built in objstms.iter().take(2).flatten() {
        seed_entries.insert(built.num, Entry::Direct(0));
    }
    seed_entries.insert(first_xref_num, Entry::Direct(0));
    let seed_xref = build_xref_stream(
        first_xref_num,
        &seed_entries,
        size,
        layout.root,
        &layout.trailer_extra,
        Some(guess.main_xref),
        budget,
        g,
    )?;
    let first_xref_offset = PREFIX_LEN as u64;
    let objects_start = first_xref_offset
        .checked_add(seed_xref.len() as u64)
        .ok_or_else(|| to_error(LinearizeError::OutOfRange))?;

    // Lay out every physical record in part order and build the placement
    // map used both by the xref streams and by the hint tables.
    let mut placements: BTreeMap<u32, Entry> = BTreeMap::new();
    let mut offsets: BTreeMap<u32, u64> = BTreeMap::new();
    let mut lengths: BTreeMap<u32, u64> = BTreeMap::new();
    let mut per_region: Vec<Vec<Vec<u8>>> = vec![Vec::new(); ngroups];
    let mut cursor = objects_start;
    let mut first_end = cursor;
    for group in 0..ngroups {
        for (num, obj) in &groups[group] {
            if !keep_direct(*num, obj, layout.root) {
                continue;
            }
            let bytes = super::write_one_bytes(*num, obj, budget, g)?;
            offsets.insert(*num, cursor);
            lengths.insert(*num, bytes.len() as u64);
            placements.insert(*num, Entry::Direct(cursor));
            cursor = cursor
                .checked_add(bytes.len() as u64)
                .ok_or_else(|| to_error(LinearizeError::OutOfRange))?;
            per_region[group].push(bytes);
        }
        if let Some(built) = objstms[group].as_mut() {
            built.offset = cursor;
            offsets.insert(built.num, cursor);
            lengths.insert(built.num, built.bytes.len() as u64);
            placements.insert(built.num, Entry::Direct(cursor));
            // Hint-table view: every member of one object stream shares the
            // storage unit's physical offset. Charge the whole container to
            // its first member and zero to the rest, so a section's summed
            // object lengths equal the real physical byte span (the Annex F
            // cumulative offsets then chain exactly).
            for (i, (member, index, _value_len)) in built.members.iter().enumerate() {
                placements.insert(
                    *member,
                    Entry::Compressed {
                        objstm: built.num,
                        index: *index,
                    },
                );
                offsets.insert(*member, cursor);
                lengths.insert(*member, if i == 0 { built.bytes.len() as u64 } else { 0 });
            }
            cursor = cursor
                .checked_add(built.bytes.len() as u64)
                .ok_or_else(|| to_error(LinearizeError::OutOfRange))?;
            per_region[group].push(built.bytes.clone());
        }
        if group == 1 {
            first_end = cursor;
        }
    }
    let hint_offset = cursor;

    // Hint tables: same Annex F bytes as the classic path, driven by the
    // effective offsets/lengths above.
    let (hint_buffer, shared_offset, hints) =
        build_hint_buffer(layout, &offsets, &lengths, budget, g)?;
    let mut hint_dict = vec![(key(b"S"), int(u64::from(shared_offset)))];
    for (name, offset) in [
        (b"O".as_slice(), hints.outline),
        (b"T".as_slice(), hints.thumbnails),
        (b"A".as_slice(), hints.named_dests),
        (b"E".as_slice(), hints.page_labels),
    ] {
        if let Some(off) = offset {
            hint_dict.push((key(name), int(u64::from(off))));
        }
    }
    let hint_obj_bytes = super::write_one_bytes(
        layout.hint_obj,
        &Obj::Stream {
            dict: hint_dict,
            data: Bytes::from(hint_buffer),
        },
        budget,
        g,
    )?;
    // qpdf requires /H[1] to span the whole hint stream object, not just its
    // stream data (same convention as the classic-xref writer).
    let hint_obj_len = hint_obj_bytes.len() as u64;
    let main_xref = hint_offset
        .checked_add(hint_obj_len)
        .ok_or_else(|| to_error(LinearizeError::OutOfRange))?;
    // For xref streams qpdf's `first_xref_item_offset` is the offset of the
    // xref stream object itself (the last section read via /Prev), and its /T
    // check seeks to /T, skips whitespace, and expects to land there. Point /T
    // directly at the main xref stream header.
    let t_offset = main_xref;

    // Seed the main xref stream to measure the final file length (/L). Its
    // self-entry depends only on `main_xref`, already known this pass.
    let mut all_entries: BTreeMap<u32, Entry> = BTreeMap::new();
    all_entries.insert(0, Entry::Free);
    all_entries.insert(1, Entry::Direct(super::HEADER.len() as u64));
    for (num, _) in &layout.new_objects {
        all_entries.insert(*num, placements[num]);
    }
    for built in objstms.iter().flatten() {
        all_entries.insert(built.num, Entry::Direct(built.offset));
    }
    all_entries.insert(layout.hint_obj, Entry::Direct(hint_offset));
    all_entries.insert(first_xref_num, Entry::Direct(first_xref_offset));
    all_entries.insert(main_xref_num, Entry::Direct(main_xref));
    let zero_xref = build_xref_stream(
        main_xref_num,
        &all_entries,
        size,
        layout.root,
        &layout.trailer_extra,
        None,
        budget,
        g,
    )?;
    let tail = format!("startxref\n{first_xref_offset}\n%%EOF\n");
    let file_len = main_xref
        .checked_add(zero_xref.len() as u64)
        .and_then(|v| v.checked_add(tail.len() as u64))
        .ok_or_else(|| to_error(LinearizeError::OutOfRange))?;

    // Prefix (header + lin dict, padded to a constant length).
    let lin_bytes = format!(
        "1 0 obj\n<< /Linearized 1 /L {l} /H [ {ho} {hl} ] /O {o} /E {e} /N {n} /T {t} /P 1 >>\nendobj\n",
        l = file_len,
        ho = hint_offset,
        hl = hint_obj_len,
        o = sizes.first_page_num,
        e = first_end,
        n = sizes.page_count,
        t = t_offset,
    )
    .into_bytes();
    let mut prefix = Vec::with_capacity(PREFIX_LEN);
    prefix.extend_from_slice(super::HEADER);
    prefix.extend_from_slice(&lin_bytes);
    if prefix.len() > PREFIX_LEN {
        return Err(to_error(LinearizeError::HintTable(
            "lin dict exceeds prefix",
        )));
    }
    prefix.resize(PREFIX_LEN - 1, b' ');
    prefix.push(b'\n');

    // Real first-page xref stream: part 4 + part 6 objects, their object
    // streams, object 1 and the stream itself. /Prev -> main xref stream.
    let mut first_entries: BTreeMap<u32, Entry> = BTreeMap::new();
    first_entries.insert(0, Entry::Free);
    first_entries.insert(1, Entry::Direct(super::HEADER.len() as u64));
    for (num, _) in layout.new_objects[..first_count].iter() {
        first_entries.insert(*num, placements[num]);
    }
    for built in objstms.iter().take(2).flatten() {
        first_entries.insert(built.num, Entry::Direct(built.offset));
    }
    first_entries.insert(first_xref_num, Entry::Direct(first_xref_offset));
    let first_xref = build_xref_stream(
        first_xref_num,
        &first_entries,
        size,
        layout.root,
        &layout.trailer_extra,
        Some(guess.main_xref),
        budget,
        g,
    )?;

    let mut all_entries: BTreeMap<u32, Entry> = BTreeMap::new();
    all_entries.insert(0, Entry::Free);
    all_entries.insert(1, Entry::Direct(super::HEADER.len() as u64));
    for (num, _) in &layout.new_objects {
        all_entries.insert(*num, placements[num]);
    }
    for built in objstms.iter().flatten() {
        all_entries.insert(built.num, Entry::Direct(built.offset));
    }
    all_entries.insert(layout.hint_obj, Entry::Direct(hint_offset));
    all_entries.insert(first_xref_num, Entry::Direct(first_xref_offset));
    all_entries.insert(main_xref_num, Entry::Direct(main_xref));
    let main_xref_table = build_xref_stream(
        main_xref_num,
        &all_entries,
        size,
        layout.root,
        &layout.trailer_extra,
        None,
        budget,
        g,
    )?;

    // Assemble.
    let mut out = Vec::new();
    out.extend_from_slice(&prefix);
    out.extend_from_slice(&first_xref);
    for group in &per_region {
        for part in group {
            out.extend_from_slice(part);
        }
    }
    out.extend_from_slice(&hint_obj_bytes);
    debug_assert_eq!(out.len() as u64, main_xref, "main xref offset mismatch");
    out.extend_from_slice(&main_xref_table);
    out.extend_from_slice(tail.as_bytes());
    g.charge(Resource::Bytes, out.len() as u64)?;

    let actual = Guess {
        file_len: out.len() as u64,
        first_start: t_offset,
        first_end,
        main_xref,
    };
    Ok((out, actual))
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::indexing_slicing,
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        clippy::arithmetic_side_effects,
        clippy::panic
    )]

    use super::*;

    #[test]
    fn entry_rows_have_a_fixed_width() {
        let mut entries = BTreeMap::new();
        entries.insert(0, Entry::Free);
        entries.insert(1, Entry::Direct(0x0102_0304));
        entries.insert(
            2,
            Entry::Compressed {
                objstm: 9,
                index: 3,
            },
        );
        let payload = xref_payload(&entries);
        assert_eq!(payload.len(), 3 * ROW_WIDTH);
        assert_eq!(&payload[0..8], &[0u8; 8]);
        assert_eq!(payload[8], 1);
        assert_eq!(&payload[9..13], &[1, 2, 3, 4]);
        assert_eq!(payload[16], 2);
        assert_eq!(&payload[17..21], &[0, 0, 0, 9]);
        assert_eq!(&payload[21..24], &[0, 0, 3]);
    }

    #[test]
    fn offsets_that_will_not_fit_are_typed_errors() {
        let mut entries = BTreeMap::new();
        entries.insert(1, Entry::Direct(u64::from(u32::MAX) + 1));
        let e = validate(&entries).expect_err("must refuse");
        assert_eq!(e.code(), selis_error::Code::VerifyFailed);

        let mut entries = BTreeMap::new();
        entries.insert(
            2,
            Entry::Compressed {
                objstm: MAX_FIELD + 1,
                index: 0,
            },
        );
        let e = validate(&entries).expect_err("must refuse");
        assert_eq!(e.code(), selis_error::Code::VerifyFailed);
    }

    #[test]
    fn index_merges_consecutive_runs() {
        let mut entries = BTreeMap::new();
        for n in [0, 1, 2, 3, 8, 9, 11] {
            entries.insert(n, Entry::Free);
        }
        assert_eq!(xref_index(&entries), vec![0, 4, 8, 2, 11, 1]);
    }
}
