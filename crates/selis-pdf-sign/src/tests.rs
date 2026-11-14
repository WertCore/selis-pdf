//! End-to-end tests over hand-built PDFs and real PEM fixtures.
//!
//! The fixtures are the OpenSSL-generated pair under `tests/fixtures`: a root
//! CA, a leaf signer, and a rogue self-signed certificate. They are test-only;
//! the crate never ships key material.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects
)]

use super::*;
use selis_sandbox::{Budget, Surface};

const ROOT: &str = include_str!("../tests/fixtures/root.crt");
const LEAF: &str = include_str!("../tests/fixtures/leaf.crt");
const LEAF_KEY: &str = include_str!("../tests/fixtures/leaf.key");
const ROGUE: &str = include_str!("../tests/fixtures/rogue.crt");
const EC_KEY: &str = include_str!("../tests/fixtures/ec.key");

const SIGNING_TIME: &str = "20261104120000Z";
/// `2026-11-04T12:00:00Z`.
const NOW: i64 = 1_793_793_600;

/// A minimal one-page PDF from raw object bodies, with a correct xref.
fn build_pdf(objects: &[(u32, &[u8])], trailer_extra: &str) -> Vec<u8> {
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
            "trailer\n<< /Size {} /Root 1 0 R{} >>\nstartxref\n{xref_at}\n%%EOF\n",
            max + 1,
            trailer_extra
        )
        .as_bytes(),
    );
    out
}

/// One page, an empty content stream, optionally rotated.
fn base_pdf(rotate: Option<i64>) -> Vec<u8> {
    let page = match rotate {
        Some(degrees) => format!(
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Rotate {degrees} \
             /Resources << >> /Contents 4 0 R >>"
        ),
        None => "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << >> \
                 /Contents 4 0 R >>"
            .to_string(),
    };
    let objects: Vec<(u32, Vec<u8>)> = vec![
        (1, b"<< /Type /Catalog /Pages 2 0 R >>".to_vec()),
        (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec()),
        (3, page.into_bytes()),
        (4, b"<< /Length 0 >>\nstream\n\nendstream".to_vec()),
    ];
    let borrowed: Vec<(u32, &[u8])> = objects
        .iter()
        .map(|(num, body)| (*num, body.as_slice()))
        .collect();
    build_pdf(&borrowed, "")
}

fn pem_der(pem: &str) -> Vec<u8> {
    selis_crypto::cert::pem_body(pem, "CERTIFICATE").expect("fixture PEM decodes")
}

fn digital_options(field: &str) -> DigitalOptions {
    DigitalOptions {
        page: 1,
        rect: [72.0, 600.0, 320.0, 690.0],
        field_name: field.to_string(),
        signer_name: "Ada Lovelace".to_string(),
        reason: Some("Approved".to_string()),
        location: Some("London".to_string()),
        signing_time: SIGNING_TIME.to_string(),
        key_pem: LEAF_KEY.to_string(),
        certificates_pem: vec![LEAF.to_string(), ROOT.to_string()],
    }
}

fn visual_options() -> VisualOptions {
    VisualOptions {
        page: 1,
        rect: [72.0, 600.0, 320.0, 690.0],
        lines: vec![
            VisualLine::new("Ada Lovelace"),
            VisualLine::new("2026-11-04"),
        ],
        strokes: vec![VisualStroke {
            points: vec![(0.05, 0.2), (0.3, 0.7), (0.55, 0.3), (0.9, 0.6)],
        }],
        ..VisualOptions::default()
    }
}

/// THE COLLISION, REPRODUCED.
///
/// An incremental update writes only the objects it changes, so a
/// document that was edited and then stamped has a newest revision whose
/// table holds ONE entry. Numbering the stamp's objects from that table
/// alone hands out a number the FIRST revision already used - and the
/// stamp's content stream lands on the page's FONT.
///
/// The page still draws its text (the renderer substitutes a face), but
/// the font's /Widths are gone, so the text layer measures every line at
/// a fraction of its real width and the line becomes unclickable.
#[test]
fn a_stamp_after_an_edit_does_not_overwrite_the_page_font() {
    // A page with a font, drawn through a content stream.
    let objects: Vec<(u32, Vec<u8>)> = vec![
            (1, b"<< /Type /Catalog /Pages 2 0 R >>".to_vec()),
            (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec()),
            (
                3,
                b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 300 200] /Resources << /Font << /F1 5 0 R >> >> /Contents 4 0 R >>"
                    .to_vec(),
            ),
            (
                4,
                b"<< /Length 34 >>
stream
BT /F1 12 Tf 10 100 Td (Hi) Tj ET
endstream".to_vec(),
            ),
            (
                5,
                b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_vec(),
            ),
        ];
    let borrowed: Vec<(u32, &[u8])> = objects
        .iter()
        .map(|(num, body)| (*num, body.as_slice()))
        .collect();
    let src = build_pdf(&borrowed, "");

    // The edit: an incremental update that rewrites the content stream
    // and nothing else. This is what the editor's own text edit writes.
    let budget = Budget::unlimited();
    let mut g = budget.guard();
    let edited = selis_pdf_cos::doc_writer::write_incremental_update(
        &src,
        &[(
            4,
            Obj::Stream {
                dict: vec![(crate::bytes(b"Length"), Obj::Int(34))],
                data: selis_bytes::Bytes::copy_from_slice(b"BT /F1 12 Tf 10 100 Td (Ho) Tj ET"),
            },
        )],
        &[(b"Root".to_vec(), Obj::Ref(Ref::new(1, 0)))],
        &budget,
        &mut g,
    )
    .expect("the edit");

    // The stamp: the same stamp, on the edited document.
    let mut options = visual_options();
    options.rect = [10.0, 100.0, 200.0, 160.0];
    options.lines = vec![VisualLine::new("A stamp")];
    options.strokes = Vec::new();
    let (stamped, _) = sign_visual(&edited, &options, &budget, &mut g).expect("the stamp");

    // The page's font must still be a FONT. Before the fix the stamp's
    // content stream was object 5, and /F1 pointed at it.
    let doc = crate::open_doc(&stamped, &budget, &mut g).expect("the stamped document");
    let font =
        resolve(&stamped, &doc, Obj::Ref(Ref::new(5, 0)), &budget, &mut g).expect("object 5");
    match font {
        Obj::Dict(pairs) => {
            let is_font = pairs.iter().any(|(key, value)| {
                key.as_slice() == b"Type"
                    && matches!(value, Obj::Name(name) if name.as_slice() == b"Font")
            });
            assert!(is_font, "object 5 is no longer the page's font");
        }
        other => panic!("object 5 is not a dictionary: {other:?}"),
    }
}

#[test]
fn a_bold_italic_line_is_drawn_in_the_bold_italic_face() {
    // The face is NAMED, not embedded: the standard fourteen are in every
    // reader, and a document that asks for a bold line costs a dictionary
    // rather than a font.
    let mut options = visual_options();
    options.lines = vec![VisualLine {
        text: "Bold".to_string(),
        size: 12.0,
        bold: true,
        italic: true,
        underline: false,
    }];
    let src = base_pdf(None);
    let budget = Budget::unlimited();
    let mut g = budget.guard();
    let (out, _) = sign_visual(&src, &options, &budget, &mut g).expect("the stamp");
    let text = String::from_utf8_lossy(&out);
    assert!(
        text.contains("Helvetica-BoldOblique"),
        "the bold italic face is in the page's resources: {text}"
    );
    assert!(
        text.contains("/SelisSigFBI"),
        "the line is drawn in the bold italic face: {text}"
    );
}

#[test]
fn an_underlined_line_is_underlined() {
    // The underline is DRAWN, the way a text decoration is drawn in PDF: a
    // flag no reader honours would be a decoration on paper.
    let mut options = visual_options();
    options.lines = vec![VisualLine {
        text: "Under".to_string(),
        size: 12.0,
        bold: false,
        italic: false,
        underline: true,
    }];
    let src = base_pdf(None);
    let budget = Budget::unlimited();
    let mut g = budget.guard();
    let (out, _) = sign_visual(&src, &options, &budget, &mut g).expect("the stamp");
    let text = String::from_utf8_lossy(&out);
    assert!(text.contains(" m\n"), "the underline is drawn: {text}");
    assert!(text.contains(" l\n"), "the underline is drawn: {text}");
    assert!(text.contains("S\n"), "the underline is stroked: {text}");
}

#[test]
fn a_plain_line_is_drawn_in_the_plain_face() {
    let mut options = visual_options();
    options.lines = vec![VisualLine {
        text: "Plain".to_string(),
        size: 12.0,
        bold: false,
        italic: false,
        underline: false,
    }];
    let src = base_pdf(None);
    let budget = Budget::unlimited();
    let mut g = budget.guard();
    let (out, _) = sign_visual(&src, &options, &budget, &mut g).expect("the stamp");
    let text = String::from_utf8_lossy(&out);
    assert!(
        text.contains("/SelisSigF 12 Tf"),
        "the plain face is used: {text}"
    );
}
#[test]
fn a_visual_stamp_appends_a_revision_and_preserves_the_prefix() {
    let src = base_pdf(None);
    let budget = Budget::profile(Surface::Viewer);
    let mut g = budget.guard();
    let (out, report) = sign_visual(&src, &visual_options(), &budget, &mut g).expect("stamps");
    assert!(out.starts_with(&src), "the original bytes stay the prefix");
    assert_eq!(report.lines, 2);
    assert_eq!(report.strokes, 1);
    assert_eq!(report.display_size, [612, 792]);
    assert!(report.stream_bytes > 0);
    assert!(
        out.windows(12).any(|window| window == b"Ada Lovelace"),
        "the typed line is in the appended stream"
    );
    assert!(
        out.windows(16).any(|window| window == b"/SelisSigF 11 Tf"),
        "the stream selects the built-in font"
    );
    // The stamp is an addition, not a rewrite: one more revision.
    let startxref = selis_pdf_cos::xref::find_startxref(&out, 4096).expect("startxref");
    let doc = selis_pdf_cos::parse_revisions(&out, startxref, &budget, &mut g).expect("parses");
    assert_eq!(doc.revisions().len(), 2);
}

#[test]
fn a_visual_stamp_keeps_text_upright_on_a_rotated_page() {
    for (rotation, matrix) in [
        (0i64, "1 0 0 1"),
        (90, "0 1 -1 0"),
        (180, "-1 0 0 -1"),
        (270, "0 -1 1 0"),
    ] {
        let src = base_pdf(Some(rotation));
        let budget = Budget::profile(Surface::Viewer);
        let mut g = budget.guard();
        let (out, _) = sign_visual(&src, &visual_options(), &budget, &mut g).expect("stamps");
        assert!(
            out.windows(matrix.len())
                .any(|window| window == matrix.as_bytes()),
            "rotation {rotation} should place text with matrix {matrix}"
        );
    }
}

#[test]
fn a_visual_stamp_refuses_an_empty_request_and_a_missing_page() {
    let src = base_pdf(None);
    let budget = Budget::profile(Surface::Viewer);
    let mut g = budget.guard();
    let empty = VisualOptions {
        lines: Vec::new(),
        strokes: Vec::new(),
        ..VisualOptions::default()
    };
    assert!(matches!(
        sign_visual(&src, &empty, &budget, &mut g),
        Err(SignError::Invalid(_))
    ));
    let mut missing = visual_options();
    missing.page = 4;
    assert!(matches!(
        sign_visual(&src, &missing, &budget, &mut g),
        Err(SignError::Invalid(_))
    ));
}

#[test]
fn a_visual_stamp_refuses_text_outside_winansi() {
    let src = base_pdf(None);
    let budget = Budget::profile(Surface::Viewer);
    let mut g = budget.guard();
    let mut options = visual_options();
    options.lines.push(VisualLine::new("签名"));
    assert!(matches!(
        sign_visual(&src, &options, &budget, &mut g),
        Err(SignError::Invalid(_))
    ));
}

#[test]
fn an_encrypted_document_is_refused_by_both_writers() {
    let objects: &[(u32, &[u8])] = &[
        (1, b"<< /Type /Catalog /Pages 2 0 R >>"),
        (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
        (
            3,
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << >> /Contents 4 0 R >>",
        ),
        (4, b"<< /Length 0 >>\nstream\n\nendstream"),
        (5, b"<< /Filter /Standard /V 1 /R 2 >>"),
    ];
    let src = build_pdf(objects, " /Encrypt 5 0 R");
    let budget = Budget::profile(Surface::Viewer);
    let mut g = budget.guard();
    assert!(matches!(
        sign_visual(&src, &visual_options(), &budget, &mut g),
        Err(SignError::Encrypted)
    ));
    assert!(matches!(
        sign_digital(&src, &digital_options("Signature1"), &budget, &mut g),
        Err(SignError::Encrypted)
    ));
}

#[test]
fn a_signature_verifies_and_reports_its_chain() {
    let src = base_pdf(None);
    let budget = Budget::profile(Surface::Viewer);
    let mut g = budget.guard();
    let (out, report) =
        sign_digital(&src, &digital_options("Signature1"), &budget, &mut g).expect("signs");
    assert!(out.starts_with(&src), "the original bytes stay the prefix");
    assert!(report.verified);
    assert_eq!(report.byte_range[0], 0);
    assert_eq!(report.sub_filter, "adbe.pkcs7.detached");
    assert_eq!(report.digest, "SHA-256");
    assert_eq!(report.signer_certificate, "Selis Test Signer");
    assert!(report.cms_bytes > 0);

    // With the root supplied as an anchor the chain is trusted.
    let read = signatures(&out, &[pem_der(ROOT)], NOW, &budget, &mut g).expect("reports");
    assert_eq!(read.signatures.len(), 1);
    let signature = &read.signatures[0];
    assert_eq!(signature.field, "Signature1");
    assert!(signature.verified);
    assert!(signature.covers_whole_document);
    assert_eq!(signature.signer.as_deref(), Some("Ada Lovelace"));
    assert_eq!(signature.reason.as_deref(), Some("Approved"));
    assert_eq!(signature.location.as_deref(), Some("London"));
    assert_eq!(
        signature.signing_time.as_deref(),
        Some("D:20261104120000+00'00'")
    );
    let trust = signature.trust.as_ref().expect("a certificate was found");
    assert!(trust.trusted, "reason: {:?}", trust.reason);
    assert_eq!(trust.chain.len(), 2);
    assert!(!trust.revocation_checked);
    assert!(!read.revocation_checked);

    // With no anchors supplied the same signature is valid but untrusted.
    let unanchored = signatures(&out, &[], NOW, &budget, &mut g).expect("reports");
    let trust = unanchored.signatures[0]
        .trust
        .as_ref()
        .expect("certificate");
    assert!(!trust.trusted);
    assert!(trust.reason.is_some());
}

#[test]
fn signing_an_already_signed_document_keeps_the_first_signature_valid() {
    let src = base_pdf(None);
    let budget = Budget::profile(Surface::Viewer);
    let mut g = budget.guard();
    let (first, _) =
        sign_digital(&src, &digital_options("Signature1"), &budget, &mut g).expect("first");
    let (second, _) =
        sign_digital(&first, &digital_options("Signature2"), &budget, &mut g).expect("second");
    assert!(second.starts_with(&first));

    let read = signatures(&second, &[pem_der(ROOT)], NOW, &budget, &mut g).expect("reports");
    assert_eq!(read.signatures.len(), 2);
    assert!(read.signatures[0].verified);
    assert!(read.signatures[1].verified);
    // The first signature still covers the bytes it signed; it no longer
    // covers the whole file, because a revision was appended after it.
    assert!(!read.signatures[0].covers_whole_document);
    assert!(read.signatures[1].covers_whole_document);
}

#[test]
fn appending_a_byte_after_signing_is_reported_as_uncovered() {
    let src = base_pdf(None);
    let budget = Budget::profile(Surface::Viewer);
    let mut g = budget.guard();
    let (mut out, _) =
        sign_digital(&src, &digital_options("Signature1"), &budget, &mut g).expect("signs");
    out.push(b'X');
    let read = signatures(&out, &[pem_der(ROOT)], NOW, &budget, &mut g).expect("reports");
    assert!(read.signatures[0].verified);
    assert!(!read.signatures[0].covers_whole_document);
}

#[test]
fn changing_a_byte_inside_the_signed_range_breaks_verification() {
    let src = base_pdf(None);
    let budget = Budget::profile(Surface::Viewer);
    let mut g = budget.guard();
    let (mut out, _) =
        sign_digital(&src, &digital_options("Signature1"), &budget, &mut g).expect("signs");
    let at = out
        .windows(3)
        .position(|window| window == b"612")
        .expect("the media box is present");
    out[at] = b'6';
    out[at + 1] = b'1';
    out[at + 2] = b'3';
    let read = signatures(&out, &[pem_der(ROOT)], NOW, &budget, &mut g).expect("reports");
    assert!(!read.signatures[0].verified);
    assert!(read.signatures[0].error.is_some());
}

#[test]
fn an_unsigned_signature_field_is_counted_not_guessed() {
    let objects: &[(u32, &[u8])] = &[
        (1, b"<< /Type /Catalog /Pages 2 0 R /AcroForm 5 0 R >>"),
        (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
        (
            3,
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << >> /Contents 4 0 R /Annots [6 0 R] >>",
        ),
        (4, b"<< /Length 0 >>\nstream\n\nendstream"),
        (5, b"<< /Fields [6 0 R] /SigFlags 3 >>"),
        (
            6,
            b"<< /FT /Sig /T (Signature1) /Subtype /Widget /Rect [72 600 320 690] /P 3 0 R >>",
        ),
    ];
    let src = build_pdf(objects, "");
    let budget = Budget::profile(Surface::Viewer);
    let mut g = budget.guard();
    let read = signatures(&src, &[], NOW, &budget, &mut g).expect("reports");
    assert!(read.signatures.is_empty());
    assert_eq!(read.unsigned_fields, 1);
}

#[test]
fn an_ec_key_is_refused_by_name() {
    let src = base_pdf(None);
    let budget = Budget::profile(Surface::Viewer);
    let mut g = budget.guard();
    let mut options = digital_options("Signature1");
    options.key_pem = EC_KEY.to_string();
    match sign_digital(&src, &options, &budget, &mut g) {
        Err(SignError::Unsupported(detail)) => assert!(detail.contains("EC")),
        other => panic!("expected an EC refusal, got {other:?}"),
    }
}

#[test]
fn a_key_that_does_not_match_the_certificate_is_refused() {
    let src = base_pdf(None);
    let budget = Budget::profile(Surface::Viewer);
    let mut g = budget.guard();
    let mut options = digital_options("Signature1");
    options.certificates_pem = vec![ROGUE.to_string()];
    match sign_digital(&src, &options, &budget, &mut g) {
        Err(SignError::Invalid(detail)) => assert!(detail.contains("does not match")),
        other => panic!("expected a key mismatch, got {other:?}"),
    }
}

#[test]
fn a_bad_signing_time_is_refused_before_a_byte_is_written() {
    let src = base_pdf(None);
    let budget = Budget::profile(Surface::Viewer);
    let mut g = budget.guard();
    let mut options = digital_options("Signature1");
    options.signing_time = "2026-11-04T12:00:00Z".to_string();
    assert!(matches!(
        sign_digital(&src, &options, &budget, &mut g),
        Err(SignError::Invalid(_))
    ));
}

#[test]
fn the_report_serialises_to_json() {
    let src = base_pdf(None);
    let budget = Budget::profile(Surface::Viewer);
    let mut g = budget.guard();
    let (out, _) =
        sign_digital(&src, &digital_options("Signature1"), &budget, &mut g).expect("signs");
    let read = signatures(&out, &[pem_der(ROOT)], NOW, &budget, &mut g).expect("reports");
    let json = signature_report_json(&read).expect("json");
    let text = String::from_utf8(json).expect("utf8");
    assert!(text.contains("\"verified\":true"));
    assert!(text.contains("\"revocationChecked\":false"));
    assert!(text.contains("Signature1"));
}
