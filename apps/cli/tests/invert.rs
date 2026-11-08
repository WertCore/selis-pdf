//! `selis invert` end-to-end: it writes a valid image-PDF of the inverted
//! raster and never rewrites the input. Pixel-level inversion is proven by the
//! engine's own unit test; this proves the command wires through.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects
)]

use std::path::PathBuf;
use std::process::Command;

fn fixture_path() -> PathBuf {
    let mut p = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    p.push("..");
    p.push("..");
    p.push("corpus");
    p.push("fixtures");
    p.push("verification_features.pdf");
    p
}

fn selis() -> Command {
    Command::new(env!("CARGO_BIN_EXE_selis"))
}

#[test]
fn invert_writes_an_image_pdf_without_touching_the_input() {
    let fixture = fixture_path();
    let before = std::fs::read(&fixture).expect("fixture");
    let out_path = std::env::temp_dir().join(format!("selis-invert-{}.pdf", std::process::id()));

    let out = selis()
        .arg("invert")
        .arg(&fixture)
        .arg(&out_path)
        .arg("--dpi")
        .arg("72")
        .output()
        .expect("run");
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let bytes = std::fs::read(&out_path).expect("output");
    assert!(bytes.starts_with(b"%PDF-"), "output is a PDF");
    let text = String::from_utf8_lossy(&bytes);
    assert!(
        text.contains("/Image"),
        "the inverted page is an embedded image"
    );
    assert!(
        text.contains("/FlateDecode"),
        "the raster is Flate-compressed"
    );

    let after = std::fs::read(&fixture).expect("fixture");
    assert_eq!(before, after, "the input must not change");
    let _ = std::fs::remove_file(&out_path);
}
