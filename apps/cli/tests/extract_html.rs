//! `selis extract --format html`: the page range is ONE positioned HTML
//! document, re-read and compared against the extraction before it is written.
//!
//! The old shape was a per-page printout wearing HTML clothes — one
//! `<p data-bbox>` per line, no canvas, and a page range concatenated into a
//! file with several `<html>` roots. What ships instead:
//!
//! * one document root for the whole range, whatever the range;
//! * one `<section>` per page at that page's own canvas size (72 DPI = one CSS
//!   px per PDF unit, the box `selis render --format png --dpi 72` paints);
//! * one span per recovered word, at the position the page's own transform
//!   puts it;
//! * the SL-3.TEXT.10 marker on the page that earned it, and nowhere else;
//! * a read-back check — the document is parsed by `selis-pdf-text`'s own
//!   strict reader and compared with the model, and a page with no area to
//!   place text on is refused rather than written.
//!
//! The other formats are per-page printouts and this slice must not disturb
//! them, so their bytes are pinned here too.
//!
//! ```text
//! cargo test -p selis-cli --test extract_html
//! ```
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::arithmetic_side_effects,
    clippy::indexing_slicing
)]

use std::path::PathBuf;
use std::process::Command;

/// See `extract_fidelity.rs`: `CARGO_BIN_EXE_selis` is authoritative in CI; a
/// locally shared target dir can be clobbered, so honour `SELIS_TEST_BIN`.
fn selis_bin() -> String {
    match std::env::var("SELIS_TEST_BIN") {
        Ok(p) if !p.is_empty() => p,
        _ => env!("CARGO_BIN_EXE_selis").to_string(),
    }
}

/// The low-confidence fixture: page 0 draws text it cannot recover, page 1
/// reads, page 2 is genuinely blank.
const TEXT10: &[u8] = include_bytes!("fixtures/text10_low_confidence.pdf");

fn write_fixture(name: &str, bytes: &[u8]) -> PathBuf {
    let mut path = std::env::temp_dir();
    path.push(format!("selis-cli-extract-html-{name}"));
    std::fs::write(&path, bytes).expect("write fixture");
    path
}

/// `selis extract --format <format> [extra args] <path>` → (code, stdout, stderr).
fn extract(path: &PathBuf, format: &str, extra: &[&str]) -> (i32, String, String) {
    let out = Command::new(selis_bin())
        .arg("extract")
        .arg("--format")
        .arg(format)
        .args(extra)
        .arg(path)
        .output()
        .expect("run selis extract");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// A two-page document whose pages are deliberately different sizes, each
/// showing two words of a standard-14 font. Written by hand: the test needs
/// per-page geometry, not a fixture generator.
///
/// Page 1 is US Letter (612×792), page 2 a small 400×300 box. At 72 DPI those
/// canvases are 612×792 px and 400×300 px, so `data-width`/`data-height` are
/// the PDF units themselves — and a section that carried the *other* page's box
/// would be visible in the assertion.
fn two_page_pdf() -> Vec<u8> {
    let first = "BT /F1 24 Tf 60 700 Td (First) Tj 60 0 Td (Page) Tj ET";
    let second = "BT /F1 18 Tf 40 200 Td (Second) Tj 90 0 Td (Page) Tj ET";
    let objects: Vec<Vec<u8>> = vec![
        b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n".to_vec(),
        b"2 0 obj\n<< /Type /Pages /Kids [3 0 R 5 0 R] /Count 2 >>\nendobj\n".to_vec(),
        b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
          /Resources << /Font << /F1 7 0 R >> >> /Contents 4 0 R >>\nendobj\n"
            .to_vec(),
        stream(4, first),
        b"5 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 400 300] \
          /Resources << /Font << /F1 7 0 R >> >> /Contents 6 0 R >>\nendobj\n"
            .to_vec(),
        stream(6, second),
        b"7 0 obj\n<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>\nendobj\n".to_vec(),
    ];
    build(objects)
}

/// One content stream object, `/Length` computed from the bytes it holds.
fn stream(id: usize, body: &str) -> Vec<u8> {
    format!(
        "{id} 0 obj\n<< /Length {} >>\nstream\n{body}\nendstream\nendobj\n",
        body.len()
    )
    .into_bytes()
}

/// A one-page document with the given `/MediaBox` and an empty content stream.
fn page_pdf(box_: &str) -> Vec<u8> {
    let objects: Vec<Vec<u8>> = vec![
        b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n".to_vec(),
        b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n".to_vec(),
        format!(
            "3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [{box_}] \
             /Resources << >> /Contents 4 0 R >>\nendobj\n"
        )
        .into_bytes(),
        b"4 0 obj\n<< /Length 0 >>\nstream\n\nendstream\nendobj\n".to_vec(),
    ];
    build(objects)
}

/// Assemble objects into a cross-reference PDF.
fn build(objects: Vec<Vec<u8>>) -> Vec<u8> {
    let mut out = b"%PDF-1.4\n".to_vec();
    let mut offsets = Vec::new();
    for object in &objects {
        offsets.push(out.len());
        out.extend_from_slice(object);
    }
    let xref = out.len();
    let size = offsets.len() + 1;
    out.extend_from_slice(format!("xref\n0 {size}\n").as_bytes());
    out.extend_from_slice(b"0000000000 65535 f \n");
    for offset in &offsets {
        out.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }
    out.extend_from_slice(
        format!("trailer\n<< /Size {size} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n").as_bytes(),
    );
    out
}

/// The document for the whole range is one HTML file with one section per page,
/// each on its own canvas, and it re-reads with every span's text and box
/// intact — the verification standard the slice was asked for.
#[test]
fn a_page_range_is_one_document_of_sections_on_their_own_canvases() {
    let path = write_fixture("two-pages.pdf", &two_page_pdf());
    let (code, document, err) = extract(&path, "html", &["--page", "0", "--last", "1"]);
    assert_eq!(code, 0, "extract failed: {err}");

    // One root, whatever the range.
    assert_eq!(document.matches("<!doctype html>").count(), 1);
    assert_eq!(document.matches("<html").count(), 1);
    assert_eq!(document.matches("</html>").count(), 1);
    assert_eq!(document.matches("<main>").count(), 1);

    // The title is the name the user typed.
    assert!(
        document.contains("<title>selis-cli-extract-html-two-pages</title>"),
        "{document}"
    );

    // Each page is a section numbered as the user selected it, sized by its own box.
    assert!(
        document.contains(
            "<section class=\"page\" data-page=\"1\" data-width=\"612\" data-height=\"792\" \
             style=\"width:612px;height:792px\">"
        ),
        "letter page: {document}"
    );
    assert!(
        document.contains(
            "<section class=\"page\" data-page=\"2\" data-width=\"400\" data-height=\"300\" \
             style=\"width:400px;height:300px\">"
        ),
        "small page: {document}"
    );

    // Read the file back independently of the writer that made it.
    let read = selis_pdf_text::read_positioned_html(&document).expect("re-read the document");
    assert_eq!(read.pages.len(), 2);
    assert_eq!(read.pages[0].number, 1);
    assert_eq!(
        (read.pages[0].width_px, read.pages[0].height_px),
        (612, 792)
    );
    assert_eq!(
        (read.pages[1].width_px, read.pages[1].height_px),
        (400, 300)
    );
    let words: Vec<String> = read
        .pages
        .iter()
        .flat_map(|page| page.spans.iter())
        .map(|span| span.text.clone())
        .collect();
    assert!(
        words.contains(&"First".to_string()) && words.contains(&"Second".to_string()),
        "re-read spans: {words:?}"
    );

    // Every span sits inside its own page's canvas, and carries a real box:
    // the position the page's transform puts the ink at, not a stacked line.
    for page in &read.pages {
        assert!(
            !page.spans.is_empty(),
            "page {} lost its words",
            page.number
        );
        for span in &page.spans {
            let [left, top, right, bottom] = span.box_px;
            assert!(left.is_finite() && top.is_finite(), "{span:?}");
            assert!(
                left >= 0.0
                    && top >= 0.0
                    && right <= f64::from(page.width_px)
                    && bottom <= f64::from(page.height_px),
                "span {span:?} leaves the {}x{} canvas of page {}",
                page.width_px,
                page.height_px,
                page.number
            );
            assert!(span.font_size_px > 0.0, "{span:?}");
        }
    }
    // The two pages do not share a canvas: the small page's words sit inside
    // 400x300, which the letter page's 612x792 would not have constrained.
    assert!(read.pages[1].spans.iter().all(|s| s.box_px[2] <= 400.0));
}

/// A selection names pages; the section carries the page's own number, not its
/// position in the selection.
#[test]
fn a_selected_page_is_numbered_as_itself() {
    let path = write_fixture("two-pages.pdf", &two_page_pdf());
    let (code, document, err) = extract(&path, "html", &["--page", "1"]);
    assert_eq!(code, 0, "extract failed: {err}");
    let read = selis_pdf_text::read_positioned_html(&document).expect("re-read");
    assert_eq!(read.pages.len(), 1);
    assert_eq!(read.pages[0].number, 2);
    assert_eq!(
        (read.pages[0].width_px, read.pages[0].height_px),
        (400, 300)
    );
    let text: String = read.pages[0]
        .spans
        .iter()
        .map(|span| span.text.as_str())
        .collect();
    assert!(text.contains("Second"), "{text}");
    assert!(!text.contains("First"), "page 1 leaked in: {text}");
}

/// `--output` writes the same verified bytes the terminal would show, and says
/// so on stderr with the page count.
#[test]
fn the_output_file_holds_the_document_stdout_shows() {
    let path = write_fixture("two-pages.pdf", &two_page_pdf());
    let mut file = std::env::temp_dir();
    file.push("selis-cli-extract-html-written.html");
    let _ = std::fs::remove_file(&file);

    let (code, stdout, err) = extract(
        &path,
        "html",
        &[
            "--page",
            "0",
            "--last",
            "1",
            "--output",
            file.to_str().unwrap(),
        ],
    );
    assert_eq!(code, 0, "extract failed: {err}");
    assert!(stdout.is_empty(), "the file got the document: {stdout}");
    assert!(
        err.contains("wrote 2 page(s)"),
        "page count on stderr: {err}"
    );

    let written = std::fs::read_to_string(&file).expect("read the written document");
    let (_, to_stdout, _) = extract(&path, "html", &["--page", "0", "--last", "1"]);
    assert_eq!(written, to_stdout, "the file and the terminal differ");
    selis_pdf_text::read_positioned_html(&written).expect("the written file re-reads");
    std::fs::remove_file(&file).ok();
}

/// A page with no area has no canvas to position text on. Refusing it is the
/// honest answer; writing a `0px` section that verifies perfectly while showing
/// nothing is not. The file is not created.
#[test]
fn a_page_with_no_area_is_refused_and_writes_no_file() {
    let path = write_fixture("zero-box.pdf", &page_pdf("0 0 0 0"));
    let mut file = std::env::temp_dir();
    file.push("selis-cli-extract-html-zero.html");
    let _ = std::fs::remove_file(&file);

    let (code, stdout, err) = extract(&path, "html", &["--output", file.to_str().unwrap()]);
    assert_ne!(code, 0, "a zero-area page must not be written: {stdout}");
    assert!(
        err.contains("no area to place text on") && err.contains("page 1"),
        "named refusal: {err}"
    );
    assert!(!file.exists(), "a refused run leaves no file behind");

    // A page the document does not have is refused before anything is read.
    let (code, _, err) = extract(&path, "html", &["--page", "7"]);
    assert_ne!(code, 0);
    assert!(err.contains("out of range"), "{err}");
}

/// The SL-3.TEXT.10 marker rides the page that earned it and no other, and a
/// page with nothing to say stays a clean section.
#[test]
fn the_low_confidence_marker_rides_its_own_page() {
    let path = write_fixture("text10.pdf", TEXT10);

    let (code, document, err) = extract(&path, "html", &["--page", "0"]);
    assert_eq!(code, 0, "extract failed: {err}");
    assert_eq!(
        document
            .matches(selis_pdf_text::LOW_CONFIDENCE_MARKER)
            .count(),
        1,
        "one marker for one flagged page: {document}"
    );
    let read = selis_pdf_text::read_positioned_html(&document).expect("re-read");
    assert!(read.pages[0].low_confidence);
    // The boxes are there; the characters are not. That is exactly what the
    // marker is for, and why silence would be a lie.
    assert!(
        read.pages[0].spans.iter().all(|span| span.text.is_empty()),
        "page 0 recovered text it did not: {:?}",
        read.pages[0].spans
    );

    // The page that reads has no marker, and the blank page that follows it
    // has none either — silence is the truth there.
    let (code, clean, err) = extract(&path, "html", &["--page", "1", "--last", "2"]);
    assert_eq!(code, 0, "extract failed: {err}");
    assert!(
        !clean.contains(selis_pdf_text::LOW_CONFIDENCE_MARKER),
        "{clean}"
    );
    let read = selis_pdf_text::read_positioned_html(&clean).expect("re-read");
    assert_eq!(read.pages.len(), 2);
    assert!(!read.pages[0].low_confidence && !read.pages[1].low_confidence);
    assert!(
        read.pages[0].spans.iter().any(|s| s.text == "Survived"),
        "page 1 words: {:?}",
        read.pages[0].spans
    );
    assert!(read.pages[1].spans.is_empty(), "the blank page gained text");
}

/// HTML moved out of the per-page printout loop; the formats that stayed there
/// must print exactly what they printed before, for a range as for one page.
#[test]
fn the_per_page_formats_are_untouched() {
    let path = write_fixture("two-pages.pdf", &two_page_pdf());
    for format in ["text", "md", "json"] {
        let (code, range, _) = extract(&path, format, &["--page", "0", "--last", "1"]);
        assert_eq!(code, 0, "{format} over a range failed");
        let (code_first, first, _) = extract(&path, format, &["--page", "0"]);
        let (code_second, second, _) = extract(&path, format, &["--page", "1"]);
        assert_eq!(code_first, 0, "{format} on page 1 failed");
        assert_eq!(code_second, 0, "{format} on page 2 failed");
        assert_eq!(
            range,
            format!("{first}\n{second}"),
            "{format}: a range is its pages"
        );
        assert!(
            !range.contains("<!doctype html>"),
            "{format} must not grow a document"
        );
    }

    // JSON keeps its per-run spans (the extraction that feeds HTML is the same
    // model, not a second one).
    let (code, json, _) = extract(&path, "json", &["--page", "0"]);
    assert_eq!(code, 0);
    assert!(json.contains("\"text\": \"First\""), "{json}");
    assert!(json.contains("\"text\": \"Page\""), "{json}");

    // And the HTML range is ONE document where the per-page formats are two.
    let (code, document, _) = extract(&path, "html", &["--page", "0", "--last", "1"]);
    assert_eq!(code, 0);
    assert_eq!(document.matches("<!doctype html>").count(), 1);
}
