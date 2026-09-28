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

#[cfg(test)]
mod tests {
    #![allow(clippy::indexing_slicing, clippy::arithmetic_side_effects)]

    use super::*;
    use crate::nav::ActionKind;
    use selis_pdf_cos::XrefEntry;
    use selis_sandbox::{CancelToken, FixedClock};

    /// Assemble a single-revision document from `(object number, body)` pairs.
    fn build(objects: &[(u32, &str)]) -> (selis_pdf_cos::Doc, Vec<u8>) {
        let mut src = Vec::new();
        let mut xref = std::collections::BTreeMap::new();
        for (num, body) in objects {
            let offset = u64::try_from(src.len()).unwrap_or(0);
            src.extend_from_slice(format!("{num} 0 obj\n{body}\nendobj\n").as_bytes());
            xref.insert(*num, XrefEntry::InUse { offset, gen: 0 });
        }
        let trailer = vec![(
            selis_bytes::Bytes::copy_from_slice(b"Root"),
            Obj::Ref(Ref::new(1, 0)),
        )];
        (selis_pdf_cos::Doc::from_single_revision(xref, trailer), src)
    }

    /// The page map the tests address destinations against: object 3 is page 0.
    fn one_page() -> PageMap {
        PageMap::new(&[crate::Page {
            num: 3,
            media_box: None,
            crop_box: None,
            rotate: None,
            resources: None,
            contents: None,
        }])
    }

    /// Read page 3's links out of `objects`.
    fn read_links(objects: &[(u32, &str)]) -> Result<Vec<LinkAnnotation>> {
        let budget = Budget::unlimited();
        let (doc, src) = build(objects);
        let mut g = budget.guard_with(&FixedClock(0), CancelToken::new());
        let mut resolver = Resolver::new(&doc, &src, &budget);
        page_links(&mut resolver, Ref::new(3, 0), &one_page(), &budget, &mut g)
    }

    /// **The bare-string file specification**, which is how most writers spell
    /// `/Launch`. A reader that only understood the dictionary form delivered
    /// this as a `/Launch` with nothing to show the reader, and "an action
    /// class with no readable target" is the state ADR-P0020 exists to keep
    /// distinguishable from "no action".
    #[test]
    fn a_launch_keeps_its_class_and_a_string_file_specification() {
        let links = read_links(&[
            (1, "<< /Type /Catalog >>"),
            (3, "<< /Type /Page /Annots [20 0 R] >>"),
            (
                20,
                "<< /Type /Annot /Subtype /Link /Rect [10 700 200 720] \
                 /A << /S /Launch /F (cmd.exe /c calc.exe) >> >>",
            ),
        ])
        .expect("read");
        let action = links
            .first()
            .and_then(|l| l.action.as_ref())
            .expect("the action survives");
        assert_eq!(action.kind, ActionKind::Launch);
        assert_eq!(action.uri.as_deref(), Some("cmd.exe /c calc.exe"));
        assert_eq!(action.kind.as_name(), b"Launch");
    }

    /// The dictionary form, and the `/UF` over `/F` precedence. Both spellings
    /// have to reach the viewer, because which one a document used is the
    /// document's business and not a reason to drop the target.
    #[test]
    fn a_launch_also_reads_a_dictionary_file_specification() {
        let links = read_links(&[
            (1, "<< /Type /Catalog >>"),
            (3, "<< /Type /Page /Annots [20 0 R] >>"),
            (20, "<< /Type /Annot /Subtype /Link /A 21 0 R >>"),
            (21, "<< /S /Launch /F 22 0 R >>"),
            (22, "<< /Type /Filespec /F (plain.txt) /UF (unicode.txt) >>"),
        ])
        .expect("read");
        let action = links
            .first()
            .and_then(|l| l.action.as_ref())
            .expect("the action survives");
        assert_eq!(action.kind, ActionKind::Launch);
        assert_eq!(
            action.uri.as_deref(),
            Some("unicode.txt"),
            "/UF is the spec's first choice"
        );
    }

    /// Annotations that are not `/Link` are not the link layer's business, and
    /// a form field's activation surface must not arrive as a clickable link.
    #[test]
    fn a_widget_annotation_is_not_a_link() {
        let links = read_links(&[
            (1, "<< /Type /Catalog >>"),
            (3, "<< /Type /Page /Annots [20 0 R 21 0 R] >>"),
            (
                20,
                "<< /Type /Annot /Subtype /Widget /Rect [10 10 100 30] /FT /Tx /T (field1) >>",
            ),
            (
                21,
                "<< /Type /Annot /Subtype /Link /Rect [0 0 10 10] /Dest [3 0 R /Fit] >>",
            ),
        ])
        .expect("read");
        assert_eq!(links.len(), 1, "the widget is skipped, the link is not");
        assert_eq!(links.first().map(|l| l.object), Some(Some(21)));
    }

    /// A link with no `/Rect` and no action is still a link. It is a *dead*
    /// link, which the viewer can refuse with a reason — a state it can only
    /// reach if the engine told it the link exists.
    #[test]
    fn a_dead_link_is_still_a_link() {
        let links = read_links(&[
            (1, "<< /Type /Catalog >>"),
            (3, "<< /Type /Page /Annots [20 0 R] >>"),
            (20, "<< /Type /Annot /Subtype /Link >>"),
        ])
        .expect("read");
        let link = links.first().expect("the link");
        assert!(link.rect.is_none());
        assert!(link.action.is_none());
        assert!(link.target.is_none());
        assert!(link.contents.is_none());
    }

    /// `/Annots` order is the document's order and the reader hands it back
    /// unchanged: `index` is what a viewer hands back to mean "this link".
    #[test]
    fn links_keep_annots_order_and_their_index() {
        let links = read_links(&[
            (1, "<< /Type /Catalog >>"),
            (3, "<< /Type /Page /Annots [20 0 R 21 0 R 22 0 R] >>"),
            (20, "<< /Type /Annot /Subtype /Link /Contents (first) >>"),
            (21, "<< /Type /Annot /Subtype /Link /Contents (second) >>"),
            (22, "<< /Type /Annot /Subtype /Link /Contents (third) >>"),
        ])
        .expect("read");
        let seen: Vec<(u32, Option<String>)> = links
            .iter()
            .map(|l| (l.index, l.contents.clone()))
            .collect();
        assert_eq!(
            seen,
            vec![
                (0, Some("first".to_owned())),
                (1, Some("second".to_owned())),
                (2, Some("third".to_owned())),
            ]
        );
    }

    /// A `/Rect` that is not four numbers is `None`, not a zero box. A
    /// fabricated zero rectangle would swallow clicks in the corner of the
    /// page, which is a *worse* defect than a link the viewer cannot place.
    #[test]
    fn a_malformed_rect_is_absent_not_zero() {
        let links = read_links(&[
            (1, "<< /Type /Catalog >>"),
            (3, "<< /Type /Page /Annots [20 0 R 21 0 R] >>"),
            (20, "<< /Type /Annot /Subtype /Link /Rect [0 0 10] >>"),
            (21, "<< /Type /Annot /Subtype /Link /Rect [0 0 10 10] >>"),
        ])
        .expect("read");
        assert!(links.first().expect("first").rect.is_none());
        assert_eq!(
            links.get(1).expect("second").rect,
            Some([0.0, 0.0, 10.0, 10.0]),
            "an honest rectangle is verbatim"
        );
    }

    /// A page with no `/Annots` has no links. That is a list, not a refusal.
    #[test]
    fn a_page_with_no_annots_has_no_links() {
        let links =
            read_links(&[(1, "<< /Type /Catalog >>"), (3, "<< /Type /Page >>")]).expect("read");
        assert!(links.is_empty());
    }

    /// Object exhaustion is a typed refusal, not a short list. The difference
    /// between "this page has three links" and "we stopped looking" is the
    /// difference between a list and a lie.
    #[test]
    fn object_exhaustion_is_a_typed_refusal() {
        let budget = Budget {
            objects: 1,
            ..Budget::unlimited()
        };
        let objects = [
            (1u32, "<< /Type /Catalog >>"),
            (3, "<< /Type /Page /Annots [20 0 R 21 0 R] >>"),
            (20, "<< /Type /Annot /Subtype /Link /Rect [0 0 1 1] >>"),
            (21, "<< /Type /Annot /Subtype /Link /Rect [0 0 1 1] >>"),
        ];
        let (doc, src) = build(&objects);
        let mut g = budget.guard_with(&FixedClock(0), CancelToken::new());
        let mut resolver = Resolver::new(&doc, &src, &budget);
        let err = page_links(&mut resolver, Ref::new(3, 0), &one_page(), &budget, &mut g)
            .expect_err("must refuse, not truncate");
        assert_eq!(err.code(), selis_error::Code::BudgetObjects);
    }

    /// **An action class this build has never heard of is reported as itself.**
    /// This is the assertion ADR-P0020 is load-bearing on: the engine reports
    /// the class, the viewer decides. `/SetState` is a real action this build
    /// does not model, and coercing it to `None` — or to a neighbouring class —
    /// would make "the viewer refused this" and "the engine never heard of it"
    /// the same wire value, and only one of those is a decision anybody made.
    #[test]
    fn an_unmodelled_action_class_surfaces_as_itself_never_as_nothing() {
        let links = read_links(&[
            (1, "<< /Type /Catalog >>"),
            (3, "<< /Type /Page /Annots [20 0 R 21 0 R] >>"),
            (
                20,
                "<< /Type /Annot /Subtype /Link /A << /S /SetState /State (x) >> >>",
            ),
            // A class this build *does* model keeps its own name, so "unknown"
            // on the wire means genuinely unmodelled rather than merely unlisted
            // in a match arm.
            (
                21,
                "<< /Type /Annot /Subtype /Link /A << /S /Rendition /R 22 0 R >> >>",
            ),
            (22, "<< /Type /MediaPlayback /D 3 0 R >>"),
        ])
        .expect("read");
        let unmodelled = links
            .first()
            .and_then(|l| l.action.as_ref())
            .expect("an unmodelled class is still an action");
        assert_eq!(
            unmodelled.kind,
            ActionKind::Other(selis_bytes::Bytes::copy_from_slice(b"SetState")),
            "the document's own name, verbatim"
        );
        assert_eq!(unmodelled.kind.as_name(), b"SetState");
        let modelled = links
            .get(1)
            .and_then(|l| l.action.as_ref())
            .expect("the modelled class is an action too");
        assert_eq!(modelled.kind, ActionKind::Rendition);
        assert_eq!(modelled.kind.as_name(), b"Rendition");
    }
}
