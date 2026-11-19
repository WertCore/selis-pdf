//! Tests for SL-5.TEXTEDIT.02's byte-region writer (the engine-level, render
//! and new-glyph evidence lives in `selis-pdf-engine`'s reflow tests).

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

use crate::reflow_edit::{
    ensure_reflow_confidence, reflow_regions, ContentRegion, ReflowEditError, ReflowMutation,
    MIN_REFLOW_CONFIDENCE,
};

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
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R /Resources << >> >>".to_vec(),
        ),
        (4, stream),
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

fn read_back_content(out: &[u8]) -> Vec<u8> {
    let budget = Budget::unlimited();
    let mut g = budget.guard();
    let start = selis_pdf_cos::xref::find_startxref(out, 4096).expect("startxref");
    let doc = selis_pdf_cos::parse_revisions(out, start, &budget, &mut g).expect("parse");
    let Obj::Stream { data, .. } =
        resolve_ref(out, &doc, Ref::new(4, 0), &budget, &mut g).expect("content stream")
    else {
        panic!("content is not a stream");
    };
    data.as_slice().to_vec()
}

/// (a) The byte-region writer changes ONLY the declared span; every other byte
/// of the decoded content stream is identical, and the original file is a
/// verbatim prefix of the output (I3).
#[test]
fn a_region_edit_changes_only_the_declared_byte_span() {
    let before = b"BT /F1 12 Tf 72 800 Td (First paragraph line one) Tj ET\nBT /F1 12 Tf 72 786 Td (First paragraph line two) Tj ET\nBT /F1 12 Tf 72 700 Td (Second paragraph line one) Tj ET";
    let src = page_with_content(before);

    // The first line's show operand spans exactly these bytes.
    let span_start = before
        .windows(b"(First paragraph line one)".len())
        .position(|w| w == b"(First paragraph line one)")
        .expect("find operand");
    let span_end = span_start + b"(First paragraph line one)".len();
    let replacement = b"(First paragraph line UNO)".to_vec();

    let budget = Budget::unlimited();
    let mut g = budget.guard();
    let (out, report) = reflow_regions(
        &src,
        0,
        &[ReflowMutation::Content(ContentRegion {
            span: span_start..span_end,
            replacement: replacement.clone(),
        })],
        &budget,
        &mut g,
    )
    .expect("reflow");

    // I3: the original bytes are a verbatim prefix of the incremental update.
    assert_eq!(
        &out[..src.len()],
        src.as_slice(),
        "original bytes are the prefix"
    );

    let after = read_back_content(&out);
    assert_eq!(
        after.len(),
        before.len() - (span_end - span_start) + replacement.len()
    );
    // Everything before and after the span is byte-identical.
    assert_eq!(&after[..span_start], &before[..span_start]);
    let tail_start = span_start + replacement.len();
    assert_eq!(&after[tail_start..], &before[span_end..]);
    // The replacement landed, and the OTHER paragraph is untouched.
    assert!(after
        .windows(replacement.len())
        .any(|w| w == replacement.as_slice()));
    assert!(after
        .windows(b"Second paragraph".len())
        .any(|w| w == b"Second paragraph"));

    assert_eq!(report.changed_regions, vec![span_start..span_end]);
    assert!(report.bytes_out >= report.bytes_in);
    assert!(report.objects_changed >= 1);
}

/// Out-of-range and overlapping regions are refused before any bytes are written.
#[test]
fn malformed_regions_are_refused() {
    let src = page_with_content(b"BT /F1 12 Tf 72 800 Td (Hi) Tj ET");
    let budget = Budget::unlimited();
    let mut g = budget.guard();
    let err = reflow_regions(
        &src,
        0,
        &[ReflowMutation::Content(ContentRegion {
            span: 0..10_000,
            replacement: b"(x)".to_vec(),
        })],
        &budget,
        &mut g,
    )
    .expect_err("out of range");
    assert!(matches!(
        err,
        ReflowEditError::Edit(crate::EditError::Invalid(_))
    ));

    let mut g = budget.guard();
    let err = reflow_regions(
        &src,
        0,
        &[
            ReflowMutation::Content(ContentRegion {
                span: 4..10,
                replacement: b"a".to_vec(),
            }),
            ReflowMutation::Content(ContentRegion {
                span: 8..14,
                replacement: b"b".to_vec(),
            }),
        ],
        &budget,
        &mut g,
    )
    .expect_err("overlapping / unsorted");
    assert!(matches!(
        err,
        ReflowEditError::Edit(crate::EditError::Invalid(_))
    ));
}

/// The confidence gate refuses a low-confidence reconstruction and points at
/// the box-level fallback.
#[test]
fn low_confidence_is_refused_by_the_gate() {
    assert!(ensure_reflow_confidence(MIN_REFLOW_CONFIDENCE).is_ok());
    assert!(ensure_reflow_confidence(0.55).is_err());
    assert!(ensure_reflow_confidence(f64::NAN).is_err());
    match ensure_reflow_confidence(0.42) {
        Err(ReflowEditError::LowConfidence {
            confidence,
            threshold,
        }) => {
            assert_eq!(confidence, 0.42);
            assert_eq!(threshold, MIN_REFLOW_CONFIDENCE);
            let msg = ReflowEditError::LowConfidence {
                confidence,
                threshold,
            }
            .to_string();
            assert!(msg.contains("box-level editing"), "message: {msg}");
        }
        other => panic!("expected LowConfidence, got {other:?}"),
    }
}
