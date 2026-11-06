//! The `selis extract` command: recover a page's text or images.
//!
//! Executes the page's content stream into a display list, groups the
//! positioned glyphs into lines, and emits them as plain text, JSON,
//! Markdown, or HTML.  With `--format=image`, writes each image XObject
//! used on the page to a PPM file and prints a JSON manifest.
//!
//! HTML is the one format that is not a per-page printout: the whole page
//! range becomes a single document whose `<section>`s are the pages, laid out
//! on each page's own canvas, and it is read back and compared against the
//! extraction before it is written.

use crate::render::write_ppm;
use crate::{read_file, CliError, CliResult};
use selis_pdf_content::display_list::Op;
use selis_pdf_engine::Session;
use selis_pdf_text::TextLine;
use selis_sandbox::{Budget, Surface};

/// The DPI the HTML document is laid out at: 72 is one CSS pixel per PDF unit,
/// the same canvas `selis render --format png --dpi 72` paints, so the text
/// lands where the rendered page puts the ink.
const HTML_DPI: f64 = 72.0;

/// Extract a page's text or images.
///
/// # Errors
///
/// `IO_READ_FAILED` when the PDF cannot be read.
pub(crate) fn run(
    path: &str,
    page: usize,
    last: Option<usize>,
    format: &str,
    output: Option<&str>,
    separators: bool,
    quality_out: Option<&str>,
) -> CliResult<()> {
    // The Phase 3 text formats read the document through the tools layer, which
    // owns the same extraction ladder and reports its own quality. They are
    // document-level answers (one table list, one article), not a per-page
    // printout, so they branch before the per-page loop below.
    if format == "tables"
        || format == "reflow"
        || format == "epub"
        || format == "docx"
        || format == "pptx"
        || format == "xlsx"
        || (format == "text" && separators)
    {
        let first = page.saturating_add(1);
        let end = last.unwrap_or(page).saturating_add(1);
        let selection = if end > first {
            format!("{first}-{end}")
        } else {
            first.to_string()
        };
        return run_text_tool(path, format, &selection, output, quality_out);
    }
    let src = read_file(path)?;
    let budget = Budget::profile(Surface::Viewer);
    let clock = crate::shell_clock();
    let session = Session::open(src, &budget, &clock)
        .map_err(|e| CliError(format!("cannot open PDF: {e}")))?;

    // Document-level formats don't need a page.
    if format == "embedded" {
        let mut g = budget.guard_with(&clock, crate::runtime::token());
        return extract_embedded(&session, &budget, &mut g, output);
    }

    let last = last.unwrap_or(page).min(session.len().saturating_sub(1));
    if page >= session.len() {
        return Err(CliError(format!(
            "page {page} out of range (document has {} pages)",
            session.len()
        )));
    }
    let mut g = budget.guard_with(&clock, crate::runtime::token());
    if format == "html" {
        // The name the user typed is the only title worth putting in the
        // document: a browser tab that says "export" tells them nothing about
        // which PDF they exported.
        let title = std::path::Path::new(path)
            .file_stem()
            .and_then(|stem| stem.to_str())
            .unwrap_or("PDF");
        return extract_html(&session, &budget, &mut g, page, last, output, title);
    }
    let mut first_output = true;
    for p in page..=last {
        let dl = session
            .page_text_display_list(p, &budget, &mut g)
            .map_err(|e| CliError(format!("cannot interpret page: {e}")))?;

        if format == "image" {
            extract_images(&dl, p)?;
            continue;
        }

        let mcid_order = session
            .mcid_order(&budget, &mut g)
            .ok()
            .filter(|v| !v.is_empty());
        let (lines, line_texts, low_confidence) =
            page_lines(&session, p, &budget, &dl, mcid_order.as_deref(), &mut g)?;
        let page_out = match format {
            "json" => {
                let mut s = selis_pdf_text::structured(&lines, &line_texts, &run_texts(&lines));
                s.low_confidence = low_confidence;
                selis_pdf_text::to_json(&s)
            }
            "md" => selis_pdf_text::to_markdown(&lines, &line_texts, low_confidence),
            _ => selis_pdf_text::to_text(&lines, &line_texts, low_confidence),
        };
        if !first_output && page != last {
            println!();
        }
        print!("{page_out}");
        first_output = false;
    }
    Ok(())
}

/// Run one of the Phase 3 text formats: the exports that answer for the whole
/// document rather than for one page.
///
/// * `tables` writes the reconstructed tables as CSV.
/// * `reflow` writes the reading-order HTML article.
/// * `epub` writes the same article as an EPUB 3 package (needs `--output`).
/// * `docx` writes the reading-order article as a Word document (needs `--output`).
/// * `pptx` writes one slide per page with positioned text boxes (needs `--output`).
/// * `xlsx` writes one sheet per reconstructed table plus a report sheet (needs `--output`).
/// * `text` with `--separators` writes one plain-text document with a separator
///   before every page, and its quality report (to `--quality-out`, or stderr).
///
/// # Errors
///
/// A typed message naming the document that could not be read, or a reflow that
/// failed its own read-back.
fn run_text_tool(
    path: &str,
    format: &str,
    selection: &str,
    output: Option<&str>,
    quality_out: Option<&str>,
) -> CliResult<()> {
    if format == "tables" {
        let export = selis_pdf_tools::extract_tables(path, Some(selection))
            .map_err(|e| CliError(e.to_string()))?;
        match output {
            Some(target) => {
                std::fs::write(target, &export.csv)
                    .map_err(|e| CliError(format!("cannot write {target}: {e}")))?;
                eprintln!(
                    "wrote {} table(s), {} row(s) to {target}",
                    export.tables, export.rows
                );
            }
            None => print!("{}", export.csv),
        }
        return Ok(());
    }
    if format == "epub" {
        let title = std::path::Path::new(path)
            .file_stem()
            .and_then(|stem| stem.to_str())
            .unwrap_or("PDF");
        let export = selis_pdf_tools::reflow_epub(path, Some(selection), title)
            .map_err(|e| CliError(e.to_string()))?;
        match output {
            Some(target) => {
                std::fs::write(target, &export.epub)
                    .map_err(|e| CliError(format!("cannot write {target}: {e}")))?;
                eprintln!(
                    "wrote {} page(s), {} paragraph(s) to {target}",
                    export.pages, export.paragraphs
                );
            }
            None => {
                return Err(CliError(
                    "--format epub needs --output: the package is binary".to_string(),
                ))
            }
        }
        return Ok(());
    }
    if format == "docx" || format == "pptx" || format == "xlsx" {
        let title = std::path::Path::new(path)
            .file_stem()
            .and_then(|stem| stem.to_str())
            .unwrap_or("PDF");
        let stem = std::path::Path::new(path)
            .file_stem()
            .and_then(|stem| stem.to_str())
            .unwrap_or("document")
            .to_string();
        let (bytes, summary) = match format {
            "docx" => {
                let export = selis_pdf_tools::extract_docx(path, Some(selection), title)
                    .map_err(|e| CliError(e.to_string()))?;
                (export.bytes, format!("{} paragraph(s)", export.paragraphs))
            }
            "pptx" => {
                let export = selis_pdf_tools::extract_pptx(path, Some(selection), title)
                    .map_err(|e| CliError(e.to_string()))?;
                (
                    export.bytes,
                    format!("{} page(s), {} text box(es)", export.pages, export.shapes),
                )
            }
            _ => {
                let export = selis_pdf_tools::extract_xlsx(path, Some(selection), title)
                    .map_err(|e| CliError(e.to_string()))?;
                (
                    export.bytes,
                    format!("{} table(s), {} row(s)", export.tables, export.rows),
                )
            }
        };
        match output {
            Some(target) => {
                std::fs::write(target, &bytes)
                    .map_err(|e| CliError(format!("cannot write {target}: {e}")))?;
                eprintln!("wrote {target} ({summary})");
            }
            None => {
                return Err(CliError(format!(
                    "--format {format} needs --output: {stem}.{format} is binary"
                )))
            }
        }
        return Ok(());
    }
    if format == "reflow" {
        let title = std::path::Path::new(path)
            .file_stem()
            .and_then(|stem| stem.to_str())
            .unwrap_or("PDF");
        let export = selis_pdf_tools::reflow_html(path, Some(selection), title)
            .map_err(|e| CliError(e.to_string()))?;
        match output {
            Some(target) => {
                std::fs::write(target, &export.html)
                    .map_err(|e| CliError(format!("cannot write {target}: {e}")))?;
                eprintln!(
                    "wrote {} page(s), {} paragraph(s) to {target}",
                    export.pages, export.paragraphs
                );
            }
            None => print!("{}", export.html),
        }
        return Ok(());
    }
    let export = selis_pdf_tools::extract_text(path, Some(selection))
        .map_err(|e| CliError(e.to_string()))?;
    match output {
        Some(target) => {
            std::fs::write(target, &export.text)
                .map_err(|e| CliError(format!("cannot write {target}: {e}")))?;
            eprintln!(
                "wrote {} page(s) to {target} ({})",
                export.quality.pages,
                export.quality.verdict()
            );
        }
        None => print!("{}", export.text),
    }
    let report = export.quality.to_json();
    match quality_out {
        Some(target) => {
            std::fs::write(target, &report)
                .map_err(|e| CliError(format!("cannot write {target}: {e}")))?;
        }
        None => eprintln!("{report}"),
    }
    Ok(())
}

/// Split the document at every page whose text matches `find`.
///
/// The rule is stated before anything is written (it is the report's own
/// `rule` field), every part is verified by the writer's structural gate, and
/// the JSON report lists the parts and the pages that matched. A refusal (no
/// page matches, or the only match is the first page) produces no file.
///
/// # Errors
///
/// A typed message from the tools layer, or a part that cannot be written.
pub(crate) fn split(path: &str, find: &str, prefix: &str, json: bool) -> CliResult<()> {
    let split =
        selis_pdf_tools::split_by_text(path, find, prefix).map_err(|e| CliError(e.to_string()))?;
    for (index, part) in split.parts.iter().enumerate() {
        eprintln!(
            "part {}: pages {}-{} -> {} ({} page(s), {})",
            index.saturating_add(1),
            part.first.saturating_add(1),
            part.last.saturating_add(1),
            part.output,
            part.verification.pages_out,
            part.verification.verdict
        );
    }
    if json {
        print!("{}", split.json);
    }
    Ok(())
}

/// Write the page range as ONE positioned HTML document.
///
/// One `<section>` per page at that page's own canvas size, one `<p>` per span
/// at its projected position, and the SL-3.TEXT.10 marker where a page drew text
/// the extractor could not recover. The document is then re-read with
/// `selis-pdf-text`'s own strict reader and compared against the model it was
/// built from, so what reaches the user's terminal or `--output` file is a
/// document known to carry its own pages. A run that cannot verify is an error
/// with a non-zero exit, never a file.
///
/// # Errors
///
/// A page that cannot be read, ordered or measured, and a document that fails
/// its own read-back.
fn extract_html(
    session: &Session,
    budget: &Budget,
    g: &mut selis_sandbox::BudgetGuard<'_>,
    page: usize,
    last: usize,
    output: Option<&str>,
    title: &str,
) -> CliResult<()> {
    let mcid_order = session.mcid_order(budget, g).ok().filter(|v| !v.is_empty());
    let mut laid: Vec<(u32, selis_pdf_engine::PageView, selis_pdf_text::Structured)> = Vec::new();
    for p in page..=last {
        let number = u32::try_from(p.saturating_add(1)).unwrap_or(u32::MAX);
        let dl = session
            .page_text_display_list(p, budget, g)
            .map_err(|e| CliError(format!("cannot interpret page {number}: {e}")))?;
        let (lines, line_texts, low_confidence) =
            page_lines(session, p, budget, &dl, mcid_order.as_deref(), g)?;
        let mut model = selis_pdf_text::structured(&lines, &line_texts, &run_texts(&lines));
        model.low_confidence = low_confidence;
        let view = session
            .page_view(p, HTML_DPI)
            .ok_or_else(|| CliError(format!("page {number} has no box to lay text out on")))?;
        if view.width == 0 || view.height == 0 {
            return Err(CliError(format!(
                "page {number} has no area to place text on: its box is {}x{} px at 72 DPI",
                view.width, view.height
            )));
        }
        laid.push((number, view, model));
    }
    let pages: Vec<selis_pdf_text::HtmlPage<'_>> = laid
        .iter()
        .map(|(number, view, model)| selis_pdf_text::HtmlPage {
            number: *number,
            width_px: view.width,
            height_px: view.height,
            ctm: view.ctm,
            content: model,
        })
        .collect();
    let document = selis_pdf_text::to_positioned_html(&pages, title)
        .map_err(|e| CliError(format!("cannot write HTML: {e}")))?;
    selis_pdf_text::verify_positioned_html(&pages, title, &document).map_err(|e| {
        CliError(format!(
            "HTML output is not the pages it was built from: {e}"
        ))
    })?;
    match output {
        Some(path) => {
            std::fs::write(path, &document)
                .map_err(|e| CliError(format!("cannot write {path}: {e}")))?;
            eprintln!("wrote {} page(s) to {path}", pages.len());
        }
        None => print!("{document}"),
    }
    Ok(())
}

/// The per-run texts of assembled lines: one string per run, in the order the
/// runs appear across a line's words. Both output shapes that keep span
/// structure — JSON and the positioned HTML document — need them, because
/// [`selis_pdf_text::structured`] falls back to repeating the whole line's text
/// when a caller does not supply them.
fn run_texts(lines: &[TextLine]) -> Vec<Vec<String>> {
    lines
        .iter()
        .map(|line| {
            line.words
                .iter()
                .flat_map(|w| w.runs.iter())
                .map(|run| {
                    run.glyphs
                        .iter()
                        .filter_map(|g| char::from_u32(u32::from(g.code)))
                        .collect::<String>()
                })
                .collect::<Vec<String>>()
        })
        .collect()
}

/// The assembled text lines and their recovered Unicode texts for a display
/// list, in reading order. `mcid_order` (the structure tree's marked-content
/// order) enables structure-first ordering for tagged PDFs; `None` falls back
/// to column-aware geometry.
///
/// Codes are replaced by their recovered Unicode (SL-3.TEXT.08: `/ToUnicode`
/// for either font class, the encoding/glyph-name chain for simple fonts;
/// SL-3.SHAPE.04 kept this order for CIDs) BEFORE assembly text is read, so
/// `line_text` below and the word splitter see characters, not raw codes —
/// a raw CID can equal 0x20 without being a space, and a raw WinAnsi byte is
/// only *luck* away from its character.
///
/// The third element of the tuple is the SL-3.TEXT.10 low-confidence flag:
/// true when the display list drew glyphs (or text ops exist) yet zero
/// characters were recovered — callers must surface the marker instead of a
/// silent empty string on such pages.
///
/// # Errors
///
/// `BUDGET_BYTES` when the reading-order ranking buffer cannot be budgeted.
pub(crate) fn page_lines(
    session: &Session,
    page: usize,
    budget: &selis_sandbox::Budget,
    dl: &selis_pdf_content::display_list::DisplayList,
    mcid_order: Option<&[u32]>,
    g: &mut selis_sandbox::BudgetGuard<'_>,
) -> CliResult<(Vec<selis_pdf_text::TextLine>, Vec<String>, bool)> {
    // Codes become Unicode scalars BEFORE assembly (SL-3.TEXT.08): the word
    // splitter strips `0x20` glyphs, and a raw CID can equal 0x20 without
    // being a space (SL-3.SHAPE.04 — e.g. Devanagari CID 32 is vowel sign I),
    // while a raw simple-font byte is an encoded code, not a character. The
    // shared gather helper also carries each glyph's pen advance and the
    // font's space width so word-gap inference has same-space references
    // (SL-3.TEXT.09).
    // The page's visible region (`/MediaBox ∩ /CropBox`) gates extraction:
    // a glyph the page clips away is not text on this page (SL-3.TEXT.26).
    // `None` (no declared region) clips nothing.
    let visible = session.page_visible_box(page);
    let mut glyphs = selis_pdf_text::gather_glyphs(dl, visible);
    let drew_text = !glyphs.is_empty();
    selis_pdf_text::apply_unicode_recovery(&mut glyphs, &mut |font, code| {
        session.text_unicode(page, font, code, budget, g)
    });
    let lines = selis_pdf_text::assemble(glyphs);
    // Reading order: column detection + XY-cut (no structure-tree MCIDs).
    let mcid_lines: Vec<selis_pdf_text::LineWithMcid> = lines
        .into_iter()
        .map(|line| {
            let mcid = line
                .words
                .iter()
                .flat_map(|w| w.runs.iter())
                .find_map(|r| r.glyphs.first().and_then(|g| g.mcid));
            selis_pdf_text::LineWithMcid { line, mcid }
        })
        .collect();
    let ordered = selis_pdf_text::order_lines(mcid_lines, mcid_order, g)
        .map_err(|e| CliError(format!("cannot order text: {e}")))?;
    let line_texts: Vec<String> = ordered.lines.iter().map(line_text).collect();
    let recovered = line_texts.iter().any(|t| !t.trim().is_empty());
    // SL-3.TEXT.10: text was drawn but nothing is readable. The flag makes
    // the formatters emit a low-confidence marker instead of a silent empty
    // page.
    let low_confidence = drew_text && !recovered;
    Ok((ordered.lines, line_texts, low_confidence))
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
        std::fs::create_dir_all(dir).map_err(|e| CliError(format!("cannot create {dir}: {e}")))?;
        let mut written = 0usize;
        for a in &attachments {
            let raw = a
                .name
                .as_ref()
                .map(|b| String::from_utf8_lossy(b.as_slice()).to_string());
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
        let name = a
            .name
            .as_ref()
            .map(|b| String::from_utf8_lossy(b.as_slice()).to_string());
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
fn extract_images(dl: &selis_pdf_content::display_list::DisplayList, page: usize) -> CliResult<()> {
    let images = image_manifest(dl, page);
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
fn image_manifest(
    dl: &selis_pdf_content::display_list::DisplayList,
    page: usize,
) -> Vec<ExtractedImage> {
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
        assert_eq!(
            imgs[0].json(0),
            r#"{"index":0,"width":1,"height":1,"file":"page-3-0.ppm"}"#
        );
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
            tm: selis_geom::Matrix::IDENTITY,
            advance: 10.0,
            font: selis_bytes::Bytes::copy_from_slice(b"F1"),
            size: 12.0,
            space: 4.0,
            dir_x: 1.0,
            dir_y: 0.0,
            mcid: None,
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
