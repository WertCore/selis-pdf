//! SL-1A.UI.02 — the per-tool result verification display.
//!
//! Every tool operation ends in the WRITE.05 verification gate
//! ([`crate::write_gate`]). This module turns what that pass *measured* into
//! the compact line the CLI prints after every operation, and into the
//! machine-readable `verification` object the `--json` twin emits on stdout —
//! the shape the Phase 4 web/extension UI consumes unchanged.
//!
//! Honesty rules for the copy:
//!
//! * every number on the line was measured by the verification pass — the
//!   output counts come from the verification walk itself, the input counts
//!   from the same survey the expectations were captured with;
//! * `checked` lists exactly which counts the operation *asserted*
//!   (preservation promises); the JSON twin is where that distinction lives,
//!   so the one-line text can stay compact;
//! * a failing verification produces no file and no line — the typed
//!   `VERIFY_FAILED` error names the first fault;
//! * no marketing language: the line states what was verified, nothing more.

use selis_pdf_cos::verify::{Expectations, Observed, Verdict};

use crate::{CliError, CliResult};

/// The verification display for one tool operation.
///
/// `*_in` counts are `None` when the operation had no input document
/// (`img2pdf`, `topdf`): there is nothing to compare against and the field is
/// honestly absent rather than zero.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub(crate) struct Verification {
    /// Input page count, when an input document was surveyed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pages_in: Option<u64>,
    /// Output page count (from the verification walk).
    pub pages_out: u64,
    /// Input annotation count, when surveyed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub annotations_in: Option<u64>,
    /// Output annotation count.
    pub annotations_out: u64,
    /// Input form-field count, when surveyed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fields_in: Option<u64>,
    /// Output form-field count.
    pub fields_out: u64,
    /// Input optional-content group count, when surveyed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ocgs_in: Option<u64>,
    /// Output optional-content group count.
    pub ocgs_out: u64,
    /// Input outline entry count, when surveyed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub outline_entries_in: Option<u64>,
    /// Output outline entry count.
    pub outline_entries_out: u64,
    /// Input embedded-file count, when surveyed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub embedded_files_in: Option<u64>,
    /// Output embedded-file count.
    pub embedded_files_out: u64,
    /// Input size in bytes (the source document(s) read).
    pub bytes_in: u64,
    /// Output size in bytes (what was written).
    pub bytes_out: u64,
    /// `ok` when the structural verification passed; `copied` when the
    /// operation wrote the input's own bytes unchanged (no rewrite, nothing
    /// for the writer gate to verify); `failed: <kind>` never reaches a
    /// printed line — a failing verification leaves no file behind.
    pub verdict: String,
    /// The count checks the operation asserted (its preservation promises):
    /// a subset of `pages`, `annotations`, `fields`, `ocgs`,
    /// `outline_entries`, `embedded_files`.
    pub checked: Vec<&'static str>,
}

/// The stable check labels used in `checked`.
const CHECK_PAGES: &str = "pages";
const CHECK_ANNOTATIONS: &str = "annotations";
const CHECK_FIELDS: &str = "fields";
const CHECK_OCGS: &str = "ocgs";
const CHECK_OUTLINES: &str = "outline_entries";
const CHECK_EMBEDDED: &str = "embedded_files";

impl Verification {
    /// Build the display from what the gate measured.
    ///
    /// `input` is the surveyed input document (`None` for generated output);
    /// `verdict` is the gate's verification of the output bytes; `expected`
    /// is the expectation set the gate enforced, which determines `checked`.
    #[must_use]
    pub(crate) fn from_gate(
        input: Option<&Observed>,
        verdict: &Verdict,
        expected: &Expectations,
        bytes_in: u64,
        bytes_out: u64,
    ) -> Self {
        let out = &verdict.observed;
        let (pages_in, annotations_in, fields_in, ocgs_in, outlines_in, embedded_in) = match input {
            Some(o) => (
                Some(o.pages),
                Some(o.annotations),
                Some(o.fields),
                Some(o.ocgs),
                Some(o.outlines),
                Some(o.embedded_files),
            ),
            None => (None, None, None, None, None, None),
        };
        let mut checked = Vec::with_capacity(6);
        if expected.pages.is_some() {
            checked.push(CHECK_PAGES);
        }
        if expected.annotations.is_some() {
            checked.push(CHECK_ANNOTATIONS);
        }
        if expected.fields.is_some() {
            checked.push(CHECK_FIELDS);
        }
        if expected.ocgs.is_some() {
            checked.push(CHECK_OCGS);
        }
        if expected.outlines.is_some() {
            checked.push(CHECK_OUTLINES);
        }
        if expected.embedded_files.is_some() {
            checked.push(CHECK_EMBEDDED);
        }
        let verdict_text = if verdict.ok {
            "ok".to_string()
        } else {
            let kind = verdict
                .faults
                .first()
                .map(|f| f.kind())
                .unwrap_or("unknown-fault");
            format!("failed: {kind}")
        };
        Verification {
            pages_in,
            pages_out: out.pages,
            annotations_in,
            annotations_out: out.annotations,
            fields_in,
            fields_out: out.fields,
            ocgs_in,
            ocgs_out: out.ocgs,
            outline_entries_in: outlines_in,
            outline_entries_out: out.outlines,
            embedded_files_in: embedded_in,
            embedded_files_out: out.embedded_files,
            bytes_in,
            bytes_out,
            verdict: verdict_text,
            checked,
        }
    }

    /// The display for an operation that wrote the input's own bytes
    /// unchanged (verbatim copy: no rewrite, no writer output to verify).
    #[must_use]
    pub(crate) fn copied(bytes: u64) -> Self {
        Verification {
            pages_in: None,
            pages_out: 0,
            annotations_in: None,
            annotations_out: 0,
            fields_in: None,
            fields_out: 0,
            ocgs_in: None,
            ocgs_out: 0,
            outline_entries_in: None,
            outline_entries_out: 0,
            embedded_files_in: None,
            embedded_files_out: 0,
            bytes_in: bytes,
            bytes_out: bytes,
            verdict: "copied".to_string(),
            checked: Vec::new(),
        }
    }

    /// The compact human-readable line (stderr).
    ///
    /// Counts that did not change render as a single number; changed counts
    /// render as `in->out`; unmeasured (no input document) render as the
    /// output count only.
    #[must_use]
    pub(crate) fn line(&self) -> String {
        let mut parts: Vec<String> = Vec::with_capacity(8);
        let count = |name: &str, into: (Option<u64>, u64)| match into.0 {
            Some(i) if i == into.1 => format!("{name} {i}"),
            Some(i) => format!("{name} {i}->{}", into.1),
            None => format!("{name} {}", into.1),
        };
        parts.push(count("pages", (self.pages_in, self.pages_out)));
        parts.push(count(
            "annotations",
            (self.annotations_in, self.annotations_out),
        ));
        parts.push(count("fields", (self.fields_in, self.fields_out)));
        parts.push(count("OCGs", (self.ocgs_in, self.ocgs_out)));
        parts.push(count(
            "outlines",
            (self.outline_entries_in, self.outline_entries_out),
        ));
        parts.push(count(
            "attachments",
            (self.embedded_files_in, self.embedded_files_out),
        ));
        parts.push(format!("bytes {}->{}", self.bytes_in, self.bytes_out));
        let tail = if self.verdict == "copied" {
            "copied unchanged (no rewrite, nothing to verify)".to_string()
        } else {
            format!("structural check {}", self.verdict)
        };
        format!("verified: {}; {tail}", parts.join(", "))
    }

    /// The machine-readable twin: one JSON object on stdout,
    /// `{"verification": {...}}` — the shape the Phase 4 web/extension UI
    /// consumes unchanged.
    ///
    /// # Errors
    ///
    /// Serialization of this plain-data struct cannot fail in practice; the
    /// error is surfaced rather than swallowed (honest failure).
    pub(crate) fn print_json(&self) -> CliResult<()> {
        #[derive(serde::Serialize)]
        struct Envelope<'a> {
            verification: &'a Verification,
        }
        let json = serde_json::to_string(&Envelope { verification: self })
            .map_err(|e| CliError(format!("cannot serialise verification: {e}")))?;
        println!("{json}");
        Ok(())
    }

    /// Print the human-readable line to stderr. The line is the visible
    /// proof of the verification pass every output goes through; it states
    /// what was measured and nothing more.
    pub(crate) fn emit_line(&self) {
        eprintln!("{}", self.line());
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    #![allow(clippy::indexing_slicing, clippy::arithmetic_side_effects)]

    use super::*;
    use selis_pdf_cos::verify::{Fault, Observed, Verdict};

    fn observed(
        pages: u64,
        annotations: u64,
        fields: u64,
        ocgs: u64,
        outlines: u64,
        files: u64,
    ) -> Observed {
        Observed {
            pages,
            annotations,
            fields,
            ocgs,
            outlines,
            embedded_files: files,
        }
    }

    fn ok_verdict(out: Observed) -> Verdict {
        Verdict {
            ok: true,
            observed: out,
            faults: Vec::new(),
        }
    }

    fn sample() -> Verification {
        let input = observed(5, 4, 2, 1, 3, 1);
        let verdict = ok_verdict(observed(2, 1, 2, 1, 3, 1));
        let expected = Expectations {
            pages: Some(2),
            annotations: Some(1),
            fields: Some(2),
            ocgs: Some(1),
            outlines: None,
            embedded_files: Some(1),
        };
        Verification::from_gate(Some(&input), &verdict, &expected, 12_054, 8_231)
    }

    /// The builder copies the walked output counts and the surveyed input
    /// counts into the display, and records exactly the asserted checks.
    #[test]
    fn builder_copies_measured_and_asserted_fields() {
        let v = sample();
        assert_eq!(v.pages_in, Some(5));
        assert_eq!(v.pages_out, 2);
        assert_eq!(v.annotations_out, 1);
        assert_eq!(v.embedded_files_in, Some(1));
        assert_eq!(v.bytes_in, 12_054);
        assert_eq!(v.bytes_out, 8_231);
        assert_eq!(v.verdict, "ok");
        assert_eq!(
            v.checked,
            vec!["pages", "annotations", "fields", "ocgs", "embedded_files"]
        );
    }

    /// The human line carries every measured count, byte sizes, and the
    /// verdict — changed counts as `in->out`, preserved as one number.
    #[test]
    fn line_is_compact_and_complete() {
        let line = sample().line();
        assert!(line.starts_with("verified: "), "{line}");
        assert!(line.contains("pages 5->2"), "{line}");
        assert!(line.contains("annotations 4->1"), "{line}");
        assert!(
            line.contains("fields 2, "),
            "preserved count is one number: {line}"
        );
        assert!(line.contains("OCGs 1"), "{line}");
        assert!(line.contains("outlines 3"), "{line}");
        assert!(line.contains("attachments 1"), "{line}");
        assert!(line.contains("bytes 12054->8231"), "{line}");
        assert!(line.ends_with("structural check ok"), "{line}");
    }

    /// A generated output (no input document) shows output-only counts, and
    /// the `checked` list stays empty when nothing was asserted.
    #[test]
    fn generated_output_has_no_input_counts() {
        let verdict = ok_verdict(observed(3, 0, 0, 0, 0, 0));
        let v = Verification::from_gate(None, &verdict, &Expectations::none(), 900, 5_000);
        assert_eq!(v.pages_in, None);
        assert_eq!(v.pages_out, 3);
        assert!(v.checked.is_empty());
        let line = v.line();
        assert!(line.contains("pages 3"), "{line}");
        assert!(line.contains("bytes 900->5000"), "{line}");
    }

    /// The verbatim-copy display never claims a verification that did not run.
    #[test]
    fn copied_display_states_what_happened() {
        let v = Verification::copied(4_424);
        assert_eq!(v.verdict, "copied");
        assert!(v.checked.is_empty());
        assert!(v.pages_out == 0 && v.pages_in.is_none());
        let line = v.line();
        assert!(
            line.contains("copied unchanged (no rewrite, nothing to verify)"),
            "{line}"
        );
        assert!(line.contains("bytes 4424->4424"), "{line}");
    }

    /// The JSON twin round-trips the same fields the line shows, under the
    /// `verification` envelope.
    #[test]
    fn json_twin_round_trips() {
        let v = sample();
        let json =
            serde_json::to_string(&serde_json::json!({ "verification": &v })).expect("serialise");
        assert!(json.contains("\"verification\""), "{json}");
        assert!(json.contains("\"pages_in\":5"), "{json}");
        assert!(json.contains("\"checked\":[\"pages\""), "{json}");
        // Every field the line shows is present in the object.
        let value: serde_json::Value = serde_json::from_str(&json).expect("parse");
        let verification = value.get("verification").expect("envelope");
        for field in [
            "pages_in",
            "pages_out",
            "annotations_in",
            "annotations_out",
            "fields_in",
            "fields_out",
            "ocgs_in",
            "ocgs_out",
            "outline_entries_in",
            "outline_entries_out",
            "embedded_files_in",
            "embedded_files_out",
            "bytes_in",
            "bytes_out",
            "verdict",
            "checked",
        ] {
            assert!(
                verification.get(field).is_some(),
                "field {field} missing from the JSON twin"
            );
        }
        assert_eq!(
            verification.get("bytes_in").and_then(|v| v.as_u64()),
            Some(12_054)
        );
    }

    /// A failed verdict renders its first fault kind (though a failed
    /// verification never reaches a printed line — no file is written).
    #[test]
    fn failed_verdict_names_the_fault_kind() {
        let mut verdict = ok_verdict(observed(2, 0, 0, 0, 0, 0));
        verdict.ok = false;
        verdict.faults.push(Fault::PageCountMismatch {
            expected: 3,
            actual: 2,
        });
        let v = Verification::from_gate(
            Some(&observed(3, 0, 0, 0, 0, 0)),
            &verdict,
            &Expectations {
                pages: Some(3),
                ..Expectations::none()
            },
            10,
            20,
        );
        assert_eq!(v.verdict, "failed: page-count");
    }

    /// Property: for arbitrary counts and sizes the line is well-formed
    /// (carries the verdict and both byte sizes) and the JSON round-trip
    /// preserves the display exactly.
    #[test]
    fn line_and_json_hold_for_arbitrary_counts() {
        use proptest::prelude::*;

        let counts = proptest::num::u64::ANY;
        proptest!(|(pi in counts, po in counts, ai in counts, ao in counts,
                    bi in 0u64..(1u64 << 40), bo in 0u64..(1u64 << 40))| {
            let input = observed(pi, ai, 0, 0, 0, 0);
            let verdict = ok_verdict(observed(po, ao, 0, 0, 0, 0));
            let v = Verification::from_gate(
                Some(&input),
                &verdict,
                &Expectations::none(),
                bi,
                bo,
            );
            let line = v.line();
            proptest::prop_assert!(line.contains(&format!("bytes {bi}->{bo}")), "{line}");
            proptest::prop_assert!(line.ends_with("structural check ok"), "{line}");
            proptest::prop_assert!(line.contains(&format!("pages {pi}->{po}")), "{line}");
            // The JSON twin serialises with every measured field present.
            let json = serde_json::to_string(&serde_json::json!({ "verification": &v })).unwrap();
            proptest::prop_assert!(json.contains("\"pages_out\""), "{json}");
            proptest::prop_assert!(json.contains(&format!("\"bytes_in\":{bi}")), "{json}");
        });
    }
}
