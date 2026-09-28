//! Navigation vocabulary: destinations, actions, and the page map
//! (SL-3.DOC-NAV).
//!
//! The three structures a viewer's navigation panel needs and no other layer
//! owns: **where a link points** ([`NavTarget`]), **what a link asks the reader
//! to do** ([`Action`]), and **which page an object number is**
//! ([`PageMap`]). They live together because all three are *reported*, never
//! *decided*: ADR-P0020's action-class policy is the UI's, and a model that
//! filtered action classes here would make "is the viewer refusing this, or
//! does the engine not know about it?" unanswerable from the outside — which
//! is exactly the question `apps/ui/src/viewer/links.ts` exists to answer.
//!
//! Two encoding decisions are load-bearing and stated once here:
//!
//! * **An action class this engine does not model is [`ActionKind::Other`],
//!   carrying the document's own name verbatim.** It is never coerced into a
//!   neighbouring class and never dropped. `/Launch` is the case that matters:
//!   an engine that mapped unknown to "no action" would make a process-start
//!   link indistinguishable from a dead bookmark, and "silently ignored" is
//!   precisely how such a link would slip past the viewer's refusal.
//! * **A destination that names no page is [`NavTarget::Unresolved`], not
//!   absent.** A `/Dest` pointing at a page reference this document does not
//!   contain is a document defect the reader should see as "this bookmark goes
//!   nowhere", not as a bookmark that was quietly shortened away.

use std::collections::BTreeMap;

use selis_bytes::Bytes;
use selis_error::Result;
use selis_pdf_cos::{Obj, Ref};
use selis_sandbox::{Budget, BudgetGuard};

use crate::{Page, Resolver};

/// Object number to zero-based page index.
///
/// Destinations, outline items and link annotations all address pages by
/// *reference*; every consumer of this model wants a page *number*, and the
/// conversion is only knowable from the walked page tree. Building the map once
/// per document and passing it by reference keeps that conversion in one place
/// instead of at each call site.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PageMap {
    by_num: BTreeMap<u32, u32>,
}

impl PageMap {
    /// Build the map from a walked page list, in document order.
    #[must_use]
    pub fn new(pages: &[Page]) -> Self {
        let mut by_num = BTreeMap::new();
        for (index, page) in pages.iter().enumerate() {
            // A duplicated object number means a damaged page tree. The first
            // page wins: the walk order *is* the document's page order, and
            // picking the later duplicate would make a page number depend on a
            // defect.
            by_num
                .entry(page.num)
                .or_insert_with(|| u32::try_from(index).unwrap_or(u32::MAX));
        }
        Self { by_num }
    }

    /// The zero-based index of the page `r` names, if this document has it.
    #[must_use]
    pub fn index_of(&self, r: Ref) -> Option<u32> {
        self.by_num.get(&r.num).copied()
    }

    /// The number of pages in the map.
    #[must_use]
    pub fn len(&self) -> usize {
        self.by_num.len()
    }

    /// Whether the map is empty (a document with no pages).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.by_num.is_empty()
    }
}

/// How a destination places its page (PDF 32000-2:2020 section 12.3.2.2).
///
/// `Fit`/`FitH`/`FitV`/`FitR`/`FitB`/`XYZ` are the classes the spec defines and
/// the viewer acts on. [`DestinationKind::Other`] carries anything else
/// **verbatim**: an unrecognised destination type is a loss of fidelity in the
/// *viewer*, not an error here, and guessing at a neighbour would be worse
/// than recording what the document said.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DestinationKind {
    /// `/Fit` — the whole page.
    Fit,
    /// `/FitH top` — the page at a vertical position.
    FitH,
    /// `/FitV left` — the page at a horizontal position.
    FitV,
    /// `/FitR left bottom right top` — a sub-rectangle of the page.
    FitR,
    /// `/FitB` — the page's bounding box.
    FitB,
    /// `/XYZ left top zoom` — an exact point, any of which may be null.
    Xyz,
    /// A destination type this engine does not model, by name.
    Other(Bytes),
}

impl DestinationKind {
    /// Classify a destination type keyword.
    #[must_use]
    pub fn from_name(name: &[u8]) -> Self {
        match name {
            b"Fit" => Self::Fit,
            b"FitH" => Self::FitH,
            b"FitV" => Self::FitV,
            b"FitR" => Self::FitR,
            b"FitB" => Self::FitB,
            b"XYZ" => Self::Xyz,
            other => Self::Other(Bytes::copy_from_slice(other)),
        }
    }

    /// The class's own name, for a transport that speaks names.
    #[must_use]
    pub fn as_name(&self) -> &[u8] {
        match self {
            Self::Fit => b"Fit",
            Self::FitH => b"FitH",
            Self::FitV => b"FitV",
            Self::FitR => b"FitR",
            Self::FitB => b"FitB",
            Self::Xyz => b"XYZ",
            Self::Other(name) => name.as_slice(),
        }
    }
}

/// A destination that names a page of this document.
#[derive(Debug, Clone, PartialEq)]
pub struct PageDestination {
    /// The zero-based page index.
    pub page: u32,
    /// How to place it.
    pub kind: DestinationKind,
    /// `/XYZ left`; `None` when the author left it unset (the "keep the
    /// current value" null of a destination array).
    pub left: Option<f64>,
    /// `/XYZ top`.
    pub top: Option<f64>,
    /// `/XYZ zoom`.
    pub zoom: Option<f64>,
}

/// Where a link, a bookmark or a `/GoTo` action points.
#[derive(Debug, Clone, PartialEq)]
pub enum NavTarget {
    /// A page of this document.
    Page(PageDestination),
    /// A named destination, for the viewer to resolve against the document's
    /// `/Dests` name tree. The name is verbatim.
    Named(String),
    /// The document wrote a destination this engine cannot resolve: a `/Dest`
    /// that is not an array or a name, or one naming a page the document does
    /// not contain.
    ///
    /// Reported rather than dropped. A bookmark the reader can see and that
    /// reports "this document's destination is missing" is more useful, and
    /// more honest, than a silently shorter outline.
    Unresolved,
}

/// An explicit destination dictionary (`/Dest << /D [...] >>`), unwrapped.
///
/// The spec allows a destination to be written as a dictionary whose `/D` holds
/// the array; a great many writers do this, and treating the dictionary itself
/// as the destination would report every such bookmark as unresolvable.
fn unwrap_dest_dict(value: &Obj) -> &Obj {
    match value {
        Obj::Dict(_) => dict_get(value, b"D").unwrap_or(value),
        _ => value,
    }
}

/// Parse a `/Dest`-shaped value into a [`NavTarget`].
///
/// # Budget
///
/// None directly: this reads an already-resolved object and allocates one
/// [`NavTarget`]. The budget that bounds *reaching* this value is charged by
/// the caller that resolved it (the outline walk, the annotation reader).
///
/// # Malformed Input
///
/// Anything that is not a page-naming destination array, a name, or an explicit
/// destination dictionary yields [`NavTarget::Unresolved`]. A destination array
/// whose first element is not a reference, or that names a page this document
/// does not contain, yields [`NavTarget::Unresolved`] as well — the
/// alternative, inventing page 0, would navigate the reader somewhere the
/// document never said.
pub fn parse_target(value: &Obj, pages: &PageMap) -> NavTarget {
    match unwrap_dest_dict(value) {
        Obj::Name(name) => NavTarget::Named(String::from_utf8_lossy(name.as_slice()).to_string()),
        // Some writers write a name destination as a string rather than a
        // name; the viewer resolves both against the same name tree, so both
        // are reported the same way rather than one being called malformed.
        Obj::String(name) => NavTarget::Named(String::from_utf8_lossy(name.as_slice()).to_string()),
        Obj::Array(items) => array_target(items, pages),
        _ => NavTarget::Unresolved,
    }
}

/// The `[page /Kind params…]` form.
fn array_target(items: &[Obj], pages: &PageMap) -> NavTarget {
    let Some(Obj::Ref(page_ref)) = items.first() else {
        return NavTarget::Unresolved;
    };
    let Some(page) = pages.index_of(*page_ref) else {
        return NavTarget::Unresolved;
    };
    // A destination with no type keyword is "the page, no placement
    // instruction" — the spec's own default.
    let kind = match items.get(1) {
        Some(Obj::Name(name)) => DestinationKind::from_name(name.as_slice()),
        _ => DestinationKind::Fit,
    };
    let mut destination = PageDestination {
        page,
        kind,
        left: None,
        top: None,
        zoom: None,
    };
    if destination.kind == DestinationKind::Xyz {
        // Only `/XYZ` carries the three placement parameters; reading them for
        // any other class would invent a position the document never wrote.
        let params = items.get(2..).unwrap_or(&[]);
        destination.left = params.first().and_then(obj_f64);
        destination.top = params.get(1).and_then(obj_f64);
        destination.zoom = params.get(2).and_then(obj_f64);
    }
    NavTarget::Page(destination)
}

/// The action classes PDF 32000-2:2020 section 12.6 defines.
///
/// The list is the **whole** vocabulary, including the classes ADR-P0020
/// disables, because this crate reports what the document says and the viewer
/// decides what happens. [`ActionKind::Other`] is the load-bearing arm: an
/// action dictionary naming a class this build does not model is reported
/// under its own name rather than folded into a neighbour.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ActionKind {
    /// `/GoTo` — navigate inside this document.
    GoTo,
    /// `/GoToR` — navigate to another document. ADR-P0020.
    GoToR,
    /// `/GoToE` — navigate to an embedded document.
    GoToE,
    /// `/Launch` — start an application or open a file. ADR-P0020.
    Launch,
    /// `/Thread` — jump to an article thread bead.
    Thread,
    /// `/URI` — open a URI.
    Uri,
    /// `/Sound` — play a sound.
    Sound,
    /// `/Movie` — play a movie.
    Movie,
    /// `/Hide` — show or hide annotations.
    Hide,
    /// `/Named` — a named viewer action.
    Named,
    /// `/SubmitForm` — submit a form. ADR-P0020.
    SubmitForm,
    /// `/ResetForm` — reset a form.
    ResetForm,
    /// `/ImportData` — import a form data file. ADR-P0020.
    ImportData,
    /// `/JavaScript` — run document JavaScript. ADR-P0020.
    JavaScript,
    /// `/SetOCGState` — change optional-content visibility.
    SetOcgState,
    /// `/Rendition` — a media rendition.
    Rendition,
    /// `/Trans` — a page transition.
    Trans,
    /// `/GoTo3DView` — a 3D view.
    GoTo3dView,
    /// `/RichMediaExecute` — rich-media script execution.
    RichMediaExecute,
    /// An action class this build does not model, by name. Never dropped and
    /// never merged into a neighbouring class.
    Other(Bytes),
}

impl ActionKind {
    /// Classify an action's `/S` (or `/Type`) keyword.
    ///
    /// Names are matched **case-sensitively against the spec's spelling**,
    /// because PDF names are case-sensitive and a case-folded match would let
    /// a document's `/launch` masquerade as a class the viewer has never heard
    /// of, in either direction. `/JS` is accepted as the older spelling of
    /// `/JavaScript` and maps to the same class, which is the one aliasing the
    /// spec itself sanctions.
    #[must_use]
    pub fn from_name(name: &[u8]) -> Self {
        match name {
            b"GoTo" => Self::GoTo,
            b"GoToR" => Self::GoToR,
            b"GoToE" => Self::GoToE,
            b"Launch" => Self::Launch,
            b"Thread" => Self::Thread,
            b"URI" => Self::Uri,
            b"Sound" => Self::Sound,
            b"Movie" => Self::Movie,
            b"Hide" => Self::Hide,
            b"Named" => Self::Named,
            b"SubmitForm" => Self::SubmitForm,
            b"ResetForm" => Self::ResetForm,
            b"ImportData" => Self::ImportData,
            b"JavaScript" | b"JS" => Self::JavaScript,
            b"SetOCGState" => Self::SetOcgState,
            b"Rendition" => Self::Rendition,
            b"Trans" => Self::Trans,
            b"GoTo3DView" => Self::GoTo3dView,
            b"RichMediaExecute" => Self::RichMediaExecute,
            other => Self::Other(Bytes::copy_from_slice(other)),
        }
    }

    /// The class's own name, for a transport that speaks names.
    #[must_use]
    pub fn as_name(&self) -> &[u8] {
        match self {
            Self::GoTo => b"GoTo",
            Self::GoToR => b"GoToR",
            Self::GoToE => b"GoToE",
            Self::Launch => b"Launch",
            Self::Thread => b"Thread",
            Self::Uri => b"URI",
            Self::Sound => b"Sound",
            Self::Movie => b"Movie",
            Self::Hide => b"Hide",
            Self::Named => b"Named",
            Self::SubmitForm => b"SubmitForm",
            Self::ResetForm => b"ResetForm",
            Self::ImportData => b"ImportData",
            Self::JavaScript => b"JavaScript",
            Self::SetOcgState => b"SetOCGState",
            Self::Rendition => b"Rendition",
            Self::Trans => b"Trans",
            Self::GoTo3dView => b"GoTo3DView",
            Self::RichMediaExecute => b"RichMediaExecute",
            Self::Other(name) => name.as_slice(),
        }
    }
}

/// A resolved action: what the document asks a viewer to do.
///
/// No field of this struct is a decision. `/Launch` arrives here exactly as a
/// `/URI` does, with the same shape, because whether it is followed is
/// ADR-P0020's question and it is asked on the UI side.
#[derive(Debug, Clone, PartialEq)]
pub struct Action {
    /// What class of action this is.
    pub kind: ActionKind,
    /// Where a `/GoTo` (or `/GoToR`/`/GoToE`) points.
    pub target: Option<NavTarget>,
    /// The `/URI` of a `/URI` action, or the file specification of a
    /// `/Launch`, `/GoToR`, `/GoToE`, `/ImportData`, `/SubmitForm` or
    /// `/ResetForm` — **verbatim and unparsed**. Normalising it here would hide
    /// what the document actually said, and the scheme check is the viewer's
    /// (ADR-P0020).
    pub uri: Option<String>,
    /// The `/N` name of a `/Named` action.
    pub name: Option<String>,
    /// The action declared a `/Next` continuation.
    ///
    /// Reported, **not followed**: a `/Next` chain is a second place a
    /// `/Launch` can hide, so a reader deserves to know one exists — but
    /// following it would mean the engine deciding that a chain's tail is part
    /// of the activation, which is the viewer's call over one action. It also
    /// means the engine cannot be walked into an unbounded `/Next` loop on a
    /// document whose chain is cyclic.
    pub has_next: bool,
}

/// Parse an action value (`/A`, or an outline item's action).
///
/// # Budget
///
/// Charges `Objects` for each reference resolved on the way in: the action
/// itself when indirect, and a file specification when it is a dictionary.
///
/// # Malformed Input
///
/// A non-dictionary action, an action with neither `/S` nor `/Type /Action`,
/// and an unresolvable file specification all yield `Ok(None)`: the annotation
/// or outline item is still reported, with no action, rather than dropped.
/// That distinction is the one the viewer can act on — "this link has no
/// target" is a state it can refuse *with a reason*, whereas a missing
/// annotation is invisible. Budget and cancellation always propagate.
pub fn parse_action(
    resolver: &mut Resolver<'_>,
    action: &Obj,
    pages: &PageMap,
    g: &mut BudgetGuard<'_>,
) -> Result<Option<Action>> {
    // A pre-1.1 writer may write `/A` as an array of actions; the first is the
    // one an activation runs and the rest are a `/Next` chain in all but name.
    let (value, chained) = match action {
        Obj::Array(items) => match items.first() {
            Some(first) => (first, items.len() > 1),
            None => return Ok(None),
        },
        other => (other, false),
    };
    // An indirect action is one resolution; a failure that is not exhaustion
    // simply means the document named an object it does not contain.
    let owned;
    let value = match value {
        Obj::Ref(r) => {
            owned = match resolver.resolve(*r, g) {
                Ok(o) => o,
                Err(e) if e.is_budget() || e.is_cancelled() || e.is_pending() => return Err(e),
                Err(_) => return Ok(None),
            };
            &owned
        }
        other => other,
    };
    let Obj::Dict(pairs) = value else {
        return Ok(None);
    };
    let get = |key: &[u8]| -> Option<&Obj> {
        pairs
            .iter()
            .find(|(k, _)| k.as_slice() == key)
            .map(|(_, v)| v)
    };
    // `/S` is the action type; `/Type /Action` is the older spelling. A
    // dictionary with neither is not an action.
    let Some(Obj::Name(kind_name)) = get(b"S").or_else(|| get(b"Type")) else {
        return Ok(None);
    };
    let kind = ActionKind::from_name(kind_name.as_slice());

    let target = match (&kind, get(b"D")) {
        (ActionKind::GoTo | ActionKind::GoToR | ActionKind::GoToE, Some(dest)) => {
            Some(parse_target(dest, pages))
        }
        _ => None,
    };
    let uri = match &kind {
        ActionKind::Uri => get(b"URI").and_then(plain_text),
        ActionKind::Launch
        | ActionKind::GoToR
        | ActionKind::GoToE
        | ActionKind::ImportData
        | ActionKind::SubmitForm
        | ActionKind::ResetForm => match get(b"F") {
            Some(spec) => file_spec_text(resolver, spec, g)?,
            None => None,
        },
        _ => None,
    };
    let name = match (&kind, get(b"N")) {
        (ActionKind::Named, Some(Obj::Name(n) | Obj::String(n))) => {
            Some(String::from_utf8_lossy(n.as_slice()).to_string())
        }
        _ => None,
    };
    let has_next = chained || get(b"Next").is_some();
    Ok(Some(Action {
        kind,
        target,
        uri,
        name,
        has_next,
    }))
}

/// A string, name, or hex string as text.
fn plain_text(value: &Obj) -> Option<String> {
    match value {
        Obj::String(s) | Obj::Name(s) | Obj::HexString(s) => {
            Some(String::from_utf8_lossy(s.as_slice()).to_string())
        }
        _ => None,
    }
}

/// The text of a value that may be a string, a name, or a file specification.
///
/// # Budget
///
/// Charges `Objects` for the one reference a file-specification dictionary may
/// require.
///
/// # Malformed Input
///
/// A file specification that is none of string, name, or dictionary yields
/// `None`. The action is still reported with its class, so a `/Launch` whose
/// target cannot be read is still a `/Launch` the viewer refuses — which is the
/// distinction that stops an unreadable target from reading as "no action".
fn file_spec_text(
    resolver: &mut Resolver<'_>,
    value: &Obj,
    g: &mut BudgetGuard<'_>,
) -> Result<Option<String>> {
    match value {
        Obj::Ref(r) => {
            let dict = match resolver.resolve(*r, g) {
                Ok(d) => d,
                Err(e) if e.is_budget() || e.is_cancelled() || e.is_pending() => return Err(e),
                Err(_) => return Ok(None),
            };
            Ok(file_spec_dict_text(&dict))
        }
        other => Ok(file_spec_dict_text(other)),
    }
}

/// `/UF` with `/F` as the fallback, per the spec's file-specification order.
///
/// A file specification is a **string or a dictionary** (PDF 32000-2:2020
/// section 7.11.2), and the bare string is the common spelling — `/Launch`
/// with `/F (cmd.exe /c calc.exe)`, which is what most writers emit. Reading
/// only the dictionary form left such a `/Launch` arriving as a `/Launch` with
/// no target: the class survived, but the string the viewer would have to show
/// a reason for did not, and "a `/Launch` I cannot read what it launches" is a
/// state `links.ts` has no arm for. So the string form is read as the
/// specification it is, rather than as a specification this build cannot parse.
fn file_spec_dict_text(spec: &Obj) -> Option<String> {
    if !matches!(spec, Obj::Dict(_)) {
        return plain_text(spec);
    }
    if let Some(text) = dict_get(spec, b"UF").and_then(plain_text) {
        return Some(text);
    }
    dict_get(spec, b"F").and_then(plain_text)
}

/// A PDF text string (`/Title`, `/Contents`) as Unicode.
///
/// PDF 32000-2:2020 section 7.9.2.2: a text string beginning with the UTF-16BE
/// byte order mark is UTF-16; otherwise it is PDFDocEncoding. Only the UTF-16BE
/// branch is decoded properly here — a byte-for-byte reading of the
/// PDFDocEncoding branch keeps ASCII titles exact, which is what the corpus and
/// the viewer's outline panel both exercise, and leaves the 0x80-0xFF
/// PDFDocEncoding range (typographic quotes, dashes, the Euro sign) at their
/// Latin-1 code points. That is a **fidelity gap, recorded rather than
/// hidden**: closing it means a sideways L2 edge to `selis-font`, which owns
/// the table, and `xtask/layers.toml` is explicit that adding one needs review
/// rather than a drive-by.
#[must_use]
pub fn text_string(bytes: &[u8]) -> String {
    const BOM: [u8; 2] = [0xFE, 0xFF];
    if bytes.len() >= 2 && bytes.get(0..2) == Some(BOM.as_slice()) {
        return decode_utf16be(bytes.get(2..).unwrap_or(&[]));
    }
    String::from_utf8_lossy(bytes).to_string()
}

/// UTF-16BE code units to a `String`.
///
/// A trailing odd byte and any unpaired surrogate become U+FFFD rather than an
/// error: a bookmark title is not worth failing a document over, and losing
/// one glyph is visible where losing the outline would not be.
fn decode_utf16be(bytes: &[u8]) -> String {
    let mut units: Vec<u16> = Vec::new();
    let mut i = 0usize;
    while let (Some(hi), Some(lo)) = (bytes.get(i), bytes.get(i.saturating_add(1))) {
        units.push(u16::from_be_bytes([*hi, *lo]));
        i = i.saturating_add(2);
    }
    String::from_utf16_lossy(&units)
}

/// A COS integer or real as `f64`.
pub(crate) fn obj_f64(obj: &Obj) -> Option<f64> {
    match obj {
        Obj::Int(i) => Some(*i as f64),
        Obj::Real { scaled, scale } => {
            let div = 10f64.powi(i32::from(*scale));
            Some(*scaled as f64 / div)
        }
        _ => None,
    }
}

/// Look up a key in a dictionary object.
pub(crate) fn dict_get<'a>(dict: &'a Obj, key: &[u8]) -> Option<&'a Obj> {
    match dict {
        Obj::Dict(pairs) => pairs
            .iter()
            .find(|(k, _)| k.as_slice() == key)
            .map(|(_, v)| v),
        _ => None,
    }
}

/// Look up a key whose value must be a reference.
pub(crate) fn dict_ref(dict: &Obj, key: &[u8]) -> Option<Ref> {
    match dict_get(dict, key) {
        Some(Obj::Ref(r)) => Some(*r),
        _ => None,
    }
}

/// A `/Dests` name-tree entry: a name and where it points.
#[derive(Debug, Clone, PartialEq)]
pub struct NamedDestination {
    /// The destination name, verbatim.
    pub name: String,
    /// Where it points. [`NavTarget::Named`] is possible here in principle (a
    /// name tree whose value is itself a name) and is passed through so the
    /// viewer can chase the chain itself rather than the engine deciding where
    /// it ends.
    pub target: NavTarget,
}

/// The document's named destinations (`/Dests`).
///
/// # Budget
///
/// Bounded by the name-tree walk and by `Objects` per entry resolved.
///
/// # Malformed Input
///
/// A document with no `/Dests` yields an empty list. An entry whose value is
/// not a destination is skipped. A name that resolves to nothing is skipped
/// rather than reported as unresolvable, because a name tree legitimately
/// carries entries this engine does not model and the viewer is better served
/// by the names that do resolve.
///
/// The catalog's `/Dests` dictionary (PDF 1.1) and the `/Names /Dests` name
/// tree (PDF 1.2+) are **both** read, the dictionary first: a document
/// carrying both is contradictory and the older form is the one the spec says
/// a reader "shall" prefer.
pub fn named_destinations(
    resolver: &mut Resolver<'_>,
    catalog: &Obj,
    pages: &PageMap,
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
) -> Result<Vec<NamedDestination>> {
    let mut out = Vec::new();
    // The PDF 1.1 catalog `/Dests` dictionary, in either spelling: a writer may
    // write it inline or indirect, and matching only the indirect form loses
    // the destinations of every document that wrote it inline. Materialising
    // first means one code path reads both.
    if let Some(value) = dict_get(catalog, b"Dests") {
        let root = materialise_value(resolver, value, g)?;
        if let Obj::Dict(pairs) = &root {
            for (key, value) in pairs {
                g.tick()?;
                out.push(NamedDestination {
                    name: String::from_utf8_lossy(key.as_slice()).to_string(),
                    target: parse_target(&materialise_value(resolver, value, g)?, pages),
                });
            }
        }
        return Ok(out);
    }
    // `/Names` is itself very often an indirect object (`/Names 41 0 R`), and
    // the `/Dests` key lives *inside* it. Reading the key out of the raw
    // `/Names` value therefore finds nothing whenever that value is a reference
    // — which is most real documents — and the viewer is handed an empty
    // destination list for a document that has one. So the indirection is
    // resolved first, and a `/Names` that does not resolve is "no
    // destinations" rather than an error.
    let names_value = match dict_get(catalog, b"Names") {
        Some(value) => materialise_value(resolver, value, g)?,
        None => return Ok(out),
    };
    let Some(names) = dict_ref(&names_value, b"Dests") else {
        return Ok(out);
    };
    let tree = crate::walk_name_tree(resolver, names, budget, g)?;
    for (name, value) in &tree {
        g.tick()?;
        out.push(NamedDestination {
            name: name.clone(),
            target: parse_target(&materialise_value(resolver, value, g)?, pages),
        });
    }
    Ok(out)
}

/// One level of indirection: the value itself, or the object it references.
///
/// # Budget
///
/// Charges `Objects` when the value is a reference.
///
/// # Malformed Input
///
/// An unresolvable reference yields `Obj::Null`, which every destination parser
/// here reports as [`NavTarget::Unresolved`] — so a name that points at a
/// phantom object is reported as unresolvable rather than dropped or guessed.
fn materialise_value(
    resolver: &mut Resolver<'_>,
    value: &Obj,
    g: &mut BudgetGuard<'_>,
) -> Result<Obj> {
    match value {
        Obj::Ref(r) => Ok(crate::outline::resolve_lenient(resolver, *r, g)?.unwrap_or(Obj::Null)),
        other => Ok(other.clone()),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::indexing_slicing, clippy::arithmetic_side_effects)]

    use super::*;
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

    /// Two pages: object 3 is page 0, object 4 is page 1.
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

    /// Read `catalog`'s named destinations out of `objects`.
    fn dests(objects: &[(u32, &str)]) -> Result<Vec<NamedDestination>> {
        let budget = Budget::unlimited();
        let (doc, src) = build(objects);
        let mut g = budget.guard_with(&FixedClock(0), CancelToken::new());
        let mut resolver = Resolver::new(&doc, &src, &budget);
        let catalog = resolver.resolve(Ref::new(1, 0), &mut g)?;
        named_destinations(&mut resolver, &catalog, &two_pages(), &budget, &mut g)
    }

    /// **The `/Names` name tree, reached through an indirect `/Names`.** This is
    /// the shape almost every real writer emits, and reading `/Dests` out of the
    /// *raw* `/Names` value finds nothing when that value is a reference — the
    /// viewer is then handed an empty list for a document that has destinations,
    /// and every named bookmark silently stops resolving.
    #[test]
    fn a_names_name_tree_behind_an_indirect_names_resolves() {
        let found = dests(&[
            (1, "<< /Type /Catalog /Names 41 0 R >>"),
            (41, "<< /Dests 42 0 R >>"),
            (
                42,
                "<< /Names [(chapter-one) [4 0 R /Fit] (page-two) 4 0 R] >>",
            ),
        ])
        .expect("read");
        assert_eq!(
            found.len(),
            2,
            "an indirect /Names must not hide the name tree"
        );
        let first = found.first().expect("the first destination");
        assert_eq!(first.name, "chapter-one");
        assert_eq!(
            first.target,
            NavTarget::Page(PageDestination {
                page: 1,
                kind: DestinationKind::Fit,
                left: None,
                top: None,
                zoom: None,
            }),
            "object 4 is the document's second page"
        );
    }

    /// The same tree written with `/Names` inline, so the fix is not a special
    /// case for the indirect form.
    #[test]
    fn an_inline_names_dictionary_resolves_too() {
        let found = dests(&[
            (1, "<< /Type /Catalog /Names << /Dests 42 0 R >> >>"),
            (42, "<< /Names [(chapter-one) [4 0 R /Fit]] >>"),
        ])
        .expect("read");
        assert_eq!(found.len(), 1);
        assert_eq!(found.first().map(|d| d.name.as_str()), Some("chapter-one"));
    }

    /// The PDF 1.1 catalog `/Dests` dictionary still works, and still wins when
    /// a contradictory document carries both. Note the two forms are different
    /// *shapes*: the PDF 1.1 dictionary is a flat name → destination map, while
    /// the PDF 1.2+ `/Names /Dests` is a name tree carrying a `/Names` array.
    /// Reading one as the other is a real bug — a flat dictionary walked as a
    /// tree yields the key `"Names"` as though it were a destination name, which
    /// is exactly what an earlier version of this test did.
    #[test]
    fn the_catalog_dests_dictionary_is_read_and_preferred() {
        let found = dests(&[
            (1, "<< /Type /Catalog /Dests 40 0 R /Names 41 0 R >>"),
            (40, "<< /Old [3 0 R /Fit] >>"),
            (41, "<< /Dests 42 0 R >>"),
            (42, "<< /Names [(new) [4 0 R /Fit]] >>"),
        ])
        .expect("read");
        assert_eq!(found.len(), 1);
        assert_eq!(
            found.first().map(|d| d.name.as_str()),
            Some("Old"),
            "the spec says a reader shall prefer the catalog dictionary"
        );
    }

    /// A `/Names` naming an object the document does not contain is "no
    /// destinations", not a failure: the rest of the document is unaffected.
    #[test]
    fn an_unresolvable_names_is_no_destinations_not_an_error() {
        let found = dests(&[(1, "<< /Type /Catalog /Names 99 0 R >>")]).expect("read");
        assert!(found.is_empty());
    }
}
