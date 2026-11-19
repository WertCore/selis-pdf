//! Object-level image and vector editing (SL-5.TEXTEDIT.05/06).
//!
//! The higher-level text edit in [`crate`] rewrites a *string* operand; the
//! object-level layer here addresses a **drawn object** instead: an image
//! XObject painted by `/Name Do`, or a vector path painted by `f`/`S`/`B`.
//! It closes the gap noted in `40-EDIT-PDF-CONTENT-TOOLS.md` — the engine had
//! no operation that addresses an existing object.
//!
//! # The object-addressing model
//!
//! A page's content stream is scanned once with a small PDF content cursor
//! that tracks the graphics state (`q`/`Q`/`cm`) exactly as the redaction
//! rewriter does. Every image draw and every painted path is reported with:
//!
//! * a stable [`ObjectId`] = `(resource name, operator index)` — the resource
//!   name the `Do` used (empty for a path) plus the 0-based index of the
//!   drawing operator in the joined content stream;
//! * a placement [`Rect`] in **PDF user space**, computed by pushing the image
//!   unit square (or the path's user-space bounding box) through the CTM in
//!   force at the draw;
//! * the byte span of the placement `cm` and of the draw itself, so a mutation
//!   can rewrite *only* those bytes.
//!
//! Because a mutation edits operands in place and never adds or removes an
//! operator, the operator index is stable across read-backs: re-enumerate the
//! saved file and the target still has the same id. (A deletion removes the id,
//! which is the point; later ids shift, as they must.)
//!
//! # What is lowered to a COS write
//!
//! Every mutation produces an [`crate::incremental::ObjectMutation`] and is
//! committed through [`crate::incremental::save_incremental`]:
//!
//! * [`replace_image`] redefines the image XObject stream (content untouched);
//! * [`move_object`] / [`resize_object`] rewrite the six operands of the
//!   placement `cm` in the page content stream;
//! * [`delete_object`] removes the drawing operator (and, for a path, its
//!   construction operands) from the content stream.
//!
//! Everything else in the content stream — and the entire original file as a
//! verbatim prefix — is left byte-identical (I3/I7 of the edit-model spec).
//!
//! # Scope (honest, tested)
//!
//! Only the page's **top-level** content streams are scanned. Form XObjects,
//! inline images (`BI`/`ID`/`EI`), marked-content runs and images used as
//! masks or with non-RGB colour spaces are *not* addressed; a request for one
//! is refused rather than guessed at. Images are emitted in two canonical
//! forms: Flate/DeviceRGB XObjects become PNG on extract, DCTDecode streams
//! are returned as their original JPEG bytes. Replacement always writes a
//! fresh Flate/DeviceRGB 8-bit XObject (ancillary keys such as `/SMask` and
//! `/Decode` are dropped), so a replacement is a normalisation, not a
//! byte-preserving re-encode of the old image.
//!
//! This is an **engine-level** capability. There is no CLI or wasm operation
//! and no UI selection tool here; the web/UI half of TEXTEDIT.05/06 is
//! deferred, and the plan records it as such.

#![forbid(unsafe_code)]

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::ops::Range;

use selis_bytes::Bytes;
use selis_geom::Rect;
use selis_pdf_cos::{Obj, Ref};
use selis_sandbox::{Budget, BudgetGuard};

use super::incremental::{materialise, save_incremental, ObjectMutation};
use super::{
    compose, content_refs, decode_content, dict_get, encode_content, invert, is_regular_start,
    page_refs, rebuild_stream, stream_filter, Cursor, Matrix6, IDENTITY,
};

/// What went wrong addressing or editing an object.
///
/// Every variant carries only user-facing text; none carries document bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ObjectError {
    /// The bytes are not a readable document, or a value the walk needs has
    /// the wrong shape.
    Malformed(String),
    /// A referenced object could not be read.
    Unresolvable(String),
    /// The document is encrypted; remove the password before editing.
    Encrypted,
    /// The request is invalid on its own terms (bad page, empty name).
    Invalid(String),
    /// The object is real but the requested edit is outside this slice (no
    /// placement operator, an unsupported image filter or colour space, ...).
    Unsupported(String),
    /// No object with the given id exists on that page.
    NotFound(String),
}

impl fmt::Display for ObjectError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Malformed(d)
            | Self::Unresolvable(d)
            | Self::Invalid(d)
            | Self::Unsupported(d)
            | Self::NotFound(d) => f.write_str(d),
            Self::Encrypted => {
                f.write_str("document is encrypted; remove the password before editing")
            }
        }
    }
}

impl std::error::Error for ObjectError {}

impl From<super::EditError> for ObjectError {
    fn from(e: super::EditError) -> Self {
        match e {
            super::EditError::Malformed(d) => Self::Malformed(d),
            super::EditError::Unresolvable(d) => Self::Unresolvable(d),
            super::EditError::Encrypted => Self::Encrypted,
            super::EditError::Invalid(d) => Self::Invalid(d),
            super::EditError::Unsupported(d) => Self::Unsupported(d),
        }
    }
}

/// A stable handle to one drawn object on a page.
///
/// `name` is the XObject resource name without its leading slash (empty for a
/// vector path); `op_index` is the 0-based index of the drawing operator in
/// the page's joined content stream. Operator-only edits keep both stable, so
/// the id can be handed back to a mutation after a read-back.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ObjectId {
    /// XObject resource name (without `/`), empty for a vector path.
    pub name: String,
    /// 0-based index of the `Do`/paint operator in the joined content stream.
    pub op_index: u32,
}

impl ObjectId {
    /// An id from a resource name and an operator index.
    ///
    /// # Budget
    ///
    /// One allocation for the name; nothing else.
    ///
    /// # Malformed Input
    ///
    /// No validation: a caller-constructed id that names no object is refused
    /// with [`ObjectError::NotFound`] at the point it is used.
    #[must_use]
    pub fn new(name: &str, op_index: u32) -> Self {
        Self {
            name: name.to_string(),
            op_index,
        }
    }
}

impl fmt::Display for ObjectId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.name.is_empty() {
            write!(f, "path@{}", self.op_index)
        } else {
            write!(f, "{}@{}", self.name, self.op_index)
        }
    }
}

/// An image XObject drawn on a page, in PDF user space.
#[derive(Debug, Clone, PartialEq)]
pub struct ImageObject {
    /// Stable handle for mutations.
    pub id: ObjectId,
    /// Resource name without the leading slash.
    pub name: String,
    /// Indirect object number of the XObject, when it is an indirect object.
    pub object_number: Option<u32>,
    /// Placement rectangle in PDF user space (the unit square under the CTM).
    pub rect: Rect,
    /// Pixel width from the XObject's `/Width`.
    pub pixel_width: u32,
    /// Pixel height from the XObject's `/Height`.
    pub pixel_height: u32,
    /// Stream filter name (`None` for a raw stream), as reported by the file.
    pub filter: Option<String>,
}

/// A painted vector path on a page, in PDF user space.
#[derive(Debug, Clone, PartialEq)]
pub struct VectorObject {
    /// Stable handle for mutations.
    pub id: ObjectId,
    /// The painting operator (`f`, `F`, `f*`, `S`, `s`, `B`, `B*`, `b`, `b*`).
    pub paint_op: String,
    /// Axis-aligned bounding box of the path in PDF user space.
    pub rect: Rect,
}

/// A parsed document plus the resolved page reference and dictionary.
type ResolvedPage = (selis_pdf_cos::Doc, Ref, Vec<(Bytes, Obj)>);

/// What an object mutation committed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObjectEditReport {
    /// The 0-based page edited.
    pub page: u32,
    /// The object addressed.
    pub object: ObjectId,
    /// Input size in bytes.
    pub bytes_in: u64,
    /// Output size in bytes (always ≥ `bytes_in`: an incremental append).
    pub bytes_out: u64,
    /// Number of COS objects redefined in the appended revision.
    pub objects_changed: usize,
    /// Whether the original bytes are a verbatim prefix of the output.
    pub prefix_preserved: bool,
}

/// Filter/shape metadata of one image XObject resource.
#[derive(Debug, Clone, PartialEq)]
struct ImageMeta {
    object_number: Option<u32>,
    width: u32,
    height: u32,
    filter: Option<String>,
}

/// The six operands of the placement `cm`, with the CTM before it.
#[derive(Debug, Clone)]
struct Placement {
    /// Byte span of the six numeric operands (whitespace between them kept).
    span: Range<usize>,
    /// CTM in force before the `cm` was applied.
    prev: Matrix6,
}

/// Which kind of draw an enumerated object is.
#[derive(Debug, Clone)]
enum Kind {
    Image { name: String, meta: ImageMeta },
    Vector { paint_op: String },
}

/// An enumerated object plus the byte spans the mutations need.
#[derive(Debug, Clone)]
struct FoundObject {
    id: ObjectId,
    rect: Rect,
    /// The object's box in its own (pre-CTM) space: the unit square for an
    /// image, the path's user-space bbox at construction for a vector path.
    local: (f64, f64, f64, f64),
    /// The placement `cm` (None when the draw had no local transform).
    placement: Option<Placement>,
    /// CTM at the draw, used to compute a new placement matrix.
    ctm: Matrix6,
    /// Span of the draw itself: `/Name Do`, or the path plus its paint op.
    op_span: Range<usize>,
    kind: Kind,
}

/// A numeric operand with its byte span.
#[derive(Debug, Clone)]
struct Num {
    span: Range<usize>,
    value: f64,
}

/// A name operand with its byte span.
#[derive(Debug, Clone)]
struct Name {
    span: Range<usize>,
    value: String,
}

/// The parsed shape of one page, loaded once for a list or mutation.
struct LoadedPage {
    content: Vec<u8>,
    filter: Option<Bytes>,
    stream_dict: Vec<(Bytes, Obj)>,
    content_refs: Vec<Ref>,
    page_dict: Vec<(Bytes, Obj)>,
    page_ref: Ref,
    image_meta: BTreeMap<String, ImageMeta>,
}

/// Parse `src` and resolve the 0-based page's dictionary.
///
/// # Budget / Malformed Input
///
/// Charges the revision parse, page-tree walk and page resolve. An unreadable
/// document, an out-of-range page or an encrypted document is a typed error.
fn resolve_page(
    src: &[u8],
    page_index: u32,
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
) -> Result<ResolvedPage, ObjectError> {
    let start = selis_pdf_cos::xref::find_startxref(src, 4096)
        .ok_or_else(|| ObjectError::Malformed("no startxref; not a readable PDF".to_string()))?;
    let doc = selis_pdf_cos::parse_revisions(src, start, budget, g)
        .map_err(|e| ObjectError::Malformed(format!("cannot open: {e}")))?;
    if super::is_encrypted(&doc) {
        return Err(ObjectError::Encrypted);
    }
    let refs = page_refs(src, &doc, budget, g)?;
    let page_ref = *refs.get(page_index as usize).ok_or_else(|| {
        ObjectError::Invalid(format!(
            "page {page_index} is out of range; the document has {} page(s)",
            refs.len()
        ))
    })?;
    let obj = selis_pdf_cos::copy::resolve_ref(src, &doc, page_ref, budget, g)
        .map_err(|e| ObjectError::Unresolvable(format!("cannot read the page: {e}")))?;
    let Obj::Dict(page_dict) = obj else {
        return Err(ObjectError::Malformed(
            "the page is not a dictionary".to_string(),
        ));
    };
    Ok((doc, page_ref, page_dict))
}

/// Load a page's joined content and its image-resource metadata.
///
/// # Budget / Malformed Input
///
/// Charges each stream decode and resource resolve. A page without a readable
/// content stream, a non-Flate filter or a malformed resource dictionary is a
/// typed error.
fn load_page(
    src: &[u8],
    doc: &selis_pdf_cos::Doc,
    page_ref: Ref,
    page_dict: &[(Bytes, Obj)],
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
) -> Result<LoadedPage, ObjectError> {
    let refs = content_refs(page_dict)?;
    let mut content: Vec<u8> = Vec::new();
    let mut filter: Option<Bytes> = None;
    let mut stream_dict: Vec<(Bytes, Obj)> = Vec::new();
    for (i, r) in refs.iter().enumerate() {
        let obj = selis_pdf_cos::copy::resolve_ref(src, doc, *r, budget, g)
            .map_err(|e| ObjectError::Unresolvable(format!("cannot read content: {e}")))?;
        let Obj::Stream { dict, data: part } = obj else {
            return Err(ObjectError::Malformed(
                "page content is not a stream".to_string(),
            ));
        };
        let part_filter = stream_filter(&dict)?;
        let decoded = decode_content(part_filter.clone(), part.as_slice(), budget, g)?;
        if i == 0 {
            filter = part_filter;
            stream_dict = dict;
        }
        if !content.is_empty() {
            content.push(b'\n');
        }
        content.extend_from_slice(&decoded);
    }
    let image_meta = image_resources(src, doc, page_dict, budget, g)?;
    Ok(LoadedPage {
        content,
        filter,
        stream_dict,
        content_refs: refs,
        page_dict: page_dict.to_vec(),
        page_ref,
        image_meta,
    })
}

/// The page's image XObject resources, keyed by resource name.
///
/// # Budget / Malformed Input
///
/// Charges each resource resolve. A non-dictionary `/Resources` or `/XObject`
/// is refused; a draw naming a non-image XObject is simply not listed.
fn image_resources(
    src: &[u8],
    doc: &selis_pdf_cos::Doc,
    page_dict: &[(Bytes, Obj)],
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
) -> Result<BTreeMap<String, ImageMeta>, ObjectError> {
    let mut out = BTreeMap::new();
    let Some(resources) = dict_get(page_dict, b"Resources") else {
        return Ok(out);
    };
    let resources = resolve_dict(src, doc, resources, budget, g, "Resources")?;
    let Some(xobjects) = dict_get(&resources, b"XObject") else {
        return Ok(out);
    };
    let xobjects = resolve_dict(src, doc, xobjects, budget, g, "XObject")?;
    for (name, value) in &xobjects {
        let (object_number, obj) = match value {
            Obj::Ref(r) => (
                Some(r.num),
                selis_pdf_cos::copy::resolve_ref(src, doc, *r, budget, g).map_err(|e| {
                    ObjectError::Unresolvable(format!("cannot read an XObject: {e}"))
                })?,
            ),
            other => (None, other.clone()),
        };
        let Obj::Stream { dict, .. } = &obj else {
            continue;
        };
        if !dict_get(dict, b"Subtype")
            .is_some_and(|v| matches!(v, Obj::Name(n) if n.as_slice() == b"Image"))
        {
            continue;
        }
        let int = |key: &[u8]| -> Option<u32> {
            match dict_get(dict, key) {
                Some(Obj::Int(v)) => u32::try_from(*v).ok(),
                _ => None,
            }
        };
        let width = int(b"Width").unwrap_or(0);
        let height = int(b"Height").unwrap_or(0);
        let filter = match dict_get(dict, b"Filter") {
            Some(Obj::Name(n)) => Some(String::from_utf8_lossy(n.as_slice()).into_owned()),
            Some(Obj::Array(items)) => match items.first() {
                Some(Obj::Name(n)) => Some(String::from_utf8_lossy(n.as_slice()).into_owned()),
                _ => Some("unknown".to_string()),
            },
            _ => None,
        };
        out.insert(
            String::from_utf8_lossy(name.as_slice()).into_owned(),
            ImageMeta {
                object_number,
                width,
                height,
                filter,
            },
        );
    }
    Ok(out)
}

/// Resolve a dictionary that may be direct or indirect.
fn resolve_dict(
    src: &[u8],
    doc: &selis_pdf_cos::Doc,
    value: &Obj,
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
    what: &str,
) -> Result<Vec<(Bytes, Obj)>, ObjectError> {
    let obj = match value {
        Obj::Ref(r) => selis_pdf_cos::copy::resolve_ref(src, doc, *r, budget, g)
            .map_err(|e| ObjectError::Unresolvable(format!("cannot read /{what}: {e}")))?,
        other => other.clone(),
    };
    match obj {
        Obj::Dict(dict) => Ok(dict),
        _ => Err(ObjectError::Malformed(format!(
            "/{what} is not a dictionary"
        ))),
    }
}

/// Scan one content stream for image draws and painted paths.
///
/// The walk mirrors the redaction rewriter: it tracks the CTM across `q`/`Q`
/// and `cm`, records numeric/name operand spans, and counts operators so ids
/// are stable. Form XObjects and inline images are not recursed into.
fn scan_content(content: &[u8], image_meta: &BTreeMap<String, ImageMeta>) -> Vec<FoundObject> {
    let names: BTreeSet<&str> = image_meta.keys().map(String::as_str).collect();
    let mut cur = Cursor::new(content);
    let mut out: Vec<FoundObject> = Vec::new();
    let mut ctm: Matrix6 = IDENTITY;
    let mut stack: Vec<(Matrix6, Option<Placement>)> = Vec::new();
    let mut placement: Option<Placement> = None;
    let mut numbers: Vec<Num> = Vec::new();
    let mut last_name: Option<Name> = None;
    let mut depth = 0usize;
    let mut op_index = 0u32;
    // Path state: the start of its first operand, its user-space bbox, the CTM
    // at construction, and the placement cm in force then.
    let mut path_start: Option<usize> = None;
    let mut path_bbox: Option<(f64, f64, f64, f64)> = None;
    let mut path_ctm: Matrix6 = IDENTITY;
    let mut path_placement: Option<Placement> = None;

    loop {
        cur.skip_ws_and_comments();
        let start = cur.pos();
        let Some(byte) = cur.peek() else { break };
        match byte {
            b'(' => {
                let _ = cur.read_literal_string();
                numbers.push(Num {
                    span: start..cur.pos(),
                    value: 0.0,
                });
            }
            b'<' => {
                if cur.peek_at(1) == Some(b'<') {
                    cur.skip_dict();
                } else {
                    let _ = cur.read_hex_string();
                }
                numbers.push(Num {
                    span: start..cur.pos(),
                    value: 0.0,
                });
            }
            b'[' => {
                cur.bump();
                depth = depth.saturating_add(1);
            }
            b']' => {
                cur.bump();
                depth = depth.saturating_sub(1);
            }
            b'/' => {
                let value = cur.read_name_token();
                last_name = Some(Name {
                    span: start..cur.pos(),
                    value: String::from_utf8_lossy(&value).into_owned(),
                });
            }
            b'%' => cur.skip_comment(),
            b if b.is_ascii_digit() || b == b'+' || b == b'-' || b == b'.' => {
                let value = cur.read_number();
                if depth == 0 {
                    numbers.push(Num {
                        span: start..cur.pos(),
                        value,
                    });
                }
            }
            b if is_regular_start(b) => {
                let word = cur.read_word();
                op_index = op_index.saturating_add(1);
                let op = word.as_slice();
                match op {
                    b"q" => stack.push((ctm, placement.clone())),
                    b"Q" => {
                        if let Some((saved_ctm, saved_placement)) = stack.pop() {
                            ctm = saved_ctm;
                            placement = saved_placement;
                        }
                    }
                    b"cm" => {
                        if numbers.len() >= 6 {
                            let first = numbers.len().saturating_sub(6);
                            let m = matrix_from(&numbers, first);
                            let span = numbers.get(first).map(|n| n.span.start).unwrap_or(0)
                                ..numbers
                                    .get(numbers.len().saturating_sub(1))
                                    .map(|n| n.span.end)
                                    .unwrap_or(0);
                            placement = Some(Placement { span, prev: ctm });
                            ctm = compose(m, ctm);
                        }
                    }
                    b"m" | b"l" | b"c" | b"v" | b"y" | b"re" | b"h" => {
                        if path_start.is_none() {
                            path_start = numbers.first().map(|n| n.span.start);
                            path_ctm = ctm;
                            path_placement = placement.clone();
                        }
                        extend_path_bbox(&mut path_bbox, op, &numbers);
                    }
                    b"f" | b"F" | b"f*" | b"S" | b"s" | b"B" | b"B*" | b"b" | b"b*" => {
                        if let (Some(pb), Some(ps)) = (path_bbox, path_start) {
                            out.push(FoundObject {
                                id: ObjectId::new("", op_index),
                                rect: transform_bbox(ctm, pb),
                                local: pb,
                                placement: path_placement,
                                ctm: path_ctm,
                                op_span: ps..cur.pos(),
                                kind: Kind::Vector {
                                    paint_op: String::from_utf8_lossy(op).into_owned(),
                                },
                            });
                        }
                        path_start = None;
                        path_bbox = None;
                        path_placement = None;
                    }
                    b"n" => {
                        // A clip/end-path: closes the path without painting.
                        path_start = None;
                        path_bbox = None;
                        path_placement = None;
                    }
                    b"Do" => {
                        if let Some(name) = last_name.clone() {
                            if names.contains(name.value.as_str()) {
                                let meta =
                                    image_meta.get(&name.value).cloned().unwrap_or(ImageMeta {
                                        object_number: None,
                                        width: 0,
                                        height: 0,
                                        filter: None,
                                    });
                                out.push(FoundObject {
                                    id: ObjectId::new(&name.value, op_index),
                                    rect: transform_bbox(ctm, (0.0, 0.0, 1.0, 1.0)),
                                    local: (0.0, 0.0, 1.0, 1.0),
                                    placement: placement.clone(),
                                    ctm,
                                    op_span: name.span.start..cur.pos(),
                                    kind: Kind::Image {
                                        name: name.value,
                                        meta,
                                    },
                                });
                            }
                        }
                    }
                    _ => {}
                }
                numbers.clear();
                last_name = None;
            }
            _ => {
                cur.bump();
            }
        }
    }
    out
}

/// Read six matrix numbers starting at `at` (missing numbers read as zero).
fn matrix_from(numbers: &[Num], at: usize) -> Matrix6 {
    let value = |i: usize| {
        numbers
            .get(at.saturating_add(i))
            .map(|n| n.value)
            .unwrap_or(0.0)
    };
    [value(0), value(1), value(2), value(3), value(4), value(5)]
}

/// Grow a path's user-space bounding box from one construction operator.
fn extend_path_bbox(bbox: &mut Option<(f64, f64, f64, f64)>, op: &[u8], numbers: &[Num]) {
    let take = match op {
        b"re" => 4usize,
        b"m" | b"l" => 2,
        b"v" | b"y" => 4,
        b"c" => 6,
        _ => 0,
    };
    if take == 0 || numbers.len() < take {
        return;
    }
    let first = numbers.len().saturating_sub(take);
    let mut iter = numbers.iter().skip(first).take(take).map(|n| n.value);
    let mut pts: Vec<(f64, f64)> = Vec::new();
    if op == b"re" {
        let x = iter.next().unwrap_or(0.0);
        let y = iter.next().unwrap_or(0.0);
        let w = iter.next().unwrap_or(0.0);
        let h = iter.next().unwrap_or(0.0);
        pts.push((x, y));
        pts.push((x + w, y + h));
    } else {
        while let (Some(x), Some(y)) = (iter.next(), iter.next()) {
            pts.push((x, y));
        }
    }
    for (x, y) in pts {
        match bbox {
            Some((x0, y0, x1, y1)) => {
                *x0 = x0.min(x);
                *y0 = y0.min(y);
                *x1 = x1.max(x);
                *y1 = y1.max(y);
            }
            None => *bbox = Some((x, y, x, y)),
        }
    }
}

/// Axis-aligned bounding box of a user-space box pushed through `ctm`.
fn transform_bbox(ctm: Matrix6, bbox: (f64, f64, f64, f64)) -> Rect {
    let [a, b, c, d, e, f] = ctm;
    let (x0, y0, x1, y1) = bbox;
    let mut min_x = f64::INFINITY;
    let mut min_y = f64::INFINITY;
    let mut max_x = f64::NEG_INFINITY;
    let mut max_y = f64::NEG_INFINITY;
    for (x, y) in [(x0, y0), (x1, y0), (x0, y1), (x1, y1)] {
        let px = a * x + c * y + e;
        let py = b * x + d * y + f;
        min_x = min_x.min(px);
        min_y = min_y.min(py);
        max_x = max_x.max(px);
        max_y = max_y.max(py);
    }
    Rect::new(min_x, min_y, max_x, max_y)
}

/// Format a matrix as six PDF number operands, joined by single spaces.
fn format_matrix(m: Matrix6) -> Vec<u8> {
    let [a, b, c, d, e, f] = m;
    [a, b, c, d, e, f]
        .iter()
        .map(|n| format_number(*n))
        .collect::<Vec<_>>()
        .join(" ")
        .into_bytes()
}

/// A compact, deterministic PDF real: integers print without `.0`, and no
/// value prints in exponent form (PDF numbers do not allow `1e-5`).
fn format_number(n: f64) -> String {
    if !n.is_finite() {
        return "0".to_string();
    }
    if n.fract() == 0.0 {
        return format!("{n:.0}");
    }
    let s = format!("{n}");
    if s.contains('e') || s.contains('E') {
        format!("{n:.9}")
            .trim_end_matches('0')
            .trim_end_matches('.')
            .to_string()
    } else {
        s
    }
}

/// Find one object by id, failing loudly when it is absent.
///
/// # Budget / Malformed Input
///
/// The page is parsed and scanned (see [`load_page`]). An unknown id is a
/// typed [`ObjectError::NotFound`]; no document bytes are written.
fn find_object(
    src: &[u8],
    page_index: u32,
    id: &ObjectId,
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
) -> Result<(LoadedPage, FoundObject), ObjectError> {
    let (doc, page_ref, page_dict) = resolve_page(src, page_index, budget, g)?;
    let page = load_page(src, &doc, page_ref, &page_dict, budget, g)?;
    let found = scan_content(&page.content, &page.image_meta)
        .into_iter()
        .find(|o| &o.id == id)
        .ok_or_else(|| {
            ObjectError::NotFound(format!(
                "no object {id} on page {page_index}; list the page's objects first"
            ))
        })?;
    Ok((page, found))
}

/// Commit content-stream mutations and/or an image-object replacement.
///
/// # Budget / Malformed Input
///
/// Materialises the source and charges the incremental save. A refusal from
/// the writer (signature/MDP, verification mismatch) is forwarded unchanged.
fn commit(
    src: &[u8],
    mutations: &mut [ObjectMutation],
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
) -> Result<Vec<u8>, ObjectError> {
    let model = materialise(src, budget, g)
        .map_err(|e| ObjectError::Unresolvable(format!("cannot materialise: {e}")))?;
    let (out, _report) = save_incremental(src, &model, mutations, budget, g)
        .map_err(|e| ObjectError::Unsupported(format!("cannot save: {e}")))?;
    Ok(out)
}

/// Push a rewritten content stream (and, for a multi-stream page, a repointed
/// `/Contents`) onto the mutation list.
fn push_content_mutation(page: &LoadedPage, mutations: &mut Vec<ObjectMutation>, content: Vec<u8>) {
    let (encoded, filter_name) = encode_content(page.filter.clone(), &content);
    let stream = rebuild_stream(&page.stream_dict, encoded, filter_name);
    let first = page.content_refs.first().map(|r| r.num).unwrap_or(0);
    mutations.push(ObjectMutation::SetObject {
        num: first,
        value: stream,
    });
    if page.content_refs.len() > 1 {
        let dict: Vec<(Bytes, Obj)> = page
            .page_dict
            .iter()
            .map(|(k, v)| {
                if k.as_slice() == b"Contents" {
                    (k.clone(), Obj::Ref(Ref::new(first, 0)))
                } else {
                    (k.clone(), v.clone())
                }
            })
            .collect();
        mutations.push(ObjectMutation::SetObject {
            num: page.page_ref.num,
            value: Obj::Dict(dict),
        });
    }
}

/// Build a report from an incremental-save result.
///
/// The caller passes the output bytes so the size and prefix evidence are
/// measured on what was actually produced.
fn report_for(
    src: &[u8],
    out: &[u8],
    page_index: u32,
    object: ObjectId,
    mutations: &[ObjectMutation],
) -> ObjectEditReport {
    ObjectEditReport {
        page: page_index,
        object,
        bytes_in: u64::try_from(src.len()).unwrap_or(u64::MAX),
        bytes_out: u64::try_from(out.len()).unwrap_or(u64::MAX),
        objects_changed: mutations.len(),
        prefix_preserved: out.get(..src.len()) == Some(src),
    }
}

/// List the image XObjects drawn on one page, in content order.
///
/// Rectangles are in PDF user space. The operator index in each [`ObjectId`]
/// is stable under the operand-only edits in this module.
///
/// # Budget
///
/// Linear in the content-stream length plus the page's XObject resources;
/// every decode and resolve is charged to `budget`/`g`.
///
/// # Malformed Input
///
/// An unreadable document, a missing page, an unsupported content filter, or
/// a malformed resource dictionary is a typed error. A draw with no image
/// resource of that name is ignored.
pub fn list_image_objects(
    src: &[u8],
    page_index: u32,
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
) -> Result<Vec<ImageObject>, ObjectError> {
    let (doc, page_ref, page_dict) = resolve_page(src, page_index, budget, g)?;
    let page = load_page(src, &doc, page_ref, &page_dict, budget, g)?;
    Ok(scan_content(&page.content, &page.image_meta)
        .into_iter()
        .filter_map(|o| match o.kind {
            Kind::Image { name, meta } => Some(ImageObject {
                id: o.id,
                object_number: meta.object_number,
                rect: o.rect,
                pixel_width: meta.width,
                pixel_height: meta.height,
                filter: meta.filter,
                name,
            }),
            Kind::Vector { .. } => None,
        })
        .collect())
}

/// List the painted vector paths on one page, in content order.
///
/// # Budget
///
/// Linear in the content-stream length, charged to `budget`/`g`.
///
/// # Malformed Input
///
/// As [`list_image_objects`]. A path with no constructible bounding box is
/// skipped; a clip-only path (`W n`) is not a painted object and is skipped.
pub fn list_vector_objects(
    src: &[u8],
    page_index: u32,
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
) -> Result<Vec<VectorObject>, ObjectError> {
    let (doc, page_ref, page_dict) = resolve_page(src, page_index, budget, g)?;
    let page = load_page(src, &doc, page_ref, &page_dict, budget, g)?;
    Ok(scan_content(&page.content, &page.image_meta)
        .into_iter()
        .filter_map(|o| match o.kind {
            Kind::Vector { paint_op } => Some(VectorObject {
                id: o.id,
                paint_op,
                rect: o.rect,
            }),
            Kind::Image { .. } => None,
        })
        .collect())
}

/// The placement matrix the draw needs after a move or resize.
///
/// `prev` is the CTM before the placement `cm`, so the new `cm` operands are
/// `desired_full_ctm × prev⁻¹` — the rest of the CTM stack is untouched. A
/// singular `prev` is refused (there is no unique operand rewrite).
fn new_placement_operands(found: &FoundObject, desired: Matrix6) -> Result<Vec<u8>, ObjectError> {
    let placement = found.placement.as_ref().ok_or_else(|| {
        ObjectError::Unsupported(
            "the object has no local placement cm to rewrite; wrap it in q ... cm ... Q"
                .to_string(),
        )
    })?;
    let prev_inv = invert(placement.prev).ok_or_else(|| {
        ObjectError::Unsupported(
            "the transform before this object is singular; cannot reposition it".to_string(),
        )
    })?;
    Ok(format_matrix(compose(desired, prev_inv)))
}

/// Move a drawn object (image or vector path) by `dx`, `dy` in PDF user space.
///
/// Only the six operands of the object's placement `cm` change; the rest of
/// the content stream and the whole original file are byte-identical.
///
/// # Budget
///
/// As [`list_image_objects`], plus one incremental save, charged to `budget`.
///
/// # Malformed Input
///
/// An unknown id is [`ObjectError::NotFound`]. An object with no placement
/// `cm`, or one under a singular transform, is refused. Read-back is performed
/// by the incremental writer (the reopened model must match the edit).
pub fn move_object(
    src: &[u8],
    page_index: u32,
    id: &ObjectId,
    dx: f64,
    dy: f64,
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
) -> Result<(Vec<u8>, ObjectEditReport), ObjectError> {
    if !dx.is_finite() || !dy.is_finite() {
        return Err(ObjectError::Invalid(
            "move deltas must be finite numbers".to_string(),
        ));
    }
    let (page, found) = find_object(src, page_index, id, budget, g)?;
    // A page-space translation composed AFTER the current CTM; `compose` keeps
    // the rest of the transform (shear/rotation/scale) intact.
    let translate = [1.0, 0.0, 0.0, 1.0, dx, dy];
    let desired = compose(found.ctm, translate);
    let operands = new_placement_operands(&found, desired)?;
    let mut content = Vec::with_capacity(page.content.len());
    content.extend_from_slice(page.content.get(..found.placement_start()).unwrap_or(&[]));
    content.extend_from_slice(&operands);
    content.extend_from_slice(page.content.get(found.placement_end()..).unwrap_or(&[]));
    let mut mutations = Vec::new();
    push_content_mutation(&page, &mut mutations, content);
    let out = commit(src, &mut mutations, budget, g)?;
    let report = report_for(src, &out, page_index, id.clone(), &mutations);
    Ok((out, report))
}

impl FoundObject {
    /// Start byte of the placement `cm` operands.
    fn placement_start(&self) -> usize {
        self.placement.as_ref().map(|p| p.span.start).unwrap_or(0)
    }

    /// End byte of the placement `cm` operands.
    fn placement_end(&self) -> usize {
        self.placement.as_ref().map(|p| p.span.end).unwrap_or(0)
    }
}

/// Resize a drawn object to the given axis-aligned rectangle in PDF user space.
///
/// The placement `cm` is rewritten so the image unit square (or the path's
/// local box) maps onto `rect`. Only the six operands of that `cm` change.
///
/// # Budget
///
/// As [`move_object`].
///
/// # Malformed Input
///
/// An unknown id is [`ObjectError::NotFound`]. A non-finite, zero-area or
/// inverted `rect`, or a draw without a local `cm`, is refused.
pub fn resize_object(
    src: &[u8],
    page_index: u32,
    id: &ObjectId,
    rect: Rect,
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
) -> Result<(Vec<u8>, ObjectEditReport), ObjectError> {
    if !rect.x0.is_finite()
        || !rect.y0.is_finite()
        || !rect.x1.is_finite()
        || !rect.y1.is_finite()
        || rect.width() <= 0.0
        || rect.height() <= 0.0
    {
        return Err(ObjectError::Invalid(
            "the target rect must be finite, positive-area and ordered".to_string(),
        ));
    }
    let (page, found) = find_object(src, page_index, id, budget, g)?;
    // Map the object's own box onto the target rect. For an image `local` is
    // the unit square; for a path it is the path's user-space bbox, so a path
    // with coordinates 5..35 also lands exactly on the requested rectangle.
    let (lx0, ly0, lx1, ly1) = found.local;
    let local_w = lx1 - lx0;
    let local_h = ly1 - ly0;
    if local_w <= 0.0 || local_h <= 0.0 || !local_w.is_finite() || !local_h.is_finite() {
        return Err(ObjectError::Unsupported(
            "the object has no positive-area local box to resize".to_string(),
        ));
    }
    let sx = rect.width() / local_w;
    let sy = rect.height() / local_h;
    let desired = [sx, 0.0, 0.0, sy, rect.x0 - lx0 * sx, rect.y0 - ly0 * sy];
    let operands = new_placement_operands(&found, desired)?;
    let mut content = Vec::with_capacity(page.content.len());
    content.extend_from_slice(page.content.get(..found.placement_start()).unwrap_or(&[]));
    content.extend_from_slice(&operands);
    content.extend_from_slice(page.content.get(found.placement_end()..).unwrap_or(&[]));
    let mut mutations = Vec::new();
    push_content_mutation(&page, &mut mutations, content);
    let out = commit(src, &mut mutations, budget, g)?;
    let report = report_for(src, &out, page_index, id.clone(), &mutations);
    Ok((out, report))
}

/// Delete a drawn object from the page's content stream.
///
/// For an image, the `/Name Do` operator and its name are removed; for a path,
/// its construction operands and paint operator are removed. Everything else
/// in the stream — including the surrounding `q`/`Q` — stays byte-identical.
///
/// # Budget
///
/// As [`move_object`].
///
/// # Malformed Input
///
/// An unknown id is [`ObjectError::NotFound`]. The image XObject itself is
/// deliberately left in the file (an unreferenced resource is harmless);
/// reclaiming its bytes is a full-rewrite concern, not an incremental one.
pub fn delete_object(
    src: &[u8],
    page_index: u32,
    id: &ObjectId,
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
) -> Result<(Vec<u8>, ObjectEditReport), ObjectError> {
    let (page, found) = find_object(src, page_index, id, budget, g)?;
    let mut content = Vec::with_capacity(page.content.len());
    content.extend_from_slice(page.content.get(..found.op_span.start).unwrap_or(&[]));
    content.extend_from_slice(page.content.get(found.op_span.end..).unwrap_or(&[]));
    let mut mutations = Vec::new();
    push_content_mutation(&page, &mut mutations, content);
    let out = commit(src, &mut mutations, budget, g)?;
    let report = report_for(src, &out, page_index, id.clone(), &mutations);
    Ok((out, report))
}

/// Replace an image XObject's pixels from a standalone PNG or JPEG file.
///
/// The new file is decoded with [`selis_image::decode`], flattened onto white
/// and written as a fresh 8-bit Flate/DeviceRGB XObject with the same indirect
/// object number. The page's content stream is left byte-identical, so every
/// other object on the page is untouched (I7).
///
/// # Budget
///
/// Decode and encode are charged to `budget`/`g`; the incremental save is
/// charged as in [`move_object`].
///
/// # Malformed Input
///
/// An unknown id is [`ObjectError::NotFound`]; a direct (non-indirect) XObject
/// or an existing filter outside `none`/Flate/DCT is refused rather than
/// guessed at. An undecodable replacement file is a typed error.
pub fn replace_image(
    src: &[u8],
    page_index: u32,
    id: &ObjectId,
    new_bytes: &[u8],
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
) -> Result<(Vec<u8>, ObjectEditReport), ObjectError> {
    let (_page, found) = find_object(src, page_index, id, budget, g)?;
    let meta = match &found.kind {
        Kind::Image { meta, .. } => meta,
        Kind::Vector { .. } => {
            return Err(ObjectError::Invalid(
                "replace_image needs an image object id".to_string(),
            ))
        }
    };
    let supported = meta
        .filter
        .as_deref()
        .is_none_or(|f| matches!(f, "FlateDecode" | "Fl" | "DCTDecode" | "DCT"));
    if !supported {
        return Err(ObjectError::Unsupported(format!(
            "image {} uses filter {:?}; this slice cannot replace it",
            id, meta.filter
        )));
    }
    let num = meta.object_number.ok_or_else(|| {
        ObjectError::Unsupported(
            "the image XObject is a direct object, not an indirect one; cannot replace it in place"
                .to_string(),
        )
    })?;
    let decoded = selis_image::decode(new_bytes, g)
        .map_err(|e| ObjectError::Unsupported(format!("cannot decode replacement image: {e}")))?;
    if decoded.width == 0 || decoded.height == 0 {
        return Err(ObjectError::Malformed(
            "the replacement image is empty".to_string(),
        ));
    }
    let rgb = selis_image::flatten_over_white(&decoded.rgba, g)
        .map_err(|e| ObjectError::Malformed(format!("cannot flatten replacement image: {e}")))?;
    let data = selis_pdf_filter::flate_encode(&rgb, super::FLATE_LEVEL);
    let len = i64::try_from(data.len()).unwrap_or(i64::MAX);
    let stream = Obj::Stream {
        dict: vec![
            (
                Bytes::copy_from_slice(b"Type"),
                Obj::Name(Bytes::copy_from_slice(b"XObject")),
            ),
            (
                Bytes::copy_from_slice(b"Subtype"),
                Obj::Name(Bytes::copy_from_slice(b"Image")),
            ),
            (
                Bytes::copy_from_slice(b"Width"),
                Obj::Int(i64::from(decoded.width)),
            ),
            (
                Bytes::copy_from_slice(b"Height"),
                Obj::Int(i64::from(decoded.height)),
            ),
            (
                Bytes::copy_from_slice(b"ColorSpace"),
                Obj::Name(Bytes::copy_from_slice(b"DeviceRGB")),
            ),
            (Bytes::copy_from_slice(b"BitsPerComponent"), Obj::Int(8)),
            (
                Bytes::copy_from_slice(b"Filter"),
                Obj::Name(Bytes::copy_from_slice(b"FlateDecode")),
            ),
            (Bytes::copy_from_slice(b"Length"), Obj::Int(len)),
        ],
        data: Bytes::copy_from_slice(&data),
    };
    let mut mutations = vec![ObjectMutation::SetObject { num, value: stream }];
    let out = commit(src, &mut mutations, budget, g)?;
    let report = report_for(src, &out, page_index, id.clone(), &mutations);
    Ok((out, report))
}

/// Extract an image object's pixels as a standalone image file.
///
/// A Flate/DeviceRGB (or raw RGB) XObject is returned as PNG; a DCTDecode
/// XObject is returned as its original JPEG bytes. Flate streams are decoded
/// under [`super::MAX_CONTENT_BYTES`] so a hostile stream cannot allocate
/// without bound.
///
/// # Budget
///
/// Decode/encode are charged to `budget`/`g`.
///
/// # Malformed Input
///
/// An unknown id is [`ObjectError::NotFound`]. A colour space or filter this
/// slice cannot turn into PNG/JPEG is refused loudly; no file is written.
pub fn extract_image(
    src: &[u8],
    page_index: u32,
    id: &ObjectId,
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
) -> Result<Vec<u8>, ObjectError> {
    let (doc, _, _) = resolve_page(src, page_index, budget, g)?;
    let (_page, found) = find_object(src, page_index, id, budget, g)?;
    let meta = match &found.kind {
        Kind::Image { meta, .. } => meta,
        Kind::Vector { .. } => {
            return Err(ObjectError::Invalid(
                "extract_image needs an image object id".to_string(),
            ))
        }
    };
    let num = meta.object_number.ok_or_else(|| {
        ObjectError::Unsupported(
            "the image XObject is a direct object; cannot extract its stream".to_string(),
        )
    })?;
    let obj = selis_pdf_cos::copy::resolve_ref(src, &doc, Ref::new(num, 0), budget, g)
        .map_err(|e| ObjectError::Unresolvable(format!("cannot read image stream: {e}")))?;
    let Obj::Stream { dict, data } = obj else {
        return Err(ObjectError::Malformed(
            "the image XObject is not a stream".to_string(),
        ));
    };
    let filter = meta.filter.as_deref();
    let raw_rgb: Option<Vec<u8>> = match filter {
        None => Some(data.as_slice().to_vec()),
        Some("FlateDecode") | Some("Fl") => Some(
            selis_pdf_filter::decode("FlateDecode", data.as_slice(), super::MAX_CONTENT_BYTES, g)
                .map_err(|e| ObjectError::Malformed(format!("cannot decode image Flate: {e}")))?,
        ),
        Some("DCTDecode") | Some("DCT") => {
            if data.as_slice().get(..2) != Some([0xFF, 0xD8].as_slice()) {
                return Err(ObjectError::Malformed(
                    "DCTDecode image does not start with a JPEG SOI marker".to_string(),
                ));
            }
            return Ok(data.as_slice().to_vec());
        }
        Some(other) => {
            return Err(ObjectError::Unsupported(format!(
                "image filter {other:?} cannot be extracted by this slice"
            )))
        }
    };
    let is_rgb8 = dict_get(&dict, b"ColorSpace")
        .map(|v| matches!(v, Obj::Name(n) if n.as_slice() == b"DeviceRGB"))
        .unwrap_or(false)
        && dict_get(&dict, b"BitsPerComponent")
            .map(|v| matches!(v, Obj::Int(v) if *v == 8))
            .unwrap_or(false);
    if !is_rgb8 {
        return Err(ObjectError::Unsupported(
            "only 8-bit DeviceRGB images can be extracted as PNG".to_string(),
        ));
    }
    let Some(rgb) = raw_rgb else {
        return Err(ObjectError::Unsupported(
            "image has no decodable sample stream".to_string(),
        ));
    };
    let mut rgba = Vec::with_capacity(rgb.len().saturating_mul(4).saturating_div(3));
    for px in rgb.chunks_exact(3) {
        let r = px.first().copied().unwrap_or(0);
        let gch = px.get(1).copied().unwrap_or(0);
        let b = px.get(2).copied().unwrap_or(0);
        rgba.extend_from_slice(&[r, gch, b, 255]);
    }
    let image = selis_image::Image {
        width: meta.width,
        height: meta.height,
        rgba,
    };
    selis_image::encode_png(&image, super::FLATE_LEVEL, g)
        .map_err(|e| ObjectError::Malformed(format!("cannot encode extracted PNG: {e}")))
}
