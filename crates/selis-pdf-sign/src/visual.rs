//! A visual signature stamp: typed lines and freehand strokes appended to one
//! page as an incremental revision.
//!
//! A visual stamp is not a cryptographic claim, and this module never presents
//! it as one. What it does guarantee is that it *only adds bytes*: the page's
//! content stream grows by one appended stream, the page keeps its original
//! `/Contents` as the first element of the array, and every earlier revision —
//! including a digital signature over one — stays byte-identical. That is why
//! stamping a signed document does not silently break its signature the way a
//! rewrite would; the viewer reports "signed, then modified", which is the
//! truth.
//!
//! The stamp is placed in **display space**: the caller gives a rectangle in
//! the coordinates a reader shows (origin bottom-left, after `/Rotate`), and
//! the geometry is mapped back through the page rotation so text is upright on
//! a rotated page. Freehand points arrive normalized to the rectangle, which
//! is what a drawing canvas naturally produces in a browser.
//!
//! The output is re-opened and checked before it is returned: the appended
//! stream must be present, referenced from the page, and byte-identical to
//! what was written.

use selis_bytes::Bytes;
use selis_pdf_cos::doc_writer::write_incremental_update;
use selis_pdf_cos::{Obj, Ref};
use selis_sandbox::{Budget, BudgetGuard};

use crate::{
    carried_trailer, dict_get, dict_set, encode_winansi, escape_text, invalid, malformed,
    next_object_number, pages, real, resolve, text_width, trailer, SignError, MAX_STROKE_POINTS,
    MAX_TEXT_BYTES,
};

/// The font resource name the appended content stream uses. Prefixed so it
/// cannot collide with a name the document already chose.
const FONT_NAME: &str = "SelisSigF";
/// A fixed-font line height, in multiples of the font size.
const LINE_LEADING: f64 = 1.25;
/// Horizontal padding inside the stamp rectangle.
const PADDING: f64 = 3.0;

/// One typed line of the stamp.
#[derive(Debug, Clone, PartialEq, serde::Deserialize)]
pub struct VisualLine {
    /// The text to draw. Characters outside WinAnsi are refused, not
    /// substituted.
    pub text: String,
    /// The line's font size in points. `0` means the stamp's default.
    pub size: f64,
}

impl VisualLine {
    /// A line at the stamp's default size.
    #[must_use]
    pub fn new(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            size: 0.0,
        }
    }
}

/// A freehand stroke: normalized points inside the stamp rectangle, `(0,0)`
/// the bottom-left and `(1,1)` the top-right. A viewer's canvas is y-down, so
/// callers flip it before handing it over.
#[derive(Debug, Clone, PartialEq, Default, serde::Deserialize)]
pub struct VisualStroke {
    /// At least two points; each is clamped into `0..=1`.
    pub points: Vec<(f64, f64)>,
}

/// What to stamp, and where.
#[derive(Debug, Clone, PartialEq, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VisualOptions {
    /// 1-based page number.
    pub page: usize,
    /// The rectangle in display-space points, `[x0, y0, x1, y1]`, origin at
    /// the bottom-left of the displayed page.
    pub rect: [f64; 4],
    /// Typed lines, drawn top to bottom.
    pub lines: Vec<VisualLine>,
    /// Freehand strokes, drawn in the order given.
    pub strokes: Vec<VisualStroke>,
    /// Stroke and text colour, RGB in `0..=1`.
    pub color: [f64; 3],
    /// Stroke width in points.
    pub stroke_width: f64,
    /// Font size for lines that do not name one.
    pub font_size: f64,
    /// Draw a thin frame around the stamp. On by default, because an unstamped
    /// rectangle is invisible and a user cannot tell where the click landed.
    pub border: bool,
}

impl Default for VisualOptions {
    fn default() -> Self {
        Self {
            page: 1,
            rect: [36.0, 36.0, 252.0, 108.0],
            lines: Vec::new(),
            strokes: Vec::new(),
            color: [0.05, 0.05, 0.12],
            stroke_width: 1.5,
            font_size: 11.0,
            border: true,
        }
    }
}

/// What was stamped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VisualReport {
    /// 1-based page the stamp landed on.
    pub page: usize,
    /// Displayed page width and height, rounded to whole points.
    pub display_size: [i64; 2],
    /// Number of typed lines drawn.
    pub lines: usize,
    /// Number of strokes drawn.
    pub strokes: usize,
    /// Size of the appended content stream.
    pub stream_bytes: usize,
}

/// Append a visual signature stamp to one page.
///
/// # Errors
///
/// * [`SignError::Invalid`] for an empty stamp, a page outside the document, a
///   degenerate rectangle, a non-finite coordinate, a character outside
///   WinAnsi, or a font size outside `1..=144`;
/// * [`SignError::Limit`] for a stamp larger than the documented caps;
/// * [`SignError::Encrypted`] for a document whose dictionaries are ciphertext;
/// * [`SignError::ReadBack`] when the produced bytes do not read back as the
///   stamp that was written.
pub fn sign_visual(
    src: &[u8],
    options: &VisualOptions,
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
) -> Result<(Vec<u8>, VisualReport), SignError> {
    validate(options)?;

    let doc = crate::open_doc(src, budget, g)?;
    if crate::is_encrypted(&doc) {
        return Err(SignError::Encrypted);
    }
    let all_pages = pages(src, &doc, budget, g)?;
    let target = options
        .page
        .checked_sub(1)
        .and_then(|index| all_pages.get(index))
        .copied()
        .ok_or_else(|| {
            invalid(format!(
                "page {} does not exist; the document has {}",
                options.page,
                all_pages.len()
            ))
        })?;
    let (display_width, display_height) = target.display_size();
    let rect = clamp_rect(options.rect, display_width, display_height)?;

    let content = content_stream(options, target, rect)?;

    let base = next_object_number(&doc);
    let stream_num = base;
    let resources_num = base.saturating_add(1);

    // The page's own object, copied so the revision can rewrite it.
    let page_obj = resolve(src, &doc, Obj::Ref(target.reference), budget, g)?;
    let Obj::Dict(mut page_dict) = page_obj else {
        return Err(malformed("the target page is not a dictionary"));
    };

    // New /Contents: the old stream(s) first, then ours.
    let appended = Obj::Ref(Ref::new(stream_num, 0));
    let contents =
        match dict_get(&page_dict, b"Contents") {
            None | Some(Obj::Null) => Obj::Array(vec![appended]),
            Some(Obj::Array(items)) => {
                let mut items = items.clone();
                items.push(appended);
                Obj::Array(items)
            }
            Some(Obj::Ref(existing)) => Obj::Array(vec![Obj::Ref(*existing), appended]),
            Some(_) => return Err(SignError::Refused(
                "this page's /Contents is an inline stream, which cannot be extended in place; \
                 save it as a new file first"
                    .to_string(),
            )),
        };
    dict_set(&mut page_dict, b"Contents", contents);

    // New /Resources: the page's existing ones plus our font.
    let mut resources = match dict_get(&page_dict, b"Resources").cloned() {
        Some(Obj::Ref(reference)) => {
            let resolved = resolve(src, &doc, Obj::Ref(reference), budget, g)?;
            match resolved {
                Obj::Dict(pairs) => pairs,
                _ => Vec::new(),
            }
        }
        Some(Obj::Dict(pairs)) => pairs,
        _ => Vec::new(),
    };
    let mut fonts = match dict_get(&resources, b"Font").cloned() {
        Some(Obj::Ref(reference)) => {
            let resolved = resolve(src, &doc, Obj::Ref(reference), budget, g)?;
            match resolved {
                Obj::Dict(pairs) => pairs,
                _ => Vec::new(),
            }
        }
        Some(Obj::Dict(pairs)) => pairs,
        _ => Vec::new(),
    };
    let font = Obj::Dict(vec![
        (crate::bytes(b"Type"), Obj::Name(crate::bytes(b"Font"))),
        (crate::bytes(b"Subtype"), Obj::Name(crate::bytes(b"Type1"))),
        (
            crate::bytes(b"BaseFont"),
            Obj::Name(crate::bytes(b"Helvetica")),
        ),
        (
            crate::bytes(b"Encoding"),
            Obj::Name(crate::bytes(b"WinAnsiEncoding")),
        ),
    ]);
    dict_set(&mut fonts, FONT_NAME.as_bytes(), font);
    dict_set(&mut resources, b"Font", Obj::Dict(fonts));
    dict_set(
        &mut page_dict,
        b"Resources",
        Obj::Ref(Ref::new(resources_num, 0)),
    );

    let objects = vec![
        (
            stream_num,
            Obj::Stream {
                dict: Vec::new(),
                data: Bytes::copy_from_slice(&content),
            },
        ),
        (resources_num, Obj::Dict(resources)),
        (target.reference.num, Obj::Dict(page_dict)),
    ];
    let carried = trailer(&doc)?;
    let trailer_entries = carried_trailer(carried);
    let out = write_incremental_update(src, &objects, &trailer_entries, budget, g)
        .map_err(|e| malformed(format!("the incremental revision failed: {e}")))?;

    // Read-back: the page must reference our stream and the stream must be
    // byte-identical to the one written.
    read_back(&out, target.reference, stream_num, &content, budget, g)?;

    Ok((
        out,
        VisualReport {
            page: options.page,
            display_size: [
                i64::from(display_width.round() as i32),
                i64::from(display_height.round() as i32),
            ],
            lines: options.lines.len(),
            strokes: options.strokes.len(),
            stream_bytes: content.len(),
        },
    ))
}

fn validate(options: &VisualOptions) -> Result<(), SignError> {
    if options.lines.is_empty() && options.strokes.is_empty() {
        return Err(invalid(
            "the stamp is empty; type a name or draw a signature first",
        ));
    }
    if options.lines.len() > 64 || options.strokes.len() > 256 {
        return Err(SignError::Limit(
            "a stamp may carry at most 64 typed lines and 256 strokes".to_string(),
        ));
    }
    let points: usize = options
        .strokes
        .iter()
        .map(|stroke| stroke.points.len())
        .sum();
    if points > MAX_STROKE_POINTS {
        return Err(SignError::Limit(format!(
            "a stamp may carry at most {MAX_STROKE_POINTS} freehand points (got {points})"
        )));
    }
    if !(options.font_size.is_finite() && (1.0..=144.0).contains(&options.font_size)) {
        return Err(invalid("the font size must be between 1 and 144 points"));
    }
    if options.lines.iter().any(|line| {
        line.text.len() > MAX_TEXT_BYTES || !(line.size.is_finite() && line.size <= 144.0)
    }) {
        return Err(SignError::Limit(format!(
            "a typed line may be at most {MAX_TEXT_BYTES} bytes and 144 points"
        )));
    }
    if !options.rect.iter().all(|value| value.is_finite()) {
        return Err(invalid("the stamp rectangle has a non-finite coordinate"));
    }
    if !options
        .color
        .iter()
        .all(|value| (0.0..=1.0).contains(value))
    {
        return Err(invalid("the stamp colour must be three values in 0..=1"));
    }
    if !(options.stroke_width.is_finite() && (0.1..=24.0).contains(&options.stroke_width)) {
        return Err(invalid(
            "the stroke width must be between 0.1 and 24 points",
        ));
    }
    for stroke in &options.strokes {
        if stroke.points.len() == 1 {
            return Err(invalid("a stroke needs at least two points"));
        }
        if stroke
            .points
            .iter()
            .any(|(x, y)| !x.is_finite() || !y.is_finite())
        {
            return Err(invalid("a freehand point is not a finite number"));
        }
    }
    Ok(())
}

/// Fit the requested rectangle into the displayed page, refusing one that
/// misses it entirely.
fn clamp_rect(rect: [f64; 4], width: f64, height: f64) -> Result<[f64; 4], SignError> {
    let [mut x0, mut y0, mut x1, mut y1] = rect;
    if x1 < x0 {
        std::mem::swap(&mut x0, &mut x1);
    }
    if y1 < y0 {
        std::mem::swap(&mut y0, &mut y1);
    }
    if x1 - x0 < 4.0 || y1 - y0 < 4.0 {
        return Err(invalid("the stamp rectangle is smaller than 4 by 4 points"));
    }
    if x1 <= 0.0 || y1 <= 0.0 || x0 >= width || y0 >= height {
        return Err(invalid(
            "the stamp rectangle is entirely outside the displayed page",
        ));
    }
    Ok([x0.max(0.0), y0.max(0.0), x1.min(width), y1.min(height)])
}

/// Build the appended content stream in user space, rotating text so it reads
/// upright on the displayed page.
pub(crate) fn content_stream(
    options: &VisualOptions,
    page: crate::Page,
    rect: [f64; 4],
) -> Result<Vec<u8>, SignError> {
    let [x0, y0, x1, y1] = rect;
    let width = x1 - x0;
    let height = y1 - y0;
    let (bx, by) = page.display_to_user(x0, y0);
    let mut out = Vec::new();
    out.extend_from_slice(b"q\n");
    // The blend mode stays normal; a stamp must not alter what is under it.
    out.extend_from_slice(
        format!(
            "{} {} {} rg\n{} {} {} RG\n",
            real(options.color[0]),
            real(options.color[1]),
            real(options.color[2]),
            real(options.color[0]),
            real(options.color[1]),
            real(options.color[2]),
        )
        .as_bytes(),
    );
    if options.border {
        out.extend_from_slice(
            format!(
                "0.5 w\n{} {} {} {} re\nS\n",
                real(bx),
                real(by),
                real(width),
                real(height)
            )
            .as_bytes(),
        );
    }
    // Freehand strokes: straight segments between consecutive points. Both
    // corners map through the same affine rotation, so a segment stays a
    // segment and needs no matrix.
    if !options.strokes.is_empty() {
        out.extend_from_slice(format!("{} w\n1 J\n1 j\n", real(options.stroke_width)).as_bytes());
        for stroke in &options.strokes {
            let mut points = stroke.points.iter();
            let Some((nx, ny)) = points.next() else {
                continue;
            };
            let (ux, uy) = page.display_to_user(
                x0 + nx.clamp(0.0, 1.0) * width,
                y0 + ny.clamp(0.0, 1.0) * height,
            );
            out.extend_from_slice(format!("{} {} m\n", real(ux), real(uy)).as_bytes());
            for (nx, ny) in points {
                let (ux, uy) = page.display_to_user(
                    x0 + nx.clamp(0.0, 1.0) * width,
                    y0 + ny.clamp(0.0, 1.0) * height,
                );
                out.extend_from_slice(format!("{} {} l\n", real(ux), real(uy)).as_bytes());
            }
            out.extend_from_slice(b"S\n");
        }
    }
    if !options.lines.is_empty() {
        // Text is built in display space and placed with a matrix rotated by
        // the page's own rotation: the viewer rotates the page, so an upright
        // glyph needs the inverse rotation in user space.
        let (dx, dy) = page.display_dir_to_user(1.0, 0.0);
        let (cx, cy) = page.display_dir_to_user(0.0, 1.0);
        let mut baseline = y1 - PADDING;
        for line in &options.lines {
            let size = if line.size > 0.0 {
                line.size
            } else {
                options.font_size
            };
            let encoded = encode_winansi(&line.text)?;
            if encoded.is_empty() {
                baseline -= size * LINE_LEADING;
                continue;
            }
            // A line wider than the box is refused, not clipped and not
            // silently shrunk: the user can choose a shorter line or a larger
            // box, and either is visible in the result.
            if text_width(&encoded, size) > width - PADDING * 2.0 {
                return Err(invalid(format!(
                    "the line {:?} does not fit the stamp box at {size} points",
                    line.text
                )));
            }
            let tx = x0 + PADDING;
            let ty = baseline - size;
            let (ux, uy) = page.display_to_user(tx, ty);
            out.extend_from_slice(b"BT\n");
            out.extend_from_slice(format!("/{FONT_NAME} {} Tf\n", real(size)).as_bytes());
            out.extend_from_slice(
                format!(
                    "{} {} {} {} {} {} Tm\n",
                    real(dx),
                    real(dy),
                    real(cx),
                    real(cy),
                    real(ux),
                    real(uy)
                )
                .as_bytes(),
            );
            out.push(b'(');
            out.extend_from_slice(&escape_text(&encoded));
            out.extend_from_slice(b") Tj\nET\n");
            baseline -= size * LINE_LEADING;
        }
    }
    out.extend_from_slice(b"Q\n");
    Ok(out)
}

/// Confirm the produced bytes carry the stamp.
fn read_back(
    out: &[u8],
    page_ref: Ref,
    stream_num: u32,
    content: &[u8],
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
) -> Result<(), SignError> {
    let doc = crate::open_doc(out, budget, g)?;
    let page = resolve(out, &doc, Obj::Ref(page_ref), budget, g)
        .map_err(|e| SignError::ReadBack(format!("the page did not re-open: {e}")))?;
    let Obj::Dict(pairs) = page else {
        return Err(SignError::ReadBack(
            "the stamped page is not a dictionary".to_string(),
        ));
    };
    let referenced = match dict_get(&pairs, b"Contents") {
        Some(Obj::Array(items)) => items
            .iter()
            .any(|item| matches!(item, Obj::Ref(reference) if reference.num == stream_num)),
        Some(Obj::Ref(reference)) => reference.num == stream_num,
        _ => false,
    };
    if !referenced {
        return Err(SignError::ReadBack(
            "the page does not reference the appended stream".to_string(),
        ));
    }
    let stored = resolve(out, &doc, Obj::Ref(Ref::new(stream_num, 0)), budget, g)
        .map_err(|e| SignError::ReadBack(format!("the stream did not re-open: {e}")))?;
    let Obj::Stream { data, .. } = stored else {
        return Err(SignError::ReadBack(
            "the appended object is not a stream".to_string(),
        ));
    };
    if data.as_slice() != content {
        return Err(SignError::ReadBack(
            "the appended stream changed between write and read".to_string(),
        ));
    }
    Ok(())
}

/// Visible text of a stamp, used by shell previews and never by the writer.
#[must_use]
pub fn stamp_preview(options: &VisualOptions) -> String {
    options
        .lines
        .iter()
        .map(|line| line.text.trim())
        .filter(|text| !text.is_empty())
        .collect::<Vec<_>>()
        .join(" / ")
}

/// A `number` conversion that keeps integer-valued coordinates as integers, so
/// a rectangle like `612 792` does not carry a needless `.00`.
#[must_use]
pub fn coordinate(value: f64) -> Obj {
    if (value - value.trunc()).abs() < f64::EPSILON && value.abs() < 9.0e15 {
        Obj::Int(value.trunc() as i64)
    } else {
        Obj::Real {
            scaled: (value * 1000.0).round() as i64,
            scale: 3,
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing,
        clippy::arithmetic_side_effects
    )]

    use super::*;

    #[test]
    fn a_stamp_with_no_content_is_refused() {
        let options = VisualOptions::default();
        assert!(validate(&options).is_err());
    }

    #[test]
    fn a_stroke_needs_two_points() {
        let mut options = VisualOptions::default();
        options.strokes.push(VisualStroke {
            points: vec![(0.5, 0.5)],
        });
        assert!(validate(&options).is_err());
        options.strokes[0].points.push((0.6, 0.4));
        assert!(validate(&options).is_ok());
    }

    #[test]
    fn a_rectangle_is_normalized_and_clamped() {
        let rect = clamp_rect([100.0, 100.0, 20.0, 20.0], 80.0, 80.0).unwrap();
        assert_eq!(rect, [20.0, 20.0, 80.0, 80.0]);
        assert!(clamp_rect([200.0, 200.0, 220.0, 220.0], 80.0, 80.0).is_err());
    }

    #[test]
    fn coordinates_stay_integers_when_they_are() {
        assert_eq!(coordinate(612.0), Obj::Int(612));
        assert!(matches!(
            coordinate(10.5),
            Obj::Real {
                scaled: 10_500,
                scale: 3
            }
        ));
    }
}
