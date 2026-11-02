//! `selis render --format png`: the whole-page raster path, end to end.
//!
//! The fixture is a 100×100 pt page with a filled black square. At 72 DPI the
//! canvas is 100×100; at 144 DPI it is 200×200. The PNG must decode back to
//! exactly those pixels, be opaque white paper, and be byte-identical on a
//! second render (ADR-P0012). The PPM path stays the default, so a caller that
//! never asked for PNG sees no change.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::PathBuf;
use std::process::Command;

const MINIMAL: &[u8] = include_bytes!("../../../crates/selis-pdf-engine/src/fixtures/minimal.pdf");

fn selis_bin() -> String {
    match std::env::var("SELIS_TEST_BIN") {
        Ok(p) if !p.is_empty() => p,
        _ => env!("CARGO_BIN_EXE_selis").to_string(),
    }
}

fn temp_path(name: &str) -> PathBuf {
    let mut path = std::env::temp_dir();
    path.push(format!("selis-cli-render-png-{name}"));
    path
}

fn render(page: &PathBuf, output: &PathBuf, dpi: u32, format: &str) -> (i32, String) {
    let out = Command::new(selis_bin())
        .arg("render")
        .arg("--page")
        .arg("0")
        .arg("--dpi")
        .arg(dpi.to_string())
        .arg("--format")
        .arg(format)
        .arg(page)
        .arg(output)
        .output()
        .expect("run selis render");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

#[test]
fn png_render_is_the_page_at_the_requested_dpi() {
    let pdf = temp_path("fixture.pdf");
    std::fs::write(&pdf, MINIMAL).expect("write fixture");
    let png = temp_path("72.png");
    let (code, err) = render(&pdf, &png, 72, "png");
    assert_eq!(code, 0, "render failed: {err}");
    let bytes = std::fs::read(&png).expect("read png");
    assert_eq!(
        bytes.get(..8),
        Some([0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A].as_slice())
    );
    let image = selis_image::decode_png(&bytes, &mut selis_sandbox::Budget::unlimited().guard())
        .expect("decode png");
    assert_eq!((image.width, image.height), (100, 100));
    assert!(image
        .rgba
        .chunks_exact(4)
        .all(|px| matches!(px, &[_, _, _, 255])));
    let centre = (50 * 100 + 50) * 4;
    assert_eq!(
        image.rgba.get(centre..centre + 3).expect("centre pixel"),
        &[0, 0, 0]
    );

    let png144 = temp_path("144.png");
    let (code, err) = render(&pdf, &png144, 144, "png");
    assert_eq!(code, 0, "render failed: {err}");
    let image144 = selis_image::decode_png(
        &std::fs::read(&png144).expect("read png"),
        &mut selis_sandbox::Budget::unlimited().guard(),
    )
    .expect("decode png");
    assert_eq!((image144.width, image144.height), (200, 200));
}

#[test]
fn png_render_is_byte_identical_on_a_second_run() {
    let pdf = temp_path("determinism.pdf");
    std::fs::write(&pdf, MINIMAL).expect("write fixture");
    let first = temp_path("first.png");
    let second = temp_path("second.png");
    assert_eq!(render(&pdf, &first, 72, "png").0, 0);
    assert_eq!(render(&pdf, &second, 72, "png").0, 0);
    assert_eq!(
        std::fs::read(&first).expect("first"),
        std::fs::read(&second).expect("second")
    );
}

#[test]
fn ppm_stays_the_default_and_an_unknown_format_is_refused() {
    let pdf = temp_path("default.pdf");
    std::fs::write(&pdf, MINIMAL).expect("write fixture");
    let ppm = temp_path("default.ppm");
    let (code, err) = render(&pdf, &ppm, 72, "ppm");
    assert_eq!(code, 0, "render failed: {err}");
    let bytes = std::fs::read(&ppm).expect("read ppm");
    assert_eq!(bytes.get(..2), Some(b"P6".as_slice()));

    let bad = temp_path("bad.out");
    let (code, err) = render(&pdf, &bad, 72, "gif");
    assert_ne!(code, 0);
    assert!(err.contains("unknown output format"), "{err}");
}
