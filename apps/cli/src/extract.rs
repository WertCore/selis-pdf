//! The `selis extract` command: recover a page's text or images.
//!
//! Executes the page's content stream into a display list, groups the
//! positioned glyphs into lines, and emits them as plain text, JSON,
//! Markdown, or HTML.  With `--format=image`, writes each image XObject
//! used on the page to a PPM file and prints a JSON manifest.

use selis_pdf_content::display_list::Op;
use selis_pdf_content::text::TextGlyph;
use selis_pdf_engine::Session;
use selis_pdf_text::TextLine;
use selis_sandbox::{Budget, CancelToken, FixedClock, Surface};

use crate::{read_file, CliError, CliResult};
use crate::render::write_ppm;

/// Extract a page's text or images.
///
/// # Errors
///
/// `IO_READ_FAILED` when the PDF cannot be read.
pub(crate) fn run(path: &str, page: usize, format: &str, output: Option<&str>) -> CliResult<()> {
    let src = read_file(path)?;
    let budget = Budget::profile(Surface::Viewer);
    let session =
        Session::open(src, &budget).map_err(|e| CliError(format!("cannot open PDF: {e}")))?;

    // Document-level formats don't need a page.
    if format == "embedded" {
        let mut g = budget.guard_with(&FixedClock(0), CancelToken::new());
        return extract_embedded(&session, &budget, &mut g, output);
    }

    if page >= session.len() {
        return Err(CliError(format!(
            "page {page} out of range (document has {} pages)",
            session.len()
        )));
    }
    let mut g = budget.guard_with(&FixedClock(0), CancelToken::new());
    let dl = session
        .page_display_list(page, &budget, &mut g)
        .map_err(|e| CliError(format!("cannot interpret page: {e}")))?;

    if format == "image" {
        return extract_images(&dl, page);
    }

    let (lines, line_texts) = page_lines(&dl);
    let out = match format {
        "json" => {
            // Per-run text (parallel to each line's flattened runs) so the
            // JSON spans carry only their own glyphs, not the whole line.
            let run_texts: Vec<Vec<String>> = lines
                .iter()
                .map(|line| {
                    line.words
                        .iter()
                        .flat_map(|w| w.runs.iter())
                        .map(|run| {
                            run.glyphs
                                .iter()
                                .filter_map(|g| char::from_u32(u32::from(g.code)))
                                .collect()
                        })
                        .collect()
                })
                .collect();
            let s = selis_pdf_text::structured(&lines, &line_texts, &run_texts);
            selis_pdf_text::to_json(&s)
        }
        "md" => selis_pdf_text::to_markdown(&lines, &line_texts),
        "html" => selis_pdf_text::to_html(&lines, &line_texts),
        _ => selis_pdf_text::to_text(&lines, &line_texts),
    };
    print!("{out}");
    Ok(())
}

/// The assembled text lines and their recovered Unicode texts for a display
/// list, in reading order (column-aware geometry).
pub(crate) fn page_lines(
    dl: &selis_pdf_content::display_list::DisplayList,
) -> (Vec<selis_pdf_text::TextLine>, Vec<String>) {
    let mut glyphs = Vec::new();
    for op in &dl.ops {
        if let Op::Text { at, runs, .. } = op {
            for run in runs {
                for &code in &run.glyphs {
                    glyphs.push(TextGlyph {
                        code,
                        at: *at,
                        font: run.font.clone(),
                        size: run.size,
                    });
                }
            }
        }
    }
    let lines = selis_pdf_text::assemble(glyphs);
    // Reading order: column detection + XY-cut (no structure-tree MCIDs).
    let mcid_lines: Vec<selis_pdf_text::LineWithMcid> = lines
        .into_iter()
        .map(|line| selis_pdf_text::LineWithMcid { line, mcid: None })
        .collect();
    let ordered = selis_pdf_text::order_lines(mcid_lines, None);
    let line_texts: Vec<String> = ordered.lines.iter().map(line_text).collect();
    (ordered.lines, line_texts)
}

/// List the document's embedded files (metadata only, newline-delimited JSON),
/// or extract them to `output` when given.
fn extract_embedded(
    session: &Session,
    budget: &Budget,
    g: &mut selis_sandbox::BudgetGuard<'_>,
    output: Option<&str>,
) -> CliResult<()> {
    let attachments = session
        .attachments(budget, g)
        .map_err(|e| CliError(format!("cannot read embedded files: {e}")))?;
    if let Some(dir) = output {
        std::fs::create_dir_all(dir)
            .map_err(|e| CliError(format!("cannot create {dir}: {e}")))?;
        let mut written = 0usize;
        for a in &attachments {
            let raw = a.name.as_ref().map(|b| String::from_utf8_lossy(b.as_slice()).to_string());
            let base = sanitise_filename(raw.as_deref().unwrap_or(&a.key));
            let file = format!("{dir}/{base}");
            let data = session
                .embedded_file_data(&a.key, budget, g)
                .map_err(|e| CliError(format!("cannot read embedded file {}: {e}", a.key)))?;
            match data {
                Some(bytes) => {
                    std::fs::write(&file, &bytes)
                        .map_err(|e| CliError(format!("cannot write {file}: {e}")))?;
                    eprintln!("wrote {} ({} bytes)", file, bytes.len());
                    written = written.saturating_add(1);
                }
                None => eprintln!("skipped {}: could not decode", a.key),
            }
        }
        eprintln!("extracted {written} embedded file(s) to {dir}");
        return Ok(());
    }
    let mut out = String::new();
    for a in &attachments {
        if !out.is_empty() {
            out.push('\n');
        }
        let name = a.name.as_ref().map(|b| String::from_utf8_lossy(b.as_slice()).to_string());
        let size = a.size.unwrap_or(-1);
        out.push_str(&format!(
            r#"{{"name":{},"size":{},"key":{}}}"#,
            json_str(name.as_deref().unwrap_or("")),
            size,
            json_str(&a.key),
        ));
    }
    println!("{out}");
    Ok(())
}

/// Make a filename safe to write: strip path separators and traversal
/// components, collapse runs of unsafe characters.
fn sanitise_filename(name: &str) -> String {
    let mut out = String::new();
    for ch in name.chars() {
        let ok = matches!(ch, 'a'..='z' | 'A'..='Z' | '0'..='9' | '-' | '_' | '.' | ' ');
        if ok {
            out.push(ch);
        } else {
            out.push('_');
        }
    }
    let trimmed = out.trim().trim_start_matches('.').trim();
    if trimmed.is_empty() {
        "file".to_string()
    } else {
        trimmed.to_string()
    }
}

/// A JSON string literal (quoted, escaped).
fn json_str(s: &str) -> String {
    let escaped = s
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
        .replace('\r', "\\r");
    format!("\"{escaped}\"")
}

/// Extract every image XObject used on the page as a PPM file, printing a
/// newline-delimited JSON manifest of `{"index", "width", "height", "file"}`.
fn extract_images(dl: &selis_pdf_content::display_list::DisplayList, page: usize) -> CliResult<()> {    let images = image_manifest(dl, page);
    let mut manifest = String::new();
    for (index, entry) in images.iter().enumerate() {
        write_ppm(&entry.file, &entry.rgba8, entry.width, entry.height)?;
        if !manifest.is_empty() {
            manifest.push('\n');
        }
        manifest.push_str(&entry.json(index));
    }
    println!("{manifest}");
    eprintln!("extracted {} image(s) from page {page}", images.len());
    Ok(())
}

/// One extracted image.
struct ExtractedImage {
    /// The output filename.
    file: String,
    /// The RGBA8 samples.
    rgba8: Vec<u8>,
    /// The image width in pixels.
    width: u32,
    /// The image height in pixels.
    height: u32,
}

impl ExtractedImage {
    /// The manifest line: `{"index":N,"width":W,"height":H,"file":"F"}`.
    fn json(&self, index: usize) -> String {
        format!(
            r#"{{"index":{index},"width":{},"height":{},"file":"{}"}}"#,
            self.width, self.height, self.file
        )
    }
}

/// Collect the image XObjects used on a page, in content order.
fn image_manifest(dl: &selis_pdf_content::display_list::DisplayList, page: usize) -> Vec<ExtractedImage> {
    let mut out = Vec::new();
    for (index, op) in dl.ops.iter().enumerate() {
        if let Op::Image {
            rgba8,
            width,
            height,
            ..
        } = op
        {
            out.push(ExtractedImage {
                file: format!("page-{page}-{index}.ppm"),
                rgba8: rgba8.as_slice().to_vec(),
                width: *width,
                height: *height,
            });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use selis_pdf_content::display_list::{DisplayList, Op, ResolvedState};
    use selis_pdf_content::gstate::GState;
    use selis_pdf_content::path::Path;

    use super::*;

    fn image_op(rgba8: &[u8], width: u32, height: u32) -> Op {
        Op::Image {
            rgba8: selis_bytes::Bytes::copy_from_slice(rgba8),
            width,
            height,
            rect: selis_geom::Rect::new(0.0, 0.0, 1.0, 1.0),
            state: ResolvedState::from(&GState::new()),
        }
    }

    #[test]
    fn image_manifest_lists_each_image_in_order() {
        let dl = DisplayList {
            ops: vec![
                image_op(&[255, 0, 0, 255], 1, 1),
                image_op(&[0, 0, 255, 255], 2, 2),
            ],
        };
        let imgs = image_manifest(&dl, 3);
        assert_eq!(imgs.len(), 2);
        assert_eq!(imgs[0].file, "page-3-0.ppm");
        assert_eq!(imgs[0].width, 1);
        assert_eq!(imgs[1].file, "page-3-1.ppm");
        assert_eq!(imgs[1].height, 2);
        assert_eq!(imgs[0].json(0), r#"{"index":0,"width":1,"height":1,"file":"page-3-0.ppm"}"#);
    }

    #[test]
    fn image_manifest_ignores_non_image_ops() {
        let g = GState::new();
        let mut st = ResolvedState::from(&g);
        st.fill = [1.0, 0.0, 0.0];
        let dl = DisplayList {
            ops: vec![
                Op::Fill {
                    path: Path::new(),
                    state: st,
                },
                image_op(&[1, 2, 3, 4], 1, 1),
            ],
        };
        let imgs = image_manifest(&dl, 0);
        assert_eq!(imgs.len(), 1);
        assert_eq!(imgs[0].file, "page-0-1.ppm");
    }

    #[test]
    fn line_text_joins_words_with_space() {
        let glyph = |code: u16, x: f64| selis_pdf_content::text::TextGlyph {
            code,
            at: selis_geom::Point::new(x, 0.0),
            font: selis_bytes::Bytes::copy_from_slice(b"F1"),
            size: 12.0,
        };
        let word = |codes: &[u16], x: f64| selis_pdf_text::TextWord {
            runs: vec![selis_pdf_text::TextRun {
                glyphs: codes.iter().map(|&c| glyph(c, x)).collect(),
                bbox: selis_geom::Rect::new(x, 0.0, x + 10.0, 12.0),
            }],
            bbox: selis_geom::Rect::new(x, 0.0, x + 10.0, 12.0),
        };
        let line = TextLine {
            words: vec![word(&[72, 105], 0.0), word(&[87], 20.0)], // "Hi" "W"
            bbox: selis_geom::Rect::new(0.0, 0.0, 30.0, 12.0),
        };
        assert_eq!(line_text(&line), "Hi W");
    }
}

/// The recovered text of a line (code → Unicode char), with a space between
/// words (the assembler strips space glyphs when splitting runs into words).
fn line_text(line: &TextLine) -> String {
    let mut out = String::new();
    for (wi, word) in line.words.iter().enumerate() {
        if wi > 0 {
            out.push(' ');
        }
        for run in &word.runs {
            for g in &run.glyphs {
                if let Some(ch) = char::from_u32(u32::from(g.code)) {
                    out.push(ch);
                }
            }
        }
    }
    out
}
