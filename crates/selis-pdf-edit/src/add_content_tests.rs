//! Tests for SL-5.TEXTEDIT.07/08: adding content with incremental writes,
//! read-back verification and render parity outside the added rectangles.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::integer_division,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::float_cmp
)]

use selis_geom::Rect;
use selis_pdf_cos::Obj;
use selis_sandbox::Budget;

use super::add_content::{
    add_background, add_bates_numbering, add_header_footer, add_image, add_link, add_shape,
    add_text_box, add_watermark, AddContentError, AddImage, Align, Background, BatesOptions,
    HeaderFooter, Link, LinkTarget, Rgb, Shape, ShapeKind, TextBox, Watermark,
};
use super::incremental::{materialise, ObjectModel};

const MARKER: &[u8] = b"% SelisAddContent\n";

fn budget() -> Budget {
    Budget::unlimited()
}

/// A two-page synthetic PDF. Page 0 carries three filled control rectangles
/// (red bottom-left, blue top-right, green centre); page 0's `/Resources` has a
/// `/F1` font so added resource names must avoid it. Page 1 is a simpler green
/// page for the Bates test.
fn fixture() -> Vec<u8> {
    let content0 =
        b"1 0 0 rg 10 10 50 50 re f\n0 0 1 rg 200 200 50 50 re f\n0 1 0 rg 120 120 40 40 re f\n";
    let content1 = b"0 1 0 rg 10 10 50 50 re f\n";

    let stream_obj = |dict: String, data: &[u8]| {
        let mut obj = dict.into_bytes();
        obj.extend_from_slice(b"\nstream\n");
        obj.extend_from_slice(data);
        obj.extend_from_slice(b"\nendstream");
        obj
    };
    let objects: Vec<Vec<u8>> = vec![
        b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(),
        b"<< /Type /Pages /Kids [3 0 R 5 0 R] /Count 2 >>".to_vec(),
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 300 300] /Contents 4 0 R /Resources << /Font << /F1 7 0 R >> >> >>".to_vec(),
        stream_obj(format!("<< /Length {} >>", content0.len()), content0),
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 300 300] /Contents 6 0 R /Resources << /Font << /F1 7 0 R >> >> >>".to_vec(),
        stream_obj(format!("<< /Length {} >>", content1.len()), content1),
        b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_vec(),
    ];

    let mut out = b"%PDF-1.4\n".to_vec();
    let mut offsets = Vec::new();
    for (i, obj) in objects.iter().enumerate() {
        offsets.push(out.len());
        out.extend_from_slice(format!("{} 0 obj\n", i + 1).as_bytes());
        out.extend_from_slice(obj);
        out.extend_from_slice(b"\nendobj\n");
    }
    let xref = out.len();
    let size = objects.len() + 1;
    out.extend_from_slice(format!("xref\n0 {size}\n").as_bytes());
    out.extend_from_slice(b"0000000000 65535 f \n");
    for offset in &offsets {
        out.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }
    out.extend_from_slice(
        format!(
            "trailer\n<< /Size {size} /Root 1 0 R /ID [<0102030405060708090a0b0c0d0e0f10> <0102030405060708090a0b0c0d0e0f10>] >>\nstartxref\n{xref}\n%%EOF\n"
        )
        .as_bytes(),
    );
    out
}

fn model_of(pdf: &[u8]) -> ObjectModel {
    let b = budget();
    let mut g = b.guard();
    materialise(pdf, &b, &mut g).expect("materialise")
}

/// Every added stream is prefixed with the inert marker comment.
fn added_streams(model: &ObjectModel) -> Vec<(u32, Vec<u8>)> {
    model
        .iter()
        .filter_map(|(num, obj)| match obj {
            Obj::Stream { data, .. } if data.as_slice().starts_with(MARKER) => {
                Some((*num, data.as_slice().to_vec()))
            }
            _ => None,
        })
        .collect()
}

fn name_is(obj: &Obj, name: &str) -> bool {
    matches!(obj, Obj::Name(n) if n.as_slice() == name.as_bytes())
}

fn dict_get<'a>(obj: &'a Obj, key: &str) -> Option<&'a Obj> {
    let pairs = match obj {
        Obj::Dict(d) | Obj::Stream { dict: d, .. } => d,
        _ => return None,
    };
    pairs
        .iter()
        .find(|(k, _)| k.as_slice() == key.as_bytes())
        .map(|(_, v)| v)
}

fn dict_name_is(obj: &Obj, key: &str, name: &str) -> bool {
    dict_get(obj, key).is_some_and(|v| name_is(v, name))
}

fn original_content_unchanged(src: &[u8], out: &[u8]) {
    // The fixture's page-0 content object is object 4; its three control draws
    // must survive byte-for-byte somewhere in the output (they are in the
    // verbatim prefix).
    for draw in [
        b"1 0 0 rg 10 10 50 50 re f".as_slice(),
        b"0 0 1 rg 200 200 50 50 re f".as_slice(),
        b"0 1 0 rg 120 120 40 40 re f".as_slice(),
    ] {
        assert!(
            out.windows(draw.len()).any(|w| w == draw),
            "control draw lost"
        );
    }
    assert_eq!(out.get(..src.len()), Some(src), "prefix not preserved");
}

#[test]
fn add_text_box_preserves_prefix_and_discloses_font() {
    let src = fixture();
    let b = budget();
    let mut g = b.guard();
    let text = TextBox {
        text: "Invoice (final)".to_string(),
        font: "Helvetica".to_string(),
        font_size: 12.0,
        color: Rgb::new(0, 0, 0),
        rect: Rect::new(20.0, 80.0, 120.0, 92.0),
    };
    let (out, report) = add_text_box(&src, 0, &text, &b, &mut g).expect("add text");
    assert!(report.prefix_preserved);
    assert_eq!(report.pages, vec![0]);
    assert!(report.objects_added >= 2); // font + content stream
    original_content_unchanged(&src, &out);

    // Read-back: the added stream carries the escaped literal and Tf/Tj ops.
    let model = model_of(&out);
    let streams = added_streams(&model);
    assert_eq!(streams.len(), 1, "one added content stream");
    let ops = String::from_utf8_lossy(&streams[0].1);
    assert!(
        ops.contains("/SelisF1 12 Tf"),
        "font resource written: {ops}"
    );
    assert!(
        ops.contains("(Invoice \\(final\\)) Tj"),
        "text escaped: {ops}"
    );
    assert!(ops.contains("BT") && ops.contains("ET"));

    // The font object exists and is a standard-14 Type1 face.
    let fonts: Vec<&Obj> = model
        .values()
        .filter(|o| matches!(o, Obj::Dict(d) if d.iter().any(|(k, v)| k.as_slice() == b"Type" && matches!(v, Obj::Name(n) if n.as_slice() == b"Font"))))
        .collect();
    assert!(fonts
        .iter()
        .any(|o| dict_name_is(o, "BaseFont", "Helvetica")));

    // Disclosure is non-empty: an unembedded standard-14 face was substituted
    // in the plan, and that substitution was surfaced (never silent).
    assert!(!report.font_disclosures.is_empty());
    assert!(report.font_disclosures[0].contains("substituted"));
}

#[test]
fn add_image_from_caller_bytes_reads_back_as_flate_rgb() {
    let src = fixture();
    let b = budget();
    let mut g = b.guard();
    let png = selis_image::encode_png(
        &selis_image::Image {
            width: 2,
            height: 2,
            rgba: vec![
                255, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255, 255, 255, 255, 255,
            ],
        },
        6,
        &mut g,
    )
    .expect("png");
    let add = AddImage {
        rect: Rect::new(250.0, 250.0, 290.0, 290.0),
        angle_degrees: 0.0,
    };
    let (out, report) = add_image(&src, 0, &add, &png, &b, &mut g).expect("add image");
    assert!(report.prefix_preserved);
    original_content_unchanged(&src, &out);

    let model = model_of(&out);
    let images: Vec<&Obj> = model
        .values()
        .filter(|o| matches!(o, Obj::Stream { dict, .. } if dict.iter().any(|(k, v)| k.as_slice() == b"Subtype" && matches!(v, Obj::Name(n) if n.as_slice() == b"Image"))))
        .collect();
    assert_eq!(images.len(), 1, "one new image XObject");
    assert!(dict_name_is(images[0], "Filter", "FlateDecode"));
    assert!(dict_name_is(images[0], "ColorSpace", "DeviceRGB"));
    assert!(dict_get(images[0], "Width").is_some());
    assert!(dict_get(images[0], "Height").is_some());

    let streams = added_streams(&model);
    let ops = String::from_utf8_lossy(&streams[0].1);
    assert!(
        ops.contains("40 0 0 40 250 250 cm"),
        "placement matrix: {ops}"
    );
    assert!(ops.contains("/SelisIm1 Do"), "image drawn: {ops}");
}

#[test]
fn add_shape_and_link_write_readable_objects() {
    let src = fixture();
    let b = budget();
    let mut g = b.guard();
    let shape = Shape {
        kind: ShapeKind::Ellipse,
        rect: Rect::new(100.0, 100.0, 180.0, 180.0),
        fill: Some(Rgb::new(255, 0, 255)),
        stroke: None,
        stroke_width: 0.0,
    };
    let (out1, _) = add_shape(&src, 0, &shape, &b, &mut g).expect("add shape");
    original_content_unchanged(&src, &out1);
    let model1 = model_of(&out1);
    let streams1 = added_streams(&model1);
    let ops = String::from_utf8_lossy(&streams1[0].1);
    assert!(ops.contains(" c"), "ellipse uses cubic beziers: {ops}");
    assert!(
        !ops.contains(" re\n"),
        "no rectangle operator for an ellipse"
    );

    let link = Link {
        rect: Rect::new(10.0, 10.0, 60.0, 60.0),
        target: LinkTarget::Uri("https://example.test/doc".to_string()),
        color: Rgb::new(0, 0, 255),
        border: true,
    };
    let (out, report) = add_link(&out1, 0, &link, &b, &mut g).expect("add link");
    assert!(report.prefix_preserved);
    assert_eq!(out.get(..out1.len()), Some(out1.as_slice()));
    let model = model_of(&out);

    let annots: Vec<&Obj> = model
        .values()
        .filter(|o| matches!(o, Obj::Dict(d) if d.iter().any(|(k, v)| k.as_slice() == b"Subtype" && matches!(v, Obj::Name(n) if n.as_slice() == b"Link"))))
        .collect();
    assert_eq!(annots.len(), 1, "one link annotation");
    assert!(dict_name_is(annots[0], "Subtype", "Link"));
    assert!(dict_get(annots[0], "AP").is_some(), "appearance attached");
    let action = dict_get(annots[0], "A").expect("/A action");
    assert!(dict_name_is(action, "S", "URI"));
    let uri = match dict_get(action, "URI") {
        Some(Obj::String(s)) => String::from_utf8_lossy(s.as_slice()).into_owned(),
        other => panic!("URI is not a string: {other:?}"),
    };
    assert_eq!(uri, "https://example.test/doc");

    // The page now carries /Annots pointing at the link.
    let pages: Vec<&Obj> = model
        .values()
        .filter(|o| dict_name_is(o, "Type", "Page"))
        .collect();
    assert!(dict_get(pages[0], "Annots").is_some());
}

#[test]
fn rotated_image_draws_a_transformed_unit_square_with_parity_outside() {
    let src = fixture();
    let before = render(&src);
    let b = budget();
    let mut g = b.guard();
    let png = selis_image::encode_png(
        &selis_image::Image {
            width: 2,
            height: 2,
            rgba: [255, 0, 0, 255].repeat(4),
        },
        6,
        &mut g,
    )
    .expect("png");
    let add = AddImage {
        rect: Rect::new(250.0, 100.0, 290.0, 140.0),
        angle_degrees: 45.0,
    };
    let (out, _) = add_image(&src, 0, &add, &png, &b, &mut g).expect("rotated image");
    let after = render(&out);
    // A 45°-rotated 40×40 square reaches ~28.3 from its centre (270,120).
    assert_parity_outside(&before, &after, Rect::new(240.0, 90.0, 300.0, 150.0));
    // NOTE: the local engine renderer derives an image's placement rect from
    // only the two diagonal corners of the CTM's unit square (exec.rs), so a
    // 45° rotation yields a degenerate AABB and paints nothing here. The PDF
    // we WRITE is correct (below); a viewer that honours the full CTM renders
    // it rotated. This renderer gap is recorded in the plan row.

    // The placement matrix carries a rotation (b and c are non-zero).
    let model = model_of(&out);
    let streams = added_streams(&model);
    let ops = String::from_utf8_lossy(&streams[0].1);
    let cm = ops
        .split(" cm")
        .next()
        .unwrap_or("")
        .split("q ")
        .nth(1)
        .unwrap_or("")
        .to_string();
    let nums: Vec<f64> = cm.split(' ').filter_map(|s| s.parse().ok()).collect();
    assert_eq!(nums.len(), 6, "six matrix operands: {cm}");
    assert!(
        nums[1].abs() > 0.0 && nums[2].abs() > 0.0,
        "rotation present: {cm}"
    );
}

#[test]
fn text_box_render_parity_outside_the_claimed_rect() {
    let src = fixture();
    let before = render(&src);
    let b = budget();
    let mut g = b.guard();
    let claimed = Rect::new(20.0, 80.0, 220.0, 100.0);
    let text = TextBox {
        text: "Invoice final".to_string(),
        font: "Helvetica".to_string(),
        font_size: 12.0,
        color: Rgb::new(0, 0, 0),
        rect: claimed,
    };
    let (out, _) = add_text_box(&src, 0, &text, &b, &mut g).expect("text box");
    let after = render(&out);
    assert_parity_outside(&before, &after, claimed);
}

// ---- render parity -------------------------------------------------------

fn render(pdf: &[u8]) -> selis_pdf_engine::RenderedPage {
    let budget = Budget::unlimited();
    let clock = selis_sandbox::FixedClock(0);
    let session = selis_pdf_engine::Session::open(pdf.to_vec(), &budget, &clock).expect("open");
    let mut g = budget.guard_with(&clock, selis_sandbox::CancelToken::new());
    session
        .render_page_rgba(0, 72.0, &budget, &mut g)
        .expect("render")
}

/// Compare two renders, ignoring pixels whose PDF-space centre falls inside
/// `added` (the declared added rectangle). Everything else must be identical.
fn assert_parity_outside(
    before: &selis_pdf_engine::RenderedPage,
    after: &selis_pdf_engine::RenderedPage,
    added: Rect,
) {
    assert_eq!((before.width, before.height), (after.width, after.height));
    let w = usize::try_from(after.width).expect("width");
    let h = usize::try_from(after.height).expect("height");
    let inside = |px: usize, py: usize| {
        // PDF y is up, image row y is down.
        let x = px as f64 + 0.5;
        let y = (h - py) as f64 - 0.5;
        x >= added.x0 && x <= added.x1 && y >= added.y0 && y <= added.y1
    };
    for py in 0..h {
        for px in 0..w {
            if inside(px, py) {
                continue;
            }
            let i = (py * w + px) * 4;
            assert_eq!(
                before.rgba[i..i + 4],
                after.rgba[i..i + 4],
                "pixel ({px},{py}) outside the added rect changed"
            );
        }
    }
}

fn pixel(page: &selis_pdf_engine::RenderedPage, x: usize, y: usize) -> (u8, u8, u8) {
    // y is PDF-space (bottom-up); image rows are top-down.
    let w = usize::try_from(page.width).expect("width");
    let h = usize::try_from(page.height).expect("height");
    let row = h - 1 - y;
    let i = (row * w + x) * 4;
    (page.rgba[i], page.rgba[i + 1], page.rgba[i + 2])
}

#[test]
fn overlay_shape_render_parity_outside_added_rect() {
    let src = fixture();
    let before = render(&src);
    let b = budget();
    let mut g = b.guard();
    let added = Rect::new(250.0, 10.0, 290.0, 50.0);
    let shape = Shape {
        kind: ShapeKind::Rect,
        rect: added,
        fill: Some(Rgb::new(255, 0, 255)),
        stroke: None,
        stroke_width: 0.0,
    };
    let (out, _) = add_shape(&src, 0, &shape, &b, &mut g).expect("add shape");
    let after = render(&out);
    assert_parity_outside(&before, &after, added);
    assert_eq!(pixel(&after, 270, 30), (255, 0, 255), "added rect painted");
    assert_eq!(pixel(&after, 30, 30), (255, 0, 0), "control untouched");
}

#[test]
fn background_is_beneath_existing_content_with_pixel_parity_outside() {
    let src = fixture();
    let before = render(&src);
    let b = budget();
    let mut g = b.guard();
    let added = Rect::new(100.0, 100.0, 180.0, 180.0);
    let bg = Background {
        color: Rgb::new(255, 255, 0),
        rect: added,
    };
    let (out, report) = add_background(&src, 0, &bg, &b, &mut g).expect("add background");
    assert!(report.prefix_preserved);
    let after = render(&out);
    assert_parity_outside(&before, &after, added);
    // The green control rectangle at 120..160 sits ON TOP of the yellow
    // background — the background was placed beneath, not painted over it.
    assert_eq!(
        pixel(&after, 140, 140),
        (0, 255, 0),
        "control above background"
    );
    assert_eq!(
        pixel(&after, 105, 105),
        (255, 255, 0),
        "background visible at corner"
    );
}

// ---- TEXTEDIT.08 derived furniture --------------------------------------

#[test]
fn header_footer_and_watermark_round_trip() {
    let src = fixture();
    let b = budget();
    let mut g = b.guard();
    let hf = HeaderFooter {
        text: "Acme Confidential".to_string(),
        font: "Helvetica".to_string(),
        font_size: 10.0,
        color: Rgb::new(0, 0, 0),
        margin: 12.0,
        align: Align::Center,
    };
    let (out1, _) = add_header_footer(&src, 0, &hf, true, &b, &mut g).expect("header");
    let (out2, _) = add_header_footer(&out1, 0, &hf, false, &b, &mut g).expect("footer");
    let mark = Watermark {
        text: "DRAFT".to_string(),
        font: "Helvetica".to_string(),
        font_size: 40.0,
        color: Rgb::new(200, 200, 200),
        angle_degrees: 45.0,
        opacity: 0.25,
    };
    let (out, report) = add_watermark(&out2, 0, &mark, &b, &mut g).expect("watermark");
    assert!(report.prefix_preserved);
    assert!(
        !report.font_disclosures.is_empty(),
        "watermark face disclosed"
    );
    assert_eq!(out.get(..src.len()), Some(src.as_slice()));

    let model = model_of(&out);
    let all_ops: String = added_streams(&model)
        .iter()
        .map(|(_, d)| String::from_utf8_lossy(d).into_owned())
        .collect();
    assert!(
        all_ops.contains("Acme Confidential"),
        "header/footer text present"
    );
    assert!(all_ops.contains("DRAFT"), "watermark text present");
    assert!(all_ops.contains(" cm"), "watermark rotation matrix present");
    assert!(
        all_ops.contains("gs"),
        "watermark opacity ExtGState applied"
    );
    // Three streams: header, footer, watermark (each its own addition).
    assert_eq!(added_streams(&model).len(), 3);
}

#[test]
fn bates_numbers_all_pages_sequentially() {
    let src = fixture();
    let b = budget();
    let mut g = b.guard();
    let options = BatesOptions {
        pattern: "CASE-000001".to_string(),
        start: 7,
        font: "Helvetica".to_string(),
        font_size: 9.0,
        color: Rgb::new(0, 0, 0),
        margin: 10.0,
        align: Align::Left,
    };
    let (out, report) = add_bates_numbering(&src, &options, &b, &mut g).expect("bates");
    assert!(report.prefix_preserved);
    assert_eq!(report.pages, vec![0, 1]);
    assert_eq!(out.get(..src.len()), Some(src.as_slice()));

    let model = model_of(&out);
    let streams = added_streams(&model);
    assert_eq!(streams.len(), 2, "one stamp stream per page");
    let texts: Vec<String> = streams
        .iter()
        .map(|(_, d)| String::from_utf8_lossy(d).into_owned())
        .collect();
    assert!(
        texts.iter().any(|t| t.contains("CASE-000007")),
        "first id: {texts:?}"
    );
    assert!(
        texts.iter().any(|t| t.contains("CASE-000008")),
        "second id: {texts:?}"
    );

    original_content_unchanged(&src, &out);
}

// ---- typed refusals ------------------------------------------------------

#[test]
fn typed_refusals_never_write_bytes() {
    let src = fixture();
    let b = budget();
    let mut g = b.guard();

    // Non-finite and inverted geometry.
    let bad_rect = Rect::new(f64::NAN, 0.0, 10.0, 10.0);
    let shape = Shape {
        kind: ShapeKind::Rect,
        rect: bad_rect,
        fill: Some(Rgb::new(0, 0, 0)),
        stroke: None,
        stroke_width: 0.0,
    };
    assert!(matches!(
        add_shape(&src, 0, &shape, &b, &mut g),
        Err(AddContentError::Invalid(_))
    ));
    let bg = Background {
        color: Rgb::new(0, 0, 0),
        rect: Rect::new(10.0, 10.0, 0.0, 10.0),
    };
    assert!(matches!(
        add_background(&src, 0, &bg, &b, &mut g),
        Err(AddContentError::Invalid(_))
    ));

    // Empty and non-ASCII text.
    for bad in ["", "café"] {
        let text = TextBox {
            text: bad.to_string(),
            font: "Helvetica".to_string(),
            font_size: 12.0,
            color: Rgb::new(0, 0, 0),
            rect: Rect::new(10.0, 10.0, 60.0, 22.0),
        };
        assert!(matches!(
            add_text_box(&src, 0, &text, &b, &mut g),
            Err(AddContentError::Invalid(_))
        ));
    }

    // Bad page index and bad link target.
    let text = TextBox {
        text: "ok".to_string(),
        font: "Helvetica".to_string(),
        font_size: 12.0,
        color: Rgb::new(0, 0, 0),
        rect: Rect::new(10.0, 10.0, 60.0, 22.0),
    };
    assert!(matches!(
        add_text_box(&src, 9, &text, &b, &mut g),
        Err(AddContentError::Invalid(_))
    ));
    let link = Link {
        rect: Rect::new(10.0, 10.0, 60.0, 60.0),
        target: LinkTarget::Uri("".to_string()),
        color: Rgb::new(0, 0, 0),
        border: false,
    };
    assert!(matches!(
        add_link(&src, 0, &link, &b, &mut g),
        Err(AddContentError::Invalid(_))
    ));

    // Undecodable image bytes and a Bates pattern with no counter field.
    let add = AddImage {
        rect: Rect::new(10.0, 10.0, 60.0, 60.0),
        angle_degrees: 0.0,
    };
    assert!(matches!(
        add_image(&src, 0, &add, b"not an image", &b, &mut g),
        Err(AddContentError::Unsupported(_))
    ));
    let options = BatesOptions {
        pattern: "NO-DIGITS".to_string(),
        start: 1,
        font: "Helvetica".to_string(),
        font_size: 9.0,
        color: Rgb::new(0, 0, 0),
        margin: 10.0,
        align: Align::Left,
    };
    assert!(matches!(
        add_bates_numbering(&src, &options, &b, &mut g),
        Err(AddContentError::Unsupported(_))
    ));

    // A font that is not a standard-14 face is unavailable here and is refused
    // with a typed error rather than silently swapped for another face.
    let unavailable = TextBox {
        text: "hello".to_string(),
        font: "NotARealFont".to_string(),
        font_size: 12.0,
        color: Rgb::new(0, 0, 0),
        rect: Rect::new(10.0, 10.0, 60.0, 22.0),
    };
    assert!(matches!(
        add_text_box(&src, 0, &unavailable, &b, &mut g),
        Err(AddContentError::Unsupported(_))
    ));

    // Confirm the source was never modified by any refusal.
    assert_eq!(model_of(&src).len(), 7);
}
