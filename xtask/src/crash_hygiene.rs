//! `xtask check-crash-hygiene` — SL-4.SHIP.01, ADR-P0017.
//!
//! A crash report is a diagnostic that leaves the user's machine, so the same
//! question `wild_hygiene` asks of the corpus applies to it with more force:
//! **where could document bytes get in, and what stops them?** The answer in
//! `selis-sandbox::crash` is a type — `Report` has no field a document can be
//! put in — plus a single egress that strips. A type is a good answer, but a
//! type nobody re-checks is a type that gets widened by someone in a hurry six
//! months from now. This gate is the re-check.
//!
//! Three mechanical properties, each of which fails loudly:
//!
//! 1. **The report type stays closed.** Every field of `Report` and `PanicFacts`
//!    must be a registry code, an integer, a `&'static str`, or the one
//!    length-bounded `String` (`site`, the reduced panic site). A new field of a
//!    byte-bearing type fails the gate with the field named, so widening the
//!    report is a deliberate act with a review trail rather than an accident.
//!
//! 2. **There is still no sink.** The reporter may not name a network, socket or
//!    HTTP API. A local ring with no egress is the honest scope while there is
//!    no consent UI and no endpoint; a reporter that grew an uploader would be a
//!    document-exfiltration channel, and this is the gate that would have caught
//!    it on the day.
//!
//! 3. **The consent default is `Denied`.** Parsed from the enum, not asserted in
//!    a comment. ADR-P0017 is explicit that crash reports sit behind a *separate*
//!    consent and that telemetry is off by default, so a reporter that defaults
//!    to recording is a policy regression whether or not anyone meant it.
//!
//! # Why a source scan and not a test
//!
//! The DoD is a runtime proof and it lives in `apps/cli/tests/crash_bytes.rs`:
//! a real parse of a real PDF, crashed on purpose, fingerprinted. This gate
//! answers the question the test cannot — *what if someone changes the type?* A
//! test only sees the fields that exist today; a source scan sees the ones that
//! were added. Both are needed, and neither is sufficient alone.

use std::path::Path;

/// The crash reporter's module, relative to the workspace root.
const CRASH_MODULE: &str = "crates/selis-sandbox/src/crash.rs";

/// The field types a report may hold.
///
/// Deliberately an allowlist, not a denylist. A denylist of "the bad types" has
/// to be updated every time Rust grows a way to hold a buffer, and the failure
/// mode of forgetting is a silent leak. An allowlist fails closed: a type nobody
/// thought about does not compile into a report.
const ALLOWED_FIELD_TYPES: &[&str] = &[
    "u64",
    "u32",
    "Code",
    "DocState",
    "&'static str",
    // The one owned string: the panic site, already reduced to a file name by
    // `PanicFacts::new` and stripped again by `Report::render`. It is allowed
    // because it is a source path reduced to a bare file name — never a
    // document path — and the DoD test asserts both halves of that claim.
    "String",
];

/// Field types that are banned outright, named in the failure message so the
/// reason is obvious without reading this file.
const BANNED_FIELD_TYPES: &[(&str, &str)] = &[
    ("&[u8]", "a byte slice is a document until proven otherwise"),
    (
        "Vec<u8>",
        "a byte buffer is a document until proven otherwise",
    ),
    (
        "&str",
        "a borrowed runtime string can be built from a document",
    ),
    ("Cow<str>", "a runtime string can be built from a document"),
    ("PathBuf", "a path names a person and a document"),
    ("Path", "a path names a person and a document"),
];

/// APIs that would give the reporter a way to send a report off the machine.
///
/// A local ring is the honest scope (ADR-P0016's cloud boundary does not exist
/// yet, and there is no consent UI to gate an upload with). A sink appearing
/// here is a design change, not a refactor, and it should cost a review.
const SINK_MARKERS: &[&str] = &[
    "reqwest",
    "ureq",
    "TcpStream",
    "UdpSocket",
    "std::net",
    "hyper::",
    "XMLHttpRequest",
    "fetch(",
    "sendBeacon",
    "https://",
    "http://",
];

/// The public check entry point.
pub fn check() -> Result<(), String> {
    check_at(Path::new("."))
}

/// Run the gate against an explicit workspace root. Split out so the unit tests
/// can point it at a fixture tree.
pub fn check_at(root: &Path) -> Result<(), String> {
    let path = root.join(CRASH_MODULE);
    let text = std::fs::read_to_string(&path)
        .map_err(|e| format!("cannot read {}: {e}", path.display()))?;

    let mut violations = Vec::new();
    violations.extend(check_field_types(&text));
    violations.extend(check_no_sink(&text));
    violations.extend(check_consent_default(&text));

    if violations.is_empty() {
        println!("check-crash-hygiene: report type closed, no sink, consent defaults to Denied");
        return Ok(());
    }
    Err(format!(
        "crash-report hygiene violations (ADR-P0017, SL-4.SHIP.01):\n  - {}",
        violations.join("\n  - ")
    ))
}

/// Property 1: every field of `Report` and `PanicFacts` is allowlisted.
fn check_field_types(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    for (struct_name, body) in structs(text) {
        for (field, ty) in fields(&body) {
            if let Some((_, why)) = BANNED_FIELD_TYPES.iter().find(|(b, _)| ty == *b) {
                out.push(format!(
                    "{CRASH_MODULE}: `{struct_name}.{field}` is `{ty}` — {why}"
                ));
                continue;
            }
            if !ALLOWED_FIELD_TYPES.contains(&ty.as_str()) {
                out.push(format!(
                    "{CRASH_MODULE}: `{struct_name}.{field}` has unallowlisted type \
                     `{ty}` — add it to ALLOWED_FIELD_TYPES only after deciding it \
                     cannot carry document bytes"
                ));
            }
        }
    }
    out
}

/// Extract `(name, body)` for each `struct Name { ... }` in the source.
///
/// A brace counter rather than a regex: the body must be found exactly, and a
/// regex over nested braces is how a gate ends up scanning the wrong lines.
fn structs(text: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let lines: Vec<&str> = text.lines().collect();
    let mut i = 0;
    while i < lines.len() {
        let trimmed = lines[i].trim();
        let rest = trimmed
            .strip_prefix("pub struct ")
            .or_else(|| trimmed.strip_prefix("struct "));
        let Some(rest) = rest else {
            i += 1;
            continue;
        };
        let name: String = rest
            .chars()
            .take_while(|c| c.is_alphanumeric() || *c == '_')
            .collect();
        if !trimmed.ends_with('{') {
            i += 1;
            continue;
        }
        let mut body = String::new();
        let mut depth = 1usize;
        i += 1;
        while i < lines.len() && depth > 0 {
            for c in lines[i].chars() {
                if c == '{' {
                    depth += 1;
                } else if c == '}' {
                    depth -= 1;
                }
            }
            if depth > 0 {
                body.push_str(lines[i]);
                body.push('\n');
            }
            i += 1;
        }
        out.push((name, body));
    }
    out
}

/// Extract `(field, type)` for each field declaration in a struct body.
fn fields(body: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for line in body.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("//") || trimmed.starts_with("#[") {
            continue;
        }
        let Some((name, ty)) = trimmed.split_once(':') else {
            continue;
        };
        let name = name.trim();
        if name.is_empty() || !name.chars().all(|c| c.is_alphanumeric() || c == '_') {
            continue;
        }
        let ty = ty.trim().trim_end_matches(',').trim();
        if !ty.is_empty() {
            out.push((name.to_owned(), ty.to_owned()));
        }
    }
    out
}

/// Property 2: no sink API is named in the reporter's code.
fn check_no_sink(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    for (idx, line) in text.lines().enumerate() {
        let trimmed = line.trim_start();
        // Doc comments *describe* the no-sink rule and must name the APIs they
        // are ruling out. A comment is not a call.
        if trimmed.starts_with("//") {
            continue;
        }
        for marker in SINK_MARKERS {
            if line.contains(marker) {
                out.push(format!(
                    "{CRASH_MODULE}:{}: names `{marker}` — the reporter has no sink by \
                     design; egress is the host's decision after the user sees the report",
                    idx + 1
                ));
            }
        }
    }
    out
}

/// Property 3: `Consent`'s `#[default]` variant is `Denied`.
fn check_consent_default(text: &str) -> Vec<String> {
    let Some(start) = text.find("pub enum Consent") else {
        return vec![format!(
            "{CRASH_MODULE}: no `Consent` enum found — ADR-P0017 requires crash reports \
             to sit behind a separate, opt-in consent"
        )];
    };
    let body = &text[start..];
    // Only the enum's own head, up to its closing brace, so a later `#[default]`
    // elsewhere in the file cannot be mistaken for this one.
    let end = body.find("\n}").map_or(body.len(), |i| i.saturating_add(2));
    let head = &body[..end];
    if head.contains("#[default]\n    Denied") {
        Vec::new()
    } else {
        vec![format!(
            "{CRASH_MODULE}: `Consent` does not default to `Denied` — telemetry and \
             crash reporting are off by default (ADR-P0017), and a crash report needs a \
             separate consent from the user"
        )]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The committed tree must pass. Anchored at the workspace root, because
    /// `cargo test` runs with the package root as CWD.
    #[test]
    fn committed_tree_passes() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("xtask lives in the workspace root");
        check_at(root).expect("check-crash-hygiene must pass on the committed tree");
    }

    /// Each property must be able to fail, or the gate is decoration. These
    /// plant the violation and require the named check to catch it.
    #[test]
    fn a_byte_field_on_the_report_is_flagged() {
        let text = "pub struct Report {\n    excerpt: Vec<u8>,\n    seq: u64,\n}\n";
        let violations = check_field_types(text);
        assert!(
            violations
                .iter()
                .any(|v| v.contains("excerpt") && v.contains("Vec<u8>")),
            "a byte buffer on Report must be flagged: {violations:?}"
        );
    }

    #[test]
    fn a_document_path_field_is_flagged() {
        let text = "pub struct PanicFacts {\n    document: PathBuf,\n}\n";
        let violations = check_field_types(text);
        assert!(
            violations
                .iter()
                .any(|v| v.contains("document") && v.contains("PathBuf")),
            "a path on PanicFacts must be flagged: {violations:?}"
        );
    }

    /// The allowlist must fail *closed*: a type nobody considered is refused
    /// rather than waved through.
    #[test]
    fn an_unconsidered_type_is_refused() {
        let text = "pub struct Report {\n    frames: Vec<BacktraceFrame>,\n}\n";
        let violations = check_field_types(text);
        assert!(
            violations.iter().any(|v| v.contains("unallowlisted")),
            "an unlisted type must be refused: {violations:?}"
        );
    }

    /// A doc comment naming a sink API is the rule being *described*, not broken.
    /// If this failed, the only way to document the policy would be to delete it.
    #[test]
    fn doc_comments_may_name_a_sink_api() {
        let text = "//! The reporter must not name `reqwest` or `TcpStream`.\npub struct A;\n";
        assert!(
            check_no_sink(text).is_empty(),
            "a doc comment naming a banned API is not a violation"
        );
    }

    #[test]
    fn code_naming_a_sink_api_is_flagged() {
        let text = "fn upload() {\n    let s = std::net::TcpStream::connect(\"h\");\n}\n";
        let violations = check_no_sink(text);
        assert!(!violations.is_empty(), "a real socket must be flagged");
        // The finding names the banned API and the line it is on -- the two
        // things a reviewer needs to act on it. It does not echo the full
        // module path, because the marker set is what matched.
        assert!(violations[0].contains("TcpStream"), "{violations:?}");
        assert!(violations[0].contains(":2:"), "{violations:?}");
    }

    #[test]
    fn a_consent_defaulting_to_granted_is_flagged() {
        let text = "pub enum Consent {\n    #[default]\n    Granted,\n    Denied,\n}\n";
        let violations = check_consent_default(text);
        assert!(
            violations.iter().any(|v| v.contains("Consent")),
            "a Granted default must be flagged: {violations:?}"
        );
    }

    #[test]
    fn a_consent_defaulting_to_denied_passes() {
        let text = "pub enum Consent {\n    #[default]\n    Denied,\n    Granted,\n}\n";
        assert!(check_consent_default(text).is_empty());
    }

    /// The brace-matched body must be the struct's own, or the gate scans the
    /// wrong lines and the allowlist means nothing.
    #[test]
    fn struct_bodies_are_brace_matched() {
        let text =
            "pub struct Report {\n    seq: u64,\n}\npub struct Other {\n    leaked: Vec<u8>,\n}\n";
        let found = structs(text);
        assert_eq!(found.len(), 2);
        assert_eq!(found[0].0, "Report");
        assert!(found[0].1.contains("seq"));
        assert!(
            !found[0].1.contains("leaked"),
            "body bled into the next struct"
        );
        let violations = check_field_types(text);
        assert!(
            violations.iter().any(|v| v.contains("leaked")),
            "the second struct must still be checked: {violations:?}"
        );
    }
}
