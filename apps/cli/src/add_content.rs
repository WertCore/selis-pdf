//! First-class add-content and object-edit CLI commands
//! (SL-5.TEXTEDIT.05/06/07/08).
//!
//! These commands call the same `selis-pdf-tools::content_edit` bridge the
//! browser tools chunk uses, so the CLI and the web editor cannot disagree
//! about what `add_text_box` or `move_object` does. The engine crates stay
//! filesystem-free; only this CLI layer reads the image input file and names
//! the output path.
//!
//! Every mutation is an incremental append: the input bytes remain a verbatim
//! prefix and the WRITE.05 gate verifies the result before it is committed.

use std::path::Path;

use clap::Subcommand;
use serde_json::{json, Value};

use crate::{CliError, CliResult};

/// The first-class content commands, flattened onto the top-level `selis`
/// command tree (so they are `selis add-text`, `selis list-objects`, ...).
#[derive(Debug, Subcommand)]
pub(crate) enum Command {
    /// Add a proper new text box to a page.
    AddText {
        /// The input PDF.
        path: String,
        /// 0-based page index.
        #[arg(long, default_value_t = 0)]
        page: u32,
        /// The box as `x0,y0,x1,y1` in PDF user space.
        #[arg(long, value_name = "X0,Y0,X1,Y1")]
        rect: String,
        /// Printable-ASCII text to draw.
        #[arg(long)]
        text: String,
        /// Requested standard-14 base font.
        #[arg(long, default_value = "Helvetica")]
        font: String,
        /// Font size in PDF points.
        #[arg(long, default_value_t = 12.0)]
        font_size: f64,
        /// Fill colour as `r,g,b` with 0..255 components.
        #[arg(long, value_name = "R,G,B", default_value = "0,0,0")]
        color: String,
        /// The output PDF.
        #[arg(short, long)]
        output: String,
        /// Emit the verification object as JSON on stdout.
        #[arg(long)]
        json: bool,
    },
    /// Place a caller-supplied PNG or JPEG on a page.
    AddImage {
        /// The input PDF.
        path: String,
        /// 0-based page index.
        #[arg(long, default_value_t = 0)]
        page: u32,
        /// The placement rectangle as `x0,y0,x1,y1` in PDF user space.
        #[arg(long, value_name = "X0,Y0,X1,Y1")]
        rect: String,
        /// The image file to place.
        #[arg(long, value_name = "FILE")]
        image: String,
        /// Counter-clockwise rotation in degrees.
        #[arg(long, default_value_t = 0.0)]
        angle: f64,
        /// The output PDF.
        #[arg(short, long)]
        output: String,
        /// Emit the verification object as JSON on stdout.
        #[arg(long)]
        json: bool,
    },
    /// Add a rect, inscribed ellipse or line.
    AddShape {
        /// The input PDF.
        path: String,
        /// 0-based page index.
        #[arg(long, default_value_t = 0)]
        page: u32,
        /// The bounding rectangle as `x0,y0,x1,y1` in PDF user space.
        #[arg(long, value_name = "X0,Y0,X1,Y1")]
        rect: String,
        /// The path kind: rect, ellipse or line.
        #[arg(long, default_value = "rect")]
        kind: String,
        /// Fill colour as `r,g,b`; omit for no fill.
        #[arg(long, value_name = "R,G,B")]
        fill: Option<String>,
        /// Stroke colour as `r,g,b`; omit for no stroke.
        #[arg(long, value_name = "R,G,B")]
        stroke: Option<String>,
        /// Stroke line width.
        #[arg(long, default_value_t = 1.0)]
        stroke_width: f64,
        /// The output PDF.
        #[arg(short, long)]
        output: String,
        /// Emit the verification object as JSON on stdout.
        #[arg(long)]
        json: bool,
    },
    /// Add a clickable link annotation.
    AddLink {
        /// The input PDF.
        path: String,
        /// 0-based page index.
        #[arg(long, default_value_t = 0)]
        page: u32,
        /// The clickable rectangle as `x0,y0,x1,y1` in PDF user space.
        #[arg(long, value_name = "X0,Y0,X1,Y1")]
        rect: String,
        /// An external URI target (mutually exclusive with --target-page).
        #[arg(long)]
        uri: Option<String>,
        /// A 0-based internal page target.
        #[arg(long)]
        target_page: Option<u32>,
        /// Border colour as `r,g,b`.
        #[arg(long, value_name = "R,G,B", default_value = "0,0,204")]
        color: String,
        /// Draw a visible border in the generated appearance.
        #[arg(long, default_value_t = true)]
        border: bool,
        /// The output PDF.
        #[arg(short, long)]
        output: String,
        /// Emit the verification object as JSON on stdout.
        #[arg(long)]
        json: bool,
    },
    /// Add a header or a footer, positioned from the media box.
    AddHeaderFooter {
        /// The input PDF.
        path: String,
        /// 0-based page index.
        #[arg(long, default_value_t = 0)]
        page: u32,
        /// The text to draw.
        #[arg(long)]
        text: String,
        /// Draw a header rather than a footer.
        #[arg(long)]
        header: bool,
        /// Horizontal alignment: left, center or right.
        #[arg(long, default_value = "left")]
        align: String,
        /// Font size in PDF points.
        #[arg(long, default_value_t = 10.0)]
        font_size: f64,
        /// Distance from the page edge, in points.
        #[arg(long, default_value_t = 36.0)]
        margin: f64,
        /// The output PDF.
        #[arg(short, long)]
        output: String,
        /// Emit the verification object as JSON on stdout.
        #[arg(long)]
        json: bool,
    },
    /// Stamp a translucent watermark on a page.
    AddWatermark {
        /// The input PDF.
        path: String,
        /// 0-based page index.
        #[arg(long, default_value_t = 0)]
        page: u32,
        /// The watermark text.
        #[arg(long)]
        text: String,
        /// Font size in PDF points.
        #[arg(long, default_value_t = 48.0)]
        font_size: f64,
        /// Fill alpha in 0..1.
        #[arg(long, default_value_t = 0.25)]
        opacity: f64,
        /// Counter-clockwise rotation in degrees.
        #[arg(long, default_value_t = 45.0)]
        angle: f64,
        /// The output PDF.
        #[arg(short, long)]
        output: String,
        /// Emit the verification object as JSON on stdout.
        #[arg(long)]
        json: bool,
    },
    /// Stamp sequential Bates ids across every page.
    AddBates {
        /// The input PDF.
        path: String,
        /// An id pattern whose last digit run is the counter (e.g. CASE-000001).
        #[arg(long)]
        pattern: String,
        /// The value on the first page.
        #[arg(long, default_value_t = 1)]
        start: u64,
        /// Horizontal alignment: left, center or right.
        #[arg(long, default_value = "left")]
        align: String,
        /// Font size in PDF points.
        #[arg(long, default_value_t = 10.0)]
        font_size: f64,
        /// Distance from the bottom edge, in points.
        #[arg(long, default_value_t = 36.0)]
        margin: f64,
        /// The output PDF.
        #[arg(short, long)]
        output: String,
        /// Emit the verification object as JSON on stdout.
        #[arg(long)]
        json: bool,
    },
    /// Paint a solid background beneath a page's existing content.
    AddBackground {
        /// The input PDF.
        path: String,
        /// 0-based page index.
        #[arg(long, default_value_t = 0)]
        page: u32,
        /// The rectangle as `x0,y0,x1,y1` in PDF user space.
        #[arg(long, value_name = "X0,Y0,X1,Y1")]
        rect: String,
        /// Fill colour as `r,g,b`.
        #[arg(long, value_name = "R,G,B", default_value = "245,245,240")]
        color: String,
        /// The output PDF.
        #[arg(short, long)]
        output: String,
        /// Emit the verification object as JSON on stdout.
        #[arg(long)]
        json: bool,
    },
    /// List the image objects on a page as JSON.
    ListObjects {
        /// The input PDF.
        path: String,
        /// 0-based page index.
        #[arg(long, default_value_t = 0)]
        page: u32,
        /// Where to write the JSON report (printed on stdout when omitted).
        #[arg(short, long)]
        output: Option<String>,
    },
    /// List the painted vector paths on a page as JSON.
    ListVectors {
        /// The input PDF.
        path: String,
        /// 0-based page index.
        #[arg(long, default_value_t = 0)]
        page: u32,
        /// Where to write the JSON report (printed on stdout when omitted).
        #[arg(short, long)]
        output: Option<String>,
    },
    /// Move a drawn object by a user-space delta.
    MoveObject {
        /// The input PDF.
        path: String,
        /// 0-based page index.
        #[arg(long, default_value_t = 0)]
        page: u32,
        /// The XObject resource name (empty for a vector path).
        #[arg(long, default_value = "")]
        name: String,
        /// The content-stream operator index.
        #[arg(long)]
        op_index: u32,
        /// Horizontal translation in points.
        #[arg(long, default_value_t = 0.0)]
        dx: f64,
        /// Vertical translation in points.
        #[arg(long, default_value_t = 0.0)]
        dy: f64,
        /// The output PDF.
        #[arg(short, long)]
        output: String,
        /// Emit the verification object as JSON on stdout.
        #[arg(long)]
        json: bool,
    },
    /// Resize a drawn object to a user-space rectangle.
    ResizeObject {
        /// The input PDF.
        path: String,
        /// 0-based page index.
        #[arg(long, default_value_t = 0)]
        page: u32,
        /// The XObject resource name (empty for a vector path).
        #[arg(long, default_value = "")]
        name: String,
        /// The content-stream operator index.
        #[arg(long)]
        op_index: u32,
        /// The target rectangle as `x0,y0,x1,y1` in PDF user space.
        #[arg(long, value_name = "X0,Y0,X1,Y1")]
        rect: String,
        /// The output PDF.
        #[arg(short, long)]
        output: String,
        /// Emit the verification object as JSON on stdout.
        #[arg(long)]
        json: bool,
    },
    /// Delete a drawn object.
    DeleteObject {
        /// The input PDF.
        path: String,
        /// 0-based page index.
        #[arg(long, default_value_t = 0)]
        page: u32,
        /// The XObject resource name (empty for a vector path).
        #[arg(long, default_value = "")]
        name: String,
        /// The content-stream operator index.
        #[arg(long)]
        op_index: u32,
        /// The output PDF.
        #[arg(short, long)]
        output: String,
        /// Emit the verification object as JSON on stdout.
        #[arg(long)]
        json: bool,
    },
    /// Replace an image's pixels with a PNG or JPEG.
    ReplaceImage {
        /// The input PDF.
        path: String,
        /// 0-based page index.
        #[arg(long, default_value_t = 0)]
        page: u32,
        /// The XObject resource name.
        #[arg(long)]
        name: String,
        /// The content-stream operator index.
        #[arg(long)]
        op_index: u32,
        /// The replacement image file.
        #[arg(long, value_name = "FILE")]
        image: String,
        /// The output PDF.
        #[arg(short, long)]
        output: String,
        /// Emit the verification object as JSON on stdout.
        #[arg(long)]
        json: bool,
    },
    /// Extract an image object as a standalone PNG/JPEG.
    ExtractImage {
        /// The input PDF.
        path: String,
        /// 0-based page index.
        #[arg(long, default_value_t = 0)]
        page: u32,
        /// The XObject resource name.
        #[arg(long)]
        name: String,
        /// The content-stream operator index.
        #[arg(long)]
        op_index: u32,
        /// Where to write the extracted image.
        #[arg(short, long)]
        output: String,
        /// Emit the verification object as JSON on stdout.
        #[arg(long)]
        json: bool,
    },
}

/// Run one of the first-class content commands.
pub(crate) fn run(command: Command) -> CliResult<()> {
    match command {
        Command::AddText {
            path,
            page,
            rect,
            text,
            font,
            font_size,
            color,
            output,
            json,
        } => run_op(
            "add_text_box",
            json!({
                "page": page,
                "rect": parse_rect(&rect)?,
                "text": text,
                "font": font,
                "font_size": font_size,
                "color": parse_color(&color)?,
            }),
            &path,
            &output,
            None,
            json,
        ),
        Command::AddImage {
            path,
            page,
            rect,
            image,
            angle,
            output,
            json,
        } => run_op(
            "add_image",
            json!({
                "page": page,
                "rect": parse_rect(&rect)?,
                "angle_degrees": angle,
            }),
            &path,
            &output,
            Some(read_image(&image)?),
            json,
        ),
        Command::AddShape {
            path,
            page,
            rect,
            kind,
            fill,
            stroke,
            stroke_width,
            output,
            json,
        } => run_op(
            "add_shape",
            json!({
                "page": page,
                "rect": parse_rect(&rect)?,
                "kind": kind,
                "fill": optional_color(fill)?,
                "stroke": optional_color(stroke)?,
                "stroke_width": stroke_width,
            }),
            &path,
            &output,
            None,
            json,
        ),
        Command::AddLink {
            path,
            page,
            rect,
            uri,
            target_page,
            color,
            border,
            output,
            json,
        } => {
            let mut options = json!({
                "page": page,
                "rect": parse_rect(&rect)?,
                "color": parse_color(&color)?,
                "border": border,
            });
            let object = options
                .as_object_mut()
                .ok_or_else(|| CliError("link options are not an object".to_string()))?;
            if let Some(uri) = uri {
                object.insert("uri".to_string(), Value::String(uri));
            } else if let Some(page) = target_page {
                object.insert("target_page".to_string(), json!(page));
            } else {
                return Err(CliError(
                    "add-link needs --uri or --target-page".to_string(),
                ));
            }
            run_op("add_link", options, &path, &output, None, json)
        }
        Command::AddHeaderFooter {
            path,
            page,
            text,
            header,
            align,
            font_size,
            margin,
            output,
            json,
        } => run_op(
            "add_header_footer",
            json!({
                "page": page,
                "text": text,
                "font": "Helvetica",
                "font_size": font_size,
                "margin": margin,
                "align": align,
                "header": header,
                "color": [0, 0, 0],
            }),
            &path,
            &output,
            None,
            json,
        ),
        Command::AddWatermark {
            path,
            page,
            text,
            font_size,
            opacity,
            angle,
            output,
            json,
        } => run_op(
            "add_watermark",
            json!({
                "page": page,
                "text": text,
                "font": "Helvetica",
                "font_size": font_size,
                "opacity": opacity,
                "angle_degrees": angle,
                "color": [128, 128, 128],
            }),
            &path,
            &output,
            None,
            json,
        ),
        Command::AddBates {
            path,
            pattern,
            start,
            align,
            font_size,
            margin,
            output,
            json,
        } => run_op(
            "add_bates_numbering",
            json!({
                "pattern": pattern,
                "start": start,
                "font": "Helvetica",
                "font_size": font_size,
                "margin": margin,
                "align": align,
                "color": [0, 0, 0],
            }),
            &path,
            &output,
            None,
            json,
        ),
        Command::AddBackground {
            path,
            page,
            rect,
            color,
            output,
            json,
        } => run_op(
            "add_background",
            json!({
                "page": page,
                "rect": parse_rect(&rect)?,
                "color": parse_color(&color)?,
            }),
            &path,
            &output,
            None,
            json,
        ),
        Command::ListObjects { path, page, output } => read_op(
            "list_image_objects",
            json!({ "page": page }),
            &path,
            &output,
        ),
        Command::ListVectors { path, page, output } => read_op(
            "list_vector_objects",
            json!({ "page": page }),
            &path,
            &output,
        ),
        Command::MoveObject {
            path,
            page,
            name,
            op_index,
            dx,
            dy,
            output,
            json,
        } => run_op(
            "move_object",
            object_options(page, &name, op_index)
                .with("dx", json!(dx))
                .with("dy", json!(dy)),
            &path,
            &output,
            None,
            json,
        ),
        Command::ResizeObject {
            path,
            page,
            name,
            op_index,
            rect,
            output,
            json,
        } => run_op(
            "resize_object",
            object_options(page, &name, op_index).with("rect", parse_rect(&rect)?),
            &path,
            &output,
            None,
            json,
        ),
        Command::DeleteObject {
            path,
            page,
            name,
            op_index,
            output,
            json,
        } => run_op(
            "delete_object",
            object_options(page, &name, op_index),
            &path,
            &output,
            None,
            json,
        ),
        Command::ReplaceImage {
            path,
            page,
            name,
            op_index,
            image,
            output,
            json,
        } => run_op(
            "replace_image",
            object_options(page, &name, op_index),
            &path,
            &output,
            Some(read_image(&image)?),
            json,
        ),
        Command::ExtractImage {
            path,
            page,
            name,
            op_index,
            output,
            json,
        } => {
            let result = selis_pdf_tools::content_edit::run(
                "extract_image",
                &object_options(page, &name, op_index),
                &path,
                &output,
                None,
            )
            .map_err(CliError::from)?;
            print_verification(&result.verification, json);
            Ok(())
        }
    }
}

/// Options for an object mutator: page plus a stable id.
fn object_options(page: u32, name: &str, op_index: u32) -> Value {
    json!({ "page": page, "id": { "name": name, "op_index": op_index } })
}

/// A tiny builder so object options can gain one extra field per command.
trait WithField: Sized {
    fn with(self, key: &str, value: Value) -> Self;
}

impl WithField for Value {
    fn with(mut self, key: &str, value: Value) -> Self {
        if let Some(object) = self.as_object_mut() {
            object.insert(key.to_string(), value);
        }
        self
    }
}

fn run_op(
    op: &str,
    options: Value,
    path: &str,
    output: &str,
    extra: Option<Vec<u8>>,
    json: bool,
) -> CliResult<()> {
    let result = selis_pdf_tools::content_edit::run(op, &options, path, output, extra.as_deref())
        .map_err(CliError::from)?;
    print_verification(&result.verification, json);
    Ok(())
}

/// Read-only ops write a report under `output` (or print it when omitted).
fn read_op(op: &str, options: Value, path: &str, output: &Option<String>) -> CliResult<()> {
    let key = output.as_deref().unwrap_or("-");
    let result = selis_pdf_tools::content_edit::run(op, &options, path, key, None)
        .map_err(CliError::from)?;
    print_verification(&result.verification, false);
    if output.is_none() {
        let bytes = selis_pdf_tools::take_output(key)
            .ok_or_else(|| CliError("the tool produced no report".to_string()))?;
        println!("{}", String::from_utf8_lossy(&bytes));
    }
    Ok(())
}

fn print_verification(verification: &selis_pdf_tools::Verification, json: bool) {
    if json {
        if let Ok(value) = serde_json::to_value(verification) {
            println!("{}", value);
        }
    } else {
        verification.emit_line();
    }
}

fn read_image(path: &str) -> CliResult<Vec<u8>> {
    std::fs::read(Path::new(path)).map_err(|e| CliError(format!("{path}: cannot read image: {e}")))
}

// ── typed option parsing (pure, so it is unit-testable) ────────────────────

/// Parse `x0,y0,x1,y1` into the engine's rectangle array.
fn parse_rect(text: &str) -> CliResult<Value> {
    let parts: Vec<&str> = text.split(',').map(str::trim).collect();
    if parts.len() != 4 {
        return Err(CliError(format!(
            "a rectangle is four comma-separated numbers, got `{text}`"
        )));
    }
    let mut values = Vec::with_capacity(4);
    for part in parts {
        let number: f64 = part
            .parse()
            .map_err(|_| CliError(format!("`{part}` is not a number")))?;
        if !number.is_finite() {
            return Err(CliError("rectangle numbers must be finite".to_string()));
        }
        values.push(json!(number));
    }
    Ok(Value::Array(values))
}

/// Parse `r,g,b` with 0..255 components.
fn parse_color(text: &str) -> CliResult<Value> {
    let parts: Vec<&str> = text.split(',').map(str::trim).collect();
    if parts.len() != 3 {
        return Err(CliError(format!(
            "a colour is three comma-separated integers, got `{text}`"
        )));
    }
    let mut values = Vec::with_capacity(3);
    for part in parts {
        let number: u8 = part
            .parse()
            .map_err(|_| CliError(format!("`{part}` is not a 0..255 integer")))?;
        values.push(json!(number));
    }
    Ok(Value::Array(values))
}

fn optional_color(text: Option<String>) -> CliResult<Value> {
    match text {
        Some(text) => parse_color(&text),
        None => Ok(Value::Null),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_rectangle() {
        assert_eq!(
            parse_rect("1, 2, 30, 40").expect("rect"),
            json!([1.0, 2.0, 30.0, 40.0])
        );
    }

    #[test]
    fn rejects_a_short_rectangle() {
        assert!(parse_rect("1,2,3").is_err());
    }

    #[test]
    fn parses_a_colour() {
        assert_eq!(
            parse_color("10,20,30").expect("colour"),
            json!([10, 20, 30])
        );
    }

    #[test]
    fn rejects_an_out_of_range_colour() {
        assert!(parse_color("256,0,0").is_err());
    }

    #[test]
    fn builds_an_object_id() {
        assert_eq!(
            object_options(2, "Im1", 3),
            json!({ "page": 2, "id": { "name": "Im1", "op_index": 3 } })
        );
    }

    #[test]
    fn clap_exposes_the_commands_at_the_top_level() {
        use clap::Parser;

        #[derive(Parser)]
        struct Cli {
            #[command(subcommand)]
            command: Command,
        }

        let cli = Cli::try_parse_from([
            "selis",
            "add-text",
            "doc.pdf",
            "--text",
            "Hello",
            "--rect",
            "0,0,80,20",
            "-o",
            "out.pdf",
        ])
        .expect("add-text parses");
        match cli.command {
            Command::AddText { text, output, .. } => {
                assert_eq!(text, "Hello");
                assert_eq!(output, "out.pdf");
            }
            _ => panic!("add-text parsed as the wrong command"),
        }

        let cli = Cli::try_parse_from(["selis", "list-objects", "doc.pdf", "--page", "2"])
            .expect("list-objects parses");
        match cli.command {
            Command::ListObjects { page, .. } => assert_eq!(page, 2),
            _ => panic!("list-objects parsed as the wrong command"),
        }
    }
}
