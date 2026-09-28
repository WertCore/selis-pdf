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

#[cfg(test)]
mod tests {
    #![allow(clippy::indexing_slicing, clippy::arithmetic_side_effects)]

    use super::*;
    use crate::nav::{ActionKind, DestinationKind};
    use selis_pdf_cos::XrefEntry;
    use selis_sandbox::{CancelToken, FixedClock};

    /// The `nav.pdf` conformance fixture, verbatim. It carries the hostile
    /// cases this module's tests are about — a cyclic `/First`, a `/Count` of
    /// 99 over two items, and an outline item whose `/A` is a `/GoTo` — and it
    /// is the *same bytes* the WASM conformance leg drives, so a behaviour
    /// proven here is the behaviour proven over the real guest ABI.
    const NAV_FIXTURE: &[u8] =
        include_bytes!("../../selis-pdf-engine/src/fixtures/nav.pdf");

    fn guard() -> BudgetGuard<'static> {
        Budget::unlimited().guard_with(&FixedClock(0), CancelToken::new())
    }

    /// Assemble a single-revision document from `(object number, body)` pairs.
    fn build(objects: &[(u32, &str)]) -> (selis_pdf_cos::Doc, Vec<u8>) {
        let mut src = Vec::new();
        let mut xref = std::collections::BTreeMap::new();
        for (num, body) in objects {
            let offset = u64::try_from(src.len()).unwrap_or(0);
            src.extend_from_slice(format!("{num} 0 obj\n{body}\nendobj\n").as_bytes());
            xref.insert(
                *num,
                XrefEntry::InUse {
                    offset,
                    gen: 0,
                },
            );
        }
        let trailer = vec![(
            selis_bytes::Bytes::copy_from_slice(b"Root"),
            Obj::Ref(Ref::new(1, 0)),
        )];
        (selis_pdf_cos::Doc::from_single_revision(xref, trailer), src)
    }

    /// A two-page document: object 3 is page 0, object 4 is page 1. The
    /// destinations in these tests are expressed in terms of it.
    fn two_pages() -> PageMap {
        PageMap::new(&[
            crate::Page {
                num: 3,
                media_box: None,
                crop_box: None,
                rotate: None,
                resources: None,
                contents: None,
            },
            crate::Page {
                num: 4,
                media_box: None,
                crop_box: None,
                rotate: None,
                resources: None,
                contents: None,
            },
        ])
    }

    /// Walk a catalog over `objects` and return the tree.
    fn walk(objects: &[(u32, &str)], budget: &Budget) -> Result<(OutlineTree, PageMap)> {
        let (doc, src) = build(objects);
        let mut g = budget.guard_with(&FixedClock(0), CancelToken::new());
        let mut resolver = Resolver::new(&doc, &src, budget);
        let catalog = resolver.resolve(Ref::new(1, 0), &mut g)?;
        let pages = two_pages();
        let tree = outline(&mut resolver, &catalog, &pages, budget, &mut g)?;
        Ok((tree, pages))
    }

    /// A catalog with a two-level outline whose root `/Count` lies (99 over two
    /// items) and whose first item's `/Count` is negative.
    fn ordinary_tree() -> Vec<(u32, &'static str)> {
        vec![
            (1, "<< /Type /Catalog /Outlines 10 0 R >>"),
            (10, "<< /Type /Outlines /First 11 0 R /Last 12 0 R /Count 99 >>"),
            (
                11,
                "<< /Title (Chapter One) /Parent 10 0 R /Next 12 0 R /First 13 0 R \
                 /Last 13 0 R /Count -2 /Dest [4 0 R /XYZ 100 200 1.5] >>",
            ),
            (12, "<< /Title (Appendix) /Parent 10 0 R /Prev 11 0 R /Dest (chapter-one) >>"),
            (13, "<< /Title (Section 1.1) /Parent 11 0 R /A 14 0 R >>"),
            (14, "<< /S /GoTo /D [3 0 R /Fit] >>"),
        ]
    }

    /// A two-level outline, with `/Count` lying at the root and negative on the
    /// first item. The walk follows `/First`/`/Next` and ignores `/Count`
    /// entirely, so the declared 99 costs the reader nothing.
    #[test]
    fn walks_a_nested_outline_and_never_trusts_count() {
        let budget = Budget::unlimited();
        let (tree, _) = walk(&ordinary_tree(), &budget).expect("walk");
        assert!(tree.present);
        assert!(!tree.truncated, "an honest tree must not be flagged");
        assert_eq!(tree.items.len(), 2, "two top-level items, not /Count 99");
        let first = tree.items.first().expect("first item");
        assert_eq!(first.title, "Chapter One");
        assert_eq!(first.count, Some(-2), "/Count is verbatim, sign included");
        let child = first.children.first().expect("child");
        assert_eq!(child.title, "Section 1.1");
        // A negative /Count is the spec's "starts collapsed". The walk must not
        // act on that convention itself: the viewer owns the decision, which is
        // why the child is in the tree either way.
        assert!(
            child.children.is_empty(),
            "the child is expanded regardless of the parent's negative /Count"
        );
    }

    /// The first item's destination resolves to a real page index, and the
    /// `/XYZ` parameters survive.
    #[test]
    fn an_xyz_destination_keeps_its_placement() {
        let budget = Budget::unlimited();
        let (tree, _) = walk(&ordinary_tree(), &budget).expect("walk");
        let first = tree.items.first().expect("first item");
        let Some(NavTarget::Page(page)) = first.target.as_ref() else {
            panic!("expected a page destination, got {:?}", first.target);
        };
        assert_eq!(page.page, 1, "object 4 is the document's second page");
        assert_eq!(page.kind, DestinationKind::Xyz);
        assert_eq!(page.left, Some(100.0));
        assert_eq!(page.top, Some(200.0));
        assert_eq!(page.zoom, Some(1.5));
    }

    /// A named `/Dest` is passed through verbatim for the viewer to resolve
    /// against `/Dests`. The engine does not chase it: deciding where a name
    /// ends is the viewer's lookup, not the reader's guess.
    #[test]
    fn a_named_destination_is_carried_not_chased() {
        let budget = Budget::unlimited();
        let (tree, _) = walk(&ordinary_tree(), &budget).expect("walk");
        let second = tree.items.get(1).expect("second item");
        assert_eq!(
            second.target,
            Some(NavTarget::Named("chapter-one".to_owned()))
        );
    }

    /// **The cycle.** Object 13's `/First` points at object 11, its own
    /// ancestor. The walk must terminate, must flag itself, and must still
    /// return everything reachable *outside* the cycle — the reader keeps the
    /// bookmarks they can see.
    #[test]
    fn a_cyclic_outline_terminates_and_is_flagged() {
        let budget = Budget::unlimited();
        let objects = vec![
            (1, "<< /Type /Catalog /Outlines 10 0 R >>"),
            (10, "<< /Type /Outlines /First 11 0 R /Count 1 >>"),
            (
                11,
                "<< /Title (Chapter One) /Parent 10 0 R /First 13 0 R /Count 1 >>",
            ),
            (13, "<< /Title (Section 1.1) /Parent 11 0 R /First 11 0 R >>"),
        ];
        // Without the visited set this call would not return.
        let (tree, _) = walk(&objects, &budget).expect("walk");
        assert!(tree.present);
        assert!(tree.truncated, "a cycle must be reported, not hidden");
        assert!(tree.pruned >= 1, "the pruned branch is counted");
        assert_eq!(tree.items.len(), 1);
        let first = tree.items.first().expect("first item");
        assert_eq!(first.title, "Chapter One");
        let child = first.children.first().expect("the child before the cycle");
        assert_eq!(child.title, "Section 1.1");
        assert!(
            child.children.is_empty(),
            "the cycle is cut, not expanded a second time"
        );
    }

    /// **The same cycle, over the committed fixture** — the bytes the WASM
    /// conformance leg drives. This is the assertion that the hostile document
    /// is a real document, not a hand-built object graph that happens to model
    /// one.
    #[test]
    fn the_fixture_outline_cycle_terminates_over_the_real_document() {
        let budget = Budget::unlimited();
        let mut g = guard();
        let (doc, _) = selis_pdf_cos::reconstruct(NAV_FIXTURE, &budget, &mut g)
            .expect("reconstruct the fixture");
        let mut resolver = Resolver::new(&doc, NAV_FIXTURE, &budget);
        let catalog = resolver.resolve(Ref::new(1, 0), &mut g).expect("catalog");
        let tree = outline(&mut resolver, &catalog, &two_pages(), &budget, &mut g).expect("walk");
        assert!(tree.present, "the fixture declares an /Outlines");
        assert!(
            tree.truncated,
            "the fixture's item 13 points /First at its own ancestor"
        );
        // The two honest top-level items survive; the third level does not.
        assert_eq!(tree.items.len(), 2);
        let first = tree.items.first().expect("first item");
        assert_eq!(first.title, "Chapter One");
        assert_eq!(first.children.len(), 1);
        assert_eq!(
            first.children.first().map(|c| c.title.as_str()),
            Some("Section 1.1")
        );
    }

    /// **The diamond.** Two items both name the same child, and that child's
    /// own two children do the same again, twenty levels deep. An
    /// ancestor-path check expands this as `2^depth`; a global visited set
    /// expands each object once, so the walk is linear in the number of
    /// objects — the only bound a hostile document cannot choose.
    #[test]
    fn a_shared_item_is_expanded_once_not_exponentially() {
        let budget = Budget::unlimited();
        let mut objects: Vec<(u32, String)> = vec![
            (1, "<< /Type /Catalog /Outlines 10 0 R >>".to_owned()),
            (10, "<< /Type /Outlines /First 100 0 R >>".to_owned()),
        ];
        for level in 0..20u32 {
            let id = 100 + level;
            let shared = id.saturating_add(1);
            objects.push((
                id,
                format!(
                    "<< /Title (L{level}) /Parent 10 0 R /First {shared} 0 R \
                     /Last {shared} 0 R >>"
                ),
            ));
        }
        let borrowed: Vec<(u32, &str)> = objects
            .iter()
            .map(|(n, b)| (*n, b.as_str()))
            .collect();
        let (tree, _) = walk(&borrowed, &budget).expect("walk");
        let mut count = 0usize;
        let mut stack: Vec<&OutlineItem> = tree.items.iter().collect();
        while let Some(item) = stack.pop() {
            count = count.saturating_add(1);
            stack.extend(item.children.iter());
        }
        assert!(
            count <= 20,
            "a shared object must be expanded once, saw {count} items"
        );
    }

    /// A catalog with no `/Outlines` is a document with no outline — which the
    /// viewer must be able to say, distinctly from "this host cannot read it".
    #[test]
    fn an_absent_outline_is_not_present_and_not_truncated() {
        let budget = Budget::unlimited();
        let (tree, _) = walk(&[(1, "<< /Type /Catalog >>")], &budget).expect("walk");
        assert!(!tree.present, "no /Outlines means the document has none");
        assert!(!tree.truncated, "and nothing was lost trying");
        assert!(tree.items.is_empty());
    }

    /// A catalog that *claims* an outline it cannot supply is `present` and
    /// `truncated`: the document has an outline, this host could not read it.
    /// Collapsing that into "no outline" is the exact confusion the optional
    /// navigation port exists to avoid.
    #[test]
    fn an_unreadable_outline_is_present_but_truncated() {
        let budget = Budget::unlimited();
        let (tree, _) = walk(&[(1, "<< /Type /Catalog /Outlines 99 0 R >>")], &budget)
            .expect("walk");
        assert!(tree.present, "the catalog claimed one");
        assert!(tree.truncated, "and we could not read it");
        assert!(tree.items.is_empty());
    }

    /// An `/Outlines` with no `/First` is an empty outline: present and whole.
    #[test]
    fn an_empty_outline_is_present_and_complete() {
        let budget = Budget::unlimited();
        let (tree, _) = walk(
            &[
                (1, "<< /Type /Catalog /Outlines 10 0 R >>"),
                (10, "<< /Type /Outlines /Count 0 >>"),
            ],
            &budget,
        )
        .expect("walk");
        assert!(tree.present);
        assert!(!tree.truncated, "an empty outline is a whole outline");
        assert!(tree.items.is_empty());
    }

    /// A `/First` naming an object the document does not contain costs the
    /// reader that branch and is flagged. It does not fail the document, and it
    /// does not pass as a complete outline either.
    #[test]
    fn an_unresolvable_item_is_flagged_not_fatal() {
        let budget = Budget::unlimited();
        let (tree, _) = walk(
            &[
                (1, "<< /Type /Catalog /Outlines 10 0 R >>"),
                (10, "<< /Type /Outlines /First 99 0 R >>"),
            ],
            &budget,
        )
        .expect("walk");
        assert!(tree.truncated);
        assert!(tree.items.is_empty());
    }

    /// A `/First` resolving to something other than a dictionary is a document
    /// defect of the same class.
    #[test]
    fn an_item_that_is_not_a_dictionary_is_flagged() {
        let budget = Budget::unlimited();
        let (tree, _) = walk(
            &[
                (1, "<< /Type /Catalog /Outlines 10 0 R >>"),
                (10, "<< /Type /Outlines /First 11 0 R >>"),
                (11, "42"),
            ],
            &budget,
        )
        .expect("walk");
        assert!(tree.truncated);
        assert!(tree.items.is_empty());
    }

    /// An item with no `/Title`, no `/Dest` and no `/A` is still an item. This
    /// is the "silently shorter outline" the whole module exists to prevent.
    #[test]
    fn a_bare_item_is_still_an_item() {
        let budget = Budget::unlimited();
        let (tree, _) = walk(
            &[
                (1, "<< /Type /Catalog /Outlines 10 0 R >>"),
                (10, "<< /Type /Outlines /First 11 0 R >>"),
                (11, "<< /Parent 10 0 R >>"),
            ],
            &budget,
        )
        .expect("walk");
        assert!(!tree.truncated, "a bare item is not a defect");
        let item = tree.items.first().expect("the item");
        assert_eq!(item.title, "");
        assert!(item.target.is_none());
        assert!(item.action.is_none());
    }

    /// A `/Dest` naming a page the document does not contain is reported as
    /// unresolved. Inventing page 0 would navigate the reader somewhere the
    /// document never said.
    #[test]
    fn a_destination_naming_a_missing_page_is_unresolved() {
        let budget = Budget::unlimited();
        let (tree, _) = walk(
            &[
                (1, "<< /Type /Catalog /Outlines 10 0 R >>"),
                (10, "<< /Type /Outlines /First 11 0 R >>"),
                (11, "<< /Title (Nowhere) /Parent 10 0 R /Dest [77 0 R /Fit] >>"),
            ],
            &budget,
        )
        .expect("walk");
        let item = tree.items.first().expect("the item");
        assert_eq!(item.target, Some(NavTarget::Unresolved));
    }

    /// An outline item whose `/A` is a `/Launch` keeps its class. The UI's
    /// `OutlineNode` has no field for an action, which is a finding — but
    /// dropping it here would deliver a `/Launch` bookmark to the viewer looking
    /// exactly like a bookmark with no target.
    #[test]
    fn a_launch_on_an_outline_item_keeps_its_class() {
        let budget = Budget::unlimited();
        let (tree, _) = walk(
            &[
                (1, "<< /Type /Catalog /Outlines 10 0 R >>"),
                (10, "<< /Type /Outlines /First 11 0 R >>"),
                (
                    11,
                    "<< /Title (Run) /Parent 10 0 R /A << /S /Launch /F (calc.exe) >> >>",
                ),
            ],
            &budget,
        )
        .expect("walk");
        let item = tree.items.first().expect("the item");
        let action = item.action.as_ref().expect("the action survives");
        assert_eq!(action.kind, ActionKind::Launch);
        assert_eq!(action.uri.as_deref(), Some("calc.exe"));
        // `/A` wins over `/Dest` (section 12.3.3), and a non-`/GoTo` action
        // contributes no destination of its own.
        assert!(item.target.is_none());
    }

    /// A `/Next` chain is reported and not followed. The chain is a second place
    /// a `/Launch` can hide, and whether a chain is part of one activation is
    /// the viewer's call — but the engine must not hide that one exists.
    #[test]
    fn a_next_chain_is_reported_and_not_followed() {
        let budget = Budget::unlimited();
        let (tree, _) = walk(
            &[
                (1, "<< /Type /Catalog /Outlines 10 0 R >>"),
                (10, "<< /Type /Outlines /First 11 0 R >>"),
                (
                    11,
                    "<< /Title (Chained) /Parent 10 0 R /A << /S /URI /URI (https://a.test/) \
                     /Next 12 0 R >> >>",
                ),
                (12, "<< /S /Launch /F (calc.exe) >>"),
            ],
            &budget,
        )
        .expect("walk");
        let action = tree
            .items
            .first()
            .and_then(|i| i.action.as_ref())
            .expect("the action");
        assert!(
            action.has_next,
            "the chain is visible, not silently dropped"
        );
        assert_eq!(action.kind, ActionKind::Uri);
    }

    /// Nesting past the depth budget is a **typed refusal**, not a truncated
    /// tree. This is the one case where "shorter" would have meant "wrong".
    #[test]
    fn depth_past_the_budget_is_a_typed_refusal_not_a_short_tree() {
        let mut objects: Vec<(u32, String)> = vec![
            (1, "<< /Type /Catalog /Outlines 10 0 R >>".to_owned()),
            (10, "<< /Type /Outlines /First 100 0 R >>".to_owned()),
        ];
        for level in 0..12u32 {
            let id = 100 + level;
            let child = id.saturating_add(1);
            objects.push((
                id,
                format!("<< /Title (L{level}) /Parent 10 0 R /First {child} 0 R >>"),
            ));
        }
        objects.push((112, "<< /Title (Leaf) /Parent 111 0 R >>".to_owned()));
        let borrowed: Vec<(u32, &str)> = objects
            .iter()
            .map(|(n, b)| (*n, b.as_str()))
            .collect();
        let budget = Budget {
            depth: 4,
            ..Budget::unlimited()
        };
        let err = walk(&borrowed, &budget).expect_err("must refuse, not truncate");
        assert_eq!(
            err.code(),
            selis_error::Code::BudgetDepth,
            "expected BUDGET_DEPTH"
        );
    }

    /// The object budget bounds the walk the same way: exhaustion is a typed
    /// `BUDGET_OBJECTS`, not a quietly short outline.
    #[test]
    fn object_exhaustion_is_a_typed_refusal() {
        let budget = Budget {
            objects: 3,
            ..Budget::unlimited()
        };
        let err = walk(&ordinary_tree(), &budget).expect_err("must refuse");
        assert_eq!(
            err.code(),
            selis_error::Code::BudgetObjects,
            "expected BUDGET_OBJECTS"
        );
    }
}
