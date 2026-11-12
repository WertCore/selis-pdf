//! Tool wrapper for the core `selis-pdf-annot` crate (Phase 5 — annotations).
//!
//! The annotation itself — the model, the appearance streams and the
//! incremental write — belongs to `selis-pdf-annot`. This module owns only the
//! file boundary and the write helper every writing tool shares: it reads the
//! input bytes, hands them to the core, and runs the result through the
//! WRITE.05 verification gate before it commits the file.

use selis_sandbox::Budget;

use crate::verify_report::Verification;
use crate::write_gate::{self, PRESERVE_ON_APPEND};
use crate::{read_file, CliError, ToolResult};

pub use selis_pdf_annot::{
	AnnotKind, AnnotSpec, PageAnnotation, HIGHLIGHT_COLOUR, INK_COLOUR, NOTE_COLOUR, SQUARE_COLOUR,
	STRIKEOUT_COLOUR, UNDERLINE_COLOUR,
};

/// Options for the browser/CLI annotate tool, deserialized from the tools
/// request. `page` is 0-based, like the edit-text tool's; the web UI shows a
/// 1-based page number and subtracts one before it sends the request.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct AnnotateOptions {
	/// 0-based index of the page the annotation goes on.
	pub page: u32,
	/// The kind of markup: `highlight`, `underline`, `strikeout`, `text`,
	/// `ink` or `square`.
	pub kind: String,
	/// The markup's rectangle, in the page's own user space.
	#[serde(default)]
	pub rect: [f64; 4],
	/// The text lines the markup covers, as rectangles in user space.
	#[serde(default)]
	pub quads: Vec<[f64; 4]>,
	/// The markup's colour, red, green and blue between 0 and 1.
	#[serde(default)]
	pub colour: Option<[f64; 3]>,
	/// The reader's comment.
	#[serde(default)]
	pub contents: String,
	/// Who left the markup.
	#[serde(default)]
	pub author: String,
	/// The points of a drawn mark.
	#[serde(default)]
	pub points: Vec<[f64; 2]>,
	/// The width of a stroke, in points.
	#[serde(default)]
	pub width: Option<f64>,
}

/// The kind a wire name asks for.
fn kind_of(name: &str) -> Option<AnnotKind> {
	match name.to_ascii_lowercase().as_str() {
		"highlight" => Some(AnnotKind::Highlight),
		"underline" => Some(AnnotKind::Underline),
		"strikeout" | "strike_out" => Some(AnnotKind::StrikeOut),
		"text" | "note" | "comment" => Some(AnnotKind::Text),
		"ink" | "draw" => Some(AnnotKind::Ink),
		"square" | "box" => Some(AnnotKind::Square),
		_ => None,
	}
}

/// The colour a kind wears when the caller names none.
fn default_colour(kind: AnnotKind) -> [f64; 3] {
	match kind {
		AnnotKind::Highlight => HIGHLIGHT_COLOUR,
		AnnotKind::Underline => UNDERLINE_COLOUR,
		AnnotKind::StrikeOut => STRIKEOUT_COLOUR,
		AnnotKind::Text => NOTE_COLOUR,
		AnnotKind::Ink => INK_COLOUR,
		AnnotKind::Square => SQUARE_COLOUR,
	}
}

/// Add one annotation to a page and write the verified result to `output`.
///
/// The write is an incremental update: the original bytes are preserved
/// verbatim as a prefix, so any digital signature over the original revision
/// stays valid.
///
/// # Errors
///
/// A [`crate::ToolError`] whose message names the refusal — an unreadable or
/// encrypted document, a page that does not exist, a markup the engine cannot
/// draw, or a rectangle that is not a rectangle. On any error no file is
/// committed.
pub fn annotate(
	path: &str,
	options: &AnnotateOptions,
	output: &str,
) -> ToolResult<Verification> {
	let src = read_file(path)?;
	let in_bytes = u64::try_from(src.len()).unwrap_or(u64::MAX);
	let budget = Budget::unlimited();
	let clock = crate::shell_clock();
	let mut g = crate::runtime::cli_guard(&budget, &clock);
	let kind = kind_of(&options.kind).ok_or_else(|| {
		CliError(format!(
			"{path}: {:?} is not a kind of markup this engine writes",
			options.kind
		))
	})?;
	let mut spec = AnnotSpec::new(
		kind,
		options.page.saturating_add(1),
		options.rect,
		options.colour.unwrap_or_else(|| default_colour(kind)),
	);
	spec.quads = options.quads.clone();
	spec.contents = options.contents.clone();
	spec.author = options.author.clone();
	spec.points = options
		.points
		.iter()
		.map(|point| (point[0], point[1]))
		.collect();
	if let Some(width) = options.width {
		spec.width = width;
	}
	let (bytes, report) = selis_pdf_annot::annotate(&src, &spec, &budget, &mut g)
		.map_err(|e| CliError(format!("{path}: {e}")))?;
	let observed = selis_pdf_cos::verify::survey(&src, &budget, &mut g)
		.map_err(|e| CliError(format!("{path}: input survey failed: {e}")))?;
	// The annotations GROW: a markup is added, so the count is not preserved
	// and the gate must not claim it is.
	let expected = write_gate::expectations_from(&observed, &PRESERVE_ON_APPEND);
	let verdict = write_gate::write_verified(&bytes, output, &expected, &budget, &mut g)?;
	let verification = Verification::from_gate(
		Some(&observed),
		&verdict,
		&expected,
		in_bytes,
		u64::try_from(bytes.len()).unwrap_or(u64::MAX),
	);
	eprintln!(
		"annotated page {}: {} markup(s) on the page, original bytes preserved -> {output}",
		report.page, report.annots
	);
	verification.emit_line();
	Ok(verification)
}

/// Options for the browser/CLI update-annotation tool.
///
/// The annotation is addressed by its position in the page's `/Annots` order,
/// exactly as the report lists them.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct UpdateAnnotationOptions {
	/// 0-based index of the page the annotation is on.
	pub page: u32,
	/// The annotation's position in the page's `/Annots` order.
	pub index: usize,
	/// Where the annotation goes, in the page's own user space.
	#[serde(default)]
	pub rect: Option<[f64; 4]>,
	/// What the annotation says now.
	#[serde(default)]
	pub contents: Option<String>,
	/// Whether the annotation is taken off the page instead of changed.
	#[serde(default)]
	pub remove: bool,
}

/// Move an annotation, or change what it says, and write the verified result
/// to `output`.
///
/// # Errors
///
/// A [`crate::ToolError`] whose message names the refusal — an unreadable
/// document, a page that does not exist, or an annotation the page does not
/// carry. On any error no file is committed.
pub fn update_annotation(
	path: &str,
	options: &UpdateAnnotationOptions,
	output: &str,
) -> ToolResult<Verification> {
	let src = read_file(path)?;
	let in_bytes = u64::try_from(src.len()).unwrap_or(u64::MAX);
	let budget = Budget::unlimited();
	let clock = crate::shell_clock();
	let mut g = crate::runtime::cli_guard(&budget, &clock);
	let (bytes, report) = if options.remove {
		selis_pdf_annot::remove_annotation(
			&src,
			options.page.saturating_add(1),
			options.index,
			&budget,
			&mut g,
		)
	} else {
		selis_pdf_annot::update_annotation(
			&src,
			options.page.saturating_add(1),
			options.index,
			options.rect,
			options.contents.clone(),
			&budget,
			&mut g,
		)
	}
	.map_err(|e| CliError(format!("{path}: {e}")))?;
	let observed = selis_pdf_cos::verify::survey(&src, &budget, &mut g)
		.map_err(|e| CliError(format!("{path}: input survey failed: {e}")))?;
	let expected = write_gate::expectations_from(&observed, &PRESERVE_ON_APPEND);
	let verdict = write_gate::write_verified(&bytes, output, &expected, &budget, &mut g)?;
	let verification = Verification::from_gate(
		Some(&observed),
		&verdict,
		&expected,
		in_bytes,
		u64::try_from(bytes.len()).unwrap_or(u64::MAX),
	);
	eprintln!(
		"updated an annotation on page {}: original bytes preserved -> {output}",
		report.page
	);
	verification.emit_line();
	Ok(verification)
}


/// The annotations a page carries, as a JSON report.
///
/// The report is a READ-ONLY view: it changes nothing and writes nothing.
///
/// # Errors
///
/// A [`crate::ToolError`] whose message names the refusal — an unreadable
/// document, or a page that does not exist.
pub fn annotations(path: &str, page: u32) -> ToolResult<String> {
	let src = read_file(path)?;
	let budget = Budget::unlimited();
	let clock = crate::shell_clock();
	let mut g = crate::runtime::cli_guard(&budget, &clock);
	let found = selis_pdf_annot::list_annotations(&src, page.saturating_add(1), &budget, &mut g)
		.map_err(|e| CliError(format!("{path}: {e}")))?;
	let rows: Vec<serde_json::Value> = found
		.iter()
		.map(|annotation| {
			serde_json::json!({
				"subtype": annotation.subtype,
				"rect": annotation.rect,
				"colour": annotation.colour,
				"contents": annotation.contents,
				"author": annotation.author,
				"quads": annotation.quads.len(),
			})
		})
		.collect();
	Ok(serde_json::json!({
		"page": page,
		"annotations": rows,
	})
	.to_string())
}
