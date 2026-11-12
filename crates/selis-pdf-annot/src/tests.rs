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
	let mut spec = AnnotSpec::new(AnnotKind::Text, 1, [200.0, 150.0, 220.0, 170.0], NOTE_COLOUR);
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
	let mut spec = AnnotSpec::new(AnnotKind::Text, 1, [200.0, 150.0, 220.0, 170.0], NOTE_COLOUR);
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
	assert!(bytes.contains("/Subtype /Ink"), "the mark is not an ink: {bytes}");
	assert!(bytes.contains("/InkList"), "the mark has no points: {bytes}");
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
	assert!(run(&src, &spec).is_err(), "the rectangle is the wrong way round");
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
fn a_page_with_no_annotations_carries_none() {
	let src = build_pdf();
	let annotations = read(&src, 1).expect("the annotations");
	assert!(annotations.is_empty());
}
