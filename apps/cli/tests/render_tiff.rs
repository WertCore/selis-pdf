//! `selis render --format tif`: the lossless multi-page container, one page.
//!
//! TIFF is the format whose whole point is that several pages live in one file,
//! and the CLI renders one page at a time — so what these tests can prove is the
//! container itself: the file opens as a TIFF, holds exactly one page, and gives
//! back the page's samples *unchanged*. There is no bound to allow for, because
//! Deflate is lossless, and no quality knob to move: the same pixels at the same
//! DPI must produce the same bytes twice (ADR-P0012), and a `--quality` argument
//! must not change them.
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
    path.push(format!("selis-cli-render-tif-{name}"));
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

/// Read the file back through the engine's own TIFF reader, after checking the
/// header that says a TIFF writer started the document.
fn decode(path: &PathBuf) -> Vec<selis_image::Image> {
    let bytes = std::fs::read(path).expect("read tif");
    assert_eq!(bytes.get(..2), Some(b"II".as_slice()), "{path:?}");
    assert_eq!(bytes.get(2..4), Some([0x2A, 0x00].as_slice()), "{path:?}");
    selis_image::decode_tiff(&bytes, &mut selis_sandbox::Budget::unlimited().guard())
        .unwrap_or_else(|e| panic!("{} does not decode: {e}", path.display()))
}

fn write_fixture(name: &str, bytes: &[u8]) -> PathBuf {
    let path = temp_path(name);
    std::fs::write(&path, bytes).expect("write fixture");
    path
}

#[test]
fn tif_render_is_one_page_at_the_requested_dpi() {
    let pdf = write_fixture("fixture.pdf", MINIMAL);
    let tif = temp_path("72.tif");
    let (code, err) = render(&pdf, &tif, 72, "tif", None);
    assert_eq!(code, 0, "render failed: {err}");
    let pages = decode(&tif);
    assert_eq!(pages.len(), 1, "one page in, one page in the container");
    let page = pages.first().expect("one page");
    assert_eq!((page.width, page.height), (100, 100));
    // A black page rendered through a lossless coder is still black, sample for
    // sample: no bound, no allowance, no ringing.
    assert!(
        page.rgba
            .chunks_exact(4)
            .all(|px| matches!(px, [0, 0, 0, 255])),
        "a black page came back with other samples"
    );

    let tif144 = temp_path("144.tif");
    let (code, err) = render(&pdf, &tif144, 144, "tif", None);
    assert_eq!(code, 0, "render failed: {err}");
    let page144 = decode(&tif144).pop().expect("one page");
    assert_eq!((page144.width, page144.height), (200, 200));
}

#[test]
fn a_text_page_comes_back_exactly_as_the_png_leg_sees_it() {
    // Two owned lossless writers, the same page, the same pixels: the TIFF's
    // samples must equal the PNG's, because neither is allowed to lose anything.
    let pdf = write_fixture("text.pdf", TEXT);
    let png = temp_path("text.png");
    let tif = temp_path("text.tif");
    assert_eq!(render(&pdf, &png, 96, "png", None).0, 0);
    assert_eq!(render(&pdf, &tif, 96, "tif", None).0, 0);
    let from_png = selis_image::decode_png(
        &std::fs::read(&png).expect("read png"),
        &mut selis_sandbox::Budget::unlimited().guard(),
    )
    .expect("decode png");
    let from_tif = decode(&tif).pop().expect("one page");
    assert_eq!(from_png.width, from_tif.width);
    assert_eq!(from_png.height, from_tif.height);
    assert_eq!(
        from_png.rgba, from_tif.rgba,
        "the TIFF does not hold the page the PNG holds"
    );
}

#[test]
fn tif_render_is_byte_identical_on_a_second_run() {
    let pdf = write_fixture("determinism.pdf", TEXT);
    let first = temp_path("first.tif");
    let second = temp_path("second.tif");
    assert_eq!(render(&pdf, &first, 72, "tif", None).0, 0);
    assert_eq!(render(&pdf, &second, 72, "tif", None).0, 0);
    assert_eq!(
        std::fs::read(&first).expect("first"),
        std::fs::read(&second).expect("second")
    );
}

#[test]
fn tiff_is_the_same_thing_as_tif() {
    let pdf = write_fixture("alias.pdf", TEXT);
    let long = temp_path("long.tif");
    let short = temp_path("short.tif");
    assert_eq!(render(&pdf, &long, 72, "tiff", None).0, 0);
    assert_eq!(render(&pdf, &short, 72, "tif", None).0, 0);
    assert_eq!(
        std::fs::read(&long).expect("long alias"),
        std::fs::read(&short).expect("short name")
    );
}

#[test]
fn quality_does_not_move_a_lossless_file() {
    let pdf = write_fixture("quality.pdf", TEXT);
    let coarse = temp_path("q10.tif");
    let fine = temp_path("q95.tif");
    assert_eq!(render(&pdf, &coarse, 72, "tif", Some(10)).0, 0);
    assert_eq!(render(&pdf, &fine, 72, "tif", Some(95)).0, 0);
    assert_eq!(
        std::fs::read(&coarse).expect("coarse"),
        std::fs::read(&fine).expect("fine"),
        "a lossless container has no quality to be given"
    );
    decode(&coarse);
    decode(&fine);
}
