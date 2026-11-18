//! Tests for signature preservation and DocMDP enforcement (SL-5.EDIT.07).
//!
//! Four obligations are exercised:
//! 1. a real digitally-signed document, annotated and incremental-saved, still
//!    reports a verifying signature over unchanged byte ranges (engine/sign
//!    report read-back); `qpdf --check` is the scriptable interop oracle;
//! 2. DocMDP `/P 1` refuses any content/page edit, `/P 2` refuses an annotation
//!    but allows a form fill, `/P 3` allows the annotation — each refusal names
//!    the level and the refused operation;
//! 3. redefining a signature's own object is refused even without DocMDP;
//! 4. a refusal produces no bytes, so a caller that only writes on success
//!    leaves the destination untouched.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::too_many_arguments
)]

use selis_bytes::Bytes;
use selis_error::Code;
use selis_pdf_cos::{Obj, Ref};
use selis_pdf_sign::{sign_digital, DigitalOptions};
use selis_sandbox::{Budget, FixedClock};

use crate::incremental::{materialise, save_incremental, ObjectModel, ObjectMutation};
use crate::rewrite::{save_rewritten, save_rewritten_atomic};
use crate::signature_guard::{
    check_mutation_allowed, inspect_signatures, verify_signatures_preserved, DocMdpLevel,
};

const ROOT: &str = include_str!("../../../crates/selis-pdf-sign/tests/fixtures/root.crt");
const LEAF: &str = include_str!("../../../crates/selis-pdf-sign/tests/fixtures/leaf.crt");
const LEAF_KEY: &str = include_str!("../../../crates/selis-pdf-sign/tests/fixtures/leaf.key");
const SIGNING_TIME: &str = "20261104120000Z";

fn b(v: &[u8]) -> Bytes {
    Bytes::copy_from_slice(v)
}

fn budget() -> Budget {
    Budget::unlimited()
}

fn guard(b: &Budget) -> selis_sandbox::BudgetGuard<'_> {
    b.guard_with(&FixedClock(0), selis_sandbox::CancelToken::new())
}

/// A minimal PDF builder with a correct classic xref (mirrors the sign crate's
/// fixture builder; kept local so this crate's tests are self-contained).
fn build_pdf(objects: &[(u32, &[u8])]) -> Vec<u8> {
    let max = objects.iter().map(|(num, _)| *num).max().unwrap_or(0);
    let mut out = Vec::new();
    out.extend_from_slice(b"%PDF-1.4\n");
    let mut offsets = vec![0u64; max as usize + 1];
    for (num, body) in objects {
        offsets[*num as usize] = out.len() as u64;
        out.extend_from_slice(format!("{num} 0 obj\n").as_bytes());
        out.extend_from_slice(body);
        out.extend_from_slice(b"\nendobj\n");
    }
    let xref_at = out.len() as u64;
    out.extend_from_slice(format!("xref\n0 {}\n", max + 1).as_bytes());
    out.extend_from_slice(b"0000000000 65535 f \n");
    for index in 1..=max {
        out.extend_from_slice(format!("{:010} 00000 n \n", offsets[index as usize]).as_bytes());
    }
    out.extend_from_slice(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref_at}\n%%EOF\n",
            max + 1
        )
        .as_bytes(),
    );
    out
}

/// A one-page PDF: catalog, page tree, page, empty content stream.
fn base_pdf() -> Vec<u8> {
    let objects: Vec<(u32, &[u8])> = vec![
        (1, b"<< /Type /Catalog /Pages 2 0 R >>"),
        (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
        (
            3,
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << >> /Contents 4 0 R >>",
        ),
        (4, b"<< /Length 0 >>\nstream\n\nendstream"),
    ];
    build_pdf(&objects)
}

fn digital_options() -> DigitalOptions {
    DigitalOptions {
        page: 1,
        rect: [72.0, 600.0, 320.0, 690.0],
        field_name: "sig1".to_string(),
        signer_name: "Ada Lovelace".to_string(),
        reason: Some("Approved".to_string()),
        location: Some("London".to_string()),
        signing_time: SIGNING_TIME.to_string(),
        key_pem: LEAF_KEY.to_string(),
        certificates_pem: vec![LEAF.to_string(), ROOT.to_string()],
    }
}

/// A one-page PDF with a certification signature dictionary and an explicit
/// DocMDP `/P` level, plus a text form field. The CMS blob is a placeholder:
/// the DocMDP guard reads the object graph, it does not verify CMS (that is
/// test 1's job with a real signature).
fn docmdp_pdf(level: i64) -> Vec<u8> {
    let field: Vec<u8> =
        b"<< /Type /Annot /Subtype /Widget /FT /Sig /T (CertSig) /V 7 0 R /P 3 0 R /Rect [0 0 10 10] >>".to_vec();
    let sig = b"<< /Type /Sig /Filter /Adobe.PPKLite /SubFilter /adbe.pkcs7.detached /ByteRange [0 100 200 100] /Contents <00> /Reference [8 0 R] >>";
    let reference = b"<< /TransformMethod /DocMDP /TransformParams 9 0 R /Data 7 0 R >>";
    let params = format!("<< /P {level} >>").into_bytes();
    let form = b"<< /Fields [6 0 R 10 0 R] /SigFlags 3 >>";
    let text_field = b"<< /FT /Tx /T (Name) /V (old) >>";
    let objects: Vec<(u32, &[u8])> = vec![
        (1, b"<< /Type /Catalog /Pages 2 0 R /AcroForm 5 0 R >>"),
        (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
        (
            3,
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 300 200] /Resources << >> /Contents 4 0 R /Annots [6 0 R] >>",
        ),
        (4, b"<< /Length 0 >>\nstream\n\nendstream"),
        (5, form),
        (6, field.as_slice()),
        (7, sig),
        (8, reference),
        (9, params.as_slice()),
        (10, text_field),
    ];
    build_pdf(&objects)
}

/// Like [`docmdp_pdf`], but the fillable field's `/FT /Tx` lives on a parent
/// field (object 10) and the child widget (object 11) only carries `/Parent`.
/// This is the inherited-`/FT` shape DocMDP P=2 must recognise.
fn docmdp_pdf_inherited_ft(level: i64) -> Vec<u8> {
    let field: Vec<u8> =
        b"<< /Type /Annot /Subtype /Widget /FT /Sig /T (CertSig) /V 7 0 R /P 3 0 R /Rect [0 0 10 10] >>".to_vec();
    let sig = b"<< /Type /Sig /Filter /Adobe.PPKLite /SubFilter /adbe.pkcs7.detached /ByteRange [0 100 200 100] /Contents <00> /Reference [8 0 R] >>";
    let reference = b"<< /TransformMethod /DocMDP /TransformParams 9 0 R /Data 7 0 R >>";
    let params = format!("<< /P {level} >>").into_bytes();
    let form = b"<< /Fields [6 0 R 10 0 R] /SigFlags 3 >>";
    let parent = b"<< /FT /Tx /T (Parent) /Kids [11 0 R] >>";
    let child = b"<< /T (Child) /Parent 10 0 R /V (old) >>";
    let objects: Vec<(u32, &[u8])> = vec![
        (1, b"<< /Type /Catalog /Pages 2 0 R /AcroForm 5 0 R >>"),
        (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
        (
            3,
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 300 200] /Resources << >> /Contents 4 0 R /Annots [6 0 R] >>",
        ),
        (4, b"<< /Length 0 >>\nstream\n\nendstream"),
        (5, form),
        (6, field.as_slice()),
        (7, sig),
        (8, reference),
        (9, params.as_slice()),
        (10, parent),
        (11, child),
    ];
    build_pdf(&objects)
}

fn find_page(model: &ObjectModel) -> u32 {
    *model
        .iter()
        .find(|(_, obj)| matches!(obj, Obj::Dict(p) if p.iter().any(|(k, v)| k.as_slice() == b"Type" && matches!(v, Obj::Name(n) if n.as_slice() == b"Page"))))
        .map(|(num, _)| num)
        .expect("the model has a page")
}

fn find_content(model: &ObjectModel) -> u32 {
    *model
        .iter()
        .find(|(_, obj)| matches!(obj, Obj::Stream { .. }))
        .map(|(num, _)| num)
        .expect("the model has a content stream")
}

fn next_num(model: &ObjectModel) -> u32 {
    model.keys().copied().max().unwrap_or(0).saturating_add(1)
}

fn dict_get<'a>(pairs: &'a [(Bytes, Obj)], key: &[u8]) -> Option<&'a Obj> {
    pairs
        .iter()
        .find(|(k, _)| k.as_slice() == key)
        .map(|(_, v)| v)
}

/// Append `annot` to a page's `/Annots` array, preserving every other entry.
fn page_with_annot(page: &Obj, annot: u32) -> Obj {
    let Obj::Dict(pairs) = page else {
        panic!("page is a dictionary");
    };
    let mut out = pairs.clone();
    let mut annots = match dict_get(&out, b"Annots") {
        Some(Obj::Array(items)) => items.clone(),
        _ => Vec::new(),
    };
    annots.push(Obj::Ref(Ref::new(annot, 0)));
    if let Some((_, value)) = out.iter_mut().find(|(k, _)| k.as_slice() == b"Annots") {
        *value = Obj::Array(annots);
    } else {
        out.push((b(b"Annots"), Obj::Array(annots)));
    }
    Obj::Dict(out)
}

fn annotation(value: Obj, num: u32, page: u32, model: &ObjectModel) -> Vec<ObjectMutation> {
    let new_page = page_with_annot(model.get(&page).expect("page exists"), num);
    vec![
        ObjectMutation::SetObject { num, value },
        ObjectMutation::SetObject {
            num: page,
            value: new_page,
        },
    ]
}

fn square_annot() -> Obj {
    Obj::Dict(vec![
        (b(b"Type"), Obj::Name(b(b"Annot"))),
        (b(b"Subtype"), Obj::Name(b(b"Square"))),
        (
            b(b"Rect"),
            Obj::Array(vec![Obj::Int(10), Obj::Int(10), Obj::Int(40), Obj::Int(40)]),
        ),
    ])
}

/// (1) A real signature survives a real annotation + incremental save.
#[test]
fn signed_document_annotated_and_saved_still_verifies() {
    let bud = budget();
    let mut g = guard(&bud);
    let src = sign_digital(&base_pdf(), &digital_options(), &bud, &mut g)
        .expect("sign the fixture")
        .0;

    // The source signature verifies and covers the whole (pre-save) document.
    let before = selis_pdf_sign::signatures(&src, &[], 0, &bud, &mut g).expect("source report");
    assert_eq!(before.signatures.len(), 1);
    assert!(before.signatures[0].verified);
    assert!(before.signatures[0].covers_whole_document);

    let model = materialise(&src, &bud, &mut g).expect("materialise signed document");
    let page = find_page(&model);
    let annot_num = next_num(&model);
    let mutations = annotation(square_annot(), annot_num, page, &model);

    // The guard allows the annotation: this is an approval signature, no DocMDP.
    check_mutation_allowed(&model, &mutations, &bud, &mut g).expect("annotation allowed");

    let (out, report) = save_incremental(&src, &model, &mutations, &bud, &mut g)
        .expect("incremental save of a signed document");
    assert!(report.prefix_preserved);
    assert_eq!(out.get(..src.len()), Some(src.as_slice()));

    // The helper re-runs the real report on the output and proves preservation.
    let evidence = verify_signatures_preserved(&src, &out, &bud, &mut g).expect("preserved");
    assert_eq!(evidence.source_signatures, 1);
    assert_eq!(evidence.preserved_signatures, 1);
    assert_eq!(evidence.byte_ranges.len(), 1);
    let range = evidence.byte_ranges[0];
    assert_eq!(range, before.signatures[0].byte_range.expect("range"));
    assert!((range[2] + range[3]) <= u64::try_from(src.len()).unwrap());
    assert!(out.len() > src.len());

    // Re-running the report the way a caller would (engine/sign read-back): the
    // signature still verifies over the same offsets. `covers_whole_document`
    // is false on the OUTPUT by design — there are appended bytes — which is
    // exactly why the guard compares against the SOURCE length above.
    let after = selis_pdf_sign::signatures(&out, &[], 0, &bud, &mut g).expect("output report");
    assert_eq!(after.signatures.len(), 1);
    assert!(
        after.signatures[0].verified,
        "{:?}",
        after.signatures[0].error
    );
    assert!(!after.signatures[0].covers_whole_document);
    assert_eq!(after.signatures[0].byte_range, Some(range));
}

/// (2) DocMDP level enforcement, one case per level.
#[test]
fn docmdp_levels_refuse_and_allow_the_right_operations() {
    let bud = budget();

    // P=1: a content/page edit is refused with a specific, named error.
    {
        let mut g = guard(&bud);
        let src = docmdp_pdf(1);
        let model = materialise(&src, &bud, &mut g).expect("materialise P=1");
        let finding = inspect_signatures(&model, &bud, &mut g).expect("inspect");
        assert_eq!(finding.certification_level, Some(DocMdpLevel::NoChanges));
        let content = find_content(&model);
        let mutations = vec![ObjectMutation::SetObject {
            num: content,
            value: Obj::Stream {
                dict: vec![(b(b"Length"), Obj::Int(2))],
                data: b(b"q\n"),
            },
        }];
        let error = check_mutation_allowed(&model, &mutations, &bud, &mut g)
            .expect_err("P=1 refuses a content edit");
        assert_eq!(error.code(), Code::SignatureWouldBreak);
        let detail = error.ctx().detail.clone().unwrap_or_default();
        assert!(detail.contains("level 1"), "{detail}");
        assert!(detail.contains("content/page edit"), "{detail}");
        // The save path itself refuses before producing bytes.
        let saved = save_incremental(&src, &model, &mutations, &bud, &mut g);
        assert!(saved.is_err());
    }

    // P=2: an annotation is refused, a form fill is allowed.
    {
        let mut g = guard(&bud);
        let src = docmdp_pdf(2);
        let model = materialise(&src, &bud, &mut g).expect("materialise P=2");
        assert_eq!(
            inspect_signatures(&model, &bud, &mut g)
                .expect("inspect")
                .certification_level,
            Some(DocMdpLevel::FillForms)
        );
        let page = find_page(&model);
        let annot_num = next_num(&model);
        let ann = annotation(square_annot(), annot_num, page, &model);
        let error = check_mutation_allowed(&model, &ann, &bud, &mut g)
            .expect_err("P=2 refuses an annotation");
        assert_eq!(error.code(), Code::SignatureWouldBreak);
        let detail = error.ctx().detail.clone().unwrap_or_default();
        assert!(detail.contains("level 2"), "{detail}");
        assert!(detail.contains("annotation"), "{detail}");
        assert!(save_incremental(&src, &model, &ann, &bud, &mut g).is_err());

        // Filling the text field (object 10) is permitted at P=2.
        let fill = vec![ObjectMutation::SetObject {
            num: 10,
            value: Obj::Dict(vec![
                (b(b"FT"), Obj::Name(b(b"Tx"))),
                (b(b"T"), Obj::String(b(b"Name"))),
                (b(b"V"), Obj::String(b(b"Ada"))),
            ]),
        }];
        check_mutation_allowed(&model, &fill, &bud, &mut g).expect("P=2 allows a form fill");
        let (out, _) =
            save_incremental(&src, &model, &fill, &bud, &mut g).expect("P=2 form fill saves");
        assert_eq!(out.get(..src.len()), Some(src.as_slice()));
    }

    // P=3: the annotation is allowed and saves.
    {
        let mut g = guard(&bud);
        let src = docmdp_pdf(3);
        let model = materialise(&src, &bud, &mut g).expect("materialise P=3");
        assert_eq!(
            inspect_signatures(&model, &bud, &mut g)
                .expect("inspect")
                .certification_level,
            Some(DocMdpLevel::FillAndAnnotate)
        );
        let page = find_page(&model);
        let annot_num = next_num(&model);
        let ann = annotation(square_annot(), annot_num, page, &model);
        check_mutation_allowed(&model, &ann, &bud, &mut g).expect("P=3 allows an annotation");
        let (out, _) =
            save_incremental(&src, &model, &ann, &bud, &mut g).expect("P=3 annotation saves");
        assert_eq!(out.get(..src.len()), Some(src.as_slice()));
        assert_eq!(
            selis_pdf_sign::signatures(&out, &[], 0, &bud, &mut g).expect("report"),
            selis_pdf_sign::signatures(&out, &[], 0, &bud, &mut g).expect("report again")
        );
    }
}

/// A widget whose `/FT` is only on its parent field is still a fillable form
/// field: DocMDP P=2 must allow filling it (the inherited-`/FT` gap).
#[test]
fn docmdp_p2_allows_form_fill_with_inherited_ft() {
    let bud = budget();
    let mut g = guard(&bud);
    let src = docmdp_pdf_inherited_ft(2);
    let model = materialise(&src, &bud, &mut g).expect("materialise P=2 inherited /FT");
    assert_eq!(
        inspect_signatures(&model, &bud, &mut g)
            .expect("inspect")
            .certification_level,
        Some(DocMdpLevel::FillForms)
    );
    // Object 11 has no own /FT; it inherits /Tx from object 10 via /Parent.
    let fill = vec![ObjectMutation::SetObject {
        num: 11,
        value: Obj::Dict(vec![
            (b(b"T"), Obj::String(b(b"Child"))),
            (b(b"V"), Obj::String(b(b"Ada"))),
        ]),
    }];
    check_mutation_allowed(&model, &fill, &bud, &mut g)
        .expect("P=2 allows a fill on a field with inherited /FT");
    let (out, _) =
        save_incremental(&src, &model, &fill, &bud, &mut g).expect("P=2 inherited-/FT fill saves");
    assert_eq!(out.get(..src.len()), Some(src.as_slice()));
}

/// P=1 keeps refusing a fill even when the field's `/FT` is inherited.
#[test]
fn docmdp_p1_still_refuses_form_fill_with_inherited_ft() {
    let bud = budget();
    let mut g = guard(&bud);
    let src = docmdp_pdf_inherited_ft(1);
    let model = materialise(&src, &bud, &mut g).expect("materialise P=1 inherited /FT");
    let fill = vec![ObjectMutation::SetObject {
        num: 11,
        value: Obj::Dict(vec![
            (b(b"T"), Obj::String(b(b"Child"))),
            (b(b"V"), Obj::String(b(b"Ada"))),
        ]),
    }];
    let error = check_mutation_allowed(&model, &fill, &bud, &mut g)
        .expect_err("P=1 refuses a fill with inherited /FT");
    assert_eq!(error.code(), Code::SignatureWouldBreak);
    let detail = error.ctx().detail.clone().unwrap_or_default();
    assert!(detail.contains("level 1"), "{detail}");
}

/// A full rewrite of a signed document is refused: no bytes and no file.
#[test]
fn signed_rewrite_is_refused_with_no_output_and_no_file() {
    let bud = budget();
    let mut g = guard(&bud);
    let src = sign_digital(&base_pdf(), &digital_options(), &bud, &mut g)
        .expect("sign fixture")
        .0;

    let err = save_rewritten(&src, &bud, &mut g)
        .expect_err("full rewrite of a signed document is refused");
    assert_eq!(err.code(), Code::SignatureWouldBreak);
    let detail = format!("{err:?}");
    assert!(detail.contains("signature"), "{detail}");

    let dir = std::env::temp_dir().join(format!("selis-edit07-rewrite-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    let dest = dir.join("rewritten.pdf");
    let _ = std::fs::remove_file(&dest);
    let err2 = save_rewritten_atomic(&dest, &src, &bud, &mut g)
        .expect_err("atomic full rewrite of a signed document is refused");
    assert_eq!(err2.code(), Code::SignatureWouldBreak);
    assert!(!dest.exists(), "no output file created on refusal");
    let _ = std::fs::remove_dir_all(&dir);
}

/// (3) Redefining the signature object itself is refused even without DocMDP.
#[test]
fn redefining_a_signature_object_is_refused() {
    let bud = budget();
    let mut g = guard(&bud);
    let src = sign_digital(&base_pdf(), &digital_options(), &bud, &mut g)
        .expect("sign the fixture")
        .0;
    let model = materialise(&src, &bud, &mut g).expect("model");
    let finding = inspect_signatures(&model, &bud, &mut g).expect("inspect");
    assert!(finding.signature_count >= 1);

    // Object 5 is the signature dictionary in this fixture.
    let mutations = vec![ObjectMutation::SetObject {
        num: 5,
        value: Obj::Dict(vec![(b(b"Type"), Obj::Name(b(b"Sig")))]),
    }];
    let error = check_mutation_allowed(&model, &mutations, &bud, &mut g)
        .expect_err("signature object must be protected");
    assert_eq!(error.code(), Code::SignatureWouldBreak);
    assert!(save_incremental(&src, &model, &mutations, &bud, &mut g).is_err());
}

/// (4) No output on refusal: a caller that only commits on success leaves the
/// destination byte-identical and creates no new file.
#[test]
fn refusal_produces_no_output_and_no_file() {
    let bud = budget();
    let mut g = guard(&bud);
    let src = docmdp_pdf(1);
    let model = materialise(&src, &bud, &mut g).expect("model");
    let content = find_content(&model);
    let mutations = vec![ObjectMutation::SetObject {
        num: content,
        value: Obj::Stream {
            dict: vec![(b(b"Length"), Obj::Int(1))],
            data: b(b"x"),
        },
    }];

    let dir = std::env::temp_dir().join(format!("selis-edit07-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join("document.pdf");
    std::fs::write(&path, &src).expect("write original");

    let result = save_incremental(&src, &model, &mutations, &bud, &mut g);
    assert!(result.is_err(), "refused save returns no bytes");
    // The production pattern: only write when a save returns Ok.
    if let Ok((out, _)) = result {
        std::fs::write(&path, &out).expect("commit");
    }
    assert_eq!(std::fs::read(&path).expect("read back"), src);
    let entries = std::fs::read_dir(&dir)
        .expect("read dir")
        .filter_map(std::result::Result::ok)
        .count();
    assert_eq!(entries, 1, "no temp/output file was created on refusal");
    let _ = std::fs::remove_dir_all(&dir);
}

/// Scriptable interop oracle: `qpdf --check` on a signed+annotated output.
/// Uses `$QPDF` when set, otherwise the known local install, else skips with a
/// note (Acrobat GUI is manual). The qpdf verdict is captured and asserted,
/// not just the process exit status.
#[test]
fn qpdf_check_on_signed_annotated_output() {
    fn qpdf_path() -> Option<std::path::PathBuf> {
        if let Ok(p) = std::env::var("QPDF") {
            let p = std::path::PathBuf::from(p);
            if p.is_file() {
                return Some(p);
            }
        }
        for candidate in [
            r"E:\softwares\pdf-tools\qpdf\qpdf.exe",
            r"C:\Program Files\qpdf 12.4.2\bin\qpdf.exe",
        ] {
            let p = std::path::PathBuf::from(candidate);
            if p.is_file() {
                return Some(p);
            }
        }
        None
    }
    let Some(qpdf) = qpdf_path() else {
        eprintln!("qpdf not found; interop oracle skipped (set QPDF to its path)");
        return;
    };

    let bud = budget();
    let mut g = guard(&bud);
    let src = sign_digital(&base_pdf(), &digital_options(), &bud, &mut g)
        .expect("sign")
        .0;
    let model = materialise(&src, &bud, &mut g).expect("model");
    let page = find_page(&model);
    let mutations = annotation(square_annot(), next_num(&model), page, &model);
    let (out, _) = save_incremental(&src, &model, &mutations, &bud, &mut g).expect("save");

    let path = std::env::temp_dir().join(format!("selis-edit07-signed-{}.pdf", std::process::id()));
    std::fs::write(&path, &out).expect("write output");
    let checked = std::process::Command::new(&qpdf)
        .arg("--check")
        .arg(&path)
        .output()
        .expect("run qpdf --check");
    let verdict = format!(
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&checked.stdout),
        String::from_utf8_lossy(&checked.stderr)
    );
    eprintln!("qpdf --check verdict on signed+annotated output:\n{verdict}");
    assert!(checked.status.success(), "qpdf --check failed: {verdict}");
    // The verdict text, not only the exit code, must report a structurally
    // clean file (qpdf writes this to stdout in this version).
    assert!(
        verdict.contains("No syntax or stream encoding errors found"),
        "unexpected qpdf verdict: {verdict}"
    );
    let _ = std::fs::remove_file(&path);
}
