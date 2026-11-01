//! The batch queue: one tool applied to many documents, each on its own.
//!
//! A pile of files is where a tool earns its keep and where it can do the most
//! damage, so the queue has one rule above all others: **a file that fails is a
//! row, not a cancelled run**. Every job is attempted, in order, and each result
//! is recorded with its own verification or its own typed error. Nothing about
//! file 3 is read while processing file 2 — a job sees only the store key it was
//! given — so a damaged or hostile document cannot contaminate its neighbours
//! beyond sharing the browser tab with them.
//!
//! The queue itself can still refuse before any work starts: an empty list is
//! probably a mistake, and an unbounded one would pin memory for the whole run.

use crate::verify_report::Verification;
use crate::{ToolError, ToolResult};

/// The most documents one queue may hold.
///
/// Every file is processed in sequence inside one request, and each verified
/// result is held until the caller collects it. The cap keeps a dropped folder
/// from turning into a run that cannot finish before the tab is closed.
pub const MAX_BATCH_FILES: usize = 64;

/// The tool a batch run applies to every queued document.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BatchTool {
    /// Rewrite each document with the lossless compressor.
    Compress,
    /// Drop each document's `/Info` dictionary and XMP packet.
    StripMetadata,
    /// Rebuild each document from whatever parts of it can be read.
    Repair,
}

impl BatchTool {
    /// Parse the name a caller sends. An unknown name is refused rather than
    /// defaulted, so a stale page cannot silently run a different tool over the
    /// user's files.
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "compress" => Some(Self::Compress),
            "strip-metadata" => Some(Self::StripMetadata),
            "repair" => Some(Self::Repair),
            _ => None,
        }
    }

    /// The name used on the wire and in the report.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Compress => "compress",
            Self::StripMetadata => "strip-metadata",
            Self::Repair => "repair",
        }
    }

    /// The word each result is named with, so a queue of `a.pdf`, `b.pdf`
    /// downloads `a-compressed.pdf`, `b-compressed.pdf`, and not three files
    /// the user has to tell apart by looking inside them.
    #[must_use]
    pub fn suffix(self) -> &'static str {
        match self {
            Self::Compress => "compressed",
            Self::StripMetadata => "clean",
            Self::Repair => "repaired",
        }
    }

    /// Run this tool on one document. Failures are returned, never swallowed:
    /// the queue decides what a failure means for the run.
    fn run(self, input: &str, output: &str) -> ToolResult<Verification> {
        match self {
            Self::Compress => crate::compress::compress_file(input, output)
                .map(|(_report, verification)| verification),
            Self::StripMetadata => crate::strip_metadata(input, output),
            Self::Repair => crate::repair(input, output),
        }
    }
}

/// One queued document: the store key its bytes live under, and the key the
/// verified result is written to.
#[derive(Debug, Clone)]
pub struct BatchJob {
    /// The input path handed to the operation.
    pub input: String,
    /// The output path the operation writes to on success.
    pub output: String,
}

impl BatchJob {
    /// A queue entry pairing input and output keys.
    #[must_use]
    pub fn new(input: &str, output: &str) -> Self {
        Self {
            input: input.to_string(),
            output: output.to_string(),
        }
    }
}

/// One queued document's outcome.
#[derive(Debug, Clone)]
pub struct BatchFile {
    /// The input key this row was produced from, in queue order.
    pub input: String,
    /// The output key holding the verified bytes, only when the file succeeded.
    pub output: Option<String>,
    /// `"ok"` or `"failed"`.
    pub status: &'static str,
    /// The typed error text when the file failed. Never a document byte.
    pub error: Option<String>,
    /// The verification of the produced document, when there was one.
    pub verification: Option<Verification>,
}

impl BatchFile {
    /// Whether this row succeeded.
    #[must_use]
    pub fn is_ok(&self) -> bool {
        self.status == "ok"
    }
}

/// The whole queue's result. A batch has no single verdict: the counts and the
/// rows are the answer, and the caller shows them.
#[derive(Debug, Clone)]
pub struct BatchOutcome {
    /// The tool that ran.
    pub tool: &'static str,
    /// One row per queued document, in queue order.
    pub files: Vec<BatchFile>,
}

impl BatchOutcome {
    /// How many documents produced a verified output.
    #[must_use]
    pub fn ok(&self) -> usize {
        self.files.iter().filter(|file| file.is_ok()).count()
    }

    /// How many documents were isolated by their own failure.
    #[must_use]
    pub fn failed(&self) -> usize {
        self.files.len().saturating_sub(self.ok())
    }
}

/// Run `tool` over every queued document and report each one separately.
///
/// The order of `files` matches the order of `jobs`, so a caller can pair a row
/// with the file the user picked. A document that fails writes nothing: its row
/// carries the error and the queue moves on to the next job.
///
/// # Errors
///
/// `TOOL_INPUT_INVALID` when the queue is empty or exceeds
/// [`MAX_BATCH_FILES`]. Per-document failures never reach here — they are rows.
pub fn run_batch(tool: BatchTool, jobs: &[BatchJob]) -> ToolResult<BatchOutcome> {
    if jobs.is_empty() {
        return Err(ToolError("the batch queue has no files to run".to_string()));
    }
    if jobs.len() > MAX_BATCH_FILES {
        return Err(ToolError(format!(
            "the batch queue holds {} files; {} is the most one run can process",
            jobs.len(),
            MAX_BATCH_FILES
        )));
    }
    let files = jobs
        .iter()
        .map(|job| run_one(tool, job))
        .collect::<Vec<BatchFile>>();
    Ok(BatchOutcome {
        tool: tool.name(),
        files,
    })
}

/// Attempt one queue entry. Nothing here can stop the queue: the only outcome
/// is a row.
fn run_one(tool: BatchTool, job: &BatchJob) -> BatchFile {
    match tool.run(&job.input, &job.output) {
        Ok(verification) => BatchFile {
            input: job.input.clone(),
            output: Some(job.output.clone()),
            status: "ok",
            error: None,
            verification: Some(verification),
        },
        Err(error) => BatchFile {
            input: job.input.clone(),
            output: None,
            status: "failed",
            error: Some(error.to_string()),
            verification: None,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{insert_input, take_output, MemorySession};

    /// A document of `pages` blank pages, each 200×200 pt, built with exact
    /// `xref` offsets so the parser is never the thing under test. Object 1 is
    /// the catalog, 2 the page tree, `3..` the pages, and the last object the
    /// `/Info` dictionary naming which fixture this is.
    fn document(pages: usize) -> Vec<u8> {
        let mut kids = String::new();
        for page in 0..pages {
            kids.push_str(&format!("{} 0 R ", page.saturating_add(3)));
        }
        let info = pages.saturating_add(3);
        let mut objects = vec![format!(
            "1 0 obj\n<< /Type /Catalog /Pages 2 0 R /Info {info} 0 R >>\nendobj\n"
        )];
        objects.push(format!(
            "2 0 obj\n<< /Type /Pages /Kids [{kids}] /Count {pages} >>\nendobj\n"
        ));
        for page in 0..pages {
            let number = page.saturating_add(3);
            objects.push(format!(
                "{number} 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] >>\nendobj\n"
            ));
        }
        objects.push(format!(
            "{info} 0 obj\n<< /Title (File {pages} pages) >>\nendobj\n"
        ));

        let mut out = b"%PDF-1.4\n".to_vec();
        let mut offsets = Vec::with_capacity(objects.len());
        for object in objects {
            offsets.push(out.len());
            out.extend_from_slice(object.as_bytes());
        }
        let xref = out.len();
        let size = offsets.len().saturating_add(1);
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

    /// Write the fixtures into the shared in-memory table under a per-test tag:
    /// the table is process-wide and these tests run on parallel threads, so
    /// keys like `in/0` would collide between cases.
    fn queue(tag: &str, files: &[&[u8]]) -> Vec<BatchJob> {
        for (index, bytes) in files.iter().enumerate() {
            insert_input(&format!("in/{tag}/{index}"), bytes.to_vec());
        }
        (0..files.len())
            .map(|index| BatchJob::new(&format!("in/{tag}/{index}"), &format!("out/{tag}/{index}")))
            .collect()
    }

    /// The rule the queue exists to enforce: a file that fails is a row, and
    /// the files around it still get their results.
    #[test]
    fn one_failing_file_does_not_cancel_the_rest_of_the_queue() {
        let _session = MemorySession::activate();
        let jobs = queue(
            "isolated",
            &[b"this is not a PDF at all", &document(1), &document(3)],
        );

        let outcome = run_batch(BatchTool::Compress, &jobs).expect("queue runs");
        assert_eq!(outcome.tool, "compress");
        assert_eq!(outcome.files.len(), 3);
        assert_eq!(outcome.ok(), 2);
        assert_eq!(outcome.failed(), 1);

        let failed = outcome.files.first().expect("first row");
        assert_eq!(failed.status, "failed");
        assert!(!failed.is_ok());
        assert_eq!(failed.output, None);
        assert_eq!(failed.verification, None);
        let error = failed.error.as_deref().expect("a reason");
        assert!(
            error.contains("in/isolated/0"),
            "the row names its own file: {error}"
        );
        assert!(
            take_output("out/isolated/0").is_none(),
            "a failure writes nothing"
        );

        let pages: Vec<u64> = outcome
            .files
            .iter()
            .filter_map(|file| file.verification.as_ref().map(|v| v.pages_out))
            .collect();
        assert_eq!(
            pages,
            vec![1, 3],
            "the survivors are reported in queue order"
        );
        for index in 1..3 {
            let key = format!("out/isolated/{index}");
            let bytes = take_output(&key).expect("a verified result per surviving file");
            assert!(bytes.starts_with(b"%PDF-"), "{key}");
        }
    }

    /// Each row is produced from its own document and nothing else: the page
    /// counts travel per file, so a queue cannot mix two files' structures.
    #[test]
    fn every_row_describes_only_its_own_document() {
        let _session = MemorySession::activate();
        let jobs = queue("own", &[&document(2), &document(5)]);

        let outcome = run_batch(BatchTool::StripMetadata, &jobs).expect("queue runs");
        assert_eq!(outcome.failed(), 0);
        let rows: Vec<(String, u64, u64)> = outcome
            .files
            .iter()
            .map(|file| {
                let verification = file.verification.as_ref().expect("verified row");
                (
                    file.input.clone(),
                    verification.pages_in.unwrap_or(0),
                    verification.pages_out,
                )
            })
            .collect();
        assert_eq!(
            rows,
            vec![
                ("in/own/0".to_string(), 2, 2),
                ("in/own/1".to_string(), 5, 5)
            ],
            "a queue never borrows a neighbour's page count"
        );

        for index in 0..2 {
            let key = format!("out/own/{index}");
            let bytes = take_output(&key).expect("output");
            let text = String::from_utf8_lossy(&bytes);
            assert!(!text.contains("/Info"), "{key} still carries metadata");
            assert!(!text.contains("File 2 pages"), "{key}");
            assert!(!text.contains("File 5 pages"), "{key}");
        }
    }

    /// A repair queue is where the isolation matters most: the damaged file is
    /// the one a user would otherwise lose the whole batch over.
    #[test]
    fn a_repair_queue_recovers_what_it_can_and_reports_what_it_cannot() {
        let _session = MemorySession::activate();
        let mut damaged = document(1);
        let at = damaged
            .windows(b"trailer".len())
            .position(|window| window == b"trailer")
            .expect("trailer");
        damaged.splice(
            at..at.saturating_add(b"trailer".len()),
            b"ZZZZZZZ".iter().copied(),
        );
        let jobs = queue("repairs", &[&damaged, b"not a PDF"]);

        let outcome = run_batch(BatchTool::Repair, &jobs).expect("queue runs");
        assert_eq!(outcome.ok(), 1);
        assert_eq!(outcome.failed(), 1);
        let recovered = outcome.files.first().expect("first row");
        assert_eq!(recovered.status, "ok");
        assert_eq!(
            recovered.verification.as_ref().expect("verified").pages_out,
            1
        );
        assert_eq!(outcome.files.get(1).expect("second row").status, "failed");
    }

    /// The queue refuses to be a mistake: nothing to do, or far too much to do
    /// in one request, is answered before a document is opened.
    #[test]
    fn an_impossible_queue_is_refused_before_any_file_is_opened() {
        let _session = MemorySession::activate();
        let empty = run_batch(BatchTool::Compress, &[]);
        assert!(empty
            .expect_err("an empty queue is refused")
            .to_string()
            .contains("no files"));

        let too_many: Vec<BatchJob> = (0..=MAX_BATCH_FILES)
            .map(|index| BatchJob::new(&format!("in/{index}"), &format!("out/{index}")))
            .collect();
        let error =
            run_batch(BatchTool::Compress, &too_many).expect_err("an oversized queue is refused");
        assert!(
            error.to_string().contains(&MAX_BATCH_FILES.to_string()),
            "{error}"
        );
    }

    /// A name the queue does not know is refused rather than guessed at: the
    /// user asked for one tool, and running another would be a silent swap.
    #[test]
    fn a_tool_name_is_matched_exactly_and_carries_its_own_suffix() {
        assert_eq!(
            BatchTool::from_name("compress").map(BatchTool::name),
            Some("compress")
        );
        assert_eq!(
            BatchTool::from_name("strip-metadata").map(BatchTool::suffix),
            Some("clean")
        );
        assert_eq!(
            BatchTool::from_name("repair").map(BatchTool::suffix),
            Some("repaired")
        );
        assert_eq!(BatchTool::from_name(""), None);
        assert_eq!(BatchTool::from_name("Unlock"), None);
        assert_eq!(BatchTool::from_name("protect"), None);
    }
}
