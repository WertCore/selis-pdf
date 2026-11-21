//! SL-5.SIGN.07 end-to-end evidence: a typed appearance template with a
//! caller-supplied logo becomes a real `/AP /N` form on a detached-CMS
//! signature widget. The source stays a verbatim prefix, the signature still
//! verifies, the /AP structure reads back, and the page renders identically
//! outside the widget rectangle.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss
)]

use selis_pdf_cos::copy::resolve_ref;
use selis_pdf_cos::{Obj, Ref};
use selis_pdf_engine::{RenderedPage, Session};
use selis_pdf_sign::{sign_digital, signatures, DigitalOptions, SignError, SignatureLogo};
#[cfg(feature = "logo")]
use selis_pdf_sign::{sign_digital_with_encoded_logo, verify_signature_appearance};
use selis_sandbox::{Budget, CancelToken, FixedClock, Surface};

const LEAF: &str = include_str!("fixtures/leaf.crt");
const ROOT: &str = include_str!("fixtures/root.crt");
const LEAF_KEY: &str = include_str!("fixtures/leaf.key");
const SIGNING_TIME: &str = "20261104120000Z";
const NOW: i64 = 1_793_793_600;

/// The widget rectangle used below, in display space (bottom-left origin).
const RECT: [f64; 4] = [72.0, 600.0, 320.0, 690.0];

fn budget() -> Budget {
    Budget::profile(Surface::Viewer)
}

/// A 2x2 red/blue checkerboard: raw RGB pixels supplied by the caller.
fn test_logo() -> SignatureLogo {
    SignatureLogo::new(2, 2, vec![255, 0, 0, 0, 0, 255, 0, 255, 0, 255, 255, 0])
}

/// A one-page PDF with a blue rectangle well away from the signature box, so
/// a render-parity comparison has non-white content to protect.
fn synthetic_pdf() -> Vec<u8> {
    let content = b"0 0 1 rg\n50 50 250 150 re\nf\n";
    let stream = format!("<< /Length {} >>\nstream\n", content.len()).into_bytes();
    let mut stream = stream;
    stream.extend_from_slice(content);
    stream.extend_from_slice(b"\nendstream");
    let objects: Vec<(u32, Vec<u8>)> = vec![
        (1, b"<< /Type /Catalog /Pages 2 0 R >>".to_vec()),
        (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec()),
        (
            3,
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << >> /Contents 4 0 R >>"
                .to_vec(),
        ),
        (4, stream),
    ];
    let max = objects.iter().map(|(num, _)| *num).max().unwrap_or(0);
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
    for index in 1..=max {
        out.extend_from_slice(format!("{:010} 00000 n \n", offsets[index as usize]).as_bytes());
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

fn options(logo: Option<SignatureLogo>) -> DigitalOptions {
    DigitalOptions {
        page: 1,
        rect: RECT,
        field_name: "Signature1".to_string(),
        signer_name: "Ada Lovelace".to_string(),
        reason: Some("Approved".to_string()),
        location: Some("London".to_string()),
        signing_time: SIGNING_TIME.to_string(),
        key_pem: LEAF_KEY.to_string(),
        certificates_pem: vec![LEAF.to_string(), ROOT.to_string()],
        logo,
        strokes: Vec::new(),
    }
}

fn render(src: &[u8]) -> RenderedPage {
    let clock = FixedClock(0);
    let session = Session::open(src.to_vec(), &budget(), &clock).expect("open");
    let mut g = budget().guard_with(&clock, CancelToken::new());
    session
        .render_page_rgba(0, 72.0, &budget(), &mut g)
        .expect("render")
}

/// Collect every stream object reachable from any revision, keyed by its
/// `/Subtype` name. This is an independent read-back of what was actually
/// serialized, not an echo of the writer's in-memory values.
/// A COS dictionary as read back from the output bytes.
type Dict = Vec<(selis_bytes::Bytes, Obj)>;

fn streams(out: &[u8]) -> Vec<(Dict, Vec<u8>)> {
    let b = budget();
    let mut g = b.guard();
    let start = selis_pdf_cos::xref::find_startxref(out, 4096).expect("startxref");
    let doc = selis_pdf_cos::parse_revisions(out, start, &b, &mut g).expect("parse revisions");
    let mut found = Vec::new();
    for revision in doc.revisions() {
        for num in revision.entries.keys() {
            if let Ok(Obj::Stream { dict, data }) =
                resolve_ref(out, &doc, Ref::new(*num, 0), &b, &mut g)
            {
                found.push((dict, data.as_slice().to_vec()));
            }
        }
    }
    found
}

fn subtype(dict: &Dict) -> Option<&[u8]> {
    dict.iter()
        .find(|(key, _)| key.as_slice() == b"Subtype")
        .and_then(|(_, value)| match value {
            Obj::Name(name) => Some(name.as_slice()),
            _ => None,
        })
}

#[test]
fn a_typed_appearance_becomes_a_readable_ap_with_a_logo() {
    let src = synthetic_pdf();
    let logo = test_logo();
    let (out, report) = sign_digital(&src, &options(Some(logo)), &budget(), &mut budget().guard())
        .expect("sign with a logo");

    // The original bytes are the output's verbatim prefix (incremental I3).
    assert!(out.starts_with(&src));
    assert!(report.verified && report.has_logo && report.appearance_bytes > 0);

    // The detached CMS still verifies over the byte range it named.
    let r = signatures(&out, &[], NOW, &budget(), &mut budget().guard()).expect("signature report");
    let sig = r
        .signatures
        .iter()
        .find(|s| s.field == "Signature1")
        .expect("signature present");
    assert!(sig.verified, "{}", sig.error.clone().unwrap_or_default());
    assert!(sig.covers_whole_document);

    let all = streams(&out);
    let form = all
        .iter()
        .find(|(dict, _)| subtype(dict) == Some(b"Form"))
        .expect("a /Subtype /Form appearance must be written");
    assert!(dict_has(&form.0, b"Resources"));
    assert!(dict_has(&form.0, b"BBox"));
    let appearance_text = String::from_utf8_lossy(&form.1);
    assert!(
        appearance_text.contains("Ada Lovelace"),
        "{appearance_text}"
    );
    assert!(
        appearance_text.contains("/SelisSigLogo Do"),
        "{appearance_text}"
    );
    let image = all
        .iter()
        .find(|(dict, _)| subtype(dict) == Some(b"Image"))
        .expect("the logo image XObject must be written");
    assert_eq!(image.1.len(), 12, "2x2 RGB logo pixels");
}

fn dict_has(dict: &Dict, key: &[u8]) -> bool {
    dict.iter().any(|(name, _)| name.as_slice() == key)
}

#[cfg(all(not(target_arch = "wasm32"), feature = "logo"))]
#[test]
fn a_decoded_png_logo_survives_detached_signing_and_ap_read_back() {
    let budget = budget();
    let mut g = budget.guard();
    let image = selis_image::Image {
        width: 2,
        height: 2,
        rgba: vec![
            255, 0, 0, 255, 0, 0, 255, 255, 0, 255, 0, 255, 255, 255, 0, 255,
        ],
    };
    let png = selis_image::encode_png(&image, 6, &mut g).expect("encode png");
    let logo = SignatureLogo::from_encoded(&png, &mut g).expect("decode png");
    let src = synthetic_pdf();
    let (out, report) = sign_digital(&src, &options(Some(logo)), &budget, &mut budget.guard())
        .expect("sign with a decoded logo");

    assert!(out.starts_with(&src));
    assert!(report.verified && report.has_logo);
    let image = streams(&out)
        .into_iter()
        .find(|(dict, _)| subtype(dict) == Some(b"Image"))
        .expect("logo XObject");
    // Alpha was dropped on decode, so the embedded bytes are exactly the raw
    // RGB logo the previous test supplies by hand.
    assert_eq!(image.1, test_logo().rgb);
}

#[test]
fn missing_logo_bytes_are_a_typed_refusal_before_any_write() {
    let src = synthetic_pdf();
    let bad = SignatureLogo::new(2, 2, Vec::new());
    let err = sign_digital(&src, &options(Some(bad)), &budget(), &mut budget().guard())
        .expect_err("no bytes, no signature");
    assert!(matches!(err, SignError::Invalid(_)), "{err}");
}

#[test]
fn render_parity_holds_outside_the_appearance_rect() {
    let src = synthetic_pdf();
    let (out, _) = sign_digital(
        &src,
        &options(Some(test_logo())),
        &budget(),
        &mut budget().guard(),
    )
    .expect("sign");
    let before = render(&src);
    let after = render(&out);
    assert_eq!((before.width, before.height), (after.width, after.height));
    let (width, height) = (before.width as usize, before.height as usize);
    assert_eq!(before.rgba.len(), width * height * 4);

    // PDF bottom-left origin → top-down pixel rows.
    // The frame is a 0.5pt stroke centred on the rectangle edge, so its
    // antialiased half-pixel lands just outside the exact rect. Mask two extra
    // pixels around it; everything beyond that must be untouched.
    let x0 = RECT[0] as usize - 2;
    let x1 = RECT[2] as usize + 2;
    let y0 = height - RECT[3] as usize - 2;
    let y1 = height - RECT[1] as usize + 2;
    let inside = |x: usize, y: usize| x >= x0 && x < x1 && y >= y0 && y < y1;

    let mut outside_diffs = 0usize;
    let mut inside_diffs = 0usize;
    for y in 0..height {
        for x in 0..width {
            let at = (y * width + x) * 4;
            let same = before.rgba[at..at + 4] == after.rgba[at..at + 4];
            if same {
                continue;
            }
            if inside(x, y) {
                inside_diffs += 1;
            } else {
                outside_diffs += 1;
            }
        }
    }
    assert_eq!(
        outside_diffs, 0,
        "page content outside the widget must be identical"
    );
    assert!(
        inside_diffs > 0,
        "the appearance must actually paint inside the widget"
    );
}

/// The WASM chunk's sign op uses this entry point: encoded logo bytes are
/// decoded inside the guest and the `/AP /N` is read back byte-for-byte before
/// the document is returned. Named evidence for the dedicated lazy chunk.
#[cfg(feature = "logo")]
#[test]
fn an_encoded_logo_signs_and_the_ap_reads_back_byte_for_byte() {
    let b = budget();
    let mut g = b.guard();
    let image = selis_image::Image {
        width: 2,
        height: 2,
        rgba: test_logo()
            .rgb
            .chunks_exact(3)
            .flat_map(|pixel| [pixel[0], pixel[1], pixel[2], 255])
            .collect(),
    };
    let png = selis_image::encode_png(&image, 6, &mut g).expect("encode png");
    let src = synthetic_pdf();
    let (out, report) =
        sign_digital_with_encoded_logo(&src, &options(None), &png, &b, &mut b.guard())
            .expect("sign with a decoded logo and verify the appearance");
    assert!(out.starts_with(&src));
    assert!(report.verified && report.has_logo);
    assert!(streams(&out)
        .iter()
        .any(|(dict, data)| subtype(dict) == Some(b"Image") && *data == test_logo().rgb));

    // The standalone read-back op finds the field by name and compares the
    // /AP /N stream to the bytes the writer reported.
    let appearance = streams(&out)
        .into_iter()
        .find(|(dict, _)| subtype(dict) == Some(b"Form"))
        .expect("appearance stream")
        .1;
    verify_signature_appearance(
        &out,
        "Signature1",
        &appearance,
        true,
        &budget(),
        &mut budget().guard(),
    )
    .expect("byte-for-byte /AP /N read-back");
    let mut changed = appearance.clone();
    changed[0] ^= 0xff;
    assert!(matches!(
        verify_signature_appearance(
            &out,
            "Signature1",
            &changed,
            true,
            &budget(),
            &mut budget().guard(),
        ),
        Err(SignError::ReadBack(_))
    ));
}

/// A logo that is not a PNG/JPEG this build understands is refused before a
/// single PDF byte is written — the browser chunk relies on the same guard.
#[cfg(feature = "logo")]
#[test]
fn an_undecodable_encoded_logo_is_refused_before_a_write() {
    let src = synthetic_pdf();
    let err = sign_digital_with_encoded_logo(
        &src,
        &options(None),
        b"not a PNG or JPEG",
        &budget(),
        &mut budget().guard(),
    )
    .expect_err("no logo, no signature");
    assert!(matches!(err, SignError::Invalid(_)), "{err}");
}
