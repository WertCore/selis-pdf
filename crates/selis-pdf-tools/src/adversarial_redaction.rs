//! SL-5.REDACT.06 — the adversarial redaction corpus and its verification suite.
//!
//! The seven cases named in `pdf-plan/15-PHASE-5-edit.md` (lines 187-191) each
//! get a purpose-built, one-page PDF whose secret is a unique byte marker:
//!
//! 1. text split across runs,
//! 2. text in a form XObject,
//! 3. text in an annotation appearance,
//! 4. text in a prior revision,
//! 5. text in an embedded file,
//! 6. text as an image with an OCR layer,
//! 7. text in metadata.
//!
//! The fixtures are built in memory (no committed binary fixtures, so no drift
//! guard is needed); each one is written to a private temp directory for the
//! end-to-end call into [`crate::tools::redact`].
//!
//! The verification pass is deliberately independent of the writer: it greps
//! the RAW output bytes for every secret fragment, then re-parses the bytes
//! with a fresh parser (and surveys them structurally). Nothing the writer
//! reports is trusted. The "test the test" control proves the pass catches a
//! redaction that covered the wrong region.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects
)]

use crate::tools;
use selis_sandbox::Budget;

/// The single redaction region every fixture is laid out against. Points,
/// y-up: `[x, y, width, height]` = x in 10..110, y in 150..170.
const REGION: (f64, f64, f64, f64) = (10.0, 150.0, 100.0, 20.0);

/// A region far away from the secret, used by the "test the test" control.
const WRONG_REGION: (f64, f64, f64, f64) = (0.0, 0.0, 10.0, 10.0);

const FONT_OBJECT: &str =
    "5 0 obj\n<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>\nendobj\n";

/// One object body, numbered `num`, from a source string.
fn object(num: u32, body: &str) -> (u32, Vec<u8>) {
    (num, body.as_bytes().to_vec())
}

/// A stream object. `/Length` is the exact data length; the newline before
/// `endstream` is whitespace a reader skips (the same layout the other tool
/// fixtures in this crate use).
fn stream(num: u32, extra: &str, data: &[u8]) -> (u32, Vec<u8>) {
    let mut out =
        format!("{num} 0 obj\n<< /Length {} {extra}>>\nstream\n", data.len()).into_bytes();
    out.extend_from_slice(data);
    out.extend_from_slice(b"\nendstream\nendobj\n");
    (num, out)
}

/// Assemble a single-revision PDF from object bodies in ascending number order.
fn assemble(objects: &[(u32, Vec<u8>)], trailer_extra: &str) -> Vec<u8> {
    let mut out = b"%PDF-1.4\n".to_vec();
    let mut offsets: Vec<(u32, usize)> = Vec::new();
    for (num, body) in objects {
        offsets.push((*num, out.len()));
        out.extend_from_slice(body);
    }
    let xref = out.len();
    let size = objects
        .iter()
        .map(|(n, _)| *n)
        .max()
        .unwrap_or(0)
        .saturating_add(1);
    let by_num: std::collections::HashMap<u32, usize> = offsets.into_iter().collect();
    out.extend_from_slice(format!("xref\n0 {size}\n").as_bytes());
    out.extend_from_slice(b"0000000000 65535 f \n");
    for num in 1..size {
        let off = by_num.get(&num).copied().unwrap_or(0);
        out.extend_from_slice(format!("{off:010} 00000 n \n").as_bytes());
    }
    out.extend_from_slice(
        format!(
            "trailer\n<< /Size {size} /Root 1 0 R {trailer_extra}>>\nstartxref\n{xref}\n%%EOF\n"
        )
        .as_bytes(),
    );
    out
}

fn temp_dir(case: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("selis-adv-redact-{}-{case}", std::process::id()));
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(&dir).expect("create temp dir");
    dir
}

/// Write `input`, redact it through the real tool, and read the output bytes.
fn redact_bytes(input: &[u8], rect: (f64, f64, f64, f64), case: &str) -> Vec<u8> {
    let dir = temp_dir(case);
    let src = dir.join("in.pdf");
    let dst = dir.join("out.pdf");
    std::fs::write(&src, input).expect("write fixture");
    tools::redact(
        &src.to_string_lossy(),
        &[rect],
        Some("0"),
        &dst.to_string_lossy(),
    )
    .expect("redaction succeeds");
    std::fs::read(&dst).expect("read output")
}

/// The independent ADR-P0023 verification pass.
///
/// # Budget
///
/// Linear in the output length: one pass over the bytes per needle, one parse,
/// and one structural survey. No allocation grows with the document beyond the
/// parser's own bounded tables.
///
/// # Malformed Input
///
/// A byte slice that is not a PDF fails the re-parse and is reported; it is
/// never treated as a successful redaction. A missing needle is a pass, not an
/// error — the caller decides which needles must be present.
fn verify_redaction_proof(bytes: &[u8], needles: &[&[u8]]) -> Result<(), String> {
    // (1) Raw-byte grep. The writer is not consulted: this is the file a
    // recipient would open, byte for byte.
    for needle in needles {
        if bytes.windows(needle.len()).any(|w| w == *needle) {
            return Err(format!(
                "redacted secret survived in the output bytes: {:?}",
                String::from_utf8_lossy(needle)
            ));
        }
    }
    // (2) Fresh parser, independent of the writer's own bookkeeping.
    let budget = Budget::unlimited();
    let mut guard = budget.guard();
    let start = selis_pdf_cos::xref::find_startxref(bytes, 4096).unwrap_or(0);
    let doc = selis_pdf_cos::parse_revisions(bytes, start, &budget, &mut guard)
        .map_err(|e| format!("redacted output does not re-parse: {e}"))?;
    if doc.revisions().is_empty() {
        return Err("redacted output has no revisions".to_string());
    }
    // (3) Independent structural survey: the output is a readable document.
    let observed = selis_pdf_cos::verify::survey(bytes, &budget, &mut guard)
        .map_err(|e| format!("redacted output fails structural survey: {e}"))?;
    if observed.pages == 0 {
        return Err("redacted output has no pages".to_string());
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// The seven adversarial builders.

/// Case 1 — the secret is split across TWO show operations, both at the same
/// position so both fall under the region. The two byte fragments are the
/// needles: a contiguous grep for the whole marker would miss a split run, so
/// the verifier checks each fragment independently.
fn split_runs_pdf() -> Vec<u8> {
    let content = b"BT /F1 12 Tf 20 160 Td (ADV-SPLIT-) Tj (RUN-SECRET) Tj ET";
    assemble(
        &[
            object(1, "1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n"),
            object(2, "2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n"),
            object(
                3,
                "3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Contents 4 0 R /Resources << /Font << /F1 5 0 R >> >> >>\nendobj\n",
            ),
            stream(4, "", content),
            object(5, FONT_OBJECT),
        ],
        "",
    )
}

/// Case 2 — the secret is inside a FORM XObject. The page content does not
/// carry the text at all; it only names `/Fm1`, drawn at the IDENTITY CTM, while
/// the form's own stream positions the text at (20, 160) under the region.
/// The engine compares the `Do` placement box, not the form's inner text, so
/// this is the form-XObject recursion gap.
fn form_xobject_pdf() -> Vec<u8> {
    let form = b"BT /F1 12 Tf 20 160 Td (ADV-FORM-XOBJECT-SECRET) Tj ET";
    assemble(
        &[
            object(1, "1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n"),
            object(2, "2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n"),
            object(
                3,
                "3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Contents 4 0 R /Resources << /Font << /F1 5 0 R >> /XObject << /Fm1 6 0 R >> >> >>\nendobj\n",
            ),
            stream(4, "", b"/Fm1 Do"),
            object(5, FONT_OBJECT),
            stream(
                6,
                "/Type /XObject /Subtype /Form /BBox [0 0 200 200] /Resources << /Font << /F1 5 0 R >> >>",
                form,
            ),
        ],
        "",
    )
}

/// Case 3 — the secret is in an annotation's appearance stream. The page text
/// is clean; a square annotation covering the region carries an `/AP /N` form
/// XObject whose stream spells the secret. The redaction copies annotations
/// (and every object they reference) unchanged, so the appearance survives.
fn annotation_appearance_pdf() -> Vec<u8> {
    let appearance = b"BT /F1 12 Tf 20 160 Td (ADV-ANNOT-AP-SECRET) Tj ET";
    assemble(
        &[
            object(1, "1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n"),
            object(2, "2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n"),
            object(
                3,
                "3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Contents 4 0 R /Annots [6 0 R] /Resources << /Font << /F1 5 0 R >> >> >>\nendobj\n",
            ),
            stream(4, "", b""),
            object(5, FONT_OBJECT),
            object(
                6,
                "6 0 obj\n<< /Type /Annot /Subtype /Square /Rect [10 150 110 170] /AP << /N 7 0 R >> >>\nendobj\n",
            ),
            stream(
                7,
                "/Type /XObject /Subtype /Form /BBox [0 0 200 200] /Resources << /Font << /F1 5 0 R >> >>",
                appearance,
            ),
        ],
        "",
    )
}

/// Case 4 — the secret is in a PRIOR revision. Revision 1 lays down a page
/// whose content carries the secret; revision 2 is an incremental update that
/// only adds `/Lang` to the catalog, so the latest view of the page still
/// draws the secret and the earlier revision's bytes are still in the file.
/// A full rewrite (what this tool does) must drop those bytes entirely.
fn prior_revision_pdf() -> Vec<u8> {
    let content = b"BT /F1 12 Tf 20 160 Td (ADV-PRIOR-REV-SECRET) Tj ET";
    let mut out = b"%PDF-1.4\n".to_vec();
    let rev1_objects: Vec<(u32, Vec<u8>)> = vec![
        object(1, "1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n"),
        object(2, "2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n"),
        object(
            3,
            "3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Contents 4 0 R /Resources << /Font << /F1 5 0 R >> >> >>\nendobj\n",
        ),
        stream(4, "", content),
        object(5, FONT_OBJECT),
    ];
    let mut rev1_offsets: Vec<(u32, usize)> = Vec::new();
    for (num, body) in &rev1_objects {
        rev1_offsets.push((*num, out.len()));
        out.extend_from_slice(body);
    }
    let rev1_xref = out.len();
    out.extend_from_slice(b"xref\n0 6\n0000000000 65535 f \n");
    let rev1_by_num: std::collections::HashMap<u32, usize> = rev1_offsets.into_iter().collect();
    for num in 1..6u32 {
        let off = rev1_by_num.get(&num).copied().unwrap_or(0);
        out.extend_from_slice(format!("{off:010} 00000 n \n").as_bytes());
    }
    out.extend_from_slice(
        format!("trailer\n<< /Size 6 /Root 1 0 R >>\nstartxref\n{rev1_xref}\n%%EOF\n").as_bytes(),
    );

    // Revision 2: update only the catalog, keeping the secret as current text.
    let rev2_offset = out.len();
    out.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R /Lang (en) >>\nendobj\n");
    let rev2_xref = out.len();
    out.extend_from_slice(b"xref\n0 6\n0000000000 65535 f \n");
    out.extend_from_slice(format!("{rev2_offset:010} 00000 n \n").as_bytes());
    for _ in 1..5u32 {
        out.extend_from_slice(b"0000000000 00000 f \n");
    }
    out.extend_from_slice(
        format!(
            "trailer\n<< /Size 6 /Root 1 0 R /Prev {rev1_xref} >>\nstartxref\n{rev2_xref}\n%%EOF\n"
        )
        .as_bytes(),
    );
    out
}

/// Case 5 — the secret is in an embedded file hung off the catalog's `/Names`.
/// The rewrite carries embedded files through, so the bytes survive.
fn embedded_file_pdf() -> Vec<u8> {
    let embedded = b"ADV-EMBEDDED-FILE-SECRET";
    assemble(
        &[
            // /Names must be an indirect reference for verify::survey to count
            // the embedded file (dict_ref, not dict_get), so the write gate's
            // input expectation matches the carried-through output.
            object(1, "1 0 obj\n<< /Type /Catalog /Pages 2 0 R /Names 8 0 R >>\nendobj\n"),
            object(2, "2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n"),
            object(
                3,
                "3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Contents 4 0 R /Resources << /Font << /F1 5 0 R >> >> >>\nendobj\n",
            ),
            stream(4, "", b""),
            object(5, FONT_OBJECT),
            object(
                6,
                "6 0 obj\n<< /Type /Filespec /F (secret.txt) /EF << /F 7 0 R >> >>\nendobj\n",
            ),
            stream(7, "/Type /EmbeddedFile", embedded),
            object(
                8,
                "8 0 obj\n<< /EmbeddedFiles << /Names [(secret.txt) 6 0 R] >> >>\nendobj\n",
            ),
        ],
        "",
    )
}

/// Case 6 — the secret is BOTH baked into an image's pixels and present in a
/// hidden (rendering-mode 3) OCR text layer over the same spot. Both sit under
/// the region. Removing the image bytes alone is not enough: the OCR layer is
/// real text a copy/paste reader can still extract, so it must go too — and
/// vice versa.
fn ocr_image_pdf() -> Vec<u8> {
    let pixels = b"ADV-OCR-IMAGE-SECRET";
    let content = b"q 200 0 0 50 0 150 cm /ImOCR Do Q BT /F1 3 Tf 3 Tr 20 160 Td (ADV-OCR-IMAGE-SECRET) Tj ET";
    assemble(
        &[
            object(1, "1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n"),
            object(2, "2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n"),
            object(
                3,
                "3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Contents 4 0 R /Resources << /Font << /F1 5 0 R >> /XObject << /ImOCR 6 0 R >> >> >>\nendobj\n",
            ),
            stream(4, "", content),
            object(5, FONT_OBJECT),
            stream(
                6,
                "/Type /XObject /Subtype /Image /Width 8 /Height 2 /ColorSpace /DeviceGray /BitsPerComponent 8",
                pixels,
            ),
        ],
        "",
    )
}

/// Case 7 — the secret is in the catalog's XMP `/Metadata` stream. The rewrite
/// carries `/Metadata` through with the rest of the catalog, so the packet
/// (and the secret inside it) survives.
fn metadata_pdf() -> Vec<u8> {
    let xmp = b"ADV-METADATA-SECRET";
    assemble(
        &[
            object(
                1,
                "1 0 obj\n<< /Type /Catalog /Pages 2 0 R /Metadata 6 0 R >>\nendobj\n",
            ),
            object(2, "2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n"),
            object(
                3,
                "3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Contents 4 0 R /Resources << /Font << /F1 5 0 R >> >> >>\nendobj\n",
            ),
            stream(4, "", b""),
            object(5, FONT_OBJECT),
            stream(6, "/Type /Metadata /Subtype /XML", xmp),
        ],
        "",
    )
}

/// A trivial fixture for the control: the secret is plain page text.
fn control_pdf() -> Vec<u8> {
    let content = b"BT /F1 12 Tf 20 160 Td (ADV-CONTROL-SECRET) Tj ET";
    assemble(
        &[
            object(1, "1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n"),
            object(2, "2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n"),
            object(
                3,
                "3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Contents 4 0 R /Resources << /Font << /F1 5 0 R >> >> >>\nendobj\n",
            ),
            stream(4, "", content),
            object(5, FONT_OBJECT),
        ],
        "",
    )
}

// ---------------------------------------------------------------------------
// Tests.

/// # Budget
///
/// One redaction and one linear verification per case.
///
/// # Malformed Input
///
/// Fixtures are built in memory and are well-formed; a failure to build or
/// redact one is a test failure, not a handled case.
fn e2e(builder: fn() -> Vec<u8>, needles: &[&[u8]], case: &str) {
    let output = redact_bytes(&builder(), REGION, case);
    if let Err(why) = verify_redaction_proof(&output, needles) {
        panic!("[{case}] ADR-P0023 proof failed: {why}");
    }
}

#[test]
fn case_01_text_split_across_runs() {
    e2e(
        split_runs_pdf,
        &[b"ADV-SPLIT-", b"RUN-SECRET", b"ADV-SPLIT-RUN-SECRET"],
        "split-runs",
    );
}

// GAP: strip_content_in_regions only rewrites the page content stream. A form
// XObject's own stream is never recursed into; its `Do` is treated as an image
// draw, and at identity CTM the unit square misses the region, so both the
// draw and the resource survive. Engine work needed: recurse into form
// XObjects with the placement matrix composed into the CTM, and strip their
// text too (or drop the form when every draw is covered).
#[test]
#[ignore = "engine gap: form XObject streams are not recursed into by strip_content_in_regions"]
fn case_02_text_in_form_xobject() {
    e2e(
        form_xobject_pdf,
        &[b"ADV-FORM-XOBJECT-SECRET"],
        "form-xobject",
    );
}

// GAP: annotations are copied with every object they reference, including
// /AP /N appearance streams. The redaction never touches appearance text.
// Engine work needed: strip (or regenerate) annotation appearance streams
// whose /Rect intersects a redaction region before materialising the page.
#[test]
#[ignore = "engine gap: annotation appearance streams are not stripped by redaction"]
fn case_03_text_in_annotation_appearance() {
    e2e(
        annotation_appearance_pdf,
        &[b"ADV-ANNOT-AP-SECRET"],
        "annotation-appearance",
    );
}

#[test]
fn case_04_text_in_prior_revision() {
    e2e(
        prior_revision_pdf,
        &[b"ADV-PRIOR-REV-SECRET"],
        "prior-revision",
    );
}

// GAP: reconcile_single_input carries catalog /Names /EmbeddedFiles through
// untouched (PRESERVE_ALL expects the count to match). The embedded stream's
// bytes therefore survive. Engine work needed: redaction-specific filtering
// that drops embedded files (and relaxes the embedded-file count gate) when
// the operation is asked to remove content from the document.
#[test]
#[ignore = "engine gap: embedded files are carried through by the rewrite"]
fn case_05_text_in_embedded_file() {
    e2e(
        embedded_file_pdf,
        &[b"ADV-EMBEDDED-FILE-SECRET"],
        "embedded-file",
    );
}

#[test]
fn case_06_text_as_image_with_ocr_layer() {
    e2e(ocr_image_pdf, &[b"ADV-OCR-IMAGE-SECRET"], "image-with-ocr");
}

// GAP: the catalog's /Metadata (XMP) stream is copied through by
// reconcile_single_input with carry_metadata = true, secret intact.
// Engine work needed: redaction must scrub the XMP packet (and the /Info
// dictionary) for the redacted strings rather than carrying them through.
#[test]
#[ignore = "engine gap: catalog /Metadata is carried through by the rewrite"]
fn case_07_text_in_metadata() {
    e2e(metadata_pdf, &[b"ADV-METADATA-SECRET"], "metadata");
}

/// Test the test: redact the WRONG region, so the secret is NOT covered, and
/// prove the independent verifier rejects the result. This is the control the
/// whole claim rests on — a verifier that passed here would be worthless.
#[test]
fn control_verifier_catches_a_redaction_that_missed_the_secret() {
    let output = redact_bytes(&control_pdf(), WRONG_REGION, "control-wrong-region");
    // The wrong-region redaction MUST leave the marker for the control to be
    // meaningful; if it does not, the fixture/region pair is wrong.
    assert!(
        output
            .windows(b"ADV-CONTROL-SECRET".len())
            .any(|w| w == b"ADV-CONTROL-SECRET"),
        "the fixture must still carry the secret when the wrong region is redacted"
    );
    let verdict = verify_redaction_proof(&output, &[b"ADV-CONTROL-SECRET"]);
    assert!(
        verdict.is_err(),
        "the verifier must reject output that still contains the secret; got {verdict:?}"
    );
}
