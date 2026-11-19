//! Add content to an existing PDF: text boxes, images, shapes, links, and the
//! derived page furniture (SL-5.TEXTEDIT.07 and SL-5.TEXTEDIT.08).
//!
//! This is the additive half of the edit model. Everything here lowers to a
//! set of COS object definitions that are committed through
//! [`crate::incremental::save_incremental`]: the original bytes stay a verbatim
//! prefix, the page's *existing* content stream object is never rewritten (a
//! new stream is added and the page is repointed at it), and the incremental
//! writer's read-back plus signature guard still apply. Because the added
//! stream is separate, every untouched byte of the original content is
//! identical and — as long as the caller keeps the declared rectangles
//! disjoint — the rendered page outside the added rectangles is pixel-identical.
//!
//! # Disclosed font substitution (TEXTEDIT.03/04)
//!
//! A text addition never silently swaps a face. The requested `/BaseFont` is
//! passed through [`selis_font::match_substitute`]; when a substitution is
//! chosen the full disclosure is returned in
//! [`AddContentReport::font_disclosures`]. When no available face can carry the
//! requested characters, the request is refused with
//! [`AddContentError::Unsupported`] rather than drawing a wrong glyph. Text is
//! restricted to printable ASCII (no font is embedded here), so there is no
//! encoding guess either.
//!
//! # No upload, no I/O
//!
//! Image bytes are supplied by the caller and decoded in memory; nothing is
//! fetched, and this crate performs no file or network I/O. The caller owns
//! every byte in and out.
//!
//! # Scope (honest, tested)
//!
//! Additions target leaf `/Page` objects addressed by 0-based index. Fonts are
//! standard-14 only (written unembedded, with the substitution disclosed);
//! images are normalised to 8-bit Flate/DeviceRGB. Links are written as proper
//! `/Annots` `/Link` entries with a border appearance stream, targeting either
//! a URI or a page. There is no CLI/wasm wiring or UI placement tool here —
//! this is the engine slice; the shells consume these APIs.

#![forbid(unsafe_code)]

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use selis_bytes::Bytes;
use selis_geom::Rect;
use selis_pdf_cos::{Doc, Obj, Ref};
use selis_sandbox::{Budget, BudgetGuard};

use super::incremental::{materialise, save_incremental, ObjectModel, ObjectMutation};
use super::{dict_get, is_encrypted, page_refs};

/// A marker comment prefixed to every stream this module adds. It is inert PDF
/// (a comment) and gives tests an exact read-back oracle for the added bytes.
const MARKER: &[u8] = b"% SelisAddContent\n";
/// Largest text string accepted, in bytes.
const MAX_TEXT_BYTES: usize = 1 << 20;
/// Largest caller-supplied image accepted before decoding, in bytes.
const MAX_IMAGE_BYTES: usize = 64 << 20;
/// Largest single added content stream, in bytes (the added ops only).
const MAX_ADDED_OPS: usize = 8 << 20;
/// No page-tree walk here goes deeper than this.
const MAX_TREE_DEPTH: u32 = 64;

/// What went wrong adding content.
///
/// Every variant carries only user-facing text; none carries document bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AddContentError {
    /// The bytes are not a readable document, or a value the walk needs has
    /// the wrong shape.
    Malformed(String),
    /// A referenced object could not be read.
    Unresolvable(String),
    /// The document is encrypted; remove the password before editing.
    Encrypted,
    /// The request is invalid on its own terms (empty text, bad page, non-finite
    /// or inverted rectangle, unsupported character).
    Invalid(String),
    /// The request is real but outside this slice (font cannot cover the
    /// characters, image cannot be decoded, pattern has no counter field).
    Unsupported(String),
}

impl fmt::Display for AddContentError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Malformed(d)
            | Self::Unresolvable(d)
            | Self::Invalid(d)
            | Self::Unsupported(d) => f.write_str(d),
            Self::Encrypted => {
                f.write_str("document is encrypted; remove the password before editing")
            }
        }
    }
}

impl std::error::Error for AddContentError {}

impl From<super::EditError> for AddContentError {
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

/// An RGB colour. Each component maps to a PDF `0..1` colour operand.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rgb {
    /// Red component, `0..=255`.
    pub r: u8,
    /// Green component, `0..=255`.
    pub g: u8,
    /// Blue component, `0..=255`.
    pub b: u8,
}

impl Rgb {
    /// A colour from three 8-bit components.
    #[must_use]
    pub const fn new(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b }
    }

    /// The `rg` (fill) operands for this colour.
    fn fill(self) -> String {
        format!(
            "{} {} {} rg",
            pdf_num(f64::from(self.r) / 255.0),
            pdf_num(f64::from(self.g) / 255.0),
            pdf_num(f64::from(self.b) / 255.0)
        )
    }

    /// The `RG` (stroke) operands for this colour.
    fn stroke(self) -> String {
        format!(
            "{} {} {} RG",
            pdf_num(f64::from(self.r) / 255.0),
            pdf_num(f64::from(self.g) / 255.0),
            pdf_num(f64::from(self.b) / 255.0)
        )
    }
}

/// Where an addition is drawn relative to the page's existing content.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Placement {
    /// Draw after existing content (the default: stamps sit on top).
    Overlay,
    /// Draw before existing content (a background, beneath what is already
    /// there). The original content stream object is still untouched; only the
    /// order of the page's `/Contents` array changes.
    Background,
}

/// A text box. `rect`'s lower-left corner is the text baseline origin; its
/// extent declares the region the addition claims (used by render-parity
/// callers); it does not clip the glyphs.
#[derive(Debug, Clone, PartialEq)]
pub struct TextBox {
    /// Printable-ASCII text to draw.
    pub text: String,
    /// Requested base font (a standard-14 name such as `Helvetica`).
    pub font: String,
    /// Font size in PDF user-space units.
    pub font_size: f64,
    /// Text colour.
    pub color: Rgb,
    /// Baseline origin and claimed extent, in PDF user space.
    pub rect: Rect,
}

/// An image addition. The bytes are supplied separately to
/// [`add_image`](add_image); this struct is the placement.
#[derive(Debug, Clone, PartialEq)]
pub struct AddImage {
    /// Rectangle the image is scaled into, in PDF user space.
    pub rect: Rect,
    /// Counter-clockwise rotation in degrees about the rectangle's centre.
    /// `0.0` gives an axis-aligned scale-and-translate placement.
    pub angle_degrees: f64,
}

/// The affine `[a b c d e f]` that maps the image unit square onto `rect`,
/// optionally rotated about the rectangle's centre.
fn image_matrix(rect: Rect, angle_degrees: f64) -> [f64; 6] {
    let w = rect.x1 - rect.x0;
    let h = rect.y1 - rect.y0;
    let cx = (rect.x0 + rect.x1) / 2.0;
    let cy = (rect.y0 + rect.y1) / 2.0;
    let radians = angle_degrees.to_radians();
    let (sin, cos) = radians.sin_cos();
    [
        cos * w,
        sin * w,
        -sin * h,
        cos * h,
        cx - cos * w / 2.0 + sin * h / 2.0,
        cy - sin * w / 2.0 - cos * h / 2.0,
    ]
}

/// The kind of path a [`Shape`] paints.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShapeKind {
    /// An axis-aligned rectangle (`re`).
    Rect,
    /// An ellipse inscribed in the rectangle (four cubic Béziers).
    Ellipse,
    /// A line from the rectangle's lower-left to its upper-right corner.
    Line,
}

/// A vector shape. `rect` is its bounding box in PDF user space.
#[derive(Debug, Clone, PartialEq)]
pub struct Shape {
    /// Which path to construct.
    pub kind: ShapeKind,
    /// Bounding box / endpoints, in PDF user space.
    pub rect: Rect,
    /// Fill colour, if the path is filled.
    pub fill: Option<Rgb>,
    /// Stroke colour, if the path is stroked.
    pub stroke: Option<Rgb>,
    /// Stroke line width (ignored when there is no stroke colour).
    pub stroke_width: f64,
}

/// Where a [`Link`] points.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LinkTarget {
    /// An external URI (`/A << /S /URI /URI (…) >>`).
    Uri(String),
    /// A 0-based page number in this document (`/A << /S /GoTo /D […] >>`).
    Page(u32),
}

/// A clickable link annotation.
#[derive(Debug, Clone, PartialEq)]
pub struct Link {
    /// The clickable rectangle, in PDF user space.
    pub rect: Rect,
    /// The link destination.
    pub target: LinkTarget,
    /// Border colour of the generated appearance.
    pub color: Rgb,
    /// Whether to draw a visible border in the appearance stream.
    pub border: bool,
}

/// Horizontal alignment for page furniture.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Align {
    /// Flush with the left margin.
    Left,
    /// Centred between the margins.
    Center,
    /// Flush with the right margin.
    Right,
}

/// A header or footer line, positioned from the page's `/MediaBox`.
#[derive(Debug, Clone, PartialEq)]
pub struct HeaderFooter {
    /// The text to draw.
    pub text: String,
    /// Requested base font.
    pub font: String,
    /// Font size.
    pub font_size: f64,
    /// Text colour.
    pub color: Rgb,
    /// Distance from the page edge, in user-space units.
    pub margin: f64,
    /// Horizontal alignment.
    pub align: Align,
}

/// A diagonal watermark stamped at the page centre.
#[derive(Debug, Clone, PartialEq)]
pub struct Watermark {
    /// The text to draw.
    pub text: String,
    /// Requested base font.
    pub font: String,
    /// Font size.
    pub font_size: f64,
    /// Text colour.
    pub color: Rgb,
    /// Counter-clockwise rotation in degrees.
    pub angle_degrees: f64,
    /// Fill alpha in `0.0..=1.0` (written as an `/ExtGState`).
    pub opacity: f64,
}

/// Bates numbering: stamp a sequential id on every page.
///
/// `pattern` must contain a run of ASCII digits (normally trailing zeros);
/// that run is the counter field. `start` is the value on the first page and
/// increments by one per page. The same position/alignment is used on every
/// page.
#[derive(Debug, Clone, PartialEq)]
pub struct BatesOptions {
    /// The id pattern, e.g. `CASE-000001`.
    pub pattern: String,
    /// The sequential id for the first page.
    pub start: u64,
    /// Requested base font.
    pub font: String,
    /// Font size.
    pub font_size: f64,
    /// Text colour.
    pub color: Rgb,
    /// Distance from the bottom page edge.
    pub margin: f64,
    /// Horizontal alignment.
    pub align: Align,
}

/// A solid background rectangle drawn beneath existing content.
#[derive(Debug, Clone, PartialEq)]
pub struct Background {
    /// Fill colour.
    pub color: Rgb,
    /// Rectangle to fill, in PDF user space.
    pub rect: Rect,
}

/// What an add-content operation produced, beyond the bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AddContentReport {
    /// 0-based pages stamped.
    pub pages: Vec<u32>,
    /// Input size in bytes.
    pub bytes_in: u64,
    /// Output size in bytes (always ≥ `bytes_in`: an incremental append).
    pub bytes_out: u64,
    /// Number of brand-new objects allocated in the appended revision.
    pub objects_added: usize,
    /// Total number of COS objects (re)defined in the appended revision.
    pub objects_changed: usize,
    /// Whether the original bytes are a verbatim prefix of the output.
    pub prefix_preserved: bool,
    /// Font-substitution disclosures (empty when no face changed).
    pub font_disclosures: Vec<String>,
}

/// A page's accumulated additions before the final COS rewrite.
#[derive(Clone)]
struct PageAccum {
    page_ref: Ref,
    media: Rect,
    /// The page's resolved, copied `/Resources` dictionary.
    resources: Vec<(Bytes, Obj)>,
    /// Names already used in `/Resources` (to keep additions collision-free).
    used_names: BTreeSet<String>,
    overlay: Vec<u8>,
    background: Vec<u8>,
    /// Object numbers of the streams created in `finish` (filled there).
    overlay_ref: Option<u32>,
    background_ref: Option<u32>,
    fonts: Vec<(Bytes, u32)>,
    xobjects: Vec<(Bytes, u32)>,
    gstates: Vec<(Bytes, u32)>,
    annots: Vec<u32>,
}

/// The plan being built: new objects plus per-page additions. Owns all data it
/// needs (no borrow into the parsed document) so it can outlive the parse.
struct Builder {
    model: ObjectModel,
    page_refs: Vec<Ref>,
    pages: BTreeMap<u32, PageAccum>,
    new_objects: Vec<(u32, Obj)>,
    disclosures: Vec<String>,
    next_num: u32,
}

/// A compact, deterministic PDF real. Integers print without `.0`, and no
/// value prints in exponent form (PDF numbers do not allow `1e-5`).
fn pdf_num(n: f64) -> String {
    if !n.is_finite() || n == 0.0 {
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

/// Six PDF number operands of a `[a b c d e f]` matrix.
fn matrix_ops(m: [f64; 6]) -> String {
    m.iter().map(|n| pdf_num(*n)).collect::<Vec<_>>().join(" ")
}

/// A finite `f64` as a COS number object: an integer when integral, otherwise
/// a six-decimal real. Non-finite values collapse to `0` (callers validate their
/// geometry before reaching here).
fn pdf_number(n: f64) -> Obj {
    if !n.is_finite() {
        return Obj::Int(0);
    }
    // Render to six decimals and parse the digits, so no float→integer cast
    // (which this crate forbids) is needed. Negative values keep their sign.
    let rendered = format!("{n:.6}");
    let digits: String = rendered
        .chars()
        .filter(|c| c.is_ascii_digit() || *c == '-')
        .collect();
    Obj::Real {
        scaled: digits.parse::<i64>().unwrap_or(0),
        scale: 6,
    }
}

/// A `Bytes` helper.
fn key(s: &str) -> Bytes {
    Bytes::copy_from_slice(s.as_bytes())
}

/// Append one newline-terminated line of ASCII content operators.
fn push_line(ops: &mut Vec<u8>, line: &str) {
    ops.extend_from_slice(line.as_bytes());
    ops.push(b'\n');
}

/// Validate a user-supplied rectangle: finite, ordered, positive area.
fn check_rect(rect: Rect, what: &str) -> Result<(), AddContentError> {
    if !rect.x0.is_finite()
        || !rect.y0.is_finite()
        || !rect.x1.is_finite()
        || !rect.y1.is_finite()
        || rect.x1 <= rect.x0
        || rect.y1 <= rect.y0
    {
        return Err(AddContentError::Invalid(format!(
            "{what} must be finite, ordered and positive-area"
        )));
    }
    Ok(())
}

/// Encode printable-ASCII text as a PDF literal string. A non-ASCII or control
/// character is refused: this slice embeds no font, so there is no honest way
/// to map it to a glyph.
fn pdf_literal(text: &str) -> Result<Vec<u8>, AddContentError> {
    if text.is_empty() {
        return Err(AddContentError::Invalid("text is empty".to_string()));
    }
    if text.len() > MAX_TEXT_BYTES {
        return Err(AddContentError::Invalid(format!(
            "text is larger than the {MAX_TEXT_BYTES}-byte limit"
        )));
    }
    let mut out = Vec::with_capacity(text.len().saturating_add(2));
    out.push(b'(');
    for byte in text.bytes() {
        if !byte.is_ascii() || byte < 0x20 || byte == 0x7f {
            return Err(AddContentError::Invalid(format!(
                "text box supports printable ASCII only; found byte {byte:#04x} (no font is \
                 embedded here, so a non-ASCII glyph would be a silent swap)"
            )));
        }
        if matches!(byte, b'(' | b')' | b'\\') {
            out.push(b'\\');
        }
        out.push(byte);
    }
    out.push(b')');
    Ok(out)
}

/// Resolve a dictionary that may be direct or indirect.
fn resolve_dict(
    src: &[u8],
    doc: &Doc,
    value: &Obj,
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
    what: &str,
) -> Result<Vec<(Bytes, Obj)>, AddContentError> {
    let obj = match value {
        Obj::Ref(r) => selis_pdf_cos::copy::resolve_ref(src, doc, *r, budget, g)
            .map_err(|e| AddContentError::Unresolvable(format!("cannot read /{what}: {e}")))?,
        other => other.clone(),
    };
    match obj {
        Obj::Dict(dict) => Ok(dict),
        _ => Err(AddContentError::Malformed(format!(
            "/{what} is not a dictionary"
        ))),
    }
}

/// Find a page-or-ancestor dictionary entry, walking `/Parent` (bounded).
fn inherited<'a>(
    src: &'a [u8],
    doc: &'a Doc,
    mut dict: Vec<(Bytes, Obj)>,
    want: &[u8],
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
) -> Result<Option<Obj>, AddContentError> {
    for _ in 0..MAX_TREE_DEPTH {
        if let Some(value) = dict_get(&dict, want) {
            return Ok(Some(value.clone()));
        }
        let Some(Obj::Ref(parent)) = dict_get(&dict, b"Parent") else {
            break;
        };
        let obj = selis_pdf_cos::copy::resolve_ref(src, doc, *parent, budget, g)
            .map_err(|e| AddContentError::Unresolvable(format!("cannot read /Parent: {e}")))?;
        match obj {
            Obj::Dict(next) => dict = next,
            _ => break,
        }
    }
    Ok(None)
}

/// Numeric value of a MediaBox item (integer or real).
fn box_number(item: Option<&Obj>) -> Option<f64> {
    match item {
        Some(Obj::Int(n)) => Some(f64::from(i32::try_from(*n).unwrap_or(i32::MAX))),
        Some(Obj::Real { scaled, scale }) => Some(*scaled as f64 / 10f64.powi(i32::from(*scale))),
        _ => None,
    }
}

/// The page's `/MediaBox`, from the page or an ancestor. Defaults to US Letter
/// when a (non-conformant) file omits it.
fn media_box(
    src: &[u8],
    doc: &Doc,
    dict: &[(Bytes, Obj)],
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
) -> Result<Rect, AddContentError> {
    let Some(value) = inherited(src, doc, dict.to_vec(), b"MediaBox", budget, g)? else {
        return Ok(Rect::new(0.0, 0.0, 612.0, 792.0));
    };
    let arr = match value {
        Obj::Ref(r) => selis_pdf_cos::copy::resolve_ref(src, doc, r, budget, g)
            .map_err(|e| AddContentError::Unresolvable(format!("cannot read /MediaBox: {e}")))?,
        other => other,
    };
    let Obj::Array(items) = arr else {
        return Err(AddContentError::Malformed(
            "/MediaBox is not an array".to_string(),
        ));
    };
    let nums = [
        box_number(items.first()),
        box_number(items.get(1)),
        box_number(items.get(2)),
        box_number(items.get(3)),
    ];
    if nums.iter().all(Option::is_some) {
        let v = nums.map(|n| n.unwrap_or(0.0));
        let (x0, y0, x1, y1) = (v[0], v[1], v[2], v[3]);
        if [x0, y0, x1, y1].iter().all(|n| n.is_finite()) && x1 > x0 && y1 > y0 {
            return Ok(Rect::new(x0, y0, x1, y1));
        }
    }
    Ok(Rect::new(0.0, 0.0, 612.0, 792.0))
}

impl Builder {
    /// Allocate a fresh object number above every live object.
    fn alloc(&mut self) -> u32 {
        let n = self.next_num;
        self.next_num = self.next_num.saturating_add(1);
        n
    }

    /// Get a page's accumulator, refusing an out-of-range page index.
    fn page(&self, page_index: u32) -> Result<&PageAccum, AddContentError> {
        self.pages.get(&page_index).ok_or_else(|| {
            AddContentError::Invalid(format!(
                "page {page_index} is out of range; the document has {} page(s)",
                self.page_refs.len()
            ))
        })
    }

    /// A resource name unused on this page, derived from `prefix` + a counter.
    /// Independent of document bytes so the result is deterministic.
    fn unique_name(&self, page_index: u32, prefix: &str) -> Bytes {
        let Some(accum) = self.pages.get(&page_index) else {
            return key(prefix);
        };
        let mut counter = 1u32;
        loop {
            let candidate = format!("Selis{prefix}{counter}");
            if !accum.used_names.contains(&candidate) {
                return key(&candidate);
            }
            counter = counter.saturating_add(1);
        }
    }

    fn remember_name(&mut self, page_index: u32, name: &Bytes) {
        if let Some(accum) = self.pages.get_mut(&page_index) {
            if let Ok(s) = String::from_utf8(name.as_slice().to_vec()) {
                accum.used_names.insert(s);
            }
        }
    }

    /// Choose a disclosed font face and install a standard-14 font resource for
    /// it on `page_index`. Returns the resource name (without `/`).
    fn font_resource(
        &mut self,
        page_index: u32,
        base_font: &str,
        needed: &[char],
        budget: &Budget,
    ) -> Result<Bytes, AddContentError> {
        if base_font.is_empty() {
            return Err(AddContentError::Invalid(
                "a text addition needs a non-empty base font".to_string(),
            ));
        }
        // This slice writes unembedded faces only. A name that is not one of
        // the 14 standard PDF faces has no program we can point at, so it is
        // genuinely unavailable — refuse rather than write a name a reader
        // cannot resolve (a silent swap to some viewer-chosen face).
        if !selis_font::standard14::is_standard(base_font) {
            return Err(AddContentError::Unsupported(format!(
                "font {base_font:?} is unavailable: this slice writes unembedded standard-14 \
                 faces only; no substitute is chosen silently"
            )));
        }
        let original = selis_font::OriginalFont::unembedded(base_font);
        let substitute = selis_font::font_match::match_substitute(&original, needed, budget);
        if !substitute.covers_all {
            return Err(AddContentError::Unsupported(format!(
                "no available face can draw the requested text: {}",
                if substitute.disclosure.is_empty() {
                    base_font.to_string()
                } else {
                    substitute.disclosure.clone()
                }
            )));
        }
        if substitute.substituted {
            self.disclosures.push(substitute.disclosure.clone());
        }
        // The face is written under its requested standard-14 name; the
        // disclosure records what a viewer without that face may substitute.
        let name = self.unique_name(page_index, "F");
        let num = self.alloc();
        let font = Obj::Dict(vec![
            (key("Type"), Obj::Name(key("Font"))),
            (key("Subtype"), Obj::Name(key("Type1"))),
            (key("BaseFont"), Obj::Name(key(base_font))),
            (key("Encoding"), Obj::Name(key("WinAnsiEncoding"))),
        ]);
        self.new_objects.push((num, font));
        if let Some(accum) = self.pages.get_mut(&page_index) {
            accum.fonts.push((name.clone(), num));
        }
        self.remember_name(page_index, &name);
        Ok(name)
    }

    /// Append an opacity `/ExtGState` resource, returning its name (without `/`).
    fn gstate_resource(&mut self, page_index: u32, opacity: f64) -> Result<Bytes, AddContentError> {
        if !opacity.is_finite() || opacity <= 0.0 || opacity > 1.0 {
            return Err(AddContentError::Invalid(
                "opacity must be in (0, 1]".to_string(),
            ));
        }
        let name = self.unique_name(page_index, "GS");
        let num = self.alloc();
        let alpha = pdf_number(opacity);
        let gs = Obj::Dict(vec![
            (key("Type"), Obj::Name(key("ExtGState"))),
            (key("ca"), alpha.clone()),
            (key("CA"), alpha),
        ]);
        self.new_objects.push((num, gs));
        if let Some(accum) = self.pages.get_mut(&page_index) {
            accum.gstates.push((name.clone(), num));
        }
        self.remember_name(page_index, &name);
        Ok(name)
    }

    /// Emit a text run at `(x, y)`, optionally rotated about that point.
    #[allow(clippy::too_many_arguments)]
    fn text_ops(
        &mut self,
        page_index: u32,
        text_box: &TextBox,
        x: f64,
        y: f64,
        angle_degrees: f64,
        opacity: Option<f64>,
        placement: Placement,
        budget: &Budget,
    ) -> Result<(), AddContentError> {
        self.page(page_index)?;
        check_rect(text_box.rect, "the text box rect")?;
        if !text_box.font_size.is_finite() || text_box.font_size <= 0.0 {
            return Err(AddContentError::Invalid(
                "font size must be finite and positive".to_string(),
            ));
        }
        if !angle_degrees.is_finite() {
            return Err(AddContentError::Invalid(
                "rotation must be finite".to_string(),
            ));
        }
        let literal = pdf_literal(&text_box.text)?;
        let needed: Vec<char> = text_box.text.chars().collect();
        let font = self.font_resource(page_index, &text_box.font, &needed, budget)?;
        let gs_name = match opacity {
            Some(alpha) => Some(self.gstate_resource(page_index, alpha)?),
            None => None,
        };

        let mut ops: Vec<u8> = Vec::new();
        push_line(&mut ops, "q");
        if angle_degrees != 0.0 {
            let radians = angle_degrees.to_radians();
            let (sin, cos) = radians.sin_cos();
            let m = [cos, sin, -sin, cos, x, y];
            push_line(&mut ops, &format!("{} cm", matrix_ops(m)));
        }
        if let Some(gs) = &gs_name {
            push_line(&mut ops, &format!("/{} gs", String::from_utf8_lossy(gs)));
        }
        push_line(&mut ops, "BT");
        push_line(
            &mut ops,
            &format!(
                "/{} {} Tf",
                String::from_utf8_lossy(&font),
                pdf_num(text_box.font_size)
            ),
        );
        push_line(&mut ops, &text_box.color.fill());
        let (tx, ty) = if angle_degrees == 0.0 {
            (x, y)
        } else {
            (0.0, 0.0)
        };
        push_line(&mut ops, &format!("{} {} Td", pdf_num(tx), pdf_num(ty)));
        ops.extend_from_slice(&literal);
        push_line(&mut ops, " Tj");
        push_line(&mut ops, "ET");
        push_line(&mut ops, "Q");
        self.append_ops(page_index, placement, ops)
    }

    /// Append ops to a page's overlay or background stream, enforcing the
    /// per-stream size cap.
    fn append_ops(
        &mut self,
        page_index: u32,
        placement: Placement,
        ops: Vec<u8>,
    ) -> Result<(), AddContentError> {
        if ops.len() > MAX_ADDED_OPS {
            return Err(AddContentError::Invalid(
                "the added content exceeds the per-stream size limit".to_string(),
            ));
        }
        if let Some(accum) = self.pages.get_mut(&page_index) {
            let target = match placement {
                Placement::Overlay => &mut accum.overlay,
                Placement::Background => &mut accum.background,
            };
            target.extend_from_slice(&ops);
            target.push(b'\n');
        }
        Ok(())
    }

    /// Finalise the plan into COS mutations plus the report's metadata. Does
    /// not consume `self`, so the caller can still pass `self.model` to `commit`.
    fn finish(&self) -> (Vec<ObjectMutation>, Vec<u32>, Vec<String>, usize) {
        let mut new_objects = self.new_objects.clone();
        let mut next_num = self.next_num;
        let mut pages = self.pages.clone();

        // One content stream per placement per page, prefixed with the marker.
        for accum in pages.values_mut() {
            for (placement, ops) in [
                (Placement::Background, &mut accum.background),
                (Placement::Overlay, &mut accum.overlay),
            ] {
                if ops.is_empty() {
                    continue;
                }
                let num = next_num;
                next_num = next_num.saturating_add(1);
                let mut data = Vec::with_capacity(MARKER.len().saturating_add(ops.len()));
                data.extend_from_slice(MARKER);
                data.extend_from_slice(ops);
                let length = i64::try_from(data.len()).unwrap_or(i64::MAX);
                new_objects.push((
                    num,
                    Obj::Stream {
                        dict: vec![(key("Length"), Obj::Int(length))],
                        data: Bytes::copy_from_slice(&data),
                    },
                ));
                match placement {
                    Placement::Overlay => accum.overlay_ref = Some(num),
                    Placement::Background => accum.background_ref = Some(num),
                }
            }
        }

        // Rewrite each page dict: repoint /Contents, merge /Resources and add
        // /Annots. The original content stream object is never touched.
        let mut mutations: Vec<ObjectMutation> = Vec::new();
        for accum in pages.values() {
            let dict = self.model.get(&accum.page_ref.num).cloned();
            let dict = match dict {
                Some(Obj::Dict(d)) => d,
                _ => Vec::new(),
            };
            let mut contents: Vec<Ref> = Vec::new();
            if let Some(existing) = dict_get(&dict, b"Contents") {
                match existing {
                    Obj::Ref(r) => contents.push(*r),
                    Obj::Array(items) => {
                        for item in items {
                            if let Obj::Ref(r) = item {
                                contents.push(*r);
                            }
                        }
                    }
                    _ => {}
                }
            }
            let mut ordered: Vec<Ref> = Vec::new();
            if let Some(n) = accum.background_ref {
                ordered.push(Ref::new(n, 0));
            }
            ordered.extend_from_slice(&contents);
            if let Some(n) = accum.overlay_ref {
                ordered.push(Ref::new(n, 0));
            }
            let contents_obj = Obj::Array(ordered.into_iter().map(Obj::Ref).collect());

            let mut resources = accum.resources.clone();
            for (name, num) in &accum.fonts {
                set_dict(&mut resources, name.as_slice(), Obj::Ref(Ref::new(*num, 0)));
            }
            for (name, num) in &accum.xobjects {
                set_dict(&mut resources, name.as_slice(), Obj::Ref(Ref::new(*num, 0)));
            }
            if !accum.gstates.is_empty() {
                let mut gs_dict = match dict_get(&resources, b"ExtGState") {
                    Some(Obj::Dict(d)) => d.clone(),
                    _ => Vec::new(),
                };
                for (name, num) in &accum.gstates {
                    gs_dict.push((name.clone(), Obj::Ref(Ref::new(*num, 0))));
                }
                set_dict(&mut resources, b"ExtGState", Obj::Dict(gs_dict));
            }

            let mut annots: Vec<Obj> = Vec::new();
            if let Some(Obj::Array(items)) = dict_get(&dict, b"Annots") {
                annots.extend(items.iter().cloned());
            }
            for num in &accum.annots {
                annots.push(Obj::Ref(Ref::new(*num, 0)));
            }

            let mut page: Vec<(Bytes, Obj)> = dict
                .iter()
                .filter(|(k, _)| !matches!(k.as_slice(), b"Contents" | b"Resources" | b"Annots"))
                .cloned()
                .collect();
            page.push((key("Contents"), contents_obj));
            page.push((key("Resources"), Obj::Dict(resources)));
            if !annots.is_empty() {
                page.push((key("Annots"), Obj::Array(annots)));
            }
            mutations.push(ObjectMutation::SetObject {
                num: accum.page_ref.num,
                value: Obj::Dict(page),
            });
        }

        let objects_added = new_objects.len();
        for (num, value) in new_objects {
            mutations.push(ObjectMutation::SetObject { num, value });
        }
        let page_list: Vec<u32> = pages.keys().copied().collect();
        (
            mutations,
            page_list,
            self.disclosures.clone(),
            objects_added,
        )
    }
}

/// Replace (or add) a key in a COS dictionary, preserving other entries.
fn set_dict(dict: &mut Vec<(Bytes, Obj)>, name: &[u8], value: Obj) {
    if let Some((_, slot)) = dict.iter_mut().find(|(k, _)| k.as_slice() == name) {
        *slot = value;
    } else {
        dict.push((Bytes::copy_from_slice(name), value));
    }
}

/// Ellipse inscribed in `rect` as four cubic Béziers (PDF's `c` operator).
fn ellipse_ops(rect: Rect) -> String {
    let k = 0.552_284_749_8f64;
    let (x0, y0, x1, y1) = (rect.x0, rect.y0, rect.x1, rect.y1);
    let cx = (x0 + x1) / 2.0;
    let cy = (y0 + y1) / 2.0;
    let rx = (x1 - x0) / 2.0;
    let ry = (y1 - y0) / 2.0;
    let kx = rx * k;
    let ky = ry * k;
    let p = |x: f64, y: f64| format!("{} {}", pdf_num(x), pdf_num(y));
    let mut out = String::new();
    out.push_str(&format!("{} m\n", p(cx + rx, cy)));
    out.push_str(&format!(
        "{} {} {} c\n",
        p(cx + rx, cy + ky),
        p(cx + kx, cy + ry),
        p(cx, cy + ry)
    ));
    out.push_str(&format!(
        "{} {} {} c\n",
        p(cx - kx, cy + ry),
        p(cx - rx, cy + ky),
        p(cx - rx, cy)
    ));
    out.push_str(&format!(
        "{} {} {} c\n",
        p(cx - rx, cy - ky),
        p(cx - kx, cy - ry),
        p(cx, cy - ry)
    ));
    out.push_str(&format!(
        "{} {} {} c\n",
        p(cx + kx, cy - ry),
        p(cx + rx, cy - ky),
        p(cx + rx, cy)
    ));
    out.push_str("h\n");
    out
}

/// Set up a document-wide builder: parse, refuse encryption, resolve all pages.
fn builder(
    src: &[u8],
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
) -> Result<Builder, AddContentError> {
    let start = selis_pdf_cos::xref::find_startxref(src, 4096).ok_or_else(|| {
        AddContentError::Malformed("no startxref; not a readable PDF".to_string())
    })?;
    let doc = selis_pdf_cos::parse_revisions(src, start, budget, g)
        .map_err(|e| AddContentError::Malformed(format!("cannot open: {e}")))?;
    if is_encrypted(&doc) {
        return Err(AddContentError::Encrypted);
    }
    let refs = page_refs(src, &doc, budget, g)?;
    let model = materialise(src, budget, g)
        .map_err(|e| AddContentError::Unresolvable(format!("cannot materialise: {e}")))?;

    let mut pages: BTreeMap<u32, PageAccum> = BTreeMap::new();
    for (index, page_ref) in refs.iter().enumerate() {
        let page_index = u32::try_from(index).unwrap_or(u32::MAX);
        let obj = selis_pdf_cos::copy::resolve_ref(src, &doc, *page_ref, budget, g)
            .map_err(|e| AddContentError::Unresolvable(format!("cannot read the page: {e}")))?;
        let Obj::Dict(dict) = obj else {
            return Err(AddContentError::Malformed(
                "the page is not a dictionary".to_string(),
            ));
        };
        let media = media_box(src, &doc, &dict, budget, g)?;
        let resources = match inherited(src, &doc, dict.clone(), b"Resources", budget, g)? {
            Some(value) => resolve_dict(src, &doc, &value, budget, g, "Resources")?,
            None => Vec::new(),
        };
        let used_names = resources
            .iter()
            .filter_map(|(k, _)| String::from_utf8(k.as_slice().to_vec()).ok())
            .collect::<BTreeSet<_>>();
        pages.insert(
            page_index,
            PageAccum {
                page_ref: *page_ref,
                media,
                resources,
                used_names,
                overlay: Vec::new(),
                background: Vec::new(),
                overlay_ref: None,
                background_ref: None,
                fonts: Vec::new(),
                xobjects: Vec::new(),
                gstates: Vec::new(),
                annots: Vec::new(),
            },
        );
    }

    let next_num = model.keys().copied().max().unwrap_or(0).saturating_add(1);
    Ok(Builder {
        model,
        page_refs: refs,
        pages,
        new_objects: Vec::new(),
        disclosures: Vec::new(),
        next_num,
    })
}

/// Commit a finished plan and build its report.
#[allow(clippy::too_many_arguments)]
fn commit(
    src: &[u8],
    model: &ObjectModel,
    mutations: &[ObjectMutation],
    pages: Vec<u32>,
    disclosures: Vec<String>,
    objects_added: usize,
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
) -> Result<(Vec<u8>, AddContentReport), AddContentError> {
    let (out, save) = save_incremental(src, model, mutations, budget, g)
        .map_err(|e| AddContentError::Unsupported(format!("cannot save: {e}")))?;
    let prefix_preserved = save.prefix_preserved;
    if !prefix_preserved {
        return Err(AddContentError::Unsupported(
            "the incremental writer did not preserve the original bytes as a prefix".to_string(),
        ));
    }
    let report = AddContentReport {
        pages,
        bytes_in: u64::try_from(src.len()).unwrap_or(u64::MAX),
        bytes_out: u64::try_from(out.len()).unwrap_or(u64::MAX),
        objects_added,
        objects_changed: mutations.len(),
        prefix_preserved,
        font_disclosures: disclosures,
    };
    Ok((out, report))
}

/// Add a text box to a page (overlay). Substitution is disclosed in the report.
///
/// # Errors
///
/// Typed [`AddContentError`] for an unreadable/encrypted document, an invalid
/// page or rect, a non-ASCII character, or a font that cannot cover the text.
pub fn add_text_box(
    src: &[u8],
    page_index: u32,
    text: &TextBox,
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
) -> Result<(Vec<u8>, AddContentReport), AddContentError> {
    let mut plan = builder(src, budget, g)?;
    plan.text_ops(
        page_index,
        text,
        text.rect.x0,
        text.rect.y0,
        0.0,
        None,
        Placement::Overlay,
        budget,
    )?;
    let (mutations, _, disclosures, added) = plan.finish();
    commit(
        src,
        &plan.model,
        &mutations,
        vec![page_index],
        disclosures,
        added,
        budget,
        g,
    )
}

/// Add a caller-supplied image to a page (overlay).
///
/// `image_bytes` must be a standalone PNG/JPEG the in-memory decoder accepts;
/// it is flattened onto white and written as an 8-bit Flate/DeviceRGB XObject.
/// No fetch or file I/O occurs.
///
/// # Errors
///
/// Typed [`AddContentError`] for an unreadable/encrypted document, an invalid
/// page or rect, or an image that cannot be decoded.
pub fn add_image(
    src: &[u8],
    page_index: u32,
    image: &AddImage,
    image_bytes: &[u8],
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
) -> Result<(Vec<u8>, AddContentReport), AddContentError> {
    if image_bytes.len() > MAX_IMAGE_BYTES {
        return Err(AddContentError::Invalid(format!(
            "image bytes exceed the {MAX_IMAGE_BYTES}-byte limit"
        )));
    }
    check_rect(image.rect, "the image rect")?;
    if !image.angle_degrees.is_finite() {
        return Err(AddContentError::Invalid(
            "image rotation must be finite".to_string(),
        ));
    }
    // One parse; validate the target page before decoding any caller bytes.
    let mut plan = builder(src, budget, g)?;
    plan.page(page_index)?;
    let decoded = selis_image::decode(image_bytes, g)
        .map_err(|e| AddContentError::Unsupported(format!("cannot decode image: {e}")))?;
    if decoded.width == 0 || decoded.height == 0 {
        return Err(AddContentError::Invalid("the image is empty".to_string()));
    }
    let rgb = selis_image::flatten_over_white(&decoded.rgba, g)
        .map_err(|e| AddContentError::Malformed(format!("cannot flatten image: {e}")))?;
    let data = selis_pdf_filter::flate_encode(&rgb, 6);
    let length = i64::try_from(data.len()).unwrap_or(i64::MAX);

    let name = plan.unique_name(page_index, "Im");
    let num = plan.alloc();
    let stream = Obj::Stream {
        dict: vec![
            (key("Type"), Obj::Name(key("XObject"))),
            (key("Subtype"), Obj::Name(key("Image"))),
            (key("Width"), Obj::Int(i64::from(decoded.width))),
            (key("Height"), Obj::Int(i64::from(decoded.height))),
            (key("ColorSpace"), Obj::Name(key("DeviceRGB"))),
            (key("BitsPerComponent"), Obj::Int(8)),
            (key("Filter"), Obj::Name(key("FlateDecode"))),
            (key("Length"), Obj::Int(length)),
        ],
        data: Bytes::copy_from_slice(&data),
    };
    plan.new_objects.push((num, stream));
    if let Some(accum) = plan.pages.get_mut(&page_index) {
        accum.xobjects.push((name.clone(), num));
    }
    plan.remember_name(page_index, &name);

    let mut ops = Vec::new();
    push_line(
        &mut ops,
        &format!(
            "q {} cm /{} Do Q",
            matrix_ops(image_matrix(image.rect, image.angle_degrees)),
            String::from_utf8_lossy(&name)
        ),
    );
    plan.append_ops(page_index, Placement::Overlay, ops)?;
    let (mutations, _, disclosures, added) = plan.finish();
    commit(
        src,
        &plan.model,
        &mutations,
        vec![page_index],
        disclosures,
        added,
        budget,
        g,
    )
}

/// Add a vector shape to a page (overlay).
///
/// # Errors
///
/// Typed [`AddContentError`] for an unreadable/encrypted document, an invalid
/// page or rect, or a non-finite stroke width.
pub fn add_shape(
    src: &[u8],
    page_index: u32,
    shape: &Shape,
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
) -> Result<(Vec<u8>, AddContentReport), AddContentError> {
    check_rect(shape.rect, "the shape rect")?;
    if !shape.stroke_width.is_finite() || shape.stroke_width < 0.0 {
        return Err(AddContentError::Invalid(
            "stroke width must be finite and non-negative".to_string(),
        ));
    }
    if shape.fill.is_none() && shape.stroke.is_none() {
        return Err(AddContentError::Invalid(
            "a shape needs a fill or a stroke colour".to_string(),
        ));
    }
    let mut plan = builder(src, budget, g)?;
    plan.page(page_index)?;
    let (x0, y0, x1, y1) = (shape.rect.x0, shape.rect.y0, shape.rect.x1, shape.rect.y1);
    let mut ops: Vec<u8> = Vec::new();
    push_line(&mut ops, "q");
    match shape.kind {
        ShapeKind::Rect => push_line(
            &mut ops,
            &format!(
                "{} {} {} {} re",
                pdf_num(x0),
                pdf_num(y0),
                pdf_num(x1 - x0),
                pdf_num(y1 - y0)
            ),
        ),
        ShapeKind::Ellipse => push_line(&mut ops, &ellipse_ops(shape.rect)),
        ShapeKind::Line => push_line(
            &mut ops,
            &format!(
                "{} {} m {} {} l",
                pdf_num(x0),
                pdf_num(y0),
                pdf_num(x1),
                pdf_num(y1)
            ),
        ),
    }
    if let Some(color) = shape.fill {
        push_line(&mut ops, &color.fill());
    }
    if let Some(color) = shape.stroke {
        push_line(&mut ops, &format!("{} w", pdf_num(shape.stroke_width)));
        push_line(&mut ops, &color.stroke());
    }
    let paint = if shape.fill.is_some() && shape.stroke.is_some() {
        "B"
    } else if shape.fill.is_some() {
        "f"
    } else {
        "S"
    };
    push_line(&mut ops, paint);
    push_line(&mut ops, "Q");
    plan.append_ops(page_index, Placement::Overlay, ops)?;
    let (mutations, _, disclosures, added) = plan.finish();
    commit(
        src,
        &plan.model,
        &mutations,
        vec![page_index],
        disclosures,
        added,
        budget,
        g,
    )
}

/// Add a link annotation to a page.
///
/// The annotation carries a proper `/A` action and a generated border
/// appearance stream, so non-Selis viewers show and follow it.
///
/// # Errors
///
/// Typed [`AddContentError`] for an unreadable/encrypted document, an invalid
/// page or rect, an empty URI, or a page target out of range.
pub fn add_link(
    src: &[u8],
    page_index: u32,
    link: &Link,
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
) -> Result<(Vec<u8>, AddContentReport), AddContentError> {
    check_rect(link.rect, "the link rect")?;
    let mut plan = builder(src, budget, g)?;
    plan.page(page_index)?;
    // Validate the target before allocating anything.
    let action = match &link.target {
        LinkTarget::Uri(uri) => {
            if uri.is_empty() {
                return Err(AddContentError::Invalid(
                    "a URI link needs a non-empty URI".to_string(),
                ));
            }
            let _ = pdf_literal(uri)?; // validates printable ASCII
            Obj::Dict(vec![
                (key("S"), Obj::Name(key("URI"))),
                (
                    key("URI"),
                    Obj::String(Bytes::copy_from_slice(uri.as_bytes())),
                ),
            ])
        }
        LinkTarget::Page(target) => {
            let page_ref = *plan
                .page_refs
                .get(usize::try_from(*target).unwrap_or(usize::MAX))
                .ok_or_else(|| {
                    AddContentError::Invalid(format!("link target page {target} is out of range"))
                })?;
            Obj::Dict(vec![
                (key("S"), Obj::Name(key("GoTo"))),
                (
                    key("D"),
                    Obj::Array(vec![Obj::Ref(page_ref), Obj::Name(key("Fit"))]),
                ),
            ])
        }
    };

    let (x0, y0, x1, y1) = (link.rect.x0, link.rect.y0, link.rect.x1, link.rect.y1);
    let width = x1 - x0;
    let height = y1 - y0;

    // The /AP appearance: a form XObject that outlines the clickable rect.
    let mut ap_ops: Vec<u8> = Vec::new();
    if link.border {
        push_line(&mut ap_ops, "q");
        push_line(&mut ap_ops, &link.color.stroke());
        push_line(&mut ap_ops, "1 w");
        push_line(
            &mut ap_ops,
            &format!(
                "{} {} {} {} re S",
                pdf_num(0.5),
                pdf_num(0.5),
                pdf_num((width - 1.0).max(0.01)),
                pdf_num((height - 1.0).max(0.01))
            ),
        );
        push_line(&mut ap_ops, "Q");
    }
    let ap_len = i64::try_from(ap_ops.len()).unwrap_or(i64::MAX);
    let ap_num = plan.alloc();
    plan.new_objects.push((
        ap_num,
        Obj::Stream {
            dict: vec![
                (key("Type"), Obj::Name(key("XObject"))),
                (key("Subtype"), Obj::Name(key("Form"))),
                (
                    key("BBox"),
                    Obj::Array(vec![
                        pdf_number(0.0),
                        pdf_number(0.0),
                        pdf_number(width),
                        pdf_number(height),
                    ]),
                ),
                (key("Resources"), Obj::Dict(Vec::new())),
                (key("Length"), Obj::Int(ap_len)),
            ],
            data: Bytes::copy_from_slice(&ap_ops),
        },
    ));

    let annot_num = plan.alloc();
    let annot = Obj::Dict(vec![
        (key("Type"), Obj::Name(key("Annot"))),
        (key("Subtype"), Obj::Name(key("Link"))),
        (
            key("Rect"),
            Obj::Array(vec![
                pdf_number(x0),
                pdf_number(y0),
                pdf_number(x1),
                pdf_number(y1),
            ]),
        ),
        (key("F"), Obj::Int(4)),
        (
            key("Border"),
            Obj::Array(vec![Obj::Int(0), Obj::Int(0), Obj::Int(1)]),
        ),
        (key("A"), action),
        (
            key("AP"),
            Obj::Dict(vec![(key("N"), Obj::Ref(Ref::new(ap_num, 0)))]),
        ),
    ]);
    plan.new_objects.push((annot_num, annot));
    if let Some(accum) = plan.pages.get_mut(&page_index) {
        accum.annots.push(annot_num);
    }
    let (mutations, _, disclosures, added) = plan.finish();
    commit(
        src,
        &plan.model,
        &mutations,
        vec![page_index],
        disclosures,
        added,
        budget,
        g,
    )
}

/// Add a header or footer to a page (overlay).
///
/// # Errors
///
/// Typed [`AddContentError`] for an unreadable/encrypted document, an invalid
/// page, or a text/font that cannot be drawn.
pub fn add_header_footer(
    src: &[u8],
    page_index: u32,
    hf: &HeaderFooter,
    is_header: bool,
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
) -> Result<(Vec<u8>, AddContentReport), AddContentError> {
    if !hf.margin.is_finite() || hf.margin < 0.0 {
        return Err(AddContentError::Invalid(
            "margin must be finite and non-negative".to_string(),
        ));
    }
    let mut plan = builder(src, budget, g)?;
    let media = plan.page(page_index)?.media;
    // A rough text width for centre/right alignment. Standard-14 Helvetica
    // averages ~0.5 em per ASCII glyph; this positions the run, it does not
    // claim exact metrics (no font program is measured here).
    let approx_width = hf.text.len() as f64 * hf.font_size * 0.5;
    let usable = (media.x1 - media.x0 - hf.margin * 2.0).max(0.0);
    let x0 = match hf.align {
        Align::Left => media.x0 + hf.margin,
        Align::Center => media.x0 + hf.margin + (usable - approx_width).max(0.0) / 2.0,
        Align::Right => media.x1 - hf.margin - approx_width,
    };
    let y0 = if is_header {
        media.y1 - hf.margin - hf.font_size
    } else {
        media.y0 + hf.margin
    };
    let rect = Rect::new(x0, y0, x0 + approx_width.max(1.0), y0 + hf.font_size);
    let text = TextBox {
        text: hf.text.clone(),
        font: hf.font.clone(),
        font_size: hf.font_size,
        color: hf.color,
        rect,
    };
    plan.text_ops(
        page_index,
        &text,
        x0,
        y0,
        0.0,
        None,
        Placement::Overlay,
        budget,
    )?;
    let (mutations, _, disclosures, added) = plan.finish();
    commit(
        src,
        &plan.model,
        &mutations,
        vec![page_index],
        disclosures,
        added,
        budget,
        g,
    )
}

/// Add a watermark to a page (overlay).
///
/// # Errors
///
/// Typed [`AddContentError`] for an unreadable/encrypted document, an invalid
/// page, or a text/font that cannot be drawn.
pub fn add_watermark(
    src: &[u8],
    page_index: u32,
    mark: &Watermark,
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
) -> Result<(Vec<u8>, AddContentReport), AddContentError> {
    if !mark.font_size.is_finite() || mark.font_size <= 0.0 {
        return Err(AddContentError::Invalid(
            "font size must be finite and positive".to_string(),
        ));
    }
    let mut plan = builder(src, budget, g)?;
    let media = plan.page(page_index)?.media;
    let cx = (media.x0 + media.x1) / 2.0;
    let cy = (media.y0 + media.y1) / 2.0;
    let rect = Rect::new(
        cx - mark.font_size,
        cy - mark.font_size,
        cx + mark.font_size,
        cy + mark.font_size,
    );
    let text = TextBox {
        text: mark.text.clone(),
        font: mark.font.clone(),
        font_size: mark.font_size,
        color: mark.color,
        rect,
    };
    plan.text_ops(
        page_index,
        &text,
        cx,
        cy,
        mark.angle_degrees,
        Some(mark.opacity),
        Placement::Overlay,
        budget,
    )?;
    let (mutations, _, disclosures, added) = plan.finish();
    commit(
        src,
        &plan.model,
        &mutations,
        vec![page_index],
        disclosures,
        added,
        budget,
        g,
    )
}

/// Render one Bates id from a pattern by replacing its last digit run with
/// `value` (zero-padded to the run's width).
fn bates_text(pattern: &str, value: u64) -> Result<String, AddContentError> {
    let bytes = pattern.as_bytes();
    let mut end = None;
    for (i, b) in bytes.iter().enumerate() {
        if b.is_ascii_digit() {
            end = Some(i.saturating_add(1));
        }
    }
    let end = end.ok_or_else(|| {
        AddContentError::Unsupported(
            "the Bates pattern has no digit run to use as a counter".to_string(),
        )
    })?;
    let mut start = end;
    while start > 0
        && bytes
            .get(start.saturating_sub(1))
            .copied()
            .is_some_and(|b| b.is_ascii_digit())
    {
        start = start.saturating_sub(1);
    }
    let width = end.saturating_sub(start);
    let digits = value.to_string();
    let padded = if digits.len() >= width {
        digits
    } else {
        format!(
            "{}{}",
            "0".repeat(width.saturating_sub(digits.len())),
            digits
        )
    };
    let head = pattern.get(..start).unwrap_or("");
    let tail = pattern.get(end..).unwrap_or("");
    Ok(format!("{head}{padded}{tail}"))
}

/// Stamp sequential Bates ids on every page (overlay).
///
/// # Errors
///
/// Typed [`AddContentError`] for an unreadable/encrypted document, a pattern
/// without a counter field, or a font that cannot cover the text.
pub fn add_bates_numbering(
    src: &[u8],
    options: &BatesOptions,
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
) -> Result<(Vec<u8>, AddContentReport), AddContentError> {
    if !options.margin.is_finite() || options.margin < 0.0 {
        return Err(AddContentError::Invalid(
            "margin must be finite and non-negative".to_string(),
        ));
    }
    if !options.font_size.is_finite() || options.font_size <= 0.0 {
        return Err(AddContentError::Invalid(
            "font size must be finite and positive".to_string(),
        ));
    }
    let mut plan = builder(src, budget, g)?;
    let page_count = plan.page_refs.len();
    for index in 0..page_count {
        let page_index = u32::try_from(index).unwrap_or(u32::MAX);
        let media = plan.page(page_index)?.media;
        let value = options
            .start
            .saturating_add(u64::try_from(index).unwrap_or(u64::MAX));
        let text = bates_text(&options.pattern, value)?;
        let approx_width = text.len() as f64 * options.font_size * 0.5;
        let usable = (media.x1 - media.x0 - options.margin * 2.0).max(0.0);
        let x0 = match options.align {
            Align::Left => media.x0 + options.margin,
            Align::Center => media.x0 + options.margin + (usable - approx_width).max(0.0) / 2.0,
            Align::Right => media.x1 - options.margin - approx_width,
        };
        let y0 = media.y0 + options.margin;
        let rect = Rect::new(x0, y0, x0 + approx_width.max(1.0), y0 + options.font_size);
        let text_box = TextBox {
            text,
            font: options.font.clone(),
            font_size: options.font_size,
            color: options.color,
            rect,
        };
        plan.text_ops(
            page_index,
            &text_box,
            x0,
            y0,
            0.0,
            None,
            Placement::Overlay,
            budget,
        )?;
    }
    let pages: Vec<u32> = (0..page_count)
        .map(|i| u32::try_from(i).unwrap_or(u32::MAX))
        .collect();
    let (mutations, _, disclosures, added) = plan.finish();
    commit(
        src,
        &plan.model,
        &mutations,
        pages,
        disclosures,
        added,
        budget,
        g,
    )
}

/// Add a solid background rectangle beneath a page's existing content.
///
/// The original content stream object is not rewritten: the new stream is
/// placed first in the page's `/Contents` array, so existing paint covers it.
///
/// # Errors
///
/// Typed [`AddContentError`] for an unreadable/encrypted document or an invalid
/// page or rect.
pub fn add_background(
    src: &[u8],
    page_index: u32,
    background: &Background,
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
) -> Result<(Vec<u8>, AddContentReport), AddContentError> {
    check_rect(background.rect, "the background rect")?;
    let mut plan = builder(src, budget, g)?;
    plan.page(page_index)?;
    let (x0, y0, x1, y1) = (
        background.rect.x0,
        background.rect.y0,
        background.rect.x1,
        background.rect.y1,
    );
    let mut ops = Vec::new();
    push_line(&mut ops, "q");
    push_line(&mut ops, &background.color.fill());
    push_line(
        &mut ops,
        &format!(
            "{} {} {} {} re f",
            pdf_num(x0),
            pdf_num(y0),
            pdf_num(x1 - x0),
            pdf_num(y1 - y0)
        ),
    );
    push_line(&mut ops, "Q");
    plan.append_ops(page_index, Placement::Background, ops)?;
    let (mutations, _, disclosures, added) = plan.finish();
    commit(
        src,
        &plan.model,
        &mutations,
        vec![page_index],
        disclosures,
        added,
        budget,
        g,
    )
}
