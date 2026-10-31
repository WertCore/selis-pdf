//! The `selis img2pdf` tool (SL-1A.TOOL.08): JPEG/PNG images → PDF.
//!
//! The engine lives in [`selis_pdf_tools::image_to_pdf`] so the CLI and the
//! browser tools chunk run the same code. This module is only the CLI's
//! argument shim: parse the page size and forward the options.

use selis_pdf_tools::{image_to_pdf, ImagePageSize, ImageToPdfOptions};

use crate::CliResult;

/// The page-size mode for image pages.
pub(crate) use selis_pdf_tools::ImagePageSize as PageFit;

/// Parse the CLI's `--page-size` spelling.
pub(crate) fn parse_page_size(value: &str) -> CliResult<PageFit> {
    ImagePageSize::parse(value).map_err(Into::into)
}

/// Convert images to a PDF, one page per image. Returns the verification
/// display.
pub(crate) fn img2pdf(
    inputs: &[String],
    output: &str,
    fit: PageFit,
    margin: f64,
) -> CliResult<crate::verify_report::Verification> {
    image_to_pdf(
        inputs,
        ImageToPdfOptions {
            page_size: fit,
            margin,
        },
        output,
    )
    .map_err(Into::into)
}
