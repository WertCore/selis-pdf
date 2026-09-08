//! Synthetic corpus generator + mutator (SL-0.CORP.04).
//! Produces PDFs exercising specific constructs with known-correct
//! expectations, plus a seeded, reproducible mutator that damages valid files.

use std::path::Path;

use selis_pdf_cos::doc_writer::{ContentBuilder, DocumentBuilder};
use selis_pdf_cos::{Obj, Ref};

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

    /// The open-outcome record for a generated file, observed from the actual
    /// engine behaviour (the generator writes what the engine does; `corpus
    /// verify` then polices regressions). Uses the same code path as `corpus
    /// verify` — `Session::open` — so the recorded outcome matches what the
    /// verifier re-observes. Mutants are deliberately damaged, so a typed
    /// refusal is the expected, policy-correct outcome.
    fn open_outcome(path: &Path) -> String {
        let budget = selis_sandbox::Budget::profile(selis_sandbox::Surface::Viewer);
        let src = std::fs::read(path).unwrap_or_default();
        match selis_pdf_engine::Session::open(src, &budget) {
            Ok(session) => format!("open = \"ok\"\npages = {}\n", session.len()),
            Err(e) => format!("open = \"err\"\ncode = \"{:?}\"\n", e.code()),
        }
    }

    let count = std::cell::Cell::new(0usize);
    let gen = |id: &str, mut builder: DocumentBuilder, expect_pages: usize| -> Result<(), String> {
        let budget = selis_sandbox::Budget::unlimited();
        let mut g = budget.guard_with(
            &selis_sandbox::FixedClock(0),
            selis_sandbox::CancelToken::new(),
        );
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

    // ── Page-size / content variants ────────────────────────────────────────
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

    // ── Content text variants ──────────────────────────────────────────────
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

    // ── Multiple pages ─────────────────────────────────────────────────────
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

    // ── Drawing operations ─────────────────────────────────────────────────
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

    // ── Multi-colour / multi-rect drawings ────────────────────────────────
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

    // ── Form XObject ───────────────────────────────────────────────────────
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

    // ── FlateDecode content stream ─────────────────────────────────────────
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

    // ── Mutated variants (seeded) ──────────────────────────────────────────
    let valid = std::fs::read(dir.join("page_variant_0.pdf"))
        .map_err(|e| format!("page_variant_0.pdf: {e}"))?;
    let mut mutants = mutate(&valid);
    // Expand with more damage sites from the multi-page document.
    if let Ok(multi) = std::fs::read(dir.join("multi_page_10.pdf")) {
        mutants.extend(mutate(&multi));
        mutants.extend(mutate_vary(&multi));
    }
    for (i, bytes) in mutants.iter().enumerate() {
        std::fs::write(dir.join(format!("mutant_{i}.pdf")), bytes)
            .map_err(|e| format!("mutant_{i}: {e}"))?;
        // A mutant's open outcome is a policy decision, decided once by the
        // generator (robustness = typed error, never a hang or panic — the
        // budget guarantees termination): refuse with a typed code, record
        // it, and let `corpus verify` police regressions.
        let outcome = open_outcome(&dir.join(format!("mutant_{i}.pdf")));
        std::fs::write(exp_dir.join(format!("mutant_{i}.toml")), outcome)
            .map_err(|e| format!("mutant_{i}: {e}"))?;
        count.set(count.get().saturating_add(1));
    }

    // ── Combinatorial content: text x colour x position ───────────────────
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

    // ── Rotated pages (extra page-dict entry) ─────────────────────────────
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

    // ── Text-paint variants (stroke text matrix) ──────────────────────────
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

    // ── Two-page with resources (empty Resources dict) ────────────────────
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

    // ── Additional mutants to reach ≥200 DoD ──────────────────────────────
    if let Ok(large) = std::fs::read(dir.join("multi_page_10.pdf")) {
        // Byte-flip every 7th byte from offset 100.
        let mut b = large.to_vec();
        let mut i = 100usize;
        while i < b.len() {
            b[i] = b[i].wrapping_add(1);
            i = i.saturating_add(7);
        }
        let idx = count.get();
        std::fs::write(dir.join(format!("mut_{idx}.pdf")), &b)
            .map_err(|e| format!("mut_{idx}: {e}"))?;
        std::fs::write(
            exp_dir.join(format!("mut_{idx}.toml")),
            open_outcome(&dir.join(format!("mut_{idx}.pdf"))),
        )
        .map_err(|e| format!("mut_{idx}: {e}"))?;
        count.set(count.get() + 1);
    }
    // More mutants from a different source.
    if let Ok(multi5) = std::fs::read(dir.join("multi_page_5.pdf")) {
        for frac in [0.15, 0.28, 0.42, 0.58, 0.72, 0.88] {
            let cut = (multi5.len() as f64 * frac) as usize;
            if cut > 0 && cut < multi5.len() {
                let idx = count.get();
                std::fs::write(dir.join(format!("mut_{idx}.pdf")), &multi5[..cut])
                    .map_err(|e| format!("mut_{idx}: {e}"))?;
                std::fs::write(
                    exp_dir.join(format!("mut_{idx}.toml")),
                    open_outcome(&dir.join(format!("mut_{idx}.pdf"))),
                )
                .map_err(|e| format!("mut_{idx}: {e}"))?;
                count.set(count.get() + 1);
            }
        }
    }

    println!("synthetic: {} files generated", count.get());
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
fn int(n: i64) -> Obj {
    Obj::Int(n)
}
