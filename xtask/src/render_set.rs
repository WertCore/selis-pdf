//! Deterministic render benchmark set (SL-2.PERF.02).
//!
//! Six single-page PDFs under `bench/render-set/`, one per render cost axis:
//! text-heavy, vector-heavy, large images, shadings, transparency, and a
//! mixed page. Every file is generated from fixed seeds (no RNG, no clock,
//! no input files), so regeneration is byte-identical and the set doubles as
//! the §12 "Render A4 text page" harness input.
//!
//! The text page is A4 (595 x 842 pt) with a realistic dense body (~4 600
//! glyphs in ~190 runs over the standard-14 Helvetica fallback); the other
//! five pages isolate their axis so the profile report can attribute stage
//! costs honestly. `xtask render-set --check` regenerates every file in
//! memory and byte-compares against the committed fixtures, so CI fails
//! loudly if a fixture drifts from its generator.

use std::path::Path;

use selis_pdf_cos::doc_writer::DocumentBuilder;
use selis_pdf_cos::{Obj, Ref};

/// (file stem, cost axis, what it stresses) in generation order.
///
/// The set lives in `bench/render-set/` (repo root CWD); see the `render-set`
/// subcommand's `--outdir` default.
pub const PAGES: &[(&str, &str, &str)] = &[
    (
        "text-heavy",
        "text",
        "glyph outline extraction + per-glyph fill over the std-14 fallback",
    ),
    (
        "vector-heavy",
        "vector",
        "path conversion + tiny-skia fill/stroke rasterisation",
    ),
    (
        "large-image",
        "image",
        "FlateDecode + full-page RGB(A) draw with downscale",
    ),
    (
        "shading",
        "shading",
        "axial/radial function-shading rasterisation per pixel",
    ),
    (
        "transparency",
        "transparency",
        "nested blend groups (push/pop layer + composite)",
    ),
    (
        "mixed",
        "mixed",
        "text + vector + image + clip + one blend group",
    ),
];

/// A4 width/height in points (the §12 text page is A4).
const PAGE_W: f64 = 595.0;
const PAGE_H: f64 = 842.0;

fn bytes(s: &[u8]) -> selis_bytes::Bytes {
    selis_bytes::Bytes::copy_from_slice(s)
}

fn int(n: i64) -> Obj {
    Obj::Int(n)
}

fn name(s: &str) -> Obj {
    Obj::Name(bytes(s.as_bytes()))
}

/// A decimal real with 4 fractional digits (scale-4 fixed point).
fn real(v: f64) -> Obj {
    let scaled = (v * 10_000.0).round().clamp(-9e14, 9e14) as i64;
    Obj::Real { scaled, scale: 4 }
}

/// Allocate a stream object with a dict and return its ref.
fn add_stream(
    doc: &mut DocumentBuilder,
    dict: Vec<(selis_bytes::Bytes, Obj)>,
    data: Vec<u8>,
) -> Ref {
    let num = doc.allocate();
    doc.add_object(
        num,
        Obj::Stream {
            dict,
            data: bytes(&data),
        },
    );
    Ref::new(num, 0)
}

/// Allocate a bare dict object and return its ref.
fn add_dict(doc: &mut DocumentBuilder, pairs: Vec<(selis_bytes::Bytes, Obj)>) -> Ref {
    let num = doc.allocate();
    let mut dict_pairs: Vec<(selis_bytes::Bytes, Obj)> = Vec::new();
    for (k, v) in pairs {
        dict_pairs.push((k, v));
    }
    doc.add_object(num, Obj::Dict(dict_pairs));
    Ref::new(num, 0)
}

/// A `/Length`-carrying content stream object.
fn add_content(doc: &mut DocumentBuilder, content: &[u8]) -> Ref {
    add_stream(
        doc,
        vec![(bytes(b"Length"), int(content.len() as i64))],
        content.to_vec(),
    )
}

/// Deterministic word salad: cycles a fixed word list by index arithmetic.
fn words(seed: usize, count: usize) -> String {
    const WORDS: &[&str] = &[
        "the", "quick", "brown", "fox", "jumps", "over", "lazy", "dog", "pack", "with", "five",
        "dozen", "liquor", "jugs", "how", "vexingly", "daft", "zebras", "jump", "bright", "vixens",
        "sphinx", "quartz", "glyph", "report", "section", "table", "figure", "index", "appendix",
        "draft",
    ];
    let mut out = String::new();
    for i in 0..count {
        if i > 0 {
            out.push(' ');
        }
        let w = WORDS.get((seed + i * 7) % WORDS.len()).unwrap_or(&"text");
        out.push_str(w);
    }
    out
}

/// Split a line into `runs` Tj chunks (ASCII only, no escaping needed).
fn tj_runs(line: &str, runs: usize) -> String {
    let chars: Vec<char> = line.chars().collect();
    let mut out = String::new();
    let per = chars.len().div_ceil(runs).max(1);
    for chunk in chars.chunks(per) {
        let s: String = chunk.iter().collect();
        out.push('(');
        out.push_str(&s);
        out.push_str(") Tj ");
    }
    out
}

/// text-heavy: 64 lines x 3 runs of Helvetica 9pt (~4 600 glyphs).
fn build_text() -> Result<DocumentBuilder, String> {
    let mut doc = DocumentBuilder::new();
    let font = add_dict(
        &mut doc,
        vec![
            (bytes(b"Type"), name("Font")),
            (bytes(b"Subtype"), name("Type1")),
            (bytes(b"BaseFont"), name("Helvetica")),
        ],
    );
    let resources = add_dict(
        &mut doc,
        vec![(
            bytes(b"Font"),
            Obj::Dict(vec![(bytes(b"F1"), Obj::Ref(font))]),
        )],
    );
    let mut content = Vec::new();
    for line in 0..64 {
        let y = 800.0 - line as f64 * 11.5;
        let text = words(line * 3 + 1, 12);
        let runs = tj_runs(&text, 3);
        let op = format!("BT /F1 9 Tf 1 0 0 1 50 {y:.1} Tm {runs}ET\n");
        content.extend_from_slice(op.as_bytes());
    }
    let content_ref = add_content(&mut doc, &content);
    doc.add_page_with(PAGE_W, PAGE_H, &[content_ref], Some(resources));
    Ok(doc)
}

/// vector-heavy: 900 filled rects + 250 stroked cubics + 250 stroked lines.
fn build_vector() -> Result<DocumentBuilder, String> {
    let mut doc = DocumentBuilder::new();
    let mut content = Vec::new();
    // Filled rect grid (30 x 30), deterministic colour rotation.
    for i in 0..900usize {
        let col = i % 30;
        let row = i / 30;
        let x = 20.0 + col as f64 * 18.5;
        let y = 20.0 + row as f64 * 26.5;
        let r = ((i * 37) % 100) as f64 / 100.0;
        let g = ((i * 53) % 100) as f64 / 100.0;
        let b = ((i * 79) % 100) as f64 / 100.0;
        let op = format!("{r:.2} {g:.2} {b:.2} rg {x:.1} {y:.1} 14 14 re f\n");
        content.extend_from_slice(op.as_bytes());
    }
    // Stroked cubic curves.
    for i in 0..250usize {
        let x0 = 30.0 + ((i * 53) % 500) as f64;
        let y0 = 30.0 + ((i * 91) % 740) as f64;
        let w = 0.5 + (i % 3) as f64 * 0.75;
        let gray = ((i * 29) % 100) as f64 / 100.0;
        let op = format!(
            "{gray:.2} G {w:.2} w {x0:.1} {y0:.1} m {x1:.1} {y1:.1} {x2:.1} {y2:.1} {x3:.1} {y3:.1} c S\n",
            x1 = x0 + 40.0,
            y1 = y0 + 10.0,
            x2 = x0 + 10.0,
            y2 = y0 + 40.0,
            x3 = x0 + 50.0,
            y3 = y0 + 50.0,
        );
        content.extend_from_slice(op.as_bytes());
    }
    // Stroked diagonals.
    for i in 0..250usize {
        let x0 = 25.0 + ((i * 71) % 520) as f64;
        let y0 = 25.0 + ((i * 37) % 750) as f64;
        let op = format!(
            "0.2 0.4 0.8 RG 1 w {x0:.1} {y0:.1} m {x1:.1} {y1:.1} l S\n",
            x1 = x0 + 30.0,
            y1 = y0 + 30.0,
        );
        content.extend_from_slice(op.as_bytes());
    }
    let content_ref = add_content(&mut doc, &content);
    doc.add_page_with(PAGE_W, PAGE_H, &[content_ref], None);
    Ok(doc)
}

/// A deterministic RGB gradient with a sparse white grid (flate-compressed).
fn gradient_rgb(width: u32, height: u32) -> Vec<u8> {
    let mut rgb = Vec::new();
    for y in 0..height {
        for x in 0..width {
            let grid = x % 80 < 2 || y % 80 < 2;
            let (r, g, b) = if grid {
                (255u8, 255u8, 255u8)
            } else {
                (
                    (x * 255 / width.max(1)) as u8,
                    (y * 255 / height.max(1)) as u8,
                    ((x + y) * 255 / width.saturating_add(height).max(1)) as u8,
                )
            };
            rgb.push(r);
            rgb.push(g);
            rgb.push(b);
        }
    }
    rgb
}

fn flate(data: &[u8]) -> Vec<u8> {
    miniz_oxide::deflate::compress_to_vec(data, 6)
}

/// Embed a flate-compressed RGB image XObject; returns its ref.
fn add_image(doc: &mut DocumentBuilder, width: u32, height: u32, rgb: &[u8]) -> Ref {
    let data = flate(rgb);
    add_stream(
        doc,
        vec![
            (bytes(b"Type"), name("XObject")),
            (bytes(b"Subtype"), name("Image")),
            (bytes(b"Width"), int(width as i64)),
            (bytes(b"Height"), int(height as i64)),
            (bytes(b"ColorSpace"), name("DeviceRGB")),
            (bytes(b"BitsPerComponent"), int(8)),
            (bytes(b"Filter"), name("FlateDecode")),
            (bytes(b"Length"), int(data.len() as i64)),
        ],
        data,
    )
}

/// large-image: 960 x 1200 RGB gradient drawn full-page.
fn build_image() -> Result<DocumentBuilder, String> {
    let mut doc = DocumentBuilder::new();
    let rgb = gradient_rgb(960, 1200);
    let img = add_image(&mut doc, 960, 1200, &rgb);
    let resources = add_dict(
        &mut doc,
        vec![(
            bytes(b"XObject"),
            Obj::Dict(vec![(bytes(b"Im1"), Obj::Ref(img))]),
        )],
    );
    let mut content = Vec::new();
    content.extend_from_slice(b"q\n595 0 0 842 0 0 cm\n/Im1 Do\nQ\n");
    let content_ref = add_content(&mut doc, &content);
    doc.add_page_with(PAGE_W, PAGE_H, &[content_ref], Some(resources));
    Ok(doc)
}

/// An inline type-2 (exponential) function dict: C0 -> C1.
fn type2_function(c0: [f64; 3], c1: [f64; 3]) -> Obj {
    Obj::Dict(vec![
        (bytes(b"FunctionType"), int(2)),
        (bytes(b"Domain"), Obj::Array(vec![real(0.0), real(1.0)])),
        (
            bytes(b"C0"),
            Obj::Array(vec![real(c0[0]), real(c0[1]), real(c0[2])]),
        ),
        (
            bytes(b"C1"),
            Obj::Array(vec![real(c1[0]), real(c1[1]), real(c1[2])]),
        ),
        (bytes(b"N"), int(1)),
    ])
}

/// shading: one axial + one radial DeviceRGB shading, full-page.
fn build_shading() -> Result<DocumentBuilder, String> {
    let mut doc = DocumentBuilder::new();
    let axial = add_dict(
        &mut doc,
        vec![
            (bytes(b"ShadingType"), int(2)),
            (bytes(b"ColorSpace"), name("DeviceRGB")),
            (
                bytes(b"Coords"),
                Obj::Array(vec![real(0.0), real(0.0), real(595.0), real(842.0)]),
            ),
            (bytes(b"Domain"), Obj::Array(vec![real(0.0), real(1.0)])),
            (
                bytes(b"Extend"),
                Obj::Array(vec![Obj::Bool(true), Obj::Bool(true)]),
            ),
            (
                bytes(b"Function"),
                type2_function([0.9, 0.2, 0.2], [0.1, 0.2, 0.9]),
            ),
        ],
    );
    let radial = add_dict(
        &mut doc,
        vec![
            (bytes(b"ShadingType"), int(3)),
            (bytes(b"ColorSpace"), name("DeviceRGB")),
            (
                bytes(b"Coords"),
                Obj::Array(vec![
                    real(297.5),
                    real(421.0),
                    real(30.0),
                    real(297.5),
                    real(421.0),
                    real(400.0),
                ]),
            ),
            (bytes(b"Domain"), Obj::Array(vec![real(0.0), real(1.0)])),
            (
                bytes(b"Extend"),
                Obj::Array(vec![Obj::Bool(true), Obj::Bool(true)]),
            ),
            (
                bytes(b"Function"),
                type2_function([1.0, 1.0, 1.0], [0.2, 0.7, 0.3]),
            ),
        ],
    );
    let resources = add_dict(
        &mut doc,
        vec![(
            bytes(b"Shading"),
            Obj::Dict(vec![
                (bytes(b"Sh1"), Obj::Ref(axial)),
                (bytes(b"Sh2"), Obj::Ref(radial)),
            ]),
        )],
    );
    let mut content = Vec::new();
    content.extend_from_slice(b"/Sh1 sh\n/Sh2 sh\n");
    // Framing rects so the page is not *only* shading.
    content.extend_from_slice(b"0 0 0 RG 2 w 10 10 575 822 re S\n");
    let content_ref = add_content(&mut doc, &content);
    doc.add_page_with(PAGE_W, PAGE_H, &[content_ref], Some(resources));
    Ok(doc)
}

/// transparency: nested Multiply/Screen BDC groups with alpha fills.
fn build_transparency() -> Result<DocumentBuilder, String> {
    let mut doc = DocumentBuilder::new();
    let gs_multiply = add_dict(
        &mut doc,
        vec![
            (bytes(b"BM"), name("Multiply")),
            (bytes(b"CA"), real(0.6)),
            (bytes(b"ca"), real(0.6)),
        ],
    );
    let gs_screen = add_dict(
        &mut doc,
        vec![
            (bytes(b"BM"), name("Screen")),
            (bytes(b"CA"), real(0.8)),
            (bytes(b"ca"), real(0.8)),
        ],
    );
    let resources = add_dict(
        &mut doc,
        vec![(
            bytes(b"ExtGState"),
            Obj::Dict(vec![
                (bytes(b"GS1"), Obj::Ref(gs_multiply)),
                (bytes(b"GS2"), Obj::Ref(gs_screen)),
            ]),
        )],
    );
    let mut content = Vec::new();
    // Opaque backdrop bands.
    for (i, colour) in [(0.9, 0.9, 0.9), (0.8, 0.85, 0.9), (0.9, 0.85, 0.8)]
        .iter()
        .enumerate()
    {
        let y = 40.0 + i as f64 * 260.0;
        let op = format!(
            "{:.2} {:.2} {:.2} rg 40 {y:.1} 515 220 re f\n",
            colour.0, colour.1, colour.2
        );
        content.extend_from_slice(op.as_bytes());
    }
    // Outer Multiply group with 40 overlapping fills.
    content.extend_from_slice(b"/GS1 BDC\n");
    for i in 0..40usize {
        let x = 50.0 + ((i * 47) % 400) as f64;
        let y = 60.0 + ((i * 89) % 660) as f64;
        let r = ((i * 37) % 100) as f64 / 100.0;
        let g = ((i * 53) % 100) as f64 / 100.0;
        let b = ((i * 79) % 100) as f64 / 100.0;
        let op = format!("{r:.2} {g:.2} {b:.2} rg {x:.1} {y:.1} 120 90 re f\n");
        content.extend_from_slice(op.as_bytes());
    }
    // Nested Screen group with 20 fills.
    content.extend_from_slice(b"/GS2 BDC\n");
    for i in 0..20usize {
        let x = 120.0 + ((i * 61) % 320) as f64;
        let y = 120.0 + ((i * 43) % 560) as f64;
        let v = 0.2 + ((i * 13) % 70) as f64 / 100.0;
        let op = format!("{v:.2} {v:.2} 0.9 rg {x:.1} {y:.1} 90 70 re f\n");
        content.extend_from_slice(op.as_bytes());
    }
    content.extend_from_slice(b"EMC\nEMC\n");
    // Constant-state alpha fills (no group): exercises `gs`.
    content.extend_from_slice(b"/GS1 gs\n");
    for i in 0..20usize {
        let x = 60.0 + ((i * 97) % 430) as f64;
        let y = 80.0 + ((i * 31) % 640) as f64;
        let op = format!("0.1 0.5 0.3 rg {x:.1} {y:.1} 60 60 re f\n");
        content.extend_from_slice(op.as_bytes());
    }
    let content_ref = add_content(&mut doc, &content);
    doc.add_page_with(PAGE_W, PAGE_H, &[content_ref], Some(resources));
    Ok(doc)
}

/// mixed: text + vectors + a small image + a clip + one blend group.
fn build_mixed() -> Result<DocumentBuilder, String> {
    let mut doc = DocumentBuilder::new();
    let font = add_dict(
        &mut doc,
        vec![
            (bytes(b"Type"), name("Font")),
            (bytes(b"Subtype"), name("Type1")),
            (bytes(b"BaseFont"), name("Helvetica")),
        ],
    );
    let rgb = gradient_rgb(480, 360);
    let img = add_image(&mut doc, 480, 360, &rgb);
    let gs = add_dict(
        &mut doc,
        vec![(bytes(b"BM"), name("Multiply")), (bytes(b"CA"), real(0.7))],
    );
    let resources = add_dict(
        &mut doc,
        vec![
            (
                bytes(b"Font"),
                Obj::Dict(vec![(bytes(b"F1"), Obj::Ref(font))]),
            ),
            (
                bytes(b"XObject"),
                Obj::Dict(vec![(bytes(b"Im1"), Obj::Ref(img))]),
            ),
            (
                bytes(b"ExtGState"),
                Obj::Dict(vec![(bytes(b"GS1"), Obj::Ref(gs))]),
            ),
        ],
    );
    let mut content = Vec::new();
    // Text block.
    for line in 0..24 {
        let y = 780.0 - line as f64 * 13.0;
        let text = words(line * 5 + 2, 10);
        let runs = tj_runs(&text, 2);
        let op = format!("BT /F1 10 Tf 1 0 0 1 50 {y:.1} Tm {runs}ET\n");
        content.extend_from_slice(op.as_bytes());
    }
    // Vector band.
    for i in 0..200usize {
        let x = 40.0 + ((i * 41) % 500) as f64;
        let y = 380.0 + ((i * 67) % 300) as f64;
        let r = ((i * 37) % 100) as f64 / 100.0;
        let op = format!("{r:.2} 0.3 0.6 rg {x:.1} {y:.1} 16 12 re f\n");
        content.extend_from_slice(op.as_bytes());
    }
    // The image, drawn 4x at different scales.
    for (x, y, w, h) in [
        (40.0, 40.0, 120.0, 90.0),
        (180.0, 40.0, 240.0, 180.0),
        (40.0, 150.0, 180.0, 135.0),
        (250.0, 240.0, 300.0, 120.0),
    ] {
        let op = format!("q {w:.1} 0 0 {h:.1} {x:.1} {y:.1} cm /Im1 Do Q\n");
        content.extend_from_slice(op.as_bytes());
    }
    // A clip region with fills inside it.
    content.extend_from_slice(b"q 100 250 200 120 re W n\n");
    for i in 0..30usize {
        let x = 80.0 + ((i * 29) % 220) as f64;
        let y = 230.0 + ((i * 53) % 140) as f64;
        let op = format!("0.8 0.2 0.2 rg {x:.1} {y:.1} 40 30 re f\n");
        content.extend_from_slice(op.as_bytes());
    }
    content.extend_from_slice(b"Q\n");
    // One blend group.
    content.extend_from_slice(b"/GS1 BDC\n");
    for i in 0..20usize {
        let x = 300.0 + ((i * 59) % 200) as f64;
        let y = 400.0 + ((i * 83) % 300) as f64;
        let b = i as f64 / 19.0;
        let op = format!("0.2 0.6 {b:.2} rg {x:.1} {y:.1} 70 50 re f\n");
        content.extend_from_slice(op.as_bytes());
    }
    content.extend_from_slice(b"EMC\n");
    let content_ref = add_content(&mut doc, &content);
    doc.add_page_with(PAGE_W, PAGE_H, &[content_ref], Some(resources));
    Ok(doc)
}

/// Build every page of the set in memory (generation order = [`PAGES`]).
fn build_all() -> Result<Vec<(&'static str, Vec<u8>)>, String> {
    // Materialise each PDF fully before writing anything, so a mid-run
    // failure cannot leave a half-regenerated set on disk.
    let mut docs: Vec<(&'static str, DocumentBuilder)> = Vec::new();
    docs.push(("text-heavy", build_text()?));
    docs.push(("vector-heavy", build_vector()?));
    docs.push(("large-image", build_image()?));
    docs.push(("shading", build_shading()?));
    docs.push(("transparency", build_transparency()?));
    docs.push(("mixed", build_mixed()?));
    let budget = selis_sandbox::Budget::unlimited();
    let clock = selis_sandbox::shell_clock();
    let mut out = Vec::new();
    for (stem, mut doc) in docs {
        let mut g = budget.guard_with(&clock, selis_sandbox::CancelToken::new());
        let pdf = doc
            .write(&budget, &mut g)
            .map_err(|e| format!("{stem}: {e}"))?;
        out.push((stem, pdf));
    }
    Ok(out)
}

/// Generate the set into `dir` (default [`SET_DIR`).
pub fn generate(dir: &Path) -> Result<(), String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let pdfs = build_all()?;
    for (stem, pdf) in &pdfs {
        let dest = dir.join(format!("{stem}.pdf"));
        std::fs::write(&dest, pdf).map_err(|e| format!("{}: {e}", dest.display()))?;
        println!("render-set: {stem}.pdf ({} bytes)", pdf.len());
    }
    println!("render-set: {} files in {}", pdfs.len(), dir.display());
    Ok(())
}

/// Regenerate in memory and byte-compare against the committed fixtures.
pub fn check(dir: &Path) -> Result<(), String> {
    let pdfs = build_all()?;
    if pdfs.len() != PAGES.len() {
        return Err(format!(
            "set has {} pages, expected {}",
            pdfs.len(),
            PAGES.len()
        ));
    }
    for (stem, pdf) in &pdfs {
        let dest = dir.join(format!("{stem}.pdf"));
        let on_disk = std::fs::read(&dest).map_err(|e| format!("{}: {e}", dest.display()))?;
        if &on_disk != pdf {
            return Err(format!(
                "{stem}.pdf drifts from its generator ({} on disk, {} generated); \
                 rerun `xtask render-set` and commit the result",
                on_disk.len(),
                pdf.len()
            ));
        }
    }
    println!("render-set: {} fixtures match their generator", pdfs.len());
    Ok(())
}

#[cfg(test)]
mod tests {
    #![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]
    use super::*;

    #[test]
    fn pages_table_matches_builders() {
        assert_eq!(PAGES.len(), 6);
        let pdfs = build_all().expect("build");
        for (stem, _, _) in PAGES {
            assert!(pdfs.iter().any(|(s, _)| s == stem), "missing {stem}");
        }
    }

    #[test]
    fn every_page_opens_in_the_engine() {
        let pdfs = build_all().expect("build");
        let budget = selis_sandbox::Budget::unlimited();
        let clock = selis_sandbox::FixedClock(0);
        for (stem, pdf) in &pdfs {
            let session = selis_pdf_engine::Session::open(pdf.clone(), &budget, &clock);
            assert!(session.is_ok(), "{stem} must open");
            assert_eq!(session.expect("open").len(), 1, "{stem} is single-page");
        }
    }

    #[test]
    fn tj_runs_splits_evenly() {
        let runs = tj_runs("abcdef", 3);
        assert_eq!(runs, "(ab) Tj (cd) Tj (ef) Tj ");
    }
}
