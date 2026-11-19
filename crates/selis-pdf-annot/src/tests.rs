//! The annotation writer's own tests.
//!
//! The claim these tests make is narrow and checkable: a markup written by
//! this crate is **in the document** as a standard PDF annotation with an
//! appearance stream, and it reads back the way it was written.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects
)]

use super::*;

use selis_pdf_cos::copy::resolve_ref;
use selis_pdf_cos::Obj;
use selis_sandbox::Budget;

/// A one-page PDF with a single line of text, from raw object bodies.
fn build_pdf() -> Vec<u8> {
    let content = b"BT /F1 12 Tf 10 100 Td (Hello World) Tj ET\n";
    let mut stream = format!("<< /Length {} >>\nstream\n", content.len()).into_bytes();
    stream.extend_from_slice(content);
    stream.extend_from_slice(b"\nendstream");
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

/// Run the writer over the fixture.
fn run(src: &[u8], spec: &AnnotSpec) -> Result<(Vec<u8>, AnnotReport)> {
    let budget = Budget::unlimited();
    let mut g = budget.guard();
    annotate(src, spec, &budget, &mut g)
}

/// Run the reader over some bytes.
fn read(src: &[u8], page: u32) -> Result<Vec<PageAnnotation>> {
    let budget = Budget::unlimited();
    let mut g = budget.guard();
    list_annotations(src, page, &budget, &mut g)
}

/// A highlight over a line of text.
fn highlight() -> AnnotSpec {
    let mut spec = AnnotSpec::new(
        AnnotKind::Highlight,
        1,
        [10.0, 90.0, 120.0, 112.0],
        HIGHLIGHT_COLOUR,
    );
    spec.quads = vec![[10.0, 90.0, 120.0, 112.0]];
    spec
}

#[test]
fn an_export_carries_the_mark_as_xfdf() {
    let src = build_pdf();
    let (out, _) = run(&src, &highlight()).expect("the mark");
    let budget = Budget::unlimited();
    let mut g = budget.guard();
    let xml = xfdf::export_xfdf(&out, &budget, &mut g).expect("the export");
    assert!(
        xml.contains("<xfdf xmlns=\"http://ns.adobe.com/xfdf/\""),
        "the XML is XFDF: {xml}"
    );
    assert!(
        xml.contains("<highlight page=\"0\""),
        "the mark is there: {xml}"
    );
    assert!(xml.contains("name=\"selis-"), "the mark has a name: {xml}");
    assert!(
        xml.contains("color=\"#FFEB3B\""),
        "the colour is there: {xml}"
    );
}

#[test]
fn a_reply_points_back_at_the_mark_it_answers() {
    let src = build_pdf();
    let (out, _) = run(&src, &highlight()).expect("the mark");
    let mut reply = AnnotSpec::new(AnnotKind::Text, 1, [10.0, 90.0, 120.0, 112.0], NOTE_COLOUR);
    reply.contents = "I have fixed this.".to_string();
    reply.reply_to = Some(0);
    let (out, _) = run(&out, &reply).expect("the reply");
    let bytes = String::from_utf8_lossy(&out).to_string();
    assert!(
        bytes.contains("/IRT"),
        "the reply carries a back-reference: {bytes}"
    );
    let found = read(&out, 1).expect("the read");
    assert_eq!(found.len(), 2, "both marks are in the document");
    assert_eq!(found.get(1).and_then(|a| a.irt), Some(0));
    assert_eq!(
        found.get(1).map(|a| a.contents.as_str()),
        Some("I have fixed this.")
    );
}

#[test]
fn a_review_state_is_written_and_read_back() {
    let src = build_pdf();
    let (out, _) = run(&src, &highlight()).expect("the mark");
    let budget = Budget::unlimited();
    let mut g = budget.guard();
    let (out, report) =
        set_review_state(&out, 1, 0, ReviewState::Resolved, &budget, &mut g).expect("the resolve");
    assert_eq!(report.kind, "state");
    let found = read(&out, 1).expect("the read");
    assert_eq!(
        found.first().and_then(|a| a.state.as_deref()),
        Some("Accepted")
    );
    assert_eq!(
        found.first().and_then(|a| a.state_model.as_deref()),
        Some("Review")
    );

    let mut g = budget.guard();
    let (out, _) =
        set_review_state(&out, 1, 0, ReviewState::Reopened, &budget, &mut g).expect("the reopen");
    let found = read(&out, 1).expect("the read");
    assert_eq!(found.first().and_then(|a| a.state.as_deref()), Some("None"));
    let bytes = String::from_utf8_lossy(&out).to_string();
    assert!(
        bytes.contains("/StateModel /Review"),
        "the model is in the FILE: {bytes}"
    );
}

#[test]
fn a_reply_to_an_annotation_that_is_not_there_is_refused() {
    let src = build_pdf();
    let mut reply = AnnotSpec::new(AnnotKind::Text, 1, [10.0, 90.0, 120.0, 112.0], NOTE_COLOUR);
    reply.reply_to = Some(3);
    assert!(run(&src, &reply).is_err());
}

#[test]
fn a_highlight_is_written_into_the_document() {
    let src = build_pdf();
    let (out, report) = run(&src, &highlight()).expect("the highlight");
    assert_eq!(report.page, 1);
    assert_eq!(report.kind, "Highlight");
    assert_eq!(report.annots, 1);

    // It is an annotation, and it is the kind we asked for.
    let bytes = String::from_utf8_lossy(&out);
    assert!(
        bytes.contains("/Subtype /Highlight"),
        "the annotation is not a highlight: {bytes}"
    );
    assert!(
        bytes.contains("/Type /Annot"),
        "the annotation is not an annotation: {bytes}"
    );
    // It carries an appearance, so a viewer that does not implement the
    // annotation still draws the mark.
    assert!(
        bytes.contains("/Subtype /Form"),
        "the annotation has no appearance: {bytes}"
    );
}

#[test]
fn the_annotation_reads_back_the_way_it_was_written() {
    let src = build_pdf();
    let (out, _) = run(&src, &highlight()).expect("the highlight");
    let annotations = read(&out, 1).expect("the annotations");
    assert_eq!(annotations.len(), 1);
    let annotation = &annotations[0];
    assert_eq!(annotation.subtype, "Highlight");
    assert!((annotation.rect[2] - 120.0).abs() < 0.01);
    assert!((annotation.rect[3] - 112.0).abs() < 0.01);
    assert_eq!(annotation.quads.len(), 1);
    assert!((annotation.colour[0] - 1.0).abs() < 0.01);
    assert!((annotation.colour[1] - 0.92).abs() < 0.01);
}

#[test]
fn a_note_carries_its_comment_and_its_author() {
    let src = build_pdf();
    let mut spec = AnnotSpec::new(
        AnnotKind::Text,
        1,
        [200.0, 150.0, 220.0, 170.0],
        NOTE_COLOUR,
    );
    spec.contents = "Please check this".to_string();
    spec.author = "A reader".to_string();
    let (out, _) = run(&src, &spec).expect("the note");
    let annotations = read(&out, 1).expect("the annotations");
    assert_eq!(annotations.len(), 1);
    assert_eq!(annotations[0].subtype, "Text");
    assert_eq!(annotations[0].contents, "Please check this");
    assert_eq!(annotations[0].author, "A reader");
}

#[test]
fn a_comment_that_is_not_ascii_still_reads_back() {
    let src = build_pdf();
    let mut spec = AnnotSpec::new(
        AnnotKind::Text,
        1,
        [200.0, 150.0, 220.0, 170.0],
        NOTE_COLOUR,
    );
    spec.contents = "naïve — café".to_string();
    let (out, _) = run(&src, &spec).expect("the note");
    let annotations = read(&out, 1).expect("the annotations");
    assert_eq!(annotations[0].contents, "naïve — café");
}

#[test]
fn a_drawn_mark_carries_its_points() {
    let src = build_pdf();
    let mut spec = AnnotSpec::new(AnnotKind::Ink, 1, [10.0, 10.0, 60.0, 40.0], INK_COLOUR);
    spec.points = vec![(10.0, 10.0), (30.0, 35.0), (60.0, 12.0)];
    let (out, _) = run(&src, &spec).expect("the ink");
    let bytes = String::from_utf8_lossy(&out);
    assert!(
        bytes.contains("/Subtype /Ink"),
        "the mark is not an ink: {bytes}"
    );
    assert!(
        bytes.contains("/InkList"),
        "the mark has no points: {bytes}"
    );
}

#[test]
fn two_marks_both_stay_on_the_page() {
    let src = build_pdf();
    let (once, _) = run(&src, &highlight()).expect("the first");
    let mut second = highlight();
    second.rect = [20.0, 20.0, 60.0, 40.0];
    let (twice, report) = run(&once, &second).expect("the second");
    assert_eq!(report.annots, 2);
    let annotations = read(&twice, 1).expect("the annotations");
    assert_eq!(annotations.len(), 2);
}

#[test]
fn a_mark_can_be_taken_off_again() {
    let src = build_pdf();
    let (written, _) = run(&src, &highlight()).expect("the highlight");
    let budget = Budget::unlimited();
    let mut g = budget.guard();
    let (out, report) = remove_annotation(&written, 1, 0, &budget, &mut g).expect("the removal");
    assert_eq!(report.annots, 0);
    let annotations = read(&out, 1).expect("the annotations");
    assert!(annotations.is_empty());
}

#[test]
fn the_original_bytes_are_preserved_verbatim() {
    let src = build_pdf();
    let (out, _) = run(&src, &highlight()).expect("the highlight");
    assert_eq!(
        out.get(..src.len()),
        Some(src.as_slice()),
        "the original must stay a byte-identical prefix"
    );
}

#[test]
fn a_page_that_does_not_exist_is_refused() {
    let src = build_pdf();
    let mut spec = highlight();
    spec.page = 9;
    assert!(run(&src, &spec).is_err(), "page 9 does not exist");
}

#[test]
fn a_rectangle_the_wrong_way_round_is_refused() {
    let src = build_pdf();
    let mut spec = highlight();
    spec.rect = [120.0, 112.0, 10.0, 90.0];
    assert!(
        run(&src, &spec).is_err(),
        "the rectangle is the wrong way round"
    );
}

#[test]
fn a_colour_outside_the_range_is_refused() {
    let src = build_pdf();
    let mut spec = highlight();
    spec.colour = [2.0, 0.0, 0.0];
    assert!(run(&src, &spec).is_err(), "2 is not a colour");
}

#[test]
fn a_document_that_is_not_a_pdf_is_refused() {
    let mut spec = highlight();
    spec.page = 1;
    assert!(run(b"not a pdf at all", &spec).is_err());
}

#[test]
fn an_annotation_can_be_moved_and_reworded() {
    let src = build_pdf();
    let (written, _) = run(&src, &highlight()).expect("the highlight");
    let budget = Budget::unlimited();
    let mut g = budget.guard();
    let (out, report) = update_annotation(
        &written,
        1,
        0,
        Some([100.0, 40.0, 210.0, 62.0]),
        Some("Moved and reworded".to_string()),
        &budget,
        &mut g,
    )
    .expect("the update");
    assert_eq!(report.page, 1);
    let annotations = read(&out, 1).expect("the annotations");
    assert_eq!(annotations.len(), 1);
    // The mark moved with the rectangle.
    assert!((annotations[0].rect[0] - 100.0).abs() < 0.01);
    assert!((annotations[0].rect[1] - 40.0).abs() < 0.01);
    // And it says what it says now.
    assert_eq!(annotations[0].contents, "Moved and reworded");
    // The quads followed the rectangle: a mark that stayed would cover the
    // wrong words.
    assert!((annotations[0].quads[0][0] - 100.0).abs() < 0.01);
}

#[test]
fn an_annotation_that_is_not_there_is_refused() {
    let src = build_pdf();
    let (written, _) = run(&src, &highlight()).expect("the highlight");
    let budget = Budget::unlimited();
    let mut g = budget.guard();
    assert!(
        update_annotation(&written, 1, 7, None, None, &budget, &mut g).is_err(),
        "there is no seventh annotation"
    );
}

#[test]
fn a_page_with_no_annotations_carries_none() {
    let src = build_pdf();
    let annotations = read(&src, 1).expect("the annotations");
    assert!(annotations.is_empty());
}

/// A one-page PDF with extra catalog entries, a page `/Annots` array, and
/// extra objects — enough to plant pre-existing annotations and an AcroForm.
fn build_pdf_with(
    catalog_extra: &str,
    page_annots: &str,
    extra_objects: &[(u32, Vec<u8>)],
) -> Vec<u8> {
    let content = b"BT /F1 12 Tf 10 100 Td (Hello World) Tj ET\n";
    let mut stream = format!("<< /Length {} >>\nstream\n", content.len()).into_bytes();
    stream.extend_from_slice(content);
    stream.extend_from_slice(b"\nendstream");
    let mut objects: Vec<(u32, Vec<u8>)> = vec![
        (
            1,
            format!("<< /Type /Catalog /Pages 2 0 R{catalog_extra} >>").into_bytes(),
        ),
        (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec()),
        (
            3,
            format!(
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 300 200] /Contents 4 0 R /Resources << /Font << /F1 5 0 R >> >>{page_annots} >>"
            )
            .into_bytes(),
        ),
        (4, stream),
        (5, b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_vec()),
    ];
    objects.extend_from_slice(extra_objects);
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

/// The raw `/AP /N` appearance bytes of annotation `index` on `page`.
fn ap_content(src: &[u8], page: u32, index: usize) -> Vec<u8> {
    let budget = Budget::unlimited();
    let mut g = budget.guard();
    let startxref = selis_pdf_cos::xref::find_startxref(src, 4096).unwrap_or(0);
    let doc = selis_pdf_cos::parse_revisions(src, startxref, &budget, &mut g).expect("the parse");
    let pages = page_refs(src, &doc, &budget, &mut g).expect("the pages");
    let page_ref = page_reference(&pages, page).expect("the page");
    let Obj::Dict(page_dict) = resolve_ref(src, &doc, page_ref, &budget, &mut g).expect("page obj")
    else {
        panic!("the page is not a dictionary")
    };
    let refs = annotation_refs(src, &doc, &page_dict, &budget, &mut g).expect("the refs");
    let Obj::Dict(pairs) = resolve_ref(src, &doc, refs[index], &budget, &mut g).expect("annot obj")
    else {
        panic!("the annotation is not a dictionary")
    };
    let ap_ref = pairs
        .iter()
        .find(|(key, _)| key.as_slice() == b"AP")
        .and_then(|(_, value)| match value {
            Obj::Dict(entries) => entries
                .iter()
                .find(|(key, _)| key.as_slice() == b"N")
                .and_then(|(_, value)| match value {
                    Obj::Ref(reference) => Some(*reference),
                    _ => None,
                }),
            _ => None,
        })
        .expect("an /AP /N reference");
    let Obj::Stream { data, .. } =
        resolve_ref(src, &doc, ap_ref, &budget, &mut g).expect("the appearance")
    else {
        panic!("the appearance is not a stream")
    };
    data.to_vec()
}

fn ink_spec(paths: Vec<Vec<(f64, f64)>>) -> AnnotSpec {
    let mut spec = AnnotSpec::new(AnnotKind::Ink, 1, [0.0, 0.0, 0.0, 0.0], INK_COLOUR);
    spec.paths = paths;
    spec
}

// ---------------------------------------------------------------------------
// SL-5.ANNOT.03 — ink and stylus geometry
// ---------------------------------------------------------------------------

#[test]
fn ink_strokes_round_trip_with_their_point_counts() {
    let src = build_pdf();
    let mut spec = ink_spec(vec![
        vec![(10.0, 10.0), (30.0, 35.0), (60.0, 12.0)],
        vec![(15.0, 60.0), (25.0, 70.0)],
        vec![(80.0, 80.0)],
    ]);
    spec.width = 2.5;
    let (out, _) = run(&src, &spec).expect("the ink");
    let bytes = String::from_utf8_lossy(&out).to_string();
    assert!(bytes.contains("/Subtype /Ink"), "the mark is ink: {bytes}");
    assert!(
        bytes.contains("/BS"),
        "the border style is written: {bytes}"
    );
    // /InkList is an array of strokes (an array of arrays), not one flat list.
    assert!(bytes.contains("/InkList [["), "strokes are nested: {bytes}");
    let found = read(&out, 1).expect("the read");
    assert_eq!(found.len(), 1);
    let counts: Vec<usize> = found[0].ink_points.iter().map(Vec::len).collect();
    assert_eq!(
        counts,
        vec![3, 2, 1],
        "each stroke's point count reads back"
    );
    assert!((found[0].ink_points[0][1].1 - 35.0).abs() < 0.01);
    assert!((found[0].ink_points[2][0].0 - 80.0).abs() < 0.01);
}

#[test]
fn an_ink_rectangle_bounds_every_point_and_carries_a_border() {
    let src = build_pdf();
    let spec = ink_spec(vec![vec![(10.0, 10.0), (60.0, 40.0)]]);
    let (out, _) = run(&src, &spec).expect("the ink");
    let found = read(&out, 1).expect("the read");
    let rect = found[0].rect;
    assert!(
        rect[0] < 10.0 && rect[1] < 10.0,
        "the rect grows to the lower-left: {rect:?}"
    );
    assert!(
        rect[2] > 60.0 && rect[3] > 40.0,
        "the rect grows to the upper-right: {rect:?}"
    );
    let bytes = String::from_utf8_lossy(&out).to_string();
    assert!(
        bytes.contains("/W 1"),
        "a default border width is written: {bytes}"
    );
    assert!(
        bytes.contains("/S /S"),
        "solid strokes are written: {bytes}"
    );
}

#[test]
fn the_legacy_single_point_line_still_writes_one_stroke() {
    let src = build_pdf();
    let mut spec = AnnotSpec::new(AnnotKind::Ink, 1, [0.0, 0.0, 0.0, 0.0], INK_COLOUR);
    spec.points = vec![(10.0, 10.0), (20.0, 20.0)];
    let (out, _) = run(&src, &spec).expect("the ink");
    let found = read(&out, 1).expect("the read");
    assert_eq!(found[0].ink_points.len(), 1);
    assert_eq!(found[0].ink_points[0].len(), 2);
}

#[test]
fn empty_non_finite_and_oversized_strokes_are_refused() {
    let src = build_pdf();
    assert!(
        run(&src, &ink_spec(Vec::new())).is_err(),
        "no strokes at all"
    );
    assert!(
        run(&src, &ink_spec(vec![Vec::new()])).is_err(),
        "an empty stroke"
    );
    assert!(
        run(&src, &ink_spec(vec![vec![(f64::NAN, 1.0)]])).is_err(),
        "a NaN point"
    );
    assert!(
        run(&src, &ink_spec(vec![vec![(1.0, f64::INFINITY)]])).is_err(),
        "an infinite point"
    );
    let too_many_paths: Vec<Vec<(f64, f64)>> =
        (0..=MAX_INK_PATHS).map(|i| vec![(i as f64, 0.0)]).collect();
    assert!(
        run(&src, &ink_spec(too_many_paths)).is_err(),
        "more than {MAX_INK_PATHS} strokes"
    );
    let too_many_points: Vec<(f64, f64)> = (0..=MAX_INK_POINTS_PER_PATH)
        .map(|i| (i as f64, 0.0))
        .collect();
    assert!(
        run(&src, &ink_spec(vec![too_many_points])).is_err(),
        "more than {MAX_INK_POINTS_PER_PATH} points in one stroke"
    );
}

// ---------------------------------------------------------------------------
// SL-5.ANNOT.02 — appearance regeneration and /NeedAppearances
// ---------------------------------------------------------------------------

fn regenerate(src: &[u8]) -> Result<(Vec<u8>, AppearanceReport)> {
    let budget = Budget::unlimited();
    let mut g = budget.guard();
    regenerate_appearances(src, &budget, &mut g)
}

#[test]
fn a_current_appearance_is_not_touched() {
    let src = build_pdf();
    let (written, _) = run(&src, &highlight()).expect("the highlight");
    let (out, report) = regenerate(&written).expect("the regeneration");
    assert_eq!(report.regenerated, 0, "nothing was stale");
    assert_eq!(report.annotations, 1);
    assert_eq!(report.pages, 1);
    assert!(!report.need_appearances);
    assert_eq!(out, written, "an untouched document is returned verbatim");
}

#[test]
fn a_missing_appearance_is_regenerated_and_verified() {
    let annot = b"<< /Type /Annot /Subtype /Highlight /Rect [10 90 120 112] /C [1 0.92 0.23] /QuadPoints [10 112 120 112 10 90 120 90] >>".to_vec();
    let src = build_pdf_with("", " /Annots [6 0 R]", &[(6, annot)]);
    let found = read(&src, 1).expect("the read");
    assert_eq!(found.len(), 1, "the annotation is in the source");
    let (out, report) = regenerate(&src).expect("the regeneration");
    assert_eq!(report.regenerated, 1, "one appearance was rebuilt");
    assert_eq!(
        out.get(..src.len()),
        Some(&src[..]),
        "the original bytes are a verbatim prefix"
    );
    assert!(
        !ap_content(&out, 1, 0).is_empty(),
        "an /AP now carries bytes"
    );
    let after = read(&out, 1).expect("the read back");
    assert_eq!(after.len(), 1);
    assert_eq!(after[0].subtype, "Highlight");
}

#[test]
fn a_stale_appearance_is_rebuilt_with_the_new_geometry() {
    let src = build_pdf();
    let (written, _) = run(&src, &highlight()).expect("the highlight");
    let moved_rect = [100.0, 40.0, 210.0, 62.0];
    let budget = Budget::unlimited();
    let mut g = budget.guard();
    let (moved, _) = update_annotation(&written, 1, 0, Some(moved_rect), None, &budget, &mut g)
        .expect("the move");
    assert_eq!(
        ap_content(&moved, 1, 0),
        ap_content(&written, 1, 0),
        "the AP was not moved with the rectangle"
    );

    // A fresh write of the moved geometry is the deterministic oracle.
    let mut fresh = highlight();
    fresh.rect = moved_rect;
    fresh.quads = vec![[100.0, 40.0, 210.0, 62.0]];
    let (expected, _) = run(&src, &fresh).expect("the fresh write");

    let (out, report) = regenerate(&moved).expect("the regeneration");
    assert_eq!(report.regenerated, 1, "the stale AP was rebuilt");
    assert_eq!(
        out.get(..src.len()),
        Some(&src[..]),
        "the original bytes stay a verbatim prefix across revisions"
    );
    assert_eq!(
        ap_content(&out, 1, 0),
        ap_content(&expected, 1, 0),
        "regeneration is byte-identical to a fresh write (render parity)"
    );
    let found = read(&out, 1).expect("the read back");
    assert!((found[0].rect[0] - 100.0).abs() < 0.01);
}

#[test]
fn a_need_appearances_form_is_reported_and_left_intact() {
    let catalog = " /AcroForm << /NeedAppearances true /Fields [] >>";
    let src = build_pdf_with(catalog, "", &[]);
    let (out, report) = regenerate(&src).expect("the regeneration");
    assert!(report.need_appearances, "the flag is reported");
    assert_eq!(report.regenerated, 0);
    let bytes = String::from_utf8_lossy(&out).to_string();
    assert!(
        bytes.contains("/NeedAppearances true"),
        "the flag is not cleared: {bytes}"
    );
}

#[test]
fn an_indirect_need_appearances_form_is_reported() {
    let src = build_pdf_with(
        " /AcroForm 6 0 R",
        "",
        &[(6, b"<< /NeedAppearances true /Fields [] >>".to_vec())],
    );
    let (_, report) = regenerate(&src).expect("the regeneration");
    assert!(report.need_appearances, "an indirect /AcroForm is followed");
}

#[test]
fn a_markup_with_unreadable_geometry_is_a_typed_error() {
    // A two-number /Rect parses as PDF but cannot be regenerated honestly.
    let annot = b"<< /Type /Annot /Subtype /Highlight /Rect [10 90] /QuadPoints [10 112 120 112 10 90 120 90] >>".to_vec();
    let src = build_pdf_with("", " /Annots [6 0 R]", &[(6, annot)]);
    let budget = Budget::unlimited();
    let mut g = budget.guard();
    let err = regenerate_appearances(&src, &budget, &mut g).expect_err("the refusal");
    assert_eq!(err.code(), Code::ObjUnexpected);
}
