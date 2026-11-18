//! Tests for the full-rewrite writer (SL-5.EDIT.05).
//!
//! Three obligations are exercised: (a) a normal rewrite round-trips with every
//! count preserved and read-back through the engine; (b) a deliberately
//! corrupted rewrite is refused by the verification gate with no output; (c) an
//! atomic commit of such a rewrite leaves the destination byte-identical.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss
)]

use selis_bytes::Bytes;
use selis_error::Code;
use selis_pdf_cos::Obj;
use selis_pdf_cos::Ref;
use selis_sandbox::{Budget, FixedClock};

use crate::rewrite::{
    build_rewritten, save_rewritten, save_rewritten_atomic, save_rewritten_atomic_with,
    save_rewritten_with, verify_rewrite, RewriteCommitFault, RewriteFault, RewriteOptions,
};

fn b(v: &[u8]) -> Bytes {
    Bytes::copy_from_slice(v)
}

/// A two-page, formed, layered, tagged, annotated document with one deliberately
/// unreachable object (99). Counts: pages=2, annots=2, fields=2, ocgs=1,
/// structure elements=2. Object numbers: 1 catalog, 2 pages, 3/5 pages,
/// 4/6 contents, 7/8 annots, 9 AcroForm, 10/11 fields, 12 OCProperties,
/// 13 OCG, 14 StructTreeRoot, 15 StructElem, 99 unreachable.
fn formed_pdf() -> Vec<u8> {
    let page = |parent: u32, content: u32, annot: u32| {
        Obj::Dict(vec![
            (b(b"Type"), Obj::Name(b(b"Page"))),
            (b(b"Parent"), Obj::Ref(Ref::new(parent, 0))),
            (
                b(b"MediaBox"),
                Obj::Array(vec![Obj::Int(0), Obj::Int(0), Obj::Int(200), Obj::Int(200)]),
            ),
            (b(b"Contents"), Obj::Ref(Ref::new(content, 0))),
            (b(b"Annots"), Obj::Array(vec![Obj::Ref(Ref::new(annot, 0))])),
        ])
    };
    let objects: Vec<(u32, Obj)> = vec![
        (
            1,
            Obj::Dict(vec![
                (b(b"Type"), Obj::Name(b(b"Catalog"))),
                (b(b"Pages"), Obj::Ref(Ref::new(2, 0))),
                (b(b"AcroForm"), Obj::Ref(Ref::new(9, 0))),
                (b(b"OCProperties"), Obj::Ref(Ref::new(12, 0))),
                (b(b"StructTreeRoot"), Obj::Ref(Ref::new(14, 0))),
            ]),
        ),
        (
            2,
            Obj::Dict(vec![
                (b(b"Type"), Obj::Name(b(b"Pages"))),
                (
                    b(b"Kids"),
                    Obj::Array(vec![Obj::Ref(Ref::new(3, 0)), Obj::Ref(Ref::new(5, 0))]),
                ),
                (b(b"Count"), Obj::Int(2)),
            ]),
        ),
        (3, page(2, 4, 7)),
        (
            4,
            Obj::Stream {
                dict: vec![(b(b"Length"), Obj::Int(2))],
                data: b(b"q\n"),
            },
        ),
        (5, page(2, 6, 8)),
        (
            6,
            Obj::Stream {
                dict: vec![(b(b"Length"), Obj::Int(2))],
                data: b(b"Q\n"),
            },
        ),
        (
            7,
            Obj::Dict(vec![
                (b(b"Type"), Obj::Name(b(b"Annot"))),
                (b(b"Subtype"), Obj::Name(b(b"Square"))),
            ]),
        ),
        (
            8,
            Obj::Dict(vec![
                (b(b"Type"), Obj::Name(b(b"Annot"))),
                (b(b"Subtype"), Obj::Name(b(b"Square"))),
            ]),
        ),
        (
            9,
            Obj::Dict(vec![(
                b(b"Fields"),
                Obj::Array(vec![Obj::Ref(Ref::new(10, 0))]),
            )]),
        ),
        (
            10,
            Obj::Dict(vec![
                (b(b"T"), Obj::String(b(b"top"))),
                (b(b"Kids"), Obj::Array(vec![Obj::Ref(Ref::new(11, 0))])),
            ]),
        ),
        (11, Obj::Dict(vec![(b(b"T"), Obj::String(b(b"kid")))])),
        (
            12,
            Obj::Dict(vec![(
                b(b"OCGs"),
                Obj::Array(vec![Obj::Ref(Ref::new(13, 0))]),
            )]),
        ),
        (13, Obj::Dict(vec![(b(b"Type"), Obj::Name(b(b"OCG")))])),
        (
            14,
            Obj::Dict(vec![
                (b(b"Type"), Obj::Name(b(b"StructTreeRoot"))),
                (b(b"K"), Obj::Array(vec![Obj::Ref(Ref::new(15, 0))])),
            ]),
        ),
        (
            15,
            Obj::Dict(vec![
                (b(b"Type"), Obj::Name(b(b"StructElem"))),
                (b(b"S"), Obj::Name(b(b"P"))),
                (b(b"K"), Obj::Array(vec![Obj::Ref(Ref::new(16, 0))])),
            ]),
        ),
        (
            16,
            Obj::Dict(vec![
                (b(b"Type"), Obj::Name(b(b"StructElem"))),
                (b(b"S"), Obj::Name(b(b"Span"))),
            ]),
        ),
        // Unreachable: a clean rewrite must drop it.
        (99, Obj::String(b(b"unreachable junk"))),
    ];
    selis_pdf_cos::doc_writer::write_objects_as_document(
        &objects,
        Ref::new(1, 0),
        &Budget::unlimited(),
        &mut Budget::unlimited().guard(),
    )
    .expect("write fixture")
}

/// Read the page count back through the engine's Session.
fn engine_page_count(bytes: &[u8]) -> usize {
    let budget = Budget::unlimited();
    let session = selis_pdf_engine::Session::open(bytes.to_vec(), &budget, &FixedClock(0))
        .expect("engine opens the rewritten document");
    let mut count = 0usize;
    while session.page_size(count).is_some() {
        count = count.saturating_add(1);
    }
    count
}

#[test]
fn normal_rewrite_round_trips_with_all_counts_preserved() {
    let src = formed_pdf();
    let budget = Budget::unlimited();
    let mut g = budget.guard();

    // Pre-save baseline through the same gate the rewrite uses.
    let before = selis_pdf_cos::verify::survey(&src, &budget, &mut g).expect("survey");
    assert_eq!(before.pages, 2);
    assert_eq!(before.annotations, 2);
    assert_eq!(before.fields, 2);
    assert_eq!(before.ocgs, 1);

    let mut g = budget.guard();
    let (out, report) = save_rewritten(&src, &budget, &mut g).expect("save_rewritten");
    assert_ne!(
        out, src,
        "I2 exception: this is a fresh single-revision file"
    );

    // The report's counts are the ones that were compared and matched.
    assert_eq!(report.verification.pages, 2);
    assert_eq!(report.verification.annotations, 2);
    assert_eq!(report.verification.fields, 2);
    assert_eq!(report.verification.ocgs, 1);
    assert_eq!(report.verification.structure_nodes, 2);
    assert_eq!(
        report.bytes_in,
        u64::try_from(src.len()).unwrap_or(u64::MAX)
    );
    assert_eq!(
        report.bytes_out,
        u64::try_from(out.len()).unwrap_or(u64::MAX)
    );

    // Unreferenced object 99 was dropped; the 16 reachable objects were
    // renumbered from 1 and written into the fresh xref.
    assert_eq!(report.objects_in, 17, "16 reachable objects + object 99");
    assert_eq!(report.objects_out, 16);
    assert_eq!(report.objects_dropped, 1);
    assert!(report.objects_renumbered > 0);

    // The output is structurally clean under the same verifier.
    let mut g = budget.guard();
    let verdict = selis_pdf_cos::verify::verify_structural(
        &out,
        &selis_pdf_cos::verify::Expectations {
            pages: Some(2),
            annotations: Some(2),
            fields: Some(2),
            ocgs: Some(1),
            ..selis_pdf_cos::verify::Expectations::none()
        },
        &budget,
        &mut g,
    )
    .expect("verify output");
    assert!(
        verdict.ok,
        "rewritten document must verify: {:?}",
        verdict.faults
    );

    // Read-back via the engine proves a real reader opens the result and sees
    // both pages after renumbering.
    assert_eq!(engine_page_count(&out), 2);
}

#[test]
fn corrupted_rewrite_dropping_a_page_is_refused_with_no_output() {
    let src = formed_pdf();
    let budget = Budget::unlimited();

    let options = RewriteOptions {
        fault: Some(RewriteFault::DropFirstPage),
        commit_fault: None,
    };
    let mut g = budget.guard();
    let err = save_rewritten_with(&src, &options, &budget, &mut g)
        .expect_err("a rewrite that dropped a page must be refused");
    assert_eq!(err.code(), Code::VerifyFailed);
    assert!(
        format!("{err:?}").contains("page count changed"),
        "the typed error names the discrepancy: {err:?}"
    );
}

#[test]
fn corrupted_rewrite_dropping_annotations_is_refused_with_no_output() {
    let src = formed_pdf();
    let budget = Budget::unlimited();

    let options = RewriteOptions {
        fault: Some(RewriteFault::DropAnnotations),
        commit_fault: None,
    };
    let mut g = budget.guard();
    let err = save_rewritten_with(&src, &options, &budget, &mut g)
        .expect_err("a rewrite that lost annotations must be refused");
    assert_eq!(err.code(), Code::VerifyFailed);
    assert!(
        format!("{err:?}").contains("annotation count changed"),
        "the typed error names the discrepancy: {err:?}"
    );
}

#[test]
fn verify_gate_catches_a_corrupted_candidate_directly() {
    let src = formed_pdf();
    let budget = Budget::unlimited();

    // Build WITHOUT verification via the internal hook so we hold the bad
    // bytes, then show the standalone gate refuses them.
    let options = RewriteOptions {
        fault: Some(RewriteFault::DropFirstPage),
        commit_fault: None,
    };
    let mut g = budget.guard();
    let (bad, _) = build_rewritten(&src, &options, &budget, &mut g).expect("build bad candidate");

    let mut g = budget.guard();
    let err = verify_rewrite(&src, &bad, &budget, &mut g)
        .expect_err("the verification gate catches the dropped page");
    assert_eq!(err.code(), Code::VerifyFailed);
    assert!(format!("{err:?}").contains("page count changed"));

    // A clean candidate verifies through the same gate.
    let mut g = budget.guard();
    let (clean, _) = build_rewritten(&src, &RewriteOptions::default(), &budget, &mut g)
        .expect("build clean candidate");
    let mut g = budget.guard();
    let verified = verify_rewrite(&src, &clean, &budget, &mut g).expect("clean verifies");
    assert_eq!(verified.pages, 2);
    assert_eq!(verified.structure_nodes, 2);
}

fn temp_dir(tag: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!("selis-edit05-{tag}-{}", std::process::id()))
}

fn no_temp_litter(dir: &std::path::Path) -> bool {
    std::fs::read_dir(dir)
        .map(|rd| {
            !rd.filter_map(Result::ok)
                .any(|e| e.file_name().to_string_lossy().contains("selis-tmp"))
        })
        .unwrap_or(false)
}

#[test]
fn atomic_commit_on_verification_failure_keeps_the_original() {
    let src = formed_pdf();
    let dir = temp_dir("atomic-fail");
    std::fs::create_dir_all(&dir).expect("mkdir");
    let dest = dir.join("doc.pdf");
    std::fs::write(&dest, &src).expect("write original");

    let budget = Budget::unlimited();
    let options = RewriteOptions {
        fault: Some(RewriteFault::DropFirstPage),
        commit_fault: None,
    };
    let mut g = budget.guard();
    let err = save_rewritten_atomic_with(&dest, &src, &options, &budget, &mut g)
        .expect_err("verification refuses before any write");
    assert_eq!(err.code(), Code::VerifyFailed);
    assert_eq!(
        std::fs::read(&dest).expect("read destination"),
        src,
        "the original survived byte-for-byte"
    );
    assert!(no_temp_litter(&dir), "no partial file was left behind");
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn atomic_commit_of_a_clean_rewrite_is_durable() {
    let src = formed_pdf();
    let dir = temp_dir("atomic-ok");
    std::fs::create_dir_all(&dir).expect("mkdir");
    let dest = dir.join("doc.pdf");
    std::fs::write(&dest, &src).expect("write original");

    let budget = Budget::unlimited();
    let mut g = budget.guard();
    let report = save_rewritten_atomic(&dest, &src, &budget, &mut g).expect("clean atomic rewrite");
    assert_eq!(report.verification.pages, 2);
    let on_disk = std::fs::read(&dest).expect("read destination");
    assert_ne!(on_disk, src, "the rewrite replaced the original");
    assert_eq!(
        engine_page_count(&on_disk),
        2,
        "engine reads the committed file"
    );
    assert!(no_temp_litter(&dir));

    // An injected failure before the rename leaves the committed document
    // intact (the EDIT.04 durable-swap contract, reused by this path).
    let committed = on_disk;
    let options = RewriteOptions {
        fault: None,
        commit_fault: Some(RewriteCommitFault::BeforeRename),
    };
    let mut g = budget.guard();
    let err = save_rewritten_atomic_with(&dest, &committed, &options, &budget, &mut g)
        .expect_err("injected pre-rename fault aborts");
    assert_eq!(err.code(), Code::WriteFailed);
    assert_eq!(std::fs::read(&dest).expect("read destination"), committed);
    assert!(no_temp_litter(&dir));
    std::fs::remove_dir_all(&dir).ok();
}
