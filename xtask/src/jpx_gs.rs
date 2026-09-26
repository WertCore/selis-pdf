//! SL-1.FILT.08 Ghostscript differential leg (oracle-match clause).
//!
//! Decodes each valid JPEG 2000 fixture two ways and compares them under a
//! documented tolerance:
//!   1. the Tier-2 wasm sandbox path (`selis_pdf_filter::jpx_decode`), and
//!   2. Ghostscript's own JPXDecode, reached by wrapping the codestream in a
//!      minimal PDF image XObject and rendering `ppmraw` at 72 dpi (1 pt = 1 px).
//!
//! This is a TEST ORACLE leg (SL-0.ORACLE.01, ADR-P0009/ADR-P0021):
//! Ghostscript is never linked into the engine, only invoked at the shell
//! boundary. It is compiled only on native hosts that enable the `wasm-host`
//! feature — the wasm32 size-check build of `xtask` must not pull wasmtime.
//!
//! # Tolerance
//!
//! The DoD is "matches Ghostscript within tolerance on the valid set". The
//! numbers are calibrated from measurement (`21-TESTING-AND-ORACLES.md §5`:
//! a tolerance is set to the measured data, never loosened to make a build
//! green). See the FILT.08 note in `pdf-plan/11-PHASE-1-cos.md` for the
//! measured values that set the documented bounds.

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::oracle::{parse_ppm, PpmImage};

/// The GS device command we use. `ppmraw` emits P6 PPM; at `-r72` one
/// PostScript point is one pixel, so a 64x48 pt image is a 64x48 px raster.
const GS_DEVICE: &str = "ppmraw";

/// Resolve the Ghostscript binary: `SELIS_GS` override, else a probe of the
/// usual names (`gswin64c`, `gswin32c`, `gs`). Returns `None` when no GS is
/// installed — the leg then reports SKIP rather than failing the build.
fn resolve_gs() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("SELIS_GS") {
        let path = PathBuf::from(p);
        if path.is_file() {
            return Some(path);
        }
        // `SELIS_GS` may name a bare command on PATH too.
        if let Some(found) = crate::oracle::find_local(path.to_str()?) {
            return Some(found);
        }
    }
    for name in ["gswin64c", "gswin32c", "gs"] {
        if let Some(found) = crate::oracle::find_local(name) {
            return Some(found);
        }
    }
    None
}

/// Build a minimal single-page PDF that paints `codestream` as a
/// `/Filter /JPXDecode` image XObject at its native pixel size (1 pt = 1 px),
/// so GS renders exactly the decoded samples at `ppmraw`/72.
fn wrap_jpx(codestream: &[u8], w: u32, h: u32, chans: u8) -> Vec<u8> {
    let color = match chans {
        1 => "DeviceGray",
        _ => "DeviceRGB",
    };
    let mut out = Vec::with_capacity(1024 + codestream.len());
    out.extend_from_slice(b"%PDF-1.4\n");
    let objs: Vec<Vec<u8>> = vec![
        b"1 0 obj << /Type /Catalog /Pages 2 0 R >> endobj\n".to_vec(),
        b"2 0 obj << /Type /Pages /Kids [3 0 R] /Count 1 >> endobj\n".to_vec(),
        format!(
            "3 0 obj << /Type /Page /Parent 2 0 R /MediaBox [0 0 {w} {h}] \
             /Resources << /XObject << /Im1 5 0 R >> >> /Contents 4 0 R >> endobj\n"
        )
        .into_bytes(),
        {
            let content = format!("q {w} 0 0 {h} 0 0 cm /Im1 Do Q\n");
            format!(
                "4 0 obj << /Length {} >> stream\n{content}endstream endobj\n",
                content.len()
            )
            .into_bytes()
        },
        {
            let mut o = format!(
                "5 0 obj << /Type /XObject /Subtype /Image /Width {w} /Height {h} \
                 /ColorSpace /{color} /BitsPerComponent 8 /Filter /JPXDecode /Length {} >> stream\n",
                codestream.len()
            )
            .into_bytes();
            o.extend_from_slice(codestream);
            o.extend_from_slice(b"\nendstream endobj\n");
            o
        },
    ];
    let mut offsets = Vec::with_capacity(objs.len());
    let mut off = out.len() as u64;
    for o in &objs {
        offsets.push(off);
        off += o.len() as u64;
    }
    for o in objs {
        out.extend_from_slice(&o);
    }
    let xref_start = out.len() as u64;
    let n_objs = offsets.len() + 1;
    out.extend_from_slice(format!("xref\n0 {n_objs}\n0000000000 65535 f \n").as_bytes());
    for o in &offsets {
        out.extend_from_slice(format!("{o:010} 00000 n \n").as_bytes());
    }
    out.extend_from_slice(
        format!("trailer\n<< /Size {n_objs} /Root 1 0 R >>\nstartxref\n{xref_start}\n%%EOF\n")
            .as_bytes(),
    );
    out
}

/// Render a wrapped JPX PDF with GS to a PPM, returning the parsed image.
fn gs_render(gs: &Path, pdf: &[u8], workdir: &Path, tag: &str) -> Result<PpmImage, String> {
    let pdf_path = workdir.join(format!("{tag}.pdf"));
    let ppm_path = workdir.join(format!("{tag}.ppm"));
    std::fs::write(&pdf_path, pdf).map_err(|e| format!("write {pdf_path:?}: {e}"))?;
    let status = Command::new(gs)
        .arg("-q")
        .arg("-dNOPAUSE")
        .arg("-dBATCH")
        .arg("-dSAFER")
        .arg(format!("-sDEVICE={GS_DEVICE}"))
        .arg("-r72")
        .arg("-o")
        .arg(&ppm_path)
        .arg(&pdf_path)
        .output()
        .map_err(|e| format!("cannot run Ghostscript ({gs:?}): {e}"))?;
    if !status.status.success() {
        return Err(format!(
            "Ghostscript render failed (exit {}): {}",
            status.status,
            String::from_utf8_lossy(&status.stderr).trim()
        ));
    }
    let bytes = std::fs::read(&ppm_path).map_err(|e| format!("read {ppm_path:?}: {e}"))?;
    parse_ppm(&bytes)
}

/// Run the compare over every valid `.j2k`/`.jp2` under `path` (a file or a
/// directory). Returns the number of failing fixtures, or an Err on a
/// structural problem (no GS, decode failure).
pub fn run(
    path: &Path,
    fraction_threshold: f64,
    mean_threshold: f64,
    report_only: bool,
) -> Result<(), String> {
    let gs = resolve_gs().ok_or_else(|| {
        "no Ghostscript binary found (set SELIS_GS or install gswin64c/gswin32c/gs on PATH)"
            .to_string()
    })?;
    let gs_version = Command::new(&gs)
        .arg("--version")
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_else(|_| "?".to_string());

    let mut files: Vec<PathBuf> = Vec::new();
    if path.is_dir() {
        let mut entries: Vec<_> = std::fs::read_dir(path)
            .map_err(|e| format!("read dir {:?}: {e}", path.display()))?
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| {
                p.extension()
                    .is_some_and(|x| x.eq_ignore_ascii_case("j2k") || x.eq_ignore_ascii_case("jp2"))
            })
            .collect();
        entries.sort();
        files = entries;
    } else {
        files.push(path.to_path_buf());
    }

    let workdir = std::env::temp_dir().join("selis-jpx-gs");
    std::fs::create_dir_all(&workdir).map_err(|e| format!("{workdir:?}: {e}"))?;

    println!(
        "SL-1.FILT.08 Ghostscript differential (GS {gs_version}, {} fixture(s))",
        files.len()
    );
    println!(
        "  tolerance: pixels within {fraction_threshold:.4} | mean |diff| <= {mean_threshold:.2}"
    );
    println!();

    let mut failures = 0usize;
    for (i, file) in files.iter().enumerate() {
        let name = file
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let data = std::fs::read(file).map_err(|e| format!("{}: {e}", file.display()))?;

        // 1. wasm sandbox decode.
        let mut guard = selis_sandbox::Budget::unlimited().guard();
        let wasm = selis_pdf_filter::jpx_decode(&data, &mut guard)
            .map_err(|e| format!("{}: wasm decode failed: {e}", file.display()))?;
        let (w, h, chans) = (wasm.width, wasm.height, wasm.channels);

        // 2. GS decode via a wrapped PDF.
        let pdf = wrap_jpx(&data, w, h, chans);
        let gs_img = gs_render(&gs, &pdf, &workdir, &format!("f{i:02}"))?;
        if gs_img.width != w || gs_img.height != h {
            return Err(format!(
                "{name}: GS produced {}x{} but wasm produced {w}x{h}",
                gs_img.width, gs_img.height
            ));
        }

        // 3. Compare RGB samples (both sides interleaved RGB; GS grey is
        //    replicated to RGB by ppmraw).
        let gs_rgb = &gs_img.rgb;
        let wasm_rgb = &wasm.data;
        let px = (w as usize) * (h as usize);
        let gs_stride = gs_rgb.len() / px;
        let wasm_stride = wasm_rgb.len() / px;
        let mut max_diff = 0u32;
        let mut sum: u64 = 0;
        let mut within = 0u64;
        for p in 0..px {
            let mut p_max = 0u32;
            for c in 0..3usize {
                let a = if c < wasm_stride {
                    wasm_rgb[p * wasm_stride + c]
                } else {
                    0
                };
                let b = if c < gs_stride {
                    gs_rgb[p * gs_stride + c]
                } else {
                    0
                };
                let d = a.abs_diff(b);
                p_max = p_max.max(d as u32);
                sum += d as u64;
            }
            max_diff = max_diff.max(p_max);
            if p_max <= 12 {
                within += 1;
            }
        }
        let mean_diff = sum as f64 / (px as f64 * 3.0);
        let frac = within as f64 / px as f64;
        let pass = (frac >= fraction_threshold && mean_diff <= mean_threshold) || report_only;
        if !pass {
            failures += 1;
        }
        println!(
            "  {name}: {w}x{h}x{chans}  max={max_diff}  mean={mean_diff:.3}  within≤12={frac:.4}  {}",
            if pass { "PASS" } else { "FAIL" }
        );
    }

    println!();
    if report_only {
        println!("  calibration report only — no pass/fail applied");
    } else if failures == 0 {
        println!("  all {} fixture(s) within tolerance", files.len());
    } else {
        println!(
            "  {failures} of {} fixture(s) OUT OF TOLERANCE",
            files.len()
        );
    }
    if failures > 0 {
        return Err(format!("{failures} fixture(s) failed the GS differential"));
    }
    Ok(())
}
