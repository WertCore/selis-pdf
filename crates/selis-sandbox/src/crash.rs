//! Crash reporting with document-byte stripping (SL-4.SHIP.01).
//!
//! ADR-P0017 requires that a crash report "strip document buffers" and that a
//! crash in a parser report *the code path, not the bytes*. This module is the
//! place that promise becomes a type, and the place a *binding* turns a caught
//! panic into something a human can read.
//!
//! It is deliberately not a telemetry channel. A [`Reporter`] has no network
//! client, no filesystem writer, and no way to name a sink: it accumulates
//! [`Report`]s in a fixed-size in-memory ring and hands them back to the host,
//! which decides what to do with them. That is the honest scope while there is
//! no consent UI and no endpoint (ADR-P0016's cloud boundary does not exist
//! yet). The alternative — a reporter with an uploader — would be a document
//! exfiltration channel with a `format!` in front of it.
//!
//! # Where the stripping happens
//!
//! **At capture, and then again at egress.** Both, deliberately, because they
//! fail in different ways and neither one alone is enough:
//!
//! * **Capture** is where the guarantee is *structural*. [`Report`] is a
//!   closed set of fields whose types cannot hold a document: no caller-filled
//!   `String`, no `Vec<u8>`, no `&[u8]`, no `Path`. There is no constructor that
//!   takes bytes, so no call site — including a future one written by someone in
//!   a hurry — can put a document in a report even by accident. The only
//!   borrowed strings are `&'static str`, which cannot be built from a document
//!   at runtime.
//! * **Egress** is where the guarantee is *re-checked*. [`Report::render`] is
//!   the only function that produces text from a report, and it passes every
//!   string field through [`strip`]. This is the belt to the capture braces: a
//!   field added to `Report` without thinking fails `xtask check-crash-hygiene`
//!   (see the gate's allowlist), and if one slips through, the render still
//!   reduces it to an allowlisted ASCII skeleton of bounded length.
//!
//! A payload that never leaves cannot leak, and the second property survives a
//! new call site that forgets to strip. Both are load-bearing here, which is
//! why both exist.
//!
//! **What `strip` is and is not.** It is a character-class and length filter —
//! the same policy as `Ctx::sanitise` and `selis_log::Value::sanitised` — so it
//! stops control bytes, forged line breaks, bidi overrides and unbounded values.
//! It is *not* a content filter: an ASCII word survives it, and a filter that
//! "recognised a name and removed it" would be guessing, not controlling. The
//! content defence is the closed field set; `strip` is the second line.
//!
//! # What a crash payload can actually contain
//!
//! Enumerated, because "strip document bytes" is only meaningful once you know
//! where a fragment could ride in.
//!
//! * **The panic message / payload** (`Box<dyn Any>`) — **yes, and it is the
//!   worst one.** A parser bug that formats a document slice into a `panic!`
//!   puts the bytes right here. Dropped **unread**, exactly as the ERR.03
//!   trampoline does. [`Reporter::record`] takes a [`PanicFacts`], never a
//!   payload, so there is no code path from payload to report at all.
//! * **The panic site** (`file:line:column`) — not document bytes, but the path
//!   is a filesystem location, and a build path leaks a username or a customer
//!   directory name. Kept, and reduced to the **file name only** at capture.
//! * **Stack frames** — symbol names cannot hold a document, but a demangled
//!   generic can be enormous, and `std::backtrace` is unavailable under the
//!   `wasm32` browser target this crate also builds for. **Not captured.**
//!   `Report` has no frame field; a partial backtrace is worse than none. When
//!   frames land, `check-crash-hygiene`'s allowlist must be extended first.
//! * **A `Range` header** (`bytes=0-65535`) — a byte count, not a byte, but it
//!   discloses document size, and size plus a code path is a fingerprint. No
//!   request context is recorded at all; `Report` has no field for it.
//! * **A URL, with or without a query** — a query string is a free-text field a
//!   caller can put anything in, and a signed URL is a bearer credential. No
//!   field; ADR-P0017 forbids URLs outright.
//! * **A buffer length** — a size oracle. No field.
//! * **A file path or name** — PII: `C:\Users\jane\Tax Returns\2025 Form.pdf`
//!   names a person and a document. No field. The panic site is a *source*
//!   path, not a document path, and is reduced to a file name.
//! * **An OS/environment string** (locale, hostname, argv) — not document
//!   bytes, but a fingerprint that links reports from one machine. No field,
//!   and `check-purity` means this crate cannot name `std::env` at all.
//!
//! What is left is worth keeping: the registry [`Code`], the code path
//! (`during`), the document state, the panic site, and a sequence number so a
//! user can say "the third crash" without the report identifying anything.
//!
//! # Consent
//!
//! [`Consent::Denied`] is the [`Default`]. A denied reporter retains nothing:
//! [`Reporter::record`] is a no-op and [`Reporter::reports`] is empty, so there
//! is no buffer to leak from and no way to enable it after the fact. Consent is
//! a property of the process, set once by the host at startup.
//!
//! A user changes it the way every other Selis setting changes: through the
//! host's own settings store, read at startup and passed to [`Reporter::new`].
//! The CLI reads `SELIS_CRASH_CONSENT` (a shell may name `std::env`; the kernel
//! may not, so the kernel takes the value, not the variable). A host with a
//! settings UI stores the same two states under its own key. There is
//! deliberately no in-process setter that flips consent after construction —
//! a crash reporter that can be talked into recording without consent is not a
//! consented reporter.
//!
//! # Budget
//!
//! The ring is [`Reporter::CAPACITY`] reports deep, and a full ring evicts the
//! oldest. A panic loop is a real failure mode (a fuzz target finding the same
//! panic across a corpus), and an unbounded ring would turn it into an
//! allocation storm inside a process that is already unhealthy.

use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

use selis_error::{Code, DocState};

/// The furthest a string in a report is ever reduced to.
///
/// Matches `Ctx::sanitise`'s bound. A crash report is a diagnostic, not a
/// transcript: past this length there is no triage value, only risk.
const MAX_FIELD_CHARS: usize = 64;

/// Reduce arbitrary text to the ASCII skeleton a report may carry.
///
/// Keeps ASCII alphanumerics plus `-`, `_`, `.` and `/`; replaces every other
/// byte with `?`; truncates. A PDF's own structural markers (`%%EOF`,
/// `/Type /Page`, `FlateDecode`) survive, because those are the strings that
/// make a crash report *useful* and they are markers, not content. Everything
/// else — where a document's prose or a user's name would be — does not.
///
/// This is the egress half of the guarantee, and it is deliberately the same
/// policy as `Ctx::sanitise` and `selis_log::Value::sanitised`: one policy,
/// three call sites, so a change to one is a change to all of them.
fn strip(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len().min(MAX_FIELD_CHARS));
    for c in raw.chars().take(MAX_FIELD_CHARS) {
        if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '/') {
            out.push(c);
        } else {
            out.push('?');
        }
    }
    out
}

/// Reduce a source path to its file name.
///
/// The panic site is the one path-shaped string a report carries, and it is a
/// *source* path (`crates/selis-pdf-cos/src/xref.rs`), not a document path. The
/// directory prefix is still dropped: a CI runner's workspace path and a
/// developer's home directory are both facts about a machine and a person, and
/// neither helps triage a panic. Handles both separators, because a `file!()`
/// on Windows contains backslashes.
///
/// The result borrows from the input, so a `&'static str` stays `&'static str`
/// and the capture path allocates nothing.
fn file_name_only(path: &str) -> &str {
    let cut = path.rfind(['/', '\\']).map_or(0, |i| i.saturating_add(1));
    &path[cut..]
}

/// Whether this process may retain crash reports at all.
///
/// Separate from any telemetry or cloud consent, and it must be: ADR-P0017 puts
/// crash reports behind "a separate consent" precisely because a crash report is
/// a different kind of artifact from a usage event. A user willing to share page
/// counts is not automatically a user willing to share a stack trace from their
/// machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Consent {
    /// Retain nothing. The default, and the only state a fresh install is in.
    #[default]
    Denied,
    /// Retain reports in the ring, for the host to show or forward.
    Granted,
}

/// What the trampoline knows about a panic, with the payload already discarded.
///
/// This is the *only* way to get panic information into a report. It is a plain
/// struct of engine-owned facts rather than a reference to the `Error` or to the
/// `Box<dyn Any>` payload:
///
/// * it has no caller-filled `String` and no byte slice;
/// * the trampoline's contract (ADR-P0017) is that the payload was dropped
///   unread, and this type makes honouring that contract a *compile* question —
///   there is no field to put a payload in.
///
/// [`PanicFacts::new`] is the constructor to use at a call site. It reduces the
/// source path to a file name, so the reduction cannot be forgotten.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PanicFacts {
    /// The registry code. `INTERNAL_PANIC` for a caught panic.
    code: Code,
    /// The engine-owned label for the code path, e.g. `"xref-stream"`.
    during: &'static str,
    /// `file:line:column` of the `panic!`, already reduced to a file name.
    site: &'static str,
}

impl PanicFacts {
    /// Build the facts, reducing the source path to its file name.
    ///
    /// `during` and `site` are `&'static str` on purpose: both are compile-time
    /// facts, and neither can be built from a document without a `format!` that
    /// this type gives no home to. A caller that genuinely needs a dynamic
    /// code-path label is holding something the report should not carry.
    #[must_use]
    pub fn new(code: Code, during: &'static str, site: &'static str) -> Self {
        Self {
            code,
            during,
            // `file!()` is a `&'static str`, so slicing it is free and the field
            // stays `&'static str`: no allocation, and no `String` in the type
            // for a document to hide in.
            site: file_name_only(site),
        }
    }

    /// The facts a trampoline recorded when no `site` was available.
    ///
    /// Used when the panic hook did not fire (a `panic!` inside a hook, or a
    /// `catch` around an already-typed error). Better an honest `site=(none)`
    /// than a fabricated one.
    #[must_use]
    pub const fn without_site(code: Code, during: &'static str) -> Self {
        Self {
            code,
            during,
            site: "",
        }
    }

    /// The registry code.
    #[must_use]
    pub const fn code(&self) -> Code {
        self.code
    }

    /// The code path label.
    #[must_use]
    pub const fn during(&self) -> &'static str {
        self.during
    }

    /// The panic site: `file:line:column`, file name only.
    #[must_use]
    pub const fn site(&self) -> &'static str {
        self.site
    }
}

/// One retained crash.
///
/// Every field is a registry code, a compile-time label, or a bounded string
/// that has been through [`strip`]. There is no field a caller can hand a
/// document to, which is the structural half of the guarantee — see the module
/// docs for why egress re-checks it anyway.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Report {
    /// 1-based, per-reporter. Lets a user say "the third one" without the
    /// report identifying anything about the document.
    seq: u64,
    /// The registry code.
    code: Code,
    /// The engine-owned code path label.
    during: &'static str,
    /// `file:line:column`, or empty when no site was recorded.
    site: String,
    /// What happened to the user's document, from the registry.
    doc_state: DocState,
}

impl Report {
    /// The 1-based sequence number of this report within its reporter.
    #[must_use]
    pub const fn seq(&self) -> u64 {
        self.seq
    }

    /// The registry code.
    #[must_use]
    pub const fn code(&self) -> Code {
        self.code
    }

    /// The code path label, e.g. `"xref-stream"`.
    #[must_use]
    pub const fn during(&self) -> &'static str {
        self.during
    }

    /// The panic site, or `""` when none was recorded.
    #[must_use]
    pub fn site(&self) -> &str {
        &self.site
    }

    /// What happened to the user's document.
    #[must_use]
    pub const fn doc_state(&self) -> DocState {
        self.doc_state
    }

    /// Render to the single-line, greppable form a support bundle carries.
    ///
    /// The only function in the workspace that turns a report into text, and the
    /// egress chokepoint: every string field goes through [`strip`] on the way
    /// out, so a field added to `Report` without thought is still reduced before
    /// it can reach a person or a network. The numeric fields need no stripping
    /// — a code id, a line number and a sequence number cannot be a document.
    #[must_use]
    pub fn render(&self) -> String {
        use fmt::Write as _;
        let mut s = String::with_capacity(160);
        let site = if self.site.is_empty() {
            "(none)".to_owned()
        } else {
            strip(&self.site)
        };
        // Every write targets a `String`, which cannot fail. `let _ =` keeps the
        // `Result` out of the signature without an `unwrap`, which the
        // workspace lint policy denies.
        let _ = write!(
            s,
            "CRASH seq={} code={} name={} during={} doc_state={} site={}",
            self.seq,
            self.code.id(),
            strip(self.code.name()),
            strip(self.during),
            self.doc_state,
            site,
        );
        s
    }
}

/// A bounded, in-process ring of crash reports.
///
/// No sink, no client, no writer. A host constructs one with the consent it read
/// at startup, hands it to its own crash path, and later calls
/// [`Reporter::reports`] to show the user what would be sent. Deciding where it
/// goes is the host's decision, made after the user has seen it — which is the
/// whole of ADR-P0017's "user-reviewable" posture, and the reason this type has
/// no `send` method.
#[derive(Debug)]
pub struct Reporter {
    consent: Consent,
    ring: Mutex<Vec<Report>>,
    /// Atomic because a reporter is shared: a WASM worker, a desktop process and
    /// a CLI all hand `&Reporter` to whichever thread caught the panic, and two
    /// threads panicking at once must not be handed the same sequence number.
    next_seq: AtomicU64,
}

impl Reporter {
    /// How many reports a reporter keeps.
    ///
    /// Small on purpose. A crash loop is a real failure mode (a fuzz target
    /// finding the same panic across a corpus of ten thousand files), and an
    /// unbounded ring would turn it into an allocation storm inside a process
    /// that is already unhealthy. Eight covers "the last few attempts" for a
    /// user reading a message and deciding what to do.
    pub const CAPACITY: usize = 8;

    /// A reporter that retains nothing unless consent says otherwise.
    #[must_use]
    pub fn new(consent: Consent) -> Self {
        Self {
            consent,
            ring: Mutex::new(Vec::new()),
            next_seq: AtomicU64::new(1),
        }
    }

    /// The consent this reporter was built with.
    #[must_use]
    pub const fn consent(&self) -> Consent {
        self.consent
    }

    /// Record a crash, if consented.
    ///
    /// Returns whether a report was retained — `false` when consent is denied,
    /// which is the default. A denied reporter does not allocate, does not
    /// count, and leaves no trace, so there is nothing to extract from the
    /// process afterwards.
    ///
    /// A full ring evicts the oldest report: dumping the newest crash is worth
    /// more than keeping the first of eight.
    pub fn record(&self, facts: &PanicFacts) -> bool {
        if self.consent == Consent::Denied {
            return false;
        }
        let Ok(mut ring) = self.ring.lock() else {
            // A panicking thread can poison the lock. Refusing to record is the
            // right failure: the alternative is panicking inside a panic
            // handler, which turns a recoverable bug into a lost process — the
            // exact outcome SL-0.ERR.03 exists to prevent.
            return false;
        };
        if ring.len() >= Self::CAPACITY {
            ring.remove(0);
        }
        let seq = self.next_seq.fetch_add(1, Ordering::Relaxed);
        ring.push(Report {
            seq,
            code: facts.code,
            during: facts.during,
            site: facts.site.to_owned(),
            doc_state: facts.code.doc_state(),
        });
        true
    }

    /// The retained reports, oldest first.
    ///
    /// Empty for a denied reporter, and empty after a crash inside the reporter
    /// itself (see [`Reporter::record`]). Never `None`: a caller that cannot
    /// distinguish "nothing crashed" from "crashing broke the reporter" is a
    /// caller that will guess, and a wrong guess here is a support ticket about
    /// the wrong bug.
    #[must_use]
    pub fn reports(&self) -> Vec<Report> {
        self.ring
            .lock()
            .map_or_else(|_| Vec::new(), |ring| ring.clone())
    }

    /// Drop every retained report.
    ///
    /// For a host that has shown them to the user and been told no. Without it,
    /// "I declined" would have to mean "the process exited", and a long-lived
    /// host (a browser tab, a desktop app) would keep the ring.
    pub fn clear(&self) {
        if let Ok(mut ring) = self.ring.lock() {
            ring.clear();
        }
    }
}

#[cfg(test)]
mod tests {
    // Fixture code only. These tests index their own freshly-built report
    // vectors and compare lengths to zero or one; none of it is reachable from
    // untrusted input, which is why `indexing_slicing` and `len_zero` are deny
    // lints in production. Deliberately scoped to this module — the reporter
    // itself below is unlinted-exempt.
    #![allow(clippy::indexing_slicing, clippy::len_zero)]

    use super::*;

    /// A realistic `file!()` from a Windows CI checkout — the directory prefix is
    /// the thing under test.
    fn facts_with_site() -> PanicFacts {
        PanicFacts::new(
            Code::InternalPanic,
            "xref-stream",
            r"C:\build\agent\work\selis\crates\selis-pdf-cos\src\xref.rs",
        )
    }

    /// Run `op` under the SL-0.ERR.03 trampoline and return the typed error it
    /// converted to.
    fn caught(op: impl FnOnce() -> Result<(), selis_error::Error>) -> selis_error::Error {
        crate::trampoline::catch("crash-test", op).expect_err("the operation must panic")
    }

    /// The structural half of the guarantee, asserted rather than promised.
    ///
    /// `PanicFacts` and `Report` take no `&[u8]`, no caller-filled `String`, and
    /// no `Path`, so there is no call — including a future one — that can hand
    /// them a document. This test is here so *widening* either type is a
    /// deliberate edit next to a test that says what the widening would break.
    #[test]
    fn capture_carries_the_useful_facts() {
        let reporter = Reporter::new(Consent::Granted);
        assert!(reporter.record(&facts_with_site()));
        let reports = reporter.reports();
        assert_eq!(reports.len(), 1);
        assert_eq!(reports[0].code(), Code::InternalPanic);
        assert_eq!(reports[0].during(), "xref-stream");
        assert_eq!(reports[0].doc_state(), DocState::Unchanged);
    }

    /// The panic site is the one path-shaped field, and a *source* path still
    /// carries the build machine's directory layout. Only the file name ships.
    #[test]
    fn site_is_reduced_to_a_file_name() {
        assert_eq!(file_name_only("/a/b/c/xref.rs"), "xref.rs");
        assert_eq!(file_name_only(r"C:\build\agent\xref.rs"), "xref.rs");
        assert_eq!(file_name_only("xref.rs"), "xref.rs");
        assert_eq!(file_name_only(""), "");

        let facts = PanicFacts::new(
            Code::InternalPanic,
            "parse",
            r"C:\Users\jane\Documents\selis\src\xref.rs",
        );
        assert_eq!(facts.site(), "xref.rs");
        assert!(!facts.site().contains("jane"));
        assert!(!facts.site().contains("Users"));
        assert!(!facts.site().contains('\\'));
    }

    /// SL-4.SHIP.01's DoD, at the level this crate can reach on its own: a panic
    /// whose payload is a document fragment, with every egress channel checked
    /// for that fragment.
    ///
    /// The engine-level proof — a real `Session::open` of a real fixture,
    /// fingerprinted against the file — lives in the CLI's `tests/crash_bytes.rs`
    /// integration test, because L1 must not depend on L3.
    #[test]
    fn no_document_bytes_in_any_channel() {
        const SECRET: &str = "SECRET-DOCUMENT-BYTES-9f3a1c";

        // A parser bug of exactly the shape ADR-P0017 warns about: the slice is
        // formatted into the panic message.
        fn hostile_parser() -> Result<(), selis_error::Error> {
            panic!("malformed token near {SECRET}");
        }

        let err = caught(hostile_parser);
        assert_eq!(err.code(), Code::InternalPanic);
        // The trampoline's own guarantee, asserted here because this is the
        // report's input: the payload never reached the error either.
        assert!(!err.to_log_line().contains(SECRET));

        let facts = PanicFacts::new(err.code(), "simulated-parse", "src/xref.rs");
        let reporter = Reporter::new(Consent::Granted);
        assert!(reporter.record(&facts));
        let report = reporter.reports().remove(0);

        // Every channel a host could read. None may carry the payload.
        let rendered = report.render();
        let debugged = format!("{report:?}");
        for channel in [&rendered, &debugged] {
            assert!(!channel.contains(SECRET), "payload leaked: {channel}");
            assert!(!channel.contains("DOCUMENT"), "payload leaked: {channel}");
            assert!(!channel.contains("9f3a1c"), "payload leaked: {channel}");
        }
        // And the report is still useful — a redactor that returned nothing
        // would pass this test and be worthless.
        assert!(rendered.contains("INTERNAL_PANIC"), "{rendered}");
        assert!(rendered.contains("during=simulated-parse"), "{rendered}");
        assert!(rendered.contains("site=xref.rs"), "{rendered}");
    }

    /// Consent denied is the default, and a denied reporter retains nothing —
    /// not a report, not a sequence number, not a buffer.
    #[test]
    fn denied_consent_is_the_default_and_retains_nothing() {
        assert_eq!(Consent::default(), Consent::Denied);

        let reporter = Reporter::new(Consent::default());
        assert!(!reporter.record(&facts_with_site()));
        assert!(reporter.reports().is_empty());

        // A denied reporter's sequence counter never advances, so a later grant
        // on a *new* reporter starts at 1 and cannot be used to count crashes
        // that were never retained.
        let granted = Reporter::new(Consent::Granted);
        assert!(granted.record(&facts_with_site()));
        assert_eq!(granted.reports()[0].seq(), 1);
    }

    /// The ring is bounded, and a panic loop cannot grow the process.
    #[test]
    fn ring_is_bounded_and_evicts_oldest() {
        let reporter = Reporter::new(Consent::Granted);
        let total = Reporter::CAPACITY + 4;
        for _ in 0..total {
            assert!(reporter.record(&facts_with_site()));
        }
        let reports = reporter.reports();
        assert_eq!(reports.len(), Reporter::CAPACITY);
        let seqs: Vec<u64> = reports.iter().map(Report::seq).collect();
        assert_eq!(seqs.first(), Some(&5), "oldest four evicted");
        assert_eq!(seqs.last(), Some(&(total as u64)));
    }

    #[test]
    fn clear_drops_everything() {
        let reporter = Reporter::new(Consent::Granted);
        assert!(reporter.record(&facts_with_site()));
        reporter.clear();
        assert!(reporter.reports().is_empty());
    }

    /// Egress stripping is the belt to the capture braces. Text that reaches
    /// `render` must come out reduced even if it was not reduced on the way in.
    ///
    /// Read the `Jane` case carefully: an ASCII word *survives*. `strip` is a
    /// character-class filter, exactly like `Ctx::sanitise` — it stops log
    /// injection, control bytes, and unbounded length, and it is **not** a
    /// content filter. Nothing that reaches `strip` is document content in the
    /// first place, because `Report` has no field a document can be put in; the
    /// filter is the second line, not the first. A `strip` that "found and
    /// removed" a name would mean it was guessing at content, and a guess is
    /// not a control.
    #[test]
    fn render_strips_hostile_text() {
        let out = strip("line\n1\x1b[31m/Title (Jane Q. Public) \u{202e}");
        // Control bytes and structure are gone: no log injection, no forged
        // line breaks, no bidi override to disguise what follows.
        assert!(!out.contains('\n'), "{out}");
        assert!(!out.contains('\x1b'), "{out}");
        assert!(!out.contains('\u{202e}'), "{out}");
        assert!(!out.contains(' '), "{out}");
        // The structural markers survive: they are what makes a report useful.
        assert!(
            strip("/Title").contains("Title"),
            "a /Name stays recognisable"
        );
        assert!(strip("FlateDecode").contains("FlateDecode"));
        assert!(strip("%%EOF").contains("EOF"));
        // And the length is bounded, so a long value cannot become a channel.
        assert!(strip(&"x".repeat(4096)).chars().count() <= MAX_FIELD_CHARS);
    }

    /// The claim the module docs make about capture, tested at the boundary
    /// that matters: a *second* egress channel added later must not be able to
    /// widen what a report holds. `Report`'s accessors return only borrowed
    /// engine-owned values and the already-bounded `site`, and `render` is the
    /// only owned text.
    #[test]
    fn report_exposes_no_channel_the_render_does_not_cover() {
        let reporter = Reporter::new(Consent::Granted);
        assert!(reporter.record(&facts_with_site()));
        let report = reporter.reports().remove(0);

        // Every string a host can read off a report, and the render is a pure
        // function of exactly these.
        let site = report.site();
        let during = report.during();
        let code_name = report.code().name();
        assert!(report.render().contains(site));
        assert!(report.render().contains(during));
        assert!(report.render().contains(code_name));
    }

    #[test]
    fn without_site_renders_honestly() {
        let reporter = Reporter::new(Consent::Granted);
        assert!(reporter.record(&PanicFacts::without_site(
            Code::InternalPanic,
            "render-page"
        )));
        let rendered = reporter.reports().remove(0).render();
        assert!(rendered.contains("site=(none)"), "{rendered}");
        assert!(rendered.contains("during=render-page"), "{rendered}");
    }
}
