//! Tool-layer bridge to `selis-pdf-edit`'s first-class add-content and
//! object-edit engines (SL-5.TEXTEDIT.05/06/07/08).
//!
//! The phase plan records a user-facing gap: the engine already implements
//! `add_text_box`, `add_image`, `add_shape`, `add_link`, the page-furniture
//! stamps and the object mutators, but the CLI and the browser only exposed
//! the older overlay "stamp"/"image-watermark" workarounds. This module is the
//! boundary those bindings call: it reads the input document from the shared
//! in-memory session, parses a JSON options object into the engine's typed
//! values, runs the engine, and commits the result through the same WRITE.05
//! gate every other writing tool uses.
//!
//! The engine crates stay network-free and filesystem-free: this module only
//! touches the session table via [`read_file`]/[`write_output`]/the write gate,
//! exactly like [`crate::edit`]. No new dependency is introduced.
//!
//! # Options
//!
//! A caller passes an op name and a JSON object. Rectangles are
//! `[x0, y0, x1, y1]` arrays in PDF user space; colours are `[r, g, b]` with
//! 8-bit components; an object id is `{ "name": "...", "op_index": 0 }`.
//! `add_image` and `replace_image` take the PNG/JPEG bytes as a second input
//! (`extra_bytes`) rather than embedding them in JSON.

#![allow(clippy::arithmetic_side_effects)]

use selis_geom::Rect;
use selis_pdf_edit::add_content::{
    self, AddContentError, AddContentReport, AddImage, Align, Background, BatesOptions,
    HeaderFooter, Link, LinkTarget, Rgb, Shape, ShapeKind, TextBox, Watermark,
};
use selis_pdf_edit::objects::{self, ObjectError, ObjectId};
use selis_sandbox::{Budget, BudgetGuard};
use serde_json::Value;

use crate::write_gate::{self, PRESERVE_ON_APPEND};
use crate::{read_file, write_output, CliError, ToolError, ToolResult, Verification};

/// The kind of artifact an op produced, used by bindings to label the output.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArtifactKind {
    /// A rewritten PDF (the incremental append).
    Pdf,
    /// A read-only JSON report (object listings).
    Report,
    /// An extracted binary (an image).
    File,
}

/// What one content op produced: a verification plus the display name/kind of
/// the output file already committed under `output`.
#[derive(Debug, Clone)]
pub struct ContentResult {
    /// The verification gate's measured result.
    pub verification: Verification,
    /// Display name for the produced file.
    pub name: String,
    /// What kind of file was produced.
    pub kind: ArtifactKind,
}

/// Run one content/object op.
///
/// `op` is one of the names documented on the module. `input` is the key of
/// the input document in the session; `output` is the key the produced file is
/// written to. `extra_bytes` carries a caller-supplied image for `add_image`
/// and `replace_image` and is ignored by every other op.
pub fn run(
    op: &str,
    options: &Value,
    input: &str,
    output: &str,
    extra_bytes: Option<&[u8]>,
) -> ToolResult<ContentResult> {
    let src = read_file(input)?;
    let stem = stem_of(input);
    match op {
        "add_text_box" => {
            let page = u32_opt(options, "page")?;
            let text_box = TextBox {
                text: string_opt(options, "text")?.to_string(),
                font: string_opt(options, "font")?.to_string(),
                font_size: num_opt(options, "font_size")?,
                color: rgb_opt(options, "color")?,
                rect: rect_opt(options, "rect")?,
            };
            let (bytes, report) =
                with_guard(|budget, g| add_content::add_text_box(&src, page, &text_box, budget, g))
                    .map_err(content_error)?;
            let verification = commit_pdf(&src, &bytes, output)?;
            disclose(&report);
            Ok(pdf_result(verification, &stem, "text"))
        }
        "add_image" => {
            let page = u32_opt(options, "page")?;
            let image = AddImage {
                rect: rect_opt(options, "rect")?,
                angle_degrees: num_default(options, "angle_degrees", 0.0)?,
            };
            let bytes_in = extra_bytes.ok_or_else(|| {
                ToolError("add_image needs an image as the second input".to_string())
            })?;
            let (bytes, report) = with_guard(|budget, g| {
                add_content::add_image(&src, page, &image, bytes_in, budget, g)
            })
            .map_err(content_error)?;
            let verification = commit_pdf(&src, &bytes, output)?;
            disclose(&report);
            Ok(pdf_result(verification, &stem, "image"))
        }
        "add_shape" => {
            let page = u32_opt(options, "page")?;
            let shape = Shape {
                kind: match string_opt(options, "kind")? {
                    "rect" => ShapeKind::Rect,
                    "ellipse" => ShapeKind::Ellipse,
                    "line" => ShapeKind::Line,
                    other => {
                        return Err(ToolError(format!("unknown shape kind: {other}")));
                    }
                },
                rect: rect_opt(options, "rect")?,
                fill: optional_rgb(options, "fill")?,
                stroke: optional_rgb(options, "stroke")?,
                stroke_width: num_default(options, "stroke_width", 1.0)?,
            };
            let (bytes, report) =
                with_guard(|budget, g| add_content::add_shape(&src, page, &shape, budget, g))
                    .map_err(content_error)?;
            let verification = commit_pdf(&src, &bytes, output)?;
            disclose(&report);
            Ok(pdf_result(verification, &stem, "shape"))
        }
        "add_link" => {
            let page = u32_opt(options, "page")?;
            let target = if let Some(uri) = optional_string(options, "uri")? {
                LinkTarget::Uri(uri)
            } else {
                LinkTarget::Page(u32_opt(options, "target_page")?)
            };
            let link = Link {
                rect: rect_opt(options, "rect")?,
                target,
                color: rgb_opt(options, "color")?,
                border: bool_default(options, "border", true)?,
            };
            let (bytes, report) =
                with_guard(|budget, g| add_content::add_link(&src, page, &link, budget, g))
                    .map_err(content_error)?;
            let verification = commit_pdf(&src, &bytes, output)?;
            disclose(&report);
            Ok(pdf_result(verification, &stem, "link"))
        }
        "add_header_footer" => {
            let page = u32_opt(options, "page")?;
            let hf = HeaderFooter {
                text: string_opt(options, "text")?.to_string(),
                font: string_opt(options, "font")?.to_string(),
                font_size: num_opt(options, "font_size")?,
                color: rgb_opt(options, "color")?,
                margin: num_default(options, "margin", 36.0)?,
                align: align_opt(options, "align")?,
            };
            let is_header = bool_opt(options, "header")?;
            let (bytes, report) = with_guard(|budget, g| {
                add_content::add_header_footer(&src, page, &hf, is_header, budget, g)
            })
            .map_err(content_error)?;
            let verification = commit_pdf(&src, &bytes, output)?;
            disclose(&report);
            Ok(pdf_result(verification, &stem, "header-footer"))
        }
        "add_watermark" => {
            let page = u32_opt(options, "page")?;
            let mark = Watermark {
                text: string_opt(options, "text")?.to_string(),
                font: string_opt(options, "font")?.to_string(),
                font_size: num_opt(options, "font_size")?,
                color: rgb_opt(options, "color")?,
                angle_degrees: num_default(options, "angle_degrees", 45.0)?,
                opacity: num_default(options, "opacity", 0.25)?,
            };
            let (bytes, report) =
                with_guard(|budget, g| add_content::add_watermark(&src, page, &mark, budget, g))
                    .map_err(content_error)?;
            let verification = commit_pdf(&src, &bytes, output)?;
            disclose(&report);
            Ok(pdf_result(verification, &stem, "watermark"))
        }
        "add_bates_numbering" => {
            let options_struct = BatesOptions {
                pattern: string_opt(options, "pattern")?.to_string(),
                start: u64_opt_default(options, "start", 1)?,
                font: string_opt(options, "font")?.to_string(),
                font_size: num_default(options, "font_size", 10.0)?,
                color: rgb_opt(options, "color")?,
                margin: num_default(options, "margin", 36.0)?,
                align: align_opt_default(options, "align", Align::Left)?,
            };
            let (bytes, report) = with_guard(|budget, g| {
                add_content::add_bates_numbering(&src, &options_struct, budget, g)
            })
            .map_err(content_error)?;
            let verification = commit_pdf(&src, &bytes, output)?;
            disclose(&report);
            Ok(pdf_result(verification, &stem, "bates"))
        }
        "add_background" => {
            let page = u32_opt(options, "page")?;
            let bg = Background {
                color: rgb_opt(options, "color")?,
                rect: rect_opt(options, "rect")?,
            };
            let (bytes, report) =
                with_guard(|budget, g| add_content::add_background(&src, page, &bg, budget, g))
                    .map_err(content_error)?;
            let verification = commit_pdf(&src, &bytes, output)?;
            disclose(&report);
            Ok(pdf_result(verification, &stem, "background"))
        }
        "list_image_objects" => {
            let page = u32_opt(options, "page")?;
            let objects =
                with_guard(|budget, g| objects::list_image_objects(&src, page, budget, g))
                    .map_err(object_error)?;
            let json = image_objects_json(&objects);
            let verification = commit_read(&src, &json, output)?;
            Ok(ContentResult {
                verification,
                name: format!("{stem}-images.json"),
                kind: ArtifactKind::Report,
            })
        }
        "list_vector_objects" => {
            let page = u32_opt(options, "page")?;
            let objects =
                with_guard(|budget, g| objects::list_vector_objects(&src, page, budget, g))
                    .map_err(object_error)?;
            let json = vector_objects_json(&objects);
            let verification = commit_read(&src, &json, output)?;
            Ok(ContentResult {
                verification,
                name: format!("{stem}-vectors.json"),
                kind: ArtifactKind::Report,
            })
        }
        "move_object" => {
            let page = u32_opt(options, "page")?;
            let id = object_id(options)?;
            let dx = num_opt(options, "dx")?;
            let dy = num_opt(options, "dy")?;
            let (bytes, _report) =
                with_guard(|budget, g| objects::move_object(&src, page, &id, dx, dy, budget, g))
                    .map_err(object_error)?;
            let verification = commit_pdf(&src, &bytes, output)?;
            Ok(pdf_result(verification, &stem, "moved"))
        }
        "resize_object" => {
            let page = u32_opt(options, "page")?;
            let id = object_id(options)?;
            let rect = rect_opt(options, "rect")?;
            let (bytes, _report) =
                with_guard(|budget, g| objects::resize_object(&src, page, &id, rect, budget, g))
                    .map_err(object_error)?;
            let verification = commit_pdf(&src, &bytes, output)?;
            Ok(pdf_result(verification, &stem, "resized"))
        }
        "delete_object" => {
            let page = u32_opt(options, "page")?;
            let id = object_id(options)?;
            let (bytes, _report) =
                with_guard(|budget, g| objects::delete_object(&src, page, &id, budget, g))
                    .map_err(object_error)?;
            let verification = commit_pdf(&src, &bytes, output)?;
            Ok(pdf_result(verification, &stem, "object-deleted"))
        }
        "replace_image" => {
            let page = u32_opt(options, "page")?;
            let id = object_id(options)?;
            let bytes_in = extra_bytes.ok_or_else(|| {
                ToolError("replace_image needs an image as the second input".to_string())
            })?;
            let (bytes, _report) = with_guard(|budget, g| {
                objects::replace_image(&src, page, &id, bytes_in, budget, g)
            })
            .map_err(object_error)?;
            let verification = commit_pdf(&src, &bytes, output)?;
            Ok(pdf_result(verification, &stem, "image-replaced"))
        }
        "extract_image" => {
            let page = u32_opt(options, "page")?;
            let id = object_id(options)?;
            let bytes = with_guard(|budget, g| objects::extract_image(&src, page, &id, budget, g))
                .map_err(object_error)?;
            let verification = commit_read(&src, &bytes, output)?;
            Ok(ContentResult {
                verification,
                name: format!("{stem}-{}.png", id.name),
                kind: ArtifactKind::File,
            })
        }
        other => Err(ToolError(format!("unknown content operation: {other}"))),
    }
}

/// Resolve a command's input/output keys from a stem without relying on the
/// native filesystem: keys are session-table names, never paths on disk.
fn stem_of(input: &str) -> String {
    let base = input.rsplit('/').next().unwrap_or(input);
    let stem = base.rsplit_once('.').map_or(base, |(name, _)| name);
    if stem.is_empty() {
        "document".to_string()
    } else {
        stem.to_string()
    }
}

/// Run a closure with the standard CLI budget guard. The guard borrows a clock
/// that lives for the whole call, so the caller cannot outlive it.
fn with_guard<T, E, F>(f: F) -> Result<T, E>
where
    F: for<'a> FnOnce(&Budget, &'a mut BudgetGuard<'a>) -> Result<T, E>,
{
    let budget = Budget::unlimited();
    let clock = crate::shell_clock();
    let mut g = crate::runtime::cli_guard(&budget, &clock);
    f(&budget, &mut g)
}

fn content_error(e: AddContentError) -> ToolError {
    ToolError(e.to_string())
}

/// Commit an incremental append under the WRITE.05 gate. Content additions and
/// object mutations are appends: pages and furniture survive, while annotations
/// and form fields may legitimately grow (a new link is an annotation).
fn commit_pdf(src: &[u8], bytes: &[u8], output: &str) -> ToolResult<Verification> {
    let budget = Budget::unlimited();
    let clock = crate::shell_clock();
    let mut g = crate::runtime::cli_guard(&budget, &clock);
    let observed = selis_pdf_cos::verify::survey(src, &budget, &mut g)
        .map_err(|e| CliError(format!("input survey failed: {e}")))?;
    let expected = write_gate::expectations_from(&observed, &PRESERVE_ON_APPEND);
    let verdict = write_gate::write_verified(bytes, output, &expected, &budget, &mut g)?;
    let verification = Verification::from_gate(
        Some(&observed),
        &verdict,
        &expected,
        u64::try_from(src.len()).unwrap_or(u64::MAX),
        u64::try_from(bytes.len()).unwrap_or(u64::MAX),
    );
    verification.emit_line();
    Ok(verification)
}

/// Write a read-only artifact (a JSON listing or an extracted image) under the
/// session key, with an honest `read` verification. Nothing is rewritten, so
/// there is no structural gate to run over the source document.
fn commit_read(src: &[u8], bytes: &[u8], output: &str) -> ToolResult<Verification> {
    write_output(output, bytes)?;
    Ok(Verification {
        pages_in: None,
        pages_out: 0,
        annotations_in: None,
        annotations_out: 0,
        fields_in: None,
        fields_out: 0,
        ocgs_in: None,
        ocgs_out: 0,
        outline_entries_in: None,
        outline_entries_out: 0,
        embedded_files_in: None,
        embedded_files_out: 0,
        bytes_in: u64::try_from(src.len()).unwrap_or(u64::MAX),
        bytes_out: u64::try_from(bytes.len()).unwrap_or(u64::MAX),
        verdict: "read".to_string(),
        checked: Vec::new(),
        repair_mode: None,
        fonts_subset: None,
        glyphs_kept: None,
    })
}

fn pdf_result(verification: Verification, stem: &str, label: &str) -> ContentResult {
    ContentResult {
        verification,
        name: format!("{stem}-{label}.pdf"),
        kind: ArtifactKind::Pdf,
    }
}

fn disclose(report: &AddContentReport) {
    for line in &report.font_disclosures {
        eprintln!("font substitution: {line}");
    }
}

fn object_error(e: ObjectError) -> ToolError {
    ToolError(e.to_string())
}

// ── option parsing ─────────────────────────────────────────────────────────

fn field<'a>(options: &'a Value, key: &str) -> ToolResult<&'a Value> {
    options
        .as_object()
        .and_then(|o| o.get(key))
        .ok_or_else(|| ToolError(format!("options need a `{key}` field")))
}

fn string_opt<'a>(options: &'a Value, key: &str) -> ToolResult<&'a str> {
    field(options, key)?
        .as_str()
        .ok_or_else(|| ToolError(format!("`{key}` must be a string")))
}

fn optional_string(options: &Value, key: &str) -> ToolResult<Option<String>> {
    match options.as_object().and_then(|o| o.get(key)) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => value
            .as_str()
            .map(str::to_string)
            .map(Some)
            .ok_or_else(|| ToolError(format!("`{key}` must be a string"))),
    }
}

fn num_opt(options: &Value, key: &str) -> ToolResult<f64> {
    field(options, key)?
        .as_f64()
        .ok_or_else(|| ToolError(format!("`{key}` must be a number")))
}

fn num_default(options: &Value, key: &str, default: f64) -> ToolResult<f64> {
    match options.as_object().and_then(|o| o.get(key)) {
        None | Some(Value::Null) => Ok(default),
        Some(_) => num_opt(options, key),
    }
}

fn u32_opt(options: &Value, key: &str) -> ToolResult<u32> {
    let value = num_opt(options, key)?;
    if !value.is_finite() || value < 0.0 || value.fract() != 0.0 || value > f64::from(u32::MAX) {
        return Err(ToolError(format!("`{key}` must be a u32 integer")));
    }
    // The range check above makes this cast exact and non-negative.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let value = value as u32;
    Ok(value)
}

fn u64_opt_default(options: &Value, key: &str, default: u64) -> ToolResult<u64> {
    match options.as_object().and_then(|o| o.get(key)) {
        None | Some(Value::Null) => Ok(default),
        Some(value) => {
            let n = value
                .as_u64()
                .ok_or_else(|| ToolError(format!("`{key}` must be a non-negative integer")))?;
            Ok(n)
        }
    }
}

fn bool_opt(options: &Value, key: &str) -> ToolResult<bool> {
    field(options, key)?
        .as_bool()
        .ok_or_else(|| ToolError(format!("`{key}` must be a boolean")))
}

fn bool_default(options: &Value, key: &str, default: bool) -> ToolResult<bool> {
    match options.as_object().and_then(|o| o.get(key)) {
        None | Some(Value::Null) => Ok(default),
        Some(_) => bool_opt(options, key),
    }
}

/// Read `[x0, y0, x1, y1]` from an option field. Rect corners are normalised
/// the same way the engine normalises PDF rectangle arrays.
fn rect_opt(options: &Value, key: &str) -> ToolResult<Rect> {
    let array = field(options, key)?
        .as_array()
        .ok_or_else(|| ToolError(format!("`{key}` must be a [x0, y0, x1, y1] array")))?;
    if array.len() != 4 {
        return Err(ToolError(format!("`{key}` must have four numbers")));
    }
    let mut corners = [0.0_f64; 4];
    for (slot, value) in corners.iter_mut().zip(array.iter()) {
        let n = value
            .as_f64()
            .ok_or_else(|| ToolError(format!("`{key}` must hold only finite numbers")))?;
        if !n.is_finite() {
            return Err(ToolError(format!("`{key}` must hold only finite numbers")));
        }
        *slot = n;
    }
    Ok(Rect::from_pdf_array(corners))
}

fn rgb_opt(options: &Value, key: &str) -> ToolResult<Rgb> {
    let array = field(options, key)?
        .as_array()
        .ok_or_else(|| ToolError(format!("`{key}` must be a [r, g, b] array")))?;
    if array.len() != 3 {
        return Err(ToolError(format!("`{key}` must have three components")));
    }
    rgb_from(array, key)
}

fn optional_rgb(options: &Value, key: &str) -> ToolResult<Option<Rgb>> {
    match options.as_object().and_then(|o| o.get(key)) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Array(array)) => rgb_from(array, key).map(Some),
        Some(_) => Err(ToolError(format!("`{key}` must be a [r, g, b] array"))),
    }
}

fn rgb_from(array: &[Value], key: &str) -> ToolResult<Rgb> {
    if array.len() != 3 {
        return Err(ToolError(format!("`{key}` must have three components")));
    }
    let mut components = [0_u8; 3];
    for (slot, value) in components.iter_mut().zip(array.iter()) {
        let n = value
            .as_u64()
            .ok_or_else(|| ToolError(format!("`{key}` components must be 0..255 integers")))?;
        *slot = u8::try_from(n)
            .map_err(|_| ToolError(format!("`{key}` components must be 0..255 integers")))?;
    }
    Ok(Rgb::new(components[0], components[1], components[2]))
}

fn align_opt(options: &Value, key: &str) -> ToolResult<Align> {
    match string_opt(options, key)? {
        "center" | "centre" => Ok(Align::Center),
        "right" => Ok(Align::Right),
        "left" => Ok(Align::Left),
        other => Err(ToolError(format!("unknown alignment: {other}"))),
    }
}

fn align_opt_default(options: &Value, key: &str, default: Align) -> ToolResult<Align> {
    match options.as_object().and_then(|o| o.get(key)) {
        None | Some(Value::Null) => Ok(default),
        Some(_) => align_opt(options, key),
    }
}

fn object_id(options: &Value) -> ToolResult<ObjectId> {
    let id = field(options, "id")?;
    let name = id
        .as_object()
        .and_then(|o| o.get("name"))
        .and_then(Value::as_str)
        .ok_or_else(|| ToolError("`id.name` must be a string".to_string()))?;
    let op_index = id
        .as_object()
        .and_then(|o| o.get("op_index"))
        .and_then(Value::as_u64)
        .ok_or_else(|| ToolError("`id.op_index` must be a non-negative integer".to_string()))?;
    let op_index = u32::try_from(op_index)
        .map_err(|_| ToolError("`id.op_index` is out of range".to_string()))?;
    Ok(ObjectId::new(name, op_index))
}

// ── read-only report encoding ──────────────────────────────────────────────

fn rect_json(rect: Rect) -> Value {
    serde_json::json!({
        "x0": rect.x0,
        "y0": rect.y0,
        "x1": rect.x1,
        "y1": rect.y1,
    })
}

fn id_json(id: &ObjectId) -> Value {
    serde_json::json!({ "name": id.name, "op_index": id.op_index })
}

fn image_objects_json(objects: &[objects::ImageObject]) -> Vec<u8> {
    let rows: Vec<Value> = objects
        .iter()
        .map(|o| {
            serde_json::json!({
                "id": id_json(&o.id),
                "name": o.name,
                "object_number": o.object_number,
                "rect": rect_json(o.rect),
                "pixel_width": o.pixel_width,
                "pixel_height": o.pixel_height,
                "filter": o.filter,
            })
        })
        .collect();
    serde_json::to_vec(&serde_json::json!({ "objects": rows })).unwrap_or_default()
}

fn vector_objects_json(objects: &[objects::VectorObject]) -> Vec<u8> {
    let rows: Vec<Value> = objects
        .iter()
        .map(|o| {
            serde_json::json!({
                "id": id_json(&o.id),
                "paint_op": o.paint_op,
                "rect": rect_json(o.rect),
            })
        })
        .collect();
    serde_json::to_vec(&serde_json::json!({ "objects": rows })).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_user_space_rect() {
        let options = serde_json::json!({ "rect": [20.0, 30.0, 120.0, 80.0] });
        let rect = rect_opt(&options, "rect").expect("rect");
        assert_eq!(rect, Rect::new(20.0, 30.0, 120.0, 80.0));
    }

    #[test]
    fn normalises_an_unordered_rect() {
        let options = serde_json::json!({ "rect": [120.0, 80.0, 20.0, 30.0] });
        assert_eq!(
            rect_opt(&options, "rect").expect("rect"),
            Rect::new(20.0, 30.0, 120.0, 80.0)
        );
    }

    #[test]
    fn parses_an_object_id() {
        let options = serde_json::json!({ "id": { "name": "Im1", "op_index": 3 } });
        let id = object_id(&options).expect("id");
        assert_eq!(id, ObjectId::new("Im1", 3));
    }

    #[test]
    fn rejects_a_non_finite_rect() {
        let value = serde_json::Value::String("nan".to_string());
        let options = serde_json::json!({ "rect": [0.0, value, 1.0, 1.0] });
        assert!(rect_opt(&options, "rect").is_err());
    }

    #[test]
    fn stem_is_backend_key_not_a_path() {
        assert_eq!(stem_of("in/0"), "0");
        assert_eq!(stem_of("in/scan.pdf"), "scan");
    }
}
