//! Tests for SL-5.TEXTEDIT.03 box/run-level editing.
//!
//! The load-bearing contract: the warning object is returned, the bytes are
//! sealed until it is acknowledged, the edit does NOT reflow, and the edited
//! document reopens with the replacement text.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss
)]

use selis_pdf_cos::copy::resolve_ref;
use selis_pdf_cos::{Obj, Ref};
use selis_sandbox::Budget;

use crate::box_edit::{edit_box, BoxEditError, BoxEditWarning};

/// A one-page document whose content shows a short line in Helvetica.
fn page_with_content(body: &[u8]) -> Vec<u8> {
    let stream = {
        let mut s = format!("<< /Length {} >>\nstream\n", body.len()).into_bytes();
        s.extend_from_slice(body);
        s.extend_from_slice(b"\nendstream");
        s
    };
    let objects: Vec<(u32, Vec<u8>)> = vec![
        (1, b"<< /Type /Catalog /Pages 2 0 R >>".to_vec()),
        (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec()),
        (
            3,
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 300 200] /Contents 4 0 R /Resources << /Font << /F1 5 0 R >> >> >>".to_vec(),
        ),
        (4, stream),
        (5, b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_vec()),
    ];
    let max = objects.iter().map(|(n, _)| *n).max().unwrap_or(0);
    let mut out = b"%PDF-1.7\n".to_vec();
    let mut offsets = vec![0u64; max as usize + 1];
    for (num, body) in &objects {
        offsets[*num as usize] = out.len() as u64;
        out.extend_from_slice(format!("{num} 0 obj\n").as_bytes());
        out.extend_from_slice(body);
        out.extend_from_slice(b"\nendobj\n");
    }
    let xref_at = out.len() as u64;
    out.extend_from_slice(format!("xref\n0 {}\n", max + 1).as_bytes());
    out.extend_from_slice(b"0000000000 65535 f \n");
    for i in 1..=max {
        out.extend_from_slice(format!("{:010} 00000 n \n", offsets[i as usize]).as_bytes());
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

/// Re-open the edited bytes and read the page's content stream — the read-back
/// oracle for "the edited PDF reopens, text correct".
fn read_back_content(out: &[u8]) -> String {
    let budget = Budget::unlimited();
    let mut g = budget.guard();
    let start = selis_pdf_cos::xref::find_startxref(out, 4096).expect("startxref");
    let doc = selis_pdf_cos::parse_revisions(out, start, &budget, &mut g).expect("parse");
    let Obj::Stream { data, .. } =
        resolve_ref(out, &doc, Ref::new(4, 0), &budget, &mut g).expect("content stream")
    else {
        panic!("content is not a stream");
    };
    String::from_utf8_lossy(&data).into_owned()
}

/// (c) The warning is returned, and bytes are refused without acknowledging it.
#[test]
fn box_edit_returns_warning_and_refuses_without_acknowledgement() {
    let src = page_with_content(b"BT /F1 12 Tf 20 160 Td (Hello world) Tj ET");
    let budget = Budget::unlimited();
    let mut g = budget.guard();
    let preview = edit_box(&src, 0, "Hello", "Goodbye", &budget, &mut g).expect("box edit");

    // The warning is a typed object with the right code and message.
    assert_eq!(preview.warning().code, BoxEditWarning::CODE);
    assert_eq!(preview.warning().code, "BOX_EDIT_NO_REFLOW");
    assert!(
        preview.warning().message().contains("did not reflow"),
        "message: {}",
        preview.warning()
    );

    // A different warning's acknowledgement must be refused — there is no way
    // to take the bytes without the ack derived from THIS warning.
    let other_src = page_with_content(b"BT /F1 12 Tf 20 160 Td (Other line) Tj ET");
    let other = edit_box(&other_src, 0, "Other", "New", &budget, &mut g).expect("other edit");
    let wrong_ack = other.warning().acknowledge();
    assert_eq!(
        preview.commit(wrong_ack).unwrap_err(),
        BoxEditError::WarningNotAcknowledged,
        "an unacknowledged box edit must be refused"
    );

    // Acknowledging the right warning releases the bytes.
    let committed = preview
        .commit(preview.warning().acknowledge())
        .expect("acknowledged");
    assert!(
        committed.bytes.len() > src.len(),
        "an incremental update appends"
    );
    assert_eq!(committed.warning.code, BoxEditWarning::CODE);
}

/// The box edit splices in place: the following text is NOT pushed along by a
/// translation, unlike the full reflowing edit.
#[test]
fn box_edit_does_not_reflow_the_line() {
    let src = page_with_content(b"BT /F1 12 Tf 20 160 Td (Total:) Tj 30 0 Td (USD) Tj ET");

    // The full edit pushes the following text along by a real width.
    let budget = Budget::unlimited();
    let mut g = budget.guard();
    let (full, _) =
        crate::edit_text(&src, 0, "Total:", "Total amount:", &budget, &mut g).expect("full edit");
    let full_text = read_back_content(&full);
    assert!(
        full_text.contains("0 Td 30 0 Td (USD)"),
        "the full edit reflows: {full_text}"
    );

    // The box edit leaves the following positioning operator untouched.
    let mut g = budget.guard();
    let preview = edit_box(&src, 0, "Total:", "Total amount:", &budget, &mut g).expect("box edit");
    let committed = preview
        .commit(preview.warning().acknowledge())
        .expect("ack");
    let box_text = read_back_content(&committed.bytes);
    assert!(
        box_text.contains("(Total amount:)"),
        "the edit landed: {box_text}"
    );
    assert!(
        !box_text.contains("0 Td 30 0 Td (USD)"),
        "a box edit must not insert a reflow shift: {box_text}"
    );
}

/// Read-back: the edited document reopens and carries the replacement text.
#[test]
fn edited_pdf_reopens_with_the_replacement_text() {
    let src = page_with_content(b"BT /F1 12 Tf 20 160 Td (Hello world) Tj ET");
    let budget = Budget::unlimited();
    let mut g = budget.guard();
    let preview = edit_box(&src, 0, "Hello", "Goodbye", &budget, &mut g).expect("box edit");
    let committed = preview
        .commit(preview.warning().acknowledge())
        .expect("ack");
    let text = read_back_content(&committed.bytes);
    assert!(
        text.contains("(Goodbye world)"),
        "the replacement is in the reopened document: {text}"
    );
    assert_eq!(committed.report.replaced, 1);
}
