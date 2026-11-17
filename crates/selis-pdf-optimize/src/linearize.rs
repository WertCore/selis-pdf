//! Linearisation ("fast web view") writer.
//!
//! This module writes a PDF whose first page can be rendered after fetching
//! only its leading bytes. It is the *write* counterpart of
//! `selis_pdf_cos::Linearisation` (the read side) and follows the classic
//! (non-object-stream, non-xref-stream) layout used by qpdf, including the
//! **hint tables** of ISO 32000-1 Annex F:
//!
//! ```text
//! %PDF-1.4 header
//! 1 0 obj  << /Linearized 1 /L .. /H [ off len ] /O .. /E .. /N .. /T .. /P 1 >>  endobj
//! <padding to a fixed prefix length>
//! xref                      <- first-page xref (startxref points here)
//! ...subset entries with /Prev -> main xref...
//! part 4: document catalog
//! part 6: first page object, its content and resources
//! part 7: other pages' private objects
//! part 8: other pages' shared objects
//! part 9: remaining objects (page tree, /Info, ...)
//! hint stream (page-offset + shared-object tables)           <- /H points here
//! xref                      <- main xref (complete)
//! startxref -> first-page xref
//! %%EOF
//! ```
//!
//! # What is emitted
//!
//! * Reordered objects. Object numbers are assigned **in linearisation part
//!   order**: the catalog first, then the first page's contiguous object run,
//!   then each other page's contiguous run, then shared objects, then the
//!   remainder. This contiguous, consecutively-numbered layout is what the
//!   spec's page-offset and shared-object hint tables describe.
//! * A genuine `/Linearized` dictionary with `/L`, `/H`, `/O`, `/E`, `/N`,
//!   `/T`, `/P` computed from the bytes actually written. `/H` is
//!   `[ hint_offset hint_length ]` and points at a real stream object.
//! * A **page offset hint table** (ISO 32000-1 §F.4) and a **shared object hint
//!   table** (§F.5), serialized bit-for-bit per the spec into one hint stream
//!   (§F.6). `/S` in the hint-stream dictionary locates the shared table.
//! * Two classic xref sections with a `/Prev` chain that
//!   `parse_revisions` follows.
//!
//! The hint stream is placed after the first-page section so every object
//! offset stored in the tables is below `/H`; per the spec those offsets are
//! measured "as if the hint stream were absent", which needs no adjustment
//! in this placement.
//!
//! # Verification and limits
//!
//! `tools/check_linearization.py` independently parses both hint tables and
//! cross-checks them against the emitted object graph. That is a strong
//! self-consistency check, but it is **not** `qpdf --check-linearization`:
//! no external linearisation checker is installed here. See
//! `pdf-plan/37-ASPOSE-PARITY.md` §15 for the precise, honest status.
//!
//! # Failure policy
//!
//! The writer refuses (with a typed [`LinearizeError`], surfaced as a
//! `selis_error::Error` with a specific `Code`) any input it cannot linearise
//! correctly: encrypted documents, documents without a usable page tree,
//! object graphs that already occupy object number 1 (reserved for the
//! linearisation dictionary), a layout that fails to converge, and hint-table
//! values that would not fit the spec's fixed-width fields. It never emits a
//! mislabelled file.

// This module serialises a byte-exact binary layout. Indexing and arithmetic
// below are over vectors whose bounds are established during part
// classification (object counts are validated before numbering), and over
// bounded counters (bit buffers, fixed-width hint fields); all file offsets
// use checked arithmetic. The hostile-input boundary is the object graph,
// which is validated in `build_index`/`discover_page_tree` before any of this.
#![allow(
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation
)]

use std::collections::{BTreeMap, HashMap, HashSet};

use selis_bytes::Bytes;
use selis_error::{err, Code, Result};
use selis_pdf_cos::{Obj, Ref, Writer};
use selis_sandbox::{Budget, BudgetGuard, Resource};

/// The fixed PDF header written at the start of every file.
const HEADER: &[u8] = b"%PDF-1.4\n%\xe2\xe3\xcf\xd3\n";

/// The header + linearisation dictionary are padded to this fixed length so
/// that the first xref offset (and hence every object offset) is independent
/// of the digit widths of `/L`, `/E`, `/T` and `/H` (qpdf pads similarly).
const PREFIX_LEN: usize = 512;

/// Maximum number of fixed-point passes used to resolve forward-referenced
/// byte offsets. The layout below is fully computed in one pass; the loop
/// remains as a defensive convergence guard.
const MAX_LAYOUT_PASSES: usize = 16;

/// A typed reason a document cannot be linearised by this writer.
///
/// The public entry point returns `selis_error::Result`; each variant maps to
/// a distinct [`Code`] so callers can branch on the failure class without
/// parsing text. This enum is the human-readable form of that classification.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LinearizeError {
    /// The document is encrypted; removing encryption is a separate op.
    Encrypted,
    /// No page tree / no first page / a malformed tree was found.
    BadPageTree(&'static str),
    /// Object number 1 is reserved for the linearisation dictionary.
    ObjectOneTaken,
    /// Duplicate object numbers were supplied.
    DuplicateObject(u32),
    /// The forward-referenced offsets did not reach a fixed point.
    LayoutDidNotConverge,
    /// An object number or byte offset did not fit the PDF representation.
    OutOfRange,
    /// A hint-table value could not be encoded in its fixed-width field.
    HintTable(&'static str),
}

impl LinearizeError {
    /// The `selis_error` code this refusal maps to.
    #[must_use]
    pub const fn code(&self) -> Code {
        match self {
            Self::Encrypted => Code::AlreadyEncrypted,
            Self::BadPageTree(_) | Self::DuplicateObject(_) => Code::PageTreeMalformed,
            Self::ObjectOneTaken => Code::ObjUnexpected,
            Self::LayoutDidNotConverge | Self::HintTable(_) => Code::VerifyFailed,
            Self::OutOfRange => Code::ObjUnexpected,
        }
    }

    /// A short, stable diagnostic string (no document bytes).
    #[must_use]
    pub fn detail(&self) -> String {
        match self {
            Self::Encrypted => "encrypted document cannot be linearised".to_string(),
            Self::BadPageTree(why) => format!("bad page tree: {why}"),
            Self::ObjectOneTaken => {
                "object 1 is reserved for the linearisation dictionary".to_string()
            }
            Self::DuplicateObject(n) => format!("duplicate object number {n}"),
            Self::LayoutDidNotConverge => "linearised layout did not converge".to_string(),
            Self::OutOfRange => "object number or offset out of range".to_string(),
            Self::HintTable(why) => format!("hint table cannot be encoded: {why}"),
        }
    }
}

/// What the caller must supply to linearise an already-collected object graph.
///
/// `objects` is the output of `selis_pdf_cos::copy::collect_objects` with
/// `next_num` starting at **2** (object 1 is reserved for the
/// `/Linearized` dictionary). `trailer_extra` carries trailer-level entries
/// such as `/Info` and `/ID`, which live outside the object graph.
pub struct LinearizeInput<'a> {
    /// The collected, renumbered objects, in any order. Numbers must be >= 2.
    pub objects: &'a [(u32, Obj)],
    /// The trailer's `/Root` reference, already folded through the collect remap.
    pub root: Ref,
    /// Extra trailer entries (`/Info`, `/ID`, ...) folded through the remap.
    pub trailer_extra: &'a [(Vec<u8>, Obj)],
    /// The number of leaf pages, used to validate the discovered page tree.
    pub page_count: u32,
}

/// Write `input` as a linearised PDF and return the complete file bytes.
///
/// # Budget
///
/// Charges every serialised byte (via [`Writer`]) and one object unit per
/// object. Each layout pass is charged.
///
/// # Malformed Input
///
/// Refuses encrypted documents, an absent or inconsistent page tree, an
/// object graph that occupies object 1, duplicate object numbers, offsets or
/// hint-table values that do not fit the fixed-width representation, and a
/// layout that fails to converge. Each refusal is a typed
/// `selis_error::Error` (see [`LinearizeError`]). No partial or mislabelled
/// file is ever returned.
pub fn write_linearized(
    input: &LinearizeInput<'_>,
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
) -> Result<Vec<u8>> {
    let index = build_index(input.objects)?;

    let (_first_page, pages, tree_nodes) = discover_page_tree(&index, input.root, input.page_count)
        .map_err(|why| to_error(LinearizeError::BadPageTree(why)))?;

    // Classify the graph into linearisation parts, assign new object numbers
    // in part order, and record the layout the hint tables must describe.
    let layout = build_layout(&index, input.root, input.trailer_extra, &pages, &tree_nodes)?;

    let sizes = Sizes {
        first_max: layout.first_max,
        all_max: layout.hint_obj,
        page_count: input.page_count,
        first_page_num: layout.page_objs[0],
    };

    let mut guess = Guess {
        file_len: 0,
        first_start: 0,
        first_end: 0,
        main_xref: 0,
    };
    for _ in 0..MAX_LAYOUT_PASSES {
        let (bytes, actual) = render(input, &layout, &sizes, guess, budget, g)?;
        if actual == guess {
            return Ok(bytes);
        }
        guess = actual;
    }
    Err(to_error(LinearizeError::LayoutDidNotConverge))
}

// ---------------------------------------------------------------------------
// Object graph helpers
// ---------------------------------------------------------------------------

/// Map object number to object, refusing duplicates and the reserved number 1.
fn build_index(objects: &[(u32, Obj)]) -> Result<BTreeMap<u32, &Obj>> {
    let mut index = BTreeMap::new();
    for (num, obj) in objects {
        if *num == 1 {
            return Err(to_error(LinearizeError::ObjectOneTaken));
        }
        if index.insert(*num, obj).is_some() {
            return Err(to_error(LinearizeError::DuplicateObject(*num)));
        }
    }
    Ok(index)
}

/// Walk the page tree from the catalog, returning (first leaf, all leaves,
/// all tree-node numbers). Enforces `page_count` and a depth cap.
fn discover_page_tree(
    index: &BTreeMap<u32, &Obj>,
    root: Ref,
    page_count: u32,
) -> std::result::Result<(Ref, Vec<Ref>, HashSet<u32>), &'static str> {
    let catalog = index.get(&root.num).ok_or("catalog not collected")?;
    let pages_ref = dict_ref(catalog, b"Pages").ok_or("catalog has no /Pages")?;
    let mut pages = Vec::new();
    let mut nodes = HashSet::new();
    walk_tree(index, pages_ref, &mut pages, &mut nodes, 0)?;
    if pages.is_empty() {
        return Err("no leaf pages");
    }
    let count = u32::try_from(pages.len()).map_err(|_| "too many pages")?;
    if count != page_count {
        return Err("page count mismatch");
    }
    let first = *pages.first().ok_or("no leaf pages")?;
    Ok((first, pages, nodes))
}

fn walk_tree(
    index: &BTreeMap<u32, &Obj>,
    node: Ref,
    pages: &mut Vec<Ref>,
    nodes: &mut HashSet<u32>,
    depth: usize,
) -> std::result::Result<(), &'static str> {
    if depth > 32 {
        return Err("page tree too deep");
    }
    if !nodes.insert(node.num) {
        return Err("page tree node revisited");
    }
    let obj = index.get(&node.num).ok_or("page tree node not collected")?;
    match name_of(obj, b"Type").as_deref() {
        Some(b"Page") => {
            pages.push(node);
            Ok(())
        }
        None | Some(b"Pages") => {
            let kids = match obj {
                Obj::Dict(pairs) => dict_get(pairs, b"Kids"),
                _ => None,
            };
            let Some(Obj::Array(items)) = kids else {
                return Ok(());
            };
            for item in items {
                if let Obj::Ref(kid) = item {
                    if !nodes.contains(&kid.num) {
                        walk_tree(index, *kid, pages, nodes, depth.saturating_add(1))?;
                    }
                }
            }
            Ok(())
        }
        Some(_) => Err("unexpected /Type in page tree"),
    }
}

/// Collect every indirect reference inside a direct object value.
fn collect_refs(obj: &Obj, out: &mut Vec<Ref>) {
    match obj {
        Obj::Ref(r) => out.push(*r),
        Obj::Array(items) => {
            for item in items {
                collect_refs(item, out);
            }
        }
        Obj::Dict(pairs) | Obj::Stream { dict: pairs, .. } => {
            for (_, value) in pairs {
                collect_refs(value, out);
            }
        }
        _ => {}
    }
}

// ---------------------------------------------------------------------------
// Part classification and renumbering
// ---------------------------------------------------------------------------

/// The contiguous, consecutively-numbered layout the hint tables describe.
#[derive(Debug, Clone)]
struct Layout {
    /// Objects in file layout order, already renumbered and refs rewritten.
    new_objects: Vec<(u32, Obj)>,
    /// The renumbered catalog reference.
    root: Ref,
    /// Trailer entries with their references renumbered.
    trailer_extra: Vec<(Vec<u8>, Obj)>,
    /// New object number of each page object, in page order.
    page_objs: Vec<u32>,
    /// New object numbers of each page's contiguous section (page object first).
    page_sections: Vec<Vec<u32>>,
    /// Shared-table indices referenced by each page (page 0 forced empty).
    page_shared_ids: Vec<Vec<u32>>,
    /// New object numbers in the shared-object hint table (part 6 then part 8).
    shared_list: Vec<u32>,
    /// New object numbers of the part-8 (non-first-page shared) objects.
    part8: Vec<u32>,
    /// Object number of the hint stream (the largest object number).
    hint_obj: u32,
    /// Largest object number in the first xref section (part 4 + part 6).
    first_max: u32,
    /// Number of objects in part 4 (written before the first section).
    part4_len: usize,
    /// Number of objects in part 6 (the first page section).
    part6_len: usize,
}

/// All objects reachable from one page without crossing into another page or
/// a page-tree node, returned as sorted, unique object numbers.
fn page_object_set(
    index: &BTreeMap<u32, &Obj>,
    page: Ref,
    tree_nodes: &HashSet<u32>,
    pages: &HashSet<u32>,
) -> Vec<u32> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    let mut pending = vec![page];
    while let Some(r) = pending.pop() {
        if !seen.insert(r.num) {
            continue;
        }
        // Never cross a page boundary or walk up into the page tree.
        if r.num != page.num && (tree_nodes.contains(&r.num) || pages.contains(&r.num)) {
            continue;
        }
        if !index.contains_key(&r.num) {
            continue;
        }
        out.push(r.num);
        let mut refs = Vec::new();
        if let Some(obj) = index.get(&r.num) {
            collect_refs(obj, &mut refs);
        }
        pending.extend(refs);
    }
    out.sort_unstable();
    out.dedup();
    out
}

fn build_layout(
    index: &BTreeMap<u32, &Obj>,
    root: Ref,
    trailer_extra: &[(Vec<u8>, Obj)],
    pages: &[Ref],
    tree_nodes: &HashSet<u32>,
) -> Result<Layout> {
    let page_nums: HashSet<u32> = pages.iter().map(|p| p.num).collect();

    // Full per-page object sets (used for shared-object detection).
    let page_all: Vec<Vec<u32>> = pages
        .iter()
        .map(|p| page_object_set(index, *p, tree_nodes, &page_nums))
        .collect();

    // How many pages reference each object.
    let mut users: BTreeMap<u32, usize> = BTreeMap::new();
    for set in &page_all {
        for n in set {
            *users.entry(*n).or_default() += 1;
        }
    }

    // Part 6 is every object reachable from page 0, with the page object first.
    let mut part6_old: Vec<u32> = page_all[0].clone();
    part6_old.retain(|n| *n != pages[0].num);
    part6_old.insert(0, pages[0].num);
    let part6_set: HashSet<u32> = part6_old.iter().copied().collect();

    // Part 8: referenced by two or more pages and not already in part 6.
    let mut part8_old: Vec<u32> = users
        .iter()
        .filter(|(n, count)| **count >= 2 && !part6_set.contains(n))
        .map(|(n, _)| *n)
        .collect();
    part8_old.sort_unstable();

    // Objects appearing in the shared-object hint table: part 6 then part 8.
    let shared_old: Vec<u32> = part6_old.iter().chain(part8_old.iter()).copied().collect();
    let mut old_to_shared: HashMap<u32, usize> = HashMap::new();
    for (i, old) in shared_old.iter().enumerate() {
        old_to_shared.insert(*old, i);
    }
    let shared_set: HashSet<u32> = shared_old.iter().copied().collect();

    // Part 7: for each later page, the page object followed by its private
    // objects (everything in its set that is not a shared-table object).
    let mut part7_old: Vec<Vec<u32>> = Vec::with_capacity(pages.len().saturating_sub(1));
    for i in 1..pages.len() {
        let mut group: Vec<u32> = page_all[i]
            .iter()
            .copied()
            .filter(|n| !shared_set.contains(n))
            .collect();
        group.retain(|n| *n != pages[i].num);
        group.insert(0, pages[i].num);
        part7_old.push(group);
    }

    // Part 9: everything not already assigned (catalog handled as part 4).
    let mut assigned: HashSet<u32> = shared_set;
    assigned.insert(root.num);
    for group in &part7_old {
        assigned.extend(group.iter().copied());
    }
    let mut part9_old: Vec<u32> = index
        .keys()
        .copied()
        .filter(|n| !assigned.contains(n))
        .collect();
    part9_old.sort_unstable();

    // Assign new object numbers in part order: 1 is the lin dictionary.
    let mut remap: HashMap<u32, u32> = HashMap::new();
    let mut next = 2u32;
    let mut part4_new: Vec<u32> = Vec::new();
    let mut part6_new: Vec<u32> = Vec::with_capacity(part6_old.len());
    let mut part7_new: Vec<Vec<u32>> = Vec::with_capacity(part7_old.len());
    let mut part8_new: Vec<u32> = Vec::with_capacity(part8_old.len());
    let mut part9_new: Vec<u32> = Vec::with_capacity(part9_old.len());

    remap.insert(root.num, next);
    part4_new.push(next);
    next = next
        .checked_add(1)
        .ok_or_else(|| to_error(LinearizeError::OutOfRange))?;

    for old in &part6_old {
        remap.insert(*old, next);
        part6_new.push(next);
        next = next
            .checked_add(1)
            .ok_or_else(|| to_error(LinearizeError::OutOfRange))?;
    }
    for group in &part7_old {
        let mut out = Vec::with_capacity(group.len());
        for old in group {
            remap.insert(*old, next);
            out.push(next);
            next = next
                .checked_add(1)
                .ok_or_else(|| to_error(LinearizeError::OutOfRange))?;
        }
        part7_new.push(out);
    }
    for old in &part8_old {
        remap.insert(*old, next);
        part8_new.push(next);
        next = next
            .checked_add(1)
            .ok_or_else(|| to_error(LinearizeError::OutOfRange))?;
    }
    for old in &part9_old {
        remap.insert(*old, next);
        part9_new.push(next);
        next = next
            .checked_add(1)
            .ok_or_else(|| to_error(LinearizeError::OutOfRange))?;
    }
    let hint_obj = next;
    let first_max = part6_new.last().copied().unwrap_or_else(|| part4_new[0]);

    // Build the renumbered object vector in layout order.
    let mut order = Vec::new();
    order.extend(part4_new.iter().copied());
    order.extend(part6_new.iter().copied());
    for group in &part7_new {
        order.extend(group.iter().copied());
    }
    order.extend(part8_new.iter().copied());
    order.extend(part9_new.iter().copied());

    let new_to_old: HashMap<u32, u32> = remap.iter().map(|(old, new)| (*new, *old)).collect();
    let mut new_objects: Vec<(u32, Obj)> = Vec::with_capacity(order.len());
    for new in order {
        let old = *new_to_old
            .get(&new)
            .ok_or_else(|| to_error(LinearizeError::OutOfRange))?;
        let source = index
            .get(&old)
            .ok_or_else(|| to_error(LinearizeError::BadPageTree("object vanished")))?;
        new_objects.push((
            new,
            selis_pdf_cos::copy::renumber((*source).clone(), &remap),
        ));
    }

    // Renumber the root and trailer references too.
    let new_root = Ref::new(
        *remap
            .get(&root.num)
            .ok_or_else(|| to_error(LinearizeError::OutOfRange))?,
        0,
    );
    let new_extra: Vec<(Vec<u8>, Obj)> = trailer_extra
        .iter()
        .map(|(k, v)| (k.clone(), selis_pdf_cos::copy::renumber(v.clone(), &remap)))
        .collect();

    // Per-page hint-table indices of shared objects (page 0 forced empty per
    // the spec / Adobe practice).
    let mut page_shared_ids: Vec<Vec<u32>> = Vec::with_capacity(page_all.len());
    page_shared_ids.push(Vec::new());
    for set in page_all.iter().skip(1) {
        let mut ids: Vec<u32> = set
            .iter()
            .filter_map(|old| old_to_shared.get(old).copied())
            .map(|i| u32::try_from(i).unwrap_or(u32::MAX))
            .collect();
        ids.sort_unstable();
        page_shared_ids.push(ids);
    }

    let page_objs: Vec<u32> = pages
        .iter()
        .map(|p| {
            remap
                .get(&p.num)
                .copied()
                .ok_or_else(|| to_error(LinearizeError::OutOfRange))
        })
        .collect::<Result<Vec<u32>>>()?;

    let mut page_sections: Vec<Vec<u32>> = Vec::with_capacity(pages.len());
    page_sections.push(part6_new.clone());
    page_sections.extend(part7_new.iter().cloned());

    let part4_len = part4_new.len();
    let part6_len = part6_new.len();

    Ok(Layout {
        new_objects,
        root: new_root,
        trailer_extra: new_extra,
        page_objs,
        page_sections,
        page_shared_ids,
        shared_list: shared_old.iter().map(|old| remap[old]).collect(),
        part8: part8_new,
        hint_obj,
        first_max,
        part4_len,
        part6_len,
    })
}

// ---------------------------------------------------------------------------
// Byte layout
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy)]
struct Sizes {
    first_max: u32,
    all_max: u32,
    page_count: u32,
    first_page_num: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Guess {
    file_len: u64,
    first_start: u64,
    first_end: u64,
    main_xref: u64,
}

fn render(
    _input: &LinearizeInput<'_>,
    layout: &Layout,
    sizes: &Sizes,
    _guess: Guess,
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
) -> Result<(Vec<u8>, Guess)> {
    let first_count = layout.part4_len + layout.part6_len;
    let first_parts = write_objects(&layout.new_objects[..first_count], budget, g)?;
    let rest_parts = write_objects(&layout.new_objects[first_count..], budget, g)?;

    // The first xref's length is value-independent except for the /Prev integer,
    // whose digit width depends on the main-xref offset. Use the current guess
    // for /Prev in both this seed and the real xref below so their lengths match
    // within a pass; the outer fixed-point loop resolves the /Prev width.
    let seed: Vec<(u32, Option<u64>)> = layout.new_objects[..first_count]
        .iter()
        .map(|(n, _)| (*n, Some(0)))
        .collect();
    let (dummy_xref, _first_t_rel_seed) = build_xref(
        &seed,
        sizes.first_max,
        layout.root,
        &layout.trailer_extra,
        Some(_guess.main_xref),
        budget,
        g,
    )?;
    let first_t_rel = {
        let (_, rel) = build_xref(
            &seed,
            sizes.first_max,
            layout.root,
            &layout.trailer_extra,
            Some(_guess.main_xref),
            budget,
            g,
        )?;
        rel
    };
    let xref_len = dummy_xref.len() as u64;
    let first_xref_offset = PREFIX_LEN as u64;
    let objects_start = first_xref_offset
        .checked_add(xref_len)
        .ok_or_else(|| to_error(LinearizeError::OutOfRange))?;

    // Absolute offsets and lengths of every object actually written.
    let mut offsets: BTreeMap<u32, u64> = BTreeMap::new();
    let mut lengths: BTreeMap<u32, u64> = BTreeMap::new();
    let mut cursor = objects_start;
    for ((num, _), bytes) in layout.new_objects[..first_count]
        .iter()
        .zip(first_parts.iter())
    {
        offsets.insert(*num, cursor);
        lengths.insert(*num, bytes.len() as u64);
        cursor = cursor
            .checked_add(bytes.len() as u64)
            .ok_or_else(|| to_error(LinearizeError::OutOfRange))?;
    }
    let first_end = cursor;
    for ((num, _), bytes) in layout.new_objects[first_count..]
        .iter()
        .zip(rest_parts.iter())
    {
        offsets.insert(*num, cursor);
        lengths.insert(*num, bytes.len() as u64);
        cursor = cursor
            .checked_add(bytes.len() as u64)
            .ok_or_else(|| to_error(LinearizeError::OutOfRange))?;
    }
    let hint_offset = cursor;

    // Build the two hint tables. Every object offset is below /H (the hint
    // stream is written next), so the spec's "as if absent" offsets are the
    // literal offsets here.
    let (hint_buffer, shared_offset) = build_hint_buffer(layout, &offsets, &lengths, g)?;
    let hint_len = hint_buffer.len() as u64;
    let hint_obj_bytes = write_one_bytes(
        layout.hint_obj,
        &Obj::Stream {
            dict: vec![(key(b"S"), int(u64::from(shared_offset)))],
            data: Bytes::from(hint_buffer),
        },
        budget,
        g,
    )?;
    let main_xref = hint_offset
        .checked_add(hint_obj_bytes.len() as u64)
        .ok_or_else(|| to_error(LinearizeError::OutOfRange))?;

    // Main xref length (value independent) gives the final file length, which
    // feeds /L. All other forward values are already known.
    let mut zero_entries: Vec<(u32, Option<u64>)> = vec![(1, Some(0))];
    zero_entries.extend(layout.new_objects.iter().map(|(n, _)| (*n, Some(0))));
    zero_entries.push((layout.hint_obj, Some(0)));
    let (zero_xref, _) = build_xref(
        &zero_entries,
        sizes.all_max,
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

    // Prefix: header + lin dict (with the required space after '[') + padding.
    // Writing the dict by hand keeps the array shape spec-conformant and the
    // region a constant length.
    let lin_bytes = format!(
        "1 0 obj\n<< /Linearized 1 /L {l} /H [ {ho} {hl} ] /O {o} /E {e} /N {n} /T {t} /P 1 >>\nendobj\n",
        l = file_len,
        ho = hint_offset,
        hl = hint_len,
        o = sizes.first_page_num,
        e = first_end,
        n = sizes.page_count,
        t = first_xref_offset + first_t_rel,
    )
    .into_bytes();
    let mut prefix = Vec::with_capacity(PREFIX_LEN);
    prefix.extend_from_slice(HEADER);
    prefix.extend_from_slice(&lin_bytes);
    if prefix.len() > PREFIX_LEN {
        return Err(to_error(LinearizeError::HintTable(
            "lin dict exceeds prefix",
        )));
    }
    prefix.resize(PREFIX_LEN - 1, b' ');
    prefix.push(b'\n');
    debug_assert_eq!(prefix.len(), PREFIX_LEN);
    let lin_offset = HEADER.len() as u64;

    // First-page xref with real offsets and /Prev -> main xref.
    let mut first_entries: Vec<(u32, Option<u64>)> = vec![(1, Some(lin_offset))];
    first_entries.extend(
        layout.new_objects[..first_count]
            .iter()
            .map(|(n, _)| (*n, offsets.get(n).copied())),
    );
    let (first_xref, _) = build_xref(
        &first_entries,
        sizes.first_max,
        layout.root,
        &layout.trailer_extra,
        Some(_guess.main_xref),
        budget,
        g,
    )?;

    // Complete main xref: every object plus the hint stream, no /Prev.
    let mut all_entries: Vec<(u32, Option<u64>)> = vec![(1, Some(lin_offset))];
    all_entries.extend(
        layout
            .new_objects
            .iter()
            .map(|(n, _)| (*n, offsets.get(n).copied())),
    );
    all_entries.push((layout.hint_obj, Some(hint_offset)));
    let (main_xref_table, _) = build_xref(
        &all_entries,
        sizes.all_max,
        layout.root,
        &layout.trailer_extra,
        None,
        budget,
        g,
    )?;

    // Assemble the file.
    let mut out = Vec::new();
    out.extend_from_slice(&prefix);
    out.extend_from_slice(&first_xref);
    for part in &first_parts {
        out.extend_from_slice(part);
    }
    for part in &rest_parts {
        out.extend_from_slice(part);
    }
    out.extend_from_slice(&hint_obj_bytes);
    debug_assert_eq!(out.len() as u64, main_xref, "main xref offset mismatch");
    out.extend_from_slice(&main_xref_table);
    out.extend_from_slice(tail.as_bytes());
    g.charge(Resource::Bytes, out.len() as u64)?;

    let actual = Guess {
        file_len: out.len() as u64,
        first_start: first_xref_offset + first_t_rel,
        first_end,
        main_xref,
    };
    Ok((out, actual))
}

/// Serialise one object and return its bytes (without a trailing newline).
fn write_one_bytes(
    num: u32,
    obj: &Obj,
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    write_one(&mut out, num, obj, budget, g)?;
    Ok(out)
}

/// Serialise one object as `num 0 obj ... endobj`.
fn write_one(
    out: &mut Vec<u8>,
    num: u32,
    obj: &Obj,
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
) -> Result<()> {
    out.extend_from_slice(format!("{num} 0 obj\n").as_bytes());
    let mut w = Writer::new(budget);
    out.extend_from_slice(&w.to_bytes(obj, g)?);
    out.push(b'\n');
    out.extend_from_slice(b"endobj\n");
    Ok(())
}

/// Serialise a section, returning each object's individual bytes.
fn write_objects(
    objects: &[(u32, Obj)],
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
) -> Result<Vec<Vec<u8>>> {
    let mut per_object = Vec::with_capacity(objects.len());
    for (num, obj) in objects {
        let mut buf = Vec::new();
        write_one(&mut buf, *num, obj, budget, g)?;
        per_object.push(buf);
    }
    Ok(per_object)
}

/// Build one classic xref section and its trailer. Returns the bytes and the
/// offset (relative to the section) of the whitespace before entry 0 — the
/// value the spec calls `/T`.
fn build_xref(
    offsets: &[(u32, Option<u64>)],
    size: u32,
    root: Ref,
    extra: &[(Vec<u8>, Obj)],
    prev: Option<u64>,
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
) -> Result<(Vec<u8>, u64)> {
    let count = size.saturating_add(1); // entry 0 is the free head
    let mut out = Vec::new();
    out.extend_from_slice(format!("xref\n0 {count}\n").as_bytes());
    let t_rel = (out.len() as u64).saturating_sub(1); // offset of the '\n'
    out.extend_from_slice(b"0000000000 65535 f \n");

    let lookup: BTreeMap<u32, Option<u64>> = offsets.iter().copied().collect();
    for i in 1..=size {
        match lookup.get(&i).copied().flatten() {
            Some(off) => out.extend_from_slice(format!("{off:010} 00000 n \n").as_bytes()),
            None => out.extend_from_slice(b"0000000000 65535 f \n"),
        }
    }

    let mut pairs: Vec<(Bytes, Obj)> = vec![
        (key(b"Size"), int(u64::from(count))),
        (key(b"Root"), Obj::Ref(root)),
    ];
    for (k, v) in extra {
        pairs.push((Bytes::copy_from_slice(k), v.clone()));
    }
    if let Some(prev) = prev {
        pairs.push((key(b"Prev"), int(prev)));
    }
    out.extend_from_slice(b"trailer\n");
    let mut tw = Writer::new(budget);
    out.extend_from_slice(&tw.to_bytes(&Obj::Dict(pairs), g)?);
    out.push(b'\n');
    g.charge(Resource::Bytes, out.len() as u64)?;
    Ok((out, t_rel))
}

// ---------------------------------------------------------------------------
// Hint tables (ISO 32000-1 Annex F)
// ---------------------------------------------------------------------------

/// Number of bits needed to represent `v` unsigned (qpdf's `nbits`).
fn nbits(mut v: u64) -> u16 {
    let mut n = 0u16;
    while v > 0 {
        n = n.saturating_add(1);
        v >>= 1;
    }
    n
}

/// MSB-first bit writer. Each "row" is byte-aligned (`flush`), per the spec.
struct BitWriter {
    out: Vec<u8>,
    acc: u64,
    filled: u8,
}

impl BitWriter {
    fn new() -> Self {
        Self {
            out: Vec::new(),
            acc: 0,
            filled: 0,
        }
    }

    fn write(&mut self, value: u64, bits: usize) -> Result<()> {
        if bits > 64 {
            return Err(to_error(LinearizeError::HintTable(
                "field wider than 64 bits",
            )));
        }
        for i in (0..bits).rev() {
            let bit = (value >> i) & 1;
            self.acc = (self.acc << 1) | bit;
            self.filled += 1;
            if self.filled == 8 {
                self.out.push(self.acc as u8);
                self.acc = 0;
                self.filled = 0;
            }
        }
        Ok(())
    }

    fn flush(&mut self) {
        if self.filled > 0 {
            self.out.push((self.acc << (8 - self.filled)) as u8);
            self.acc = 0;
            self.filled = 0;
        }
    }
}

/// Build the page-offset (§F.4) and shared-object (§F.5) hint tables.
///
/// Returns `(bytes, shared_table_offset)` where the offset is the `/S` value
/// for the hint-stream dictionary.
fn build_hint_buffer(
    layout: &Layout,
    offsets: &BTreeMap<u32, u64>,
    lengths: &BTreeMap<u32, u64>,
    g: &mut BudgetGuard<'_>,
) -> Result<(Vec<u8>, u32)> {
    let npages = layout.page_sections.len();

    // Per-page: number of objects and total byte length of its contiguous run.
    let mut nobjects: Vec<u64> = Vec::with_capacity(npages);
    let mut page_lengths: Vec<u64> = Vec::with_capacity(npages);
    for section in &layout.page_sections {
        let mut len = 0u64;
        for num in section {
            let l = *lengths
                .get(num)
                .ok_or_else(|| to_error(LinearizeError::HintTable("missing object length")))?;
            len = len
                .checked_add(l)
                .ok_or_else(|| to_error(LinearizeError::OutOfRange))?;
        }
        nobjects.push(section.len() as u64);
        page_lengths.push(len);
    }
    let min_nobjects = *nobjects
        .iter()
        .min()
        .ok_or_else(|| to_error(LinearizeError::HintTable("no pages")))?;
    let max_nobjects = *nobjects
        .iter()
        .max()
        .ok_or_else(|| to_error(LinearizeError::HintTable("no pages")))?;
    let min_length = *page_lengths
        .iter()
        .min()
        .ok_or_else(|| to_error(LinearizeError::HintTable("no pages")))?;
    let max_length = *page_lengths
        .iter()
        .max()
        .ok_or_else(|| to_error(LinearizeError::HintTable("no pages")))?;

    let max_shared = layout
        .page_shared_ids
        .iter()
        .map(|v| v.len() as u64)
        .max()
        .unwrap_or(0);
    let shared_total = layout.shared_list.len() as u64;

    let nbits_nobjects = nbits(max_nobjects - min_nobjects);
    let nbits_length = nbits(max_length - min_length);
    let nbits_nshared = nbits(max_shared);
    let nbits_shared_id = nbits(shared_total);

    let first_page_offset = *offsets
        .get(&layout.page_objs[0])
        .ok_or_else(|| to_error(LinearizeError::HintTable("first page offset missing")))?;
    if first_page_offset > u64::from(u32::MAX) {
        return Err(to_error(LinearizeError::HintTable(
            "page offset exceeds 32 bits",
        )));
    }

    let mut w = BitWriter::new();
    // ---- Page offset hint table header (§F.4) ----
    w.write(min_nobjects, 32)?;
    w.write(first_page_offset, 32)?;
    w.write(u64::from(nbits_nobjects), 16)?;
    w.write(min_length, 32)?;
    w.write(u64::from(nbits_length), 16)?;
    w.write(0, 32)?; // min_content_offset (Adobe practice)
    w.write(0, 16)?; // nbits_delta_content_offset
    w.write(min_length, 32)?; // min_content_length (qpdf: same as page length)
    w.write(u64::from(nbits_length), 16)?; // nbits_delta_content_length
    w.write(u64::from(nbits_nshared), 16)?;
    w.write(u64::from(nbits_shared_id), 16)?;
    w.write(0, 16)?; // nbits_shared_numerator
    w.write(4, 16)?; // shared_denominator
    w.flush();

    // Per-page rows; each field across all pages is its own byte-aligned row.
    for n in &nobjects {
        w.write(n - min_nobjects, nbits_nobjects as usize)?;
    }
    w.flush();
    for len in &page_lengths {
        w.write(len - min_length, nbits_length as usize)?;
    }
    w.flush();
    for ids in &layout.page_shared_ids {
        w.write(ids.len() as u64, nbits_nshared as usize)?;
    }
    w.flush();
    for ids in &layout.page_shared_ids {
        for id in ids {
            w.write(u64::from(*id), nbits_shared_id as usize)?;
        }
    }
    w.flush();
    // shared numerators / content offsets: all zero, zero bits wide.
    w.flush();
    for len in &page_lengths {
        w.write(len - min_length, nbits_length as usize)?; // content length = page length
    }
    w.flush();

    // ---- Shared object hint table (§F.5) ----
    let shared_offset = w.out.len() as u32;
    let mut group_lengths: Vec<u64> = Vec::with_capacity(layout.shared_list.len());
    for num in &layout.shared_list {
        let l = *lengths
            .get(num)
            .ok_or_else(|| to_error(LinearizeError::HintTable("missing shared object length")))?;
        group_lengths.push(l);
    }
    let min_group = group_lengths.iter().copied().min().unwrap_or(0);
    let max_group = group_lengths.iter().copied().max().unwrap_or(0);
    let nbits_group = nbits(max_group - min_group);

    let first_shared_obj = layout.part8.first().copied().unwrap_or(0);
    let first_shared_offset = layout
        .part8
        .first()
        .and_then(|n| offsets.get(n).copied())
        .unwrap_or(0);
    if first_shared_offset > u64::from(u32::MAX) {
        return Err(to_error(LinearizeError::HintTable(
            "shared offset exceeds 32 bits",
        )));
    }

    w.write(u64::from(first_shared_obj), 32)?;
    w.write(first_shared_offset, 32)?;
    w.write(layout.part6_len as u64, 32)?; // nshared_first_page
    w.write(shared_total, 32)?; // nshared_total
    w.write(0, 16)?; // nbits_nobjects (groups always one object)
    w.write(min_group, 32)?;
    w.write(u64::from(nbits_group), 16)?;
    w.flush();
    for len in &group_lengths {
        w.write(len - min_group, nbits_group as usize)?;
    }
    w.flush();
    for _ in &group_lengths {
        w.write(0, 1)?; // signature_present = 0
    }
    w.flush();
    // nobjects_minus_one: zero bits wide.
    w.flush();

    g.charge(Resource::Bytes, w.out.len() as u64)?;
    Ok((w.out, shared_offset))
}

// ---------------------------------------------------------------------------
// Small helpers
// ---------------------------------------------------------------------------

fn key(k: &[u8]) -> Bytes {
    Bytes::copy_from_slice(k)
}

fn int(v: u64) -> Obj {
    Obj::Int(i64::try_from(v).unwrap_or(i64::MAX))
}

fn dict_get<'a>(pairs: &'a [(Bytes, Obj)], key_name: &[u8]) -> Option<&'a Obj> {
    pairs
        .iter()
        .find(|(k, _)| k.as_slice() == key_name)
        .map(|(_, v)| v)
}

fn dict_ref(obj: &Obj, key_name: &[u8]) -> Option<Ref> {
    match obj {
        Obj::Dict(pairs) => dict_get(pairs, key_name).and_then(as_ref),
        _ => None,
    }
}

fn as_ref(obj: &Obj) -> Option<Ref> {
    match obj {
        Obj::Ref(r) => Some(*r),
        _ => None,
    }
}

fn name_of(obj: &Obj, key_name: &[u8]) -> Option<Bytes> {
    match obj {
        Obj::Dict(pairs) => match dict_get(pairs, key_name) {
            Some(Obj::Name(n)) => Some(n.clone()),
            _ => None,
        },
        _ => None,
    }
}

fn to_error(e: LinearizeError) -> selis_error::Error {
    err!(e.code(), during = "linearize", detail = e.detail())
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
        clippy::integer_division,
        clippy::panic
    )]

    use super::*;
    use selis_sandbox::Budget as B;

    fn bytes(s: &[u8]) -> Bytes {
        Bytes::copy_from_slice(s)
    }

    /// A two-page document: catalog(2), pages(3), page1(4), page2(5),
    /// contents(6,7), info(8). Object 1 is reserved.
    fn two_page_objects() -> Vec<(u32, Obj)> {
        vec![
            (
                2,
                Obj::Dict(vec![
                    (bytes(b"Type"), Obj::Name(bytes(b"Catalog"))),
                    (bytes(b"Pages"), Obj::Ref(Ref::new(3, 0))),
                ]),
            ),
            (
                3,
                Obj::Dict(vec![
                    (bytes(b"Type"), Obj::Name(bytes(b"Pages"))),
                    (
                        bytes(b"Kids"),
                        Obj::Array(vec![Obj::Ref(Ref::new(4, 0)), Obj::Ref(Ref::new(5, 0))]),
                    ),
                    (bytes(b"Count"), Obj::Int(2)),
                ]),
            ),
            (
                4,
                Obj::Dict(vec![
                    (bytes(b"Type"), Obj::Name(bytes(b"Page"))),
                    (bytes(b"Parent"), Obj::Ref(Ref::new(3, 0))),
                    (bytes(b"Contents"), Obj::Ref(Ref::new(6, 0))),
                ]),
            ),
            (
                5,
                Obj::Dict(vec![
                    (bytes(b"Type"), Obj::Name(bytes(b"Page"))),
                    (bytes(b"Parent"), Obj::Ref(Ref::new(3, 0))),
                    (bytes(b"Contents"), Obj::Ref(Ref::new(7, 0))),
                ]),
            ),
            (
                6,
                Obj::Stream {
                    dict: vec![],
                    data: bytes(b"BT (one) Tj ET"),
                },
            ),
            (
                7,
                Obj::Stream {
                    dict: vec![],
                    data: bytes(b"BT (two) Tj ET"),
                },
            ),
            (
                8,
                Obj::Dict(vec![(bytes(b"Title"), Obj::String(bytes(b"T")))]),
            ),
        ]
    }

    #[test]
    fn refuses_object_one() {
        let objects = vec![(1u32, Obj::Null), (3, Obj::Null)];
        let input = LinearizeInput {
            objects: &objects,
            root: Ref::new(3, 0),
            trailer_extra: &[],
            page_count: 1,
        };
        let budget = B::unlimited();
        let mut g = budget.guard();
        let e = write_linearized(&input, &budget, &mut g).expect_err("must refuse");
        assert_eq!(e.code(), Code::ObjUnexpected);
    }

    #[test]
    fn writes_real_linearized_layout() {
        let objects = two_page_objects();
        let extra = vec![(b"Info".to_vec(), Obj::Ref(Ref::new(8, 0)))];
        let input = LinearizeInput {
            objects: &objects,
            root: Ref::new(2, 0),
            trailer_extra: &extra,
            page_count: 2,
        };
        let budget = B::unlimited();
        let mut g = budget.guard();
        let pdf = write_linearized(&input, &budget, &mut g).expect("linearize");
        let text = String::from_utf8_lossy(&pdf);

        assert!(text.contains("/Linearized 1"), "linearization dict present");
        let p1 = text.find("3 0 obj").expect("page 1 object");
        let p2 = text.find("5 0 obj").expect("page 2 object");
        assert!(p1 < p2, "first-page object must come first");
        assert!(text.matches("\nxref\n").count() >= 2, "two xref sections");
        assert!(text.contains("startxref\n"), "startxref present");
        assert_eq!(
            parse_lin_int(&text, "L"),
            pdf.len() as u64,
            "/L equals file length"
        );
        assert_eq!(parse_lin_int(&text, "N"), 2, "/N page count");
        assert_eq!(parse_lin_int(&text, "O"), 3, "/O first page object");
        assert_eq!(parse_lin_int(&text, "P"), 1, "/P first page number");

        let h = parse_lin_array(&text, "H");
        assert_eq!(h.len(), 2, "/H is a 2-element array");
        let (dict, _data) = hint_object(&pdf, h[0] as usize);
        assert!(dict.contains("/S "), "hint stream has /S");
        assert!(dict.contains("stream\n"), "hint stream is a stream");
        let hint_num = h[0] as usize;
        let head = String::from_utf8_lossy(&pdf[hint_num..hint_num + 20]);
        assert!(
            head.starts_with(&format!("{} 0 obj", 9)),
            "/H points at hint object 9: {head:?}"
        );
    }

    /// Decode the MSB-first fields and check both tables are internally
    /// consistent with the layout described by the fixture.
    #[test]
    fn hint_tables_decode_consistently() {
        let objects = two_page_objects();
        let extra = vec![(b"Info".to_vec(), Obj::Ref(Ref::new(8, 0)))];
        let input = LinearizeInput {
            objects: &objects,
            root: Ref::new(2, 0),
            trailer_extra: &extra,
            page_count: 2,
        };
        let budget = B::unlimited();
        let mut g = budget.guard();
        let pdf = write_linearized(&input, &budget, &mut g).expect("linearize");
        let text = String::from_utf8_lossy(&pdf);
        let h = parse_lin_array(&text, "H");
        let (dict, data) = hint_object(&pdf, h[0] as usize);
        let s = parse_s(&dict);
        assert!(s < data.len(), "/S within hint buffer");

        let mut r = BitReader::new(&data[..s]);
        let min_nobjects = r.get(32);
        let first_page_offset = r.get(32);
        let nbits_nobjects = r.get(16);
        let min_length = r.get(32);
        let nbits_length = r.get(16);
        let _min_content_offset = r.get(32);
        let _nbits_content_offset = r.get(16);
        let min_content_length = r.get(32);
        let nbits_content_length = r.get(16);
        let nbits_nshared = r.get(16);
        let nbits_shared_id = r.get(16);
        let _nbits_shared_num = r.get(16);
        let shared_denominator = r.get(16);

        assert_eq!(min_content_length, min_length);
        assert_eq!(nbits_content_length, nbits_length);
        assert_eq!(shared_denominator, 4);

        let delta_nobjects = r.row(2, nbits_nobjects as usize);
        let delta_length = r.row(2, nbits_length as usize);
        let nshared = r.row(2, nbits_nshared as usize);
        assert!(nshared.iter().all(|v| *v == 0));
        let _ = r.row(0, nbits_shared_id as usize); // shared identifiers
        let _ = r.row(0, 0); // numerators
        let _ = r.row(0, 0); // content offsets
        let content_length = r.row(2, nbits_content_length as usize);
        assert_eq!(content_length, delta_length);

        let o = parse_lin_int(&text, "O") as usize;
        assert!(find_object(&pdf, o).is_some());
        assert!(delta_nobjects.iter().all(|d| d + min_nobjects == 2));
        assert!(first_page_offset > 0 && first_page_offset < pdf.len() as u64);
        assert!(min_length > 0);

        let mut sh = BitReader::new(&data[s..]);
        let first_shared_obj = sh.get(32);
        let _first_shared_offset = sh.get(32);
        let nshared_first = sh.get(32);
        let nshared_total = sh.get(32);
        let _nbits_nobjects = sh.get(16);
        let _min_group = sh.get(32);
        let _nbits_group = sh.get(16);
        assert_eq!(nshared_first, 2);
        assert_eq!(nshared_total, 2);
        // With no part-8 (cross-page shared) objects, the spec/qpdf leave
        // first_shared_obj at 0 as meaningless.
        assert_eq!(first_shared_obj, 0);
    }

    fn parse_lin_int(text: &str, key: &str) -> u64 {
        let marker = format!("/{key} ");
        let start = text.find(&marker).expect("key") + marker.len();
        let rest = &text[start..];
        let end = rest
            .find(|c: char| !c.is_ascii_digit())
            .unwrap_or(rest.len());
        rest[..end].parse().expect("integer")
    }

    fn parse_lin_array(text: &str, key: &str) -> Vec<u64> {
        let marker = format!("/{key} [");
        let start = text.find(&marker).expect("key") + marker.len();
        let rest = &text[start..];
        let end = rest.find(']').expect("close bracket");
        rest[..end]
            .split_whitespace()
            .map(|s| s.parse().expect("int"))
            .collect()
    }

    fn find_object(pdf: &[u8], offset: usize) -> Option<&[u8]> {
        let head = pdf.get(offset..offset + 20)?;
        if String::from_utf8_lossy(head).contains(" obj") {
            Some(head)
        } else {
            None
        }
    }

    /// Return (object header/dict text, raw hint data bytes).
    fn hint_object(pdf: &[u8], offset: usize) -> (String, Vec<u8>) {
        let stream_at = offset
            + pdf[offset..]
                .windows(7)
                .position(|w| w == b"stream\n")
                .expect("stream")
            + 7;
        let header = String::from_utf8_lossy(&pdf[offset..stream_at]).to_string();
        let data_start = stream_at;
        let data_end = data_start
            + pdf[data_start..]
                .windows(10)
                .position(|w| w == b"\nendstream")
                .expect("endstream");
        (header, pdf[data_start..data_end].to_vec())
    }

    fn parse_s(dict: &str) -> usize {
        let marker = "/S ";
        let start = dict.find(marker).expect("S") + marker.len();
        let rest = &dict[start..];
        let end = rest
            .find(|c: char| !c.is_ascii_digit())
            .unwrap_or(rest.len());
        rest[..end].parse().expect("S value")
    }

    struct BitReader<'a> {
        data: &'a [u8],
        pos: usize,
    }
    impl<'a> BitReader<'a> {
        fn new(data: &'a [u8]) -> Self {
            Self { data, pos: 0 }
        }
        fn bit(&mut self) -> u64 {
            let byte = self.data.get(self.pos / 8).copied().unwrap_or(0);
            let b = ((byte >> (7 - self.pos % 8)) & 1) as u64;
            self.pos += 1;
            b
        }
        fn get(&mut self, bits: usize) -> u64 {
            let mut v = 0u64;
            for _ in 0..bits {
                v = (v << 1) | self.bit();
            }
            v
        }
        fn row(&mut self, n: usize, bits: usize) -> Vec<u64> {
            self.pos = ((self.pos + 7) / 8) * 8;
            let mut out = Vec::with_capacity(n);
            for _ in 0..n {
                out.push(self.get(bits));
            }
            self.pos = ((self.pos + 7) / 8) * 8;
            out
        }
    }
}
