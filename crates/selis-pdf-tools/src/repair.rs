//! Damaged-file repair (SL-1A.TOOL.08).
//!
//! Repair never guesses at content. It opens the input through the strongest
//! strategy that succeeds, rewrites every recovered page through the same
//! writer and structural gate as the other tools, and reports which strategy
//! produced the output:
//!
//! * **strict** — the ordinary parser accepted the file; the output is a clean
//!   rewrite of the document as it stood;
//! * **rolled-back revision** — the newest revision was damaged, so the
//!   previous intact revision was used;
//! * **reconstructed index** — the cross-reference table was unusable, so the
//!   object table was rebuilt by scanning for `N G obj` headers.
//!
//! The page count of the recovered document is asserted on the output, so a
//! "repair" can never silently drop pages. Encrypted inputs are refused:
//! copying encrypted stream bytes into an unencrypted writer output would
//! produce unreadable content, and removing encryption is the separate
//! unlock operation.

use selis_pdf_cos::doc_writer::DocumentBuilder;
use selis_pdf_cos::verify::Expectations;
use selis_sandbox::{Budget, BudgetGuard};

use crate::verify_report::Verification;
use crate::{read_file, CliError, CliResult};

/// How a repair opened its input.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RepairMode {
    /// The ordinary parser accepted the file.
    Strict,
    /// The newest revision was damaged; an earlier intact one was used.
    Rollback,
    /// The cross-reference table was unusable; the index was rebuilt.
    Reconstructed,
}

impl RepairMode {
    /// The short, honest label shown in the verification line and the JSON
    /// twin.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Strict => "strict",
            Self::Rollback => "rolled-back revision",
            Self::Reconstructed => "reconstructed index",
        }
    }
}

/// Open `src` through the strongest strategy that succeeds.
fn open(
    src: &[u8],
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
) -> Result<(selis_pdf_cos::Doc, RepairMode), CliError> {
    if let Some(startxref) = selis_pdf_cos::xref::find_startxref(src, 4096) {
        match selis_pdf_cos::parse_revisions(src, startxref, budget, g) {
            Ok(doc) => return Ok((doc, RepairMode::Strict)),
            Err(e) if e.is_cancelled() || e.is_budget() => return Err(CliError(e.to_string())),
            Err(_) => {}
        }
        match selis_pdf_cos::parse_revisions_resilient(src, startxref, budget, g) {
            Ok(Some(doc)) => return Ok((doc, RepairMode::Rollback)),
            Ok(None) => {}
            Err(e) if e.is_cancelled() || e.is_budget() => return Err(CliError(e.to_string())),
            Err(_) => {}
        }
    }
    let (doc, _deviation) = selis_pdf_cos::reconstruct(src, budget, g)
        .map_err(|e| CliError(format!("cannot recover the object table: {e}")))?;
    Ok((doc, RepairMode::Reconstructed))
}

/// Rewrite every recovered page into a clean, self-consistent document.
///
/// Returns the verification display with `repair_mode` set, so callers cannot
/// present a salvaged document as an ordinary rewrite.
///
/// # Errors
///
/// A typed [`CliError`] when the input cannot be recovered at all, is
/// encrypted, has no pages, or the rewritten output fails the structural gate
/// (in which case no file is written).
pub fn repair(path: &str, output: &str) -> CliResult<Verification> {
    let src = read_file(path)?;
    let in_bytes = u64::try_from(src.len()).unwrap_or(u64::MAX);
    let budget = Budget::unlimited();
    let clock = crate::shell_clock();
    let mut g = crate::runtime::cli_guard(&budget, &clock);

    let (doc, mode) =
        open(&src, &budget, &mut g).map_err(|e| CliError(format!("{path}: {}", e.0)))?;
    crate::tools::refuse_encrypted_input(path, &doc)?;

    let page_refs = crate::tools::page_refs_of(&src, &doc, &budget, &mut g)
        .map_err(|e| CliError(format!("{path}: {e}")))?;
    if page_refs.is_empty() {
        return Err(CliError(format!(
            "{path}: the recovered document has no pages"
        )));
    }

    let mut out = DocumentBuilder::new();
    let mut next_num = 3u32;
    let mut kept: Vec<(usize, u32, selis_pdf_cos::Ref)> = Vec::with_capacity(page_refs.len());
    for (index, page_ref) in page_refs.iter().enumerate() {
        let merged = crate::tools::copy_page_from_doc(
            &mut out,
            &src,
            &doc,
            *page_ref,
            &budget,
            &mut g,
            &mut next_num,
        )
        .map_err(|e| CliError(format!("{path}: {e}")))?;
        kept.push((index, page_ref.num, merged));
    }
    crate::tools::reconcile_single_input(
        &mut out, &src, &doc, &page_refs, &kept, true, &budget, &mut g,
    )
    .map_err(CliError)?;

    let bytes = out
        .write(&budget, &mut g)
        .map_err(|e| CliError(format!("write failed: {e}")))?;
    // The recovered document defines what "all pages" means; asserting that
    // count on the output is what makes "no page was dropped" a measurement
    // rather than a promise.
    let expected = Expectations {
        pages: Some(u64::try_from(page_refs.len()).unwrap_or(u64::MAX)),
        ..Expectations::none()
    };
    let verdict = crate::write_gate::write_verified(&bytes, output, &expected, &budget, &mut g)?;
    let mut verification = Verification::from_gate(
        None,
        &verdict,
        &expected,
        in_bytes,
        u64::try_from(bytes.len()).unwrap_or(u64::MAX),
    );
    // The recovered document is the only trustworthy statement about the
    // input's page count, and it is exactly the count the output must keep.
    verification.pages_in = Some(u64::try_from(page_refs.len()).unwrap_or(u64::MAX));
    verification.repair_mode = Some(mode.as_str().to_string());
    eprintln!("recovered via {} -> {output}", mode.as_str());
    verification.emit_line();
    Ok(verification)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    #![allow(clippy::indexing_slicing, clippy::arithmetic_side_effects)]

    use super::*;

    fn temp_dir(tag: &str) -> std::path::PathBuf {
        let base = std::env::temp_dir().join(format!(
            "selis-repair-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&base).expect("mkdir");
        base
    }

    /// A three-page, uncompressed PDF whose `startxref` offset is `xref_at`.
    fn three_page_pdf() -> (Vec<u8>, usize) {
        let objects = [
            "1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n",
            "2 0 obj\n<< /Type /Pages /Kids [3 0 R 4 0 R 5 0 R] /Count 3 >>\nendobj\n",
            "3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] >>\nendobj\n",
            "4 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] >>\nendobj\n",
            "5 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] >>\nendobj\n",
        ];
        let mut out = b"%PDF-1.4\n".to_vec();
        let mut offsets = Vec::new();
        for object in objects {
            offsets.push(out.len());
            out.extend_from_slice(object.as_bytes());
        }
        let xref_at = out.len();
        out.extend_from_slice(b"xref\n0 6\n0000000000 65535 f \n");
        for offset in &offsets {
            out.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
        }
        out.extend_from_slice(
            format!("trailer\n<< /Size 6 /Root 1 0 R >>\nstartxref\n{xref_at}\n%%EOF\n").as_bytes(),
        );
        (out, xref_at)
    }

    #[test]
    fn a_clean_document_reports_strict() {
        let dir = temp_dir("strict");
        let input = dir.join("clean.pdf");
        let output = dir.join("out.pdf");
        let (pdf, _) = three_page_pdf();
        std::fs::write(&input, &pdf).expect("write input");

        let verification =
            repair(input.to_str().unwrap(), output.to_str().unwrap()).expect("repair");
        assert_eq!(verification.repair_mode.as_deref(), Some("strict"));
        assert_eq!(verification.pages_out, 3);
        assert_eq!(verification.pages_in, Some(3));
        assert_eq!(verification.verdict, "ok");
        let bytes = std::fs::read(&output).expect("read output");
        assert!(bytes.starts_with(b"%PDF-"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_wrong_startxref_is_reconstructed_and_keeps_every_page() {
        let dir = temp_dir("reconstructed");
        let input = dir.join("damaged.pdf");
        let output = dir.join("out.pdf");
        let (mut pdf, xref_at) = three_page_pdf();
        // Destroy the cross-reference keyword itself. The parser tolerates a
        // mis-aimed offset, so the table has to be genuinely unreadable before
        // the scanner is the only strategy left.
        pdf[xref_at..xref_at + 4].copy_from_slice(b"zzzz");
        std::fs::write(&input, &pdf).expect("write input");

        let verification =
            repair(input.to_str().unwrap(), output.to_str().unwrap()).expect("repair");
        assert_eq!(
            verification.repair_mode.as_deref(),
            Some("reconstructed index")
        );
        assert_eq!(verification.pages_out, 3);
        let bytes = std::fs::read(&output).expect("read output");
        assert!(bytes.starts_with(b"%PDF-"));
        // The rewritten document opens strictly.
        let budget = Budget::unlimited();
        let mut g = budget.guard();
        let sx = selis_pdf_cos::xref::find_startxref(&bytes, 4096).expect("startxref");
        selis_pdf_cos::parse_revisions(&bytes, sx, &budget, &mut g).expect("output parses");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_torn_newest_revision_rolls_back() {
        let dir = temp_dir("rollback");
        let input = dir.join("torn.pdf");
        let output = dir.join("out.pdf");
        let (pdf, _) = three_page_pdf();
        // Append a second revision whose startxref target is garbage. The
        // resilient reader rolls back to the intact first revision.
        let mut torn = pdf.clone();
        torn.extend_from_slice(b"trailer\n<< /Size 6 /Root 1 0 R >>\nstartxref\n999999\n%%EOF\n");
        std::fs::write(&input, &torn).expect("write input");

        let verification =
            repair(input.to_str().unwrap(), output.to_str().unwrap()).expect("repair");
        assert_eq!(
            verification.repair_mode.as_deref(),
            Some("rolled-back revision")
        );
        assert_eq!(verification.pages_out, 3);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn unrecoverable_input_is_refused() {
        let dir = temp_dir("garbage");
        let input = dir.join("garbage.pdf");
        let output = dir.join("out.pdf");
        std::fs::write(&input, b"this is not a pdf at all").expect("write input");
        let error = repair(input.to_str().unwrap(), output.to_str().unwrap())
            .expect_err("garbage must not repair");
        assert!(error.0.contains("cannot recover"), "{}", error.0);
        assert!(!output.exists(), "a failed repair writes nothing");
        std::fs::remove_dir_all(&dir).ok();
    }
}
