//! The `selis extract` command: recover a page's text.
//!
//! Executes the page's content stream into a display list, groups the
//! positioned glyphs into lines, and emits them as plain text, JSON,
//! Markdown, or HTML.

use selis_pdf_content::display_list::Op;
use selis_pdf_content::text::TextGlyph;
use selis_pdf_engine::Session;
use selis_pdf_text::TextLine;
use selis_sandbox::{Budget, CancelToken, FixedClock, Surface};

use crate::{read_file, CliError, CliResult};

/// Extract a page's text.
///
/// # Errors
///
/// `IO_READ_FAILED` when the PDF cannot be read.
pub(crate) fn run(path: &str, page: usize, format: &str) -> CliResult<()> {
    let src = read_file(path)?;
    let budget = Budget::profile(Surface::Viewer);
    let session =
        Session::open(src, &budget).map_err(|e| CliError(format!("cannot open PDF: {e}")))?;
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

    // Collect the positioned glyphs from the text ops.
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
    let line_texts: Vec<String> = lines.iter().map(line_text).collect();

    let out = match format {
        "json" => {
            let s = selis_pdf_text::structured(&lines, &line_texts, &[]);
            selis_pdf_text::to_json(&s)
        }
        "md" => selis_pdf_text::to_markdown(&lines, &line_texts),
        "html" => selis_pdf_text::to_html(&lines, &line_texts),
        _ => selis_pdf_text::to_text(&lines, &line_texts),
    };
    print!("{out}");
    Ok(())
}

/// The recovered text of a line (code → Unicode char).
fn line_text(line: &TextLine) -> String {
    let mut out = String::new();
    for word in &line.words {
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
