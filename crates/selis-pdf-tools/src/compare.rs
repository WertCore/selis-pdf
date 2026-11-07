//! Compare two PDFs: a read-only, render-based diff. Nothing is rewritten.
//!
//! Two documents can look identical and differ in their bytes, or share bytes
//! and render differently, so the honest comparison is at the pixel level: each
//! page of both documents is rendered through the same engine at the same DPI
//! and compared sample for sample, which makes the verdict deterministic. A
//! whole-document text comparison rides alongside it, so a reader learns both
//! "do these look the same" and "do these say the same".
//!
//! The core lives here because the `text` feature links both the renderer and
//! the text ladder; the CLI and a later desktop shell call it unchanged.

use serde::Serialize;

use selis_pdf_engine::{RenderedPage, Session};
use selis_sandbox::Budget;

use crate::{read_file, CliError, ToolResult};

/// The comparison of two documents.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ComparisonReport {
    /// The versioned schema tag (`selis-compare/1`).
    pub schema: String,
    /// `ok` when both documents were compared.
    pub status: String,
    /// Page count of the first document.
    pub pages_a: u64,
    /// Page count of the second document.
    pub pages_b: u64,
    /// Whether the two documents are identical: same page count, every page
    /// pixel-identical, and the extracted text the same.
    pub identical: bool,
    /// Whether the whole-document extracted text is identical.
    pub text_identical: bool,
    /// One row per page position, up to the longer document's length.
    pub pages: Vec<PageComparison>,
}

/// The comparison of one page position across the two documents.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PageComparison {
    /// The 1-based page number.
    pub page: u64,
    /// `both`, `a_only`, or `b_only`.
    pub present_in: String,
    /// Whether the two renders match sample for sample (only when `both`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pixels_identical: Option<bool>,
    /// The fraction of pixels that differ, 0.0..=1.0 (only when `both`). A page
    /// whose dimensions differ is reported as fully different (1.0).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub diff_fraction: Option<f64>,
}

/// The lowest and highest render resolutions the comparison accepts.
const MIN_DPI: u32 = 1;
/// The highest render resolution the comparison accepts.
const MAX_DPI: u32 = 600;

/// Compare two documents page by page, rendering each at `dpi`, and compare
/// their extracted text. Neither document is modified.
///
/// # Errors
///
/// A typed [`CliError`] when `dpi` is out of range, either file cannot be read
/// or opened, or a page cannot be rendered.
pub fn compare_pdfs(path_a: &str, path_b: &str, dpi: u32) -> ToolResult<ComparisonReport> {
    if !(MIN_DPI..=MAX_DPI).contains(&dpi) {
        return Err(CliError(format!(
            "dpi must be between {MIN_DPI} and {MAX_DPI}"
        )));
    }
    let budget = Budget::unlimited();
    let clock = crate::shell_clock();
    let session_a = Session::open(read_file(path_a)?, &budget, &clock)
        .map_err(|e| CliError(format!("{path_a}: cannot open PDF: {e}")))?;
    let session_b = Session::open(read_file(path_b)?, &budget, &clock)
        .map_err(|e| CliError(format!("{path_b}: cannot open PDF: {e}")))?;
    let pages_a = session_a.len();
    let pages_b = session_b.len();
    let dpi_f = f64::from(dpi);

    let mut pages = Vec::with_capacity(pages_a.max(pages_b));
    let mut all_pages_identical = pages_a == pages_b;
    for index in 0..pages_a.max(pages_b) {
        let number = u64::try_from(index.saturating_add(1)).unwrap_or(u64::MAX);
        match (index < pages_a, index < pages_b) {
            (true, true) => {
                let mut g = crate::runtime::cli_guard(&budget, &clock);
                let render_a = session_a
                    .render_page_rgba(index, dpi_f, &budget, &mut g)
                    .map_err(|e| CliError(format!("{path_a}: page {number}: {e}")))?;
                let mut g = crate::runtime::cli_guard(&budget, &clock);
                let render_b = session_b
                    .render_page_rgba(index, dpi_f, &budget, &mut g)
                    .map_err(|e| CliError(format!("{path_b}: page {number}: {e}")))?;
                let (identical, fraction) = diff_rgba(&render_a, &render_b);
                if !identical {
                    all_pages_identical = false;
                }
                pages.push(PageComparison {
                    page: number,
                    present_in: "both".to_string(),
                    pixels_identical: Some(identical),
                    diff_fraction: Some(fraction),
                });
            }
            (true, false) => {
                all_pages_identical = false;
                pages.push(PageComparison {
                    page: number,
                    present_in: "a_only".to_string(),
                    pixels_identical: None,
                    diff_fraction: None,
                });
            }
            _ => {
                all_pages_identical = false;
                pages.push(PageComparison {
                    page: number,
                    present_in: "b_only".to_string(),
                    pixels_identical: None,
                    diff_fraction: None,
                });
            }
        }
    }

    // The text halves are compared whole-document, separators and all; two files
    // with different page counts already differ above, so the separators lining
    // up only matters when the page counts match.
    let text_a = crate::text::extract_text(path_a, None)?.text;
    let text_b = crate::text::extract_text(path_b, None)?.text;
    let text_identical = text_a == text_b;

    Ok(ComparisonReport {
        schema: "selis-compare/1".to_string(),
        status: "ok".to_string(),
        pages_a: u64::try_from(pages_a).unwrap_or(u64::MAX),
        pages_b: u64::try_from(pages_b).unwrap_or(u64::MAX),
        identical: all_pages_identical && text_identical,
        text_identical,
        pages,
    })
}

impl ComparisonReport {
    /// The report as indented JSON: the one export shape.
    ///
    /// # Errors
    ///
    /// [`CliError`] when serialization fails.
    pub fn to_json(&self) -> ToolResult<Vec<u8>> {
        serde_json::to_vec_pretty(self)
            .map_err(|e| CliError(format!("cannot encode the comparison report: {e}")))
    }
}

/// Compare two renders: `(identical, fraction-of-differing-pixels)`. Pages of
/// different dimensions are fully different.
fn diff_rgba(a: &RenderedPage, b: &RenderedPage) -> (bool, f64) {
    if a.width != b.width || a.height != b.height {
        return (false, 1.0);
    }
    if a.rgba == b.rgba {
        return (true, 0.0);
    }
    let total = (a.rgba.len() / 4).max(1);
    let differing = a
        .rgba
        .chunks_exact(4)
        .zip(b.rgba.chunks_exact(4))
        .filter(|(pa, pb)| pa != pb)
        .count();
    (false, differing as f64 / total as f64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{insert_input, MemorySession};

    fn build_xref(objects: Vec<Vec<u8>>) -> Vec<u8> {
        let mut out = b"%PDF-1.7\n".to_vec();
        let mut offsets = Vec::with_capacity(objects.len());
        for object in objects {
            offsets.push(out.len());
            out.extend_from_slice(&object);
        }
        let xref = out.len();
        let size = offsets.len() + 1;
        out.extend_from_slice(format!("xref\n0 {size}\n").as_bytes());
        out.extend_from_slice(b"0000000000 65535 f \n");
        for offset in offsets {
            out.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
        }
        out.extend_from_slice(
            format!("trailer\n<< /Size {size} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n")
                .as_bytes(),
        );
        out
    }

    fn blank_pdf(pages: usize) -> Vec<u8> {
        let mut objects = vec![b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n".to_vec()];
        let kids: Vec<String> = (0..pages).map(|i| format!("{} 0 R", 3 + i)).collect();
        objects.push(
            format!(
                "2 0 obj\n<< /Type /Pages /Kids [{}] /Count {pages} >>\nendobj\n",
                kids.join(" ")
            )
            .into_bytes(),
        );
        for i in 0..pages {
            objects.push(
                format!(
                    "{} 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] >>\nendobj\n",
                    3 + i
                )
                .into_bytes(),
            );
        }
        build_xref(objects)
    }

    /// A one-page document whose page is filled solid black.
    fn black_pdf() -> Vec<u8> {
        let content = b"0 0 0 rg\n0 0 200 200 re\nf\n";
        let mut stream = format!("4 0 obj\n<< /Length {} >>\nstream\n", content.len()).into_bytes();
        stream.extend_from_slice(content);
        stream.extend_from_slice(b"\nendstream\nendobj\n");
        let objects = vec![
            b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n".to_vec(),
            b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n".to_vec(),
            b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Contents 4 0 R >>\nendobj\n".to_vec(),
            stream,
        ];
        build_xref(objects)
    }

    #[test]
    fn identical_documents_compare_equal() {
        let _session = MemorySession::activate();
        insert_input("compare/same/a.pdf", blank_pdf(1));
        insert_input("compare/same/b.pdf", blank_pdf(1));

        let report = compare_pdfs("compare/same/a.pdf", "compare/same/b.pdf", 72).expect("report");
        assert_eq!(report.status, "ok");
        assert_eq!((report.pages_a, report.pages_b), (1, 1));
        assert!(report.identical, "two identical blanks are identical");
        assert!(report.text_identical);
        assert_eq!(report.pages[0].present_in, "both");
        assert_eq!(report.pages[0].pixels_identical, Some(true));
        assert_eq!(report.pages[0].diff_fraction, Some(0.0));
    }

    #[test]
    fn differing_pixels_are_detected() {
        let _session = MemorySession::activate();
        insert_input("compare/pix/a.pdf", blank_pdf(1));
        insert_input("compare/pix/b.pdf", black_pdf());

        let report = compare_pdfs("compare/pix/a.pdf", "compare/pix/b.pdf", 72).expect("report");
        assert!(!report.identical, "white vs black is not identical");
        assert_eq!(report.pages[0].pixels_identical, Some(false));
        let fraction = report.pages[0].diff_fraction.expect("fraction");
        assert!(fraction > 0.5, "most of the page differs, got {fraction}");
        // Neither page draws text, so the text halves still match.
        assert!(
            report.text_identical,
            "a pixel-only difference leaves text equal"
        );
    }

    #[test]
    fn a_different_page_count_is_detected() {
        let _session = MemorySession::activate();
        insert_input("compare/count/a.pdf", blank_pdf(1));
        insert_input("compare/count/b.pdf", blank_pdf(2));

        let report =
            compare_pdfs("compare/count/a.pdf", "compare/count/b.pdf", 72).expect("report");
        assert_eq!((report.pages_a, report.pages_b), (1, 2));
        assert!(!report.identical);
        assert_eq!(report.pages.len(), 2);
        assert_eq!(report.pages[1].present_in, "b_only");
        assert_eq!(report.pages[1].pixels_identical, None);
    }

    #[test]
    fn the_report_serializes() {
        let _session = MemorySession::activate();
        insert_input("compare/json/a.pdf", blank_pdf(1));
        insert_input("compare/json/b.pdf", blank_pdf(1));
        let report = compare_pdfs("compare/json/a.pdf", "compare/json/b.pdf", 72).expect("report");
        let json = String::from_utf8(report.to_json().expect("json")).expect("UTF-8");
        assert!(json.contains("selis-compare/1"));
    }

    #[test]
    fn a_bad_dpi_is_refused() {
        let _session = MemorySession::activate();
        insert_input("compare/dpi/a.pdf", blank_pdf(1));
        insert_input("compare/dpi/b.pdf", blank_pdf(1));
        assert!(compare_pdfs("compare/dpi/a.pdf", "compare/dpi/b.pdf", 0).is_err());
    }
}
