//! Synthetic corpus generator + mutator (SL-0.CORP.04).
//! Produces PDFs exercising specific constructs with known-correct
//! expectations, plus a seeded, reproducible mutator that damages valid files.
//!
//! The `indic` subset (SL-3.SHAPE.04) shapes Devanagari/Tamil/Bengali lines
//! with the product shaper (`selis-shape`, HarfBuzz-verified in
//! `crates/selis-shape/tests/indic.rs`) and embeds them as Type0/Identity-H
//! composite fonts: the only faithful way to carry complex-script text in a
//! PDF, and the path the engine's CID replay must handle.

use std::collections::BTreeMap;
use std::path::Path;

use selis_bytes::Bytes;
use selis_pdf_cos::doc_writer::{ContentBuilder, DocumentBuilder};
use selis_pdf_cos::{Obj, Ref};
use selis_shape::{Shaper, ShapingParams, SwashShaper};

/// Regenerate the expectation record for `path`, preserving any `[annotation]`
/// table from the previous record so a rerun of this generator cannot silently
/// wipe the recorded triage verdicts (SL-0.ORACLE.05).
fn preserve_annotation(path: std::path::PathBuf, record: String) -> String {
    let mut record = record;
    if let Ok(old) = std::fs::read_to_string(&path) {
        if let Some(annotation) = extract_annotation_table(&old) {
            if !record.contains("[annotation]") {
                record.push('\n');
                record.push_str(&annotation);
            }
        }
    }
    record
}

/// The `[annotation]` table of an expectation record, if any (from the table
/// header to the next table header or end of file). Module-level so tests
/// can exercise the preservation contract directly.
fn extract_annotation_table(text: &str) -> Option<String> {
    let mut lines = text.lines().peekable();
    let mut block = String::new();
    let mut inside = false;
    while let Some(line) = lines.next() {
        if line.trim() == "[annotation]" {
            inside = true;
            block.push_str(line);
            block.push('\n');
            continue;
        }
        if inside {
            if line.starts_with('[') {
                break;
            }
            block.push_str(line);
            block.push('\n');
        }
    }
    if inside {
        Some(block)
    } else {
        None
    }
}

pub fn generate() -> Result<(), String> {
    let dir = Path::new("corpus/pdfs/synthetic");
    std::fs::create_dir_all(dir).map_err(|e| format!("{dir:?}: {e}"))?;
    let exp_dir = Path::new("corpus/expect/synthetic");
    std::fs::create_dir_all(exp_dir).map_err(|e| format!("{exp_dir:?}: {e}"))?;

    // Clear stale outputs so a rerun is reproducible.
    for e in std::fs::read_dir(dir).map_err(|e| format!("{dir:?}: {e}"))? {
        let p = e.map_err(|e| format!("{e}"))?.path();
        let _ = std::fs::remove_file(p);
    }

    let count = std::cell::Cell::new(0usize);
    let gen = |id: &str, mut builder: DocumentBuilder, expect_pages: usize| -> Result<(), String> {
        let budget = selis_sandbox::Budget::unlimited();
        let clock = selis_sandbox::shell_clock();
        let mut g = budget.guard_with(&clock, selis_sandbox::CancelToken::new());
        let bytes = builder
            .write(&budget, &mut g)
            .map_err(|e| format!("{id}: {e}"))?;
        std::fs::write(dir.join(format!("{id}.pdf")), &bytes).map_err(|e| format!("{id}: {e}"))?;
        let expect = format!("open = \"ok\"\npages = {expect_pages}\n");
        std::fs::write(exp_dir.join(format!("{id}.toml")), &expect)
            .map_err(|e| format!("{id}: {e}"))?;
        count.set(count.get().saturating_add(1));
        Ok(())
    };

    // â”€â”€ Page-size / content variants â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
    for (i, (w, h)) in [
        (612.0, 792.0),
        (595.0, 842.0),
        (200.0, 200.0),
        (1440.0, 1440.0),
    ]
    .iter()
    .enumerate()
    {
        let mut doc = DocumentBuilder::new();
        let content = ContentBuilder::new()
            .begin_text()
            .set_font("Helvetica", 12.0)
            .text_at(50.0, 700.0)
            .show_text(&format!("Page variant {i}"))
            .end_text()
            .to_bytes();
        doc.add_page(*w, *h, &content);
        gen(&format!("page_variant_{i}"), doc, 1)?;
    }

    // â”€â”€ Content text variants â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
    for i in 0..10 {
        let mut doc = DocumentBuilder::new();
        let content = ContentBuilder::new()
            .begin_text()
            .set_font("Helvetica", 12.0)
            .text_at(50.0 + i as f64 * 10.0, 700.0 - i as f64 * 10.0)
            .show_text(&format!("Content variant {i} with some text"))
            .end_text()
            .to_bytes();
        doc.add_page(612.0, 792.0, &content);
        gen(&format!("content_variant_{i}"), doc, 1)?;
    }

    // â”€â”€ Multiple pages â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
    for pages in [2usize, 5, 10] {
        let mut doc = DocumentBuilder::new();
        for p in 0..pages {
            let content = ContentBuilder::new()
                .begin_text()
                .set_font("Helvetica", 10.0)
                .text_at(50.0, 700.0)
                .show_text(&format!("Page {p}"))
                .end_text()
                .to_bytes();
            doc.add_page(612.0, 792.0, &content);
        }
        gen(&format!("multi_page_{pages}"), doc, pages)?;
    }

    // â”€â”€ Drawing operations â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
    for (i, (r, g, b)) in [
        (0.9, 0.2, 0.2),
        (0.2, 0.9, 0.2),
        (0.2, 0.2, 0.9),
        (0.5, 0.5, 0.5),
    ]
    .iter()
    .enumerate()
    {
        let mut doc = DocumentBuilder::new();
        let content = ContentBuilder::new()
            .set_fill(*r, *g, *b)
            .fill_rect(10.0 + i as f64 * 50.0, 10.0, 200.0, 100.0)
            .to_bytes();
        doc.add_page(612.0, 792.0, &content);
        gen(&format!("drawing_colour_{i}"), doc, 1)?;
    }

    // â”€â”€ Multi-colour / multi-rect drawings â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
    for i in 0..5 {
        let mut doc = DocumentBuilder::new();
        let mut cb = ContentBuilder::new();
        for j in 0..(i + 1) * 3 {
            cb.set_fill(j as f64 * 0.1, (i as f64) * 0.1, 0.5)
                .fill_rect(10.0 + j as f64 * 30.0, 10.0, 25.0, 50.0);
        }
        doc.add_page(612.0, 792.0, &cb.to_bytes());
        gen(&format!("multi_rect_{i}"), doc, 1)?;
    }

    // â”€â”€ Form XObject â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
    for depth in 0..3u32 {
        let mut doc = DocumentBuilder::new();
        let inner = ContentBuilder::new()
            .set_fill(0.0, 0.0, 1.0)
            .fill_rect(0.0, 0.0, 100.0, 100.0)
            .to_bytes();
        let form_num = doc.allocate();
        doc.add_object(
            form_num,
            Obj::Stream {
                dict: vec![
                    (bytes(b"Type"), Obj::Name(bytes(b"XObject"))),
                    (bytes(b"Subtype"), Obj::Name(bytes(b"Form"))),
                    (
                        bytes(b"BBox"),
                        Obj::Array(vec![int(0), int(0), int(100), int(100)]),
                    ),
                    (bytes(b"Length"), Obj::Int(inner.len() as i64)),
                ],
                data: selis_bytes::Bytes::from(inner),
            },
        );
        let page_content = ContentBuilder::new().show_raw(b"/F1 Do\n").to_bytes();
        let content_num = doc.allocate();
        doc.add_object(
            content_num,
            Obj::Stream {
                dict: vec![(bytes(b"Length"), Obj::Int(page_content.len() as i64))],
                data: selis_bytes::Bytes::from(page_content),
            },
        );
        doc.add_page_with(612.0, 792.0, &[Ref::new(content_num, 0)], None);
        gen(&format!("form_xobject_{depth}"), doc, 1)?;
    }

    // â”€â”€ FlateDecode content stream â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
    let mut doc = DocumentBuilder::new();
    let raw = ContentBuilder::new()
        .begin_text()
        .set_font("Helvetica", 12.0)
        .text_at(50.0, 700.0)
        .show_text("Flate content.")
        .end_text()
        .to_bytes();
    let content_num = doc.allocate();
    doc.add_object(
        content_num,
        Obj::Stream {
            dict: vec![
                (bytes(b"Length"), Obj::Int(raw.len() as i64)),
                (bytes(b"Filter"), Obj::Name(bytes(b"FlateDecode"))),
            ],
            data: selis_bytes::Bytes::from(raw),
        },
    );
    doc.add_page_with(612.0, 792.0, &[Ref::new(content_num, 0)], None);
    gen("flate_content", doc, 1)?;

    // â”€â”€ Mutated variants (seeded) â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
    let valid = std::fs::read(dir.join("page_variant_0.pdf"))
        .map_err(|e| format!("page_variant_0.pdf: {e}"))?;
    let mut mutants = mutate(&valid);
    // Expand with more damage sites from the multi-page document.
    if let Ok(multi) = std::fs::read(dir.join("multi_page_10.pdf")) {
        mutants.extend(mutate(&multi));
        mutants.extend(mutate_vary(&multi));
    }
    for (i, bytes) in mutants.iter().enumerate() {
        let dest = dir.join(format!("mutant_{i}.pdf"));
        std::fs::write(&dest, bytes).map_err(|e| format!("mutant_{i}: {e}"))?;
        // A mutant's expectation is whatever the engine actually does with it
        // (SL-0.CORP.03): record the real outcome, so `corpus verify` flags
        // any later drift instead of silently accepting it. A damaged file
        // that opens after a tolerance fix is a *known-good* record, not a
        // contradiction â€” the expectation diff is what documents that.
        let expect = preserve_annotation(
            exp_dir.join(format!("mutant_{i}.toml")),
            crate::corpus::open_outcome_toml(&dest)?,
        );
        std::fs::write(exp_dir.join(format!("mutant_{i}.toml")), expect)
            .map_err(|e| format!("mutant_{i}: {e}"))?;
        count.set(count.get().saturating_add(1));
    }

    // â”€â”€ Combinatorial content: text x colour x position â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
    for t in 0..5 {
        for c in 0..4 {
            for pos in 0..5 {
                let id = format!("combo_{t}_{c}_{pos}");
                let mut doc = DocumentBuilder::new();
                let colours = [
                    (0.9, 0.1, 0.1),
                    (0.1, 0.9, 0.1),
                    (0.1, 0.1, 0.9),
                    (0.6, 0.6, 0.1),
                ];
                let (r, g, b) = colours[c];
                let mut cb = ContentBuilder::new();
                cb.set_fill(r, g, b).fill_rect(
                    10.0 + pos as f64 * 40.0,
                    10.0 + pos as f64 * 30.0,
                    100.0,
                    100.0,
                );
                if t % 2 == 0 {
                    cb.begin_text()
                        .set_font("Helvetica", 10.0 + t as f64)
                        .text_at(20.0, 300.0)
                        .show_text(&format!("Combo {t}-{c}-{pos}"))
                        .end_text();
                }
                doc.add_page(612.0, 792.0, &cb.to_bytes());
                gen(&id, doc, 1)?;
            }
        }
    }

    // â”€â”€ Rotated pages (extra page-dict entry) â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
    for rot in [0i64, 90, 180, 270] {
        let id = format!("rotate_{rot}");
        let mut doc = DocumentBuilder::new();
        let content = ContentBuilder::new()
            .begin_text()
            .set_font("Helvetica", 12.0)
            .text_at(50.0, 700.0)
            .show_text("Rotated page")
            .end_text()
            .to_bytes();
        let content_num = doc.allocate();
        doc.add_object(
            content_num,
            Obj::Stream {
                dict: vec![(bytes(b"Length"), Obj::Int(content.len() as i64))],
                data: selis_bytes::Bytes::from(content),
            },
        );
        doc.add_page_with_extra(
            612.0,
            792.0,
            &[Ref::new(content_num, 0)],
            None,
            vec![(b"Rotate".to_vec(), Obj::Int(rot))],
        );
        gen(&id, doc, 1)?;
    }

    // â”€â”€ Text-paint variants (stroke text matrix) â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
    for t in 0..8 {
        let id = format!("text_paint_{t}");
        let mut doc = DocumentBuilder::new();
        let mut cb = ContentBuilder::new();
        cb.begin_text()
            .set_font("Times-Roman", 8.0 + t as f64)
            .set_text_matrix(40.0, 700.0 - t as f64 * 40.0)
            .show_text(&format!("Text paint {t}"))
            .end_text();
        doc.add_page(612.0, 792.0, &cb.to_bytes());
        gen(&id, doc, 1)?;
    }

    // â”€â”€ Two-page with resources (empty Resources dict) â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
    for i in 0..12 {
        let id = format!("resources_page_{i}");
        let mut doc = DocumentBuilder::new();
        let content = ContentBuilder::new()
            .set_fill(0.1 * (i as f64 % 10.0), 0.0, 0.0)
            .fill_rect(5.0, 5.0, 100.0 + i as f64 * 10.0, 100.0)
            .to_bytes();
        let content_num = doc.allocate();
        doc.add_object(
            content_num,
            Obj::Stream {
                dict: vec![(bytes(b"Length"), Obj::Int(content.len() as i64))],
                data: selis_bytes::Bytes::from(content),
            },
        );
        doc.add_page_with(612.0, 792.0, &[Ref::new(content_num, 0)], None);
        gen(&id, doc, 1)?;
    }

    // â”€â”€ Additional mutants to reach â‰¥200 DoD â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
    if let Ok(large) = std::fs::read(dir.join("multi_page_10.pdf")) {
        // Byte-flip every 7th byte from offset 100.
        let mut b = large.to_vec();
        let mut i = 100usize;
        while i < b.len() {
            b[i] = b[i].wrapping_add(1);
            i = i.saturating_add(7);
        }
        let idx = count.get();
        let dest = dir.join(format!("mut_{idx}.pdf"));
        std::fs::write(&dest, &b).map_err(|e| format!("mut_{idx}: {e}"))?;
        let expect = preserve_annotation(
            exp_dir.join(format!("mut_{idx}.toml")),
            crate::corpus::open_outcome_toml(&dest)?,
        );
        std::fs::write(exp_dir.join(format!("mut_{idx}.toml")), expect)
            .map_err(|e| format!("mut_{idx}: {e}"))?;
        count.set(count.get() + 1);
    }
    // More mutants from a different source.
    if let Ok(multi5) = std::fs::read(dir.join("multi_page_5.pdf")) {
        for frac in [0.15, 0.28, 0.42, 0.58, 0.72, 0.88] {
            let cut = (multi5.len() as f64 * frac) as usize;
            if cut > 0 && cut < multi5.len() {
                let idx = count.get();
                let dest = dir.join(format!("mut_{idx}.pdf"));
                std::fs::write(&dest, &multi5[..cut]).map_err(|e| format!("mut_{idx}: {e}"))?;
                let expect = preserve_annotation(
                    exp_dir.join(format!("mut_{idx}.toml")),
                    crate::corpus::open_outcome_toml(&dest)?,
                );
                std::fs::write(exp_dir.join(format!("mut_{idx}.toml")), expect)
                    .map_err(|e| format!("mut_{idx}: {e}"))?;
                count.set(count.get() + 1);
            }
        }
    }

    // ── Rotated pages with swapped MediaBox dimensions (SL-2.RAST.12) ─────
    //
    // The compound case: a landscape (already-swapped) MediaBox combined with
    // /Rotate. The rendered canvas must be the rotated MediaBox — portrait
    // again for 90/270 — with the content transformed accordingly. The
    // content marks two opposite corners so a wrong rotation is visible in
    // any render.
    for rot in [90i64, 180, 270] {
        let id = format!("rotate_swapped_{rot}");
        let mut doc = DocumentBuilder::new();
        let mut content = ContentBuilder::new();
        content
            .set_fill(0.0, 0.0, 0.8)
            .fill_rect(600.0, 60.0, 120.0, 80.0)
            .begin_text()
            .set_font("Helvetica", 14.0)
            .text_at(60.0, 540.0)
            .show_text(&format!("Swapped MediaBox Rotate {rot}"))
            .end_text();
        let content_bytes = content.to_bytes();
        let content_num = doc.allocate();
        doc.add_object(
            content_num,
            Obj::Stream {
                dict: vec![(bytes(b"Length"), Obj::Int(content_bytes.len() as i64))],
                data: selis_bytes::Bytes::from(content_bytes),
            },
        );
        // Landscape Letter: width and height already swapped vs the portrait
        // default.
        doc.add_page_with_extra(
            792.0,
            612.0,
            &[Ref::new(content_num, 0)],
            None,
            vec![(b"Rotate".to_vec(), Obj::Int(rot))],
        );
        gen(&id, doc, 1)?;
    }

    // ── SL-2.RAST.13 regression corpus: one file per confirmed root cause ──

    // Text positioned with `TD` (which also sets the leading): the move uses
    // the positive ty (a negated ty mirrored TD text below the page).
    {
        let id = "text_td".to_string();
        let mut doc = DocumentBuilder::new();
        let content = b"BT /Helvetica 14 Tf 20 20 TD (TD-positioned text) Tj ET".to_vec();
        let content_num = doc.allocate();
        doc.add_object(
            content_num,
            Obj::Stream {
                dict: vec![(bytes(b"Length"), Obj::Int(content.len() as i64))],
                data: selis_bytes::Bytes::from(content),
            },
        );
        doc.add_page_with_extra(300.0, 120.0, &[Ref::new(content_num, 0)], None, Vec::new());
        gen(&id, doc, 1)?;
    }

    // /Contents as an indirect reference to an array of stream refs (ISO
    // 32000-2 §7.7.3.3): a page model that keeps the bare array ref renders
    // blank.
    {
        let id = "contents_ref_array".to_string();
        let mut doc = DocumentBuilder::new();
        let content = b"1 0 0 rg 20 20 260 80 re f".to_vec();
        let content_num = doc.allocate();
        doc.add_object(
            content_num,
            Obj::Stream {
                dict: vec![(bytes(b"Length"), Obj::Int(content.len() as i64))],
                data: selis_bytes::Bytes::from(content),
            },
        );
        // The array of content-stream refs, itself an indirect object.
        let array_num = doc.allocate();
        doc.add_object(
            array_num,
            Obj::Array(vec![Obj::Ref(Ref::new(content_num, 0))]),
        );
        // /Contents points at the array object (single-ref shape).
        doc.add_page_with_extra(300.0, 120.0, &[Ref::new(array_num, 0)], None, Vec::new());
        gen(&id, doc, 1)?;
    }

    // An LZW-encoded content stream: the decoder must read codes MSB-first
    // and widen the code width per /EarlyChange (the spec default 1 applies
    // when /DecodeParms is absent).
    {
        let id = "lzw_content".to_string();
        let mut doc = DocumentBuilder::new();
        let content = b"1 0 0 rg 20 20 260 80 re f 0 0 1 rg 40 40 200 40 re f".to_vec();
        let encoded = selis_pdf_filter::lzw_encode(&content, 1);
        let content_num = doc.allocate();
        doc.add_object(
            content_num,
            Obj::Stream {
                dict: vec![
                    (bytes(b"Length"), Obj::Int(encoded.len() as i64)),
                    (bytes(b"Filter"), Obj::Name(bytes(b"LZWDecode"))),
                ],
                data: selis_bytes::Bytes::from(encoded),
            },
        );
        doc.add_page_with_extra(300.0, 120.0, &[Ref::new(content_num, 0)], None, Vec::new());
        gen(&id, doc, 1)?;
    }

    // An annotation whose /AP /N normal appearance carries the page's only
    // visible content: the appearance must render as a form mapped onto the
    // /Rect (ISO 32000-2 §12.5.5).
    {
        let id = "annotation_appearance".to_string();
        let mut doc = DocumentBuilder::new();
        // Empty page content: everything visible comes from the annot.
        let content_num = doc.allocate();
        doc.add_object(
            content_num,
            Obj::Stream {
                dict: vec![(bytes(b"Length"), Obj::Int(0))],
                data: selis_bytes::Bytes::new(),
            },
        );
        // The normal-appearance form: a filled rect + text in form space.
        let form_content =
            b"0.1 0.4 1 rg 0 0 400 200 re f 0 0 0 rg BT /Helvetica 24 Tf 20 90 Td (Annotation appearance) Tj ET"
                .to_vec();
        let form_num = doc.allocate();
        doc.add_object(
            form_num,
            Obj::Stream {
                dict: vec![
                    (bytes(b"Type"), Obj::Name(bytes(b"XObject"))),
                    (bytes(b"Subtype"), Obj::Name(bytes(b"Form"))),
                    (
                        bytes(b"BBox"),
                        Obj::Array(vec![int(0), int(0), int(400), int(200)]),
                    ),
                    (bytes(b"Length"), Obj::Int(form_content.len() as i64)),
                ],
                data: selis_bytes::Bytes::from(form_content),
            },
        );
        // /AP << /N form >>.
        let ap_num = doc.allocate();
        doc.add_object(
            ap_num,
            Obj::Dict(vec![(bytes(b"N"), Obj::Ref(Ref::new(form_num, 0)))]),
        );
        // The annotation itself.
        let annot_num = doc.allocate();
        doc.add_object(
            annot_num,
            Obj::Dict(vec![
                (bytes(b"Type"), Obj::Name(bytes(b"Annot"))),
                (bytes(b"Subtype"), Obj::Name(bytes(b"Square"))),
                (
                    bytes(b"Rect"),
                    Obj::Array(vec![real(100.0), real(100.0), real(500.0), real(300.0)]),
                ),
                (bytes(b"F"), Obj::Int(4)),
                (bytes(b"AP"), Obj::Ref(Ref::new(ap_num, 0))),
            ]),
        );
        doc.add_page_with_extra(
            612.0,
            792.0,
            &[Ref::new(content_num, 0)],
            None,
            vec![(
                b"Annots".to_vec(),
                Obj::Array(vec![Obj::Ref(Ref::new(annot_num, 0))]),
            )],
        );
        gen(&id, doc, 1)?;
    }

    // An image XObject placed at an offset (the placement origin must
    // translate AFTER the DPI scale — a pre-scaled origin drew the image off
    // the canvas at high DPI).
    {
        let id = "image_offset".to_string();
        let mut doc = DocumentBuilder::new();
        let content = b"q 100 0 0 80 60 30 cm /Im0 Do Q".to_vec();
        let content_num = doc.allocate();
        doc.add_object(
            content_num,
            Obj::Stream {
                dict: vec![(bytes(b"Length"), Obj::Int(content.len() as i64))],
                data: selis_bytes::Bytes::from(content),
            },
        );
        // A 2×2 DeviceRGB image, raw samples (red/green/blue/white).
        let samples: Vec<u8> = vec![255, 0, 0, 0, 255, 0, 0, 0, 255, 255, 255, 255];
        let image_num = doc.allocate();
        doc.add_object(
            image_num,
            Obj::Stream {
                dict: vec![
                    (bytes(b"Type"), Obj::Name(bytes(b"XObject"))),
                    (bytes(b"Subtype"), Obj::Name(bytes(b"Image"))),
                    (bytes(b"Width"), Obj::Int(2)),
                    (bytes(b"Height"), Obj::Int(2)),
                    (bytes(b"ColorSpace"), Obj::Name(bytes(b"DeviceRGB"))),
                    (bytes(b"BitsPerComponent"), Obj::Int(8)),
                    (bytes(b"Length"), Obj::Int(samples.len() as i64)),
                ],
                data: selis_bytes::Bytes::from(samples),
            },
        );
        let resources_num = doc.allocate();
        doc.add_object(
            resources_num,
            Obj::Dict(vec![(
                bytes(b"XObject"),
                Obj::Dict(vec![(bytes(b"Im0"), Obj::Ref(Ref::new(image_num, 0)))]),
            )]),
        );
        doc.add_page_with_extra(
            300.0,
            160.0,
            &[Ref::new(content_num, 0)],
            Some(Ref::new(resources_num, 0)),
            Vec::new(),
        );
        gen(&id, doc, 1)?;
    }

    // An inline image whose data run is shorter than /W × /H needs: missing
    // samples are zero-filled (black), matching the render oracle, instead of
    // the image silently vanishing.
    {
        let id = "inline_image_truncated".to_string();
        let mut doc = DocumentBuilder::new();
        let mut content = b"100 0 0 100 0 0 cm\nBI /W 4 /H 4 /CS /RGB /BPC 8\nID\n".to_vec();
        // Only 6 of the required 48 sample bytes.
        content.extend_from_slice(b"\x40\x80\xc0\x20\x60\xa0");
        content.extend_from_slice(b"\nEI");
        let content_num = doc.allocate();
        doc.add_object(
            content_num,
            Obj::Stream {
                dict: vec![(bytes(b"Length"), Obj::Int(content.len() as i64))],
                data: selis_bytes::Bytes::from(content),
            },
        );
        doc.add_page_with_extra(100.0, 100.0, &[Ref::new(content_num, 0)], None, Vec::new());
        gen(&id, doc, 1)?;
    }

    println!("synthetic: {} files generated", count.get());
    generate_indic()?;
    Ok(())
}

/// More deterministic damage: corrupt every tenth byte in a sliding window
/// and truncate at finer fractions, so the mutant set scales past 200.
fn mutate_vary(src: &[u8]) -> Vec<Vec<u8>> {
    let mut out = Vec::new();
    for frac in [
        0.1, 0.2, 0.25, 0.35, 0.45, 0.5, 0.55, 0.65, 0.7, 0.75, 0.8, 0.85, 0.95,
    ] {
        let cut = (src.len() as f64 * frac) as usize;
        if cut > 0 && cut < src.len() {
            out.push(src[..cut].to_vec());
        }
    }
    for step in [8usize, 16, 32] {
        let mut b = src.to_vec();
        let mut i = step;
        while i < b.len() {
            b[i] = b[i].wrapping_add(1);
            i = i.saturating_add(step);
        }
        out.push(b);
    }
    out
}

/// Deterministically damage a valid PDF: truncate, corrupt an offset, break a
/// stream length, and flip bytes. Seeded so the output is reproducible.
fn mutate(src: &[u8]) -> Vec<Vec<u8>> {
    let mut out = Vec::new();
    // 1. Truncate at a few points.
    for frac in [0.3, 0.6, 0.9] {
        let cut = (src.len() as f64 * frac) as usize;
        if cut > 0 && cut < src.len() {
            out.push(src[..cut].to_vec());
        }
    }
    // 2. Corrupt a numeric token (xref offset / stream length).
    for &target in &[b"xref" as &[u8], b"startxref", b"/Length", b"stream"] {
        if let Some(pos) = find_bytes(src, target) {
            let mut b = src.to_vec();
            let idx = (pos + target.len() + 2).min(b.len().saturating_sub(1));
            b[idx] = b'9';
            out.push(b);
        }
    }
    // 3. Byte flips at deterministic positions.
    for &at in &[0usize, 16, 64, 128, 256] {
        if at < src.len() {
            let mut b = src.to_vec();
            b[at] ^= 0xff;
            out.push(b);
        }
    }
    // 4. Break the trailer /Root reference.
    if let Some(pos) = find_bytes(src, b"/Root") {
        let mut b = src.to_vec();
        let idx = (pos + 6 + 1).min(b.len().saturating_sub(1));
        if b.get(idx).is_some_and(|c| c.is_ascii_digit()) {
            b[idx] = b'0';
        }
        out.push(b);
    }
    out
}

fn find_bytes(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

fn bytes(s: &[u8]) -> selis_bytes::Bytes {
    selis_bytes::Bytes::copy_from_slice(s)
}
/// A real-number object (milli-unit scaled, matching the writer's convention).
fn real(v: f64) -> Obj {
    #[allow(clippy::cast_possible_truncation)]
    let scaled = (v * 1000.0).round() as i64;
    Obj::Real { scaled, scale: 3 }
}

fn int(n: i64) -> Obj {
    Obj::Int(n)
}

// ── Indic complex-script subset (SL-3.SHAPE.04) ──────────────────────────
// Shaped Type0/Identity-H fixtures: matras, conjuncts, ligatures, and
// reordering cases for Devanagari, Tamil, and Bengali. Every string below
// is covered by the HarfBuzz parity table in
// `crates/selis-shape/tests/indic.rs` — the generator refuses to emit a
// line it cannot shape as a single script run.

/// A fixture font: the vendored OFL TTF plus its descriptor metrics
/// (verified with fontTools at vendoring; informational for replay — the
/// rasterizer reads metrics from the embedded program itself).
struct IndicFont {
    /// The PDF `/BaseFont` name.
    name: &'static str,
    /// The vendored TTF path, relative to the workspace root.
    file: &'static str,
    /// `/Ascent`, `/Descent`, `/CapHeight`.
    ascent: i64,
    descent: i64,
    cap_height: i64,
    /// `/FontBBox`.
    bbox: [i64; 4],
}

const INDIC_FONTS: &[IndicFont] = &[
    IndicFont {
        name: "NotoSansDevanagari",
        file: "corpus/fixtures/fonts/NotoSansDevanagari-Regular.ttf",
        ascent: 896,
        descent: -408,
        cap_height: 622,
        bbox: [-585, -530, 1574, 1347],
    },
    IndicFont {
        name: "NotoSansTamil",
        file: "corpus/fixtures/fonts/NotoSansTamil-Regular.ttf",
        ascent: 870,
        descent: -370,
        cap_height: 714,
        bbox: [-586, -321, 2188, 870],
    },
    IndicFont {
        name: "NotoSansBengali",
        file: "corpus/fixtures/fonts/NotoSansBengali-Regular.ttf",
        ascent: 917,
        descent: -408,
        cap_height: 622,
        bbox: [-596, -408, 1244, 917],
    },
];

/// One fixture document: a font plus title/body lines. Every line must be a
/// single script run in the document's script (the generator errors
/// otherwise — a mixed-script line would need a second font).
struct IndicDoc {
    name: &'static str,
    font: usize,
    title: &'static str,
    lines: &'static [&'static str],
}

const INDIC_DOCS: &[IndicDoc] = &[
    IndicDoc {
        name: "indic_deva_00",
        font: 0,
        title: "देवनागरी",
        lines: &[
            "अ आ इ ई उ ऊ ए ऐ ओ औ अं अः",
            "का कि की कु कू के कै को कौ कं कः",
            "१२३",
        ],
    },
    IndicDoc {
        name: "indic_deva_01",
        font: 0,
        title: "देवनागरी",
        lines: &["क्त त्र ज्ञ स्व न्द ह्म स्त क्ष", "र्क क़ कँ"],
    },
    IndicDoc {
        name: "indic_deva_02",
        font: 0,
        title: "देवनागरी",
        lines: &["नमस्ते दुनिया", "१२३"],
    },
    IndicDoc {
        name: "indic_taml_00",
        font: 1,
        title: "தமிழ்",
        lines: &["அ ஆ இ ஈ உ ஊ எ ஏ ஐ ஒ ஓ ஔ", "கா கி கீ கு கூ கெ கே கை கொ கோ கௌ"],
    },
    IndicDoc {
        name: "indic_taml_01",
        font: 1,
        title: "தமிழ்",
        lines: &["க் க்ஷ ஸ்ரீ", "வணக்கம்"],
    },
    IndicDoc {
        name: "indic_taml_02",
        font: 1,
        title: "தமிழ்",
        lines: &["வணக்கம் உலகம்"],
    },
    IndicDoc {
        name: "indic_beng_00",
        font: 2,
        title: "বাংলা",
        lines: &["অ আ ই ঈ উ ঊ এ ঐ ও ঔ", "কা কি কী কু কূ কৃ কেঁ কাঁ"],
    },
    IndicDoc {
        name: "indic_beng_01",
        font: 2,
        title: "বাংলা",
        lines: &["ক্ষ ক্ত ন্ত্র জ্ঞ স্ব স্ক", "র্ক ক্র"],
    },
    IndicDoc {
        name: "indic_beng_02",
        font: 2,
        title: "বাংলা",
        // The two-part vowels below are written DECOMPOSED (explicit
        // escapes — never let an editor normalize them to the precomposed
        // forms): KA + ে (U+09C7) + া/ৗ splits the vowel around the base.
        lines: &["ক\u{9c7}\u{9be} ক\u{9c7}\u{9d7}", "নমস্কার বিশ্ব"],
    },
];

/// A shaped glyph in font units (the shaper runs at the font's own upm).
struct Shaped {
    gid: u16,
    advance: f64,
    x_off: f64,
    y_off: f64,
    cluster: u32,
}

/// Shape `text` with the product shaper (1000-upm convention, so one shaped
/// unit is one font unit). Errors when the line is not a single script run.
fn shape_line(font_data: &[u8], text: &str) -> Result<(Vec<Shaped>, u32), String> {
    let runs = selis_shape::script_runs(text);
    if runs.len() != 1 {
        return Err(format!("{text:?}: expected one script run, got {runs:?}"));
    }
    let params = ShapingParams {
        text,
        font_data,
        font_size: 1000.0,
        script: runs[0].script,
        language: None,
        features: &[],
    };
    let out = SwashShaper
        .shape(&params)
        .map_err(|e| format!("{text:?}: shaping failed: {e}"))?;
    if out.glyphs.is_empty() {
        return Err(format!("{text:?}: no glyphs"));
    }
    Ok((
        out.glyphs
            .iter()
            .map(|g| Shaped {
                gid: g.glyph_id,
                advance: f64::from(g.x_advance),
                x_off: f64::from(g.x_offset),
                y_off: f64::from(g.y_offset),
                cluster: g.cluster,
            })
            .collect(),
        runs[0].script,
    ))
}

/// Map each distinct char of `texts` (shaped in isolation) to its glyph id.
/// Only single-glyph isolated outputs count: a glyph shared with a conjunct
/// form keeps the char meaning (e.g. the base), while conjunct-only glyphs
/// fall back to their syllable's first char in [`tounicode_for`].
fn isolated_gid_map(font_data: &[u8], texts: &[&str]) -> Result<BTreeMap<u16, char>, String> {
    let chars: Vec<char> = texts
        .iter()
        .flat_map(|t| t.chars())
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect();
    let mut map = BTreeMap::new();
    for ch in chars {
        let text: String = [ch].iter().collect();
        let (shaped, _) = shape_line(font_data, &text)?;
        if shaped.len() == 1 && shaped[0].cluster == 0 && shaped[0].gid != 0 {
            map.insert(shaped[0].gid, ch);
        }
    }
    Ok(map)
}

/// The syllable byte ranges of `text` for a shaping: each cluster start
/// opens a syllable running to the next start (or end of text). Over-merged
/// clusters only widen syllables — boundaries never split one.
fn syllables(text: &str, shaped: &[Shaped]) -> Vec<(usize, usize)> {
    let mut starts: Vec<usize> = shaped.iter().map(|g| g.cluster as usize).collect();
    starts.sort();
    starts.dedup();
    starts
        .iter()
        .enumerate()
        .map(|(i, s)| {
            let end = starts.get(i + 1).copied().unwrap_or(text.len());
            (*s, end)
        })
        .collect()
}

/// The Unicode scalar a shown glyph recovers to: its isolated char when the
/// glyph is that char's own form, else the first char of its syllable (the
/// standard single-char ToUnicode fallback real producers emit for
/// conjuncts — bounded and documented, identical for both engines).
fn tounicode_for(
    gid: u16,
    cluster: u32,
    syllables: &[(usize, usize)],
    text: &str,
    isolated: &BTreeMap<u16, char>,
) -> char {
    if let Some(ch) = isolated.get(&gid) {
        return *ch;
    }
    let at = cluster as usize;
    let start = syllables
        .iter()
        .rev()
        .find(|(s, e)| *s <= at && at < *e)
        .map(|(s, _)| *s)
        .unwrap_or(0);
    text[start..].chars().next().unwrap_or('\u{FFFD}')
}

/// Generate the `indic` subset into `corpus/pdfs/indic` (+ expectations in
/// `corpus/expect/indic`). Deterministic: shaping, ordering, and number
/// formatting are all fixed; the writer emits no IDs or timestamps.
fn generate_indic() -> Result<(), String> {
    let pdf_dir = Path::new("corpus/pdfs/indic");
    let exp_dir = Path::new("corpus/expect/indic");
    if pdf_dir.exists() {
        std::fs::remove_dir_all(pdf_dir).map_err(|e| format!("clear indic: {e}"))?;
    }
    if exp_dir.exists() {
        std::fs::remove_dir_all(exp_dir).map_err(|e| format!("clear indic expect: {e}"))?;
    }
    std::fs::create_dir_all(pdf_dir).map_err(|e| format!("mkdir indic: {e}"))?;
    std::fs::create_dir_all(exp_dir).map_err(|e| format!("mkdir indic expect: {e}"))?;

    // Load the fixture fonts once (from the workspace root, like the rest
    // of the corpus tooling).
    let mut font_bytes = Vec::new();
    for font in INDIC_FONTS {
        let data = std::fs::read(font.file).map_err(|e| format!("read {}: {e}", font.file))?;
        font_bytes.push(data);
    }

    for doc in INDIC_DOCS {
        let font = &INDIC_FONTS[doc.font];
        let data = &font_bytes[doc.font];
        // All lines share the document script; the title shapes first so a
        // script mismatch fails fast with context.
        let mut shaped_lines: Vec<(f32, String, Vec<Shaped>)> = Vec::new();
        let (title_shaped, script) =
            shape_line(data, doc.title).map_err(|e| format!("{} title: {e}", doc.name))?;
        shaped_lines.push((36.0, doc.title.to_string(), title_shaped));
        for line in doc.lines {
            let (shaped, line_script) =
                shape_line(data, line).map_err(|e| format!("{}: {e}", doc.name))?;
            if line_script != script {
                return Err(format!(
                    "{}: line {line:?} itemised to a different script",
                    doc.name
                ));
            }
            shaped_lines.push((24.0, (*line).to_string(), shaped));
        }
        // The isolated map covers every distinct char of the document.
        let mut texts: Vec<&str> = vec![doc.title];
        texts.extend(doc.lines.iter().copied());
        let isolated = isolated_gid_map(data, &texts).map_err(|e| format!("{}: {e}", doc.name))?;

        // Widths: one entry per shown glyph id, sorted for determinism.
        let mut widths: BTreeMap<u16, i64> = BTreeMap::new();
        for (_, _, shaped) in &shaped_lines {
            for g in shaped {
                widths.insert(g.gid, g.advance.round() as i64);
            }
        }
        // ToUnicode: one bfchar per shown glyph id, sorted.
        let mut recovery: BTreeMap<u16, char> = BTreeMap::new();
        for (_, text, shaped) in &shaped_lines {
            let sylls = syllables(text, shaped);
            for g in shaped {
                recovery.insert(
                    g.gid,
                    tounicode_for(g.gid, g.cluster, &sylls, text, &isolated),
                );
            }
        }

        let pdf = indic_pdf(font, data, &shaped_lines, &widths, &recovery)?;
        let dest = pdf_dir.join(format!("{}.pdf", doc.name));
        std::fs::write(&dest, &pdf).map_err(|e| format!("{}: {e}", doc.name))?;
        let expect = preserve_annotation(
            exp_dir.join(format!("{}.toml", doc.name)),
            crate::corpus::open_outcome_toml(&dest)?,
        );
        std::fs::write(exp_dir.join(format!("{}.toml", doc.name)), expect)
            .map_err(|e| format!("{} expect: {e}", doc.name))?;
    }
    println!("synthetic: {} indic files generated", INDIC_DOCS.len());
    Ok(())
}

/// Assemble one shaped Type0 PDF: FontFile2 + ToUnicode + CIDFontType2
/// descendant + Type0 wrapper + one page of TJ lines.
fn indic_pdf(
    font: &IndicFont,
    data: &[u8],
    lines: &[(f32, String, Vec<Shaped>)],
    widths: &BTreeMap<u16, i64>,
    recovery: &BTreeMap<u16, char>,
) -> Result<Vec<u8>, String> {
    let budget = selis_sandbox::Budget::unlimited();
    let clock = selis_sandbox::shell_clock();
    let mut guard = budget.guard_with(&clock, selis_sandbox::CancelToken::new());
    let mut doc = DocumentBuilder::new();

    // The embedded program (unfiltered: deterministic bytes, and the engine
    // exercises the raw FontFile2 path real producers also emit).
    let font_file = doc.allocate();
    doc.add_object(
        font_file,
        Obj::Stream {
            dict: vec![
                (bytes(b"Length"), int(data.len() as i64)),
                (bytes(b"Length1"), int(data.len() as i64)),
            ],
            data: Bytes::copy_from_slice(data),
        },
    );
    // ToUnicode: one bfchar per shown CID.
    let mut bfchar = String::from(
        "/CIDInit /ProcSet findresource begin\n12 dict begin\nbegincmap\n/CMapName /Adobe-Identity-UCS def\n/CMapType 2 def\n1 begincodespacerange\n<0000> <FFFF>\nendcodespacerange\n",
    );
    bfchar.push_str(&format!("{} beginbfchar\n", recovery.len()));
    for (cid, ch) in recovery {
        bfchar.push_str(&format!("<{cid:04X}> <{:04X}>\n", *ch as u32));
    }
    bfchar
        .push_str("endbfchar\nendcmap\nCMapName currentdict /CMap defineresource pop\nend\nend\n");
    let to_unicode = doc.allocate();
    doc.add_object(
        to_unicode,
        Obj::Stream {
            dict: vec![(bytes(b"Length"), int(bfchar.len() as i64))],
            data: Bytes::copy_from_slice(bfchar.as_bytes()),
        },
    );
    // The descendant CIDFont: /W from shaped advances, /DW default.
    let mut w_array = Vec::new();
    for (gid, width) in widths {
        w_array.push(Obj::Int(*gid as i64));
        w_array.push(Obj::Array(vec![Obj::Int(*width)]));
    }
    let descendant = doc.allocate();
    doc.add_object(
        descendant,
        Obj::Dict(vec![
            (bytes(b"Type"), Obj::Name(bytes(b"Font"))),
            (bytes(b"Subtype"), Obj::Name(bytes(b"CIDFontType2"))),
            (bytes(b"BaseFont"), Obj::Name(bytes(font.name.as_bytes()))),
            (
                bytes(b"CIDSystemInfo"),
                Obj::Dict(vec![
                    (bytes(b"Registry"), Obj::String(bytes(b"Adobe"))),
                    (bytes(b"Ordering"), Obj::String(bytes(b"Identity"))),
                    (bytes(b"Supplement"), int(0)),
                ]),
            ),
            (
                bytes(b"FontDescriptor"),
                Obj::Dict(vec![
                    (bytes(b"FontName"), Obj::Name(bytes(font.name.as_bytes()))),
                    (bytes(b"Flags"), int(4)),
                    (
                        bytes(b"FontBBox"),
                        Obj::Array(font.bbox.iter().map(|v| int(*v)).collect()),
                    ),
                    (bytes(b"ItalicAngle"), int(0)),
                    (bytes(b"Ascent"), int(font.ascent)),
                    (bytes(b"Descent"), int(font.descent)),
                    (bytes(b"CapHeight"), int(font.cap_height)),
                    (bytes(b"StemV"), int(80)),
                    (bytes(b"FontFile2"), Obj::Ref(Ref::new(font_file, 0))),
                ]),
            ),
            (bytes(b"CIDToGIDMap"), Obj::Name(bytes(b"Identity"))),
            (bytes(b"DW"), int(1000)),
            (bytes(b"W"), Obj::Array(w_array)),
        ]),
    );
    let type0 = doc.allocate();
    doc.add_object(
        type0,
        Obj::Dict(vec![
            (bytes(b"Type"), Obj::Name(bytes(b"Font"))),
            (bytes(b"Subtype"), Obj::Name(bytes(b"Type0"))),
            (bytes(b"BaseFont"), Obj::Name(bytes(font.name.as_bytes()))),
            (bytes(b"Encoding"), Obj::Name(bytes(b"Identity-H"))),
            (
                bytes(b"DescendantFonts"),
                Obj::Array(vec![Obj::Ref(Ref::new(descendant, 0))]),
            ),
            (bytes(b"ToUnicode"), Obj::Ref(Ref::new(to_unicode, 0))),
        ]),
    );
    let resources = doc.allocate();
    doc.add_object(
        resources,
        Obj::Dict(vec![(
            bytes(b"Font"),
            Obj::Dict(vec![(bytes(b"F1"), Obj::Ref(Ref::new(type0, 0)))]),
        )]),
    );

    // One TJ per y-stable glyph run: kerns absorb shaped x offsets, `Td`
    // carries y offsets (marks above/below the base). All numbers use fixed
    // precision so reruns are byte-identical.
    let mut content = Vec::new();
    content.extend_from_slice(b"BT\n");
    // Pen tracking in text space (matches the interpreter exactly: rounded
    // /W advances, kerns as written).
    let (mut pen_x, mut pen_y) = (0.0f64, 0.0f64);
    let mut line_y = 760.0f64;
    for (size, _, shaped) in lines {
        let size = f64::from(*size);
        let k = size / 1000.0;
        let line_x = 56.0f64;
        content.extend_from_slice(
            format!(
                "{:.2} {:.2} Td\n/F1 {:.1} Tf\n",
                line_x - pen_x,
                line_y - pen_y,
                size
            )
            .as_bytes(),
        );
        pen_x = line_x;
        pen_y = line_y;
        // Glyph-space pen (what the next kern measures from).
        let (mut px, mut py) = (pen_x, pen_y);
        let mut cum_x = 0.0f64;
        let mut tj_open = false;
        for g in shaped {
            let want_x = line_x + (cum_x + g.x_off) * k;
            let want_y = line_y + g.y_off * k;
            if (want_y - py).abs() >= 0.005 {
                if tj_open {
                    content.extend_from_slice(b"] TJ\n");
                    tj_open = false;
                }
                content.extend_from_slice(format!("0 {:.2} Td\n", want_y - py).as_bytes());
                py = want_y;
            }
            if !tj_open {
                content.extend_from_slice(b"[");
                tj_open = true;
            }
            let kern = (px - want_x) / k;
            if kern.abs() >= 0.005 {
                content.extend_from_slice(format!("{kern:.2} ").as_bytes());
            }
            content.extend_from_slice(format!("<{:04X}> ", g.gid).as_bytes());
            // The interpreter advances by the rounded /W width we wrote.
            let w = widths
                .get(&g.gid)
                .copied()
                .expect("every shown gid has a /W entry") as f64;
            px = want_x + w * k;
            cum_x += g.advance;
        }
        if tj_open {
            content.extend_from_slice(b"] TJ\n");
        }
        line_y -= size * 1.5;
    }
    content.extend_from_slice(b"ET\n");
    let content_ref = doc.allocate();
    doc.add_object(
        content_ref,
        Obj::Stream {
            dict: vec![(bytes(b"Length"), int(content.len() as i64))],
            data: Bytes::copy_from_slice(&content),
        },
    );
    doc.add_page_with(
        595.0,
        842.0,
        &[Ref::new(content_ref, 0)],
        Some(Ref::new(resources, 0)),
    );
    doc.write(&budget, &mut guard)
        .map_err(|e| format!("indic write: {e}"))
}
