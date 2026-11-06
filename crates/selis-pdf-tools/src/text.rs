//! Text tools: extraction with its quality report, tables, and splitting at a
//! text boundary (Phase 3).
//!
//! Everything here reads a document through the engine's own extraction
//! pipeline — the page's display list, the visible-region gate, Unicode
//! recovery, run/word/line assembly and reading order — so a text export, a
//! table row and a split boundary all see the same words the viewer's text
//! layer and `selis extract` see. The reconstruction itself (quality totals,
//! page separators, column inference, paragraph reflow) lives in
//! `selis-pdf-text`; this module owns only the document boundary.
//!
//! The module is behind the `text` feature because it links the engine, which
//! is deliberately absent from the plain tools chunk.

use selis_pdf_engine::Session;
use selis_pdf_text::{ExtractionQuality, PageQuality, ReflowPage, Table, TextLine};
use selis_sandbox::{Budget, BudgetGuard};

use crate::{read_file, CliError, SplitPart, ToolResult, MAX_SPLIT_OUTPUTS};

/// One page's export: its 1-based number and its recovered text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextPage {
    /// The 1-based page number.
    pub number: u32,
    /// The page's recovered text, one line per newline.
    pub text: String,
}

/// A plain-text export and the quality report that describes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextExport {
    /// The whole export, with a separator before every page.
    pub text: String,
    /// What the export recovered, per page.
    pub quality: ExtractionQuality,
}

/// A table export: the CSV a reader opens and the JSON a program reads.
#[derive(Debug, Clone, PartialEq)]
pub struct TablesExport {
    /// Every table as CSV, one `#` comment line per table.
    pub csv: String,
    /// The same tables as a JSON report (`selis-tables/1`).
    pub json: String,
    /// How many tables were found.
    pub tables: usize,
    /// How many rows they hold together.
    pub rows: usize,
    /// The lowest confidence of any table (1.0 when there are none).
    pub confidence: f64,
}

/// A reflowed HTML article.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HtmlExport {
    /// The document bytes, as UTF-8 text.
    pub html: String,
    /// How many pages the article holds.
    pub pages: u64,
    /// How many paragraphs it holds.
    pub paragraphs: usize,
}

/// A split at text boundaries: the parts, the pages that matched, and the rule
/// that was applied, as JSON.
#[derive(Debug, Clone)]
pub struct TextSplit {
    /// The parts, in document order.
    pub parts: Vec<SplitPart>,
    /// The 1-based pages whose text matched the needle.
    pub matched_pages: Vec<u32>,
    /// The boundary rule and its matches, for a caller that shows it before
    /// committing.
    pub json: String,
}

/// The recovered lines of one page, their texts and the low-confidence flag.
///
/// This is the same ladder `selis extract` and the HTML chunk drive. Keeping it
/// here means the text tools cannot drift from them: one gather, one recovery,
/// one ordering.
fn page_lines(
    session: &Session,
    page: usize,
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
) -> ToolResult<(Vec<TextLine>, Vec<String>, bool)> {
    let number = page.saturating_add(1);
    let dl = session
        .page_text_display_list(page, budget, g)
        .map_err(|e| CliError(format!("page {number}: cannot interpret: {e}")))?;
    let visible = session.page_visible_box(page);
    let mut glyphs = selis_pdf_text::gather_glyphs(&dl, visible);
    let drew_text = !glyphs.is_empty();
    selis_pdf_text::apply_unicode_recovery(&mut glyphs, &mut |font, code| {
        session.text_unicode(page, font, code, budget, g)
    });
    let lines = selis_pdf_text::assemble(glyphs);
    let mcid_order = session
        .mcid_order(budget, g)
        .ok()
        .filter(|order| !order.is_empty());
    let mcid_lines: Vec<selis_pdf_text::LineWithMcid> = lines
        .into_iter()
        .map(|line| {
            let mcid = line
                .words
                .iter()
                .flat_map(|word| word.runs.iter())
                .find_map(|run| run.glyphs.first().and_then(|glyph| glyph.mcid));
            selis_pdf_text::LineWithMcid { line, mcid }
        })
        .collect();
    let ordered = selis_pdf_text::order_lines(mcid_lines, mcid_order.as_deref(), g)
        .map_err(|e| CliError(format!("page {number}: cannot order text: {e}")))?;
    let line_texts: Vec<String> = ordered.lines.iter().map(line_text).collect();
    let recovered = line_texts.iter().any(|text| !text.trim().is_empty());
    Ok((ordered.lines, line_texts, drew_text && !recovered))
}

/// The recovered text of a line, one space between words.
///
/// The assembler strips space glyphs when it splits runs into words, so the
/// separator has to be put back here — the same rule the HTML chunk and the
/// CLI apply.
fn line_text(line: &TextLine) -> String {
    let mut out = String::new();
    for (index, word) in line.words.iter().enumerate() {
        if index > 0 {
            out.push(' ');
        }
        for run in &word.runs {
            for glyph in &run.glyphs {
                if let Some(ch) = char::from_u32(u32::from(glyph.code)) {
                    out.push(ch);
                }
            }
        }
    }
    out
}

/// Parse a 1-based page selection; an empty selection means every page.
fn parse_pages(selection: Option<&str>, page_count: usize) -> ToolResult<Vec<usize>> {
    let text = selection.map(str::trim).unwrap_or("");
    if text.is_empty() {
        return Ok((0..page_count).collect());
    }
    if page_count == 0 {
        return Err(CliError("the document has no pages".to_string()));
    }
    let mut pages: Vec<usize> = Vec::new();
    for part in text.split(',') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        let (first, last) = match part.split_once('-') {
            Some((a, b)) => (parse_page(a, page_count)?, parse_page(b, page_count)?),
            None => {
                let n = parse_page(part, page_count)?;
                (n, n)
            }
        };
        if first > last {
            return Err(CliError(format!("page range {part:?} runs backwards")));
        }
        for n in first..=last {
            let index = n.saturating_sub(1);
            if !pages.contains(&index) {
                pages.push(index);
            }
        }
    }
    if pages.is_empty() {
        return Err(CliError("no pages selected".to_string()));
    }
    Ok(pages)
}

fn parse_page(text: &str, page_count: usize) -> ToolResult<usize> {
    let text = text.trim();
    let n: usize = text
        .parse()
        .map_err(|_| CliError(format!("{text:?} is not a page number")))?;
    if n == 0 || n > page_count {
        return Err(CliError(format!(
            "page {n} is outside this document (1-{page_count})"
        )));
    }
    Ok(n)
}

/// Open a document from the session's own store key and hand it to `body`.
fn with_session<T>(
    path: &str,
    body: impl FnOnce(&Session, &Budget) -> ToolResult<T>,
) -> ToolResult<T> {
    let src = read_file(path)?;
    let budget = Budget::unlimited();
    let clock = crate::shell_clock();
    let session = Session::open(src, &budget, &clock)
        .map_err(|e| CliError(format!("{path}: cannot open PDF: {e}")))?;
    body(&session, &budget)
}

/// Recover the document's text, with a separator before every page, and the
/// quality report that says what the ladder actually recovered.
///
/// # Errors
///
/// A typed [`crate::ToolError`] naming the document and the page that could not
/// be read, or a page selection outside the document.
pub fn extract_text(path: &str, pages: Option<&str>) -> ToolResult<TextExport> {
    with_session(path, |session, budget| {
        let clock = crate::shell_clock();
        let selected = parse_pages(pages, session.len())?;
        let mut exported: Vec<(u32, String)> = Vec::with_capacity(selected.len());
        let mut quality: Vec<PageQuality> = Vec::with_capacity(selected.len());
        for page in selected {
            let number = u32::try_from(page.saturating_add(1)).unwrap_or(u32::MAX);
            let mut g = crate::runtime::cli_guard(budget, &clock);
            let (lines, line_texts, low_confidence) = page_lines(session, page, budget, &mut g)?;
            let drew_text = low_confidence || !lines.is_empty();
            let text = selis_pdf_text::to_text(&lines, &line_texts, low_confidence);
            quality.push(PageQuality {
                number,
                characters: line_texts.iter().fold(0u64, |total, line| {
                    total.saturating_add(u64::try_from(line.chars().count()).unwrap_or(u64::MAX))
                }),
                lines: u64::try_from(line_texts.len()).unwrap_or(u64::MAX),
                drew_text,
                low_confidence,
            });
            exported.push((number, text));
        }
        Ok(TextExport {
            text: selis_pdf_text::to_text_with_page_separators(&exported),
            quality: ExtractionQuality::from_pages(quality),
        })
    })
}

/// Recover every table the page selection holds.
///
/// Columns are inferred from where words start, never from drawn rules, and
/// each table reports the share of its cells that landed in their column. A
/// document with no table is an answer — an empty CSV and a report that says
/// zero — not an error.
///
/// # Errors
///
/// As [`extract_text`].
pub fn extract_tables(path: &str, pages: Option<&str>) -> ToolResult<TablesExport> {
    with_session(path, |session, budget| {
        let clock = crate::shell_clock();
        let selected = parse_pages(pages, session.len())?;
        let mut tables: Vec<Table> = Vec::new();
        for page in selected {
            let number = u32::try_from(page.saturating_add(1)).unwrap_or(u32::MAX);
            let mut g = crate::runtime::cli_guard(budget, &clock);
            let (lines, _, _) = page_lines(session, page, budget, &mut g)?;
            tables.extend(selis_pdf_text::tables_on_page(&lines, number));
        }
        let rows = tables.iter().fold(0usize, |total, table| {
            total.saturating_add(table.rows.len())
        });
        let confidence = tables
            .iter()
            .map(|table| table.confidence)
            .fold(1.0_f64, f64::min);
        let mut report = String::from("{\n  \"schema\": \"selis-tables/1\",\n");
        report.push_str(&format!("  \"tables\": {},\n", tables.len()));
        report.push_str(&format!("  \"rows\": {rows},\n"));
        report.push_str(&format!("  \"min_confidence\": {confidence:.2},\n"));
        report.push_str("  \"found\": [\n");
        for (index, table) in tables.iter().enumerate() {
            report.push_str(&format!(
                "    {{\"page\": {}, \"columns\": {}, \"rows\": {}, \"confidence\": {:.2}}}",
                table.page,
                table.columns,
                table.rows.len(),
                table.confidence
            ));
            if index.saturating_add(1) < tables.len() {
                report.push(',');
            }
            report.push('\n');
        }
        report.push_str("  ]\n}\n");
        Ok(TablesExport {
            csv: selis_pdf_text::tables_to_csv(&tables),
            json: report,
            tables: tables.len(),
            rows,
            confidence,
        })
    })
}

/// Split the document at every page whose text matches `needle`.
///
/// The rule is fixed and stated in the returned report: a new part starts at
/// each matching page except the first, so the text that matched opens the part
/// it belongs to and no page is lost. Matching is case-insensitive on each
/// line's recovered text; the needle is trimmed, and an empty one is refused
/// rather than splitting every page.
///
/// # Errors
///
/// No page matching, a rule that produces a single part (the only match is the
/// first page), more than [`MAX_SPLIT_OUTPUTS`] parts, or a document that
/// cannot be read.
pub fn split_by_text(path: &str, needle: &str, output_prefix: &str) -> ToolResult<TextSplit> {
    let needle = needle.trim();
    if needle.is_empty() {
        return Err(CliError(
            "split by text needs the text to split at".to_string(),
        ));
    }
    let folded = needle.to_lowercase();
    with_session(path, |session, budget| {
        let clock = crate::shell_clock();
        let count = session.len();
        let mut matched: Vec<u32> = Vec::new();
        for page in 0..count {
            let mut g = crate::runtime::cli_guard(budget, &clock);
            let (_, line_texts, _) = page_lines(session, page, budget, &mut g)?;
            if line_texts
                .iter()
                .any(|line| line.to_lowercase().contains(&folded))
            {
                matched.push(u32::try_from(page.saturating_add(1)).unwrap_or(u32::MAX));
            }
        }
        if matched.is_empty() {
            return Err(CliError(format!(
                "no page contains {needle:?}, so there is no boundary to split at"
            )));
        }
        // Boundaries are 0-based page indices that start a new part.
        let mut starts: Vec<usize> = vec![0];
        for page in &matched {
            let index = usize::try_from(*page).unwrap_or(0).saturating_sub(1);
            if index > 0 && starts.last().copied() != Some(index) {
                starts.push(index);
            }
        }
        if starts.len() < 2 {
            return Err(CliError(format!(
                "{needle:?} matches only the first page, so splitting there would produce one part"
            )));
        }
        if starts.len() > MAX_SPLIT_OUTPUTS {
            return Err(CliError(format!(
                "that rule produces {} parts; at most {MAX_SPLIT_OUTPUTS} are supported",
                starts.len()
            )));
        }
        let mut parts: Vec<SplitPart> = Vec::with_capacity(starts.len());
        for (part, first) in starts.iter().enumerate() {
            let last = starts
                .get(part.saturating_add(1))
                .map_or(count.saturating_sub(1), |next| next.saturating_sub(1));
            let output = format!("{output_prefix}{part}.pdf");
            let verification = crate::split(path, *first, last, &output)?;
            parts.push(SplitPart {
                first: *first,
                last,
                output,
                verification,
                over_target: false,
            });
        }
        let mut report = String::from("{\n  \"schema\": \"selis-split-text/1\",\n");
        report.push_str(&format!("  \"needle\": {},\n", json_str(needle)));
        report.push_str(
            "  \"rule\": \"a new part starts at each matching page except the first\",\n",
        );
        report.push_str("  \"matched_pages\": [");
        for (index, page) in matched.iter().enumerate() {
            if index > 0 {
                report.push_str(", ");
            }
            report.push_str(&page.to_string());
        }
        report.push_str("],\n  \"parts\": [\n");
        for (index, part) in parts.iter().enumerate() {
            report.push_str(&format!(
                "    {{\"part\": {}, \"first\": {}, \"last\": {}, \"pages\": {}}}",
                index.saturating_add(1),
                part.first.saturating_add(1),
                part.last.saturating_add(1),
                part.verification.pages_out
            ));
            if index.saturating_add(1) < parts.len() {
                report.push(',');
            }
            report.push('\n');
        }
        report.push_str("  ]\n}\n");
        Ok(TextSplit {
            parts,
            matched_pages: matched,
            json: report,
        })
    })
}

/// Escape a string for a JSON literal.
fn json_str(value: &str) -> String {
    let mut out = String::from("\"");
    for ch in value.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            ch if ch < ' ' => out.push_str(&format!("\\u{:04x}", ch as u32)),
            ch => out.push(ch),
        }
    }
    out.push('"');
    out
}

/// Reflow the page selection into one HTML article and verify it against the
/// paragraphs it was written from.
///
/// The article is not a layout copy: paragraphs are joined, headings are read
/// from type size, and positions are gone. It is checked before it is returned
/// — every paragraph's escaped text must appear, in order, in the bytes that
/// were written — so a dropped paragraph is a failure rather than a shorter
/// article.
///
/// # Errors
///
/// As [`extract_text`], plus a document that fails its own read-back.
pub fn reflow_html(path: &str, pages: Option<&str>, title: &str) -> ToolResult<HtmlExport> {
    with_session(path, |session, budget| {
        let clock = crate::shell_clock();
        let selected = parse_pages(pages, session.len())?;
        let mut reflowed: Vec<ReflowPage> = Vec::with_capacity(selected.len());
        for page in selected {
            let number = u32::try_from(page.saturating_add(1)).unwrap_or(u32::MAX);
            let mut g = crate::runtime::cli_guard(budget, &clock);
            let (lines, line_texts, _) = page_lines(session, page, budget, &mut g)?;
            reflowed.push(ReflowPage {
                number,
                paragraphs: selis_pdf_text::paragraphs(&lines, &line_texts),
            });
        }
        let html = selis_pdf_text::to_reflow_html(title, &reflowed);
        selis_pdf_text::verify_reflow_html(&html, &reflowed)
            .map_err(|e| CliError(format!("{path}: the reflow failed its own check: {e}")))?;
        let paragraphs = reflowed.iter().fold(0usize, |total, page| {
            total.saturating_add(page.paragraphs.len())
        });
        Ok(HtmlExport {
            html,
            pages: u64::try_from(reflowed.len()).unwrap_or(u64::MAX),
            paragraphs,
        })
    })
}

/// A reflowed EPUB 3 package and what it holds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EpubExport {
    /// The `.epub` bytes.
    pub epub: Vec<u8>,
    /// How many pages the package holds, as that many chapters.
    pub pages: u64,
    /// How many paragraphs those chapters hold.
    pub paragraphs: usize,
}

/// Write the selection as an EPUB 3 package.
///
/// The book itself — chapters, navigation, package document, the stored-ZIP
/// container and the read-back check — is `selis_pdf_text::to_epub`, so the
/// desktop app and the SDK produce the same package without this crate. This
/// wrapper only supplies the reflowed pages, which need a session.
///
/// # Errors
///
/// As [`extract_text`], plus a package that fails its own read-back.
pub fn reflow_epub(path: &str, pages: Option<&str>, title: &str) -> ToolResult<EpubExport> {
    with_session(path, |session, budget| {
        let clock = crate::shell_clock();
        let selected = parse_pages(pages, session.len())?;
        let mut reflowed: Vec<ReflowPage> = Vec::with_capacity(selected.len());
        for page in selected {
            let number = u32::try_from(page.saturating_add(1)).unwrap_or(u32::MAX);
            let mut g = crate::runtime::cli_guard(budget, &clock);
            let (lines, line_texts, _) = page_lines(session, page, budget, &mut g)?;
            reflowed.push(ReflowPage {
                number,
                paragraphs: selis_pdf_text::paragraphs(&lines, &line_texts),
            });
        }
        let epub = selis_pdf_text::to_epub(title, &reflowed)
            .map_err(|e| CliError(format!("{path}: the EPUB failed its own check: {e}")))?;
        let paragraphs = reflowed.iter().fold(0usize, |total, page| {
            total.saturating_add(page.paragraphs.len())
        });
        Ok(EpubExport {
            epub,
            pages: u64::try_from(reflowed.len()).unwrap_or(u64::MAX),
            paragraphs,
        })
    })
}

/// A Word document and what it holds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WordExport {
    /// The `.docx` bytes.
    pub bytes: Vec<u8>,
    /// How many source pages contributed paragraphs.
    pub pages: u64,
    /// How many paragraphs the document holds.
    pub paragraphs: usize,
}

/// A PowerPoint deck and what it holds.
#[derive(Debug, Clone, PartialEq)]
pub struct SlidesExport {
    /// The `.pptx` bytes.
    pub bytes: Vec<u8>,
    /// How many slides the deck holds.
    pub pages: u64,
    /// How many positioned text boxes they hold together.
    pub shapes: usize,
}

/// An Excel workbook and what it holds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkbookExport {
    /// The `.xlsx` bytes.
    pub bytes: Vec<u8>,
    /// How many tables the workbook holds.
    pub tables: usize,
    /// How many rows those tables hold together.
    pub rows: usize,
}

/// Write the selection as a Word document: the frame facsimile, one frame per
/// recovered line on a page the size of the source page, zero margins. The
/// package and its read-back check are `selis_pdf_text::to_docx_frames`; this
/// wrapper measures each page view and builds the frames from the same lines
/// the text layer reports.
///
/// # Errors
///
/// As [`extract_text`], plus a page with no view or a package that fails its
/// read-back.
pub fn extract_docx(path: &str, pages: Option<&str>, title: &str) -> ToolResult<WordExport> {
    with_session(path, |session, budget| {
        let clock = crate::shell_clock();
        let selected = parse_pages(pages, session.len())?;
        let mut frame_pages: Vec<selis_pdf_text::FramePage> = Vec::with_capacity(selected.len());
        for page in selected {
            let number = u32::try_from(page.saturating_add(1)).unwrap_or(u32::MAX);
            let view = session
                .page_view(page, 72.0)
                .ok_or_else(|| CliError(format!("page {number}: cannot measure the page")))?;
            let mut g = crate::runtime::cli_guard(budget, &clock);
            let (lines, line_texts, _) = page_lines(session, page, budget, &mut g)?;
            frame_pages.push(selis_pdf_text::frame_page_from_lines(
                number,
                f64::from(view.width),
                f64::from(view.height),
                view.ctm,
                &lines,
                &line_texts,
            ));
        }
        let bytes = selis_pdf_text::to_docx_frames(title, &frame_pages).map_err(|e| {
            CliError(format!(
                "{path}: the Word document failed its own check: {e}"
            ))
        })?;
        let paragraphs = frame_pages
            .iter()
            .fold(0usize, |total, page| total.saturating_add(page.lines.len()));
        Ok(WordExport {
            bytes,
            pages: u64::try_from(frame_pages.len()).unwrap_or(u64::MAX),
            paragraphs,
        })
    })
}

/// Write the selection as a PowerPoint deck: one slide per page, each recovered
/// line placed on the page's own 72-DPI canvas. The caller may supply the page
/// rendered as a PNG (see [`extract_pptx_with_images`]) and the deck then
/// carries it full-bleed behind the text boxes; without it the deck is the
/// positioned-text-boxes facsimile without a page image.
///
/// # Errors
///
/// As [`extract_text`], plus a page with no view or a package that fails its
/// read-back.
pub fn extract_pptx(path: &str, pages: Option<&str>, title: &str) -> ToolResult<SlidesExport> {
    extract_pptx_with_images(path, pages, title, None)
}

/// Write the selection as a PowerPoint deck with the page images supplied by
/// the caller: one PNG per selected page, in selection order. The images are
/// placed full-bleed behind the text boxes, so the deck reads as a facsimile
/// and the words stay editable on top of the page's own ink.
pub fn extract_pptx_with_images(
    path: &str,
    pages: Option<&str>,
    title: &str,
    images: Option<&[Vec<u8>]>,
) -> ToolResult<SlidesExport> {
    with_session(path, |session, budget| {
        let clock = crate::shell_clock();
        let selected = parse_pages(pages, session.len())?;
        if let Some(images) = images {
            if images.len() != selected.len() {
                return Err(CliError(format!(
                    "the images do not line up with the selected pages ({} vs {})",
                    images.len(),
                    selected.len()
                )));
            }
        }
        let mut slides: Vec<selis_pdf_text::Slide> = Vec::with_capacity(selected.len());
        for (position, page) in selected.into_iter().enumerate() {
            let number = u32::try_from(page.saturating_add(1)).unwrap_or(u32::MAX);
            let view = session
                .page_view(page, 72.0)
                .ok_or_else(|| CliError(format!("page {number}: cannot measure the page")))?;
            let mut g = crate::runtime::cli_guard(budget, &clock);
            let (lines, line_texts, _) = page_lines(session, page, budget, &mut g)?;
            let mut slide = selis_pdf_text::slide_from_lines(
                number,
                f64::from(view.width),
                f64::from(view.height),
                view.ctm,
                &lines,
                &line_texts,
            );
            if let Some(images) = images {
                slide.image = images.get(position).cloned();
            }
            slides.push(slide);
        }
        let bytes = selis_pdf_text::to_pptx(title, &slides).map_err(|e| {
            CliError(format!(
                "{path}: the PowerPoint deck failed its own check: {e}"
            ))
        })?;
        let shapes = slides.iter().fold(0usize, |total, slide| {
            total.saturating_add(slide.shapes.len())
        });
        Ok(SlidesExport {
            bytes,
            pages: u64::try_from(slides.len()).unwrap_or(u64::MAX),
            shapes,
        })
    })
}

/// Write the selection's reconstructed tables as an Excel workbook.
///
/// One worksheet per table and a `Report` sheet carrying each table's page,
/// size and confidence. A document with no table is an answer — a workbook with
/// an empty report and no table sheet — not an error.
///
/// # Errors
///
/// As [`extract_text`], plus a package that fails its own read-back.
pub fn extract_xlsx(path: &str, pages: Option<&str>, title: &str) -> ToolResult<WorkbookExport> {
    with_session(path, |session, budget| {
        let clock = crate::shell_clock();
        let selected = parse_pages(pages, session.len())?;
        let mut tables: Vec<selis_pdf_text::Table> = Vec::new();
        for page in selected {
            let number = u32::try_from(page.saturating_add(1)).unwrap_or(u32::MAX);
            let mut g = crate::runtime::cli_guard(budget, &clock);
            let (lines, _, _) = page_lines(session, page, budget, &mut g)?;
            tables.extend(selis_pdf_text::tables_on_page(&lines, number));
        }
        let bytes = selis_pdf_text::to_xlsx(title, &tables).map_err(|e| {
            CliError(format!(
                "{path}: the Excel workbook failed its own check: {e}"
            ))
        })?;
        let rows = tables.iter().fold(0usize, |total, table| {
            total.saturating_add(table.rows.len())
        });
        Ok(WorkbookExport {
            bytes,
            tables: tables.len(),
            rows,
        })
    })
}
