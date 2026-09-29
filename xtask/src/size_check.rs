//! `xtask size-check` — SL-0.WS.09.
//!
//! Builds the WASM target, optimises with wasm-opt, measures the
//! brotli-compressed size of each linked artifact, and compares every
//! measurement against two limits:
//!
//! * the absolute budget in `xtask/size-budgets.toml` (ADR-P0011);
//! * the 2% regression rule against the last recorded measurement in
//!   `xtask/size-baseline.json`.
//!
//! A budget whose artifact is not measured yet fails as *not measured* —
//! never as passing. `--update-baseline` re-records the baseline for
//! deliberate size changes.

use std::collections::BTreeMap;
use std::path::Path;

const BUDGETS: &str = "xtask/size-budgets.toml";
const BASELINE: &str = "xtask/size-baseline.json";

/// The regression threshold from the task Do: a measured size more than this
/// percentage above the baseline fails the check.
pub(crate) const MAX_REGRESSION_PCT: f64 = 2.0;

/// One budget row from `xtask/size-budgets.toml`.
#[derive(serde::Deserialize, Debug, Clone)]
struct BudgetRow {
    /// Human-readable chunk name (reported in failures).
    #[allow(dead_code)]
    chunk: String,
    /// The artifact this budget is evaluated against.
    artifact: String,
    /// Maximum brotli-compressed size in bytes.
    budget: u64,
}

/// The `[[budget]]` array at the root of `xtask/size-budgets.toml`.
#[derive(serde::Deserialize, Debug, Clone)]
struct BudgetsFile {
    budget: Vec<BudgetRow>,
}

/// The last measured artifact sizes, persisted to `xtask/size-baseline.json`
/// and committed so CI can enforce the regression rule (SL-0.WS.09). Keyed by
/// artifact name, values are brotli-compressed bytes.
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct SizeBaseline(BTreeMap<String, u64>);

impl SizeBaseline {
    /// The recorded size for `artifact`, if any.
    fn get(&self, artifact: &str) -> Option<u64> {
        self.0.get(artifact).copied()
    }

    /// Record or replace the size of `artifact`.
    fn set(&mut self, artifact: &str, brotli_bytes: u64) {
        self.0.insert(artifact.to_string(), brotli_bytes);
    }
}

/// The outcome of comparing one measured artifact against its budget and the
/// baseline. `NotMeasured` exists so unmeasurable chunks fail loudly instead
/// of silently passing the gate.
#[derive(Debug, PartialEq)]
pub(crate) enum SizeVerdict {
    /// Within budget and within the regression threshold.
    Ok,
    /// Over the absolute budget.
    OverBudget { budget: u64 },
    /// Over the baseline by more than [`MAX_REGRESSION_PCT`].
    Regressed { baseline: u64, pct: f64 },
    /// The artifact was not part of this run; it cannot pass.
    NotMeasured,
}

/// Decide the verdict for one artifact: budget first, then regression.
pub(crate) fn verdict_for(
    _artifact: &str,
    measured: Option<u64>,
    budget: u64,
    baseline: Option<u64>,
) -> SizeVerdict {
    let Some(size) = measured else {
        return SizeVerdict::NotMeasured;
    };
    if size > budget {
        return SizeVerdict::OverBudget { budget };
    }
    match baseline {
        Some(base) => {
            let pct = regression_pct(size, base);
            if pct > MAX_REGRESSION_PCT {
                SizeVerdict::Regressed {
                    baseline: base,
                    pct,
                }
            } else {
                SizeVerdict::Ok
            }
        }
        // No baseline yet: the absolute budget is the gate; this run records
        // the first baseline entry.
        None => SizeVerdict::Ok,
    }
}

/// Percentage change of `new` over `base`, positive when larger. A zero
/// baseline is treated as infinite regression (any positive size fails).
pub(crate) fn regression_pct(new: u64, base: u64) -> f64 {
    if base == 0 {
        return if new == 0 { 0.0 } else { f64::INFINITY };
    }
    let new = new as f64;
    let base = base as f64;
    (new - base) / base * 100.0
}

/// Check that the WASM target fits within the size budgets and did not
/// regress more than [`MAX_REGRESSION_PCT`] against the recorded baseline.
///
/// With `update_baseline`, the fresh measurements replace the baseline
/// instead of being compared against it (a deliberate accept of size drift).
/// With `strict`, budgets whose artifact is not measured yet also fail the
/// run; by default they are reported loudly but do not fail, so the PR gate
/// stays actionable while chunk artifacts do not exist yet.
pub fn run(update_baseline: bool, strict: bool) -> Result<(), String> {
    let text = std::fs::read_to_string(BUDGETS).map_err(|e| format!("{BUDGETS}: {e}"))?;
    let budgets: BudgetsFile = toml::from_str(&text).map_err(|e| format!("{BUDGETS}: {e}"))?;
    let budgets = budgets.budget;
    let measured = measure_artifacts()?;

    let mut baseline = if update_baseline {
        SizeBaseline::default()
    } else {
        load_baseline()?
    };

    let mut failures = Vec::new();
    for row in &budgets {
        let size = measured.get(&row.artifact).copied();
        let base = baseline.get(&row.artifact);
        match verdict_for(&row.artifact, size, row.budget, base) {
            SizeVerdict::Ok => {
                let base_txt = base
                    .map(|b| b.to_string())
                    .unwrap_or_else(|| "none".to_string());
                let size_txt = size
                    .map(|s| s.to_string())
                    .unwrap_or_else(|| "-".to_string());
                println!(
                    "  {:26} {size_txt:>8} bytes — budget {} ok (baseline {base_txt})",
                    row.artifact, row.budget
                );
            }
            SizeVerdict::OverBudget { budget } => {
                let size_txt = size
                    .map(|s| s.to_string())
                    .unwrap_or_else(|| "-".to_string());
                failures.push(format!(
                    "  {} [{}]: {size_txt} > budget {budget}",
                    row.chunk, row.artifact
                ));
            }
            SizeVerdict::Regressed { baseline: b, pct } => {
                let size_txt = size
                    .map(|s| s.to_string())
                    .unwrap_or_else(|| "-".to_string());
                failures.push(format!(
                    "  {} [{}]: {size_txt} regressed {pct:+.2}% (>{}% vs baseline {b})",
                    row.chunk, row.artifact, MAX_REGRESSION_PCT
                ));
            }
            SizeVerdict::NotMeasured => {
                let msg = format!(
                    "  {} [{}]: NOT MEASURED — no artifact in this run (budget {})",
                    row.chunk, row.artifact, row.budget
                );
                if strict {
                    failures.push(msg);
                } else {
                    println!("  WARNING {msg}");
                }
            }
        }
        if let Some(size) = size {
            baseline.set(&row.artifact, size);
        }
    }

    if update_baseline {
        write_baseline(&baseline)?;
        println!("size-check: baseline updated ({BASELINE})");
    }

    println!(
        "wasm size-check: {} artifact(s) measured, {} budget(s) checked",
        measured.len(),
        budgets.len()
    );
    if failures.is_empty() {
        println!("size-check: all budgets met");
        Ok(())
    } else {
        println!("size-check FAILED:");
        for f in &failures {
            println!("{f}");
        }
        Err("size budgets or regression rule violated".to_string())
    }
}

/// The release-profile overrides that make the measured `.wasm` the one a
/// browser would actually be served.
///
/// ## Why these are `--config` flags and not a `[profile]` entry in Cargo.toml
///
/// Both obvious alternatives were tried on this workspace and **neither
/// worked**, silently, which is the part worth recording:
///
/// - `[profile.release.target.'cfg(target_arch = "wasm32")']` — cargo rejects
///   the `cfg()` form outright: `warning: unused manifest key:
///   profile.release.target`. The build then produces a thin-LTO artifact.
/// - `[profile.release.target.wasm32-unknown-unknown]` — the `cfg()` warning
///   disappears, cargo 1.94 accepts the key without complaint, and the build
///   is *still* byte-for-byte identical to the thin-LTO artifact
///   (5 534 846 B, the same as before). It reads as though it took effect.
///
/// That second one is the dangerous failure: a manifest that looks right,
/// produces no warning, and quietly does nothing. The only reason it was
/// caught is that the artifact size was compared against a known fat-LTO
/// build (4 890 021 B) rather than assumed. **Verify a size claim against a
/// number, not against the absence of an error.**
///
/// `--config` on the command line is the form that provably works: it takes
/// the same `profile.*` values and cargo applies them for real. It is also
/// the better shape regardless, because it scopes the expensive settings to
/// the one command that measures a browser artifact.
///
/// ## Why fat LTO at all
///
/// `profile.release` is `lto = "thin"` and deliberately stays that way: a
/// workspace-wide fat LTO measured 11m53s on this machine, most of it
/// linking `xtask` against cranelift, and none of the native binaries are
/// ever downloaded by a browser. The `.wasm` is a closed world — nothing is
/// linked into it the engine does not call — so cross-crate inlining gets to
/// see the whole program at once. Measured on the core module, thin → fat
/// plus `codegen-units = 1`, after `wasm-opt -O3` and the custom-section
/// strip:
///
///   raw    4 375 551 -> 3 985 067   (-8.9%)
///   brotli 1 382 812 -> 1 308 009   (-5.4%, the number a user pays)
///
/// The `code` section alone drops 15.6%; the rest is inlining across the
/// engine / font / filter boundaries.
///
/// `panic` is left inherited (`unwind`) and is NOT set to `abort`:
/// SL-0.ERR.03's trampoline needs unwinding, which is why that size lever
/// stays off the table.
const FAT_LTO_ARGS: &[&str] = &[
    "--config",
    "profile.release.lto=\"fat\"",
    "--config",
    "profile.release.codegen-units=1",
];

/// Build every wasm-producing target and measure the brotli size of the
/// optimised module of each resulting artifact. Returns artifact name →
/// brotli bytes. SL-4.WASM.02: the 6-chunk split (core + jpx/cjk/ocr/convert/editor)
/// plus the `xtask` canary are each built explicitly so the gate never
/// silently passes a missing chunk (every budget row must be measured).
fn measure_artifacts() -> Result<BTreeMap<String, u64>, String> {
    // SL-4.WASM.02 chunks — each is a separate `cdylib` (separate `.wasm`).
    const WASM_PACKAGES: &[&str] = &[
        "selis-pdf-wasm",
        "selis-pdf-wasm-jpx",
        "selis-pdf-wasm-cjk",
        "selis-pdf-wasm-ocr",
        "selis-pdf-wasm-convert",
        "selis-pdf-wasm-editor",
        "xtask",
    ];
    for pkg in WASM_PACKAGES {
        let status = std::process::Command::new("cargo")
            .args([
                "build",
                "-p",
                pkg,
                "--target",
                "wasm32-unknown-unknown",
                "--release",
            ])
            // Fat LTO, scoped to *this* build only. See FAT_LTO_ARGS below for
            // why it arrives as --config rather than a profile override.
            .args(FAT_LTO_ARGS)
            .status()
            .map_err(|e| format!("cargo build -p {pkg} --target wasm32: {e}"))?;
        if !status.success() {
            return Err(format!("wasm build failed for package {pkg}"));
        }
    }

    // Optimise with wasm-opt. On Windows the npm-installed binary is a `.cmd`
    // wrapper, not an `.exe`, so resolve it explicitly.
    let wasm_opt = if std::env::consts::OS == "windows" {
        "wasm-opt.cmd".to_string()
    } else {
        "wasm-opt".to_string()
    };

    let wasm_dir = std::env::var_os("CARGO_TARGET_DIR")
        .map(|d| Path::new(&d).join("wasm32-unknown-unknown/release"))
        .unwrap_or_else(|| Path::new("target/wasm32-unknown-unknown/release").to_path_buf());
    let mut out = BTreeMap::new();
    let entries = std::fs::read_dir(&wasm_dir).map_err(|e| format!("{wasm_dir:?}: {e}"))?;
    for entry in entries.filter_map(|e| e.ok()) {
        let path = entry.path();
        let is_fresh_wasm = path.extension().is_some_and(|x| x == "wasm")
            && !path.to_string_lossy().ends_with(".opt.wasm");
        if !is_fresh_wasm {
            continue;
        }
        let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        let opt_out = path.with_extension("opt.wasm");
        let status = std::process::Command::new(&wasm_opt)
            .arg("-O3")
            .arg(&path)
            .arg("-o")
            .arg(&opt_out)
            .status()
            .map_err(|e| format!("wasm-opt: {e}"))?;
        if !status.success() {
            return Err(
                "wasm-opt failed. Install with `cargo install wasm-opt` or `npm i -g wasm-opt`."
                    .to_string(),
            );
        }
        // The strip writes the file back, so the path now *is* the stripped
        // artifact; `brotli_compress` measures a file because it shells out to
        // Node's zlib, and re-encoding the bytes in Rust would measure a
        // different compressor than the one that serves this in production.
        strip_custom_sections(&opt_out)?;
        let compressed = brotli_compress(&opt_out)?;
        out.insert(stem.to_string(), compressed.len() as u64);
        let raw = std::fs::metadata(&opt_out)
            .map_err(|e| format!("{opt_out:?}: {e}"))?
            .len();
        println!(
            "  measured {:26} raw {} → brotli {} (custom sections stripped)",
            stem,
            raw,
            compressed.len()
        );
    }
    if out.is_empty() {
        return Err("no wasm artifacts found after the build".to_string());
    }
    Ok(out)
}

/// Load the recorded baseline; empty when the file does not exist yet.
fn load_baseline() -> Result<SizeBaseline, String> {
    if !Path::new(BASELINE).exists() {
        return Ok(SizeBaseline::default());
    }
    let text = std::fs::read_to_string(BASELINE).map_err(|e| format!("{BASELINE}: {e}"))?;
    serde_json::from_str(&text).map_err(|e| format!("{BASELINE}: {e}"))
}

/// Persist the baseline in a stable, reviewable form.
fn write_baseline(baseline: &SizeBaseline) -> Result<(), String> {
    let json = serde_json::to_string_pretty(baseline).map_err(|e| format!("serialize: {e}"))?;
    std::fs::write(BASELINE, format!("{json}\n")).map_err(|e| format!("{BASELINE}: {e}"))
}

/// brotli-compress a file (shared with `cjk-build`: same node toolchain as
/// the CI size job, same measured quantity as the size budgets).
/// Drop every **custom** section from a `.wasm`, and write the result back.
///
/// ## Why this exists
///
/// `wasm-opt` does not remove the `__wasm_bindgen_unstable` schema section, and
/// the built-in strip flags do not reach it either: measured on the core module,
/// `-O3 --strip-debug --strip-producers --strip-target-features` came out
/// 1 698 B *larger* than no flags at all. `wasm-tools strip` does remove it —
/// 174 720 B on the same input — and a custom section is by definition metadata
/// the engine never reads at instantiation, so it is pure transfer cost.
///
/// It is done here rather than by shelling out to `wasm-tools` so the build
/// keeps working with only the tooling it already documents as required
/// (`wasm-opt`), instead of adding a second binary to every contributor's and
/// CI's PATH for a 40-line transformation.
///
/// ## What it must not do
///
/// Only section id `0` is dropped. The magic header, the version, and every
/// typed section are copied byte-for-byte, so the result is the same module
/// with metadata removed — verified with `wasm-tools validate` and by the
/// unchanged import/export counts in the size report. An unparseable input is
/// an error rather than a silent passthrough, because a passthrough would
/// quietly keep shipping the section this exists to remove.
///
/// On the wire the win is much smaller than the raw figure suggests, and that
/// is worth knowing before anyone chases it again: the section is a table of
/// repetitive symbol names, so brotli already compresses it well. 174 720 B of
/// raw is 24 185 B of transfer.
pub(crate) fn strip_custom_sections(path: &Path) -> Result<Vec<u8>, String> {
    const WASM_MAGIC: [u8; 8] = [0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00];

    let input = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    if input.len() < 8 || input[..8] != WASM_MAGIC {
        return Err(format!(
            "{}: not a wasm module (bad magic); refusing to strip",
            path.display()
        ));
    }

    let mut out = input[..8].to_vec();
    let mut at = 8usize;
    while at < input.len() {
        let id = input[at];
        at += 1;
        let (size, used) = read_leb128(&input[at..])
            .ok_or_else(|| format!("{}: truncated section size at {at}", path.display()))?;
        at += used;
        let end = at
            .checked_add(size as usize)
            .filter(|end| *end <= input.len())
            .ok_or_else(|| format!("{}: section at {at} runs past end of file", path.display()))?;
        if id != 0 {
            out.push(id);
            out.extend_from_slice(&encode_leb128(size));
            out.extend_from_slice(&input[at..end]);
        }
        at = end;
    }
    std::fs::write(path, &out).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(out)
}

/// Read an unsigned LEB128, returning the value and how many bytes it used.
fn read_leb128(bytes: &[u8]) -> Option<(u64, usize)> {
    let mut value = 0u64;
    let mut shift = 0u32;
    for (index, byte) in bytes.iter().enumerate() {
        if shift >= 64 {
            return None;
        }
        value |= u64::from(byte & 0x7f) << shift;
        if byte & 0x80 == 0 {
            return Some((value, index + 1));
        }
        shift += 7;
    }
    None
}

/// Encode an unsigned LEB128.
fn encode_leb128(mut value: u64) -> Vec<u8> {
    let mut out = Vec::new();
    loop {
        let byte = (value & 0x7f) as u8;
        value >>= 7;
        if value == 0 {
            out.push(byte);
            return out;
        }
        out.push(byte | 0x80);
    }
}

pub(crate) fn brotli_compress(path: &Path) -> Result<Vec<u8>, String> {
    // Use Node.js zlib's brotli from the command line. The compressed bytes
    // go to a temp file, not stdout — writing a large buffer to a pipe-backed
    // stdout can hit EAGAIN on the runner (observed with the ~7 MB wasm
    // artifact); a regular file cannot.
    //
    // The name is unique per call, and that is load-bearing: `cjk-build` calls
    // this once per emitted file, and the test suite builds several payloads
    // in parallel threads. A single shared name let one call read another's
    // bytes — mid-write, so the read failed and the caller recorded a size of
    // 0, or it read a *different* file's compressed length, which is worse
    // because it is silently wrong. The sequence number is what makes the
    // calls independent; the pid keeps two test binaries apart.
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let seq = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let out_path = std::env::temp_dir().join(format!(
        "selis-size-brotli-{}-{seq}.bin",
        std::process::id()
    ));
    let script = format!(
        "const z=require('node:zlib');const fs=require('fs');\
         const d=fs.readFileSync('{src}');\
         fs.writeFileSync('{dst}',z.brotliCompressSync(d));",
        src = path.display().to_string().replace('\\', "\\\\"),
        dst = out_path.display().to_string().replace('\\', "\\\\"),
    );
    let out = std::process::Command::new("node")
        .args(["-e", &script])
        .output()
        .map_err(|e| format!("node brotli: {e}"));
    let out = match out {
        Ok(out) => out,
        Err(e) => {
            let _ = std::fs::remove_file(&out_path);
            return Err(e);
        }
    };
    if !out.status.success() {
        let _ = std::fs::remove_file(&out_path);
        let stderr = String::from_utf8_lossy(&out.stderr);
        return Err(format!("node brotli compression failed: {stderr}"));
    }
    let bytes = std::fs::read(&out_path).map_err(|e| format!("{}: {e}", out_path.display()));
    let _ = std::fs::remove_file(&out_path);
    bytes
}

#[cfg(test)]
mod tests {
    #![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]
    use super::*;

    // ---- custom-section stripping ----

    /// A minimal module: magic + one typed section + one custom section.
    fn module_with_a_custom_section() -> Vec<u8> {
        let mut wasm = vec![0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00];
        // Type section (id 1), one-byte body.
        wasm.extend_from_slice(&[1, 1, 0]);
        // Custom section (id 0). Its body is a 1-byte name length, the 4 name
        // bytes, then the payload — so the declared size is built from those
        // parts rather than hand-counted, because a fixture that lies about
        // its own size is testing a malformed module, not the strip.
        let mut body = vec![4, b'n', b'a', b'm', b'e'];
        body.extend_from_slice(&[5, 1, 2, 3, 4, 5]);
        wasm.push(0);
        wasm.extend_from_slice(&encode_leb128(body.len() as u64));
        wasm.extend_from_slice(&body);
        wasm
    }

    #[test]
    fn strip_removes_the_custom_section_and_keeps_the_typed_ones() {
        let dir = std::env::temp_dir().join("selis-strip-keeps");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("m.wasm");
        let before = module_with_a_custom_section();
        std::fs::write(&path, &before).unwrap();

        let out = strip_custom_sections(&path).unwrap();

        // Exactly the magic and the type section, in order.
        let mut expected = before[..8].to_vec();
        expected.extend_from_slice(&[1, 1, 0]);
        assert_eq!(out, expected);
        assert!(
            !out.windows(4).any(|w| w == b"name"),
            "custom name survived"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn strip_is_written_back_so_the_shipped_artifact_is_the_stripped_one() {
        // The caller measures the file on disk, so returning stripped bytes
        // without writing them would report a size the artifact does not have.
        let dir = std::env::temp_dir().join("selis-strip-writeback");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("m.wasm");
        std::fs::write(&path, module_with_a_custom_section()).unwrap();
        let out = strip_custom_sections(&path).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), out);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn strip_refuses_a_file_that_is_not_wasm() {
        // A silent passthrough here would quietly keep shipping exactly the
        // section this exists to remove.
        let dir = std::env::temp_dir().join("selis-strip-notwasm");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("m.wasm");
        std::fs::write(&path, b"this is not a wasm module at all").unwrap();
        let err = strip_custom_sections(&path).unwrap_err();
        assert!(err.contains("not a wasm module"), "{err}");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn leb128_round_trips_across_the_multi_byte_boundary() {
        // 127 is the last value that fits one byte; 128 and 624 485 need two
        // and three, and a strip that mis-encoded a size would corrupt the
        // module rather than fail.
        for value in [0u64, 1, 127, 128, 300, 16_383, 16_384, 624_485, 1_000_000] {
            let encoded = encode_leb128(value);
            let (decoded, used) = read_leb128(&encoded).unwrap();
            assert_eq!(decoded, value, "value {value}");
            assert_eq!(used, encoded.len(), "length for {value}");
        }
    }

    #[test]
    fn leb128_rejects_a_truncated_encoding() {
        assert!(read_leb128(&[0x80, 0x80]).is_none());
        assert!(read_leb128(&[]).is_none());
    }

    // ---- regression math ----

    #[test]
    fn regression_pct_zero_change_is_zero() {
        assert!(regression_pct(100, 100).abs() < 1e-9);
    }

    #[test]
    fn regression_pct_increase_and_decrease() {
        assert!((regression_pct(110, 100) - 10.0).abs() < 1e-9);
        assert!((regression_pct(90, 100) + 10.0).abs() < 1e-9);
    }

    #[test]
    fn regression_pct_zero_baseline_is_infinite_for_any_size() {
        assert!(regression_pct(1, 0).is_infinite());
        assert_eq!(regression_pct(0, 0), 0.0);
    }

    // ---- verdicts ----

    #[test]
    fn missing_measurement_is_never_ok() {
        assert_eq!(verdict_for("a", None, 100, None), SizeVerdict::NotMeasured);
        assert_eq!(
            verdict_for("a", None, 100, Some(50)),
            SizeVerdict::NotMeasured
        );
    }

    #[test]
    fn over_absolute_budget_fails_even_if_improved() {
        assert_eq!(
            verdict_for("a", Some(150), 100, Some(160)),
            SizeVerdict::OverBudget { budget: 100 }
        );
    }

    #[test]
    fn regression_boundary_is_exclusive() {
        // Exactly +2% is allowed; anything beyond fails.
        assert_eq!(verdict_for("a", Some(102), 200, Some(100)), SizeVerdict::Ok);
        assert_eq!(
            verdict_for("a", Some(103), 200, Some(100)),
            SizeVerdict::Regressed {
                baseline: 100,
                pct: 3.0
            }
        );
    }

    #[test]
    fn decrease_and_small_increase_pass() {
        assert_eq!(verdict_for("a", Some(90), 200, Some(100)), SizeVerdict::Ok);
        assert_eq!(verdict_for("a", Some(101), 200, Some(100)), SizeVerdict::Ok);
    }

    #[test]
    fn first_run_with_no_baseline_uses_only_the_budget() {
        assert_eq!(verdict_for("a", Some(120), 200, None), SizeVerdict::Ok);
        assert_eq!(
            verdict_for("a", Some(250), 200, None),
            SizeVerdict::OverBudget { budget: 200 }
        );
    }

    // ---- baseline persistence ----

    #[test]
    fn baseline_round_trips_through_json() {
        let mut b = SizeBaseline::default();
        b.set("xtask", 158_453);
        b.set("core-parser-cdylib", 102_500);
        let json = serde_json::to_string_pretty(&b).expect("serialize");
        let back: SizeBaseline = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back, b);
        assert_eq!(back.get("xtask"), Some(158_453));
        assert_eq!(back.get("absent"), None);
    }
}
