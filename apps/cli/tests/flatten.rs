//! `selis flatten` with a render check: a link annotation whose appearance
//! fills its box red is baked into the page, and rendering the flattened output
//! shows the red mark in the page content (no annotation involved).
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects
)]

use std::path::PathBuf;
use std::process::Command;

fn selis() -> Command {
    Command::new(env!("CARGO_BIN_EXE_selis"))
}

/// A one-page PDF with one link annotation whose normal appearance fills its
/// 20x20 box red, placed at Rect [50 50 70 70].
fn annotated_pdf() -> Vec<u8> {
    let appearance = b"1 0 0 rg\n0 0 20 20 re\nf\n";
    let objects: Vec<Vec<u8>> = vec![
        b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n".to_vec(),
        b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n".to_vec(),
        b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Annots [4 0 R] >>\nendobj\n".to_vec(),
        b"4 0 obj\n<< /Type /Annot /Subtype /Link /Rect [50 50 70 70] /AP << /N 5 0 R >> >>\nendobj\n".to_vec(),
        {
            let mut s =
                format!("5 0 obj\n<< /Type /XObject /Subtype /Form /BBox [0 0 20 20] /Length {} >>\nstream\n", appearance.len())
                    .into_bytes();
            s.extend_from_slice(appearance);
            s.extend_from_slice(b"\nendstream\nendobj\n");
            s
        },
    ];
    let mut out = b"%PDF-1.7\n".to_vec();
    let mut offsets = Vec::new();
    for object in &objects {
        offsets.push(out.len());
        out.extend_from_slice(object);
    }
    let xref = out.len();
    let size = objects.len() + 1;
    out.extend_from_slice(format!("xref\n0 {size}\n").as_bytes());
    out.extend_from_slice(b"0000000000 65535 f \n");
    for offset in offsets {
        out.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }
    out.extend_from_slice(
        format!("trailer\n<< /Size {size} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n").as_bytes(),
    );
    out
}

/// The RGB of one pixel in a P6 PPM.
fn ppm_pixel(ppm: &[u8], width: usize, x: usize, y: usize) -> (u8, u8, u8) {
    // Skip the three header lines (P6 / "w h" / "255"), each newline-terminated.
    let mut cursor = 0;
    let mut newlines = 0;
    while newlines < 3 && cursor < ppm.len() {
        if ppm[cursor] == b'\n' {
            newlines += 1;
        }
        cursor += 1;
    }
    let offset = cursor + (y * width + x) * 3;
    (ppm[offset], ppm[offset + 1], ppm[offset + 2])
}

#[test]
fn a_flattened_appearance_renders_in_the_page() {
    let tmp = std::env::temp_dir();
    let input = tmp.join(format!("selis-flat-in-{}.pdf", std::process::id()));
    let flattened = tmp.join(format!("selis-flat-out-{}.pdf", std::process::id()));
    let ppm = tmp.join(format!("selis-flat-{}.ppm", std::process::id()));
    std::fs::write(&input, annotated_pdf()).expect("write fixture");

    let flatten = selis()
        .arg("flatten")
        .arg(&input)
        .arg("-o")
        .arg(&flattened)
        .output()
        .expect("flatten");
    assert!(
        flatten.status.success(),
        "flatten stderr: {}",
        String::from_utf8_lossy(&flatten.stderr)
    );
    let bytes = std::fs::read(&flattened).expect("output");
    assert!(
        !String::from_utf8_lossy(&bytes).contains("/Annots"),
        "the annotation array is gone"
    );

    // Render the flattened page and read the baked mark back from the pixels.
    let render = selis()
        .arg("render")
        .arg(&flattened)
        .arg(&ppm)
        .arg("--format")
        .arg("ppm")
        .output()
        .expect("render");
    assert!(
        render.status.success(),
        "render stderr: {}",
        String::from_utf8_lossy(&render.stderr)
    );
    let pixels = std::fs::read(&ppm).expect("ppm");
    // Rect [50 50 70 70] in PDF space; image is top-down, so page y=60 is row 140.
    let (r, g, b) = ppm_pixel(&pixels, 200, 60, 140);
    assert!(
        r > 200 && g < 80 && b < 80,
        "the baked appearance renders red, got {r},{g},{b}"
    );

    for path in [&input, &flattened, &ppm] {
        let _ = std::fs::remove_file(path);
    }
}
