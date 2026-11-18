//! Compressed cross-reference path for the linearization writer.
//!
//! This is the write counterpart of `selis_pdf_cos::xref_stream`. It emits the
//! same linearized part order and Annex F hint tables as the classic path in
//! `linearize.rs`, but represents both xref sections as PDF 1.5 cross-reference
//! streams (`/Type /XRef`) and packs eligible non-stream objects into object
//! streams (`/ObjStm`).
//!
//! # Layout and numbering (qpdf --check-linearization conformant)
//!
//! qpdf's checker enforces two rules that this module follows exactly:
//!
//! 1. **No type-2 xref entry before a type-1 entry in `/Index` order.** A
//!    compressed object therefore never keeps a low object number while its
//!    `/ObjStm` container has a high one. Every object number is assigned so
//!    that each xref section lists all uncompressed objects (including the
//!    object-stream containers, the hint stream and both xref streams) first,
//!    and only then the compressed members. See [`plan_numbers`].
//! 2. **Annex F tables describe "storage units".** qpdf resolves a compressed
//!    object to the object stream that contains it before it classifies the
//!    object into a linearization part. Consequently a page section, the
//!    shared-object table and the outline table are expressed in terms of
//!    *units*: an uncompressed object is one unit, and a whole `/ObjStm` is one
//!    unit charged to its position in the part. A compressed member is not a
//!    separate unit. `build_layout` classifies the classic (all-direct) graph;
//!    this module replaces each run of compressed members with its container
//!    before feeding the result to `build_hint_buffer`.
//!
//! Object numbers and part lengths otherwise stay in the classic part order,
//! so `/O` (first page object), `/N`, `/E` and the optional hint tables keep
//! the same meaning as in the classic-xref writer. Only the numbers *after*
//! the storage-unit region are reserved for compressed members, which places
//! them behind every type-1 entry in both xref streams.
//!
//! ```text
//! header + /Linearized dict (padded)          object 1
//! first-page XRef stream  <- startxref
//! part 4 (catalog, direct) + its /ObjStm
//! part 6 (first page) + its /ObjStm
//! part 7 (each later page) + its /ObjStm
//! part 8 (shared), part 9 (remainder): same pattern
//! hint stream
//! main XRef stream (all objects, /Prev-free)
//! startxref -> first-page XRef stream, %%EOF
//!
//! number order:  2 ..= U            storage units (uncompressed + containers)
//!                U+1 ..= U+3        hint + first/main xref streams
//!                U+4 ..= F          compressed members of the FIRST section
//!                F+1 ..= M          compressed members of the MAIN section
//! ```
//!
//! The first xref stream's `/Index` is `[1 1, 2 (F-1)]` (every object up to the
//! last first-section compressed member); the main one is `[0 M+1]` (every
//! object). Both list type-1 entries before type-2 entries, as required.

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

use std::collections::{BTreeMap, HashMap, HashSet};

use selis_bytes::Bytes;
use selis_error::Result;
use selis_pdf_cos::Obj;
use selis_sandbox::{Budget, BudgetGuard, Resource};

use super::{
    build_hint_buffer, int, key, name_of, outline_object_set, to_error, Guess, Layout,
    LinearizeError, LinearizeInput, Ref, Sizes, PREFIX_LEN,
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
    /// Object number of the `/ObjStm` (final numbering).
    num: u32,
    /// Full `N 0 obj ... endobj` bytes.
    bytes: Vec<u8>,
    /// `(member object number, index, value length)` per member, all final.
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
fn keep_direct(num: u32, obj: &Obj, root: Ref) -> bool {
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

/// Object numbers of the outline tree. qpdf checks the F.6 table against the
/// outline object group, so those objects stay uncompressed (otherwise qpdf
/// would count their container instead of the outline items).
fn outline_objects(layout: &Layout) -> HashSet<u32> {
    outline_object_set(&layout.new_objects, layout.root)
        .map(|(_, objs)| objs.into_iter().collect())
        .unwrap_or_default()
}

/// The numbering plan: final object numbers and the final, hint-table-ready
/// layout. Produced once per file; the fixed-point loop in `render` only
/// resolves byte offsets, which never change these numbers.
struct NumberingPlan {
    /// Classic number -> final number (all objects: units and compressed).
    final_of: BTreeMap<u32, u32>,
    /// Same mapping as a HashMap for `renumber`.
    remap: HashMap<u32, u32>,
    /// Region -> final number of its `/ObjStm` container (if any).
    objstm_final: Vec<Option<u32>>,
    /// Classic number -> region.
    region_of: BTreeMap<u32, usize>,
    /// Compressible members per region (classic numbers).
    members: Vec<Vec<u32>>,
    /// Final number of the first-page xref stream.
    first_xref_num: u32,
    /// Final number of the main xref stream.
    main_xref_num: u32,
    /// Last object number in the FIRST xref section (all compressed members of
    /// parts 4 and 6 included). The first xref `/Index` covers `1..=first_end_num`.
    first_end_num: u32,
    /// Trailer `/Size` = highest object number + 1.
    size: u32,
    /// A layout whose tables and object graph use final storage-unit numbers.
    hint_layout: Layout,
}

/// Build the final numbering and the storage-unit view used by the hint tables.
fn plan_numbers(layout: &Layout) -> Result<NumberingPlan> {
    let regions = assign_regions(layout)?;
    let ngroups = layout.page_sections.len().saturating_add(3);
    let protected = outline_objects(layout);

    // Split each region into storage units (direct objects + one container)
    // and the compressible members packed into that container.
    let mut members: Vec<Vec<u32>> = vec![Vec::new(); ngroups];
    let mut region_of = BTreeMap::new();
    for (i, (num, obj)) in layout.new_objects.iter().enumerate() {
        let region = *regions
            .get(i)
            .ok_or_else(|| to_error(LinearizeError::UnsupportedLayout("region index")))?;
        region_of.insert(*num, region);
        if !keep_direct(*num, obj, layout.root) && !protected.contains(num) {
            members[region].push(*num);
        }
    }

    // Assign final numbers. Storage units first (in part order), then the
    // hint/xref streams, then compressed members grouped by xref section so
    // every type-2 entry follows every type-1 entry in `/Index` order.
    let mut final_of = BTreeMap::new();
    let mut objstm_final: Vec<Option<u32>> = vec![None; ngroups];
    let mut next = 2u32;
    for (i, (num, _)) in layout.new_objects.iter().enumerate() {
        if members.get(regions[i]).is_some_and(|m| m.contains(num)) {
            continue;
        }
        final_of.insert(*num, next);
        next = next
            .checked_add(1)
            .ok_or_else(|| to_error(LinearizeError::OutOfRange))?;
        let region = regions[i];
        if objstm_final[region].is_none() && members.get(region).is_some_and(|m| !m.is_empty()) {
            objstm_final[region] = Some(next);
            next = next
                .checked_add(1)
                .ok_or_else(|| to_error(LinearizeError::OutOfRange))?;
        }
    }
    // A region whose every object is compressible has no unit to anchor its
    // container while walking the units above; give that container a number
    // here so its members still resolve.
    for region in 0..ngroups {
        if objstm_final[region].is_none() && !members[region].is_empty() {
            objstm_final[region] = Some(next);
            next = next
                .checked_add(1)
                .ok_or_else(|| to_error(LinearizeError::OutOfRange))?;
        }
    }
    let hint_obj = next;
    next = next
        .checked_add(1)
        .ok_or_else(|| to_error(LinearizeError::OutOfRange))?;
    let first_xref_num = next;
    next = next
        .checked_add(1)
        .ok_or_else(|| to_error(LinearizeError::OutOfRange))?;
    let main_xref_num = next;
    next = next
        .checked_add(1)
        .ok_or_else(|| to_error(LinearizeError::OutOfRange))?;
    // First-section compressed members (parts 4 and 6) come before main ones.
    for region in 0..ngroups.min(2) {
        for member in &members[region] {
            final_of.insert(*member, next);
            next = next
                .checked_add(1)
                .ok_or_else(|| to_error(LinearizeError::OutOfRange))?;
        }
    }
    let first_end_num = next - 1;
    for region in 2..ngroups {
        for member in &members[region] {
            final_of.insert(*member, next);
            next = next
                .checked_add(1)
                .ok_or_else(|| to_error(LinearizeError::OutOfRange))?;
        }
    }
    let size = next;
    if main_xref_num > MAX_FIELD || size > MAX_FIELD.saturating_add(1) {
        return Err(to_error(LinearizeError::UnsupportedLayout(
            "too many objects for compressed mode",
        )));
    }

    // Resolve a classic object number to its final *unit* number. A
    // compressed member maps to its object stream, everything else to itself.
    let remap: HashMap<u32, u32> = final_of.iter().map(|(k, v)| (*k, *v)).collect();
    let unit_final = |classic: u32| -> u32 {
        if let Some(region) = region_of.get(&classic) {
            if members.get(*region).is_some_and(|m| m.contains(&classic)) {
                if let Some(Some(num)) = objstm_final.get(*region) {
                    return *num;
                }
            }
        }
        remap.get(&classic).copied().unwrap_or(classic)
    };

    // Remap the object graph: object streams become unit placeholders (they
    // carry no references of their own), compressed members are omitted and
    // every reference to one is rewritten to its container.
    let mut new_objects: Vec<(u32, Obj)> = Vec::new();
    for (i, (num, obj)) in layout.new_objects.iter().enumerate() {
        let region = regions[i];
        if members[region].contains(num) {
            continue;
        }
        let final_num = *remap
            .get(num)
            .ok_or_else(|| to_error(LinearizeError::OutOfRange))?;
        new_objects.push((
            final_num,
            selis_pdf_cos::copy::renumber((*obj).clone(), &remap),
        ));
    }
    for num in objstm_final.iter().flatten() {
        // A placeholder with no references: F.6/F.4 walkers see the
        // container exactly where qpdf's `resolveCompressedObject` does.
        new_objects.push((*num, Obj::Dict(Vec::new())));
    }

    // Remap every structure consumed by the hint-table builder to units.
    let page_objs: Vec<u32> = layout.page_objs.iter().map(|n| unit_final(*n)).collect();
    let mut page_sections: Vec<Vec<u32>> = layout
        .page_sections
        .iter()
        .map(|s| dedup(s.iter().map(|n| unit_final(*n)).collect()))
        .collect();
    let shared_list: Vec<u32> = dedup(layout.shared_list.iter().map(|n| unit_final(*n)).collect());
    let part8: Vec<u32> = dedup(layout.part8.iter().map(|n| unit_final(*n)).collect());
    let page_shared_ids: Vec<Vec<u32>> = layout
        .page_shared_ids
        .iter()
        .map(|ids| {
            dedup(
                ids.iter()
                    .filter_map(|idx| layout.shared_list.get(*idx as usize))
                    .map(|classic| {
                        let unit = unit_final(*classic);
                        shared_list
                            .iter()
                            .position(|n| *n == unit)
                            .map(|i| i as u32)
                            .unwrap_or(u32::MAX)
                    })
                    .filter(|i| *i != u32::MAX)
                    .collect(),
            )
        })
        .collect();
    let part6_len = page_sections.first().map(Vec::len).unwrap_or(0);
    let root_final = unit_final(layout.root.num);
    let trailer_extra: Vec<(Vec<u8>, Obj)> = layout
        .trailer_extra
        .iter()
        .map(|(k, v)| (k.clone(), selis_pdf_cos::copy::renumber(v.clone(), &remap)))
        .collect();
    let mut optional = layout.optional.clone();
    if let Some(fields) = optional.forms.as_mut() {
        for field in fields.iter_mut() {
            // The F.10 table names the *field object itself* (which may be a
            // compressed member), not its storage-unit container. Keep its
            // final member number; only the Annex F tables qpdf checks are
            // expressed in units.
            field.obj = remap.get(&field.obj).copied().unwrap_or(field.obj);
        }
    }
    // page_sections must always begin with the first page object.
    if let Some(first) = page_sections.first_mut() {
        if let Some(page) = page_objs.first() {
            if !first.contains(page) {
                first.insert(0, *page);
            }
        }
    }

    let hint_layout = Layout {
        new_objects,
        root: Ref::new(root_final, 0),
        trailer_extra,
        page_objs,
        page_sections,
        page_shared_ids,
        shared_list,
        part8,
        hint_obj,
        first_max: first_end_num,
        part4_len: layout.part4_len,
        part6_len,
        optional,
    };
    Ok(NumberingPlan {
        final_of,
        remap,
        objstm_final,
        region_of,
        members,
        first_xref_num,
        main_xref_num,
        first_end_num,
        size,
        hint_layout,
    })
}

fn dedup(mut values: Vec<u32>) -> Vec<u32> {
    values.sort_unstable();
    values.dedup();
    values
}

/// Serialize one region's compressible objects into an `/ObjStm`.
/// `members` are already in final numbering.
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

/// Build one xref-stream object and return its bytes.
#[allow(clippy::too_many_arguments)]
fn build_xref_stream(
    num: u32,
    entries: &BTreeMap<u32, Entry>,
    size: u32,
    root: Ref,
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
///
/// # Budget
///
/// Charges every serialized object, object stream and xref stream through the
/// caller's [`BudgetGuard`], including the hint-buffer bytes. The caller's
/// fixed-point loop runs at most `MAX_LAYOUT_PASSES` times.
///
/// # Malformed Input
///
/// Object numbers and offsets that exceed the `/W [1 4 3]` fields, a region
/// that cannot be laid out, or a layout whose forward-referenced offsets do
/// not converge are refused with a typed [`LinearizeError`]; no partial or
/// mislabelled file is returned.
pub(super) fn render(
    _input: &LinearizeInput<'_>,
    layout: &Layout,
    _sizes: &Sizes,
    guess: Guess,
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
) -> Result<(Vec<u8>, Guess)> {
    let plan = plan_numbers(layout)?;
    let h = &plan.hint_layout;
    let ngroups = plan.objstm_final.len();

    // Build the object streams with final member numbers.
    let mut objstms: Vec<Option<BuiltObjStm>> = (0..ngroups).map(|_| None).collect();
    for region in 0..ngroups {
        let Some(num) = plan.objstm_final[region] else {
            continue;
        };
        let mut members: Vec<(u32, Obj)> = Vec::new();
        for classic in &plan.members[region] {
            // Members live in the original (classic) graph under their
            // classic number; only their final number changes.
            let obj = layout
                .new_objects
                .iter()
                .find(|(n, _)| *n == *classic)
                .map(|(_, o)| o.clone())
                .ok_or_else(|| to_error(LinearizeError::OutOfRange))?;
            let final_num = *plan
                .final_of
                .get(classic)
                .ok_or_else(|| to_error(LinearizeError::OutOfRange))?;
            // References inside the member must use the final numbering too,
            // otherwise they can point at the hint/xref streams instead of the
            // objects they used to reference.
            let obj = selis_pdf_cos::copy::renumber(obj, &plan.remap);
            members.push((final_num, obj));
        }
        objstms[region] = Some(build_objstm(num, &members, budget, g)?);
    }

    // Seed the first-page xref stream to measure its fixed length. Its /Prev
    // is the only value-dependent integer; the fixed-point loop resolves it.
    let mut seed_entries: BTreeMap<u32, Entry> = BTreeMap::new();
    for num in 1..=plan.first_end_num {
        seed_entries.insert(num, Entry::Direct(0));
    }
    seed_entries.insert(plan.first_xref_num, Entry::Direct(0));
    let seed_xref = build_xref_stream(
        plan.first_xref_num,
        &seed_entries,
        plan.size,
        h.root,
        &h.trailer_extra,
        Some(guess.main_xref),
        budget,
        g,
    )?;
    let first_xref_offset = PREFIX_LEN as u64;
    let objects_start = first_xref_offset
        .checked_add(seed_xref.len() as u64)
        .ok_or_else(|| to_error(LinearizeError::OutOfRange))?;

    // Lay out every physical record in part order and build the placement map.
    let mut placements: BTreeMap<u32, Entry> = BTreeMap::new();
    // Entry 0 is free; qpdf needs it to compute `first_xref_item_offset` for
    // the /T check. Without it the checker reports "computed = 0".
    placements.insert(0, Entry::Free);
    // Object 1 is the /Linearized dictionary written in the prefix.
    placements.insert(1, Entry::Direct(super::HEADER.len() as u64));
    let mut offsets: BTreeMap<u32, u64> = BTreeMap::new();
    let mut lengths: BTreeMap<u32, u64> = BTreeMap::new();
    let mut per_region: Vec<Vec<Vec<u8>>> = vec![Vec::new(); ngroups];
    let mut cursor = objects_start;
    let mut first_end = cursor;
    for region in 0..ngroups {
        for (num, obj) in &h.new_objects {
            // Unit objects belonging to this region (containers are Dict
            // placeholders handled below; hint/xref streams are not in the
            // graph).
            let classic = plan
                .final_of
                .iter()
                .find(|(_, final_num)| **final_num == *num)
                .map(|(classic, _)| *classic);
            let Some(classic) = classic else { continue };
            if plan.region_of.get(&classic) != Some(&region) {
                continue;
            }
            let bytes = super::write_one_bytes(*num, obj, budget, g)?;
            offsets.insert(*num, cursor);
            lengths.insert(*num, bytes.len() as u64);
            placements.insert(*num, Entry::Direct(cursor));
            cursor = cursor
                .checked_add(bytes.len() as u64)
                .ok_or_else(|| to_error(LinearizeError::OutOfRange))?;
            per_region[region].push(bytes);
        }
        if let Some(built) = objstms[region].as_mut() {
            built.offset = cursor;
            let container = built.num;
            offsets.insert(container, cursor);
            lengths.insert(container, built.bytes.len() as u64);
            placements.insert(container, Entry::Direct(cursor));
            // qpdf's "storage unit" view: a compressed member has no byte
            // range of its own. Charge the whole container to its first
            // member and zero to the rest so a section's summed object
            // lengths equal the real physical byte span.
            for (i, (member, index, _)) in built.members.iter().enumerate() {
                placements.insert(
                    *member,
                    Entry::Compressed {
                        objstm: container,
                        index: *index,
                    },
                );
                offsets.insert(*member, cursor);
                lengths.insert(*member, if i == 0 { built.bytes.len() as u64 } else { 0 });
            }
            cursor = cursor
                .checked_add(built.bytes.len() as u64)
                .ok_or_else(|| to_error(LinearizeError::OutOfRange))?;
            per_region[region].push(built.bytes.clone());
        }
        if region == 1 {
            first_end = cursor;
        }
    }
    let hint_offset = cursor;

    // Annex F hint tables from the storage-unit view.
    let (hint_buffer, shared_offset, hints) = build_hint_buffer(h, &offsets, &lengths, budget, g)?;
    let mut hint_dict = vec![(key(b"S"), int(u64::from(shared_offset)))];
    for (name, offset) in [
        (b"O".as_slice(), hints.outline),
        (b"T".as_slice(), hints.thumbnails),
        (b"A".as_slice(), hints.named_dests),
        (b"E".as_slice(), hints.page_labels),
        (b"F".as_slice(), hints.forms),
    ] {
        if let Some(off) = offset {
            hint_dict.push((key(name), int(u64::from(off))));
        }
    }
    let hint_obj_bytes = super::write_one_bytes(
        h.hint_obj,
        &Obj::Stream {
            dict: hint_dict,
            data: Bytes::from(hint_buffer),
        },
        budget,
        g,
    )?;
    // qpdf requires /H[1] to span the whole hint stream object.
    let hint_obj_len = hint_obj_bytes.len() as u64;
    let main_xref = hint_offset
        .checked_add(hint_obj_len)
        .ok_or_else(|| to_error(LinearizeError::OutOfRange))?;
    // For xref streams qpdf's `first_xref_item_offset` is the offset of the
    // xref stream object that holds entry 0 (the main xref), so /T points at
    // its header.
    let t_offset = main_xref;
    placements.insert(h.hint_obj, Entry::Direct(hint_offset));
    placements.insert(plan.first_xref_num, Entry::Direct(first_xref_offset));
    placements.insert(plan.main_xref_num, Entry::Direct(main_xref));

    // Seed the main xref stream to measure the final file length (/L).
    let zero_xref = build_xref_stream(
        plan.main_xref_num,
        &placements,
        plan.size,
        h.root,
        &h.trailer_extra,
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
        o = h.page_objs[0],
        e = first_end,
        n = h.page_objs.len(),
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

    // Real first-page xref stream: objects 1..=first_end_num, with the
    // compressed members of parts 4 and 6 last. /Prev -> main xref.
    let mut first_entries: BTreeMap<u32, Entry> = BTreeMap::new();
    for num in 1..=plan.first_end_num {
        if let Some(entry) = placements.get(&num) {
            first_entries.insert(num, *entry);
        }
    }
    let first_xref = build_xref_stream(
        plan.first_xref_num,
        &first_entries,
        plan.size,
        h.root,
        &h.trailer_extra,
        Some(guess.main_xref),
        budget,
        g,
    )?;

    let main_xref_table = build_xref_stream(
        plan.main_xref_num,
        &placements,
        plan.size,
        h.root,
        &h.trailer_extra,
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
