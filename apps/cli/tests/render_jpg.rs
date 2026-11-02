//! `selis render --format jpg`: the lossy whole-page raster path, end to end.
//!
//! Two fixtures stand in for the two ends of the corpus. `minimal.pdf` is a
//! page painted entirely black, so any pixel that comes back bright is a coding
//! fault, and `text.pdf` is glyphs on paper, which is the content a quality
//! knob actually moves. JPEG is lossy, so the pixel assertions are bounds rather
//! than equality; what stays exact is that the file is the page at the DPI asked
//! for, that a second render is byte-identical (ADR-P0012), and that the
//! `jpeg` alias is the same encoder run.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::PathBuf;
use std::process::Command;

const MINIMAL: &[u8] = include_bytes!("../../../crates/selis-pdf-engine/src/fixtures/minimal.pdf");
const TEXT: &[u8] = include_bytes!("../../../crates/selis-pdf-engine/src/fixtures/text.pdf");

fn selis_bin() -> String {
    match std::env::var("SELIS_TEST_BIN") {
        Ok(p) if !p.is_empty() => p,
        _ => env!("CARGO_BIN_EXE_selis").to_string(),
    }
}

fn temp_path(name: &str) -> PathBuf {
    let mut path = std::env::temp_dir();
    path.push(format!("selis-cli-render-jpg-{name}"));
    path
}

fn render(
    page: &PathBuf,
    output: &PathBuf,
    dpi: u32,
    format: &str,
    quality: Option<u32>,
) -> (i32, String) {
    let mut command = Command::new(selis_bin());
    command
        .arg("render")
        .arg("--page")
        .arg("0")
        .arg("--dpi")
        .arg(dpi.to_string())
        .arg("--format")
        .arg(format);
    if let Some(quality) = quality {
        command.arg("--quality").arg(quality.to_string());
    }
    command.arg(page).arg(output);
    let out = command.output().expect("run selis render");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// Read the file back through the engine's own JPEG reader, after checking the
/// two markers that say the writer finished the stream.
fn decode(path: &PathBuf) -> selis_image::Image {
    let bytes = std::fs::read(path).expect("read jpg");
    assert_eq!(bytes.get(..2), Some([0xFF, 0xD8].as_slice()), "{path:?}");
    assert_eq!(
        bytes.get(bytes.len().saturating_sub(2)..),
        Some([0xFF, 0xD9].as_slice()),
        "{path:?}"
    );
    selis_image::decode_jpeg(&bytes, &mut selis_sandbox::Budget::unlimited().guard())
        .unwrap_or_else(|e| panic!("{} does not decode: {e}", path.display()))
}

fn write_fixture(name: &str, bytes: &[u8]) -> PathBuf {
    let path = temp_path(name);
    std::fs::write(&path, bytes).expect("write fixture");
    path
}

#[test]
fn jpg_render_is_the_page_at_the_requested_dpi() {
    let pdf = write_fixture("fixture.pdf", MINIMAL);
    let jpg = temp_path("72.jpg");
    let (code, err) = render(&pdf, &jpg, 72, "jpg", Some(90));
    assert_eq!(code, 0, "render failed: {err}");
    let image = decode(&jpg);
    assert_eq!((image.width, image.height), (100, 100));
    // A black page has to stay black: the worst RGB sample over the whole frame
    // is the witness, and it is a bound because the coder is allowed to ring.
    let worst = image
        .rgba
        .chunks_exact(4)
        .filter_map(|px| {
            px.get(..3)
                .map(|rgb| rgb.iter().fold(0u64, |m, v| m.max(u64::from(*v))))
        })
        .max()
        .unwrap_or(255);
    assert!(worst < 64, "a black page came back at {worst} levels");
    assert!(
        image
            .rgba
            .chunks_exact(4)
            .all(|px| px.get(3).copied() == Some(255)),
        "the JPEG carries transparency"
    );

    let jpg144 = temp_path("144.jpg");
    let (code, err) = render(&pdf, &jpg144, 144, "jpg", Some(90));
    assert_eq!(code, 0, "render failed: {err}");
    let image144 = decode(&jpg144);
    assert_eq!((image144.width, image144.height), (200, 200));
}

#[test]
fn the_quality_knob_moves_the_file() {
    let pdf = write_fixture("quality.pdf", TEXT);
    let coarse = temp_path("q10.jpg");
    let fine = temp_path("q95.jpg");
    assert_eq!(render(&pdf, &coarse, 72, "jpg", Some(10)).0, 0);
    assert_eq!(render(&pdf, &fine, 72, "jpg", Some(95)).0, 0);
    let coarse_bytes = std::fs::read(&coarse).expect("read coarse");
    let fine_bytes = std::fs::read(&fine).expect("read fine");
    assert!(
        coarse_bytes.len() < fine_bytes.len(),
        "quality 10 produced {} bytes against quality 95's {}",
        coarse_bytes.len(),
        fine_bytes.len()
    );
    decode(&coarse);
    decode(&fine);
}

#[test]
fn jpg_render_is_byte_identical_on_a_second_run() {
    let pdf = write_fixture("determinism.pdf", TEXT);
    let first = temp_path("first.jpg");
    let second = temp_path("second.jpg");
    assert_eq!(render(&pdf, &first, 72, "jpg", Some(85)).0, 0);
    assert_eq!(render(&pdf, &second, 72, "jpg", Some(85)).0, 0);
    assert_eq!(
        std::fs::read(&first).expect("first"),
        std::fs::read(&second).expect("second")
    );
}

#[test]
fn jpeg_is_the_same_thing_as_jpg() {
    let pdf = write_fixture("alias.pdf", TEXT);
    let long = temp_path("long.jpg");
    let short = temp_path("short.jpg");
    assert_eq!(render(&pdf, &long, 72, "jpeg", Some(80)).0, 0);
    assert_eq!(render(&pdf, &short, 72, "jpg", Some(80)).0, 0);
    assert_eq!(
        std::fs::read(&long).expect("long alias"),
        std::fs::read(&short).expect("short name")
    );
}

#[test]
fn quality_outside_the_writers_range_is_refused() {
    let pdf = write_fixture("bounds.pdf", MINIMAL);
    for quality in [0u32, 101, 1000] {
        let out = temp_path(&format!("q{quality}.jpg"));
        let (code, err) = render(&pdf, &out, 72, "jpg", Some(quality));
        assert_ne!(code, 0, "quality {quality} was accepted");
        assert!(
            err.contains("JPEG quality must be between 1 and 100"),
            "{err}"
        );
    }
}

#[test]
fn an_unknown_format_names_the_ones_that_exist() {
    let pdf = write_fixture("unknown.pdf", MINIMAL);
    let out = temp_path("unknown.tiff");
    let (code, err) = render(&pdf, &out, 72, "tiff", None);
    assert_ne!(code, 0);
    assert!(err.contains("unknown output format"), "{err}");
    assert!(err.contains("ppm, png or jpg"), "{err}");
}
