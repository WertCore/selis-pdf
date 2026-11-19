//! Annotations: the standard PDF markup, with generated appearance streams
//! (SL-5.ANNOT.01, Phase 5).
//!
//! ## What this is
//!
//! The markup a reader leaves on a document - a highlight, an underline, a
//! strikeout, a note, a drawn mark. It is written as **spec-conformant PDF
//! annotations** with generated `/AP` appearance streams, because that is
//! what every other viewer reads (ADR-P0026). A private sidecar would be
//! easier and would be worth nothing to anyone else.
//!
//! Three things live here, and they are deliberately separate:
//!
//! * the **model** ([`AnnotKind`], [`AnnotSpec`], [`PageAnnotation`]) - what a
//!   markup is, in the terms the PDF specification uses;
//! * the **appearance generator** - the `/AP` stream each kind draws, so a
//!   viewer that does not implement the annotation's own appearance still
//!   shows the mark;
//! * the **writer** ([`annotate`], [`remove_annotation`]) and the **reader**
//!   ([`list_annotations`]) - the annotation in the document, and the
//!   annotations a document already carries.
//!
//! ## The honesty rules this crate is built around
//!
//! * every write is an **incremental revision**: the bytes before the edit are
//!   preserved verbatim, so a signature over an earlier revision stays
//!   verifiable;
//! * an annotation is written **with** its appearance: a mark that only some
//!   viewers draw is a mark that is not in the document;
//! * the reader reports what the document says. An annotation with no
//!   `/Contents` is an annotation with no text, not an empty comment.
//!
//! This crate never touches the filesystem or the network; it reads the bytes
//! the caller hands it and returns the bytes the caller hands out.

#![forbid(unsafe_code)]
#![allow(
    // Geometry is a chain of small float operations on validated values; the
    // crate still refuses a non-finite input at every entry point.
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss,
    clippy::integer_division
)]

use selis_bytes::Bytes;
use selis_error::{err, Code, Result};
use selis_pdf_cos::copy::resolve_ref;
use selis_pdf_cos::doc_writer::write_incremental_update;
use selis_pdf_cos::{Obj, Ref};
use selis_sandbox::{Budget, BudgetGuard};

pub mod xfdf;

#[cfg(test)]
mod tests;

/// The most pages a document may hold before the writer refuses it.
pub const MAX_PAGES: usize = 100_000;
/// The most annotations one page may carry.
pub const MAX_ANNOTS_PER_PAGE: usize = 2_000;
/// The most characters a comment may carry.
pub const MAX_COMMENT_CHARS: usize = 4_000;
/// The most characters an author name may carry.
pub const MAX_AUTHOR_CHARS: usize = 256;
/// The most points one ink mark may carry, in total across all its strokes.
pub const MAX_INK_POINTS: usize = 20_000;
/// The most separate strokes one ink annotation may carry.
pub const MAX_INK_PATHS: usize = 256;
/// The most points one stroke of an ink annotation may carry.
pub const MAX_INK_POINTS_PER_PATH: usize = 20_000;
/// The most text quads one markup may cover.
pub const MAX_QUADS: usize = 4_000;

/// The kinds of markup this crate writes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnnotKind {
    /// A highlight over the text the quads name.
    Highlight,
    /// A line under the text the quads name.
    Underline,
    /// A line through the text the quads name.
    StrikeOut,
    /// A note: the small folded-page icon, with the reader's comment.
    Text,
    /// A drawn mark: the reader's own strokes.
    Ink,
    /// A rectangle: a box drawn on the page.
    Square,
}

impl AnnotKind {
    /// The PDF `/Subtype` this kind is written as.
    #[must_use]
    pub const fn subtype(self) -> &'static str {
        match self {
            Self::Highlight => "Highlight",
            Self::Underline => "Underline",
            Self::StrikeOut => "StrikeOut",
            Self::Text => "Text",
            Self::Ink => "Ink",
            Self::Square => "Square",
        }
    }

    /// The kind a PDF `/Subtype` names, when this crate writes it.
    #[must_use]
    pub fn from_subtype(subtype: &str) -> Option<Self> {
        match subtype {
            "Highlight" => Some(Self::Highlight),
            "Underline" => Some(Self::Underline),
            "StrikeOut" => Some(Self::StrikeOut),
            "Text" => Some(Self::Text),
            "Ink" => Some(Self::Ink),
            "Square" => Some(Self::Square),
            _ => None,
        }
    }
}

/// One markup, as the caller asks for it.
#[derive(Debug, Clone, PartialEq)]
pub struct AnnotSpec {
    /// The kind of markup.
    pub kind: AnnotKind,
    /// The 1-based page the markup goes on.
    pub page: u32,
    /// The markup's rectangle, in the page's own user space.
    pub rect: [f64; 4],
    /// The text lines the markup covers, as rectangles in user space.
    pub quads: Vec<[f64; 4]>,
    /// The markup's colour, red, green and blue between 0 and 1.
    pub colour: [f64; 3],
    /// The reader's comment.
    pub contents: String,
    /// Who left the markup.
    pub author: String,
    /// The points of a single-stroke drawn mark, in user space. Kept as the
    /// convenient one-line form; [`Self::paths`] is the general, multi-stroke
    /// form and takes precedence when it is non-empty.
    pub points: Vec<(f64, f64)>,
    /// The separate strokes of a drawn mark, in user space. Each inner vector
    /// becomes one array in the PDF `/InkList`. Pressure and tilt are not part
    /// of this model: geometry is pressure-agnostic by design (ANNOT.03).
    pub paths: Vec<Vec<(f64, f64)>>,
    /// The width of a stroke, in points.
    pub width: f64,
    /// The annotation this one replies to, as its position in the page's
    /// `/Annots` order. A reply carries a PDF `/IRT` back-reference.
    pub reply_to: Option<usize>,
}

impl AnnotSpec {
    /// A markup of a kind, on a page, with the reader's colour.
    #[must_use]
    pub fn new(kind: AnnotKind, page: u32, rect: [f64; 4], colour: [f64; 3]) -> Self {
        Self {
            kind,
            page,
            rect,
            quads: Vec::new(),
            colour,
            contents: String::new(),
            author: String::new(),
            points: Vec::new(),
            paths: Vec::new(),
            width: 1.0,
            reply_to: None,
        }
    }
}

/// One annotation, as the document carries it.
#[derive(Debug, Clone, PartialEq)]
pub struct PageAnnotation {
    /// The PDF `/Subtype`.
    pub subtype: String,
    /// The annotation's rectangle, in user space.
    pub rect: [f64; 4],
    /// The annotation's colour, red, green and blue between 0 and 1.
    pub colour: [f64; 3],
    /// The comment the annotation carries, when it carries one.
    pub contents: String,
    /// Who left it, when it says.
    pub author: String,
    /// The text lines it covers, when it covers any.
    pub quads: Vec<[f64; 4]>,
    /// The annotation this one replies to, as its position in the page's
    /// `/Annots` order.
    pub irt: Option<usize>,
    /// The review state (`/State`), when the annotation carries one.
    pub state: Option<String>,
    /// The state model (`/StateModel`) the state belongs to.
    pub state_model: Option<String>,
    /// The annotation's name (`/NM`), when it carries one. XFDF replies
    /// reference their parent by this name.
    pub name: Option<String>,
    /// The points of each stroke of an ink annotation, in `/InkList` order.
    /// A stroke's point count is its length. Empty for every other subtype.
    pub ink_points: Vec<Vec<(f64, f64)>>,
}

/// What a write produced, beyond the bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnnotReport {
    /// The 1-based page the annotation went on.
    pub page: u32,
    /// The kind that was written.
    pub kind: &'static str,
    /// How many annotations the page carries afterwards.
    pub annots: usize,
    /// The input size in bytes.
    pub bytes_in: u64,
    /// The output size in bytes.
    pub bytes_out: u64,
}

/// What an appearance regeneration produced, beyond the bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppearanceReport {
    /// How many pages were walked.
    pub pages: usize,
    /// How many annotations were examined.
    pub annotations: usize,
    /// How many appearance streams were actually regenerated.
    pub regenerated: usize,
    /// Whether the document's AcroForm carries `/NeedAppearances true`, i.e.
    /// asks a viewer to rebuild **widget** appearances. This flag is reported,
    /// never cleared here: widget appearance synthesis belongs to the FORM
    /// layer (`selis-pdf-form`, via `synthesize_appearances`), and clearing it
    /// without writing those widget appearances would be a silent lie. See
    /// [`regenerate_appearances`].
    pub need_appearances: bool,
    /// The input size in bytes.
    pub bytes_in: u64,
    /// The output size in bytes.
    pub bytes_out: u64,
}

/// The review state a reader sets on a mark.
///
/// The PDF review state model names the states `Accepted` and `None`:
/// a mark the reader has dealt with, and a mark that is open again. The
/// editor calls these Resolved and Reopened.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReviewState {
    /// The mark has been dealt with: `/State /Accepted`.
    Resolved,
    /// The mark is open again: `/State /None`.
    Reopened,
}

impl ReviewState {
    /// The PDF name written into `/State`.
    #[must_use]
    pub const fn state_name(self) -> &'static str {
        match self {
            Self::Resolved => "Accepted",
            Self::Reopened => "None",
        }
    }
}

/// The annotation flags this crate writes: `Print` (4).
///
/// A mark the reader cannot print is a mark that does not exist for half the
/// people who will see the document.
const FLAG_PRINT: i64 = 4;

/// The default highlight colour: yellow, the colour every reader expects.
pub const HIGHLIGHT_COLOUR: [f64; 3] = [1.0, 0.92, 0.23];

/// The default underline colour: the ink itself.
pub const UNDERLINE_COLOUR: [f64; 3] = [0.05, 0.05, 0.12];

/// The default strikeout colour: the ink itself.
pub const STRIKEOUT_COLOUR: [f64; 3] = [0.05, 0.05, 0.12];

/// The default note colour: the yellow every reader expects.
pub const NOTE_COLOUR: [f64; 3] = [1.0, 0.92, 0.23];

/// The default ink colour: the ink itself.
pub const INK_COLOUR: [f64; 3] = [0.05, 0.05, 0.12];

/// The default square colour: the ink itself.
pub const SQUARE_COLOUR: [f64; 3] = [0.05, 0.05, 0.12];

/// Add one annotation to a page.
///
/// The write is an **incremental revision**: the bytes before it are preserved
/// verbatim, so a signature over an earlier revision stays verifiable.
///
/// # Budget
///
/// The caller's budget bounds the work: the document is parsed under it, the
/// walk of the page tree is charged, and the bytes written are charged. The
/// number of annotations on a page, the size of a comment and the number of
/// ink points are all bounded by this crate's own caps.
///
/// # Malformed Input
///
/// A document that cannot be parsed, or whose newest revision carries
/// `/Encrypt`, is refused with `OBJ_UNEXPECTED` or `MALFORMED`. A page that
/// does not exist is refused with `OBJ_UNEXPECTED`. A rectangle that is not
/// finite, or that is empty, is refused with `INVALID`. An annotation count
/// above the cap is refused with `LIMIT`.
pub fn annotate(
    src: &[u8],
    spec: &AnnotSpec,
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
) -> Result<(Vec<u8>, AnnotReport)> {
    validate(spec)?;
    let (doc, pages) = open(src, budget, g)?;
    let page_ref = page_reference(&pages, spec.page)?;
    let page_obj = resolve_ref(src, &doc, page_ref, budget, g).map_err(|e| {
        err!(
            Code::ObjUnexpected,
            during = "annot-page",
            detail = format!("the page cannot be read: {e}")
        )
    })?;
    let Obj::Dict(mut page_dict) = page_obj else {
        return Err(err!(
            Code::ObjUnexpected,
            during = "annot-page",
            detail = "the page is not a dictionary"
        ));
    };

    let existing = annotation_refs(src, &doc, &page_dict, budget, g)?;
    if existing.len() >= MAX_ANNOTS_PER_PAGE {
        return Err(err!(
            Code::MutationInvalid,
            during = "annot-page",
            detail = "the page already carries the most annotations this crate writes"
        ));
    }

    // An ink mark's rectangle is derived from its strokes so it always bounds
    // every point (with room for the stroke width). Other kinds keep the
    // caller's rectangle.
    let paths = if spec.kind == AnnotKind::Ink {
        ink_paths(spec)
    } else {
        Vec::new()
    };
    let rect = if spec.kind == AnnotKind::Ink {
        ink_rect(&paths, spec.width)?
    } else {
        spec.rect
    };

    // The appearance stream, then the annotation that points at it.
    let base = next_object_number(&doc);
    let ap_num = base;
    let annot_num = base.saturating_add(1);
    let (appearance, resources) = appearance_stream(spec, rect)?;
    let ap = Obj::Stream {
        dict: vec![
            (
                Bytes::copy_from_slice(b"Type"),
                Obj::Name(Bytes::copy_from_slice(b"XObject")),
            ),
            (
                Bytes::copy_from_slice(b"Subtype"),
                Obj::Name(Bytes::copy_from_slice(b"Form")),
            ),
            (Bytes::copy_from_slice(b"BBox"), rect_array(rect)),
            (Bytes::copy_from_slice(b"Resources"), resources),
            (
                Bytes::copy_from_slice(b"Length"),
                Obj::Int(i64::try_from(appearance.len()).unwrap_or(i64::MAX)),
            ),
        ],
        data: Bytes::copy_from_slice(&appearance),
    };

    let mut annot = vec![
        (
            Bytes::copy_from_slice(b"Type"),
            Obj::Name(Bytes::copy_from_slice(b"Annot")),
        ),
        (
            Bytes::copy_from_slice(b"Subtype"),
            Obj::Name(Bytes::copy_from_slice(spec.kind.subtype().as_bytes())),
        ),
        (Bytes::copy_from_slice(b"Rect"), rect_array(rect)),
        (Bytes::copy_from_slice(b"C"), colour_array(spec.colour)),
        (Bytes::copy_from_slice(b"F"), Obj::Int(FLAG_PRINT)),
        (Bytes::copy_from_slice(b"P"), Obj::Ref(page_ref)),
        (
            Bytes::copy_from_slice(b"AP"),
            Obj::Dict(vec![(
                Bytes::copy_from_slice(b"N"),
                Obj::Ref(Ref::new(ap_num, 0)),
            )]),
        ),
    ];
    if !spec.quads.is_empty() {
        annot.push((
            Bytes::copy_from_slice(b"QuadPoints"),
            quad_points(&spec.quads),
        ));
    }
    if !spec.contents.is_empty() {
        annot.push((
            Bytes::copy_from_slice(b"Contents"),
            text_string(&spec.contents),
        ));
    }
    if !spec.author.is_empty() {
        annot.push((Bytes::copy_from_slice(b"T"), text_string(&spec.author)));
    }
    if spec.kind == AnnotKind::Ink {
        // /InkList is an array of strokes, each an array of x/y numbers.
        annot.push((Bytes::copy_from_slice(b"InkList"), ink_list(&paths)));
        annot.push((Bytes::copy_from_slice(b"BS"), border(spec.width)));
    }
    // A name is what an XFDF reply references. Write one on every mark so
    // a conversation exported from this document stays a conversation.
    annot.push((
        Bytes::copy_from_slice(b"NM"),
        text_string(&format!("selis-{}", annot_num)),
    ));
    if let Some(index) = spec.reply_to {
        let parent = existing.get(index).copied().ok_or_else(|| {
            err!(
                Code::ObjUnexpected,
                during = "annot-reply",
                detail = format!(
                    "the page carries {} annotation(s); there is no #{} to reply to",
                    existing.len(),
                    index
                )
            )
        })?;
        annot.push((Bytes::copy_from_slice(b"IRT"), Obj::Ref(parent)));
    }

    // The page's own `/Annots`: ours is appended to whatever was there.
    let mut annots: Vec<Obj> = existing.iter().map(|r| Obj::Ref(*r)).collect();
    annots.push(Obj::Ref(Ref::new(annot_num, 0)));
    set_entry(&mut page_dict, b"Annots", Obj::Array(annots));

    let objects = vec![
        (ap_num, ap),
        (annot_num, Obj::Dict(annot)),
        (page_ref.num, Obj::Dict(page_dict)),
    ];
    let trailer = carried_trailer(&doc)?;
    let out = write_incremental_update(src, &objects, &trailer, budget, g).map_err(|e| {
        err!(
            Code::ObjUnexpected,
            during = "annot-write",
            detail = format!("the incremental revision failed: {e}")
        )
    })?;

    // Read-back: the page must carry the annotation, and the annotation must
    // carry the appearance that was drawn.
    let (check_doc, check_pages) = open(&out, budget, g)?;
    let check_page = page_reference(&check_pages, spec.page)?;
    let check_obj = resolve_ref(&out, &check_doc, check_page, budget, g).map_err(|e| {
        err!(
            Code::ObjUnexpected,
            during = "annot-verify",
            detail = format!("the written page cannot be read: {e}")
        )
    })?;
    let Obj::Dict(check_dict) = check_obj else {
        return Err(err!(
            Code::ObjUnexpected,
            during = "annot-verify",
            detail = "the written page is not a dictionary"
        ));
    };
    let written = annotation_refs(&out, &check_doc, &check_dict, budget, g)?;
    if written.len() != existing.len() + 1 {
        return Err(err!(
            Code::ObjUnexpected,
            during = "annot-verify",
            detail = "the written page does not carry the annotation"
        ));
    }

    let bytes_in = u64::try_from(src.len()).unwrap_or(u64::MAX);
    let bytes_out = u64::try_from(out.len()).unwrap_or(u64::MAX);
    Ok((
        out,
        AnnotReport {
            page: spec.page,
            kind: spec.kind.subtype(),
            annots: written.len(),
            bytes_in,
            bytes_out,
        },
    ))
}

/// The annotations a page carries, in `/Annots` order.
///
/// # Budget
///
/// The caller's budget bounds the parse and the walk: every object resolved
/// is charged, and the number of annotations read is capped.
///
/// # Malformed Input
///
/// A document that cannot be parsed is refused with `MALFORMED`. A page that
/// does not exist is refused with `OBJ_UNEXPECTED`. An annotation that is
/// not a dictionary is **skipped**, not refused: a document with one broken
/// annotation still has the others, and refusing the whole page would hide
/// them.
pub fn list_annotations(
    src: &[u8],
    page: u32,
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
) -> Result<Vec<PageAnnotation>> {
    let (doc, pages) = open(src, budget, g)?;
    let page_ref = page_reference(&pages, page)?;
    let page_obj = resolve_ref(src, &doc, page_ref, budget, g).map_err(|e| {
        err!(
            Code::ObjUnexpected,
            during = "annot-list",
            detail = format!("the page cannot be read: {e}")
        )
    })?;
    let Obj::Dict(page_dict) = page_obj else {
        return Err(err!(
            Code::ObjUnexpected,
            during = "annot-list",
            detail = "the page is not a dictionary"
        ));
    };
    let refs = annotation_refs(src, &doc, &page_dict, budget, g)?;
    let mut out = Vec::new();
    for &annot_ref in &refs {
        let Ok(Obj::Dict(pairs)) = resolve_ref(src, &doc, annot_ref, budget, g) else {
            continue;
        };
        let subtype = name_of(&pairs, b"Subtype").unwrap_or_default();
        let rect = array4_of(&pairs, b"Rect").unwrap_or([0.0, 0.0, 0.0, 0.0]);
        let colour = colour_of(&pairs).unwrap_or([0.0, 0.0, 0.0]);
        let contents = text_of(&pairs, b"Contents");
        let author = text_of(&pairs, b"T");
        let quads = quads_of(&pairs);
        // An /IRT names the annotation being replied to by REFERENCE; the
        // report lists annotations by their position in /Annots, so resolve
        // one into the other for the caller.
        let irt = match name_ref_of(&pairs, b"IRT") {
            Some(target) => refs.iter().position(|candidate| *candidate == target),
            None => None,
        };
        let state = name_of(&pairs, b"State");
        let state_model = name_of(&pairs, b"StateModel");
        let name = text_of(&pairs, b"NM").into();
        let ink_points = ink_paths_of(&pairs);
        out.push(PageAnnotation {
            subtype,
            rect,
            colour,
            contents,
            author,
            quads,
            irt,
            state,
            state_model,
            name,
            ink_points,
        });
    }
    Ok(out)
}

/// Take one annotation off a page.
///
/// The index is the annotation's position in the page's `/Annots` order, as
/// [`list_annotations`] reports it.
///
/// # Budget
///
/// The caller's budget bounds the parse, the walk and the bytes written.
///
/// # Malformed Input
///
/// A document that cannot be parsed is refused with `MALFORMED`. A page that
/// does not exist, or an index the page does not carry, is refused with
/// `OBJ_UNEXPECTED`.
pub fn remove_annotation(
    src: &[u8],
    page: u32,
    index: usize,
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
) -> Result<(Vec<u8>, AnnotReport)> {
    let (doc, pages) = open(src, budget, g)?;
    let page_ref = page_reference(&pages, page)?;
    let page_obj = resolve_ref(src, &doc, page_ref, budget, g).map_err(|e| {
        err!(
            Code::ObjUnexpected,
            during = "annot-remove",
            detail = format!("the page cannot be read: {e}")
        )
    })?;
    let Obj::Dict(mut page_dict) = page_obj else {
        return Err(err!(
            Code::ObjUnexpected,
            during = "annot-remove",
            detail = "the page is not a dictionary"
        ));
    };
    let refs = annotation_refs(src, &doc, &page_dict, budget, g)?;
    if index >= refs.len() {
        return Err(err!(
            Code::ObjUnexpected,
            during = "annot-remove",
            detail = format!(
                "the page carries {} annotation(s); there is no #{index}",
                refs.len()
            )
        ));
    }
    let mut kept = refs;
    kept.remove(index);
    let kept_objects: Vec<Obj> = kept.iter().map(|r| Obj::Ref(*r)).collect();
    set_entry(&mut page_dict, b"Annots", Obj::Array(kept_objects));
    let objects = vec![(page_ref.num, Obj::Dict(page_dict))];
    let trailer = carried_trailer(&doc)?;
    let out = write_incremental_update(src, &objects, &trailer, budget, g).map_err(|e| {
        err!(
            Code::ObjUnexpected,
            during = "annot-remove",
            detail = format!("the incremental revision failed: {e}")
        )
    })?;
    let bytes_in = u64::try_from(src.len()).unwrap_or(u64::MAX);
    let bytes_out = u64::try_from(out.len()).unwrap_or(u64::MAX);
    Ok((
        out,
        AnnotReport {
            page,
            kind: "removed",
            annots: kept.len(),
            bytes_in,
            bytes_out,
        },
    ))
}

/// Set a mark's review state, and write the verified result to the caller.
///
/// A review state is the PDF `/State` and `/StateModel` pair: `Accepted` or
/// `None` in the `Review` model. The editor shows these as Resolved and
/// Reopened; a reader in another viewer sees the state too, because it is a
/// dictionary entry on the annotation, not a note in the editor's own panel.
///
/// # Budget
///
/// The caller's budget bounds the parse, the walk and the bytes written.
///
/// # Malformed Input
///
/// A document that cannot be parsed is refused with `MALFORMED`. A page that
/// does not exist, or an index the page does not carry, is refused with
/// `OBJ_UNEXPECTED`.
pub fn set_review_state(
    src: &[u8],
    page: u32,
    index: usize,
    state: ReviewState,
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
) -> Result<(Vec<u8>, AnnotReport)> {
    let (doc, pages) = open(src, budget, g)?;
    let page_ref = page_reference(&pages, page)?;
    let page_obj = resolve_ref(src, &doc, page_ref, budget, g).map_err(|e| {
        err!(
            Code::ObjUnexpected,
            during = "annot-state",
            detail = format!("the page cannot be read: {e}")
        )
    })?;
    let Obj::Dict(page_dict) = page_obj else {
        return Err(err!(
            Code::ObjUnexpected,
            during = "annot-state",
            detail = "the page is not a dictionary"
        ));
    };
    let refs = annotation_refs(src, &doc, &page_dict, budget, g)?;
    let Some(annot_ref) = refs.get(index).copied() else {
        return Err(err!(
            Code::ObjUnexpected,
            during = "annot-state",
            detail = format!(
                "the page carries {} annotation(s); there is no #{index}",
                refs.len()
            )
        ));
    };
    let annot_obj = resolve_ref(src, &doc, annot_ref, budget, g).map_err(|e| {
        err!(
            Code::ObjUnexpected,
            during = "annot-state",
            detail = format!("the annotation cannot be read: {e}")
        )
    })?;
    let Obj::Dict(mut annot) = annot_obj else {
        return Err(err!(
            Code::ObjUnexpected,
            during = "annot-state",
            detail = "the annotation is not a dictionary"
        ));
    };
    set_entry(
        &mut annot,
        b"State",
        Obj::Name(Bytes::copy_from_slice(state.state_name().as_bytes())),
    );
    set_entry(
        &mut annot,
        b"StateModel",
        Obj::Name(Bytes::copy_from_slice(b"Review")),
    );
    let objects = vec![(annot_ref.num, Obj::Dict(annot))];
    let trailer = carried_trailer(&doc)?;
    let out = write_incremental_update(src, &objects, &trailer, budget, g).map_err(|e| {
        err!(
            Code::ObjUnexpected,
            during = "annot-state",
            detail = format!("the incremental revision failed: {e}")
        )
    })?;
    let bytes_in = u64::try_from(src.len()).unwrap_or(u64::MAX);
    let bytes_out = u64::try_from(out.len()).unwrap_or(u64::MAX);
    Ok((
        out,
        AnnotReport {
            page,
            kind: "state",
            annots: refs.len(),
            bytes_in,
            bytes_out,
        },
    ))
}

/**
 * Move an annotation, or change what it says.
 *
 * The annotation is addressed by its position in the page's `/Annots` order,
 * exactly as `list_annotations` reports it: an annotation has no name of its
 * own that a reader could use, and a name we invented would be a name the
 * reader never sees.
 *
 * # Budget
 *
 * The caller's budget bounds the parse, the walk and the bytes written.
 *
 * # Malformed Input
 *
 * A document that cannot be parsed is refused with `OBJ_UNEXPECTED`. A page
 * that does not exist, or an index the page does not carry, is refused with
 * `OBJ_UNEXPECTED`. A rectangle that is not a rectangle is refused with
 * `MUTATION_INVALID`.
 */
pub fn update_annotation(
    src: &[u8],
    page: u32,
    index: usize,
    rect: Option<[f64; 4]>,
    contents: Option<String>,
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
) -> Result<(Vec<u8>, AnnotReport)> {
    let (doc, pages) = open(src, budget, g)?;
    let page_ref = page_reference(&pages, page)?;
    let page_obj = resolve_ref(src, &doc, page_ref, budget, g).map_err(|e| {
        err!(
            Code::ObjUnexpected,
            during = "annot-update",
            detail = format!("the page cannot be read: {e}")
        )
    })?;
    let Obj::Dict(page_dict) = page_obj else {
        return Err(err!(
            Code::ObjUnexpected,
            during = "annot-update",
            detail = "the page is not a dictionary"
        ));
    };
    let refs = annotation_refs(src, &doc, &page_dict, budget, g)?;
    let Some(annot_ref) = refs.get(index).copied() else {
        return Err(err!(
            Code::ObjUnexpected,
            during = "annot-update",
            detail = format!(
                "the page carries {} annotation(s); there is no #{index}",
                refs.len()
            )
        ));
    };
    let annot_obj = resolve_ref(src, &doc, annot_ref, budget, g).map_err(|e| {
        err!(
            Code::ObjUnexpected,
            during = "annot-update",
            detail = format!("the annotation cannot be read: {e}")
        )
    })?;
    let Obj::Dict(mut annot) = annot_obj else {
        return Err(err!(
            Code::ObjUnexpected,
            during = "annot-update",
            detail = "the annotation is not a dictionary"
        ));
    };

    if let Some(rect) = rect {
        for value in rect {
            if !value.is_finite() {
                return Err(err!(
                    Code::MutationInvalid,
                    during = "annot-update",
                    detail = "the rectangle is not a finite rectangle"
                ));
            }
        }
        if rect[2] < rect[0] || rect[3] < rect[1] {
            return Err(err!(
                Code::MutationInvalid,
                during = "annot-update",
                detail = "the rectangle is the wrong way round"
            ));
        }
        set_entry(&mut annot, b"Rect", rect_array(rect));
        // The quads follow the rectangle: a mark that stayed where it was
        // would cover the wrong words.
        if let Some(quads) = Some(quads_of(&annot)) {
            if !quads.is_empty() {
                let moved: Vec<[f64; 4]> = quads
                    .iter()
                    .map(|quad| {
                        let dx = rect[0] - quad[0];
                        let dy = rect[1] - quad[1];
                        [quad[0] + dx, quad[1] + dy, quad[2] + dx, quad[3] + dy]
                    })
                    .collect();
                set_entry(&mut annot, b"QuadPoints", quad_points(&moved));
            }
        }
    }
    if let Some(contents) = contents {
        if contents.chars().count() > MAX_COMMENT_CHARS {
            return Err(err!(
                Code::MutationInvalid,
                during = "annot-update",
                detail = "the comment is too long"
            ));
        }
        if contents.is_empty() {
            annot.retain(|(key, _)| key.as_slice() != b"Contents");
        } else {
            set_entry(&mut annot, b"Contents", text_string(&contents));
        }
    }

    let objects = vec![(annot_ref.num, Obj::Dict(annot))];
    let trailer = carried_trailer(&doc)?;
    let out = write_incremental_update(src, &objects, &trailer, budget, g).map_err(|e| {
        err!(
            Code::ObjUnexpected,
            during = "annot-update",
            detail = format!("the incremental revision failed: {e}")
        )
    })?;
    let bytes_in = u64::try_from(src.len()).unwrap_or(u64::MAX);
    let bytes_out = u64::try_from(out.len()).unwrap_or(u64::MAX);
    Ok((
        out,
        AnnotReport {
            page,
            kind: "updated",
            annots: refs.len(),
            bytes_in,
            bytes_out,
        },
    ))
}

/// Regenerate the appearance streams of annotations that are missing one or
/// carry a stale one, and return the verified incremental revision.
///
/// An appearance is **stale** when its `/AP /N` is absent, unreadable, empty,
/// or a Form XObject whose `/BBox` no longer matches the annotation's `/Rect`
/// (the case after a move). Subtypes this crate does not draw — widgets and
/// unknown `/Subtype`s — are left untouched: they are not markup this engine
/// can honestly regenerate.
///
/// Every regenerated stream is built by the same [`appearance_stream`] path
/// the writer uses, so the bytes are deterministic and match a freshly written
/// mark. Each annotation's `/AP` is repointed at a fresh Form XObject; the
/// write is one incremental revision whose original bytes remain a verbatim
/// prefix, and the result is re-opened and checked stream by stream before it
/// is returned. Any failure is a typed error, never a silent "looks fine".
///
/// # `/NeedAppearances`
///
/// A document's AcroForm may carry `/NeedAppearances true`, asking a viewer to
/// rebuild **widget** appearances. That flag is reported in
/// [`AppearanceReport::need_appearances`] and is deliberately left set: widget
/// appearance synthesis belongs to `selis-pdf-form` (its
/// `FillFormsOptions::synthesize_appearances` path), and clearing the flag here
/// without writing those widget appearances would claim work that was not
/// done. This function only ever regenerates markup annotations; it never sets
/// `/NeedAppearances` to make a viewer do markup work either, because it writes
/// real `/AP` streams instead.
///
/// # Budget
///
/// The caller's budget bounds the parse, the walk and the bytes written.
///
/// # Malformed Input
///
/// A document that cannot be parsed, or that is encrypted, is refused with
/// `OBJ_UNEXPECTED`. A supported markup subtype whose rectangle cannot be read
/// or whose geometry fails validation is a typed error, not a skipped mark.
pub fn regenerate_appearances(
    src: &[u8],
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
) -> Result<(Vec<u8>, AppearanceReport)> {
    let (doc, pages) = open(src, budget, g)?;
    let need_appearances = acroform_needs_appearances(src, &doc, budget, g)?;
    let bytes_in = u64::try_from(src.len()).unwrap_or(u64::MAX);
    let report_base = |examined, regenerated, bytes_out| AppearanceReport {
        pages: pages.len(),
        annotations: examined,
        regenerated,
        need_appearances,
        bytes_in,
        bytes_out,
    };

    let mut objects: Vec<(u32, Obj)> = Vec::new();
    // (annotation number, appearance number, rect, appearance bytes) — the
    // contract checked against the re-opened output below.
    let mut expected: Vec<(u32, u32, [f64; 4], Vec<u8>)> = Vec::new();
    let mut next_num = next_object_number(&doc);
    let mut examined = 0usize;
    let mut regenerated = 0usize;

    for &page_ref in &pages {
        let page_obj = resolve_ref(src, &doc, page_ref, budget, g).map_err(|e| {
            err!(
                Code::ObjUnexpected,
                during = "annot-regen",
                detail = format!("a page cannot be read: {e}")
            )
        })?;
        let Obj::Dict(page_dict) = page_obj else {
            return Err(err!(
                Code::ObjUnexpected,
                during = "annot-regen",
                detail = "a page is not a dictionary"
            ));
        };
        for annot_ref in annotation_refs(src, &doc, &page_dict, budget, g)? {
            examined = examined.saturating_add(1);
            let Ok(Obj::Dict(pairs)) = resolve_ref(src, &doc, annot_ref, budget, g) else {
                continue;
            };
            let Some(kind) =
                name_of(&pairs, b"Subtype").and_then(|subtype| AnnotKind::from_subtype(&subtype))
            else {
                // Widgets and subtypes we cannot draw are not markup to us.
                continue;
            };
            let rect = array4_of(&pairs, b"Rect").ok_or_else(|| {
                err!(
                    Code::ObjUnexpected,
                    during = "annot-regen",
                    detail = format!("annotation {annot_ref:?} has no readable /Rect")
                )
            })?;
            if ap_is_current(src, &doc, &pairs, rect, budget, g)? {
                continue;
            }

            // Rebuild the spec from what the annotation says, then draw it
            // through the exact path the writer uses.
            let paths = ink_paths_of(&pairs);
            let mut spec =
                AnnotSpec::new(kind, 1, rect, colour_of(&pairs).unwrap_or([0.0, 0.0, 0.0]));
            spec.quads = quads_of(&pairs);
            spec.contents = text_of(&pairs, b"Contents");
            spec.author = text_of(&pairs, b"T");
            spec.width = border_width_of(&pairs);
            spec.paths = paths.clone();
            validate(&spec).map_err(|e| {
                err!(
                    Code::ObjUnexpected,
                    during = "annot-regen",
                    detail = format!("annotation {annot_ref:?} cannot be regenerated: {e}")
                )
            })?;
            let rect = if kind == AnnotKind::Ink {
                ink_rect(&paths, spec.width)?
            } else {
                spec.rect
            };
            let (appearance, resources) = appearance_stream(&spec, rect)?;

            let ap_num = next_num;
            next_num = next_num.saturating_add(1);
            let ap = Obj::Stream {
                dict: vec![
                    (
                        Bytes::copy_from_slice(b"Type"),
                        Obj::Name(Bytes::copy_from_slice(b"XObject")),
                    ),
                    (
                        Bytes::copy_from_slice(b"Subtype"),
                        Obj::Name(Bytes::copy_from_slice(b"Form")),
                    ),
                    (Bytes::copy_from_slice(b"BBox"), rect_array(rect)),
                    (Bytes::copy_from_slice(b"Resources"), resources),
                    (
                        Bytes::copy_from_slice(b"Length"),
                        Obj::Int(i64::try_from(appearance.len()).unwrap_or(i64::MAX)),
                    ),
                ],
                data: Bytes::copy_from_slice(&appearance),
            };

            let mut updated = pairs.clone();
            set_entry(
                &mut updated,
                b"AP",
                Obj::Dict(vec![(
                    Bytes::copy_from_slice(b"N"),
                    Obj::Ref(Ref::new(ap_num, 0)),
                )]),
            );
            set_entry(&mut updated, b"Rect", rect_array(rect));
            if kind == AnnotKind::Ink {
                set_entry(&mut updated, b"InkList", ink_list(&paths));
                set_entry(&mut updated, b"BS", border(spec.width));
            }

            objects.push((ap_num, ap));
            objects.push((annot_ref.num, Obj::Dict(updated)));
            expected.push((annot_ref.num, ap_num, rect, appearance));
            regenerated = regenerated.saturating_add(1);
        }
    }

    if objects.is_empty() {
        return Ok((src.to_vec(), report_base(examined, 0, bytes_in)));
    }

    let out = write_incremental_update(src, &objects, &carried_trailer(&doc)?, budget, g).map_err(
        |e| {
            err!(
                Code::ObjUnexpected,
                during = "annot-regen",
                detail = format!("the incremental revision failed: {e}")
            )
        },
    )?;
    if out.get(..src.len()) != Some(src) {
        return Err(err!(
            Code::ObjUnexpected,
            during = "annot-regen",
            detail = "the original bytes are not a verbatim prefix"
        ));
    }

    // Read-back: every regenerated annotation must now point at a Form
    // XObject with the rectangle we wrote and the exact bytes we drew.
    let (check_doc, _) = open(&out, budget, g)?;
    for (annot_num, ap_num, rect, appearance) in &expected {
        let annot_obj =
            resolve_ref(&out, &check_doc, Ref::new(*annot_num, 0), budget, g).map_err(|e| {
                err!(
                    Code::ObjUnexpected,
                    during = "annot-regen-verify",
                    detail = format!("a regenerated annotation cannot be read: {e}")
                )
            })?;
        let Obj::Dict(pairs) = annot_obj else {
            return Err(err!(
                Code::ObjUnexpected,
                during = "annot-regen-verify",
                detail = "a regenerated annotation is not a dictionary"
            ));
        };
        let wanted = Obj::Dict(vec![(
            Bytes::copy_from_slice(b"N"),
            Obj::Ref(Ref::new(*ap_num, 0)),
        )]);
        if pairs
            .iter()
            .find(|(key, _)| key.as_slice() == b"AP")
            .map(|(_, value)| value)
            != Some(&wanted)
        {
            return Err(err!(
                Code::ObjUnexpected,
                during = "annot-regen-verify",
                detail = "a regenerated annotation does not carry the new /AP"
            ));
        }
        let ap_obj =
            resolve_ref(&out, &check_doc, Ref::new(*ap_num, 0), budget, g).map_err(|e| {
                err!(
                    Code::ObjUnexpected,
                    during = "annot-regen-verify",
                    detail = format!("a regenerated appearance cannot be read: {e}")
                )
            })?;
        let Obj::Stream { dict, data } = ap_obj else {
            return Err(err!(
                Code::ObjUnexpected,
                during = "annot-regen-verify",
                detail = "a regenerated appearance is not a stream"
            ));
        };
        if array4_of(&dict, b"BBox") != Some(*rect) || data.as_slice() != appearance.as_slice() {
            return Err(err!(
                Code::ObjUnexpected,
                during = "annot-regen-verify",
                detail = "a regenerated appearance does not match what was drawn"
            ));
        }
    }

    let bytes_out = u64::try_from(out.len()).unwrap_or(u64::MAX);
    Ok((out, report_base(examined, regenerated, bytes_out)))
}

/// Does an annotation already carry a current appearance stream?
fn ap_is_current(
    src: &[u8],
    doc: &selis_pdf_cos::Doc,
    pairs: &[(Bytes, Obj)],
    rect: [f64; 4],
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
) -> Result<bool> {
    let Some(ap) = pairs
        .iter()
        .find(|(key, _)| key.as_slice() == b"AP")
        .and_then(|(_, value)| match value {
            Obj::Dict(entries) => Some(entries),
            _ => None,
        })
    else {
        return Ok(false);
    };
    let Some(normal) = ap
        .iter()
        .find(|(key, _)| key.as_slice() == b"N")
        .map(|(_, value)| value)
    else {
        return Ok(false);
    };
    let current = match normal {
        Obj::Stream { dict, data } => stream_matches(dict, data, rect),
        Obj::Ref(reference) => match resolve_ref(src, doc, *reference, budget, g) {
            Ok(Obj::Stream { dict, data }) => stream_matches(&dict, &data, rect),
            _ => false,
        },
        _ => false,
    };
    Ok(current)
}

/// A Form XObject is current when it has bytes and its `/BBox` still matches
/// the annotation's rectangle (within the 3-decimal precision this crate
/// writes).
fn stream_matches(dict: &[(Bytes, Obj)], data: &selis_bytes::Bytes, rect: [f64; 4]) -> bool {
    if data.is_empty() {
        return false;
    }
    let Some(bbox) = array4_of(dict, b"BBox") else {
        return false;
    };
    (0..4).all(|i| (bbox[i] - rect[i]).abs() <= 0.01)
}

/// Whether the document's AcroForm asks readers to regenerate widget
/// appearances (`/NeedAppearances true`). Direct and indirect `/AcroForm`
/// values are both handled.
fn acroform_needs_appearances(
    src: &[u8],
    doc: &selis_pdf_cos::Doc,
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
) -> Result<bool> {
    let view = doc
        .at_revision(doc.len().saturating_sub(1))
        .ok_or_else(|| {
            err!(
                Code::ObjUnexpected,
                during = "annot-regen-form",
                detail = "the document has no revisions"
            )
        })?;
    let root = view.root.ok_or_else(|| {
        err!(
            Code::ObjUnexpected,
            during = "annot-regen-form",
            detail = "the document has no /Root"
        )
    })?;
    let Obj::Dict(catalog) = resolve_ref(src, doc, root, budget, g).map_err(|e| {
        err!(
            Code::ObjUnexpected,
            during = "annot-regen-form",
            detail = format!("the catalog cannot be read: {e}")
        )
    })?
    else {
        return Ok(false);
    };
    let Some(form) = catalog
        .iter()
        .find(|(key, _)| key.as_slice() == b"AcroForm")
        .map(|(_, value)| value)
    else {
        return Ok(false);
    };
    let dict = match form {
        Obj::Dict(entries) => entries.clone(),
        Obj::Ref(reference) => match resolve_ref(src, doc, *reference, budget, g) {
            Ok(Obj::Dict(entries)) => entries,
            _ => return Ok(false),
        },
        _ => return Ok(false),
    };
    Ok(dict
        .iter()
        .any(|(key, value)| key.as_slice() == b"NeedAppearances" && *value == Obj::Bool(true)))
}

/// The border width an annotation asks for, defaulting to 1 point.
fn border_width_of(pairs: &[(Bytes, Obj)]) -> f64 {
    pairs
        .iter()
        .find(|(key, _)| key.as_slice() == b"BS")
        .and_then(|(_, value)| match value {
            Obj::Dict(entries) => entries
                .iter()
                .find(|(key, _)| key.as_slice() == b"W")
                .and_then(|(_, width)| number(width)),
            _ => None,
        })
        .filter(|width| width.is_finite() && *width >= 0.0)
        .unwrap_or(1.0)
}

/// The appearance stream for a markup, drawn into `rect`, and the resources it
/// draws with. `rect` is the annotation rectangle; for ink it is derived from
/// the strokes by [`ink_rect`] so nothing is clipped.
fn appearance_stream(spec: &AnnotSpec, rect: [f64; 4]) -> Result<(Vec<u8>, Obj)> {
    let (x0, y0, x1, y1) = (rect[0], rect[1], rect[2], rect[3]);
    let width = x1 - x0;
    let height = y1 - y0;
    let mut content = String::new();
    let mut resources = Vec::new();

    match spec.kind {
        AnnotKind::Highlight => {
            // The mark is translucent, so the text under it still reads: a
            // highlight that hides the words is not a highlight.
            content.push_str("/GS1 gs\n");
            for quad in &spec.quads {
                content.push_str(&format!(
                    "{} {} {} {} re f\n",
                    num(quad[0]),
                    num(quad[1]),
                    num(quad[2] - quad[0]),
                    num(quad[3] - quad[1])
                ));
            }
            resources.push((
                Bytes::copy_from_slice(b"ExtGState"),
                Obj::Dict(vec![(
                    Bytes::copy_from_slice(b"GS1"),
                    Obj::Dict(vec![
                        (Bytes::copy_from_slice(b"ca"), real(0.55)),
                        (Bytes::copy_from_slice(b"CA"), real(0.55)),
                        (
                            Bytes::copy_from_slice(b"Type"),
                            Obj::Name(Bytes::copy_from_slice(b"ExtGState")),
                        ),
                    ]),
                )]),
            ));
        }
        AnnotKind::Underline => {
            content.push_str(&format!("{} w\n", num(spec.width.max(0.5))));
            for quad in &spec.quads {
                let y = quad[1] + (quad[3] - quad[1]) * 0.08;
                content.push_str(&format!(
                    "{} {} m\n{} {} l\nS\n",
                    num(quad[0]),
                    num(y),
                    num(quad[2]),
                    num(y)
                ));
            }
        }
        AnnotKind::StrikeOut => {
            content.push_str(&format!("{} w\n", num(spec.width.max(0.5))));
            for quad in &spec.quads {
                let y = (quad[1] + quad[3]) / 2.0;
                content.push_str(&format!(
                    "{} {} m\n{} {} l\nS\n",
                    num(quad[0]),
                    num(y),
                    num(quad[2]),
                    num(y)
                ));
            }
        }
        AnnotKind::Text => {
            // The folded-page icon every reader knows.
            let fold = (width.min(height) * 0.4).max(1.0);
            content.push_str(&format!(
                "{} {} {} {} re f\n",
                num(x0),
                num(y0),
                num(width),
                num(height)
            ));
            // The fold itself, in the page's own white.
            content.push_str(&format!(
                "1 1 1 rg\n{} {} m\n{} {} l\n{} {} l\nf\n",
                num(x1 - fold),
                num(y1),
                num(x1),
                num(y1 - fold),
                num(x1),
                num(y1)
            ));
        }
        AnnotKind::Ink => {
            // One subpath per stroke, round caps and joins. The caller supplies
            // plain x/y points: geometry is pressure-agnostic by design, so
            // the same strokes appear in every viewer (ANNOT.03).
            content.push_str(&format!("{} w\n1 J\n1 j\n", num(spec.width.max(0.5))));
            for path in ink_paths(spec) {
                let mut points = path.iter();
                if let Some((px, py)) = points.next() {
                    content.push_str(&format!("{} {} m\n", num(*px), num(*py)));
                    for (x, y) in points {
                        content.push_str(&format!("{} {} l\n", num(*x), num(*y)));
                    }
                    content.push_str("S\n");
                }
            }
        }
        AnnotKind::Square => {
            content.push_str(&format!("{} w\n", num(spec.width.max(0.5))));
            content.push_str(&format!(
                "{} {} {} {} re S\n",
                num(x0),
                num(y0),
                num(width),
                num(height)
            ));
        }
    }

    let colour = format!(
        "{} {} {} rg\n",
        num(spec.colour[0]),
        num(spec.colour[1]),
        num(spec.colour[2])
    );
    let body = format!("{colour}{content}");
    Ok((body.into_bytes(), Obj::Dict(resources)))
}

/// Refuse a markup that cannot be drawn.
fn validate(spec: &AnnotSpec) -> Result<()> {
    if spec.page == 0 {
        return Err(err!(
            Code::MutationInvalid,
            during = "annot-validate",
            detail = "a page is 1-based; page 0 does not exist"
        ));
    }
    for value in spec.rect {
        if !value.is_finite() {
            return Err(err!(
                Code::MutationInvalid,
                during = "annot-validate",
                detail = "the rectangle is not a finite rectangle"
            ));
        }
    }
    if spec.rect[2] < spec.rect[0] || spec.rect[3] < spec.rect[1] {
        return Err(err!(
            Code::MutationInvalid,
            during = "annot-validate",
            detail = "the rectangle is the wrong way round"
        ));
    }
    if spec.quads.len() > MAX_QUADS {
        return Err(err!(
            Code::MutationInvalid,
            during = "annot-validate",
            detail = "the markup covers too many lines"
        ));
    }
    if spec.contents.chars().count() > MAX_COMMENT_CHARS {
        return Err(err!(
            Code::MutationInvalid,
            during = "annot-validate",
            detail = "the comment is too long"
        ));
    }
    if spec.author.chars().count() > MAX_AUTHOR_CHARS {
        return Err(err!(
            Code::MutationInvalid,
            during = "annot-validate",
            detail = "the author name is too long"
        ));
    }
    if !spec.width.is_finite() || spec.width < 0.0 {
        return Err(err!(
            Code::MutationInvalid,
            during = "annot-validate",
            detail = "the stroke width is not a finite, non-negative width"
        ));
    }
    let paths = ink_paths(spec);
    if paths.len() > MAX_INK_PATHS {
        return Err(err!(
            Code::MutationInvalid,
            during = "annot-validate",
            detail = "the drawn mark has too many strokes"
        ));
    }
    let mut total = 0usize;
    for path in &paths {
        if path.is_empty() {
            return Err(err!(
                Code::MutationInvalid,
                during = "annot-validate",
                detail = "an ink stroke is empty"
            ));
        }
        if path.len() > MAX_INK_POINTS_PER_PATH {
            return Err(err!(
                Code::MutationInvalid,
                during = "annot-validate",
                detail = "one stroke of the drawn mark has too many points"
            ));
        }
        total = total.saturating_add(path.len());
        for (x, y) in path {
            if !x.is_finite() || !y.is_finite() {
                return Err(err!(
                    Code::MutationInvalid,
                    during = "annot-validate",
                    detail = "the drawn mark contains a non-finite point"
                ));
            }
        }
    }
    if total > MAX_INK_POINTS {
        return Err(err!(
            Code::MutationInvalid,
            during = "annot-validate",
            detail = "the drawn mark has too many points"
        ));
    }
    if spec.kind == AnnotKind::Ink && total == 0 {
        return Err(err!(
            Code::MutationInvalid,
            during = "annot-validate",
            detail = "an ink annotation needs at least one point"
        ));
    }
    for colour in spec.colour {
        if !colour.is_finite() || !(0.0..=1.0).contains(&colour) {
            return Err(err!(
                Code::MutationInvalid,
                during = "annot-validate",
                detail = "the colour is not a colour between 0 and 1"
            ));
        }
    }
    Ok(())
}

/// Open the document and its pages.
fn open(
    src: &[u8],
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
) -> Result<(selis_pdf_cos::Doc, Vec<Ref>)> {
    let startxref = selis_pdf_cos::xref::find_startxref(src, 4096).unwrap_or(0);
    let doc = selis_pdf_cos::parse_revisions(src, startxref, budget, g).map_err(|e| {
        err!(
            Code::ObjUnexpected,
            during = "annot-open",
            detail = format!("the document cannot be opened: {e}")
        )
    })?;
    if doc.revisions().last().is_some_and(|r| r.encrypt.is_some()) {
        return Err(err!(
            Code::ObjUnexpected,
            during = "annot-open",
            detail = "the document is encrypted; remove the password first"
        ));
    }
    let pages = page_refs(src, &doc, budget, g)?;
    Ok((doc, pages))
}

/// The page references, in reading order.
fn page_refs(
    src: &[u8],
    doc: &selis_pdf_cos::Doc,
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
) -> Result<Vec<Ref>> {
    let view = doc
        .at_revision(doc.len().saturating_sub(1))
        .ok_or_else(|| {
            err!(
                Code::ObjUnexpected,
                during = "annot-pages",
                detail = "the document has no revisions"
            )
        })?;
    let root = view.root.ok_or_else(|| {
        err!(
            Code::ObjUnexpected,
            during = "annot-pages",
            detail = "the document has no /Root"
        )
    })?;
    let catalog = resolve_ref(src, doc, root, budget, g).map_err(|e| {
        err!(
            Code::ObjUnexpected,
            during = "annot-pages",
            detail = format!("the catalog cannot be read: {e}")
        )
    })?;
    let Obj::Dict(pairs) = catalog else {
        return Err(err!(
            Code::ObjUnexpected,
            during = "annot-pages",
            detail = "the catalog is not a dictionary"
        ));
    };
    let pages_ref = pairs
        .iter()
        .find(|(k, _)| k.as_slice() == b"Pages")
        .and_then(|(_, v)| match v {
            Obj::Ref(r) => Some(*r),
            _ => None,
        });
    let pages_ref = pages_ref.ok_or_else(|| {
        err!(
            Code::ObjUnexpected,
            during = "annot-pages",
            detail = "the catalog has no /Pages"
        )
    })?;
    let mut out = Vec::new();
    collect_pages(src, doc, pages_ref, &mut out, 0, budget, g)?;
    Ok(out)
}

/// Walk the page tree, collecting the leaves.
fn collect_pages(
    src: &[u8],
    doc: &selis_pdf_cos::Doc,
    node: Ref,
    out: &mut Vec<Ref>,
    depth: u32,
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
) -> Result<()> {
    if depth > 64 {
        return Ok(());
    }
    if out.len() >= MAX_PAGES {
        return Ok(());
    }
    let Ok(Obj::Dict(pairs)) = resolve_ref(src, doc, node, budget, g) else {
        return Ok(());
    };
    let kids = pairs
        .iter()
        .find(|(k, _)| k.as_slice() == b"Kids")
        .and_then(|(_, v)| match v {
            Obj::Array(items) => Some(items.clone()),
            _ => None,
        });
    let kind = name_of(&pairs, b"Type").unwrap_or_default();
    if kind == "Pages" {
        if let Some(items) = kids {
            for kid in items {
                if let Obj::Ref(kid_ref) = kid {
                    collect_pages(src, doc, kid_ref, out, depth.saturating_add(1), budget, g)?;
                }
            }
        }
        return Ok(());
    }
    out.push(node);
    Ok(())
}

/// The page a 1-based number names.
fn page_reference(pages: &[Ref], page: u32) -> Result<Ref> {
    pages
        .get(page.saturating_sub(1) as usize)
        .copied()
        .ok_or_else(|| {
            err!(
                Code::ObjUnexpected,
                during = "annot-page",
                detail = format!(
                    "page {page} does not exist; the document has {} page(s)",
                    pages.len()
                )
            )
        })
}

/// The annotation references on a page, from its (possibly indirect) `/Annots`.
fn annotation_refs(
    src: &[u8],
    doc: &selis_pdf_cos::Doc,
    page_dict: &[(Bytes, Obj)],
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
) -> Result<Vec<Ref>> {
    let value = page_dict
        .iter()
        .find(|(k, _)| k.as_slice() == b"Annots")
        .map(|(_, v)| v.clone());
    let Some(value) = value else {
        return Ok(Vec::new());
    };
    let items = match value {
        Obj::Ref(reference) => match resolve_ref(src, doc, reference, budget, g) {
            Ok(Obj::Array(items)) => items,
            _ => return Ok(Vec::new()),
        },
        Obj::Array(items) => items,
        _ => return Ok(Vec::new()),
    };
    let mut out = Vec::new();
    for item in items {
        if let Obj::Ref(reference) = item {
            out.push(reference);
        }
    }
    Ok(out)
}

/// Set an entry in a dictionary, or add it.
fn set_entry(dict: &mut Vec<(Bytes, Obj)>, key: &[u8], value: Obj) {
    for entry in dict.iter_mut() {
        if entry.0.as_slice() == key {
            entry.1 = value;
            return;
        }
    }
    dict.push((Bytes::copy_from_slice(key), value));
}

/// The name of an entry, when it is a name.
fn name_of(pairs: &[(Bytes, Obj)], key: &[u8]) -> Option<String> {
    pairs
        .iter()
        .find(|(k, _)| k.as_slice() == key)
        .and_then(|(_, v)| match v {
            Obj::Name(name) => Some(String::from_utf8_lossy(name.as_slice()).into_owned()),
            _ => None,
        })
}

/// The reference value of an entry, when it holds a reference.
fn name_ref_of(pairs: &[(Bytes, Obj)], key: &[u8]) -> Option<Ref> {
    pairs
        .iter()
        .find(|(k, _)| k.as_slice() == key)
        .and_then(|(_, value)| match value {
            Obj::Ref(reference) => Some(*reference),
            _ => None,
        })
}

/// A four-number array.
fn array4_of(pairs: &[(Bytes, Obj)], key: &[u8]) -> Option<[f64; 4]> {
    pairs
        .iter()
        .find(|(k, _)| k.as_slice() == key)
        .and_then(|(_, v)| match v {
            Obj::Array(items) if items.len() >= 4 => {
                let mut out = [0.0f64; 4];
                for (index, slot) in out.iter_mut().enumerate() {
                    *slot = number(&items[index])?;
                }
                Some(out)
            }
            _ => None,
        })
}

/// A number, from an integer or a real.
fn number(value: &Obj) -> Option<f64> {
    match value {
        Obj::Int(n) => Some(*n as f64),
        Obj::Real { scaled, scale } => Some(*scaled as f64 / 10f64.powi(i32::from(*scale))),
        _ => None,
    }
}

/// The colour of an annotation, when it carries one.
fn colour_of(pairs: &[(Bytes, Obj)]) -> Option<[f64; 3]> {
    pairs
        .iter()
        .find(|(k, _)| k.as_slice() == b"C")
        .and_then(|(_, v)| match v {
            Obj::Array(items) if items.len() >= 3 => {
                let mut out = [0.0f64; 3];
                for (index, slot) in out.iter_mut().enumerate() {
                    *slot = number(&items[index])?;
                }
                Some(out)
            }
            _ => None,
        })
}

/// The text of an entry, when it carries one.
fn text_of(pairs: &[(Bytes, Obj)], key: &[u8]) -> String {
    pairs
        .iter()
        .find(|(k, _)| k.as_slice() == key)
        .and_then(|(_, v)| match v {
            Obj::String(bytes) => Some(decode_text(bytes.as_slice())),
            Obj::HexString(bytes) => Some(decode_text(bytes.as_slice())),
            _ => None,
        })
        .unwrap_or_default()
}

/// The quads of an annotation, when it covers any.
fn quads_of(pairs: &[(Bytes, Obj)]) -> Vec<[f64; 4]> {
    let Some(Obj::Array(items)) = pairs
        .iter()
        .find(|(k, _)| k.as_slice() == b"QuadPoints")
        .map(|(_, v)| v.clone())
    else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for quad in items.chunks(8) {
        if quad.len() < 8 {
            break;
        }
        // The writer stores the four vertices counterclockwise from the
        // upper left (x0,y1 x1,y1 x0,y0 x1,y0). Read the same order back into
        // the [x0,y0,x1,y1] rectangle this crate uses everywhere else, or a
        // regenerated appearance would drift from the original mark.
        let indices = [0usize, 5, 2, 1];
        let mut values = [0.0f64; 4];
        let mut ok = true;
        for (slot, index) in values.iter_mut().zip(indices) {
            match number(&quad[index]) {
                Some(value) => *slot = value,
                None => ok = false,
            }
        }
        if ok {
            out.push(values);
        }
    }
    out
}

/// The `/QuadPoints` array: the vertices of each quad, counterclockwise from
/// the upper left, as the specification asks.
fn quad_points(quads: &[[f64; 4]]) -> Obj {
    let mut items = Vec::with_capacity(quads.len() * 8);
    for quad in quads {
        let (x0, y0, x1, y1) = (quad[0], quad[1], quad[2], quad[3]);
        items.push(real(x0));
        items.push(real(y1));
        items.push(real(x1));
        items.push(real(y1));
        items.push(real(x0));
        items.push(real(y0));
        items.push(real(x1));
        items.push(real(y0));
    }
    Obj::Array(items)
}

/// The strokes of a drawn mark: `paths` when supplied, otherwise the legacy
/// single `points` line wrapped as one stroke.
fn ink_paths(spec: &AnnotSpec) -> Vec<Vec<(f64, f64)>> {
    if !spec.paths.is_empty() {
        return spec.paths.clone();
    }
    if !spec.points.is_empty() {
        return vec![spec.points.clone()];
    }
    Vec::new()
}

/// The rectangle that tightly bounds every stroke, grown by half the stroke
/// width (and a point) so no mark is clipped by its own appearance. An empty
/// stroke set is refused by [`validate`] before this is reached.
fn ink_rect(paths: &[Vec<(f64, f64)>], width: f64) -> Result<[f64; 4]> {
    let pad = (width / 2.0).max(1.0);
    let (mut x0, mut y0, mut x1, mut y1) = (f64::MAX, f64::MAX, f64::MIN, f64::MIN);
    for (x, y) in paths.iter().flatten() {
        x0 = x0.min(*x);
        y0 = y0.min(*y);
        x1 = x1.max(*x);
        y1 = y1.max(*y);
    }
    if !x0.is_finite() || !y0.is_finite() || !x1.is_finite() || !y1.is_finite() {
        return Err(err!(
            Code::MutationInvalid,
            during = "annot-ink",
            detail = "the drawn mark has no finite points"
        ));
    }
    Ok([x0 - pad, y0 - pad, x1 + pad, y1 + pad])
}

/// The `/BS` border style of an ink stroke: solid, with round caps and joins.
fn border(width: f64) -> Obj {
    Obj::Dict(vec![
        (
            Bytes::copy_from_slice(b"Type"),
            Obj::Name(Bytes::copy_from_slice(b"Border")),
        ),
        (Bytes::copy_from_slice(b"W"), real(width.max(0.5))),
        (
            Bytes::copy_from_slice(b"S"),
            Obj::Name(Bytes::copy_from_slice(b"S")),
        ),
        (Bytes::copy_from_slice(b"LC"), Obj::Int(1)),
        (Bytes::copy_from_slice(b"LJ"), Obj::Int(1)),
    ])
}

/// The `/InkList` array: an array of strokes, each an array of x/y numbers.
fn ink_list(paths: &[Vec<(f64, f64)>]) -> Obj {
    Obj::Array(
        paths
            .iter()
            .map(|path| {
                let mut items = Vec::with_capacity(path.len().saturating_mul(2));
                for (x, y) in path {
                    items.push(real(*x));
                    items.push(real(*y));
                }
                Obj::Array(items)
            })
            .collect(),
    )
}

/// The strokes a document's `/InkList` carries, read back from an annotation.
/// Accepts both the spec's array-of-arrays and the older flat single-stroke
/// form, so documents written before multi-stroke support still read back.
fn ink_paths_of(pairs: &[(Bytes, Obj)]) -> Vec<Vec<(f64, f64)>> {
    let Some(value) = pairs
        .iter()
        .find(|(key, _)| key.as_slice() == b"InkList")
        .map(|(_, value)| value)
    else {
        return Vec::new();
    };
    let Obj::Array(items) = value else {
        return Vec::new();
    };
    if items
        .first()
        .is_some_and(|first| matches!(first, Obj::Array(_)))
    {
        return items
            .iter()
            .filter_map(|item| match item {
                Obj::Array(values) => Some(points_of(values)),
                _ => None,
            })
            .collect();
    }
    // A flat legacy list of numbers: one stroke.
    if items.first().is_some_and(|first| number(first).is_some()) {
        return vec![points_of(items)];
    }
    Vec::new()
}

/// The x/y points of one stroke array, in order.
fn points_of(values: &[Obj]) -> Vec<(f64, f64)> {
    let mut out = Vec::with_capacity(values.len().saturating_div(2));
    let mut index = 0usize;
    while index + 1 < values.len() {
        let (Some(x), Some(y)) = (number(&values[index]), number(&values[index + 1])) else {
            break;
        };
        out.push((x, y));
        index = index.saturating_add(2);
    }
    out
}

/// A rectangle as the PDF array it is written as.
fn rect_array(rect: [f64; 4]) -> Obj {
    Obj::Array(vec![
        real(rect[0]),
        real(rect[1]),
        real(rect[2]),
        real(rect[3]),
    ])
}

/// A colour as the PDF array it is written as.
fn colour_array(colour: [f64; 3]) -> Obj {
    Obj::Array(vec![real(colour[0]), real(colour[1]), real(colour[2])])
}

/// A number in a content stream: an integer when it is whole, and at most
/// three decimals otherwise.
fn num(value: f64) -> String {
    if !value.is_finite() {
        return "0".to_string();
    }
    let rounded = (value * 1000.0).round() / 1000.0;
    if (rounded - rounded.trunc()).abs() < f64::EPSILON {
        return format!("{}", rounded.trunc() as i64);
    }
    let mut text = format!("{rounded:.3}");
    while text.ends_with('0') {
        text.pop();
    }
    if text.ends_with('.') {
        text.pop();
    }
    text
}

/// A real number, written as an object.
fn real(value: f64) -> Obj {
    if !value.is_finite() {
        return Obj::Int(0);
    }
    let scaled = (value * 1000.0).round();
    let clamped = scaled.clamp(-9_223_372_036_854_775_000.0, 9_223_372_036_854_775_000.0);
    Obj::Real {
        scaled: clamped as i64,
        scale: 3,
    }
}

/// A text string: a literal string when it is plain ASCII, and a UTF-16BE
/// hex string when it is not.
fn text_string(text: &str) -> Obj {
    if text.is_ascii() {
        // The COS writer escapes a literal string itself, so the bytes handed
        // to it are the text's own bytes and not the literal's syntax.
        return Obj::String(Bytes::copy_from_slice(text.as_bytes()));
    }
    let mut bytes = vec![0xfe, 0xff];
    for unit in text.encode_utf16() {
        bytes.push((unit >> 8) as u8);
        bytes.push((unit & 0xff) as u8);
    }
    Obj::HexString(Bytes::copy_from_slice(&bytes))
}

/// The text of a string, decoded from PDFDocEncoding or UTF-16BE.
fn decode_text(bytes: &[u8]) -> String {
    if bytes.len() >= 2 && bytes[0] == 0xfe && bytes[1] == 0xff {
        let mut units = Vec::with_capacity(bytes.len() / 2);
        let mut index = 2;
        while index + 1 < bytes.len() {
            units.push(u16::from_be_bytes([bytes[index], bytes[index + 1]]));
            index += 2;
        }
        return String::from_utf16_lossy(&units);
    }
    bytes.iter().map(|byte| char::from(*byte)).collect()
}

/// The next free object number.
fn next_object_number(doc: &selis_pdf_cos::Doc) -> u32 {
    let mut highest = 0u32;
    for revision in doc.revisions() {
        for number in revision.entries.keys() {
            highest = highest.max(*number);
        }
    }
    highest.saturating_add(1)
}

/// The trailer entries a new revision carries forward.
fn carried_trailer(doc: &selis_pdf_cos::Doc) -> Result<Vec<(Vec<u8>, Obj)>> {
    let revision = doc.revisions().last().ok_or_else(|| {
        err!(
            Code::ObjUnexpected,
            during = "annot-trailer",
            detail = "the document has no revisions"
        )
    })?;
    let root = revision.root.ok_or_else(|| {
        err!(
            Code::ObjUnexpected,
            during = "annot-trailer",
            detail = "the trailer has no /Root"
        )
    })?;
    let mut out = vec![(b"Root".to_vec(), Obj::Ref(root))];
    if let Some((_, id)) = revision.trailer.iter().find(|(k, _)| k.as_slice() == b"ID") {
        out.push((b"ID".to_vec(), id.clone()));
    }
    Ok(out)
}
