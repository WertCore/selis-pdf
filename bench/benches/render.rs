//! Page-render benchmarks — SL-2.PERF.02.
//!
//! Renders each page of the deterministic benchmark set (`xtask render-set`,
//! `bench/render-set/`) end-to-end at native scale (72 DPI = 1 pt per px,
//! the scale at which engine and oracles rasterise identical pixel counts).
//! Each iteration opens the document in setup (excluded) and times
//! `render_page` — display-list build plus rasterisation, the first-paint
//! path. Means feed `bench/baselines.json` (reference record) and back the
//! §12 render rows in `xtask/perf-budgets.toml`.

#![allow(missing_docs)]

use std::path::{Path, PathBuf};

use criterion::{black_box, criterion_group, criterion_main, BatchSize, Criterion};

use selis_pdf_engine::Session;

/// (bench name, set file) in benchmark order.
const PAGES: &[(&str, &str)] = &[
    ("render_text_heavy", "text-heavy.pdf"),
    ("render_vector_heavy", "vector-heavy.pdf"),
    ("render_large_image", "large-image.pdf"),
    ("render_shading", "shading.pdf"),
    ("render_transparency", "transparency.pdf"),
    ("render_mixed", "mixed.pdf"),
];

fn set_file(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("render-set")
        .join(name)
}

/// Read a set file, exiting loudly (not silently skipping) when it is
/// missing — a bench that measures nothing must fail, never pass.
fn read_set_file(path: &Path) -> Vec<u8> {
    match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(e) => {
            eprintln!(
                "render bench: {}: {e} (run `cargo xtask render-set` first)",
                path.display()
            );
            std::process::exit(1);
        }
    }
}

fn open_session(src: &[u8]) -> Option<Session> {
    let budget = selis_sandbox::Budget::profile(selis_sandbox::Surface::Viewer);
    let clock = selis_sandbox::FixedClock(0);
    Session::open(src.to_vec(), &budget, &clock).ok()
}

fn page_view_of(session: &Session) -> Option<selis_pdf_engine::PageView> {
    session.page_view(0, 72.0)
}

fn bench_render(c: &mut Criterion) {
    for (name, file) in PAGES {
        let src = read_set_file(&set_file(file));
        let session = match open_session(&src) {
            Some(s) => s,
            None => {
                eprintln!("render bench: {file}: engine cannot open the set file");
                std::process::exit(1);
            }
        };
        let view = match page_view_of(&session) {
            Some(v) => v,
            None => {
                eprintln!("render bench: {file}: page has no usable media box");
                std::process::exit(1);
            }
        };
        let (w, h) = (view.width, view.height);
        let ctm = view.ctm;
        c.bench_function(name, |b| {
            b.iter_batched(
                || open_session(&src),
                |session| {
                    if let Some(session) = session {
                        let budget = selis_sandbox::Budget::profile(selis_sandbox::Surface::Viewer);
                        let clock = selis_sandbox::FixedClock(0);
                        if let Some(mut backend) = selis_pdf_engine::TinySkiaBackend::new(w, h) {
                            let mut g =
                                budget.guard_with(&clock, selis_sandbox::CancelToken::new());
                            let _ = black_box(session.render_page(
                                0,
                                &mut backend,
                                ctm,
                                &budget,
                                &mut g,
                            ));
                        }
                    }
                },
                BatchSize::SmallInput,
            );
        });
    }
}

criterion_group!(benches, bench_render);
criterion_main!(benches);
