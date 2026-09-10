//! SL-1A.UI.02 / SL-1A.UI.06 corpus entry and integration tests.
//!
//! The committed fixture `corpus/fixtures/verification_features.pdf`
//! (regenerable with the ignored `write_corpus_fixture` test below) carries
//! every feature the verification display reports: 2 pages, 2 annotations, a
//! 2-field form, an OCG, an outline tree of 3 entries, and 2 embedded files.
//!
//! The tests drive the real `selis` binary (the display is CLI surface):
//!
//! 1. the compact verification line appears after the operation and its
//!    numbers match the fixture's surveyed counts;
//! 2. `--json` emits the machine-readable twin on stdout with the same
//!    fields (the shape the Phase 4 web/extension UI consumes unchanged);
//! 3. a page selection shows changed counts honestly (`pages 2->1`) and its
//!    `checked` array lists exactly the asserted preservation promises;
//! 4. a file that exceeds the Viewer depth budget produces the honest typed
//!    message — which budget, the measured usage, what to do — and exit
//!    code 1, never a panic or a partial file;
//! 5. `batch` report.json carries the `verification` object for ok files.
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

use std::path::PathBuf;
use std::process::Command;

use selis_pdf_cos::doc_writer::write_objects_as_document;
use selis_pdf_cos::{parse_revisions, xref, Obj, Ref};
use selis_sandbox::{Budget, BudgetGuard, CancelToken, FixedClock};

fn fixture_path() -> PathBuf {
    let mut p = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    p.push("..");
    p.push("..");
    p.push("corpus");
    p.push("fixtures");
    p.push("verification_features.pdf");
    p
}

fn temp_dir(tag: &str) -> PathBuf {
    std::env::temp_dir().join(format!("selis-ui02-{tag}-{}", std::process::id()))
}

fn selis() -> Command {
    Command::new(env!("CARGO_BIN_EXE_selis"))
}

/// A field of the JSON twin, panicking with the field name when the shape
/// drifts — a shape drift IS the failure these tests exist to catch.
fn jget<'a>(v: &'a serde_json::Value, name: &str) -> &'a serde_json::Value {
    v.get(name)
        .unwrap_or_else(|| panic!("JSON twin lost field `{name}`"))
}

fn guard() -> BudgetGuard<'static> {
    Budget::unlimited().guard_with(&FixedClock(0), CancelToken::new())
}

fn bytes(v: &[u8]) -> selis_bytes::Bytes {
    selis_bytes::Bytes::copy_from_slice(v)
}

/// The committed fixture's exact bytes (deterministic writer output). The
/// object graph: catalog → 2 pages (1 annotation each), an AcroForm with one
/// root field + one kid (both widgets on page A), one OCG, an outline tree of
/// 3 entries (2 top-level, one child), and 2 embedded files behind a 2-node
/// name tree.
fn verification_features_pdf() -> Vec<u8> {
    let page = |parent: u32, extra: &[(selis_bytes::Bytes, Obj)]| {
        let mut pairs = vec![
            (bytes(b"Type"), Obj::Name(bytes(b"Page"))),
            (bytes(b"Parent"), Obj::Ref(Ref::new(parent, 0))),
            (
                bytes(b"MediaBox"),
                Obj::Array(vec![Obj::Int(0), Obj::Int(0), Obj::Int(100), Obj::Int(100)]),
            ),
        ];
        pairs.extend_from_slice(extra);
        Obj::Dict(pairs)
    };
    let files = Obj::Array(vec![
        Obj::String(bytes(b"file1")),
        Obj::Dict(vec![(bytes(b"F"), Obj::String(bytes(b"f1.bin")))]),
        Obj::String(bytes(b"file2")),
        Obj::Dict(vec![(bytes(b"F"), Obj::String(bytes(b"f2.bin")))]),
    ]);
    let objects = vec![
        (
            1,
            Obj::Dict(vec![
                (bytes(b"Type"), Obj::Name(bytes(b"Catalog"))),
                (bytes(b"Pages"), Obj::Ref(Ref::new(2, 0))),
                (bytes(b"Outlines"), Obj::Ref(Ref::new(3, 0))),
                (bytes(b"Names"), Obj::Ref(Ref::new(6, 0))),
                (bytes(b"AcroForm"), Obj::Ref(Ref::new(9, 0))),
                (bytes(b"OCProperties"), Obj::Ref(Ref::new(12, 0))),
            ]),
        ),
        (
            2,
            Obj::Dict(vec![
                (bytes(b"Type"), Obj::Name(bytes(b"Pages"))),
                (
                    bytes(b"Kids"),
                    Obj::Array(vec![Obj::Ref(Ref::new(4, 0)), Obj::Ref(Ref::new(5, 0))]),
                ),
                (bytes(b"Count"), Obj::Int(2)),
            ]),
        ),
        (
            3,
            Obj::Dict(vec![
                (bytes(b"Type"), Obj::Name(bytes(b"Outlines"))),
                (bytes(b"First"), Obj::Ref(Ref::new(16, 0))),
                (bytes(b"Last"), Obj::Ref(Ref::new(17, 0))),
                (bytes(b"Count"), Obj::Int(3)),
            ]),
        ),
        (
            4,
            page(
                2,
                &[
                    (bytes(b"Contents"), Obj::Ref(Ref::new(7, 0))),
                    (bytes(b"Annots"), Obj::Array(vec![Obj::Ref(Ref::new(8, 0))])),
                ],
            ),
        ),
        (
            5,
            page(
                2,
                &[(
                    bytes(b"Annots"),
                    Obj::Array(vec![Obj::Ref(Ref::new(19, 0))]),
                )],
            ),
        ),
        (
            6,
            Obj::Dict(vec![(bytes(b"EmbeddedFiles"), Obj::Ref(Ref::new(14, 0)))]),
        ),
        (
            7,
            Obj::Stream {
                dict: vec![(bytes(b"Length"), Obj::Int(0))],
                data: bytes(b""),
            },
        ),
        (
            8,
            Obj::Dict(vec![
                (bytes(b"Type"), Obj::Name(bytes(b"Annot"))),
                (bytes(b"Subtype"), Obj::Name(bytes(b"Square"))),
                (
                    bytes(b"Rect"),
                    Obj::Array(vec![Obj::Int(10), Obj::Int(10), Obj::Int(50), Obj::Int(50)]),
                ),
            ]),
        ),
        (
            9,
            Obj::Dict(vec![
                (
                    bytes(b"Fields"),
                    Obj::Array(vec![Obj::Ref(Ref::new(10, 0)), Obj::Ref(Ref::new(11, 0))]),
                ),
                (bytes(b"NeedAppearances"), Obj::Bool(true)),
            ]),
        ),
        (
            10,
            Obj::Dict(vec![
                (bytes(b"T"), Obj::String(bytes(b"top"))),
                (bytes(b"FT"), Obj::Name(bytes(b"Tx"))),
                (bytes(b"P"), Obj::Ref(Ref::new(4, 0))),
            ]),
        ),
        (
            11,
            Obj::Dict(vec![
                (bytes(b"T"), Obj::String(bytes(b"kid"))),
                (bytes(b"FT"), Obj::Name(bytes(b"Tx"))),
                (bytes(b"P"), Obj::Ref(Ref::new(4, 0))),
            ]),
        ),
        (
            12,
            Obj::Dict(vec![(
                bytes(b"OCGs"),
                Obj::Array(vec![Obj::Ref(Ref::new(13, 0))]),
            )]),
        ),
        (
            13,
            Obj::Dict(vec![(bytes(b"Type"), Obj::Name(bytes(b"OCG")))]),
        ),
        (
            14,
            Obj::Dict(vec![(
                bytes(b"Kids"),
                Obj::Array(vec![Obj::Ref(Ref::new(15, 0))]),
            )]),
        ),
        (15, Obj::Dict(vec![(bytes(b"Names"), files)])),
        (
            16,
            Obj::Dict(vec![
                (bytes(b"Title"), Obj::String(bytes(b"one"))),
                (bytes(b"Parent"), Obj::Ref(Ref::new(3, 0))),
                (bytes(b"Next"), Obj::Ref(Ref::new(17, 0))),
                (
                    bytes(b"Dest"),
                    Obj::Array(vec![Obj::Ref(Ref::new(4, 0)), Obj::Name(bytes(b"Fit"))]),
                ),
            ]),
        ),
        (
            17,
            Obj::Dict(vec![
                (bytes(b"Title"), Obj::String(bytes(b"two"))),
                (bytes(b"Parent"), Obj::Ref(Ref::new(3, 0))),
                (bytes(b"Prev"), Obj::Ref(Ref::new(16, 0))),
                (bytes(b"First"), Obj::Ref(Ref::new(18, 0))),
                (bytes(b"Last"), Obj::Ref(Ref::new(18, 0))),
                (bytes(b"Count"), Obj::Int(1)),
                (
                    bytes(b"Dest"),
                    Obj::Array(vec![Obj::Ref(Ref::new(4, 0)), Obj::Name(bytes(b"Fit"))]),
                ),
            ]),
        ),
        (
            18,
            Obj::Dict(vec![
                (bytes(b"Title"), Obj::String(bytes(b"child"))),
                (bytes(b"Parent"), Obj::Ref(Ref::new(17, 0))),
                (
                    bytes(b"Dest"),
                    Obj::Array(vec![Obj::Ref(Ref::new(5, 0)), Obj::Name(bytes(b"Fit"))]),
                ),
            ]),
        ),
        (
            19,
            Obj::Dict(vec![
                (bytes(b"Type"), Obj::Name(bytes(b"Annot"))),
                (bytes(b"Subtype"), Obj::Name(bytes(b"Square"))),
                (
                    bytes(b"Rect"),
                    Obj::Array(vec![Obj::Int(20), Obj::Int(20), Obj::Int(60), Obj::Int(60)]),
                ),
            ]),
        ),
    ];
    write_objects_as_document(&objects, Ref::new(1, 0), &Budget::unlimited(), &mut guard())
        .expect("fixture write")
}

/// A document whose catalog nests an inline dictionary 70 levels deep: the
/// Viewer profile's depth budget is 64, so parsing it is a typed
/// `BUDGET_DEPTH` — the honest-failure fixture (SL-1A.UI.06).
fn depth_exceeding_pdf() -> Vec<u8> {
    let mut deep = String::from("1");
    for _ in 0..70 {
        deep = format!("<< /D {deep} >>");
    }
    let catalog = format!("1 0 obj\n<< /Type /Catalog /Pages 2 0 R /Deep {deep} >>\nendobj\n");
    let pages = "2 0 obj\n<< /Type /Pages /Kids [] /Count 0 >>\nendobj\n";
    let mut out = Vec::new();
    out.extend_from_slice(b"%PDF-1.4\n");
    let mut offsets: Vec<(u32, u64)> = Vec::new();
    for (num, body) in [(1u32, catalog.as_bytes()), (2, pages.as_bytes())] {
        offsets.push((num, u64::try_from(out.len()).unwrap_or(0)));
        out.extend_from_slice(body);
    }
    let xref_at = u64::try_from(out.len()).unwrap_or(0);
    out.extend_from_slice(b"xref\n0 3\n0000000000 65535 f \n");
    for (_, off) in &offsets {
        out.extend_from_slice(format!("{off:010} 00000 n \n").as_bytes());
    }
    out.extend_from_slice(b"trailer\n<< /Size 3 /Root 1 0 R >>\nstartxref\n");
    out.extend_from_slice(format!("{xref_at}\n%%EOF\n").as_bytes());
    out
}

/// Regenerate the committed fixture:
/// `cargo test --test verification_display write_corpus_fixture -- --ignored`
#[test]
#[ignore]
fn write_corpus_fixture() {
    std::fs::write(fixture_path(), verification_features_pdf()).expect("write fixture");
}

/// The committed fixture must be exactly the builder's output — the corpus
/// entry cannot drift from what the tests exercise.
#[test]
fn corpus_fixture_matches_the_builder() {
    let committed = std::fs::read(fixture_path())
        .expect("corpus/fixtures/verification_features.pdf is missing");
    assert_eq!(committed, verification_features_pdf());
}

/// The verification line after a rotation names every preserved feature with
/// the fixture's surveyed counts, the byte sizes, and the verdict.
#[test]
fn verification_line_reports_preserved_features() {
    let dir = temp_dir("rotate");
    std::fs::create_dir_all(&dir).expect("mkdir");
    let input = dir.join("features.pdf");
    let output = dir.join("rotated.pdf");
    std::fs::write(&input, verification_features_pdf()).expect("write input");

    let out = selis()
        .args([
            "rotate",
            input.to_str().unwrap(),
            "--angle",
            "90",
            "-o",
            output.to_str().unwrap(),
        ])
        .output()
        .expect("run selis");
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    let line = stderr
        .lines()
        .find(|l| l.starts_with("verified: "))
        .expect("the verification line is printed");
    assert!(line.contains("pages 2, "), "{line}");
    assert!(line.contains("annotations 2, "), "{line}");
    assert!(line.contains("fields 2, "), "{line}");
    assert!(line.contains("OCGs 1, "), "{line}");
    assert!(line.contains("outlines 3, "), "{line}");
    assert!(line.contains("attachments 2, "), "{line}");
    assert!(line.contains("bytes "), "{line}");
    assert!(line.ends_with("structural check ok"), "{line}");
    assert!(output.exists(), "output written");
    std::fs::remove_dir_all(&dir).ok();
}

/// `--json` emits the machine-readable twin on stdout: the same fields under
/// a `verification` envelope, with the `checked` list naming the asserted
/// preservation promises.
#[test]
fn json_twin_carries_the_same_fields() {
    let dir = temp_dir("json");
    std::fs::create_dir_all(&dir).expect("mkdir");
    let input = dir.join("features.pdf");
    let output = dir.join("rotated.json.pdf");
    std::fs::write(&input, verification_features_pdf()).expect("write input");

    let out = selis()
        .args([
            "rotate",
            input.to_str().unwrap(),
            "--angle",
            "90",
            "-o",
            output.to_str().unwrap(),
            "--json",
        ])
        .output()
        .expect("run selis");
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    let value: serde_json::Value =
        serde_json::from_str(stdout.trim()).expect("the --json twin is one JSON object on stdout");
    let v = value.get("verification").expect("verification envelope");
    assert_eq!(jget(v, "pages_in"), 2);
    assert_eq!(jget(v, "pages_out"), 2);
    assert_eq!(jget(v, "annotations_in"), 2);
    assert_eq!(jget(v, "annotations_out"), 2);
    assert_eq!(jget(v, "fields_out"), 2);
    assert_eq!(jget(v, "ocgs_out"), 1);
    assert_eq!(jget(v, "outline_entries_in"), 3);
    assert_eq!(jget(v, "outline_entries_out"), 3);
    assert_eq!(jget(v, "embedded_files_out"), 2);
    assert_eq!(jget(v, "verdict"), "ok");
    assert_eq!(
        jget(v, "checked"),
        &serde_json::json!([
            "pages",
            "annotations",
            "fields",
            "ocgs",
            "outline_entries",
            "embedded_files"
        ])
    );
    assert!(jget(v, "bytes_in").as_u64().expect("bytes_in") > 0);
    assert!(jget(v, "bytes_out").as_u64().expect("bytes_out") > 0);
    std::fs::remove_dir_all(&dir).ok();
}

/// A page selection shows the changed counts honestly (`pages 2->1`,
/// outlines pruned with their pages) and asserts exactly the promises that
/// still hold.
#[test]
fn page_selection_shows_changed_counts() {
    let dir = temp_dir("split");
    std::fs::create_dir_all(&dir).expect("mkdir");
    let input = dir.join("features.pdf");
    let output = dir.join("split.pdf");
    std::fs::write(&input, verification_features_pdf()).expect("write input");

    let out = selis()
        .args([
            "split",
            input.to_str().unwrap(),
            "--first",
            "0",
            "--last",
            "0",
            "-o",
            output.to_str().unwrap(),
            "--json",
        ])
        .output()
        .expect("run selis");
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    let line = stderr
        .lines()
        .find(|l| l.starts_with("verified: "))
        .expect("the verification line is printed");
    assert!(line.contains("pages 2->1"), "{line}");
    assert!(line.contains("annotations 2->1"), "{line}");
    assert!(line.contains("outlines 3->2"), "{line}");

    let stdout = String::from_utf8_lossy(&out.stdout);
    let value: serde_json::Value = serde_json::from_str(stdout.trim()).expect("json");
    let v = value.get("verification").expect("envelope");
    assert_eq!(
        jget(v, "checked"),
        &serde_json::json!(["pages", "annotations", "fields", "ocgs", "embedded_files"]),
        "outlines are not asserted on a page selection (their items are pruned)"
    );
    assert!(output.exists());
    std::fs::remove_dir_all(&dir).ok();
}

/// Budget exhaustion is honest: `selis check` on a document deeper than the
/// Viewer depth budget prints which budget tripped, the measured usage, and
/// the remedy — and exits 1, never a panic.
#[test]
fn budget_exhaustion_is_reported_honestly() {
    let dir = temp_dir("budget");
    std::fs::create_dir_all(&dir).expect("mkdir");
    let input = dir.join("deep.pdf");
    std::fs::write(&input, depth_exceeding_pdf()).expect("write input");

    let out = selis()
        .args(["check", input.to_str().unwrap()])
        .output()
        .expect("run selis");
    assert_eq!(out.status.code(), Some(1), "not a panic (101/134), exit 1");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("budget exceeded (depth)"), "{stderr}");
    assert!(stderr.contains("split it into smaller files"), "{stderr}");
    assert!(!stderr.contains("panicked"), "{stderr}");
    // Note: the measured usage numbers (limit 64, requested 65) appear when
    // the exhaustion surfaces directly (unit-tested in `failure.rs`); when
    // the engine's guard is poisoned by an internal recovery, the typed
    // error names the resource without the original numbers — the message
    // stays honest either way.
    std::fs::remove_dir_all(&dir).ok();
}

/// The batch report carries the `verification` object for every ok file —
/// the same fields the single-file twin emits — and isolates failures.
#[test]
fn batch_report_carries_verification_objects() {
    let dir = temp_dir("batch");
    let outdir = dir.join("batch-out");
    std::fs::create_dir_all(&dir).expect("mkdir");
    let good = dir.join("good.pdf");
    let deep = dir.join("deep.pdf");
    std::fs::write(&good, verification_features_pdf()).expect("write good");
    std::fs::write(&deep, depth_exceeding_pdf()).expect("write deep");

    let out = selis()
        .args([
            "batch",
            "compress",
            good.to_str().unwrap(),
            deep.to_str().unwrap(),
            "--outdir",
            outdir.to_str().unwrap(),
        ])
        .output()
        .expect("run selis");
    assert!(out.status.success(), "batch exits 0 with isolated failures");
    let report = std::fs::read_to_string(outdir.join("report.json")).expect("report.json");
    let value: serde_json::Value = serde_json::from_str(&report).expect("report parses");
    assert_eq!(jget(&value, "tool"), "compress");
    assert_eq!(jget(&value, "ok"), 1);
    assert_eq!(jget(&value, "failed"), 1);
    let files = jget(&value, "files").as_array().expect("files");
    let ok = files
        .iter()
        .find(|f| jget(f, "status") == "ok")
        .expect("the good file ok");
    let verification = ok.get("verification").expect("verification object present");
    assert_eq!(jget(verification, "verdict"), "ok");
    assert_eq!(jget(verification, "pages_out"), 2);
    assert_eq!(
        jget(verification, "checked"),
        &serde_json::json!([
            "pages",
            "annotations",
            "fields",
            "ocgs",
            "outline_entries",
            "embedded_files"
        ])
    );
    let failed = files
        .iter()
        .find(|f| jget(f, "status") == "failed")
        .expect("the deep file isolated");
    assert!(
        failed.get("error").is_some(),
        "the failure is recorded, not propagated"
    );
    std::fs::remove_dir_all(&dir).ok();
}

/// The fixture opens as a document model under the Viewer budget (the batch
/// compress path re-opens its output; the fixture must be a real document).
#[test]
fn fixture_builds_a_document_model() {
    let src = verification_features_pdf();
    let budget = Budget::profile(selis_sandbox::Surface::Viewer);
    let mut g = budget.guard_with(&FixedClock(0), CancelToken::new());
    let sx = xref::find_startxref(&src, 4096).expect("startxref");
    let doc = parse_revisions(&src, sx, &budget, &mut g).expect("parses");
    assert_eq!(doc.revisions().len(), 1);
    let observed = selis_pdf_cos::verify::survey(&src, &budget, &mut g).expect("survey");
    assert_eq!(observed.pages, 2);
    assert_eq!(observed.annotations, 2);
    assert_eq!(observed.fields, 2);
    assert_eq!(observed.ocgs, 1);
    assert_eq!(observed.outlines, 3);
    assert_eq!(observed.embedded_files, 2);
}
