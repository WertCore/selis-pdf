//! The `selis topdf` tool (SL-5.CONV.05 subset): Markdown/HTML/LaTeX/CSV/
//! SVG/EPUB/Word/PowerPoint/Excel → PDF via the `selis-pdf-convert` layout
//! engine.

use selis_pdf_convert::{
    csv_to_pdf, docx_to_pdf, epub_to_pdf, html_to_pdf, latex_to_pdf, markdown_to_pdf, pptx_to_pdf,
    svg_to_pdf, xlsx_to_pdf, Options, PageSize,
};
use selis_sandbox::Budget;

use crate::{read_file, CliError, CliResult};

/// Convert a Markdown, HTML, LaTeX, CSV, SVG, EPUB, Word, PowerPoint or
/// Excel file to PDF.
pub(crate) fn topdf(
    input: &str,
    output: &str,
    format: &str,
    page_size: &str,
    title: Option<&str>,
) -> CliResult<crate::verify_report::Verification> {
    let data = read_file(input)?;
    let in_bytes = u64::try_from(data.len()).unwrap_or(u64::MAX);

    let fmt = match format {
        "auto" => detect_format(input).ok_or_else(|| {
            CliError(format!(
                "cannot infer format of {input}; pass --format md, html, tex, csv, svg, epub, docx, pptx or xlsx"
            ))
        })?,
        "md" | "markdown" => "md",
        "html" | "htm" | "xhtml" => "html",
        "tex" | "latex" => "tex",
        "csv" => "csv",
        "svg" => "svg",
        "epub" => "epub",
        "docx" | "word" => "docx",
        "pptx" | "powerpoint" => "pptx",
        "xlsx" | "excel" => "xlsx",
        other => {
            return Err(CliError(format!(
                "unknown format `{other}` (expected auto, md, html, tex, csv, svg, epub, docx, pptx or xlsx)"
            )))
        }
    };

    // Only the text formats need their input decoded; the OOXML and EPUB
    // paths read the bytes themselves, so a binary file is never turned into a
    // lossy string it would only throw away.
    let text = match fmt {
        "md" | "html" | "tex" | "csv" | "svg" => String::from_utf8_lossy(&data).into_owned(),
        _ => String::new(),
    };
    let size = match page_size {
        "letter" => PageSize::Letter,
        "a4" => PageSize::A4,
        other => {
            return Err(CliError(format!(
                "unknown page size `{other}` (expected letter or a4)"
            )))
        }
    };

    let budget = Budget::unlimited();
    let clock = crate::shell_clock();
    let mut g = crate::runtime::cli_guard(&budget, &clock);
    let opts = Options {
        page_size: size,
        title: title.map(str::to_string),
    };
    let bytes = match fmt {
        "md" => markdown_to_pdf(&text, &opts, &budget, &mut g),
        "tex" => latex_to_pdf(&text, &opts, &budget, &mut g),
        "csv" => csv_to_pdf(&text, &opts, &budget, &mut g),
        "svg" => svg_to_pdf(&text, &opts, &budget, &mut g),
        "epub" => epub_to_pdf(&data, &opts, &budget, &mut g),
        "docx" => docx_to_pdf(&data, &opts, &budget, &mut g),
        "pptx" => pptx_to_pdf(&data, &opts, &budget, &mut g),
        "xlsx" => xlsx_to_pdf(&data, &opts, &budget, &mut g),
        _ => html_to_pdf(&text, &opts, &budget, &mut g),
    }
    .map_err(|e| CliError(format!("{input}: {e}")))?;

    // WRITE.05: generated output is verified (reference resolution + page
    // tree; no count expectations — there is no input document to survey)
    // and committed atomically.
    let verdict = crate::write_gate::write_generated(&bytes, output, &budget, &mut g)?;
    let display = crate::verify_report::Verification::from_gate(
        None,
        &verdict,
        &selis_pdf_cos::verify::Expectations::none(),
        in_bytes,
        u64::try_from(bytes.len()).unwrap_or(u64::MAX),
    );
    eprintln!("wrote {output} ({} bytes)", bytes.len());
    display.emit_line();
    Ok(display)
}

/// Infer the source format from the file extension.
fn detect_format(path: &str) -> Option<&'static str> {
    let lower = path.to_ascii_lowercase();
    if lower.ends_with(".md") || lower.ends_with(".markdown") || lower.ends_with(".mdown") {
        return Some("md");
    }
    if lower.ends_with(".html") || lower.ends_with(".htm") || lower.ends_with(".xhtml") {
        return Some("html");
    }
    if lower.ends_with(".tex") || lower.ends_with(".latex") {
        return Some("tex");
    }
    if lower.ends_with(".csv") {
        return Some("csv");
    }
    if lower.ends_with(".svg") {
        return Some("svg");
    }
    if lower.ends_with(".epub") {
        return Some("epub");
    }
    if lower.ends_with(".docx") {
        return Some("docx");
    }
    if lower.ends_with(".pptx") {
        return Some("pptx");
    }
    if lower.ends_with(".xlsx") {
        return Some("xlsx");
    }
    None
}
