//! SL-5.TEXTEDIT.02 end-to-end evidence: byte region, render parity outside
//! the paragraph, a new glyph rendering after subset extension, and the
//! low-confidence refusal that points at the box path.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::float_cmp
)]

use selis_geom::Rect;
use selis_pdf_cos::copy::resolve_ref;
use selis_pdf_cos::Obj;
use selis_pdf_engine::{reflow_paragraph, NewGlyph, ReflowError, RenderedPage, Session};
use selis_sandbox::{Budget, CancelToken, FixedClock, Surface};

const TWO_PARAGRAPHS: &[u8] =
    include_bytes!("../../../corpus/fixtures/paragraphs/two_paragraphs.pdf");
const MINI_TTF: &[u8] = include_bytes!("../../selis-font/tests/fixtures/mini.ttf");

fn budget() -> Budget {
    Budget::profile(Surface::Viewer)
}

fn open(src: &[u8]) -> Session {
    Session::open(src.to_vec(), &budget(), &FixedClock(0)).expect("open")
}

fn render(src: &[u8]) -> RenderedPage {
    let clock = FixedClock(0);
    let session = open(src);
    let mut g = budget().guard_with(&clock, CancelToken::new());
    session
        .render_page_rgba(0, 72.0, &budget(), &mut g)
        .expect("render")
}

fn decoded_content(src: &[u8]) -> Vec<u8> {
    let b = budget();
    let mut g = b.guard();
    let start = selis_pdf_cos::xref::find_startxref(src, 4096).expect("startxref");
    let doc = selis_pdf_cos::parse_revisions(src, start, &b, &mut g).expect("parse");
    let Obj::Stream { data, .. } =
        resolve_ref(src, &doc, selis_pdf_cos::Ref::new(4, 0), &b, &mut g).expect("content obj")
    else {
        panic!("content is not a stream");
    };
    data.as_slice().to_vec()
}

/// A one-page PDF embedding the mini TrueType subset (A, B) with two
/// paragraphs, so the confidence gate clears and a new glyph can be added.
fn pdf_with_embedded_font(body: &[u8]) -> Vec<u8> {
    let mut stream = format!("<< /Length {} >>\nstream\n", body.len()).into_bytes();
    stream.extend_from_slice(body);
    stream.extend_from_slice(b"\nendstream");
    let mut font_stream = format!("<< /Length {} >>\nstream\n", MINI_TTF.len()).into_bytes();
    font_stream.extend_from_slice(MINI_TTF);
    font_stream.extend_from_slice(b"\nendstream");
    let objects: Vec<(u32, Vec<u8>)> = vec![
        (1, b"<< /Type /Catalog /Pages 2 0 R >>".to_vec()),
        (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec()),
        (
            3,
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R /Resources << /Font << /F1 5 0 R >> >> >>".to_vec(),
        ),
        (4, stream),
        (
            5,
            b"<< /Type /Font /Subtype /TrueType /BaseFont /MiniTest /FirstChar 65 /LastChar 66 /Widths [700 900] /FontDescriptor 6 0 R >>".to_vec(),
        ),
        (
            6,
            b"<< /Type /FontDescriptor /FontName /MiniTest /Flags 32 /MissingWidth 500 /FontFile2 7 0 R >>".to_vec(),
        ),
        (7, font_stream),
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

/// Mask a PDF user-space rectangle out of two RGBA renders, then compare every
/// other pixel. The mask is in top-down raster coordinates at 72 dpi.
fn assert_render_equal_outside(a: &RenderedPage, b: &RenderedPage, rect: Rect) {
    assert_eq!((a.width, a.height), (b.width, b.height));
    let h = a.height as i64;
    let x0 = (rect.x0 as i64).max(0);
    let x1 = (rect.x1 as i64).min(a.width as i64);
    let y0 = (h - rect.y1 as i64).max(0); // top edge
    let y1 = (h - rect.y0 as i64).min(h); // bottom edge
    let mut compared = 0usize;
    for py in 0..a.height as i64 {
        for px in 0..a.width as i64 {
            let masked = px >= x0 && px < x1 && py >= y0 && py < y1;
            if masked {
                continue;
            }
            let i = (py as usize * a.width as usize + px as usize) * 4;
            assert_eq!(
                a.rgba[i..i + 4],
                b.rgba[i..i + 4],
                "pixel ({px},{py}) outside the edited paragraph changed"
            );
            compared = compared.saturating_add(1);
        }
    }
    assert!(compared > 1000, "the render comparison covered the page");
}

fn painted_in(page: &RenderedPage, rect: Rect) -> usize {
    let h = page.height as i64;
    let x0 = (rect.x0 as i64).max(0);
    let x1 = (rect.x1 as i64).min(page.width as i64);
    let y0 = (h - rect.y1 as i64).max(0);
    let y1 = (h - rect.y0 as i64).min(h);
    let mut painted = 0usize;
    for py in y0..y1 {
        for px in x0..x1 {
            let i = (py as usize * page.width as usize + px as usize) * 4;
            if page.rgba[i] < 200 {
                painted = painted.saturating_add(1);
            }
        }
    }
    painted
}

/// (a) + (b): editing one word changes only the first paragraph's content
/// region, the original bytes are a verbatim prefix, and the page renders
/// identically once that paragraph's rectangle is masked out.
#[test]
fn one_word_edit_is_region_local_and_render_parity_holds() {
    let src = TWO_PARAGRAPHS;
    let clock = FixedClock(0);
    let (out, report) =
        reflow_paragraph(src, 0, 0, "First", "Firs", &[], &budget(), &clock).expect("reflow");

    // I3: original bytes are a verbatim prefix of the incremental update.
    assert_eq!(&out[..src.len()], src);
    assert!(report.confidence >= selis_pdf_edit::MIN_REFLOW_CONFIDENCE);
    assert_eq!(
        report.lines_out, 2,
        "the two-line paragraph stays two lines"
    );

    // (a) The decoded stream is byte-identical outside the reported span.
    let before = decoded_content(src);
    let after = decoded_content(&out);
    let span = report.changed_span.clone();
    assert!(span.start <= span.end && span.end <= before.len());
    assert_eq!(&after[..span.start], &before[..span.start]);
    let replacement_len =
        ((span.end - span.start) as i64 + after.len() as i64 - before.len() as i64) as usize;
    let after_tail_start = span.start + replacement_len;
    assert_eq!(&after[after_tail_start..], &before[span.end..]);
    assert!(
        String::from_utf8_lossy(&after[span.start..after_tail_start]).contains("Firs paragraph"),
        "the replacement is in the edited region"
    );
    assert!(
        String::from_utf8_lossy(&after).contains("Second paragraph line one"),
        "the other paragraph is untouched"
    );

    // (b) Render parity outside the paragraph rectangle.
    let session = open(src);
    let mut g = budget().guard_with(&clock, CancelToken::new());
    let (paragraphs, _) = session
        .infer_paragraphs(0, &budget(), &mut g)
        .expect("paragraphs");
    let rect = paragraphs[0].bbox;
    assert_render_equal_outside(&render(src), &render(&out), rect);
}

/// (c): a character absent from the embedded subset renders after the subset
/// is extended with a new outline, and the edit leaves a verbatim prefix.
#[test]
fn new_glyph_absent_from_subset_renders_after_extension() {
    let body = b"0 0 0 rg\nBT /F1 12 Tf 72 700 Td (AB) Tj ET\nBT /F1 12 Tf 72 686 Td (AB) Tj ET\nBT /F1 12 Tf 72 600 Td (AB) Tj ET\nBT /F1 12 Tf 72 586 Td (AB) Tj ET";
    let src = pdf_with_embedded_font(body);
    let clock = FixedClock(0);

    // Before: the subset carries A and B only — no glyph for 'C' (0x43).
    let b = budget();
    let mut g = b.guard();
    let start = selis_pdf_cos::xref::find_startxref(&src, 4096).expect("startxref");
    let doc = selis_pdf_cos::parse_revisions(&src, start, &b, &mut g).expect("parse");
    let Obj::Stream {
        data: original_font,
        ..
    } = resolve_ref(&src, &doc, selis_pdf_cos::Ref::new(7, 0), &b, &mut g).expect("font program")
    else {
        panic!("font program is not a stream");
    };
    let before_width = {
        let mut fd = selis_font::model::FontDict::simple(selis_font::model::FontSubtype::TrueType);
        fd.font_file = Some(selis_font::model::FontFile::TrueType(
            selis_bytes::Bytes::copy_from_slice(original_font.as_slice()),
        ));
        selis_font::width::resolve(&fd, &mut g)
            .expect("resolve original widths")
            .width(0x43)
    };
    assert_eq!(
        before_width, 0.0,
        "code 0x43 is absent from the original subset"
    );

    let new_glyph = NewGlyph {
        ch: 'C',
        outline: vec![
            selis_font::outline::OutlineCmd::Move { x: 0.0, y: 0.0 },
            selis_font::outline::OutlineCmd::Line { x: 0.0, y: 600.0 },
            selis_font::outline::OutlineCmd::Line { x: 400.0, y: 600.0 },
            selis_font::outline::OutlineCmd::Line { x: 400.0, y: 0.0 },
            selis_font::outline::OutlineCmd::Close,
        ],
        advance: 400,
    };
    let (out, report) = reflow_paragraph(
        &src,
        0,
        0,
        "AB",
        "ABC",
        &[new_glyph.clone()],
        &budget(),
        &clock,
    )
    .expect("reflow with new glyph");
    assert!(report.font_extended, "the embedded subset was extended");
    assert_eq!(&out[..src.len()], src);

    // The re-embedded font now maps code 0x43 to a real 400-unit advance.
    let mut g2 = b.guard();
    let out_start = selis_pdf_cos::xref::find_startxref(&out, 4096).expect("startxref");
    let out_doc = selis_pdf_cos::parse_revisions(&out, out_start, &b, &mut g2).expect("parse out");
    let Obj::Stream { data: new_font, .. } =
        resolve_ref(&out, &out_doc, selis_pdf_cos::Ref::new(7, 0), &b, &mut g2).expect("new font")
    else {
        panic!("new font is not a stream");
    };
    let after_width = {
        let mut fd = selis_font::model::FontDict::simple(selis_font::model::FontSubtype::TrueType);
        fd.font_file = Some(selis_font::model::FontFile::TrueType(
            selis_bytes::Bytes::copy_from_slice(new_font.as_slice()),
        ));
        selis_font::width::resolve(&fd, &mut g2)
            .expect("resolve new widths")
            .width(0x43)
    };
    assert_eq!(
        after_width, 400.0,
        "the new glyph now has an advance in the font"
    );

    // Render: the edited paragraph paints MORE glyph pixels than before — the
    // new 'C' is actually drawn, not a .notdef gap or a dropped character.
    // Use a generous rectangle around paragraph 0 (y >= 650): the model bbox
    // covers only the first glyph's advance, not the newly added third glyph.
    let region = Rect::new(0.0, 650.0, 612.0, 792.0);
    let before_px = painted_in(&render(&src), region);
    let after_px = painted_in(&render(&out), region);
    assert!(
        after_px > before_px,
        "the new glyph renders: painted pixels {before_px} -> {after_px}"
    );
    let _ = new_glyph;
}

/// (d): a low-confidence paragraph is refused by model-based reflow and the
/// error points at the box-level fallback (SL-5.TEXTEDIT.03).
#[test]
fn low_confidence_paragraph_refuses_and_suggests_box_path() {
    // text.pdf is a single-line page: with no within-/between-paragraph gaps
    // the model has no spacing evidence, so its confidence is far below 0.80.
    let src = include_bytes!("../src/fixtures/text.pdf");
    let session = open(src);
    let mut g = budget().guard_with(&FixedClock(0), CancelToken::new());
    let (_, confidence) = session
        .infer_paragraphs(0, &budget(), &mut g)
        .expect("paragraphs");
    assert!(
        confidence.value < selis_pdf_edit::MIN_REFLOW_CONFIDENCE,
        "fixture confidence {} should be low",
        confidence.value
    );
    let err = reflow_paragraph(src, 0, 0, "AB", "AC", &[], &budget(), &FixedClock(0))
        .expect_err("low-confidence reflow must be refused");
    match err {
        ReflowError::LowConfidence {
            confidence,
            suggestion,
            ..
        } => {
            assert!(confidence < selis_pdf_edit::MIN_REFLOW_CONFIDENCE);
            assert!(
                suggestion.contains("edit_box"),
                "the fallback must be named: {suggestion}"
            );
        }
        other => panic!("expected LowConfidence, got {other:?}"),
    }
}
