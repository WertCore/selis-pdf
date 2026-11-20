//! SL-5.SIGN.07 CLI binding: `selis sign --logo <PNG>` decodes the file to RGB
//! before the detached CMS write; undecodable bytes are refused before output.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::arithmetic_side_effects
)]

use std::process::Command;

const KEY: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../crates/selis-pdf-sign/tests/fixtures/leaf.key"
);
const LEAF: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../crates/selis-pdf-sign/tests/fixtures/leaf.crt"
);
const ROOT: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../crates/selis-pdf-sign/tests/fixtures/root.crt"
);

fn selis() -> Command {
    Command::new(env!("CARGO_BIN_EXE_selis"))
}

fn one_page_pdf() -> Vec<u8> {
    let objects: Vec<&[u8]> = vec![
        b"<< /Type /Catalog /Pages 2 0 R >>",
        b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>",
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << >> >>",
    ];
    let mut out = b"%PDF-1.7\n".to_vec();
    let mut offsets = Vec::new();
    for (index, body) in objects.iter().enumerate() {
        offsets.push(out.len());
        out.extend_from_slice(format!("{} 0 obj\n", index + 1).as_bytes());
        out.extend_from_slice(body);
        out.extend_from_slice(b"\nendobj\n");
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

fn temp(name: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!("selis-sign-logo-{name}-{}", std::process::id()))
}

#[test]
fn a_png_logo_is_decoded_into_the_signature_appearance() {
    let input = temp("in.pdf");
    let logo = temp("logo.png");
    let output = temp("out.pdf");
    std::fs::write(&input, one_page_pdf()).expect("fixture");

    let budget = selis_sandbox::Budget::unlimited();
    let mut g = budget.guard();
    let image = selis_image::Image {
        width: 2,
        height: 2,
        rgba: vec![
            255, 0, 0, 255, 0, 0, 255, 255, 0, 255, 0, 255, 255, 255, 0, 0,
        ],
    };
    let png = selis_image::encode_png(&image, 6, &mut g).expect("encode png");
    std::fs::write(&logo, &png).expect("logo");

    let run = selis()
        .arg("sign")
        .arg(&input)
        .arg("-o")
        .arg(&output)
        .arg("--key")
        .arg(KEY)
        .arg("--cert")
        .arg(LEAF)
        .arg("--cert")
        .arg(ROOT)
        .arg("--time")
        .arg("20261104120000Z")
        .arg("--name")
        .arg("Ada Lovelace")
        .arg("--logo")
        .arg(&logo)
        .output()
        .expect("sign");
    assert!(
        run.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&run.stderr)
    );
    let out = std::fs::read(&output).expect("output");
    let text = String::from_utf8_lossy(&out);
    assert!(
        text.contains("/Subtype /Image"),
        "the logo XObject is written"
    );
    // Alpha was dropped: these are exactly the 12 raw RGB samples from above.
    assert!(
        out.windows(12)
            .any(|w| w == [255, 0, 0, 0, 0, 255, 0, 255, 0, 255, 255, 0]),
        "decoded RGB bytes are embedded"
    );

    for path in [&input, &logo, &output] {
        let _ = std::fs::remove_file(path);
    }
}

#[test]
fn undecodable_logo_bytes_are_refused_before_a_write() {
    let input = temp("refuse-in.pdf");
    let logo = temp("bad-logo.png");
    let output = temp("refuse-out.pdf");
    std::fs::write(&input, one_page_pdf()).expect("fixture");
    std::fs::write(&logo, b"not an image").expect("logo");

    let run = selis()
        .arg("sign")
        .arg(&input)
        .arg("-o")
        .arg(&output)
        .arg("--key")
        .arg(KEY)
        .arg("--cert")
        .arg(LEAF)
        .arg("--time")
        .arg("20261104120000Z")
        .arg("--name")
        .arg("Ada Lovelace")
        .arg("--logo")
        .arg(&logo)
        .output()
        .expect("sign");
    assert!(!run.status.success());
    assert!(!output.exists(), "a refusal leaves no output");
    assert!(
        String::from_utf8_lossy(&run.stderr).contains("not a PNG or JPEG"),
        "{}",
        String::from_utf8_lossy(&run.stderr)
    );

    for path in [&input, &logo] {
        let _ = std::fs::remove_file(path);
    }
}
