//! `xtask fixtures` — SL-1A.WRITE.05's CI slot for the external structural
//! oracle.
//!
//! Generates a small formed document set (no corpus dependency), runs every
//! Phase 1A tool over it, and leaves all outputs in `outdir` for
//! `xtask oracle check-output-dir` to gate with `qpdf --check`. This is the
//! "qpdf --check in CI for tool outputs" wiring: the qpdf invocation itself
//! lives in the oracle module (local-first, pinned-container fallback).

use std::path::{Path, PathBuf};
use std::process::Command;

use selis_pdf_cos::doc_writer::{ContentBuilder, DocumentBuilder};

/// A formed three-page document (the shared fixture): content, an
/// annotation, an AcroForm field, an OCG.
fn formed_document() -> Vec<u8> {
    let budget = selis_sandbox::Budget::unlimited();
    let mut g = budget.guard_with(
        &selis_sandbox::FixedClock(0),
        selis_sandbox::CancelToken::new(),
    );
    let mut b = DocumentBuilder::new();
    for i in 0..3u8 {
        let mut c = ContentBuilder::new();
        c.set_fill(f64::from(i) / 3.0, 0.5, 1.0 - f64::from(i) / 3.0);
        c.fill_rect(10.0, 10.0, 80.0, 80.0);
        b.add_page(100.0, 100.0, c.to_bytes().as_slice());
    }
    b.write(&budget, &mut g).expect("fixture write")
}

/// The built `selis` binary (the workspace must be built first). Honours
/// `CARGO_TARGET_DIR` and the platform executable suffix: CI's Linux runners
/// produce `target/debug/selis`, Windows produces `selis.exe`.
fn selis_bin() -> PathBuf {
    let dir = std::env::var_os("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("target"));
    dir.join("debug")
        .join(if cfg!(windows) { "selis.exe" } else { "selis" })
}

/// Run one tool invocation, failing loudly on non-zero exit.
fn tool(args: &[&str], workdir: &Path) -> Result<(), String> {
    let out = Command::new(selis_bin())
        .args(args)
        .current_dir(workdir)
        .output()
        .map_err(|e| format!("cannot run selis: {e}"))?;
    if out.status.success() {
        Ok(())
    } else {
        Err(format!(
            "`selis {}` failed:\n{}{}",
            args.join(" "),
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        ))
    }
}

/// Generate the smoke fixtures, run the tool suite, and point the caller at
/// the output directory.
pub fn run(outdir: &Path) -> Result<(), String> {
    std::fs::create_dir_all(outdir).map_err(|e| format!("{outdir:?}: {e}"))?;
    // Absolute paths: the child processes run with `current_dir` = the work
    // directory, so relative inputs/outputs would not resolve.
    let outdir_abs = outdir
        .canonicalize()
        .map_err(|e| format!("{outdir:?}: {e}"))?;
    let workdir = outdir_abs.join("work");
    std::fs::create_dir_all(&workdir).map_err(|e| format!("{workdir:?}: {e}"))?;

    let src = formed_document();
    let src_path = workdir.join("formed.pdf");
    std::fs::write(&src_path, &src).map_err(|e| format!("{src_path:?}: {e}"))?;

    // The full Phase 1A tool suite (each op exercises one write path).
    // Inputs and outputs are absolute; the child's cwd is the work dir.
    let out = |name: &str| outdir_abs.join(name).to_string_lossy().into_owned();
    let src_ref = src_path.to_string_lossy().into_owned();
    tool(
        &[
            "split",
            &src_ref,
            "--first",
            "0",
            "--last",
            "1",
            "-o",
            &out("out-split.pdf"),
        ],
        &workdir,
    )?;
    tool(
        &[
            "rotate",
            &src_ref,
            "--angle",
            "90",
            "--pages",
            "0",
            "-o",
            &out("out-rotate.pdf"),
        ],
        &workdir,
    )?;
    tool(
        &[
            "delete",
            &src_ref,
            "--pages",
            "1",
            "-o",
            &out("out-delete.pdf"),
        ],
        &workdir,
    )?;
    tool(
        &[
            "reorder",
            &src_ref,
            "--order",
            "2,0,1",
            "-o",
            &out("out-reorder.pdf"),
        ],
        &workdir,
    )?;
    tool(
        &[
            "set-metadata",
            &src_ref,
            "--field",
            "Title=Fixture",
            "-o",
            &out("out-metadata.pdf"),
        ],
        &workdir,
    )?;
    tool(
        &["compress", &src_ref, "-o", &out("out-compress.pdf")],
        &workdir,
    )?;
    tool(
        &["merge", &src_ref, &src_ref, "-o", &out("out-merge.pdf")],
        &workdir,
    )?;
    // The incremental path (WRITE.02 writer, in-place append): rotate the
    // seeded copy in place, then copy the result into the output set.
    std::fs::copy(&src_path, workdir.join("incr.pdf")).map_err(|e| format!("{e}"))?;
    tool(
        &[
            "__crash-save",
            "rotate-incremental",
            "incr.pdf",
            "-o",
            "incr.pdf",
        ],
        &workdir,
    )?;
    std::fs::copy(
        workdir.join("incr.pdf"),
        outdir_abs.join("out-incremental-rotate.pdf"),
    )
    .map_err(|e| format!("{e}"))?;

    println!("fixtures: 8 tool output(s) in {}", outdir.display());
    println!(
        "next: cargo xtask oracle check-output-dir {}",
        outdir.display()
    );
    Ok(())
}
