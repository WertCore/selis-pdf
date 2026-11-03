//! `selis render --format webp`: the lossless WebP, one page.
//!
//! WebP is the format whose claim is a smaller file for exactly the same
//! pixels, and the CLI renders one page at a time — so what these tests can
//! prove is the container and the claim: the file opens as a VP8L WebP, gives
//! back the page's samples *unchanged* including its alpha, and is no bigger
//! than the PNG of the same page. There is no bound to allow for, because VP8L
//! is lossless, and no quality knob to move: the same pixels at the same DPI
//! must produce the same bytes twice (ADR-P0012), and a `--quality` argument
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
    path.push(format!("selis-cli-render-webp-{name}"));
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

/// Read the file back through the engine's own WebP reader, after checking the
/// header that says a VP8L WebP writer started the document.
fn decode(path: &PathBuf) -> selis_image::Image {
    let bytes = std::fs::read(path).expect("read webp");
    assert_eq!(bytes.get(..4), Some(b"RIFF".as_slice()), "{path:?}");
    assert_eq!(bytes.get(8..12), Some(b"WEBP".as_slice()), "{path:?}");
    assert_eq!(bytes.get(12..16), Some(b"VP8L".as_slice()), "{path:?}");
    selis_image::decode_webp(&bytes, &mut selis_sandbox::Budget::unlimited().guard())
        .unwrap_or_else(|e| panic!("{} does not decode: {e}", path.display()))
}

fn write_fixture(name: &str, bytes: &[u8]) -> PathBuf {
    let path = temp_path(name);
    std::fs::write(&path, bytes).expect("write fixture");
    path
}

#[test]
fn webp_render_is_the_page_at_the_requested_dpi() {
    let pdf = write_fixture("fixture.pdf", MINIMAL);
    let webp = temp_path("72.webp");
    let (code, err) = render(&pdf, &webp, 72, "webp", None);
    assert_eq!(code, 0, "render failed: {err}");
    let page = decode(&webp);
    assert_eq!((page.width, page.height), (100, 100));
    // A black page rendered through a lossless coder is still black, sample for
    // sample: no bound, no allowance, no ringing.
    assert!(
        page.rgba
            .chunks_exact(4)
            .all(|px| matches!(px, [0, 0, 0, 255])),
        "a black page came back with other samples"
    );

    let webp144 = temp_path("144.webp");
    let (code, err) = render(&pdf, &webp144, 144, "webp", None);
    assert_eq!(code, 0, "render failed: {err}");
    let page144 = decode(&webp144);
    assert_eq!((page144.width, page144.height), (200, 200));
}

#[test]
fn a_text_page_comes_back_exactly_as_the_png_leg_sees_it() {
    // Two owned lossless writers, the same page, the same pixels: the WebP's
    // samples must equal the PNG's, alpha included, because neither is allowed
    // to lose anything — and the WebP is the one that has to be smaller.
    let pdf = write_fixture("text.pdf", TEXT);
    let png = temp_path("text.png");
    let webp = temp_path("text.webp");
    assert_eq!(render(&pdf, &png, 96, "png", None).0, 0);
    assert_eq!(render(&pdf, &webp, 96, "webp", None).0, 0);
    let from_png = selis_image::decode_png(
        &std::fs::read(&png).expect("read png"),
        &mut selis_sandbox::Budget::unlimited().guard(),
    )
    .expect("decode png");
    let from_webp = decode(&webp);
    assert_eq!(from_png.width, from_webp.width);
    assert_eq!(from_png.height, from_webp.height);
    assert_eq!(
        from_png.rgba, from_webp.rgba,
        "the WebP does not hold the page the PNG holds"
    );
    assert!(
        std::fs::read(&webp).expect("read webp").len()
            <= std::fs::read(&png).expect("read png").len(),
        "the WebP is not the smaller lossless file it is shipped for being"
    );
}

#[test]
fn webp_render_is_byte_identical_on_a_second_run() {
    let pdf = write_fixture("determinism.pdf", TEXT);
    let first = temp_path("first.webp");
    let second = temp_path("second.webp");
    assert_eq!(render(&pdf, &first, 72, "webp", None).0, 0);
    assert_eq!(render(&pdf, &second, 72, "webp", None).0, 0);
    assert_eq!(
        std::fs::read(&first).expect("first"),
        std::fs::read(&second).expect("second")
    );
}

#[test]
fn quality_does_not_move_a_lossless_file() {
    let pdf = write_fixture("quality.pdf", TEXT);
    let coarse = temp_path("q10.webp");
    let fine = temp_path("q95.webp");
    assert_eq!(render(&pdf, &coarse, 72, "webp", Some(10)).0, 0);
    assert_eq!(render(&pdf, &fine, 72, "webp", Some(95)).0, 0);
    assert_eq!(
        std::fs::read(&coarse).expect("coarse"),
        std::fs::read(&fine).expect("fine"),
        "a lossless file has no quality to be given"
    );
    decode(&coarse);
    decode(&fine);
}
