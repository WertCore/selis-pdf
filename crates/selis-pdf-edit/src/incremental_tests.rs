//! Tests for the object-level incremental writer (EDIT.03).

use selis_bytes::Bytes;
use selis_pdf_cos::Obj;
use selis_sandbox::Budget;

use crate::incremental::{apply, materialise, save_incremental, ObjectMutation};

fn base_pdf() -> Vec<u8> {
    // 1=catalog, 2=pages, 3=page, 4=contents, 5=info.
    let objects: Vec<(u32, &str)> = vec![
        (1, "<< /Type /Catalog /Pages 2 0 R >>"),
        (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
        (
            3,
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R >>",
        ),
        (4, "<< /Length 4 >>\nstream\nBT\nET\nendstream"),
        (5, "<< /Title (base) >>"),
    ];
    let mut body = String::from("%PDF-1.4\n");
    let mut offsets = Vec::new();
    for (num, val) in &objects {
        offsets.push((*num, body.len() as u64));
        body.push_str(&format!("{num} 0 obj\n{val}\nendobj\n"));
    }
    let xref = body.len();
    body.push_str(&format!("xref\n0 {}\n", objects.len() + 1));
    body.push_str("0000000000 65535 f \n");
    for i in 0..objects.len() {
        let off = offsets.get(i).map(|(_, o)| *o).unwrap_or(0);
        body.push_str(&format!("{off:010} 00000 n \n"));
    }
    body.push_str(&format!(
        "trailer\n<< /Size {} /Root 1 0 R /Info 5 0 R /ID [<0102030405060708090a0b0c0d0e0f10> <0102030405060708090a0b0c0d0e0f10>] >>\nstartxref\n{xref}\n%%EOF\n",
        objects.len() + 1
    ));
    body.into_bytes()
}

fn budget() -> Budget {
    Budget::unlimited()
}

#[test]
fn set_object_preserves_prefix_and_updates_model() {
    let src = base_pdf();
    let budget = budget();
    let mut g = budget.guard();
    let model = materialise(&src, &budget, &mut g).expect("materialise");
    let new_title = Obj::Dict(vec![(
        Bytes::copy_from_slice(b"Title"),
        Obj::String(Bytes::copy_from_slice(b"edited")),
    )]);
    let mutations = vec![ObjectMutation::SetObject {
        num: 5,
        value: new_title.clone(),
    }];
    let expected = apply(&model, &mutations);

    let (out, report) = save_incremental(&src, &model, &mutations, &budget, &mut g).expect("save");
    assert!(
        report.prefix_preserved,
        "I3: original bytes are a verbatim prefix"
    );
    assert_eq!(report.objects_changed, 1);
    assert_eq!(report.revisions, 2, "one appended revision");
    assert!(out.len() > src.len(), "append grows the file");

    let mut g2 = budget.guard();
    let reopened = materialise(&out, &budget, &mut g2).expect("reopen");
    assert_eq!(reopened, expected, "I7: only object 5 changed");

    // The appended trailer carries /ID with a refreshed second element.
    let text = String::from_utf8_lossy(&out[src.len()..]);
    assert!(text.contains("/Prev "), "trailer has /Prev");
    assert!(text.contains("/ID"), "trailer carries /ID");
}

#[test]
fn net_noop_mutation_returns_source_unchanged() {
    let src = base_pdf();
    let budget = budget();
    let mut g = budget.guard();
    let model = materialise(&src, &budget, &mut g).expect("materialise");
    let original = model.get(&5).cloned().expect("object 5");
    let mutations = vec![
        ObjectMutation::SetObject {
            num: 5,
            value: Obj::Int(42),
        },
        ObjectMutation::SetObject {
            num: 5,
            value: original,
        },
    ];
    let (out, report) = save_incremental(&src, &model, &mutations, &budget, &mut g).expect("save");
    assert_eq!(out, src, "a change and its inverse emit nothing");
    assert_eq!(report.objects_changed, 0);
}

#[test]
fn new_object_allocation_is_appended() {
    let src = base_pdf();
    let budget = budget();
    let mut g = budget.guard();
    let model = materialise(&src, &budget, &mut g).expect("materialise");
    let mutations = vec![ObjectMutation::SetObject {
        num: 9,
        value: Obj::Dict(vec![(
            Bytes::copy_from_slice(b"Type"),
            Obj::Name(Bytes::copy_from_slice(b"Custom")),
        )]),
    }];
    let (out, report) = save_incremental(&src, &model, &mutations, &budget, &mut g).expect("save");
    assert!(report.prefix_preserved);
    let mut g2 = budget.guard();
    let reopened = materialise(&out, &budget, &mut g2).expect("reopen");
    assert!(
        reopened.contains_key(&9),
        "the new object resolves in the output"
    );
    assert_eq!(reopened.get(&9), apply(&model, &mutations).get(&9));
}

#[test]
fn delete_object_emits_a_free_entry_and_removes_it_from_the_model() {
    let src = base_pdf();
    let budget = budget();
    let mut g = budget.guard();
    let model = materialise(&src, &budget, &mut g).expect("materialise");
    let mutations = vec![ObjectMutation::DeleteObject { num: 5 }];
    let (out, report) = save_incremental(&src, &model, &mutations, &budget, &mut g).expect("save");
    assert!(report.prefix_preserved);
    assert_eq!(report.objects_changed, 1);
    let mut g2 = budget.guard();
    let reopened = materialise(&out, &budget, &mut g2).expect("reopen");
    assert!(
        !reopened.contains_key(&5),
        "the freed object no longer resolves"
    );
    let appended = String::from_utf8_lossy(&out[src.len()..]);
    assert!(appended.contains("f \n"), "a free xref entry was emitted");
}

#[test]
fn every_prior_revision_is_readable() {
    let src = base_pdf();
    let budget = budget();
    let mut g = budget.guard();
    let model = materialise(&src, &budget, &mut g).expect("materialise");
    // Two successive saves: the second must preserve BOTH revisions' bytes.
    let m1 = vec![ObjectMutation::SetObject {
        num: 5,
        value: Obj::Int(1),
    }];
    let (mid, _) = save_incremental(&src, &model, &m1, &budget, &mut g).expect("save 1");
    let mut g1 = budget.guard();
    let mid_model = materialise(&mid, &budget, &mut g1).expect("materialise mid");
    let m2 = vec![ObjectMutation::SetObject {
        num: 5,
        value: Obj::Int(2),
    }];
    let (final_bytes, report) =
        save_incremental(&mid, &mid_model, &m2, &budget, &mut g).expect("save 2");
    assert!(final_bytes[..mid.len()] == mid, "I3 across a second save");
    assert!(
        final_bytes[..src.len()] == src,
        "original revision still a prefix"
    );
    assert_eq!(report.revisions, 3, "original + two appended revisions");
}
