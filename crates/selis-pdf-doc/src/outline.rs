//! The `/Outlines` bookmark tree (SL-3.DOC-NAV, the engine half of SL-4.UI.06).
//!
//! ## The shape, and why
//!
//! **Nested, not flat-with-depth.** A `/Outlines` tree is a tree, the UI's
//! `OutlineNode` is a tree, and the one thing a sidebar needs that a flat list
//! makes the reader reconstruct is the parent/child relation. Nesting also
//! makes the cycle defence legible: a cycle is a node reachable from itself,
//! which is only a statement you can make about a tree.
//!
//! ## The cycle, and why depth-limiting is not the answer
//!
//! A document can point an outline item's `/First` at its own ancestor. The
//! walk is therefore over an **explicit worklist with a global visited set of
//! object numbers**, not over native recursion:
//!
//! * Native recursion is banned in this crate (SL-0.SBX.04) — document-chosen
//!   nesting depth on the Rust stack is a process abort the panic trampoline
//!   cannot catch.
//! * The visited set makes the cycle **structurally impossible** rather than
//!   merely bounded: an object number is expanded at most once in the whole
//!   walk, so the loop a self-referential `/First` would drive has no second
//!   iteration. The depth budget is still enforced (it is a kernel invariant,
//!   ADR-P0006), but it is a second line, not the one holding this up.
//! * A *global* set rather than an ancestor-path set, because a diamond — item
//!   `A` with two children that both point at the same object `D` — expands
//!   exponentially under path-only checking. `A`'s two parents each reach `D`,
//!   each of `D`'s two parents reaches the next level, and depth *d* is `2^d`
//!   nodes: a denial-of-service vector wearing a tree costume. Refusing to
//!   expand an object twice bounds the walk by the number of objects in the
//!   file, which is the only bound a hostile document cannot choose.

//!
//! ## What a malformed outline yields
//!
//! **A partial tree plus [`OutlineTree::truncated`], never a typed refusal** —
//! for *document* defects. A cyclic branch, an item that resolves to nothing,
//! an item whose `/Title` is a number: each costs the reader that branch and
//! nothing else, and the flag says so rather than letting a short outline pass
//! as a complete one. `/Count` is never trusted for traversal at all — it is
//! carried verbatim on each item (its sign is the spec's "starts collapsed"
//! convention) and the walk follows `/First`/`/Next`.
//!
//! **Budget and cancellation are the exception, and they are refusals.** A walk
//! that ran out of budget has not produced a shorter true answer, it has
//! produced a wrong one, and ADR-P0006's poisoning guard exists precisely
//! because "a parser that swallows a budget error and keeps going produces a
//! wrong-but-plausible result". Those propagate as typed `BUDGET_*` /
//! `CANCELLED` errors, which a transport surfaces as a failed op —
//! distinguishable, at the seam, from an empty outline and from a partial one.

use std::collections::BTreeSet;

use selis_error::Result;
use selis_pdf_cos::{Obj, Ref};
use selis_sandbox::{Budget, BudgetGuard, Resource};

use crate::nav::{self, Action, NavTarget, PageMap};
use crate::Resolver;

/// One outline (bookmark) item.
#[derive(Debug, Clone, PartialEq)]
pub struct OutlineItem {
    /// The `/Title` as the document wrote it. Never localised, never trimmed.
    pub title: String,
    /// Where the item points, from `/Dest` or from a `/GoTo` action.
    pub target: Option<NavTarget>,
    /// The item's `/A` action, when it has one.
    ///
    /// Reported rather than dropped. The UI's `OutlineNode` has no field for
    /// it — a finding recorded for SL-4.UI.06 — but an outline item carrying
    /// `/S /Launch` must not arrive at the viewer looking like a bookmark with
    /// no target, which is the shape a reader would click without a second
    /// thought.
    pub action: Option<Action>,
    /// `/Count` verbatim, when the document set it.
    ///
    /// **Negative means the subtree starts collapsed**, which is the PDF
    /// convention (section 12.3.3) and the reason a viewer's initial expansion
    /// is a decision rather than a constant. Never used to bound the walk: a
    /// document that says `/Count 1` over four children gets four children.
    pub count: Option<i64>,
    /// The item's children, in document order.
    pub children: Vec<OutlineItem>,
}

/// A document's outline, and what it took to get it.
#[derive(Debug, Clone, PartialEq)]
pub struct OutlineTree {
    /// The top-level items, in document order.
    pub items: Vec<OutlineItem>,
    /// Whether the catalog declared an `/Outlines` tree at all.
    ///
    /// The distinction the viewer is built on: `false` means *this document
    /// has no outline*, which is different from *this host cannot read the
    /// outline* and different again from `true` with a truncated walk.
    pub present: bool,
    /// Whether the walk gave up on anything — a cycle, an unresolvable item, a
    /// branch cut short. `false` with a non-empty `items` means the tree is
    /// believed complete.
    pub truncated: bool,
    /// How many branches were given up on, so a shell can say "some of this
    /// outline could not be read" rather than merely "this outline is
    /// incomplete".
    pub pruned: u32,
}

impl OutlineTree {
    /// The tree of a document that declares no `/Outlines`.
    fn absent() -> Self {
        Self {
            items: Vec::new(),
            present: false,
            truncated: false,
            pruned: 0,
        }
    }

    /// The tree of a document whose `/Outlines` we could not read at all.
    ///
    /// `present` stays `true`: the catalog *did* claim an outline, so the
    /// document has one and this host could not read it — the exact
    /// distinction the viewer's optional navigation port is built around.
    fn unreadable() -> Self {
        Self {
            items: Vec::new(),
            present: true,
            truncated: true,
            pruned: 1,
        }
    }
}

/// One node of the walk, before its children are attached.
///
/// The walk fills a flat arena in visit (pre-order) and assembles afterwards,
/// which is what keeps the traversal itself iterative. A child index is always
/// greater than its parent's, so the assembly can run in reverse.
struct Node {
    item: Option<OutlineItem>,
    children: Vec<usize>,
    root: bool,
}

/// What the walk is about to visit: a reference, or an item the document
/// embedded directly in the `/First` or `/Next` slot.
///
/// A damaged writer does embed outline items inline; `walk_inline_page` in
/// `doc.rs` handles the same case in the page tree. An inline item carries no
/// object number, so it cannot be *in* a reference cycle — its nesting is
/// bounded by the file's own size and by the depth budget.
enum Pending {
    Ref(Ref),
    Inline(Obj),
}

/// One open level of the walk: where to resume, and for whom.
struct Resume {
    next: Option<Pending>,
    owner: Option<usize>,
    depth: u16,
}

/// Walk the catalog's `/Outlines` tree.
///
/// # Budget
///
/// `Objects` is charged per item (the resolver charges it for an indirect
/// item; an inline one is charged here, so a document whose outline is entirely
/// inline is bounded too), and `Depth` is entered once per open level of the
/// tree. `budget` is the caller's and is threaded only so this and the rest of
/// the crate's document readers keep one signature.
///
/// # Malformed Input
///
/// **Document defects yield a partial tree, flagged.** A cyclic or shared item
/// (a second visit to an object number), an item that does not resolve, an item
/// that resolves to something other than a dictionary, and an `/Outlines` the
/// document cannot supply all set [`OutlineTree::truncated`] and cost the
/// reader that branch only. A missing `/Title`, a missing or non-integer
/// `/Count`, and a missing or malformed `/Dest`/`/A` cost the item its
/// destination, never the item itself. `/Count` is never used to bound the
/// walk.
///
/// **Budget exhaustion and cancellation are refusals, not partial trees** —
/// they propagate as typed errors. See the module documentation for why that
/// distinction is the load-bearing one.
pub fn outline(
    resolver: &mut Resolver<'_>,
    catalog: &Obj,
    pages: &PageMap,
    _budget: &Budget,
    g: &mut BudgetGuard<'_>,
) -> Result<OutlineTree> {
    let Some(root_ref) = nav::dict_ref(catalog, b"Outlines") else {
        return Ok(OutlineTree::absent());
    };
    // `present` follows the catalog's *claim*, not our ability to read it: a
    // document that declares an `/Outlines` it cannot supply has an outline we
    // cannot show, which is a different thing from having none.
    let root = match resolve_lenient(resolver, root_ref, g)? {
        Some(obj) => obj,
        None => return Ok(OutlineTree::unreadable()),
    };
    let Some(first) = child_of(&root, b"First") else {
        return Ok(OutlineTree {
            items: Vec::new(),
            present: true,
            truncated: false,
            pruned: 0,
        });
    };

    let mut nodes: Vec<Node> = Vec::new();
    let mut visited: BTreeSet<u32> = BTreeSet::new();
    let mut stack: Vec<Resume> = Vec::new();
    let mut truncated = false;
    let mut pruned: u32 = 0;
    let mut cur: Option<Pending> = Some(first);
    let mut owner: Option<usize> = None;
    let mut depth: u16 = 1;

    // One `enter` per open level and one `leave` when it closes, so the depth
    // the kernel sees is the outline's real nesting. `DepthGuard` cannot be
    // held across the whole walk because the levels are interleaved on one
    // stack rather than nested on the Rust stack; an early return leaves the
    // counter high, which is harmless because every early return out of this
    // loop is a budget or cancellation failure and those poison the guard
    // (ADR-P0006), so nothing further can be charged against it.
    g.enter()?;
    let mut walking = true;
    while walking {
        let mut descended = false;
        while let Some(pending) = cur.take() {
            g.tick()?;
            // The cycle and diamond guard, before any resolution: an object
            // number is expanded at most once in the whole walk, so the loop a
            // self-referential `/First` would drive has no second iteration.
            if let Pending::Ref(r) = &pending {
                if !visited.insert(r.num) {
                    truncated = true;
                    pruned = pruned.saturating_add(1);
                    break;
                }
            }
            let dict = match materialise(resolver, &pending, g)? {
                Some(d) => d,
                None => {
                    truncated = true;
                    pruned = pruned.saturating_add(1);
                    break;
                }
            };
            if !matches!(dict, Obj::Dict(_)) {
                truncated = true;
                pruned = pruned.saturating_add(1);
                break;
            }
            // An inline item resolved no object, so nothing has been charged
            // for it yet; without this a document whose outline is entirely
            // inline would walk for free.
            if matches!(pending, Pending::Inline(_)) {
                g.charge_one(Resource::Objects)?;
            }
            let item = build_item(resolver, &dict, pages, g)?;
            let next = child_of(&dict, b"Next");
            let first_child = child_of(&dict, b"First");
            let me = nodes.len();
            nodes.push(Node {
                item: Some(item),
                children: Vec::new(),
                root: owner.is_none(),
            });
            if let Some(parent) = owner {
                if let Some(node) = nodes.get_mut(parent) {
                    node.children.push(me);
                }
            }
            match first_child {
                Some(kid) => {
                    stack.push(Resume {
                        next,
                        owner: Some(me),
                        depth,
                    });
                    // The child's level. `enter` is also the depth bound: a
                    // document nested past the budget gets a typed
                    // `BUDGET_DEPTH` refusal here, not a truncated tree.
                    g.enter()?;
                    cur = Some(kid);
                    owner = Some(me);
                    depth = depth.saturating_add(1);
                    descended = true;
                    break;
                }
                None => cur = next,
            }
        }
        if descended {
            continue;
        }
        g.leave();
        match stack.pop() {
            Some(resume) => {
                cur = resume.next;
                owner = resume.owner;
                depth = resume.depth;
            }
            None => walking = false,
        }
    }

    // Assemble. Every child index is greater than its parent's, so one reverse
    // pass folds each finished subtree into its parent — no recursion, and the
    // nesting the walk enforced is the nesting the reader sees.
    for index in (0..nodes.len()).rev() {
        let child_indices = match nodes.get_mut(index) {
            Some(node) => std::mem::take(&mut node.children),
            None => continue,
        };
        if child_indices.is_empty() {
            continue;
        }
        let mut folded = Vec::new();
        for child in child_indices {
            if let Some(taken) = nodes.get_mut(child).and_then(|c| c.item.take()) {
                folded.push(taken);
            }
        }
        if let Some(item) = nodes.get_mut(index).and_then(|n| n.item.as_mut()) {
            item.children = folded;
        }
    }
    let mut roots: Vec<usize> = Vec::new();
    for (index, node) in nodes.iter().enumerate() {
        if node.root {
            roots.push(index);
        }
    }
    let mut items = Vec::new();
    for index in roots {
        if let Some(item) = nodes.get_mut(index).and_then(|n| n.item.take()) {
            items.push(item);
        }
    }
    Ok(OutlineTree {
        items,
        present: true,
        truncated,
        pruned,
    })
}

/// Build one item from its dictionary.
///
/// # Budget
///
/// Charges `Objects` for the item's `/A` when it is indirect, through
/// [`nav::parse_action`].
///
/// # Malformed Input
///
/// A `/Title` that is not a string or a name becomes the empty string; a
/// `/Count` that is not an integer is `None`; a missing or unreadable `/A` is
/// `None`; a `/Dest` that resolves to nothing is [`NavTarget::Unresolved`]. The
/// item itself is always returned — an outline entry with an unreadable
/// destination is something the reader can see and be told about, and dropping
/// it would be the "silently shorter outline" this whole module exists to
/// avoid.
fn build_item(
    resolver: &mut Resolver<'_>,
    dict: &Obj,
    pages: &PageMap,
    g: &mut BudgetGuard<'_>,
) -> Result<OutlineItem> {
    let title = match nav::dict_get(dict, b"Title") {
        Some(Obj::String(s)) | Some(Obj::HexString(s)) => nav::text_string(s.as_slice()),
        Some(Obj::Name(n)) => String::from_utf8_lossy(n.as_slice()).to_string(),
        _ => String::new(),
    };
    let count = match nav::dict_get(dict, b"Count") {
        Some(Obj::Int(i)) => Some(*i),
        _ => None,
    };
    let action = match nav::dict_get(dict, b"A") {
        Some(value) => nav::parse_action(resolver, value, pages, g)?,
        None => None,
    };
    // PDF 32000-2:2020 section 12.3.3: when an item carries both, `/A` wins. So
    // a `/Dest` beside a non-`/GoTo` action is ignored rather than reported as
    // a second, competing destination the viewer would have to choose between.
    let target = match &action {
        Some(act) => act.target.clone(),
        None => nav::dict_get(dict, b"Dest").map(|d| nav::parse_target(d, pages)),
    };
    Ok(OutlineItem {
        title,
        target,
        action,
        count,
        children: Vec::new(),
    })
}

/// A `/First` or `/Next` entry: a reference, or an item the document embedded
/// directly in the slot.
fn child_of(dict: &Obj, key: &[u8]) -> Option<Pending> {
    match nav::dict_get(dict, key)? {
        Obj::Ref(r) => Some(Pending::Ref(*r)),
        Obj::Dict(_) => nav::dict_get(dict, key).map(|v| Pending::Inline(v.clone())),
        _ => None,
    }
}

/// Resolve a pending item to its dictionary.
///
/// # Budget
///
/// Charges whatever the resolver charges for the reference (`Objects`); an
/// inline item costs nothing here.
///
/// # Malformed Input
///
/// An unresolvable reference is `Ok(None)` — the branch is dropped and the
/// caller flags the tree. Budget, cancellation and pending errors propagate:
/// they are not a document defect and must not be mistaken for one.
fn materialise(
    resolver: &mut Resolver<'_>,
    pending: &Pending,
    g: &mut BudgetGuard<'_>,
) -> Result<Option<Obj>> {
    match pending {
        Pending::Inline(obj) => Ok(Some(obj.clone())),
        Pending::Ref(r) => resolve_lenient(resolver, *r, g),
    }
}

/// Resolve a reference, distinguishing "the document is wrong" from "we ran
/// out of budget".
///
/// # Budget
///
/// Whatever [`Resolver::resolve`] charges.
///
/// # Malformed Input
///
/// `Ok(None)` for an object the document does not contain. Budget,
/// cancellation and pending errors propagate unchanged, so a caller can never
/// mistake exhaustion for a damaged file — which is the whole reason this
/// helper exists rather than a bare `.ok()`.
pub(crate) fn resolve_lenient(
    resolver: &mut Resolver<'_>,
    r: Ref,
    g: &mut BudgetGuard<'_>,
) -> Result<Option<Obj>> {
    match resolver.resolve(r, g) {
        Ok(obj) => Ok(Some(obj)),
        Err(e) if e.is_budget() || e.is_cancelled() || e.is_pending() => Err(e),
        Err(_) => Ok(None),
    }
}
