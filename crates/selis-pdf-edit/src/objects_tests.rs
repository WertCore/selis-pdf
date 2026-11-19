//! Tests for object-level image/vector editing (SL-5.TEXTEDIT.05/06).

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
use selis_pdf_cos::{Obj, Ref};
use selis_sandbox::Budget;

use super::objects::{
    delete_object, extract_image, list_image_objects, list_vector_objects, move_object,
    replace_image, resize_object, ObjectError, ObjectId,
};

/// The control image's draw, which no mutation in these tests touches.
const CONTROL_DRAW: &str = "q 20 0 0 20 200 200 cm /Im2 Do Q";

fn budget() -> Budget {
    Budget::unlimited()
}

/// A one-page PDF carrying two Flate/DeviceRGB image XObjects, one DCT image
/// and two vector paths. `first_filter` lets a test make the first image carry
/// an unsupported filter without disturbing the operator layout.
fn fixture(first_filter: &str) -> Vec<u8> {
    let content = b"q 100 0 0 50 10 10 cm /Im1 Do Q\nq 20 0 0 20 200 200 cm /Im2 Do Q\nq 2 0 0 2 0 0 cm 5 5 30 20 re f Q\nq 1 0 0 1 0 0 cm 100 150 40 10 re S Q\nq 8 0 0 8 250 250 cm /Im3 Do Q\n";

    let rgb_a: Vec<u8> = vec![
        255, 0, 0, // red
        0, 255, 0, // green
        0, 0, 255, // blue
        255, 255, 255, // white
    ];
    let rgb_b: Vec<u8> = vec![0; 12];
    let flate_a = selis_pdf_filter::flate_encode(&rgb_a, 6);
    let flate_b = selis_pdf_filter::flate_encode(&rgb_b, 6);

    let jpeg = selis_image::encode_jpeg(
        &selis_image::Image {
            width: 2,
            height: 2,
            rgba: vec![
                10, 20, 30, 255, 10, 20, 30, 255, 10, 20, 30, 255, 10, 20, 30, 255,
            ],
        },
        80,
        &mut budget().guard(),
    )
    .expect("jpeg fixture");

    // Stream data is binary: append the raw bytes, never a lossy UTF-8 string.
    let stream_obj = |dict: String, data: &[u8]| {
        let mut obj = dict.into_bytes();
        obj.extend_from_slice(b"\nstream\n");
        obj.extend_from_slice(data);
        obj.extend_from_slice(b"\nendstream");
        obj
    };
    let objects: Vec<Vec<u8>> = vec![
        b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(),
        b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec(),
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 300 300] /Contents 4 0 R /Resources << /XObject << /Im1 5 0 R /Im2 6 0 R /Im3 7 0 R >> >> >>".to_vec(),
        stream_obj(format!("<< /Length {} >>", content.len()), content),
        stream_obj(
            format!(
                "<< /Type /XObject /Subtype /Image /Width 2 /Height 2 /ColorSpace /DeviceRGB /BitsPerComponent 8 /Filter /{first_filter} /Length {} >>",
                flate_a.len()
            ),
            &flate_a,
        ),
        stream_obj(
            format!(
                "<< /Type /XObject /Subtype /Image /Width 2 /Height 2 /ColorSpace /DeviceRGB /BitsPerComponent 8 /Filter /FlateDecode /Length {} >>",
                flate_b.len()
            ),
            &flate_b,
        ),
        stream_obj(
            format!(
                "<< /Type /XObject /Subtype /Image /Width 2 /Height 2 /ColorSpace /DeviceRGB /BitsPerComponent 8 /Filter /DCTDecode /Length {} >>",
                jpeg.len()
            ),
            &jpeg,
        ),
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

fn src() -> Vec<u8> {
    fixture("FlateDecode")
}

/// The decoded bytes of content-stream object 4 (the fixture stores it raw).
fn content_of(pdf: &[u8]) -> Vec<u8> {
    let b = budget();
    let mut g = b.guard();
    let start = selis_pdf_cos::xref::find_startxref(pdf, 4096).expect("startxref");
    let doc = selis_pdf_cos::parse_revisions(pdf, start, &b, &mut g).expect("parse");
    match selis_pdf_cos::copy::resolve_ref(pdf, &doc, Ref::new(4, 0), &b, &mut g).expect("resolve")
    {
        Obj::Stream { data, .. } => data.as_slice().to_vec(),
        other => panic!("not a stream: {other:?}"),
    }
}

fn approx(a: f64, b: f64) {
    assert!((a - b).abs() < 1e-6, "expected {b}, got {a}");
}

fn rect_is(actual: Rect, x0: f64, y0: f64, x1: f64, y1: f64) {
    approx(actual.x0, x0);
    approx(actual.y0, y0);
    approx(actual.x1, x1);
    approx(actual.y1, y1);
}

fn png_bytes(rgba: Vec<u8>) -> Vec<u8> {
    selis_image::encode_png(
        &selis_image::Image {
            width: 2,
            height: 2,
            rgba,
        },
        6,
        &mut budget().guard(),
    )
    .expect("png")
}

#[test]
fn list_images_and_vectors_uses_stable_ids_and_user_space_rects() {
    let pdf = src();
    let b = budget();
    let mut g = b.guard();
    let images = list_image_objects(&pdf, 0, &b, &mut g).expect("list images");
    assert_eq!(images.len(), 3);
    assert_eq!(images[0].id, ObjectId::new("Im1", 3));
    assert_eq!(images[0].name, "Im1");
    assert_eq!(images[0].object_number, Some(5));
    assert_eq!((images[0].pixel_width, images[0].pixel_height), (2, 2));
    rect_is(images[0].rect, 10.0, 10.0, 110.0, 60.0);
    assert_eq!(images[1].id, ObjectId::new("Im2", 7));
    rect_is(images[1].rect, 200.0, 200.0, 220.0, 220.0);
    assert_eq!(images[2].id, ObjectId::new("Im3", 21));
    assert_eq!(images[2].filter.as_deref(), Some("DCTDecode"));

    let vectors = list_vector_objects(&pdf, 0, &b, &mut g).expect("list vectors");
    assert_eq!(vectors.len(), 2);
    assert_eq!(vectors[0].id, ObjectId::new("", 12));
    assert_eq!(vectors[0].paint_op, "f");
    rect_is(vectors[0].rect, 10.0, 10.0, 70.0, 50.0);
    assert_eq!(vectors[1].id, ObjectId::new("", 17));
    assert_eq!(vectors[1].paint_op, "S");
    rect_is(vectors[1].rect, 100.0, 150.0, 140.0, 160.0);
}

#[test]
fn replace_image_changes_only_the_xobject_and_preserves_control() {
    let pdf = src();
    let b = budget();
    let mut g = b.guard();
    let before = list_image_objects(&pdf, 0, &b, &mut g).expect("before");
    let id = before[0].id.clone();
    let control = before[1].clone();

    let replacement = png_bytes(vec![
        1, 2, 3, 255, 4, 5, 6, 255, 7, 8, 9, 255, 10, 11, 12, 255,
    ]);
    let (out, report) = replace_image(&pdf, 0, &id, &replacement, &b, &mut g).expect("replace");
    assert!(report.prefix_preserved);
    assert_eq!(out.get(..pdf.len()), Some(pdf.as_slice()));
    assert_eq!(report.objects_changed, 1);

    // Content stream byte-identical: replace never touches it.
    assert_eq!(content_of(&out), content_of(&pdf));

    let mut g2 = b.guard();
    let after = list_image_objects(&out, 0, &b, &mut g2).expect("after");
    assert_eq!(after[0].id, id, "op index stable after read-back");
    rect_is(after[0].rect, 10.0, 10.0, 110.0, 60.0);
    assert_eq!(after[1], control, "I7: control image byte-identical");

    // Read-back #2: the extracted replacement decodes to the new pixels.
    let mut g3 = b.guard();
    let extracted = extract_image(&out, 0, &id, &b, &mut g3).expect("extract replacement");
    let decoded = selis_image::decode(&extracted, &mut g3).expect("decode replacement");
    assert_eq!((decoded.width, decoded.height), (2, 2));
    assert_eq!(
        decoded.rgba,
        vec![1, 2, 3, 255, 4, 5, 6, 255, 7, 8, 9, 255, 10, 11, 12, 255]
    );
}

#[test]
fn move_image_shifts_placement_and_preserves_control_bytes() {
    let pdf = src();
    let b = budget();
    let mut g = b.guard();
    let before = list_image_objects(&pdf, 0, &b, &mut g).expect("before");
    let id = before[0].id.clone();
    let control = before[1].clone();
    assert!(content_of(&pdf)
        .windows(CONTROL_DRAW.len())
        .any(|w| w == CONTROL_DRAW.as_bytes()));

    let (out, report) = move_object(&pdf, 0, &id, 5.0, 7.0, &b, &mut g).expect("move");
    assert!(report.prefix_preserved);

    let content = content_of(&out);
    assert!(
        content
            .windows(CONTROL_DRAW.len())
            .any(|w| w == CONTROL_DRAW.as_bytes()),
        "control draw bytes unchanged in the rewritten stream"
    );
    let text = String::from_utf8_lossy(&content);
    assert!(
        text.contains("100 0 0 50 15 17 cm"),
        "only the translation operands changed"
    );

    let mut g2 = b.guard();
    let after = list_image_objects(&out, 0, &b, &mut g2).expect("after");
    rect_is(after[0].rect, 15.0, 17.0, 115.0, 67.0);
    assert_eq!(after[1], control, "I7: control unchanged after move");
}

#[test]
fn resize_image_rewrites_only_placement_operands() {
    let pdf = src();
    let b = budget();
    let mut g = b.guard();
    let id = list_image_objects(&pdf, 0, &b, &mut g).expect("before")[0]
        .id
        .clone();
    let (out, report) = resize_object(&pdf, 0, &id, Rect::new(20.0, 30.0, 120.0, 90.0), &b, &mut g)
        .expect("resize");
    assert!(report.prefix_preserved);
    let resized_content = content_of(&out);
    let text = String::from_utf8_lossy(&resized_content);
    assert!(text.contains("100 0 0 60 20 30 cm"), "new size/translation");
    let mut g2 = b.guard();
    let after = list_image_objects(&out, 0, &b, &mut g2).expect("after");
    rect_is(after[0].rect, 20.0, 30.0, 120.0, 90.0);
    rect_is(after[1].rect, 200.0, 200.0, 220.0, 220.0);
}

#[test]
fn delete_image_removes_only_its_draw() {
    let pdf = src();
    let b = budget();
    let mut g = b.guard();
    let before = list_image_objects(&pdf, 0, &b, &mut g).expect("before");
    let id = before[0].id.clone();
    let control = before[1].clone();
    let (out, report) = delete_object(&pdf, 0, &id, &b, &mut g).expect("delete");
    assert!(report.prefix_preserved);
    let content = content_of(&out);
    let text = String::from_utf8_lossy(&content);
    assert!(!text.contains("/Im1 Do"), "target draw gone");
    assert!(
        content
            .windows(CONTROL_DRAW.len())
            .any(|w| w == CONTROL_DRAW.as_bytes()),
        "control draw bytes still present"
    );
    let mut g2 = b.guard();
    let after = list_image_objects(&out, 0, &b, &mut g2).expect("after");
    assert_eq!(after.len(), 2, "only the deleted image disappeared");
    // Deletion removes an operator, so the control's op index shifts by one —
    // that is the documented consequence of a byte-exact operator removal.
    // Its identity, resource, object number and placement are unchanged.
    assert_eq!(after[0].name, control.name, "I7: control resource kept");
    assert_eq!(
        after[0].object_number, control.object_number,
        "I7: control XObject kept"
    );
    assert_eq!(after[0].rect, control.rect, "I7: control placement kept");
}

#[test]
fn extract_image_returns_png_for_flate_and_jpeg_bytes_for_dct() {
    let pdf = src();
    let b = budget();
    let mut g = b.guard();
    let images = list_image_objects(&pdf, 0, &b, &mut g).expect("list");

    let png = extract_image(&pdf, 0, &images[0].id, &b, &mut g).expect("png");
    let decoded = selis_image::decode(&png, &mut g).expect("decode png");
    assert_eq!((decoded.width, decoded.height), (2, 2));
    assert_eq!(
        decoded.rgba,
        vec![255, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255, 255, 255, 255, 255],
        "extracted pixels match the fixture's flate RGB samples"
    );

    let jpeg = extract_image(&pdf, 0, &images[2].id, &b, &mut g).expect("jpeg");
    assert_eq!(jpeg.get(..2), Some([0xFF, 0xD8].as_slice()), "SOI");
    let decoded = selis_image::decode(&jpeg, &mut g).expect("decode jpeg");
    assert_eq!((decoded.width, decoded.height), (2, 2));
}

#[test]
fn vector_move_resize_and_delete_round_trip_with_control() {
    let pdf = src();
    let b = budget();
    let mut g = b.guard();
    let before = list_vector_objects(&pdf, 0, &b, &mut g).expect("before vectors");
    let target = before[0].id.clone();
    let control = before[1].clone();
    let control_draw = b"q 1 0 0 1 0 0 cm 100 150 40 10 re S Q";

    let (moved, _) = move_object(&pdf, 0, &target, 3.0, 4.0, &b, &mut g).expect("move vector");
    let mut g2 = b.guard();
    let after_move = list_vector_objects(&moved, 0, &b, &mut g2).expect("after move");
    rect_is(after_move[0].rect, 13.0, 14.0, 73.0, 54.0);
    assert_eq!(after_move[1], control, "I7: control vector unchanged");
    assert!(content_of(&moved)
        .windows(control_draw.len())
        .any(|w| w == control_draw));

    let (resized, _) = resize_object(&pdf, 0, &target, Rect::new(0.0, 0.0, 10.0, 5.0), &b, &mut g)
        .expect("resize vector");
    let mut g3 = b.guard();
    let after_resize = list_vector_objects(&resized, 0, &b, &mut g3).expect("after resize");
    rect_is(after_resize[0].rect, 0.0, 0.0, 10.0, 5.0);
    assert_eq!(after_resize[1], control);

    let (deleted, _) = delete_object(&pdf, 0, &target, &b, &mut g).expect("delete vector");
    let deleted_content = content_of(&deleted);
    let text = String::from_utf8_lossy(&deleted_content);
    assert!(!text.contains("5 5 30 20 re"), "path construction removed");
    let mut g4 = b.guard();
    let after_delete = list_vector_objects(&deleted, 0, &b, &mut g4).expect("after delete");
    assert_eq!(after_delete.len(), 1, "only the target path disappeared");
    assert_eq!(after_delete[0].paint_op, control.paint_op);
    assert_eq!(
        after_delete[0].rect, control.rect,
        "I7: control placement kept"
    );
    let mut g5 = b.guard();
    let images = list_image_objects(&deleted, 0, &b, &mut g5).expect("images survive");
    assert_eq!(images[0].id, ObjectId::new("Im1", 3));
}

#[test]
fn unknown_id_and_bad_input_fail_loudly() {
    let pdf = src();
    let b = budget();
    let mut g = b.guard();
    let bogus = ObjectId::new("Im9", 999);
    assert!(matches!(
        move_object(&pdf, 0, &bogus, 1.0, 1.0, &b, &mut g),
        Err(ObjectError::NotFound(_))
    ));
    assert!(matches!(
        resize_object(&pdf, 0, &bogus, Rect::new(0.0, 0.0, 1.0, 1.0), &b, &mut g),
        Err(ObjectError::NotFound(_))
    ));
    assert!(matches!(
        delete_object(&pdf, 0, &bogus, &b, &mut g),
        Err(ObjectError::NotFound(_))
    ));
    assert!(matches!(
        replace_image(&pdf, 0, &bogus, b"x", &b, &mut g),
        Err(ObjectError::NotFound(_))
    ));
    assert!(matches!(
        extract_image(&pdf, 0, &bogus, &b, &mut g),
        Err(ObjectError::NotFound(_))
    ));
    let real = list_image_objects(&pdf, 0, &b, &mut g).expect("list")[0]
        .id
        .clone();
    assert!(matches!(
        resize_object(&pdf, 0, &real, Rect::new(0.0, 0.0, 0.0, 5.0), &b, &mut g),
        Err(ObjectError::Invalid(_))
    ));
    assert!(matches!(
        move_object(&pdf, 0, &real, f64::NAN, 0.0, &b, &mut g),
        Err(ObjectError::Invalid(_))
    ));
    assert!(matches!(
        list_image_objects(b"not a pdf", 0, &b, &mut g),
        Err(ObjectError::Malformed(_))
    ));
}

#[test]
fn unsupported_filter_is_refused_for_replace_and_extract() {
    let pdf = fixture("JPXDecode");
    let b = budget();
    let mut g = b.guard();
    let id = list_image_objects(&pdf, 0, &b, &mut g).expect("list")[0]
        .id
        .clone();
    let replacement = png_bytes(vec![
        1, 2, 3, 255, 4, 5, 6, 255, 7, 8, 9, 255, 10, 11, 12, 255,
    ]);
    assert!(matches!(
        replace_image(&pdf, 0, &id, &replacement, &b, &mut g),
        Err(ObjectError::Unsupported(_))
    ));
    assert!(matches!(
        extract_image(&pdf, 0, &id, &b, &mut g),
        Err(ObjectError::Unsupported(_))
    ));
}
