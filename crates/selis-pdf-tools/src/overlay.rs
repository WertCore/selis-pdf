//! Page overlays: page numbers, text watermarks, headers/footers, and Bates
//! numbering.
//!
//! Every operation copies the source document through the full-document
//! writer (so existing structures survive reconciliation), appends one new
//! content stream to the selected pages, and adds only the resources that
//! stream needs: a standard-14 Helvetica font and, when the overlay is
//! translucent, an `ExtGState` carrying `/ca` and `/CA`. Existing content
//! streams, resources, annotations, and page-dictionary entries are left in
//! place.
//!
//! All text is encoded as WinAnsi and shown with `/WinAnsiEncoding`; the
//! built-in Helvetica standard-14 metrics are used for alignment and
//! centering. This slice is deliberately text-only: image watermarks, font
//! embedding, and tiled watermarks are not implemented.
//!
//! Page selections in the option structs are **1-based** (`"1-3,5"`), unlike
//! the older zero-based CLI page-selection syntax used by the raw page tools.

#![allow(
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss
)]

use std::collections::{HashMap, HashSet};
use std::sync::OnceLock;

use selis_bytes::Bytes;
use selis_pdf_cos::doc_writer::DocumentBuilder;
use selis_pdf_cos::{Obj, Ref};
use selis_sandbox::{Budget, BudgetGuard};
use serde::{Deserialize, Serialize};

use crate::verify_report::Verification;
use crate::write_gate::PRESERVE_ALL;
use crate::{read_file, CliError, CliResult, ToolResult};

/// The largest font size an overlay accepts, in points.
const MAX_FONT_SIZE: f64 = 400.0;

/// The largest Bates number width an overlay accepts.
const MAX_BATES_DIGITS: usize = 20;

/// A color in device RGB, each component in the inclusive range `0.0..=1.0`.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct OverlayColor {
    /// Red component (`0.0..=1.0`).
    pub r: f64,
    /// Green component (`0.0..=1.0`).
    pub g: f64,
    /// Blue component (`0.0..=1.0`).
    pub b: f64,
}

impl Default for OverlayColor {
    fn default() -> Self {
        Self {
            r: 0.0,
            g: 0.0,
            b: 0.0,
        }
    }
}

/// Where a stamp sits on the displayed page.
///
/// `Top*` uses `margin_y` as the baseline distance from the top edge;
/// `Bottom*` uses it as the baseline distance from the bottom edge.
/// `Left*`/`*Right` use `margin_x` as the distance from the side edge.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OverlayPosition {
    /// Top edge, left aligned.
    TopLeft,
    /// Top edge, centered.
    TopCenter,
    /// Top edge, right aligned.
    TopRight,
    /// Bottom edge, left aligned.
    BottomLeft,
    /// Bottom edge, centered.
    #[default]
    BottomCenter,
    /// Bottom edge, right aligned.
    BottomRight,
}

/// The text format used by [`add_page_numbers`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PageNumberFormat {
    /// The page number alone (`3`).
    #[default]
    Plain,
    /// The page number and total (`3/10`).
    Fraction,
    /// `Page 3 of 10`.
    PageOfPages,
}

/// Options for [`add_page_numbers`].
///
/// `pages` is a 1-based selection (`"1-3,5"`); `None` stamps every page.
/// Numbering advances once for each selected page, starting at `start`. The
/// total used by [`PageNumberFormat::Fraction`] and
/// [`PageNumberFormat::PageOfPages`] is the document's page count.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct PageNumberOptions {
    /// 1-based page selection; `None` means every page.
    pub pages: Option<String>,
    /// Corner/edge placement of the number.
    pub position: OverlayPosition,
    /// Horizontal margin from the left or right edge, in points.
    pub margin_x: f64,
    /// Vertical baseline margin from the top or bottom edge, in points.
    pub margin_y: f64,
    /// The number printed on the first selected page.
    pub start: u32,
    /// The text format.
    pub format: PageNumberFormat,
    /// Font size in points.
    pub font_size: f64,
    /// Text color.
    pub color: OverlayColor,
}

impl Default for PageNumberOptions {
    fn default() -> Self {
        Self {
            pages: None,
            position: OverlayPosition::BottomCenter,
            margin_x: 36.0,
            margin_y: 24.0,
            start: 1,
            format: PageNumberFormat::Plain,
            font_size: 10.0,
            color: OverlayColor::default(),
        }
    }
}

/// The orientation of a text watermark.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WatermarkOrientation {
    /// A single line running diagonally upward across the page center.
    #[default]
    Diagonal,
    /// A single horizontal line across the page center.
    Horizontal,
}

/// Options for [`add_watermark`].
///
/// The watermark is one text line centered on each selected page. Tiling is
/// not implemented in this slice and is deliberately absent from the option
/// shape rather than accepted and ignored.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct WatermarkOptions {
    /// The watermark text. Must be non-empty and WinAnsi-encodable.
    pub text: String,
    /// Diagonal (45°) or horizontal placement.
    pub orientation: WatermarkOrientation,
    /// Opacity in `0.0..=1.0`; values below 1 add an `ExtGState`.
    pub opacity: f64,
    /// Font size in points.
    pub font_size: f64,
    /// Text color.
    pub color: OverlayColor,
    /// 1-based page selection; `None` means every page.
    pub pages: Option<String>,
}

impl Default for WatermarkOptions {
    fn default() -> Self {
        Self {
            text: String::new(),
            orientation: WatermarkOrientation::Diagonal,
            opacity: 0.2,
            font_size: 48.0,
            color: OverlayColor::default(),
            pages: None,
        }
    }
}

/// One header or footer line: three independently aligned text slots.
///
/// Supported placeholders are `{page}` (1-based physical page number) and
/// `{pages}` (total physical page count). Unknown placeholders are rejected,
/// so `{date}` is not silently printed literally.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct HeaderFooterSlots {
    /// Left-aligned text.
    pub left: String,
    /// Centered text.
    pub center: String,
    /// Right-aligned text.
    pub right: String,
}

impl HeaderFooterSlots {
    fn is_empty(&self) -> bool {
        self.left.is_empty() && self.center.is_empty() && self.right.is_empty()
    }
}

/// Options for [`add_header_footer`].
///
/// `header_margin` is the baseline distance from the top edge and
/// `footer_margin` the baseline distance from the bottom edge. At least one
/// slot must be non-empty.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct HeaderFooterOptions {
    /// 1-based page selection; `None` means every page.
    pub pages: Option<String>,
    /// Header slots.
    pub header: HeaderFooterSlots,
    /// Footer slots.
    pub footer: HeaderFooterSlots,
    /// Horizontal margin for left/right slots, in points.
    pub margin_x: f64,
    /// Header baseline distance from the top edge, in points.
    pub header_margin: f64,
    /// Footer baseline distance from the bottom edge, in points.
    pub footer_margin: f64,
    /// Font size in points.
    pub font_size: f64,
    /// Text color.
    pub color: OverlayColor,
}

impl Default for HeaderFooterOptions {
    fn default() -> Self {
        Self {
            pages: None,
            header: HeaderFooterSlots::default(),
            footer: HeaderFooterSlots::default(),
            margin_x: 36.0,
            header_margin: 36.0,
            footer_margin: 36.0,
            font_size: 9.0,
            color: OverlayColor::default(),
        }
    }
}

/// Options for [`add_bates`].
///
/// Each selected page receives `prefix + zero-padded number + suffix`, with
/// the number advancing once per selected page from `start`. If the number is
/// wider than `digits`, it is printed in full rather than truncated.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct BatesOptions {
    /// Text placed before the number.
    pub prefix: String,
    /// Text placed after the number.
    pub suffix: String,
    /// The number printed on the first selected page.
    pub start: u32,
    /// Minimum zero-padded width of the number (`1..=20`).
    pub digits: usize,
    /// 1-based page selection; `None` means every page.
    pub pages: Option<String>,
    /// Corner/edge placement of the stamp.
    pub position: OverlayPosition,
    /// Horizontal margin from the left or right edge, in points.
    pub margin_x: f64,
    /// Vertical baseline margin from the top or bottom edge, in points.
    pub margin_y: f64,
    /// Font size in points.
    pub font_size: f64,
    /// Text color.
    pub color: OverlayColor,
}

impl Default for BatesOptions {
    fn default() -> Self {
        Self {
            prefix: String::new(),
            suffix: String::new(),
            start: 1,
            digits: 6,
            pages: None,
            position: OverlayPosition::BottomRight,
            margin_x: 36.0,
            margin_y: 24.0,
            font_size: 8.0,
            color: OverlayColor::default(),
        }
    }
}

/// Add page numbers to all pages or a 1-based page selection.
///
/// The source content, resources, annotations, and catalog structures are
/// carried through the writer; one overlay content stream is appended per
/// selected page. Returns the operation's structural verification.
///
/// # Errors
///
/// [`ToolError`](crate::ToolError) when the selection, color, margins, or
/// font size are invalid, the document cannot be parsed, or the output fails
/// structural verification.
pub fn add_page_numbers(
    path: &str,
    options: &PageNumberOptions,
    output: &str,
) -> ToolResult<Verification> {
    validate_style(&OverlayStyle {
        font_size: options.font_size,
        color: options.color,
        opacity: 1.0,
    })
    .map_err(CliError)?;
    validate_margin(options.margin_x, "margin_x").map_err(CliError)?;
    validate_margin(options.margin_y, "margin_y").map_err(CliError)?;
    let style = OverlayStyle {
        font_size: options.font_size,
        color: options.color,
        opacity: 1.0,
    };
    let start = options.start;
    let format = options.format;
    let position = options.position;
    let margin_x = options.margin_x;
    let margin_y = options.margin_y;
    apply_overlay(
        &OverlayJob {
            path,
            output,
            pages: options.pages.as_deref(),
            style,
            action: "added page numbers",
        },
        |ctx| {
            let number = u64::from(start).saturating_add(ctx.selected_ordinal as u64);
            let total = ctx.page_count as u64;
            let text = match format {
                PageNumberFormat::Plain => number.to_string(),
                PageNumberFormat::Fraction => format!("{number}/{total}"),
                PageNumberFormat::PageOfPages => format!("Page {number} of {total}"),
            };
            stamp_at_position(ctx, position, margin_x, margin_y, &text)
        },
    )
}

/// Add one text watermark to all pages or a 1-based page selection.
///
/// The watermark is centered on the displayed page. Opacity below `1.0` is
/// implemented with an `ExtGState`; a fully opaque watermark adds no graphics
/// state resource.
///
/// # Errors
///
/// [`ToolError`](crate::ToolError) when the text is empty or not
/// WinAnsi-encodable, opacity/color/font size are invalid, the document
/// cannot be parsed, or the output fails structural verification.
pub fn add_watermark(
    path: &str,
    options: &WatermarkOptions,
    output: &str,
) -> ToolResult<Verification> {
    if options.text.is_empty() {
        return Err(CliError("watermark text must not be empty".to_string()));
    }
    let style = OverlayStyle {
        font_size: options.font_size,
        color: options.color,
        opacity: options.opacity,
    };
    validate_style(&style).map_err(CliError)?;
    let encoded = encode_win_ansi(&options.text).map_err(CliError)?;
    let orientation = options.orientation;
    apply_overlay(
        &OverlayJob {
            path,
            output,
            pages: options.pages.as_deref(),
            style,
            action: "added watermark",
        },
        |ctx| Ok(stamp_watermark(ctx, orientation, &encoded)),
    )
}

/// Add header and/or footer text with left, center, and right slots.
///
/// Placeholders `{page}` and `{pages}` expand to the physical page number and
/// total page count. A slot that is empty renders nothing.
///
/// # Errors
///
/// [`ToolError`](crate::ToolError) when both lines are empty, a placeholder is
/// unknown, a slot is not WinAnsi-encodable, margins/font size/color are
/// invalid, the document cannot be parsed, or the output fails structural
/// verification.
pub fn add_header_footer(
    path: &str,
    options: &HeaderFooterOptions,
    output: &str,
) -> ToolResult<Verification> {
    if options.header.is_empty() && options.footer.is_empty() {
        return Err(CliError(
            "header/footer needs at least one non-empty slot".to_string(),
        ));
    }
    let style = OverlayStyle {
        font_size: options.font_size,
        color: options.color,
        opacity: 1.0,
    };
    validate_style(&style).map_err(CliError)?;
    validate_margin(options.margin_x, "margin_x").map_err(CliError)?;
    validate_margin(options.header_margin, "header_margin").map_err(CliError)?;
    validate_margin(options.footer_margin, "footer_margin").map_err(CliError)?;
    let header = options.header.clone();
    let footer = options.footer.clone();
    let margin_x = options.margin_x;
    let header_margin = options.header_margin;
    let footer_margin = options.footer_margin;
    apply_overlay(
        &OverlayJob {
            path,
            output,
            pages: options.pages.as_deref(),
            style,
            action: "added header/footer",
        },
        |ctx| {
            let mut content = Vec::new();
            let (_, display_height) = ctx.geometry.display_size();
            if !header.is_empty() {
                push_slots(
                    &mut content,
                    ctx,
                    &header,
                    ctx.geometry
                        .clamp_baseline(display_height - header_margin, ctx.style.font_size),
                    margin_x,
                )?;
            }
            if !footer.is_empty() {
                push_slots(
                    &mut content,
                    ctx,
                    &footer,
                    ctx.geometry
                        .clamp_baseline(footer_margin, ctx.style.font_size),
                    margin_x,
                )?;
            }
            Ok(content)
        },
    )
}

/// Add Bates numbering to all pages or a 1-based page selection.
///
/// Each selected page is stamped once, in page order. `digits` is a minimum
/// width; a larger number is printed in full. An empty prefix and suffix are
/// valid.
///
/// # Errors
///
/// [`ToolError`](crate::ToolError) when `digits` is outside `1..=20`, the
/// stamp is not WinAnsi-encodable, margins/font size/color are invalid, the
/// document cannot be parsed, or the output fails structural verification.
pub fn add_bates(path: &str, options: &BatesOptions, output: &str) -> ToolResult<Verification> {
    if options.digits == 0 || options.digits > MAX_BATES_DIGITS {
        return Err(CliError(format!(
            "Bates digit width must be between 1 and {MAX_BATES_DIGITS} (got {})",
            options.digits
        )));
    }
    let style = OverlayStyle {
        font_size: options.font_size,
        color: options.color,
        opacity: 1.0,
    };
    validate_style(&style).map_err(CliError)?;
    validate_margin(options.margin_x, "margin_x").map_err(CliError)?;
    validate_margin(options.margin_y, "margin_y").map_err(CliError)?;
    let digits = options.digits;
    let start = options.start;
    let prefix = options.prefix.clone();
    let suffix = options.suffix.clone();
    let position = options.position;
    let margin_x = options.margin_x;
    let margin_y = options.margin_y;
    apply_overlay(
        &OverlayJob {
            path,
            output,
            pages: options.pages.as_deref(),
            style,
            action: "added Bates numbers",
        },
        |ctx| {
            let number = u64::from(start).saturating_add(ctx.selected_ordinal as u64);
            let text = format!("{prefix}{number:0digits$}{suffix}");
            stamp_at_position(ctx, position, margin_x, margin_y, &text)
        },
    )
}

/// One page's geometry in its displayed (rotation-applied) orientation.
#[derive(Debug, Clone, Copy)]
struct PageGeometry {
    /// Unrotated media-box width, in points.
    width: f64,
    /// Unrotated media-box height, in points.
    height: f64,
    /// Normalized `/Rotate` (0, 90, 180, or 270).
    rotation: i64,
}

impl PageGeometry {
    fn new(media: (f64, f64), rotation: i64) -> Result<Self, String> {
        let (width, height) = media;
        if !width.is_finite() || !height.is_finite() || width <= 0.0 || height <= 0.0 {
            return Err(format!(
                "page has an invalid media box ({width} x {height} points)"
            ));
        }
        let rotation = rotation.rem_euclid(360);
        if !matches!(rotation, 0 | 90 | 180 | 270) {
            return Err(format!(
                "page rotation {rotation} is not a multiple of 90 degrees"
            ));
        }
        Ok(Self {
            width,
            height,
            rotation,
        })
    }

    fn display_size(&self) -> (f64, f64) {
        if matches!(self.rotation, 90 | 270) {
            (self.height, self.width)
        } else {
            (self.width, self.height)
        }
    }

    /// Convert a display-space point (bottom-left origin) into user space.
    fn display_to_user(&self, u: f64, v: f64) -> (f64, f64) {
        match self.rotation {
            90 => (self.width - v, u),
            180 => (self.width - u, self.height - v),
            270 => (v, self.height - u),
            _ => (u, v),
        }
    }

    /// Map a display-space direction into user space.
    fn display_dir_to_user(&self, du: f64, dv: f64) -> (f64, f64) {
        match self.rotation {
            90 => (-dv, du),
            180 => (-du, -dv),
            270 => (dv, -du),
            _ => (du, dv),
        }
    }

    /// A text matrix that draws at display-space `(u, v)` with display-space
    /// baseline angle `angle` (radians), upright to the reader.
    fn text_matrix(&self, u: f64, v: f64, angle: f64) -> [f64; 6] {
        let (sin, cos) = angle.sin_cos();
        let (a, b) = self.display_dir_to_user(cos, sin);
        let (c, d) = self.display_dir_to_user(-sin, cos);
        let (e, f) = self.display_to_user(u, v);
        [a, b, c, d, e, f]
    }

    fn clamp_x(&self, x: f64, text_width: f64) -> f64 {
        let (display_width, _) = self.display_size();
        let max = (display_width - text_width).max(0.0);
        x.clamp(0.0, max)
    }

    fn clamp_baseline(&self, y: f64, font_size: f64) -> f64 {
        let (_, display_height) = self.display_size();
        let low = (font_size * 0.2).min(display_height * 0.5);
        let high = (display_height - font_size * 0.8).max(low);
        y.clamp(low, high)
    }
}

/// The style shared by every overlay stamp.
#[derive(Debug, Clone, Copy)]
struct OverlayStyle {
    font_size: f64,
    color: OverlayColor,
    opacity: f64,
}

/// Everything a per-page overlay builder needs.
struct StampContext<'a> {
    /// Zero-based physical page index.
    page_index: usize,
    /// Total physical page count.
    page_count: usize,
    /// Zero-based position within the selected pages.
    selected_ordinal: usize,
    /// The page's geometry.
    geometry: PageGeometry,
    /// The font resource name chosen for this document.
    font_name: &'a str,
    /// The graphics-state resource name, when opacity requires one.
    gs_name: Option<&'a str>,
    /// The overlay style.
    style: OverlayStyle,
}

/// A path-based overlay request.
struct OverlayJob<'a> {
    path: &'a str,
    output: &'a str,
    pages: Option<&'a str>,
    style: OverlayStyle,
    action: &'a str,
}

/// Copy the document, append stamps to the selected pages, and verify.
fn apply_overlay<F>(job: &OverlayJob<'_>, mut build: F) -> CliResult<Verification>
where
    F: FnMut(&StampContext<'_>) -> Result<Vec<u8>, String>,
{
    let src = read_file(job.path)?;
    let in_bytes = u64::try_from(src.len()).unwrap_or(u64::MAX);
    let budget = Budget::unlimited();
    let clock = crate::shell_clock();
    let mut g = crate::runtime::cli_guard(&budget, &clock);
    let startxref = selis_pdf_cos::xref::find_startxref(&src, 4096).unwrap_or(0);
    let doc = selis_pdf_cos::parse_revisions(&src, startxref, &budget, &mut g)
        .map_err(|e| CliError(format!("{}: cannot open: {e}", job.path)))?;
    let page_refs = crate::tools::page_refs_of(&src, &doc, &budget, &mut g)
        .map_err(|e| CliError(format!("{}: {e}", job.path)))?;
    if page_refs.is_empty() {
        return Err(CliError(format!("{}: document has no pages", job.path)));
    }
    let mut selected = match job.pages {
        Some(spec) => parse_one_based_selection(spec, page_refs.len())
            .map_err(|e| CliError(format!("{}: {e}", job.path)))?,
        None => (0..page_refs.len()).collect(),
    };
    // Sequential numbering follows physical page order even when the user
    // typed the selection out of order (`3,1` numbers page 1 first).
    selected.sort_unstable();
    let ordinal_by_page: HashMap<usize, usize> = selected
        .iter()
        .enumerate()
        .map(|(ordinal, page)| (*page, ordinal))
        .collect();

    let used_names =
        resource_names_in_use(&src, &doc, &page_refs, &budget, &mut g).map_err(CliError)?;
    let font_name = unique_name("SelisOverlayFont", &used_names);
    let gs_name = unique_name("SelisOverlayGS", &used_names);

    let mut out = DocumentBuilder::new();
    let mut next_num = 3u32;
    let font_ref = {
        let num = out.allocate();
        out.add_object(num, font_object());
        Ref::new(num, 0)
    };
    let gs_ref = if job.style.opacity < 1.0 {
        let num = out.allocate();
        out.add_object(num, gs_object(job.style.opacity));
        Some(Ref::new(num, 0))
    } else {
        None
    };

    let mut kept: Vec<crate::tools::KeptPage> = Vec::with_capacity(page_refs.len());
    for (page_index, page_ref) in page_refs.iter().enumerate() {
        let (media, _, _, rotation, _) =
            crate::tools::page_info(&src, &doc, *page_ref, &budget, &mut g)
                .map_err(|e| CliError(format!("{}: {e}", job.path)))?;
        let geometry = PageGeometry::new(media, rotation)
            .map_err(|e| CliError(format!("{}: page {}: {e}", job.path, page_index + 1)))?;
        let merged_ref =
            crate::tools::copy_page(&mut out, &src, *page_ref, &budget, &mut g, &mut next_num)
                .map_err(|e| CliError(format!("{}: {e}", job.path)))?;

        if let Some(&selected_ordinal) = ordinal_by_page.get(&page_index) {
            let ctx = StampContext {
                page_index,
                page_count: page_refs.len(),
                selected_ordinal,
                geometry,
                font_name: &font_name,
                gs_name: gs_ref.as_ref().map(|_| gs_name.as_str()),
                style: job.style,
            };
            let content = build(&ctx)
                .map_err(|e| CliError(format!("{}: page {}: {e}", job.path, page_index + 1)))?;
            let content_ref = add_stream(&mut out, &content);
            append_content(&mut out, merged_ref, content_ref)
                .map_err(|e| CliError(format!("{}: {e}", job.path)))?;
            let resources_ref = ensure_resources_ref(&mut out, merged_ref)
                .map_err(|e| CliError(format!("{}: {e}", job.path)))?;
            add_subdict_entry(
                &mut out,
                resources_ref,
                b"Font",
                &font_name,
                Obj::Ref(font_ref),
            )
            .map_err(|e| CliError(format!("{}: {e}", job.path)))?;
            if let Some(gs_ref) = gs_ref {
                add_subdict_entry(
                    &mut out,
                    resources_ref,
                    b"ExtGState",
                    &gs_name,
                    Obj::Ref(gs_ref),
                )
                .map_err(|e| CliError(format!("{}: {e}", job.path)))?;
            }
        }

        kept.push((page_index, page_ref.num, merged_ref));
    }

    crate::tools::reconcile_single_input(
        &mut out, &src, &doc, &page_refs, &kept, true, &budget, &mut g,
    )
    .map_err(CliError)?;
    let verification = crate::tools::write_document(
        out,
        job.output,
        Some(&src),
        PRESERVE_ALL,
        Some(u64::try_from(page_refs.len()).unwrap_or(u64::MAX)),
        None,
        in_bytes,
        &budget,
        &mut g,
    )?;
    eprintln!("{} -> {}", job.action, job.output);
    verification.emit_line();
    Ok(verification)
}

/// Render `text` at a page corner/edge.
fn stamp_at_position(
    ctx: &StampContext<'_>,
    position: OverlayPosition,
    margin_x: f64,
    margin_y: f64,
    text: &str,
) -> Result<Vec<u8>, String> {
    let encoded = encode_win_ansi(text)?;
    let width = text_width(&encoded, ctx.style.font_size);
    let (display_width, display_height) = ctx.geometry.display_size();
    let horizontal = match position {
        OverlayPosition::TopLeft | OverlayPosition::BottomLeft => HorizontalAlignment::Left,
        OverlayPosition::TopCenter | OverlayPosition::BottomCenter => HorizontalAlignment::Center,
        OverlayPosition::TopRight | OverlayPosition::BottomRight => HorizontalAlignment::Right,
    };
    let top = matches!(
        position,
        OverlayPosition::TopLeft | OverlayPosition::TopCenter | OverlayPosition::TopRight
    );
    let mut x = match horizontal {
        HorizontalAlignment::Left => margin_x,
        HorizontalAlignment::Center => (display_width - width) / 2.0,
        HorizontalAlignment::Right => display_width - margin_x - width,
    };
    x = ctx.geometry.clamp_x(x, width);
    let raw_y = if top {
        display_height - margin_y
    } else {
        margin_y
    };
    let y = ctx.geometry.clamp_baseline(raw_y, ctx.style.font_size);
    let matrix = ctx.geometry.text_matrix(x, y, 0.0);
    let mut content = Vec::new();
    push_text_block(&mut content, ctx, matrix, &encoded);
    Ok(content)
}

/// Render a single watermark line centered on the page.
fn stamp_watermark(
    ctx: &StampContext<'_>,
    orientation: WatermarkOrientation,
    encoded: &[u8],
) -> Vec<u8> {
    let width = text_width(encoded, ctx.style.font_size);
    let (display_width, display_height) = ctx.geometry.display_size();
    let (u, v, angle) = match orientation {
        WatermarkOrientation::Horizontal => (
            (display_width - width) / 2.0,
            (display_height - ctx.style.font_size * 0.7) / 2.0,
            0.0,
        ),
        WatermarkOrientation::Diagonal => {
            let angle = std::f64::consts::FRAC_PI_4;
            let (sin, cos) = angle.sin_cos();
            (
                display_width / 2.0 - cos * width / 2.0 + sin * ctx.style.font_size * 0.35,
                display_height / 2.0 - sin * width / 2.0 - cos * ctx.style.font_size * 0.35,
                angle,
            )
        }
    };
    let matrix = ctx.geometry.text_matrix(u, v, angle);
    let mut content = Vec::new();
    push_text_block(&mut content, ctx, matrix, encoded);
    content
}

/// Render one header/footer line's slots at baseline `y`.
fn push_slots(
    content: &mut Vec<u8>,
    ctx: &StampContext<'_>,
    slots: &HeaderFooterSlots,
    y: f64,
    margin_x: f64,
) -> Result<(), String> {
    let (display_width, _) = ctx.geometry.display_size();
    for (text, align) in [
        (&slots.left, HorizontalAlignment::Left),
        (&slots.center, HorizontalAlignment::Center),
        (&slots.right, HorizontalAlignment::Right),
    ] {
        if text.is_empty() {
            continue;
        }
        let expanded = expand_placeholders(text, ctx.page_index + 1, ctx.page_count)?;
        let encoded = encode_win_ansi(&expanded)?;
        let width = text_width(&encoded, ctx.style.font_size);
        let mut x = match align {
            HorizontalAlignment::Left => margin_x,
            HorizontalAlignment::Center => (display_width - width) / 2.0,
            HorizontalAlignment::Right => display_width - margin_x - width,
        };
        x = ctx.geometry.clamp_x(x, width);
        let matrix = ctx.geometry.text_matrix(x, y, 0.0);
        push_text_block(content, ctx, matrix, &encoded);
    }
    Ok(())
}

/// Append one `q ... Q` text block with the given text matrix.
fn push_text_block(
    content: &mut Vec<u8>,
    ctx: &StampContext<'_>,
    matrix: [f64; 6],
    encoded: &[u8],
) {
    content.extend_from_slice(b"q\n");
    if let Some(gs_name) = ctx.gs_name {
        content.extend_from_slice(format!("/{gs_name} gs\n").as_bytes());
    }
    let color = ctx.style.color;
    content.extend_from_slice(
        format!(
            "{} {} {} rg\n",
            number(color.r),
            number(color.g),
            number(color.b)
        )
        .as_bytes(),
    );
    content.extend_from_slice(b"BT\n");
    content.extend_from_slice(
        format!("/{} {} Tf\n", ctx.font_name, number(ctx.style.font_size)).as_bytes(),
    );
    let [a, b, c, d, e, f] = matrix;
    content.extend_from_slice(
        format!(
            "{} {} {} {} {} {} Tm\n",
            number(a),
            number(b),
            number(c),
            number(d),
            number(e),
            number(f)
        )
        .as_bytes(),
    );
    push_escaped_string(content, encoded);
    content.extend_from_slice(b" Tj\nET\nQ\n");
}

/// Escape a WinAnsi byte string as a PDF literal string.
fn push_escaped_string(out: &mut Vec<u8>, text: &[u8]) {
    out.push(b'(');
    for &byte in text {
        match byte {
            b'(' => out.extend_from_slice(b"\\("),
            b')' => out.extend_from_slice(b"\\)"),
            b'\\' => out.extend_from_slice(b"\\\\"),
            _ => out.push(byte),
        }
    }
    out.push(b')');
}

/// Expand `{page}`/`{pages}` placeholders in a header/footer slot.
fn expand_placeholders(template: &str, page: usize, pages: usize) -> Result<String, String> {
    let mut out = String::with_capacity(template.len());
    let mut rest = template;
    loop {
        let Some(open) = rest.find('{') else {
            out.push_str(rest);
            break;
        };
        out.push_str(&rest[..open]);
        let after = &rest[open + 1..];
        let Some(close) = after.find('}') else {
            return Err(format!("unterminated placeholder in `{template}`"));
        };
        let token = &after[..close];
        match token {
            "page" => out.push_str(&page.to_string()),
            "pages" => out.push_str(&pages.to_string()),
            other => {
                return Err(format!(
                    "unknown placeholder `{{{other}}}`; supported placeholders are {{page}} and {{pages}}"
                ));
            }
        }
        rest = &after[close + 1..];
    }
    Ok(out)
}

/// Encode UTF-8 text as WinAnsi bytes, rejecting characters the encoding
/// cannot represent.
fn encode_win_ansi(text: &str) -> Result<Vec<u8>, String> {
    let map = win_ansi_reverse();
    let mut out = Vec::with_capacity(text.len());
    for ch in text.chars() {
        if ch.is_control() {
            return Err(format!(
                "character U+{:04X} is a control character and cannot be stamped",
                u32::from(ch)
            ));
        }
        let code = map.get(&u32::from(ch)).ok_or_else(|| {
            format!(
                "character `{ch}` (U+{:04X}) is not representable in WinAnsi",
                u32::from(ch)
            )
        })?;
        out.push(*code);
    }
    Ok(out)
}

/// The reverse WinAnsi map (Unicode scalar → byte), built once.
fn win_ansi_reverse() -> &'static HashMap<u32, u8> {
    static MAP: OnceLock<HashMap<u32, u8>> = OnceLock::new();
    MAP.get_or_init(|| {
        let mut map = HashMap::new();
        for (code, name) in selis_font::tables::WinAnsiEncoding.iter().enumerate() {
            if *name == ".notdef" {
                continue;
            }
            let Some(unicode) = selis_font::agl::glyph_to_unicode(name) else {
                continue;
            };
            let Ok(code) = u8::try_from(code) else {
                continue;
            };
            let _ = map.entry(unicode).or_insert(code);
        }
        map
    })
}

/// Width of a WinAnsi string in Helvetica at `font_size` points.
fn text_width(text: &[u8], font_size: f64) -> f64 {
    let mut total = 0.0;
    for &code in text {
        let name = selis_font::tables::WinAnsiEncoding
            .get(usize::from(code))
            .copied()
            .unwrap_or("space");
        let advance = selis_font::standard14::width("Helvetica", name).unwrap_or(556);
        total += f64::from(advance) * font_size / 1000.0;
    }
    total
}

/// Format a coordinate/color with the writer's three-decimal convention.
fn number(value: f64) -> String {
    let text = format!("{value:.3}");
    let text = text.trim_end_matches('0').trim_end_matches('.').to_string();
    if text.is_empty() || text == "-" || text == "-0" {
        "0".to_string()
    } else {
        text
    }
}

/// A dictionary `Real` with three fractional digits.
fn real_object(value: f64) -> Obj {
    let scaled = (value * 1000.0).round();
    Obj::Real {
        scaled: scaled as i64,
        scale: 3,
    }
}

/// Parse a 1-based page selection (`"1-3,5"`) into zero-based indices.
fn parse_one_based_selection(spec: &str, count: usize) -> Result<Vec<usize>, String> {
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    for raw in spec.split(',') {
        let part = raw.trim();
        if part.is_empty() {
            continue;
        }
        let (start, end) = match part.split_once('-') {
            Some((first, last)) => {
                let start = parse_page_number(first.trim(), part)?;
                let end = parse_page_number(last.trim(), part)?;
                (start, end)
            }
            None => {
                let page = parse_page_number(part, part)?;
                (page, page)
            }
        };
        if start == 0 {
            return Err(format!(
                "page selection `{part}` is 1-based; page numbers start at 1"
            ));
        }
        if end < start {
            return Err(format!("page range `{part}` goes backwards"));
        }
        if end > count {
            return Err(format!(
                "page selection `{part}` is out of range (document has {count} pages)"
            ));
        }
        for page in start..=end {
            let zero = page - 1;
            if seen.insert(zero) {
                out.push(zero);
            }
        }
    }
    if out.is_empty() {
        return Err(format!("`{spec}` selects no pages"));
    }
    Ok(out)
}

/// Parse one page number from a selection part.
fn parse_page_number(value: &str, part: &str) -> Result<usize, String> {
    value
        .parse::<usize>()
        .map_err(|_| format!("`{part}` is not a page number"))
}

/// Collect resource names already used by any `/Font` or `/ExtGState`
/// subdictionary, so the overlay never shadows an existing resource.
fn resource_names_in_use(
    src: &[u8],
    doc: &selis_pdf_cos::Doc,
    page_refs: &[Ref],
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
) -> Result<HashSet<Vec<u8>>, String> {
    let mut used = HashSet::new();
    for page_ref in page_refs {
        let (_, _, resources, _, _) = crate::tools::page_info(src, doc, *page_ref, budget, g)?;
        let Some(resources) = resources else {
            continue;
        };
        let resources = resolve_value(src, doc, resources, budget, g)?;
        let Obj::Dict(pairs) = resources else {
            continue;
        };
        for key in [b"Font".as_slice(), b"ExtGState".as_slice()] {
            let Some((_, value)) = pairs.iter().find(|(k, _)| k.as_slice() == key) else {
                continue;
            };
            let value = resolve_value(src, doc, value.clone(), budget, g)?;
            if let Obj::Dict(entries) = value {
                for (name, _) in entries {
                    let _ = used.insert(name.to_vec());
                }
            }
        }
    }
    Ok(used)
}

/// Resolve an indirect value up to a small depth, for resource inspection.
fn resolve_value(
    src: &[u8],
    doc: &selis_pdf_cos::Doc,
    value: Obj,
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
) -> Result<Obj, String> {
    let mut current = value;
    for _ in 0..8 {
        match current {
            Obj::Ref(reference) => {
                current = selis_pdf_cos::copy::resolve_ref(src, doc, reference, budget, g)
                    .map_err(|e| e.to_string())?;
            }
            other => return Ok(other),
        }
    }
    Err("resource dictionary nesting is too deep".to_string())
}

/// Pick a resource name not present in `used`.
fn unique_name(base: &str, used: &HashSet<Vec<u8>>) -> String {
    if !used.contains(base.as_bytes()) {
        return base.to_string();
    }
    let mut index = 1u32;
    loop {
        let candidate = format!("{base}{index}");
        if !used.contains(candidate.as_bytes()) {
            return candidate;
        }
        index = index.saturating_add(1);
    }
}

/// The standard-14 Helvetica font dictionary used by every overlay.
fn font_object() -> Obj {
    Obj::Dict(vec![
        (bytes(b"Type"), Obj::Name(bytes(b"Font"))),
        (bytes(b"Subtype"), Obj::Name(bytes(b"Type1"))),
        (bytes(b"BaseFont"), Obj::Name(bytes(b"Helvetica"))),
        (bytes(b"Encoding"), Obj::Name(bytes(b"WinAnsiEncoding"))),
    ])
}

/// A graphics state carrying the watermark opacity for fill and stroke.
fn gs_object(opacity: f64) -> Obj {
    Obj::Dict(vec![
        (bytes(b"Type"), Obj::Name(bytes(b"ExtGState"))),
        (bytes(b"ca"), real_object(opacity)),
        (bytes(b"CA"), real_object(opacity)),
    ])
}

/// Add one content stream object and return its reference.
fn add_stream(out: &mut DocumentBuilder, content: &[u8]) -> Ref {
    let num = out.allocate();
    out.add_object(
        num,
        Obj::Stream {
            dict: vec![(
                bytes(b"Length"),
                Obj::Int(i64::try_from(content.len()).unwrap_or(i64::MAX)),
            )],
            data: Bytes::from(content.to_vec()),
        },
    );
    Ref::new(num, 0)
}

/// Find an object's index by object number.
fn object_index(objects: &[(u32, Obj)], num: u32) -> Option<usize> {
    objects.iter().position(|(number, _)| *number == num)
}

/// Append `extra` after the page's existing content streams.
fn append_content(out: &mut DocumentBuilder, page: Ref, extra: Ref) -> Result<(), String> {
    let page_index = object_index(out.objects_mut().as_slice(), page.num)
        .ok_or_else(|| format!("page object {} was not found", page.num))?;
    let contents = {
        let objects = out.objects_mut();
        let (_, page_obj) = objects
            .get(page_index)
            .ok_or_else(|| "page object disappeared".to_string())?;
        let Obj::Dict(pairs) = page_obj else {
            return Err("page object is not a dictionary".to_string());
        };
        pairs
            .iter()
            .find(|(key, _)| key.as_slice() == b"Contents")
            .map(|(_, value)| value.clone())
    };

    // A `/Contents` reference may point at an array object. Appending into
    // that array keeps the page valid (an array of streams must not gain a
    // nested array child).
    if let Some(Obj::Ref(reference)) = contents {
        if let Some(index) = object_index(out.objects_mut().as_slice(), reference.num) {
            let objects = out.objects_mut();
            if let Some((_, Obj::Array(items))) = objects.get_mut(index) {
                items.push(Obj::Ref(extra));
                return Ok(());
            }
        }
    }

    let objects = out.objects_mut();
    let (_, page_obj) = objects
        .get_mut(page_index)
        .ok_or_else(|| "page object disappeared".to_string())?;
    let Obj::Dict(pairs) = page_obj else {
        return Err("page object is not a dictionary".to_string());
    };
    if let Some((_, slot)) = pairs
        .iter_mut()
        .find(|(key, _)| key.as_slice() == b"Contents")
    {
        match slot {
            Obj::Array(items) => items.push(Obj::Ref(extra)),
            Obj::Ref(existing) => {
                *slot = Obj::Array(vec![Obj::Ref(*existing), Obj::Ref(extra)]);
            }
            Obj::Null => *slot = Obj::Ref(extra),
            _ => return Err("page /Contents is not a stream or array".to_string()),
        }
    } else {
        pairs.push((bytes(b"Contents"), Obj::Ref(extra)));
    }
    Ok(())
}

/// Ensure the page has an indirect `/Resources` dictionary and return it.
fn ensure_resources_ref(out: &mut DocumentBuilder, page: Ref) -> Result<Ref, String> {
    let page_index = object_index(out.objects_mut().as_slice(), page.num)
        .ok_or_else(|| format!("page object {} was not found", page.num))?;
    let existing = {
        let objects = out.objects_mut();
        let (_, page_obj) = objects
            .get(page_index)
            .ok_or_else(|| "page object disappeared".to_string())?;
        let Obj::Dict(pairs) = page_obj else {
            return Err("page object is not a dictionary".to_string());
        };
        pairs
            .iter()
            .find(|(key, _)| key.as_slice() == b"Resources")
            .map(|(_, value)| value.clone())
    };
    match existing {
        Some(Obj::Ref(reference)) => Ok(reference),
        Some(inline @ Obj::Dict(_)) => {
            let num = out.allocate();
            out.add_object(num, inline);
            let reference = Ref::new(num, 0);
            set_page_entry(out, page, b"Resources", Obj::Ref(reference))?;
            Ok(reference)
        }
        None => {
            let num = out.allocate();
            out.add_object(num, Obj::Dict(Vec::new()));
            let reference = Ref::new(num, 0);
            set_page_entry(out, page, b"Resources", Obj::Ref(reference))?;
            Ok(reference)
        }
        _ => Err("page /Resources is not a dictionary".to_string()),
    }
}

/// Set or append one page-dictionary entry.
fn set_page_entry(
    out: &mut DocumentBuilder,
    page: Ref,
    key: &[u8],
    value: Obj,
) -> Result<(), String> {
    let page_index = object_index(out.objects_mut().as_slice(), page.num)
        .ok_or_else(|| format!("page object {} was not found", page.num))?;
    let objects = out.objects_mut();
    let (_, page_obj) = objects
        .get_mut(page_index)
        .ok_or_else(|| "page object disappeared".to_string())?;
    let Obj::Dict(pairs) = page_obj else {
        return Err("page object is not a dictionary".to_string());
    };
    if let Some((_, slot)) = pairs.iter_mut().find(|(entry, _)| entry.as_slice() == key) {
        *slot = value;
    } else {
        pairs.push((bytes(key), value));
    }
    Ok(())
}

/// Add `value` under `name` in the resources' `owner_key` subdictionary,
/// following one indirect reference when the subdictionary is indirect.
fn add_subdict_entry(
    out: &mut DocumentBuilder,
    resources: Ref,
    owner_key: &[u8],
    name: &str,
    value: Obj,
) -> Result<(), String> {
    let resources_index = object_index(out.objects_mut().as_slice(), resources.num)
        .ok_or_else(|| format!("resource object {} was not found", resources.num))?;
    let indirect_target = {
        let objects = out.objects_mut();
        let (_, object) = objects
            .get(resources_index)
            .ok_or_else(|| "resource object disappeared".to_string())?;
        let Obj::Dict(pairs) = object else {
            return Err("page /Resources is not a dictionary".to_string());
        };
        match pairs.iter().find(|(key, _)| key.as_slice() == owner_key) {
            None => None,
            Some((_, Obj::Dict(_))) => None,
            Some((_, Obj::Ref(reference))) => Some(*reference),
            Some(_) => {
                return Err(format!(
                    "page /Resources /{} is not a dictionary",
                    String::from_utf8_lossy(owner_key)
                ));
            }
        }
    };

    if let Some(target) = indirect_target {
        let target_index = object_index(out.objects_mut().as_slice(), target.num)
            .ok_or_else(|| format!("resource subdictionary {} was not found", target.num))?;
        let objects = out.objects_mut();
        let (_, object) = objects
            .get_mut(target_index)
            .ok_or_else(|| "resource subdictionary disappeared".to_string())?;
        let Obj::Dict(entries) = object else {
            return Err("resource subdictionary is not a dictionary".to_string());
        };
        if entries
            .iter()
            .any(|(key, _)| key.as_slice() == name.as_bytes())
        {
            return Err(format!("resource name `{name}` already exists"));
        }
        entries.push((bytes(name.as_bytes()), value));
        return Ok(());
    }

    let objects = out.objects_mut();
    let (_, object) = objects
        .get_mut(resources_index)
        .ok_or_else(|| "resource object disappeared".to_string())?;
    let Obj::Dict(pairs) = object else {
        return Err("page /Resources is not a dictionary".to_string());
    };
    if let Some((_, slot)) = pairs
        .iter_mut()
        .find(|(key, _)| key.as_slice() == owner_key)
    {
        let Obj::Dict(entries) = slot else {
            return Err("resource subdictionary is not a dictionary".to_string());
        };
        if entries
            .iter()
            .any(|(key, _)| key.as_slice() == name.as_bytes())
        {
            return Err(format!("resource name `{name}` already exists"));
        }
        entries.push((bytes(name.as_bytes()), value));
    } else {
        pairs.push((
            bytes(owner_key),
            Obj::Dict(vec![(bytes(name.as_bytes()), value)]),
        ));
    }
    Ok(())
}

/// A `selis_bytes::Bytes` from a byte slice.
fn bytes(value: &[u8]) -> Bytes {
    Bytes::copy_from_slice(value)
}

/// Where a line sits horizontally.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HorizontalAlignment {
    Left,
    Center,
    Right,
}

/// Validate a color's components.
fn validate_color(color: OverlayColor) -> Result<(), String> {
    for (name, value) in [("r", color.r), ("g", color.g), ("b", color.b)] {
        if !value.is_finite() || !(0.0..=1.0).contains(&value) {
            return Err(format!(
                "color component `{name}` must be between 0 and 1 (got {value})"
            ));
        }
    }
    Ok(())
}

/// Validate a font size.
fn validate_font_size(value: f64) -> Result<(), String> {
    if !value.is_finite() || value <= 0.0 || value > MAX_FONT_SIZE {
        return Err(format!(
            "font size must be greater than 0 and at most {MAX_FONT_SIZE} (got {value})"
        ));
    }
    Ok(())
}

/// Validate a non-negative, finite margin.
fn validate_margin(value: f64, name: &str) -> Result<(), String> {
    if !value.is_finite() || value < 0.0 {
        return Err(format!(
            "{name} must be a non-negative number (got {value})"
        ));
    }
    Ok(())
}

/// Validate everything an [`OverlayStyle`] carries.
fn validate_style(style: &OverlayStyle) -> Result<(), String> {
    validate_font_size(style.font_size)?;
    validate_color(style.color)?;
    if !style.opacity.is_finite() || !(0.0..=1.0).contains(&style.opacity) {
        return Err(format!(
            "opacity must be between 0 and 1 (got {})",
            style.opacity
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::indexing_slicing,
        clippy::panic
    )]

    use super::*;
    use selis_sandbox::Budget;

    fn temp_dir(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("selis-overlay-{}-{name}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).expect("create temp dir");
        dir
    }

    /// A three-page PDF with a page-level content stream on every page.
    fn three_page_pdf() -> Vec<u8> {
        let budget = Budget::unlimited();
        let mut g = budget.guard();
        let mut builder = DocumentBuilder::new();
        for index in 0..3 {
            let content = format!("q 1 0 0 1 {index} 0 cm Q");
            builder.add_page(200.0, 300.0, content.as_bytes());
        }
        builder.write(&budget, &mut g).expect("write fixture")
    }

    fn output_page_streams(path: &std::path::Path) -> Vec<String> {
        let bytes = std::fs::read(path).expect("read output");
        let budget = Budget::unlimited();
        let mut g = budget.guard();
        let startxref = selis_pdf_cos::xref::find_startxref(&bytes, 4096).unwrap_or(0);
        let doc = selis_pdf_cos::parse_revisions(&bytes, startxref, &budget, &mut g)
            .expect("parse output");
        let page_refs =
            crate::tools::page_refs_of(&bytes, &doc, &budget, &mut g).expect("page refs");
        page_refs
            .iter()
            .map(|page_ref| {
                let (_, contents, _, _, _) =
                    crate::tools::page_info(&bytes, &doc, *page_ref, &budget, &mut g)
                        .expect("page info");
                let mut text = String::new();
                for content_ref in contents {
                    let obj = selis_pdf_cos::copy::resolve_ref(
                        &bytes,
                        &doc,
                        content_ref,
                        &budget,
                        &mut g,
                    )
                    .expect("resolve content");
                    if let Obj::Stream { data, .. } = obj {
                        text.push_str(&String::from_utf8_lossy(data.as_slice()));
                    }
                }
                text
            })
            .collect()
    }

    fn run_page_numbers(dir: &std::path::Path, options: &PageNumberOptions) -> std::path::PathBuf {
        let input = dir.join("in.pdf");
        let output = dir.join("out.pdf");
        std::fs::write(&input, three_page_pdf()).expect("write input");
        add_page_numbers(input.to_str().unwrap(), options, output.to_str().unwrap())
            .expect("page numbers");
        output
    }

    #[test]
    fn page_number_selection_is_one_based_and_stamps_only_selected_pages() {
        let dir = temp_dir("page-numbers");
        let output = run_page_numbers(
            &dir,
            &PageNumberOptions {
                // Deliberately out of order: numbering still follows the
                // document, not the order the ranges were typed.
                pages: Some("3,1".to_string()),
                start: 5,
                format: PageNumberFormat::Fraction,
                position: OverlayPosition::TopLeft,
                font_size: 12.0,
                ..PageNumberOptions::default()
            },
        );
        let streams = output_page_streams(&output);
        assert_eq!(streams.len(), 3);
        assert!(
            streams[0].contains("(5/3) Tj"),
            "first page: {}",
            streams[0]
        );
        assert!(
            !streams[1].contains("SelisOverlayFont"),
            "unselected page must not be stamped: {}",
            streams[1]
        );
        assert!(
            streams[2].contains("(6/3) Tj"),
            "third page: {}",
            streams[2]
        );
    }

    #[test]
    fn watermark_opacity_uses_a_graphics_state_and_selection() {
        let dir = temp_dir("watermark");
        let input = dir.join("in.pdf");
        let output = dir.join("out.pdf");
        std::fs::write(&input, three_page_pdf()).expect("write input");
        add_watermark(
            input.to_str().unwrap(),
            &WatermarkOptions {
                text: "CONFIDENTIAL".to_string(),
                opacity: 0.35,
                pages: Some("2".to_string()),
                ..WatermarkOptions::default()
            },
            output.to_str().unwrap(),
        )
        .expect("watermark");
        let streams = output_page_streams(&output);
        assert!(streams[1].contains("/SelisOverlayGS gs"));
        assert!(streams[1].contains("(CONFIDENTIAL) Tj"));
        assert!(!streams[0].contains("SelisOverlayFont"));
        assert!(!streams[2].contains("SelisOverlayFont"));
    }

    #[test]
    fn opaque_watermark_does_not_add_a_graphics_state() {
        let dir = temp_dir("watermark-opaque");
        let input = dir.join("in.pdf");
        let output = dir.join("out.pdf");
        std::fs::write(&input, three_page_pdf()).expect("write input");
        add_watermark(
            input.to_str().unwrap(),
            &WatermarkOptions {
                text: "DRAFT".to_string(),
                opacity: 1.0,
                ..WatermarkOptions::default()
            },
            output.to_str().unwrap(),
        )
        .expect("watermark");
        let streams = output_page_streams(&output);
        for stream in &streams {
            assert!(!stream.contains("/SelisOverlayGS gs"));
            assert!(stream.contains("(DRAFT) Tj"));
        }
    }

    #[test]
    fn header_footer_expands_page_placeholders() {
        let dir = temp_dir("header-footer");
        let input = dir.join("in.pdf");
        let output = dir.join("out.pdf");
        std::fs::write(&input, three_page_pdf()).expect("write input");
        add_header_footer(
            input.to_str().unwrap(),
            &HeaderFooterOptions {
                header: HeaderFooterSlots {
                    left: "Page {page} of {pages}".to_string(),
                    center: String::new(),
                    right: String::new(),
                },
                footer: HeaderFooterSlots {
                    left: String::new(),
                    center: String::new(),
                    right: "Draft".to_string(),
                },
                ..HeaderFooterOptions::default()
            },
            output.to_str().unwrap(),
        )
        .expect("header/footer");
        let streams = output_page_streams(&output);
        assert!(streams[0].contains("(Page 1 of 3) Tj"));
        assert!(streams[1].contains("(Page 2 of 3) Tj"));
        assert!(streams[2].contains("(Page 3 of 3) Tj"));
        for stream in &streams {
            assert!(stream.contains("(Draft) Tj"));
        }
    }

    #[test]
    fn bates_numbers_pad_and_advance_only_over_selected_pages() {
        let dir = temp_dir("bates");
        let input = dir.join("in.pdf");
        let output = dir.join("out.pdf");
        std::fs::write(&input, three_page_pdf()).expect("write input");
        add_bates(
            input.to_str().unwrap(),
            &BatesOptions {
                prefix: "ABC-".to_string(),
                start: 9,
                digits: 4,
                pages: Some("2-3".to_string()),
                ..BatesOptions::default()
            },
            output.to_str().unwrap(),
        )
        .expect("bates");
        let streams = output_page_streams(&output);
        assert!(!streams[0].contains("SelisOverlayFont"));
        assert!(streams[1].contains("(ABC-0009) Tj"));
        assert!(streams[2].contains("(ABC-0010) Tj"));
    }

    #[test]
    fn invalid_overlay_options_fail_before_creating_output() {
        let dir = temp_dir("invalid");
        let input = dir.join("in.pdf");
        let output = dir.join("out.pdf");
        std::fs::write(&input, three_page_pdf()).expect("write input");

        let err = add_watermark(
            input.to_str().unwrap(),
            &WatermarkOptions {
                text: String::new(),
                ..WatermarkOptions::default()
            },
            output.to_str().unwrap(),
        )
        .expect_err("empty watermark");
        assert!(err.to_string().contains("must not be empty"));

        let err = add_header_footer(
            input.to_str().unwrap(),
            &HeaderFooterOptions::default(),
            output.to_str().unwrap(),
        )
        .expect_err("empty header/footer");
        assert!(err.to_string().contains("at least one"));

        let err = add_bates(
            input.to_str().unwrap(),
            &BatesOptions {
                digits: 0,
                ..BatesOptions::default()
            },
            output.to_str().unwrap(),
        )
        .expect_err("zero digits");
        assert!(err.to_string().contains("width"));

        assert!(!output.exists(), "failed operations commit nothing");
    }

    #[test]
    fn selection_parser_rejects_zero_out_of_range_and_backwards_ranges() {
        assert_eq!(
            parse_one_based_selection("1-2,2,3", 3).expect("selection"),
            vec![0, 1, 2]
        );
        assert!(parse_one_based_selection("0", 3).is_err());
        assert!(parse_one_based_selection("4", 3).is_err());
        assert!(parse_one_based_selection("2-1", 3).is_err());
        assert!(parse_one_based_selection(" , ", 3).is_err());
    }

    #[test]
    fn placeholders_and_win_ansi_reject_unknown_input() {
        assert_eq!(
            expand_placeholders("Page {page}/{pages}", 2, 3).expect("expand"),
            "Page 2/3"
        );
        assert!(expand_placeholders("{date}", 1, 1).is_err());
        assert!(expand_placeholders("{page", 1, 1).is_err());
        assert_eq!(encode_win_ansi("Caf\u{e9}").expect("WinAnsi"), b"Caf\xe9");
        assert!(encode_win_ansi("\u{1F600}").is_err());
    }

    #[test]
    fn rotated_page_matrix_keeps_overlay_upright() {
        let geometry = PageGeometry::new((200.0, 300.0), 90).expect("geometry");
        assert_eq!(geometry.display_size(), (300.0, 200.0));
        let matrix = geometry.text_matrix(10.0, 20.0, 0.0);
        assert_eq!(matrix, [0.0, 1.0, -1.0, 0.0, 180.0, 10.0]);
        let diagonal = geometry.text_matrix(10.0, 20.0, std::f64::consts::FRAC_PI_4);
        assert!(diagonal[0] < 0.0 && diagonal[1] > 0.0);
        assert!((diagonal[0] * diagonal[0] + diagonal[1] * diagonal[1] - 1.0).abs() < 1e-12);
        assert!((diagonal[0] * diagonal[2] + diagonal[1] * diagonal[3]).abs() < 1e-12);
    }
}
