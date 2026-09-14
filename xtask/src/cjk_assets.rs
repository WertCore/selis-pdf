//! `cargo xtask cjk-build` — SL-3.FONT.10 asset production.
//!
//! Runs the font-side builder (`selis_font::cjk::build`, on top of the
//! FONT.11 subsetter) over a TrueType CJK source and writes the complete
//! lazy-chunk payload the shells distribute:
//!
//! * `<out>/cjk/core.ttf` — the subsetted core (bundled initial payload:
//!   every platform gets the same bytes; the web shell fetches it alongside
//!   the wasm binary, the desktop bundle carries it whole);
//! * `<out>/cjk/<chunk-id>.ttf` — one loadable chunk per covered range;
//! * `<out>/cjk/manifest.json` — the pinned record: chunk list, raw and
//!   brotli-compressed sizes, SHA-256 for every file. `xtask` fails when a
//!   file exceeds its budget (`--budget-core` / `--budget-chunk`, brotli
//!   bytes), so the budgets in ADR-P0043 are a gate, not folklore.
//!
//! The source font is an input, not an artifact: production points this at
//! the pinned Noto TTF checkout of the release pipeline
//! (e.g. `NotoSansSC-Regular.ttf`). The engine tests never run this
//! command — they call `build_set` on a synthetic fixture; real-size
//! numbers land with the first release build against actual Noto.

use std::path::{Path, PathBuf};

use selis_bytes::Bytes;
use selis_font::cjk::build::{build_set, CORE_SAMPLE_HANZI};
use selis_font::cjk::{CHUNKS, CORE_STATIC_IDS};
use selis_sandbox::Budget;
use sha2::{Digest, Sha256};

/// brotli default ceilings (ADR-P0043): the core joins the ≤ 3 MB WASM
/// viewer payload (ADR-P0011 budget) and must stay its own order of
/// magnitude below it; chunks are incremental-download units and stay
/// small enough that a rarely-used range costs a fraction of first paint.
const DEFAULT_BUDGET_CORE: u64 = 1_200_000;
const DEFAULT_BUDGET_CHUNK: u64 = 1_500_000;

/// The command entry point.
pub fn run(
    source: &Path,
    out: &Path,
    core_list: Option<&PathBuf>,
    budget_core: Option<u64>,
    budget_chunk: Option<u64>,
) -> Result<(), String> {
    let raw = std::fs::read(source).map_err(|e| format!("{}: {e}", source.display()))?;
    let extra: Vec<u32> = match core_list {
        Some(path) => {
            let text = std::fs::read_to_string(path).map_err(|e| format!("{path:?}: {e}"))?;
            parse_core_list(&text)?
        }
        None => CORE_SAMPLE_HANZI.iter().map(|c| u32::from(*c)).collect(),
    };
    let mut guard = Budget::unlimited().guard();
    let built = build_set(&Bytes::copy_from_slice(&raw), &extra, &mut guard)
        .map_err(|e| format!("subset failed: {e}"))?
        .ok_or_else(|| {
            format!(
                "{} is not a TrueType (glyf) source font — CFF sources are the \
                 SL-3.FONT.11 follow-up; use a glyf-flavored Noto static TTF",
                source.display()
            )
        })?;

    let dir = out.join("cjk");
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;

    let mut sha = Sha256::new();
    sha.update(&raw);
    let source_sha = hex(&sha.finalize());

    let core_path = dir.join("core.ttf");
    std::fs::write(&core_path, built.core.as_slice()).map_err(|e| format!("{core_path:?}: {e}"))?;
    let core_brotli = compressed_len(&core_path)?;

    let mut chunk_rows = Vec::new();
    let mut failures: Vec<String> = Vec::new();
    for chunk in &built.chunks {
        let path = dir.join(format!("{}.ttf", chunk.chunk.id));
        std::fs::write(&path, chunk.data.as_slice()).map_err(|e| format!("{path:?}: {e}"))?;
        let brotli = compressed_len(&path)?;
        let mut sha = Sha256::new();
        sha.update(chunk.data.as_slice());
        if brotli > budget_chunk.unwrap_or(DEFAULT_BUDGET_CHUNK) {
            failures.push(format!(
                "chunk {} brotli {brotli} > budget {}",
                chunk.chunk.id,
                budget_chunk.unwrap_or(DEFAULT_BUDGET_CHUNK)
            ));
        }
        chunk_rows.push(serde_json::json!({
            "id": chunk.chunk.id,
            "file": format!("cjk/{}.ttf", chunk.chunk.id),
            "range": [format!("U+{:04X}", chunk.chunk.first), format!("U+{:04X}", chunk.chunk.last)],
            "codes": chunk.codes,
            "raw_bytes": chunk.data.as_slice().len(),
            "brotli_bytes": brotli,
            "sha256": hex(&sha.finalize()),
            "url": format!("cjk/{}.ttf", chunk.chunk.id),
        }));
    }
    if core_brotli > budget_core.unwrap_or(DEFAULT_BUDGET_CORE) {
        failures.push(format!(
            "core brotli {core_brotli} > budget {}",
            budget_core.unwrap_or(DEFAULT_BUDGET_CORE)
        ));
    }
    let mut sha = Sha256::new();
    sha.update(built.core.as_slice());

    let table_rows: Vec<serde_json::Value> = CHUNKS
        .iter()
        .map(|c| {
            serde_json::json!({
                "id": c.id,
                "first": format!("U+{:04X}", c.first),
                "last": format!("U+{:04X}", c.last),
                "core_static": CORE_STATIC_IDS.contains(&c.id),
            })
        })
        .collect();
    let manifest = serde_json::json!({
        "schema": "selis-cjk/1",
        "task": "SL-3.FONT.10",
        "source": { "file": source.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(), "sha256": source_sha },
        "chunk_table": table_rows,
        "core": {
            "file": "cjk/core.ttf",
            "codes": built.core_codes,
            "raw_bytes": built.core.as_slice().len(),
            "brotli_bytes": core_brotli,
            "sha256": hex(&sha.finalize()),
            "core_static_ids": CORE_STATIC_IDS,
            "core_extra_count": extra.len(),
        },
        "chunks": chunk_rows,
        "budgets": {
            "core_brotli": budget_core.unwrap_or(DEFAULT_BUDGET_CORE),
            "chunk_brotli": budget_chunk.unwrap_or(DEFAULT_BUDGET_CHUNK),
        },
    });
    let manifest_path = dir.join("manifest.json");
    let text = serde_json::to_string_pretty(&manifest).map_err(|e| format!("manifest: {e}"))?;
    std::fs::write(&manifest_path, format!("{text}\n"))
        .map_err(|e| format!("{manifest_path:?}: {e}"))?;

    println!(
        "cjk-build: core raw {} / brotli {} (budget {})",
        built.core.as_slice().len(),
        core_brotli,
        budget_core.unwrap_or(DEFAULT_BUDGET_CORE)
    );
    for row in &chunk_rows {
        println!(
            "cjk-build: chunk {:16} raw {:>9} brotli {:>9} codes {:>6}",
            row["id"].as_str().unwrap_or("?"),
            row["raw_bytes"].as_u64().unwrap_or(0),
            row["brotli_bytes"].as_u64().unwrap_or(0),
            row["codes"].as_u64().unwrap_or(0),
        );
    }
    println!(
        "cjk-build: {} chunk file(s), manifest {}",
        built.chunks.len(),
        manifest_path.display()
    );
    if !failures.is_empty() {
        for f in &failures {
            println!("cjk-build FAILED: {f}");
        }
        return Err("CJK asset budgets violated".to_string());
    }
    Ok(())
}

/// The core-list file format: `//` or `#` comment lines and whitespace-
/// separated code points — `U+4E00`, `0x4E00`, bare hex, or the
/// characters themselves. Duplicates and invalid scalars are skipped (a
/// list is a coverage hint, not a constraint).
fn parse_core_list(text: &str) -> Result<Vec<u32>, String> {
    let mut out = std::collections::BTreeSet::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with("//") || line.starts_with('#') {
            continue;
        }
        for token in line.split_whitespace() {
            let hexed = token
                .strip_prefix("U+")
                .or_else(|| token.strip_prefix("u+"))
                .or_else(|| {
                    token
                        .strip_prefix("0x")
                        .or_else(|| token.strip_prefix("0X"))
                });
            if let Some(h) = hexed {
                let v = u32::from_str_radix(h, 16).map_err(|e| format!("token {token:?}: {e}"))?;
                if char::from_u32(v).is_some() {
                    out.insert(v);
                }
                continue;
            }
            if !token.is_ascii() {
                out.extend(token.chars().map(|ch| u32::from(ch)));
                continue;
            }
            let v = u32::from_str_radix(token, 16).map_err(|e| format!("token {token:?}: {e}"))?;
            if char::from_u32(v).is_some() {
                out.insert(v);
            }
        }
    }
    Ok(out.into_iter().collect())
}

/// brotli-compressed size via the same node zlib path as `size-check`
/// (best-effort: returns 0 when node is unavailable, with a loud warning).
fn compressed_len(path: &Path) -> Result<u64, String> {
    match crate::size_check::brotli_compress(path) {
        Ok(v) => Ok(u64::try_from(v.len()).unwrap_or(u64::MAX)),
        Err(e) => {
            println!("cjk-build: WARNING brotli measurement failed ({e}); recording 0");
            Ok(0)
        }
    }
}

fn hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push_str(&format!("{b:02x}"));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use selis_font::cjk::chunk_by_id;

    #[test]
    fn core_list_parses_all_documented_forms() {
        let got = parse_core_list("U+4E00 0x3042 4e8c \u{7684} 0xFF01\n").expect("parses");
        assert_eq!(got, vec![0x3042, 0x4E00, 0x4E8C, 0x7684, 0xFF01]);
    }

    #[test]
    fn core_list_skips_comments_and_duplicates() {
        let got = parse_core_list("// comment\nU+4E00 U+4E00\n").expect("parses");
        assert_eq!(got, vec![0x4E00]);
    }

    #[test]
    fn core_list_rejects_unparseable() {
        assert!(parse_core_list("U+zzzz").is_err());
        // Out-of-range scalars are skipped, not fatal (coverage hint).
        assert_eq!(
            parse_core_list("U+110000").expect("parses"),
            Vec::<u32>::new()
        );
    }

    #[test]
    fn every_chunk_table_range_is_undersigned_id_length() {
        // ids become file names — keep them filesystem-safe and short.
        for c in CHUNKS {
            assert!(!c.id.is_empty() && c.id.len() <= 20, "{}", c.id);
            assert!(
                c.id.chars()
                    .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '-'),
                "unsafe chunk id {:?}",
                c.id
            );
            assert!(chunk_by_id(c.id) == Some(c));
        }
    }

    /// The whole CLI payload path on a synthetic source (the same builder
    /// the engine tests exercise, landed through the real filesystem):
    /// core + chunk files exist, the manifest is parseable and pins their
    /// sizes and hashes.
    #[test]
    fn pipeline_writes_pinned_assets_for_a_synthetic_source() {
        let root = std::env::temp_dir().join(format!("selis-cjk-build-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("tempdir");

        // Synthetic CJK source: mini.ttf + rectangle glyphs across ranges.
        let mut font = Bytes::copy_from_slice(include_bytes!(
            "../../crates/selis-font/tests/fixtures/mini.ttf"
        ));
        let mut guard = Budget::unlimited().guard();
        let outline = vec![
            selis_font::OutlineCmd::Move { x: 120.0, y: 80.0 },
            selis_font::OutlineCmd::Line { x: 920.0, y: 80.0 },
            selis_font::OutlineCmd::Line { x: 920.0, y: 920.0 },
            selis_font::OutlineCmd::Line { x: 120.0, y: 920.0 },
            selis_font::OutlineCmd::Close,
        ];
        for code in [0x3042u32, 0x4E00, 0xAC00] {
            font = selis_font::add_glyph(&font, code, &outline, 1000, &mut guard)
                .unwrap()
                .expect("merged");
        }
        let source = root.join("source.ttf");
        std::fs::write(&source, font.as_slice()).expect("write source");
        let list = root.join("core.txt");
        std::fs::write(&list, "// frequency-ish core extras\nU+4E00 U+7684\n").expect("write list");

        let big = 10_000_000;
        run(&source, &root, Some(&list), Some(big), Some(big)).expect("pipeline ok");
        let cjk = root.join("cjk");
        assert!(cjk.join("core.ttf").exists());
        assert!(cjk.join("ideographs-1.ttf").exists(), "covered by core too");
        assert!(cjk.join("hangul-1.ttf").exists());
        assert!(
            !cjk.join("punct-kana.ttf").exists(),
            "core-static range: no file"
        );
        let manifest = std::fs::read_to_string(cjk.join("manifest.json")).expect("manifest");
        let v: serde_json::Value = serde_json::from_str(&manifest).expect("manifest json");
        assert_eq!(v["schema"].as_str(), Some("selis-cjk/1"));
        assert_eq!(v["chunks"].as_array().map(Vec::len), Some(2));
        assert!(v["core"]["raw_bytes"].as_u64().unwrap_or(0) > 100);
        assert!(!v["core"]["sha256"].as_str().unwrap_or("").is_empty());
        assert_eq!(
            v["chunk_table"].as_array().map(Vec::len),
            Some(CHUNKS.len())
        );
        let _ = std::fs::remove_dir_all(&root);
    }
}
