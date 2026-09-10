//! Render throughput measurement (SL-2.PERF.02).
//!
//! `xtask perf-render` renders every page of the benchmark set (`xtask
//! render-set`) with the engine and with each available oracle, and writes
//! `bench/render-results.json` — the record the `ratio` row of
//! `xtask/perf-budgets.toml` is gated on (read by `xtask perf-check`).
//!
//! Method (so the numbers are reproducible and honestly labelled):
//!
//! * The engine runs **in-process** (this binary): one `Session::open` per
//!   file (timed separately as cold open), one untimed warm-up render, then
//!   `repeats` timed `render_page` calls; the median is recorded. Open is
//!   excluded from the render number; display-list build and rasterisation
//!   are both included (that is what a viewer waits for on first paint).
//!   A wrapping checksum over the final pixmap is recorded per repeat and
//!   must agree across repeats, or the run fails: a throughput number over a
//!   non-deterministic renderer is meaningless (SL-2.RAST.09).
//! * Oracles run as **spawned processes** at the same DPI over the same
//!   files (`mutool draw`, `pdfium_driver --page 1 --dpi N`); the median of
//!   `repeats` wall times is recorded **spawn-inclusive**. That overhead
//!   penalises the oracle side; the report says so. Spawned binaries resolve
//!   through explicit `--*-path` flags or PATH lookup, and are executed by
//!   canonicalised absolute path.
//! * The ratio is oracle_ms / selis_ms per page (throughput ratio); the
//!   headline is the geometric mean across the set. The DoD target (≥0.6×
//!   PDFium) is evaluated on `geomean_vs_pdfium`.
//! * DPI defaults to 72 (1 pt = 1 px), where the engine and the oracles
//!   rasterise identical pixel counts. The engine has no page→device matrix
//!   yet, so a 150-DPI canvas would compare a full oracle raster against
//!   unscaled engine content; `--dpi 150` is accepted but the results file
//!   records the DPI and the PERF report explains the gap (CONF.01 consumes
//!   the device-matrix fix).
//!
//! Record in release (`cargo run --release -p xtask -- perf-render`): a
//! debug build measures the unoptimised engine and the results file records
//! the profile. Under `debug_assertions` the command still runs (useful for
//! smoke checks) but prints a warning and stamps `"profile": "debug"`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Instant;

use selis_pdf_content::display_list::Op;

/// One page's measurement.
struct PageResult {
    file: String,
    axis: String,
    op_count: usize,
    histogram: BTreeMap<&'static str, usize>,
    glyph_count: usize,
    open_ms: f64,
    dl_ms: f64,
    render_ms: f64,
    checksum: String,
    counters: BTreeMap<String, u64>,
    oracle_ms: BTreeMap<String, f64>,
}

/// Which oracle binaries to time and where they resolve from.
pub struct OracleTools {
    /// Preferred oracle ids in order (`mutool`, `pdfium`).
    pub tools: Vec<String>,
    /// Explicit `pdfium_driver` path (else PATH lookup).
    pub pdfium_driver: Option<PathBuf>,
    /// Explicit `mutool` path (else PATH lookup).
    pub mutool: Option<PathBuf>,
}

/// Measure the set into `out` (default `bench/render-results.json`).
#[allow(clippy::too_many_arguments)]
pub fn run(
    set: &Path,
    dpi: u32,
    repeats: usize,
    out: &Path,
    tools: &OracleTools,
    skip_oracles: bool,
) -> Result<(), String> {
    if repeats == 0 {
        return Err("--repeats must be at least 1".to_string());
    }
    if cfg!(debug_assertions) {
        println!(
            "perf-render: WARNING — debug build; numbers measure the unoptimised engine. \
             Record reference numbers with `cargo run --release -p xtask -- perf-render`."
        );
    }
    let mut files: Vec<PathBuf> = std::fs::read_dir(set)
        .map_err(|e| format!("{}: {e}", set.display()))?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "pdf"))
        .collect();
    files.sort();
    if files.is_empty() {
        return Err(format!("no PDFs in {}", set.display()));
    }

    // Resolve oracle binaries once (canonicalised absolute paths).
    let mut oracle_bins: Vec<(String, PathBuf)> = Vec::new();
    if !skip_oracles {
        for tool in &tools.tools {
            match tool.as_str() {
                "mutool" => {
                    if let Some(bin) = resolve_tool(&tools.mutool, "mutool") {
                        oracle_bins.push(("mutool".to_string(), bin));
                    } else {
                        println!("perf-render: mutool not found (PATH or --mutool); skipped");
                    }
                }
                "pdfium" => {
                    if let Some(bin) = resolve_tool(&tools.pdfium_driver, "pdfium_driver") {
                        oracle_bins.push(("pdfium".to_string(), bin));
                    } else {
                        println!(
                            "perf-render: pdfium_driver not found (PATH or --pdfium-driver); skipped"
                        );
                    }
                }
                other => {
                    return Err(format!(
                        "unknown oracle tool `{other}` (want mutool|pdfium)"
                    ))
                }
            }
        }
    }

    let tmp = std::env::temp_dir().join(format!("selis-perf-render-{}", std::process::id()));
    std::fs::create_dir_all(&tmp).map_err(|e| format!("{}: {e}", tmp.display()))?;

    // Per-tool spawn cost (a trivial invocation, median of 5): the oracle
    // wall times below are spawn-inclusive, so this component — measured,
    // not guessed — lets the report separate process overhead from render
    // throughput instead of silently crediting it to us.
    let mut spawn_ms: BTreeMap<String, f64> = BTreeMap::new();
    for (id, bin) in &oracle_bins {
        let mut samples: Vec<f64> = Vec::new();
        for _ in 0..5 {
            let t = Instant::now();
            let _ = measure_spawn(id, bin);
            samples.push(t.elapsed().as_secs_f64() * 1000.0);
        }
        let median = median_ms(samples);
        println!("perf-render: {id} spawn ≈ {median:.1}ms (spawn-inclusive oracle times below)");
        spawn_ms.insert(id.clone(), median);
    }

    let mut pages: Vec<PageResult> = Vec::new();
    for file in &files {
        let stem = file
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("?")
            .to_string();
        println!("perf-render: {stem} ...");
        let page = measure_page(file, &stem, dpi, repeats, &oracle_bins, &tmp)?;
        println!(
            "  ops={} glyphs={} open={:.2}ms dl={:.2}ms render={:.2}ms checksum={} {}",
            page.op_count,
            page.glyph_count,
            page.open_ms,
            page.dl_ms,
            page.render_ms,
            page.checksum,
            oracle_summary(&page),
        );
        pages.push(page);
    }

    let _ = std::fs::remove_dir_all(&tmp);
    write_results(out, set, dpi, repeats, &pages, &spawn_ms)
}

/// Median of the samples in milliseconds.
fn median_ms(mut samples: Vec<f64>) -> f64 {
    samples.sort_by(|a, b| a.total_cmp(b));
    let mid = samples.len() / 2;
    samples.get(mid).copied().unwrap_or(0.0)
}

/// The median render checksum must be unanimous: every repeat's pixmap hash
/// must equal the first, or the renderer is non-deterministic (RAST.09).
fn measure_page(
    file: &Path,
    stem: &str,
    dpi: u32,
    repeats: usize,
    oracle_bins: &[(String, PathBuf)],
    tmp: &Path,
) -> Result<PageResult, String> {
    let src = std::fs::read(file).map_err(|e| format!("{}: {e}", file.display()))?;
    let budget = selis_sandbox::Budget::profile(selis_sandbox::Surface::Viewer);
    let clock = selis_sandbox::shell_clock();

    let t = Instant::now();
    let session = selis_pdf_engine::Session::open(src, &budget, &clock)
        .map_err(|e| format!("{}: open: {e}", file.display()))?;
    let open_ms = t.elapsed().as_secs_f64() * 1000.0;
    if session.len() != 1 {
        return Err(format!(
            "{}: expected 1 page, found {}",
            file.display(),
            session.len()
        ));
    }
    let (w_pt, h_pt) = session
        .page_size(0)
        .ok_or_else(|| format!("{}: page 0 has no media box", file.display()))?;
    let scale = if dpi == 0 { 1.0 } else { f64::from(dpi) / 72.0 };
    let (w, h) = (dim(w_pt * scale), dim(h_pt * scale));
    if w == 0 || h == 0 {
        return Err(format!("{}: zero-area canvas", file.display()));
    }

    // Display-list build (timed separately for the profile split) and the
    // op histogram / glyph count.
    let t = Instant::now();
    let mut g = budget.guard_with(&clock, selis_sandbox::CancelToken::new());
    let dl = session
        .page_display_list(0, &budget, &mut g)
        .map_err(|e| format!("{}: display list: {e}", file.display()))?;
    let dl_ms = t.elapsed().as_secs_f64() * 1000.0;
    let (histogram, glyph_count) = histogram(&dl);

    // Warm-up (settles lazy font/table caches), then timed repeats.
    let mut backend = selis_pdf_engine::TinySkiaBackend::new(w, h)
        .ok_or_else(|| format!("{stem}: cannot create {w}x{h} canvas"))?;
    let mut g = budget.guard_with(&clock, selis_sandbox::CancelToken::new());
    session
        .render_page(0, &mut backend, &budget, &mut g)
        .map_err(|e| format!("{}: warm-up render: {e}", file.display()))?;
    let mut samples: Vec<f64> = Vec::new();
    let mut checksums: Vec<String> = Vec::new();
    let mut stats_of_first: Option<selis_pdf_engine::RenderStats> = None;
    for i in 0..repeats {
        let mut backend = selis_pdf_engine::TinySkiaBackend::new(w, h)
            .ok_or_else(|| format!("{stem}: cannot create {w}x{h} canvas"))?;
        let mut g = budget.guard_with(&clock, selis_sandbox::CancelToken::new());
        let mut stats = selis_pdf_engine::RenderStats::default();
        let t = Instant::now();
        session
            .render_page_with_stats(0, &mut backend, &budget, &mut g, &mut stats)
            .map_err(|e| format!("{}: render: {e}", file.display()))?;
        samples.push(t.elapsed().as_secs_f64() * 1000.0);
        checksums.push(checksum(backend.pixmap().data()));
        // The workload counters must agree across repeats too: a walk that
        // resolves a different number of fonts on its second run is as
        // non-deterministic as one that paints different pixels.
        if i == 0 {
            stats_of_first = Some(stats);
        } else if stats_of_first.as_ref() != Some(&stats) {
            return Err(format!(
                "{stem}: repeat {i} counters differ: renderer non-deterministic"
            ));
        }
    }
    let first = checksums.first().cloned().unwrap_or_default();
    for (i, c) in checksums.iter().enumerate() {
        if c != &first {
            return Err(format!(
                "{stem}: repeat {i} pixmap checksum {c} != {first}: renderer non-deterministic"
            ));
        }
    }

    // Oracles: one untimed warm-up, then timed repeats (spawn-inclusive).
    let mut oracle_ms: BTreeMap<String, f64> = BTreeMap::new();
    for (id, bin) in oracle_bins {
        let out_path = tmp.join(format!("{stem}-{id}.png"));
        let _ = run_oracle(id, bin, dpi, file, &out_path); // warm-up
        let mut samples: Vec<f64> = Vec::new();
        for _ in 0..repeats {
            let t = Instant::now();
            run_oracle(id, bin, dpi, file, &out_path)?;
            samples.push(t.elapsed().as_secs_f64() * 1000.0);
        }
        let _ = std::fs::remove_file(&out_path);
        oracle_ms.insert(id.clone(), median_ms(samples));
    }

    let mut counters: BTreeMap<String, u64> = BTreeMap::new();
    if let Some(stats) = stats_of_first {
        counters.insert("ops".to_string(), stats.ops);
        counters.insert("font_resolves".to_string(), stats.font_resolves);
        counters.insert("font_bytes".to_string(), stats.font_bytes);
        counters.insert("glyph_lookups".to_string(), stats.glyph_lookups);
        counters.insert("glyph_outlines".to_string(), stats.glyph_outlines);
        counters.insert("image_draws".to_string(), stats.image_draws);
        counters.insert("image_bytes".to_string(), stats.image_bytes);
        counters.insert("clip_rebuilds".to_string(), stats.clip_rebuilds);
        counters.insert("clip_paths".to_string(), stats.clip_paths);
        counters.insert("smask_resolves".to_string(), stats.smask_resolves);
        counters.insert("shading_resolves".to_string(), stats.shading_resolves);
        counters.insert("pattern_resolves".to_string(), stats.pattern_resolves);
        counters.insert("layers_pushed".to_string(), stats.layers_pushed);
        counters.insert("layers_popped".to_string(), stats.layers_popped);
        counters.insert("font_cache_hits".to_string(), stats.font_cache_hits);
        counters.insert("gid_cache_hits".to_string(), stats.gid_cache_hits);
        counters.insert("outline_cache_hits".to_string(), stats.outline_cache_hits);
    }

    Ok(PageResult {
        file: file
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("?")
            .to_string(),
        axis: axis_of(stem).to_string(),
        op_count: dl.len(),
        histogram,
        glyph_count,
        open_ms,
        dl_ms,
        render_ms: median_ms(samples),
        checksum: first,
        counters,
        oracle_ms,
    })
}

/// Count ops per variant and total glyphs (the profile's workload shape).
fn histogram(
    dl: &selis_pdf_content::display_list::DisplayList,
) -> (BTreeMap<&'static str, usize>, usize) {
    let mut map: BTreeMap<&'static str, usize> = BTreeMap::new();
    let mut glyphs = 0usize;
    for op in &dl.ops {
        let key = match op {
            Op::Fill { .. } => "fill",
            Op::Stroke { .. } => "stroke",
            Op::FillStroke { .. } => "fill_stroke",
            Op::Text { runs, .. } => {
                glyphs = glyphs.saturating_add(runs.iter().map(|r| r.glyphs.len()).sum());
                "text"
            }
            Op::Image { .. } => "image",
            Op::InlineImage { .. } => "inline_image",
            Op::Shading { .. } => "shading",
            Op::PushLayer { .. } => "push_layer",
            Op::PopLayer => "pop_layer",
        };
        map.insert(key, map.get(key).copied().unwrap_or(0).saturating_add(1));
    }
    (map, glyphs)
}

/// A wrapping FNV-1a hash of the pixmap (determinism tripwire, not art).
fn checksum(pixels: &[u8]) -> String {
    let mut h = 0xcbf2_9ce4_8422_2325u64;
    for chunk in pixels.chunks(1024) {
        for &b in chunk {
            h ^= u64::from(b);
            h = h.wrapping_mul(0x0000_0100_0000_01b3);
        }
    }
    format!("{h:016x}")
}

/// A finite, non-negative f64 as a u32 dimension (ceil, saturate).
fn dim(v: f64) -> u32 {
    if !v.is_finite() || v < 0.0 {
        return 0;
    }
    let c = v.ceil();
    if c >= f64::from(u32::MAX) {
        return u32::MAX;
    }
    c as u32
}

/// The cost axis from the file stem (`text-heavy` → `text`).
fn axis_of(stem: &str) -> &str {
    stem.split('-').next().unwrap_or(stem)
}

/// Resolve an oracle binary: the explicit path wins, else PATH lookup; the
/// returned path is canonicalised (absolute) for the spawn. `None` when the
/// tool is not installed — the caller skips it loudly, never silently.
fn resolve_tool(explicit: &Option<PathBuf>, name: &str) -> Option<PathBuf> {
    if let Some(p) = explicit {
        return std::fs::canonicalize(p).ok();
    }
    let probe = if std::env::consts::OS == "windows" {
        format!("{name}.exe")
    } else {
        name.to_string()
    };
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        let cand = dir.join(&probe);
        if cand.is_file() {
            if let Ok(abs) = std::fs::canonicalize(&cand) {
                return Some(abs);
            }
        }
    }
    None
}

/// Run one oracle render, honouring each tool's CLI contract (the same
/// contract `xtask oracle render` uses):
/// `mutool draw -r <dpi> -o <out> <in>` or
/// `pdfium_driver --page 1 --dpi <dpi> <in> <out>`.
fn run_oracle(id: &str, bin: &Path, dpi: u32, file: &Path, out: &Path) -> Result<(), String> {
    let mut cmd = std::process::Command::new(bin);
    match id {
        "mutool" => {
            cmd.arg("draw")
                .arg("-r")
                .arg(dpi.to_string())
                .arg("-o")
                .arg(out)
                .arg(file);
        }
        "pdfium" => {
            cmd.arg("--page")
                .arg("1")
                .arg("--dpi")
                .arg(dpi.to_string())
                .arg(file)
                .arg(out);
        }
        other => return Err(format!("unknown oracle tool `{other}`")),
    }
    let status = cmd
        .output()
        .map_err(|e| format!("{}: spawn: {e}", bin.display()))?;
    if status.status.success() && out.exists() {
        Ok(())
    } else {
        Err(format!(
            "{}: oracle render failed (exit {})",
            bin.display(),
            status.status
        ))
    }
}

/// Time a trivial invocation of an oracle binary (spawn + init, no render).
/// The exit status is irrelevant; only the wall time is measured.
fn measure_spawn(id: &str, bin: &Path) -> Result<(), String> {
    let mut cmd = std::process::Command::new(bin);
    match id {
        "mutool" => {
            cmd.arg("version");
        }
        "pdfium" => {}
        other => return Err(format!("unknown oracle tool `{other}`")),
    }
    cmd.output()
        .map_err(|e| format!("{}: spawn probe: {e}", bin.display()))?;
    Ok(())
}

/// One-line oracle summary for the console table.
fn oracle_summary(page: &PageResult) -> String {
    let mut parts: Vec<String> = Vec::new();
    for (id, ms) in &page.oracle_ms {
        let ratio = if page.render_ms > 0.0 {
            ms / page.render_ms
        } else {
            0.0
        };
        parts.push(format!("{id}={ms:.1}ms (x{ratio:.2})"));
    }
    parts.join(" ")
}

/// Write `bench/render-results.json` (the perf-check ratio record).
fn write_results(
    out: &Path,
    set: &Path,
    dpi: u32,
    repeats: usize,
    pages: &[PageResult],
    spawn_ms: &BTreeMap<String, f64>,
) -> Result<(), String> {
    let mut oracle_ids: Vec<String> = Vec::new();
    for page in pages {
        for id in page.oracle_ms.keys() {
            if !oracle_ids.contains(id) {
                oracle_ids.push(id.clone());
            }
        }
    }
    // Geometric mean of per-page ratios per oracle (the headline numbers).
    let mut geomeans: BTreeMap<String, f64> = BTreeMap::new();
    for id in &oracle_ids {
        let mut log_sum = 0.0f64;
        let mut n = 0u32;
        for page in pages {
            if let Some(ms) = page.oracle_ms.get(id) {
                if page.render_ms > 0.0 && *ms > 0.0 {
                    log_sum += (ms / page.render_ms).ln();
                    n += 1;
                }
            }
        }
        if n > 0 {
            geomeans.insert(id.clone(), (log_sum / f64::from(n)).exp());
        }
    }

    let mut json = String::from("{\n");
    json.push_str("  \"version\": 1,\n");
    json.push_str(&format!("  \"dpi\": {dpi},\n"));
    json.push_str(&format!("  \"repeats\": {repeats},\n"));
    json.push_str(&format!(
        "  \"profile\": \"{}\",\n",
        if cfg!(debug_assertions) {
            "debug"
        } else {
            "release"
        }
    ));
    json.push_str(&format!("  \"set\": \"{}\",\n", set.display()));
    json.push_str("  \"oracle_spawn_ms\": {");
    let mut first = true;
    for (k, v) in spawn_ms {
        if !first {
            json.push_str(", ");
        }
        first = false;
        json.push_str(&format!("\"{k}\": {v:.3}"));
    }
    json.push_str("},\n");
    json.push_str("  \"pages\": [\n");
    for (i, page) in pages.iter().enumerate() {
        json.push_str("    {\n");
        json.push_str(&format!("      \"file\": \"{}\",\n", page.file));
        json.push_str(&format!("      \"axis\": \"{}\",\n", page.axis));
        json.push_str(&format!("      \"op_count\": {},\n", page.op_count));
        json.push_str(&format!("      \"glyph_count\": {},\n", page.glyph_count));
        json.push_str("      \"op_histogram\": {");
        let mut first = true;
        for (k, v) in &page.histogram {
            if !first {
                json.push_str(", ");
            }
            first = false;
            json.push_str(&format!("\"{k}\": {v}"));
        }
        json.push_str("},\n");
        json.push_str(&format!("      \"open_ms\": {:.3},\n", page.open_ms));
        json.push_str(&format!("      \"dl_ms\": {:.3},\n", page.dl_ms));
        json.push_str(&format!("      \"render_ms\": {:.3},\n", page.render_ms));
        json.push_str(&format!("      \"checksum\": \"{}\",\n", page.checksum));
        json.push_str("      \"counters\": {");
        let mut first = true;
        for (k, v) in &page.counters {
            if !first {
                json.push_str(", ");
            }
            first = false;
            json.push_str(&format!("\"{k}\": {v}"));
        }
        json.push_str("},\n");
        json.push_str("      \"oracle_ms\": {");
        let mut first = true;
        for (k, v) in &page.oracle_ms {
            if !first {
                json.push_str(", ");
            }
            first = false;
            json.push_str(&format!("\"{k}\": {v:.3}"));
        }
        json.push_str("},\n");
        json.push_str("      \"ratio\": {");
        let mut first = true;
        for (k, v) in &page.oracle_ms {
            if !first {
                json.push_str(", ");
            }
            first = false;
            let r = if page.render_ms > 0.0 {
                v / page.render_ms
            } else {
                0.0
            };
            json.push_str(&format!("\"{k}\": {r:.4}"));
        }
        json.push_str("}\n");
        json.push_str(if i + 1 < pages.len() {
            "    },\n"
        } else {
            "    }\n"
        });
    }
    json.push_str("  ],\n");
    for id in &oracle_ids {
        let key = format!("geomean_vs_{id}");
        match geomeans.get(id) {
            Some(g) => json.push_str(&format!("  \"{key}\": {g:.4},\n")),
            None => json.push_str(&format!("  \"{key}\": null,\n")),
        }
    }
    json.push_str("  \"note\": \"oracle_ms is spawn-inclusive wall time; selis render_ms is in-process steady-state (open excluded)\"\n");
    json.push_str("}\n");

    if let Some(parent) = out.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", out.display()))?;
        }
    }
    std::fs::write(out, &json).map_err(|e| format!("{}: {e}", out.display()))?;
    println!("perf-render: wrote {}", out.display());
    for id in &oracle_ids {
        if let Some(g) = geomeans.get(id) {
            println!("perf-render: geomean_vs_{id} = {g:.3}x");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]
    use super::*;

    #[test]
    fn median_picks_the_middle() {
        assert!((median_ms(vec![3.0, 1.0, 2.0]) - 2.0).abs() < 1e-12);
        assert!((median_ms(vec![5.0]) - 5.0).abs() < 1e-12);
        assert!((median_ms(vec![]) - 0.0).abs() < 1e-12);
    }

    #[test]
    fn checksum_is_stable_and_sensitive() {
        let a = checksum(&[1, 2, 3, 4]);
        assert_eq!(a, checksum(&[1, 2, 3, 4]));
        assert_ne!(a, checksum(&[1, 2, 3, 5]));
    }

    #[test]
    fn axis_of_splits_on_dash() {
        assert_eq!(axis_of("text-heavy"), "text");
        assert_eq!(axis_of("mixed"), "mixed");
    }

    #[test]
    fn unknown_tool_is_rejected_by_name() {
        // resolve_tool only probes; the name check lives in run(). The probe
        // of a nonsense binary returns None rather than spawning anything.
        assert!(resolve_tool(&None, "selis-no-such-oracle-xyz").is_none());
    }
}
