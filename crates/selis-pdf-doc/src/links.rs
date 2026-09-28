//! Link annotations (`/Annots` with `/Subtype /Link`) — SL-3.DOC-NAV.
//!
//! The other half of what `SL-4.UI.06`'s `NavigationPort.pageLinks` needs, and
//! the reader with the sharpest edge in the document model: **every value it
//! returns is something a viewer might act on.** A URI is a place the reader
//! can be sent; a `/Launch` is a process the reader's machine can start. So the
//! rule this module is built around is the one ADR-P0020 makes structural —
//!
//! > Report the action class. Decide what happens with it.
//!
//! Every action dictionary becomes an [`Action`] with its class named
//! verbatim, including the four classes ADR-P0020 disables and including a
//! class this build has never heard of ([`ActionKind::Other`]). Nothing is
//! filtered, reordered by severity, or dropped: a `/Launch` that arrived at
//! the viewer as "a link with no action" would be a link the viewer had no
//! reason to refuse, which is exactly how one gets through.
//!
//! Annotations of other subtypes (`/Widget`, `/Popup`, `/Text`, …) are skipped
//! silently and for a different reason: they are not links, the port asked for
//! links, and their presence says nothing about whether this page's links
//! could be read.

use selis_error::Result;
use selis_pdf_cos::{Obj, Ref};
use selis_sandbox::{Budget, BudgetGuard, Resource};

use crate::nav::{self, Action, NavTarget, PageMap};
use crate::Resolver;

/// One `/Link` annotation on a page.
#[derive(Debug, Clone, PartialEq)]
pub struct LinkAnnotation {
    /// Position in the page's `/Annots` array, zero-based. The document-scoped
    /// id a viewer hands back when the reader clicks this link.
    pub index: u32,
    /// The annotation's object number, when it is an indirect object; `None`
    /// for an annotation the document embedded directly in `/Annots`.
    pub object: Option<u32>,
    /// `/Rect`, `[x0 y0 x1 y1]` in PDF user space, **verbatim and unnormalised**
    /// — a link whose rectangle is inverted is a document defect a viewer may
    /// usefully normalise for hit-testing, and the engine is not the layer that
    /// decides what "where" means. `None` when `/Rect` is absent or is not
    /// four numbers.
    pub rect: Option<[f64; 4]>,
    /// What the link asks for: its `/A` action, or its `/Dest` when it has no
    /// action. `None` for a link with neither, which is a dead link rather
    /// than a missing one.
    pub target: Option<NavTarget>,
    /// The link's `/A` action, when it has one.
    pub action: Option<Action>,
    /// `/Contents`, the link's accessible name.
    pub contents: Option<String>,
}

/// Read one page's link annotations, in `/Annots` order.
///
/// # Budget
///
/// `Objects` is charged per annotation resolved and per action dictionary the
/// resolver has to follow. `budget` is the caller's, threaded only so this and
/// the rest of the crate's document readers keep one signature.
///
/// # Malformed Input
///
/// A page with no `/Annots`, an `/Annots` that is not an array, an entry that
/// is not a dictionary, an annotation whose `/Subtype` is not `/Link`, and a
/// `/Rect` that is not four numbers are each skipped rather than failing the
/// page: a page whose links are partly unreadable still has the links it
/// declared, and returning them is what lets the viewer show *some* of them
/// instead of none. Budget and cancellation propagate as typed errors — the
/// difference between "this page has three links" and "we stopped looking" is
/// the difference between a list and a lie.
pub fn page_links(
    resolver: &mut Resolver<'_>,
    page: Ref,
    pages: &PageMap,
    _budget: &Budget,
    g: &mut BudgetGuard<'_>,
) -> Result<Vec<LinkAnnotation>> {
    let mut out = Vec::new();
    let page_dict = match crate::outline::resolve_lenient(resolver, page, g)? {
        Some(d) => d,
        None => return Ok(out),
    };
    let annots = match nav::dict_get(&page_dict, b"Annots") {
        Some(Obj::Array(items)) => items.clone(),
        _ => return Ok(out),
    };
    for (index, entry) in annots.iter().enumerate() {
        g.tick()?;
        let (dict, object) = match entry {
            Obj::Ref(r) => match crate::outline::resolve_lenient(resolver, *r, g)? {
                Some(d) => (d, Some(r.num)),
                None => continue,
            },
            Obj::Dict(_) => (entry.clone(), None),
            _ => continue,
        };
        if !is_link(&dict) {
            continue;
        }
        g.charge_one(Resource::Objects)?;
        out.push(build_link(resolver, index, object, &dict, pages, g)?);
    }
    Ok(out)
}

/// Whether an annotation dictionary is a `/Link`.
///
/// An annotation with no `/Subtype` is not a link. Guessing "probably a link"
/// would put a `/Widget`'s activation surface into the link layer, which is the
/// wrong surface for a form field.
fn is_link(dict: &Obj) -> bool {
    matches!(nav::dict_get(dict, b"Subtype"), Some(Obj::Name(n)) if n.as_slice() == b"Link")
}

/// Build one link annotation from its dictionary.
///
/// # Budget
///
/// Charges whatever [`nav::parse_action`] charges for an indirect `/A`.
///
/// # Malformed Input
///
/// Every field is independently optional: a link with no `/Rect`, no `/A`, no
/// `/Dest` and no `/Contents` is still returned, as a link with nowhere to go.
/// That is a state the viewer can *refuse with a reason* — which is the whole
/// point of `LinkRefusal`'s `no-target` arm — and it is only reachable if the
/// reader is told the link exists.
fn build_link(
    resolver: &mut Resolver<'_>,
    index: usize,
    object: Option<u32>,
    dict: &Obj,
    pages: &PageMap,
    g: &mut BudgetGuard<'_>,
) -> Result<LinkAnnotation> {
    let action = match nav::dict_get(dict, b"A") {
        Some(value) => nav::parse_action(resolver, value, pages, g)?,
        None => None,
    };
    // A link uses `/A` if it has one and `/Dest` otherwise (§12.6.4.2); both
    // present is the same precedence rule the outline walk uses.
    let target = match &action {
        Some(act) => act.target.clone(),
        None => nav::dict_get(dict, b"Dest").map(|d| nav::parse_target(d, pages)),
    };
    let contents = match nav::dict_get(dict, b"Contents") {
        Some(Obj::String(s)) | Some(Obj::HexString(s)) => Some(nav::text_string(s.as_slice())),
        _ => None,
    };
    Ok(LinkAnnotation {
        index: u32::try_from(index).unwrap_or(u32::MAX),
        object,
        rect: nav::dict_get(dict, b"Rect").and_then(rect_of),
        target,
        action,
        contents,
    })
}

/// A `/Rect` array as `[x0 y0 x1 y1]`, verbatim.
fn rect_of(value: &Obj) -> Option<[f64; 4]> {
    let Obj::Array(items) = value else {
        return None;
    };
    // A `/Rect` that is not exactly four numbers is not a rectangle. The
    // surplus is ignored and the shortfall rejected: a three-element array has
    // no fourth edge to invent.
    if items.len() != 4 {
        return None;
    }
    let mut out = [0.0f64; 4];
    for (slot, item) in out.iter_mut().zip(items.iter()) {
        *slot = nav::obj_f64(item)?;
    }
    Some(out)
}
