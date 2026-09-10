//! The `selis topdf` tool (SL-5.CONV.05 subset): Markdown/HTML → PDF via the
//! `selis-pdf-convert` layout engine.

use selis_pdf_convert::{html_to_pdf, markdown_to_pdf, Options, PageSize};
use selis_sandbox::Budget;

use crate::{read_file, CliError, CliResult};

/// Convert a Markdown or HTML file to PDF.
pub(crate) fn topdf(
    input: &str,
    output: &str,
    format: &str,
    page_size: &str,
    title: Option<&str>,
) -> CliResult<crate::verify_report::Verification> {
    let data = read_file(input)?;
    let in_bytes = u64::try_from(data.len()).unwrap_or(u64::MAX);
    let text = String::from_utf8_lossy(&data);

    let fmt = match format {
        "auto" => detect_format(input).ok_or_else(|| {
            CliError(format!(
                "cannot infer format of {input}; pass --format md or --format html"
            ))
        })?,
        "md" | "markdown" => "md",
        "html" | "htm" => "html",
        other => {
            return Err(CliError(format!(
                "unknown format `{other}` (expected auto, md or html)"
            )))
        }
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
    let mut g = crate::runtime::cli_guard(&budget);
    let opts = Options {
        page_size: size,
        title: title.map(str::to_string),
    };
    let bytes = match fmt {
        "md" => markdown_to_pdf(&text, &opts, &budget, &mut g),
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
    None
}
