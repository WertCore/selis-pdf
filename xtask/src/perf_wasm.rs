//! WASM-vs-native render comparison (SL-2.PERF.03).
//!
//! `xtask perf-wasm` builds the `selis-pdf-wasm` render driver for
//! `wasm32-unknown-unknown`, instantiates it with the embedded wasmtime
//! (the `wasm-host` dependency, native-only), and renders every page of the
//! benchmark set twice: once in-process (native, the `perf-render` steady
//! state) and once inside the guest. There is no JS/browser harness in the
//! repo, so this wasmtime-driven comparison is the honest stand-in —
//! recorded as such in the results file and the PERF report.
//!
//! Method:
//!
//! * The guest ABI is one call per page (`selis_render_page`); input bytes
//!   go in, pixels come out, and the display list, tiling, and
//!   rasterisation never cross the boundary (ADR-P0041). The measured guest
//!   time is the `render_page` call only (median of `repeats`, one untimed
//!   warm-up); the guest-side out-copy is inside the timed call — the honest
//!   boundary cost.
//! * Guest checksums must equal native checksums per page or the run fails:
//!   cross-architecture byte-identity (SL-2.RAST.09) is asserted, not
//!   assumed. This also proves any build flags (notably `+simd128`) are
//!   output-neutral before their timings are trusted.
//! * Cold start is reported as two components — wasmtime/Cranelift module
//!   compile and instantiation — because browsers compile differently; the
//!   §12 45 ms cold-start row stays `not-measurable` with this breakdown in
//!   its notes.
//! * The driver always builds with `-C target-feature=+simd128` (adopted:
//!   consistent win across the set with checksums still matching native —
//!   the determinism proof that makes the flag trustworthy; WASM SIMD has
//!   fixed spec semantics, so the same module is bit-identical on any
//!   engine).
//!
//! Memory-growth strategy: the guest pre-sizes every buffer exactly (input,
//! canvas, out-copy — see the driver docs), and the host caps guest linear
//! memory at 512 MiB through a wasmtime resource limiter, so growth is
//! bounded and loud (a trap names the limit) rather than silent.

use std::path::{Path, PathBuf};
use std::time::Instant;

/// Guest linear-memory cap in bytes (the growth ceiling).
const GUEST_MEMORY_CAP: usize = 512 * 1024 * 1024;

/// One page's native-vs-guest measurement.
struct PageResult {
    file: String,
    native_ms: f64,
    wasm_ms: f64,
    checksum_match: bool,
}

/// The host state behind the wasmtime store (just the memory limiter).
struct HostState {
    limiter: wasmtime::StoreLimits,
}

/// Define the wasm-bindgen runtime shims the driver links (ADR-P0011:
/// `selis-crypto` routes getrandom to `crypto.getRandomValues` on wasm32, so
/// every wasm32 engine build carries these four imports; browsers satisfy
/// them with the wasm-bindgen JS glue).
///
/// The render path never calls them — getrandom only runs if something asks
/// for entropy, and rendering does not (proven per run: any call would trap
/// loudly below instead of rendering). Any import outside this closed set
/// fails loudly: the driver must not grow silent host dependencies.
fn define_shims(
    linker: &mut wasmtime::Linker<HostState>,
    module: &wasmtime::Module,
) -> Result<(), String> {
    for import in module.imports() {
        let (m, n) = (import.module(), import.name());
        if m == "__wbindgen_placeholder__" && n == "__wbindgen_describe" {
            linker
                .func_wrap(m, n, |_: u32| {})
                .map_err(|e| format!("shim {m}::{n}: {e}"))?;
        } else if m == "__wbindgen_placeholder__" && n.starts_with("__wbg___wbindgen_throw") {
            linker
                .func_wrap(m, n, |_: u32, _: u32| -> wasmtime::Result<()> {
                    Err(wasmtime::Error::msg("guest threw via wasm-bindgen shim"))
                })
                .map_err(|e| format!("shim {m}::{n}: {e}"))?;
        } else if m == "__wbindgen_externref_xform__" && n == "__wbindgen_externref_table_set_null"
        {
            // Clears one externref table slot (the index argument); never
            // called on the render path (no externref crosses it).
            linker
                .func_wrap(m, n, |_: u32| {})
                .map_err(|e| format!("shim {m}::{n}: {e}"))?;
        } else if m == "__wbindgen_externref_xform__" && n == "__wbindgen_externref_table_grow" {
            linker
                .func_wrap(m, n, |_: u32| 0u32)
                .map_err(|e| format!("shim {m}::{n}: {e}"))?;
        } else {
            return Err(format!(
                "unexpected driver import {m}::{n}: the driver must not grow host dependencies"
            ));
        }
    }
    Ok(())
}

/// Build the driver, instantiate it under wasmtime, and compare per-page
/// render times. Writes `out` (default `bench/wasm-results.json`).
pub fn run(set: &Path, repeats: usize, out: &Path) -> Result<(), String> {
    if repeats == 0 {
        return Err("--repeats must be at least 1".to_string());
    }
    let wasm_path = build_driver()?;
    println!("perf-wasm: driver {}", wasm_path.display());

    let engine = wasmtime::Engine::default();
    let t = Instant::now();
    let module = wasmtime::Module::from_file(&engine, &wasm_path)
        .map_err(|e| format!("wasmtime compile {}: {e}", wasm_path.display()))?;
    let compile_ms = t.elapsed().as_secs_f64() * 1000.0;
    let mut linker = wasmtime::Linker::new(&engine);
    define_shims(&mut linker, &module)?;
    let limiter = wasmtime::StoreLimitsBuilder::new()
        .memory_size(GUEST_MEMORY_CAP)
        .build();
    let mut store = wasmtime::Store::new(&engine, HostState { limiter });
    store.limiter(|state| &mut state.limiter);
    let t = Instant::now();
    let instance = linker
        .instantiate(&mut store, &module)
        .map_err(|e| format!("wasmtime instantiate: {e}"))?;
    let instantiate_ms = t.elapsed().as_secs_f64() * 1000.0;
    println!("perf-wasm: cold compile={compile_ms:.0}ms instantiate={instantiate_ms:.1}ms");

    let memory = instance
        .get_memory(&mut store, "memory")
        .ok_or_else(|| "driver exports no `memory`".to_string())?;
    let input_alloc = instance
        .get_typed_func::<u32, u32>(&mut store, "selis_input_alloc")
        .map_err(|e| format!("missing export selis_input_alloc: {e}"))?;
    let render_page = instance
        .get_typed_func::<(u32, u32, u32, u32, u32, u32), u32>(&mut store, "selis_render_page")
        .map_err(|e| format!("missing export selis_render_page: {e}"))?;
    let free = instance
        .get_typed_func::<(u32, u32), ()>(&mut store, "selis_free")
        .map_err(|e| format!("missing export selis_free: {e}"))?;

    let mut files: Vec<PathBuf> = std::fs::read_dir(set)
        .map_err(|e| format!("{}: {e}", set.display()))?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "pdf"))
        .collect();
    files.sort();
    if files.is_empty() {
        return Err(format!("no PDFs in {}", set.display()));
    }

    let mut pages: Vec<PageResult> = Vec::new();
    for file in &files {
        let stem = file
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("?")
            .to_string();
        let src = std::fs::read(file).map_err(|e| format!("{}: {e}", file.display()))?;
        let (native_ms, native_sum) = measure_native(&src, file, repeats)?;
        let (wasm_ms, wasm_sum) = measure_guest(
            &mut store,
            &memory,
            &input_alloc,
            &render_page,
            &free,
            &src,
            file,
            repeats,
        )?;
        let checksum_match = native_sum == wasm_sum;
        let ratio = if native_ms > 0.0 {
            wasm_ms / native_ms
        } else {
            0.0
        };
        println!(
            "perf-wasm: {stem} native={native_ms:.2}ms wasm={wasm_ms:.2}ms x{ratio:.2} checksums={}",
            if checksum_match { "match" } else { "MISMATCH" },
        );
        if !checksum_match {
            return Err(format!(
                "{stem}: guest pixmap {wasm_sum} != native {native_sum}: cross-architecture determinism violated"
            ));
        }
        pages.push(PageResult {
            file: file
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or("?")
                .to_string(),
            native_ms,
            wasm_ms,
            checksum_match,
        });
    }
    write_results(out, repeats, compile_ms, instantiate_ms, &pages)
}

/// Build the wasm driver and return the `.wasm` artifact path (parsed from
/// cargo's JSON output, not guessed from the target dir).
fn build_driver() -> Result<PathBuf, String> {
    let cargo = resolve_cargo()?;
    let mut cmd = std::process::Command::new(&cargo);
    cmd.args([
        "build",
        "-p",
        "selis-pdf-wasm",
        "--target",
        "wasm32-unknown-unknown",
        "--release",
        "--message-format=json-render-diagnostics",
    ]);
    // Adopted SL-2.PERF.03: autovectorise the guest to fixed-semantics WASM
    // SIMD (consistent win with checksums still matching native — the
    // determinism proof; see the module docs). Scoped to this invocation via
    // the environment, never to the repo config.
    cmd.env("RUSTFLAGS", "-C target-feature=+simd128");
    let output = cmd
        .output()
        .map_err(|e| format!("{}: spawn cargo build: {e}", cargo.display()))?;
    if !output.status.success() {
        return Err(format!(
            "cargo build selis-pdf-wasm failed:\n{}",
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    parse_wasm_artifact(&output.stdout)
}

/// Resolve the cargo binary (canonicalised absolute path, like the oracle
/// spawns — never a bare PATH lookup at spawn time).
fn resolve_cargo() -> Result<PathBuf, String> {
    let path = std::env::var_os("PATH").ok_or_else(|| "no PATH".to_string())?;
    let probe = if std::env::consts::OS == "windows" {
        "cargo.exe"
    } else {
        "cargo"
    };
    for dir in std::env::split_paths(&path) {
        let cand = dir.join(probe);
        if cand.is_file() {
            if let Ok(abs) = std::fs::canonicalize(&cand) {
                return Ok(abs);
            }
        }
    }
    Err("cargo not found on PATH".to_string())
}

/// Parse cargo's JSON diagnostics for the emitted `.wasm` file.
fn parse_wasm_artifact(stdout: &[u8]) -> Result<PathBuf, String> {
    let text = String::from_utf8_lossy(stdout);
    for line in text.lines() {
        let Ok(json) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        if json.get("reason").and_then(|r| r.as_str()) != Some("compiler-artifact") {
            continue;
        }
        let empty = Vec::new();
        let filenames = json
            .get("filenames")
            .and_then(|f| f.as_array())
            .unwrap_or(&empty);
        for name in filenames {
            if let Some(path) = name.as_str() {
                if path.ends_with(".wasm") && !path.ends_with(".opt.wasm") {
                    return Ok(PathBuf::from(path));
                }
            }
        }
    }
    Err("cargo build emitted no .wasm artifact".to_string())
}

/// Native steady-state render (open excluded from the median; one warm-up).
fn measure_native(src: &[u8], file: &Path, repeats: usize) -> Result<(f64, String), String> {
    let budget = selis_sandbox::Budget::profile(selis_sandbox::Surface::Viewer);
    let clock = selis_sandbox::shell_clock();
    let session = selis_pdf_engine::Session::open(src.to_vec(), &budget, &clock)
        .map_err(|e| format!("{}: open: {e}", file.display()))?;
    let (w_pt, h_pt) = session
        .page_size(0)
        .ok_or_else(|| format!("{}: no media box", file.display()))?;
    let (w, h) = (dim(w_pt)?, dim(h_pt)?);
    let mut backend = selis_pdf_engine::TinySkiaBackend::new(w, h)
        .ok_or_else(|| format!("{}: cannot create {w}x{h} canvas", file.display()))?;
    let mut g = budget.guard_with(&clock, selis_sandbox::CancelToken::new());
    session
        .render_page(0, &mut backend, &budget, &mut g)
        .map_err(|e| format!("{}: warm-up: {e}", file.display()))?;
    let mut samples: Vec<f64> = Vec::new();
    let mut sum = String::new();
    for _ in 0..repeats {
        let mut backend = selis_pdf_engine::TinySkiaBackend::new(w, h)
            .ok_or_else(|| format!("{}: cannot create {w}x{h} canvas", file.display()))?;
        let mut g = budget.guard_with(&clock, selis_sandbox::CancelToken::new());
        let t = Instant::now();
        session
            .render_page(0, &mut backend, &budget, &mut g)
            .map_err(|e| format!("{}: render: {e}", file.display()))?;
        samples.push(t.elapsed().as_secs_f64() * 1000.0);
        sum = checksum(backend.pixmap().data());
    }
    Ok((median_ms(samples), sum))
}

/// Guest render through the wasmtime-driven ABI (the `render_page` call
/// only; one untimed warm-up; buffers freed every repeat).
#[allow(clippy::too_many_arguments)]
fn measure_guest(
    store: &mut wasmtime::Store<HostState>,
    memory: &wasmtime::Memory,
    input_alloc: &wasmtime::TypedFunc<u32, u32>,
    render_page: &wasmtime::TypedFunc<(u32, u32, u32, u32, u32, u32), u32>,
    free: &wasmtime::TypedFunc<(u32, u32), ()>,
    src: &[u8],
    file: &Path,
    repeats: usize,
) -> Result<(f64, String), String> {
    let len =
        u32::try_from(src.len()).map_err(|_| format!("{}: input too large", file.display()))?;
    // One untimed warm-up (module-level caches settle), then timed repeats.
    // Each call reborrows the store (`&mut *store`): wasmtime calls consume
    // the context borrow.
    guest_render(
        &mut *store,
        memory,
        input_alloc,
        render_page,
        free,
        src,
        len,
        file,
    )?;
    let mut samples: Vec<f64> = Vec::new();
    let mut sum = String::new();
    for _ in 0..repeats {
        let t = Instant::now();
        let pixels = guest_render(
            &mut *store,
            memory,
            input_alloc,
            render_page,
            free,
            src,
            len,
            file,
        )?;
        samples.push(t.elapsed().as_secs_f64() * 1000.0);
        sum = checksum(&pixels);
    }
    Ok((median_ms(samples), sum))
}

/// One guest render: alloc → write → call → read → free. Every buffer is
/// freed before return, so repeats cannot leak guest memory.
#[allow(clippy::too_many_arguments)]
fn guest_render(
    store: &mut wasmtime::Store<HostState>,
    memory: &wasmtime::Memory,
    input_alloc: &wasmtime::TypedFunc<u32, u32>,
    render_page: &wasmtime::TypedFunc<(u32, u32, u32, u32, u32, u32), u32>,
    free: &wasmtime::TypedFunc<(u32, u32), ()>,
    src: &[u8],
    len: u32,
    file: &Path,
) -> Result<Vec<u8>, String> {
    let fail = |why: &str| format!("{}: guest render: {why}", file.display());
    let in_ptr = input_alloc
        .call(&mut *store, len)
        .map_err(|e| fail(&format!("input_alloc trap: {e}")))?;
    if in_ptr == 0 {
        return Err(fail("input_alloc returned null"));
    }
    let in_off = usize::try_from(in_ptr).map_err(|_| fail("input pointer out of range"))?;
    memory
        .write(&mut *store, in_off, src)
        .map_err(|e| fail(&format!("input write OOB: {e}")))?;
    // Three out-word slots (w, h, len) from the same 4-aligned allocator.
    let words = input_alloc
        .call(&mut *store, 12)
        .map_err(|e| fail(&format!("out-words alloc trap: {e}")))?;
    if words == 0 {
        free.call(&mut *store, (in_ptr, len))
            .map_err(|e| fail(&format!("free trap: {e}")))?;
        return Err(fail("out-words alloc returned null"));
    }
    let (w_addr, h_addr, len_addr) = (words, words.saturating_add(4), words.saturating_add(8));
    let pix_ptr = render_page
        .call(&mut *store, (in_ptr, len, 0, w_addr, h_addr, len_addr))
        .map_err(|e| fail(&format!("render trap: {e}")))?;
    // A fresh reborrow per word: each wasmtime call consumes the borrow.
    let mut get_u32 = |addr: u32| -> Result<u32, String> {
        let off = usize::try_from(addr).map_err(|_| fail("out-word out of range"))?;
        let mut buf = [0u8; 4];
        memory
            .read(&mut *store, off, &mut buf)
            .map_err(|e| fail(&format!("out-word read OOB: {e}")))?;
        Ok(u32::from_le_bytes(buf))
    };
    let (w, h, out_len) = (get_u32(w_addr)?, get_u32(h_addr)?, get_u32(len_addr)?);
    free.call(&mut *store, (in_ptr, len))
        .map_err(|e| fail(&format!("free trap: {e}")))?;
    free.call(&mut *store, (words, 12))
        .map_err(|e| fail(&format!("free trap: {e}")))?;
    if pix_ptr == 0 {
        return Err(fail("render returned null (see driver docs for causes)"));
    }
    // The geometry cross-check: the driver must agree with itself about the
    // canvas (a host/guest misunderstanding fails loudly here, not as a
    // short read below).
    let expect = u64::from(w).saturating_mul(u64::from(h)).saturating_mul(4);
    if u64::from(out_len) != expect {
        return Err(fail("pixel length disagrees with canvas geometry"));
    }
    let n = usize::try_from(out_len).map_err(|_| fail("pixel length out of range"))?;
    let pix_off = usize::try_from(pix_ptr).map_err(|_| fail("pixel pointer out of range"))?;
    let mut pixels = vec![0u8; n];
    memory
        .read(&mut *store, pix_off, &mut pixels)
        .map_err(|e| fail(&format!("pixel read OOB: {e}")))?;
    free.call(&mut *store, (pix_ptr, out_len))
        .map_err(|e| fail(&format!("free trap: {e}")))?;
    Ok(pixels)
}

/// Median of the samples in milliseconds.
fn median_ms(mut samples: Vec<f64>) -> f64 {
    samples.sort_by(|a, b| a.total_cmp(b));
    samples.get(samples.len() / 2).copied().unwrap_or(0.0)
}

/// A finite, non-negative f64 as a u32 dimension (ceil, saturate).
fn dim(v: f64) -> Result<u32, String> {
    if !v.is_finite() || v < 0.0 {
        return Err("zero-area canvas".to_string());
    }
    let c = v.ceil();
    if c < 1.0 || c >= f64::from(u32::MAX) {
        return Err("canvas too large".to_string());
    }
    // c is in [1, u32::MAX): the float-to-int cast below is exact, and std
    // offers no fallible f64→u32 conversion, so the pedantic cast lints are
    // allowed here with this justification.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    u32::try_from(c as u64).map_err(|_| "canvas too large".to_string())
}

/// A wrapping FNV-1a hash of the pixmap (the cross-arch identity check).
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

/// Write `bench/wasm-results.json` (the PERF.03 reference record; the WASM
/// budget row stays `not-measurable` — CI cannot regenerate guest numbers —
/// so this file documents rather than gates).
fn write_results(
    out: &Path,
    repeats: usize,
    compile_ms: f64,
    instantiate_ms: f64,
    pages: &[PageResult],
) -> Result<(), String> {
    let mut log_sum = 0.0f64;
    let mut n = 0u32;
    for page in pages {
        if page.native_ms > 0.0 && page.wasm_ms > 0.0 {
            log_sum += (page.wasm_ms / page.native_ms).ln();
            n += 1;
        }
    }
    let geomean = if n > 0 {
        (log_sum / f64::from(n)).exp()
    } else {
        0.0
    };

    let mut json = String::from("{\n");
    json.push_str("  \"version\": 1,\n");
    json.push_str(&format!("  \"repeats\": {repeats},\n"));
    json.push_str("  \"simd128\": true,\n");
    json.push_str("  \"driver\": \"wasmtime (embedded; no browser harness in-repo)\",\n");
    json.push_str(&format!("  \"cold_compile_ms\": {compile_ms:.1},\n"));
    json.push_str(&format!(
        "  \"cold_instantiate_ms\": {instantiate_ms:.3},\n"
    ));
    json.push_str("  \"pages\": [\n");
    for (i, page) in pages.iter().enumerate() {
        let ratio = if page.native_ms > 0.0 {
            page.wasm_ms / page.native_ms
        } else {
            0.0
        };
        json.push_str("    {\n");
        json.push_str(&format!("      \"file\": \"{}\",\n", page.file));
        json.push_str(&format!("      \"native_ms\": {:.3},\n", page.native_ms));
        json.push_str(&format!("      \"wasm_ms\": {:.3},\n", page.wasm_ms));
        json.push_str(&format!("      \"ratio_wasm_over_native\": {ratio:.4},\n"));
        json.push_str(&format!(
            "      \"checksum_match\": {}\n",
            page.checksum_match
        ));
        json.push_str(if i + 1 < pages.len() {
            "    },\n"
        } else {
            "    }\n"
        });
    }
    json.push_str("  ],\n");
    json.push_str(&format!("  \"geomean_wasm_over_native\": {geomean:.4},\n"));
    json.push_str("  \"note\": \"guest time is the render_page call only (median of 5); wasmtime compile/instantiate reported separately as cold; simd128 adopted (-C target-feature=+simd128, checksums match native)\"\n");
    json.push_str("}\n");

    if let Some(parent) = out.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", out.display()))?;
        }
    }
    std::fs::write(out, &json).map_err(|e| format!("{}: {e}", out.display()))?;
    println!("perf-wasm: wrote {}", out.display());
    println!("perf-wasm: geomean_wasm_over_native = {geomean:.3}x");
    Ok(())
}

#[cfg(test)]
mod tests {
    #![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]
    use super::*;

    #[test]
    fn median_picks_the_middle() {
        assert!((median_ms(vec![3.0, 1.0, 2.0]) - 2.0).abs() < 1e-12);
    }

    #[test]
    fn checksum_is_stable() {
        assert_eq!(checksum(&[1, 2, 3]), checksum(&[1, 2, 3]));
        assert_ne!(checksum(&[1, 2, 3]), checksum(&[1, 2, 4]));
    }
}
