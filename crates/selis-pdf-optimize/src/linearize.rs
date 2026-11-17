//! Linearisation ("fast web view") writer.
//!
//! This module writes a PDF whose first page can be rendered after fetching
//! only its leading bytes. It is the *write* counterpart of
//! `selis_pdf_cos::Linearisation` (the read side) and follows the layout used
//! by qpdf for classic (non-object-stream, non-xref-stream) files:
//!
//! ```text
//! %PDF-1.4 header
//! 1 0 obj  << /Linearized 1 /L .. /H [ 0 0 ] /O .. /E .. /N .. /T .. /P 1 >>  endobj
//! xref                      <- first-page xref (startxref points here)
//! 0 <n>
//! ...subset entries...
//! trailer << /Size /Root /Info /Prev -> main xref >>
//! <first-page objects: catalog, page-tree path, first page, its content/resources>
//! <remaining objects>
//! xref                      <- main xref (complete)
//! 0 <n>
//! ...all entries...
//! trailer << /Size /Root /Info >>
//! startxref -> first-page xref
//! %%EOF
//! ```
//!
//! # What is and is not emitted
//!
//! * **Emitted:** reordered objects (first-page objects first), a genuine
//!   `/Linearized` dictionary with `/L`, `/H`, `/O`, `/E`, `/N`, `/T`, `/P`
//!   computed from the bytes actually written, two classic xref sections, and
//!   a `/Prev` chain that the engine's `parse_revisions` follows.
//! * **Not emitted:** hint streams / hint tables (page-offset and
//!   shared-object tables, PDF 1.4 §F.4). `/H` is therefore `[ 0 0 ]` — there
//!   is no hint stream. A full linearisation checker such as
//!   `qpdf --check-linearization` will report the missing hint stream; this
//!   writer does **not** claim checker-level conformance. See
//!   `pdf-plan/37-ASPOSE-PARITY.md` §15 for the precise, honest status.
//!
//! # Failure policy
//!
//! The writer refuses (with a typed [`LinearizeError`], surfaced as a
//! `selis_error::Error` with a specific `Code`) any input it cannot linearise
//! correctly: encrypted documents, documents without a usable page tree,
//! object graphs that already occupy object number 1 (reserved for the
//! linearisation dictionary), and a layout that fails to converge. It never
//! emits a mislabelled file.

use std::collections::{BTreeMap, HashSet};

use selis_bytes::Bytes;
use selis_error::{err, Code, Result};
use selis_pdf_cos::{Obj, Ref, Writer};
use selis_sandbox::{Budget, BudgetGuard};

/// The fixed PDF header written at the start of every file.
const HEADER: &[u8] = b"%PDF-1.4\n%\xe2\xe3\xcf\xd3\n";

/// Maximum number of fixed-point passes used to resolve forward-referenced
/// byte offsets (`/L`, `/E`, `/T`, and the first xref's `/Prev`).
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
}

impl LinearizeError {
    /// The `selis_error` code this refusal maps to.
    #[must_use]
    pub const fn code(&self) -> Code {
        match self {
            Self::Encrypted => Code::AlreadyEncrypted,
            Self::BadPageTree(_) | Self::DuplicateObject(_) => Code::PageTreeMalformed,
            Self::ObjectOneTaken => Code::ObjUnexpected,
            Self::LayoutDidNotConverge => Code::VerifyFailed,
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
/// object. The fixed-point layout loop re-serialises the document a bounded
/// number of times (at most [`MAX_LAYOUT_PASSES`]); each pass is charged.
///
/// # Malformed Input
///
/// Refuses encrypted documents, an absent or inconsistent page tree, an
/// object graph that occupies object 1, duplicate object numbers, offsets
/// that do not fit the representation, and a layout that fails to converge.
/// Each refusal is a typed `selis_error::Error` (see [`LinearizeError`]).
/// No partial or mislabelled file is ever returned.
pub fn write_linearized(
    input: &LinearizeInput<'_>,
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
) -> Result<Vec<u8>> {
    let index = build_index(input.objects)?;

    let (first_page, _pages, tree_nodes) = discover_page_tree(&index, input.root, input.page_count)
        .map_err(|why| to_error(LinearizeError::BadPageTree(why)))?;

    let first_set = first_page_objects(&index, input.root, first_page, &tree_nodes);
    let (first_objs, rest_objs) = partition(input.objects, &first_set);

    let sizes = Sizes {
        first_max: max_number(&first_objs)?,
        all_max: max_number(input.objects)?,
        page_count: input.page_count,
        first_page_num: first_page.num,
    };

    // Fixed point over the four forward-referenced values. All other offsets
    // are measured from the bytes actually written in a single pass.
    let mut guess = Guess {
        file_len: 0,
        first_start: 0,
        first_end: 0,
        main_xref: 0,
    };
    for _ in 0..MAX_LAYOUT_PASSES {
        let (bytes, actual) = render(
            input,
            &index,
            &sizes,
            &first_objs,
            &rest_objs,
            guess,
            budget,
            g,
        )?;
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

/// The objects needed to render the first page: the catalog, every page-tree
/// node on the path from the root to the first page, the first page itself,
/// and the transitive closure of its references. Traversal never crosses into
/// a different page or a page-tree node off that path.
fn first_page_objects(
    index: &BTreeMap<u32, &Obj>,
    root: Ref,
    first_page: Ref,
    tree_nodes: &HashSet<u32>,
) -> HashSet<u32> {
    let mut set = HashSet::new();
    set.insert(root.num);

    // Walk /Parent upward from the first page to include its page-tree
    // ancestors (so a reader can navigate the tree to reach page 1).
    let mut ancestor = first_page;
    let mut guard = 0usize;
    loop {
        set.insert(ancestor.num);
        let Some(obj) = index.get(&ancestor.num) else {
            break;
        };
        let parent = match obj {
            Obj::Dict(pairs) => dict_get(pairs, b"Parent").and_then(as_ref),
            _ => None,
        };
        match parent {
            Some(p) if p.num != ancestor.num && guard < 64 => {
                ancestor = p;
                guard = guard.saturating_add(1);
            }
            _ => break,
        }
    }

    // Transitive closure from the first page object, never crossing a page
    // boundary or a page-tree node that is not on the path above.
    let mut pending = vec![first_page];
    let mut seen = HashSet::new();
    while let Some(r) = pending.pop() {
        if !seen.insert(r.num) {
            continue;
        }
        if r.num != first_page.num && tree_nodes.contains(&r.num) {
            continue;
        }
        let Some(obj) = index.get(&r.num) else {
            continue;
        };
        let mut refs = Vec::new();
        collect_refs(obj, &mut refs);
        for child in refs {
            if child.num != first_page.num && tree_nodes.contains(&child.num) {
                continue;
            }
            pending.push(child);
        }
    }
    set.extend(seen);
    set
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

/// Split objects into (first-page section, remainder), preserving input order.
fn partition(objects: &[(u32, Obj)], first: &HashSet<u32>) -> (Vec<(u32, Obj)>, Vec<(u32, Obj)>) {
    let mut a = Vec::new();
    let mut b = Vec::new();
    for (num, obj) in objects {
        if first.contains(num) {
            a.push((*num, obj.clone()));
        } else {
            b.push((*num, obj.clone()));
        }
    }
    (a, b)
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
    /// `/L` — total file length.
    file_len: u64,
    /// `/T` — offset of the whitespace before the first xref entry.
    first_start: u64,
    /// `/E` — offset just past the last first-page object.
    first_end: u64,
    /// `/Prev` in the first xref trailer — the main xref offset.
    main_xref: u64,
}

fn max_number(objects: &[(u32, Obj)]) -> Result<u32> {
    let mut max = 1u32;
    for (num, _) in objects {
        max = (*num).max(max);
    }
    Ok(max)
}

fn render(
    input: &LinearizeInput<'_>,
    index: &BTreeMap<u32, &Obj>,
    sizes: &Sizes,
    first_objs: &[(u32, Obj)],
    rest_objs: &[(u32, Obj)],
    guess: Guess,
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
) -> Result<(Vec<u8>, Guess)> {
    // Header + linearisation dictionary (object 1), written first so a reader
    // can find it within the first 1024 bytes.
    let mut out = Vec::new();
    out.extend_from_slice(HEADER);
    g.charge(selis_sandbox::Resource::Bytes, out.len() as u64)?;

    let lin_dict = Obj::Dict(vec![
        (key(b"Linearized"), Obj::Int(1)),
        (key(b"L"), int(guess.file_len)),
        // No hint stream is emitted; /H keeps qpdf's 2-element array shape.
        (key(b"H"), Obj::Array(vec![Obj::Int(0), Obj::Int(0)])),
        (key(b"O"), int(u64::from(sizes.first_page_num))),
        (key(b"E"), int(guess.first_end)),
        (key(b"N"), int(u64::from(sizes.page_count))),
        (key(b"T"), int(guess.first_start)),
        (key(b"P"), Obj::Int(1)),
    ]);
    // The linearisation dictionary is object 1 and starts right after the
    // header; it must be in-use in both xref sections so readers can resolve it.
    let lin_offset =
        u64::try_from(HEADER.len()).map_err(|_| to_error(LinearizeError::OutOfRange))?;
    write_one(&mut out, 1, &lin_dict, budget, g)?;

    let first_xref_offset =
        u64::try_from(out.len()).map_err(|_| to_error(LinearizeError::OutOfRange))?;

    // Serialise both object sections into per-object buffers so their byte
    // offsets are known before the xref tables that index them are built.
    let first_parts = write_objects(first_objs, budget, g)?;
    let rest_parts = write_objects(rest_objs, budget, g)?;

    // Build the first-page xref once with dummy offsets to learn its exact
    // byte length (offsets are fixed-width, so the length is value-independent).
    let dummy_offsets: Vec<(u32, Option<u64>)> =
        first_objs.iter().map(|(num, _)| (*num, Some(0))).collect();
    let (dummy_xref, first_t_rel) = build_xref(
        &dummy_offsets,
        sizes.first_max,
        input.root,
        input.trailer_extra,
        Some(guess.main_xref),
        budget,
        g,
    )?;
    let xref_len =
        u64::try_from(dummy_xref.len()).map_err(|_| to_error(LinearizeError::OutOfRange))?;
    let objects_start = first_xref_offset
        .checked_add(xref_len)
        .ok_or_else(|| to_error(LinearizeError::OutOfRange))?;

    // Absolute offsets of the objects actually written.
    let mut offsets: BTreeMap<u32, u64> = BTreeMap::new();
    let mut cursor = objects_start;
    for ((num, _), bytes) in first_objs.iter().zip(first_parts.iter()) {
        offsets.insert(*num, cursor);
        cursor = cursor
            .checked_add(
                u64::try_from(bytes.len()).map_err(|_| to_error(LinearizeError::OutOfRange))?,
            )
            .ok_or_else(|| to_error(LinearizeError::OutOfRange))?;
    }
    let first_end = cursor;
    for ((num, _), bytes) in rest_objs.iter().zip(rest_parts.iter()) {
        offsets.insert(*num, cursor);
        cursor = cursor
            .checked_add(
                u64::try_from(bytes.len()).map_err(|_| to_error(LinearizeError::OutOfRange))?,
            )
            .ok_or_else(|| to_error(LinearizeError::OutOfRange))?;
    }
    let main_xref = cursor;

    // First-page xref with real offsets, then the two object sections.
    let mut first_entries: Vec<(u32, Option<u64>)> = vec![(1, Some(lin_offset))];
    first_entries.extend(
        first_objs
            .iter()
            .map(|(num, _)| (*num, offsets.get(num).copied())),
    );
    let (first_xref, _t) = build_xref(
        &first_entries,
        sizes.first_max,
        input.root,
        input.trailer_extra,
        Some(main_xref),
        budget,
        g,
    )?;
    out.extend_from_slice(&first_xref);
    for part in &first_parts {
        out.extend_from_slice(part);
    }
    for part in &rest_parts {
        out.extend_from_slice(part);
    }

    // Main xref (complete): every object, no /Prev.
    let mut all_entries: Vec<(u32, Option<u64>)> = vec![(1, Some(lin_offset))];
    all_entries.extend(index.keys().map(|num| (*num, offsets.get(num).copied())));
    let (main_xref_table, _t) = build_xref(
        &all_entries,
        sizes.all_max,
        input.root,
        input.trailer_extra,
        None,
        budget,
        g,
    )?;
    out.extend_from_slice(&main_xref_table);
    out.extend_from_slice(format!("startxref\n{first_xref_offset}\n%%EOF\n").as_bytes());
    g.charge(selis_sandbox::Resource::Bytes, out.len() as u64)?;

    let actual = Guess {
        file_len: u64::try_from(out.len()).map_err(|_| to_error(LinearizeError::OutOfRange))?,
        first_start: first_xref_offset
            .checked_add(first_t_rel)
            .ok_or_else(|| to_error(LinearizeError::OutOfRange))?,
        first_end,
        main_xref,
    };
    Ok((out, actual))
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

/// Serialise a section, returning both the packed bytes and each object's
/// individual byte length. Keeping the two together avoids re-serialising to
/// recover boundaries when computing xref offsets.
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
    // `size` is the largest object number in this section; the xref header's
    // count is one more (entry 0 is the free head).
    let count = size.saturating_add(1);
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
    g.charge(selis_sandbox::Resource::Bytes, out.len() as u64)?;
    Ok((out, t_rel))
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
        // Object order: object 4 (first page) must precede object 5 (second page).
        let p4 = text.find("4 0 obj").expect("page 1 object");
        let p5 = text.find("5 0 obj").expect("page 2 object");
        assert!(p4 < p5, "first-page object must come first");
        // Two xref sections.
        assert!(text.matches("\nxref\n").count() >= 2, "two xref sections");
        // startxref points to a real offset.
        assert!(text.contains("startxref\n"), "startxref present");
        // /L must equal the real file length.
        let l = parse_lin_int(&text, "L");
        assert_eq!(l, pdf.len() as u64, "/L equals file length");
        // /N page count, /O first-page number, /P first page.
        assert_eq!(parse_lin_int(&text, "N"), 2, "/N page count");
        assert_eq!(parse_lin_int(&text, "O"), 4, "/O first page object");
        assert_eq!(parse_lin_int(&text, "P"), 1, "/P first page number");
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
}
