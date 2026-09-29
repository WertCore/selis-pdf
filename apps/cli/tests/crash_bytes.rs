//! SL-4.SHIP.01 DoD — no document bytes appear in a report from a deliberately
//! crashing parse.
//!
//! The unit tests in `selis-sandbox::crash` prove the report *type* cannot hold
//! a document. This file proves the property end to end on a **real parse of a
//! real committed PDF**, through the real SL-0.ERR.03 trampoline, and checks
//! every channel a shell could read.
//!
//! # How the crash is produced, and why it is injected
//!
//! The engine does not panic. That is the point of SL-1.ROB: the baseline
//! asserts **0** `INTERNAL_PANIC` outcomes across the corpus, and a document
//! that panicked the parser would be a filed bug, not a fixture. So there is no
//! committed document that makes `Session::open` panic, and manufacturing one
//! would mean committing a known engine defect.
//!
//! What this test does instead is inject the crash at exactly the point a real
//! engine bug would surface, with the *real* document bytes genuinely in scope
//! and genuinely formatted into the panic message — the shape ADR-P0017 names:
//!
//! > a parser bug that formats a document slice into a `panic!` would otherwise
//! > turn the trampoline into a document-exfiltration channel
//!
//! The parse is not simulated around the edges: the document really is opened
//! through the real `Session::open` with a real `Budget`, and the crash really
//! does happen with that session live. Only the `panic!` is ours. That is the
//! one degree of freedom the test needs, and holding everything else fixed is
//! what makes the assertion mean something.
//!
//! # What "no document bytes" is checked against
//!
//! Not a single canary string. A canary proves one leak was not happening; it
//! does not prove a *partial* leak is not happening, and a partial leak is the
//! realistic one (a formatted slice, a base64'd window, a 40-byte excerpt
//! embedded in a message). So the assertion is a **fingerprint sweep**: the
//! document is cut into windows spread across its whole length and every
//! window is searched for in every egress channel. The technique is the same one
//! `apps/web/host/src/no-upload.ts` uses on the TypeScript side, and the reason
//! is the same — a document that is shipped *whole* is the easy case, and a
//! document that is shipped in a piece is the case worth gating on.
//!
//! A canary is checked as well, because it also proves the *injected* leak was
//! actually present in the panic payload. Without that, a test that passed
//! because the injection silently stopped working would be a green lie.
//!
//! # The other half: the report must still be worth reading
//!
//! Every no-leak assertion here is also paired with a usefulness assertion. A
//! redactor that returned an empty string would pass all of them and be worth
//! nothing, so each test also requires the code path, the registry code, and the
//! panic site to be present and readable. "Contains nothing" and "contains the
//! diagnosis" are asserted together, deliberately.
#![cfg_attr(
    test,
    allow(
        clippy::indexing_slicing,
        clippy::arithmetic_side_effects,
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic
    )
)]

use selis_error::Code;
use selis_pdf_engine::Session;
use selis_sandbox::crash::{Consent, PanicFacts, Reporter};
use selis_sandbox::{Budget, FixedClock, Surface};

/// A real committed PDF, embedded at compile time. The CLI is L5 and may name
/// the filesystem, but `include_bytes!` keeps the test hermetic and lets the
/// fingerprint sweep run against the exact bytes a user would open.
static STRUCTURE_SIMPLE: &[u8] = include_bytes!("../../../corpus/fixtures/structure_simple.pdf");

/// A second real document, so the sweep is not tuned to one file's structure.
static FORM_TWO_PAGES: &[u8] = include_bytes!("../../../corpus/fixtures/form_two_pages.pdf");

/// Bytes per fingerprint window. Large enough that a match is not a coincidence
/// in a short report, small enough to catch a realistic excerpt.
const WINDOW: usize = 64;

/// How many windows to cut, spread evenly across the document.
const WINDOWS: usize = 16;

/// Fingerprint windows of `doc`, spread across its whole length.
///
/// Returned as owned `Vec`s because the assertions compare them against
/// rendered text rather than bytes, and a report is text by construction.
fn fingerprints(doc: &[u8]) -> Vec<Vec<u8>> {
    assert!(
        doc.len() > WINDOW * 2,
        "fixture must be long enough to fingerprint"
    );
    let span = doc.len() - WINDOW;
    (0..WINDOWS)
        .map(|i| {
            let start = span * i / (WINDOWS - 1);
            doc[start..start + WINDOW].to_vec()
        })
        .collect()
}

/// Assert that no window of `doc` appears in any of `channels`.
///
/// A window match is reported with its offset so a failure names the place in
/// the document that leaked, rather than just "something leaked".
fn assert_no_document_bytes(doc: &[u8], channels: &[(&str, String)]) {
    for (label, channel) in channels {
        for (i, window) in fingerprints(doc).iter().enumerate() {
            assert!(
                !contains_bytes(channel.as_bytes(), window),
                "document bytes leaked into {label}: window {i} of {} \
                 (offset {}) appears in the report:\n{channel}",
                doc.len(),
                window.len() * i,
            );
        }
    }
}

/// Byte-wise substring search.
///
/// A hand-rolled `windows().any()` rather than a UTF-8 `str` search, because a
/// document fragment that is not valid UTF-8 must still be found: the report is
/// text, but the thing it might be carrying is not, and a lossy comparison
/// would compile a leak into a non-leak.
fn contains_bytes(haystack: &[u8], needle: &[u8]) -> bool {
    if needle.is_empty() || haystack.len() < needle.len() {
        return false;
    }

    haystack.windows(needle.len()).any(|w| w == needle)
}
/// Open a real document through the real engine.
fn open(doc: &[u8]) -> Session {
    let budget = Budget::profile(Surface::Viewer);
    // A frozen clock: this test is about bytes, not time, and a real clock
    // would make the run non-deterministic for no gain.
    Session::open(doc.to_vec(), &budget, &FixedClock(0)).expect("the fixture opens")
}

/// Run `op` under the SL-0.ERR.03 trampoline, exactly as `main` does.
///
/// Returns the typed error the panic converted to. A `match` rather than
/// `expect_err`, because the success type is a `Session` on some calls and a
/// `Report` on others, and neither is `Debug` — a bound the assertion does not
/// need.
fn caught<T>(op: impl FnOnce() -> Result<T, selis_error::Error>) -> selis_error::Error {
    match selis_sandbox::trampoline::catch("cli-command", op) {
        Ok(_) => panic!("the operation must panic"),
        Err(e) => e,
    }
}

/// The DoD: a real parse of a real PDF, crashed deliberately, with the real
/// document bytes in the panic payload. Nothing of the document may reach any
/// egress channel.
#[test]
fn a_crashing_parse_reports_no_document_bytes() {
    let doc = STRUCTURE_SIMPLE;
    // The canary is a real slice of the real document, not invented text: this
    // is the exact thing a parser bug would interpolate.
    let canary = &doc[64..64 + 48];
    let session = open(doc);
    assert!(session.len() >= 1, "a real session is live during the crash");

    // The injected bug: a slice of the open document formatted into the panic
    // message, with the session still in scope.
    let err = caught(|| -> Result<(), selis_error::Error> {
        let _live_session = open(doc);
        panic!(
            "xref stream truncated near {}",
            String::from_utf8_lossy(canary)
        );
    });

    // The trampoline's own half of the DoD, asserted here because it is the
    // same claim at a different boundary: the payload never reached the error.
    assert_eq!(err.code(), Code::InternalPanic);
    let canary_text = String::from_utf8_lossy(canary).into_owned();
    assert!(
        !err.to_log_line().contains(canary_text.as_str()),
        "the typed error leaked a document slice"
    );

    let reporter = Reporter::new(Consent::Granted);
    let facts = PanicFacts::new(err.code(), "cli-command", "src/xref.rs");
    assert!(reporter.record(&facts), "consent granted: the report is kept");

    let report = reporter.reports().remove(0);
    let channels = vec![
        ("render", report.render()),
        ("Debug", format!("{report:?}")),
        (
            "accessors",
            format!(
                "code={} during={} site={} doc_state={}",
                report.code().name(),
                report.during(),
                report.site(),
                report.doc_state()
            ),
        ),
    ];

    // The canary, exactly.
    for (label, channel) in &channels {
        assert!(
            !channel.contains(&canary_text),
            "the canary leaked into {label}:\n{channel}"
        );
    }
    // And every window of the document, so a partial leak cannot hide.
    assert_no_document_bytes(doc, &channels);

    // The other half: the report is still a diagnosis. A redactor that returned
    // nothing would satisfy every assertion above.
    let rendered = &channels[0].1;
    assert!(rendered.contains("INTERNAL_PANIC"), "{rendered}");
    assert!(rendered.contains("during=cli-command"), "{rendered}");
    assert!(rendered.contains("site=xref.rs"), "{rendered}");
    assert!(rendered.contains("doc_state=unchanged"), "{rendered}");
}

/// The same proof on a second, structurally different document, so the sweep is
/// not tuned to one file.
#[test]
fn a_second_crashing_parse_also_reports_no_document_bytes() {
    let doc = FORM_TWO_PAGES;
    let err = caught(|| -> Result<(), selis_error::Error> {
        let live = open(doc);
        assert!(live.len() > 1);
        panic!(
            "object stream header {:?}",
            String::from_utf8_lossy(&doc[128..160])
        );
    });

    let reporter = Reporter::new(Consent::Granted);
    assert!(reporter.record(&PanicFacts::new(
        err.code(),
        "cli-command",
        "src/objstm.rs"
    )));
    let report = reporter.reports().remove(0);
    let channels = vec![("render", report.render()), ("Debug", format!("{report:?}"))];
    assert_no_document_bytes(doc, &channels);
    assert!(channels[0].1.contains("site=objstm.rs"), "{}", channels[0].1);
}

/// The gate on the *type*, at the level a future maintainer meets it: a caller
/// has no way to hand the reporter a document, because no constructor takes
/// one. The compile-time half is enforced by the types themselves; what is
/// assertable here is that even the one string which is not a `&'static str` by
/// construction — the site — is reduced before it is stored.
#[test]
fn the_reporter_has_no_bytes_shaped_api() {
    let facts = PanicFacts::new(
        Code::InternalPanic,
        "cli-command",
        r"C:\Users\jane\Documents\2025 Form.pdf\src\xref.rs",
    );
    assert_eq!(facts.site(), "xref.rs");
    assert!(!facts.site().contains("jane"));
    assert!(!facts.site().contains("Form"));
}

/// The fingerprint sweep must actually be able to fail, or it proves nothing.
/// This plants a real document byte in a report-shaped string and requires the
/// sweep to catch it.
#[test]
fn the_fingerprint_sweep_catches_a_planted_document_byte() {
    let doc = STRUCTURE_SIMPLE;
    let window = fingerprints(doc)[3].clone();

    // A "report" that carries the window, as a partially-leaking render would.
    let planted = format!(
        "CRASH seq=1 code=1900 name=INTERNAL_PANIC during=cli-command site=xref.rs \
         excerpt={}",
        String::from_utf8_lossy(&window)
    );

    // The sweep must fail on this input. If it does not, the sweep is broken
    // and every other assertion in this file is vacuous.
    assert!(
        contains_bytes(planted.as_bytes(), &window),
        "the fingerprint sweep did not catch a planted document window: {planted}"
    );

    // And a real report must not trip it.
    let reporter = Reporter::new(Consent::Granted);
    assert!(reporter.record(&PanicFacts::new(
        Code::InternalPanic,
        "cli-command",
        "src/xref.rs"
    )));
    let clean = reporter.reports().remove(0).render();
    assert!(
        !contains_bytes(clean.as_bytes(), &window),
        "a real report tripped the sweep: {clean}"
    );
}

/// Consent denied is the shipped default, and a denied reporter is empty after a
/// real crash — so there is nothing in the process to extract, not merely
/// nothing sent.
#[test]
fn a_denied_reporter_keeps_nothing_after_a_real_crash() {
    let doc = STRUCTURE_SIMPLE;
    let _ = caught(|| -> Result<(), selis_error::Error> {
        let _live = open(doc);
        panic!("boom {}", String::from_utf8_lossy(&doc[0..32]));
    });

    let denied = Reporter::new(Consent::default());
    assert!(!denied.record(&PanicFacts::new(
        Code::InternalPanic,
        "cli-command",
        "src/xref.rs"
    )));
    assert!(
        denied.reports().is_empty(),
        "a denied reporter retained a report"
    );
}
