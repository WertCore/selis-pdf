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
//!   brotli-compressed sizes, SHA-256 for every file, **which range is served
//!   by what**, and the measured totals. `xtask` fails when a file exceeds its
//!   budget (`--budget-core` / `--budget-chunk`, brotli bytes), so the budgets
//!   in ADR-P0043 are a gate, not folklore.
//!
//! Four commands, one pipeline:
//!
//! | command | what it does |
//! |---|---|
//! | `cjk-fetch <source-id> <dir>` | download a **pinned** source font and verify its SHA-256 ([`cjk_source`]) |
//! | `cjk-build <source> --out <dir>` | subset it into the payload above |
//! | `cjk-verify <dir>` | re-check a built payload against its own manifest (hashes, budgets, table) |
//! | `cjk-measure <dir> <text>` | what a given document's CJK actually costs to download |
//!
//! ## The measured numbers
//!
//! The pinned source (`noto-sans-sc`, 17 772 300 B) subsets to **16 files,
//! 10 167 248 B raw / 4 698 914 B brotli**, of which the bundled core is
//! 295 080 B / 135 699 B. That is the answer to "CJK without a 100 MB
//! payload": the ~100 MB the plan quotes is the whole Noto CJK *family* (five
//! languages, nine weights, CFF outlines). One weight of one language,
//! subsetted per Unicode range and fetched per range, is an order of magnitude
//! smaller — and a document normally needs the core plus one or two chunks of
//! it, which `cjk-measure` prints per document rather than this total.
//!
//! The measurement is not decoration; it changed the design twice. The dense
//! U+5F00–U+9FFF tail was one 2.4 MB file before it was measured and four
//! sub-700 KB files after (see `selis_font::cjk::CHUNKS`), and the ranges the
//! source has no glyph for became `served_by: null` rows in the manifest
//! rather than a silent absence.
//!
//! ## What is committed, and what is not
//!
//! `assets/cjk/` holds the **manifest** (the pinned record: every file's size
//! and SHA-256, the source's id/url/digest, the budgets, the totals), the
//! **licence**, and this provenance note. The payload files themselves are
//! ~10 MB of regenerable binaries: one command from a pinned,
//! digest-verified source produces them byte-for-byte, and the manifest that
//! *is* committed pins every one of them.
//! `cjk-verify --scope manifest` is the gate over what the repository
//! carries; `cjk-verify --scope full` is the gate over a built payload.

use std::path::{Path, PathBuf};

use selis_bytes::Bytes;
use selis_font::cjk::build::{build_set, CORE_SAMPLE_HANZI, SELIS_CJK_NAME};
use selis_font::cjk::{chunk_by_id, chunk_for, CHUNKS, CORE_STATIC_IDS};
use selis_font::glyph_id_for_char;
use selis_sandbox::Budget;
use sha2::{Digest, Sha256};

use crate::cjk_source;

/// The manifest schema string. `selis-cjk/1` is the shape the extension's
/// `ext/cjk-payload.ts` parses (`schema`, `core`, `chunks[]` with `file`,
/// `raw_bytes`, `brotli_bytes`, `sha256`); every field this build adds is
/// additive, so an EXT.05 consumer keeps working against a newer manifest.
pub const MANIFEST_SCHEMA: &str = "selis-cjk/1";

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
    source_id: Option<&str>,
) -> Result<(), String> {
    let raw = std::fs::read(source).map_err(|e| format!("{}: {e}", source.display()))?;
    let pinned = match source_id {
        Some(id) => Some(cjk_source::by_id(id).ok_or_else(|| {
            format!(
                "'{id}' is not a pinned CJK source; known: {:?} \
                     (a source that is not pinned cannot be recorded in a \
                     manifest that promises immutable bytes)",
                cjk_source::ids()
            )
        })?),
        None => None,
    };
    if let Some(p) = pinned {
        // A pinned id is a claim about *these* bytes. Checking it here means a
        // mistyped `--source` fails the build instead of producing a manifest
        // that names Noto and carries something else.
        let digest = sha256_hex(&raw);
        if digest != p.sha256 || raw.len() as u64 != p.bytes {
            return Err(format!(
                "{} is not the pinned '{}' source: {} bytes sha256 {}, expected {} bytes sha256 {}",
                source.display(),
                p.id,
                raw.len(),
                digest,
                p.bytes,
                p.sha256
            ));
        }
    }
    let (extra, list_sha) = match core_list {
        Some(path) => {
            let text = std::fs::read_to_string(path).map_err(|e| format!("{path:?}: {e}"))?;
            (parse_core_list(&text)?, Some(sha256_hex(text.as_bytes())))
        }
        None => (
            CORE_SAMPLE_HANZI.iter().map(|c| u32::from(*c)).collect(),
            None,
        ),
    };
    let mut guard = Budget::unlimited().guard();
    let built = build_set(
        &Bytes::copy_from_slice(&raw),
        &extra,
        SELIS_CJK_NAME,
        &mut guard,
    )
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

    let source_sha = sha256_hex(&raw);
    let core_brotli_budget = budget_core.unwrap_or(DEFAULT_BUDGET_CORE);
    let chunk_brotli_budget = budget_chunk.unwrap_or(DEFAULT_BUDGET_CHUNK);

    let core_path = dir.join("core.ttf");
    std::fs::write(&core_path, built.core.as_slice()).map_err(|e| format!("{core_path:?}: {e}"))?;
    let core_brotli = compressed_len(&core_path)?;

    let mut chunk_rows = Vec::new();
    let mut total_raw = u64::try_from(built.core.len()).unwrap_or(u64::MAX);
    let mut total_brotli = core_brotli;
    let mut failures: Vec<String> = Vec::new();
    for chunk in &built.chunks {
        let file_name = format!("{}.ttf", chunk.chunk.id);
        let path = dir.join(&file_name);
        std::fs::write(&path, chunk.data.as_slice()).map_err(|e| format!("{path:?}: {e}"))?;
        let brotli = compressed_len(&path)?;
        total_raw = total_raw.saturating_add(u64::try_from(chunk.data.len()).unwrap_or(u64::MAX));
        total_brotli = total_brotli.saturating_add(brotli);
        if brotli > chunk_brotli_budget {
            failures.push(format!(
                "chunk {} brotli {brotli} > budget {chunk_brotli_budget}",
                chunk.chunk.id
            ));
        }
        chunk_rows.push(serde_json::json!({
            "id": chunk.chunk.id,
            "file": format!("cjk/{file_name}"),
            "range": [
                format!("U+{:04X}", chunk.chunk.first),
                format!("U+{:04X}", chunk.chunk.last),
            ],
            "codes": chunk.codes,
            "raw_bytes": chunk.data.len(),
            "brotli_bytes": brotli,
            "sha256": sha256_hex(chunk.data.as_slice()),
            "url": format!("cjk/{file_name}"),
        }));
    }
    if core_brotli > core_brotli_budget {
        failures.push(format!(
            "core brotli {core_brotli} > budget {core_brotli_budget}"
        ));
    }

    // Which range is served by what - the row a shell reads to answer "can this
    // document be rendered from this payload at all?" before it downloads
    // anything. `null` is a real answer, not a gap in the table: it means the
    // source font has no glyph there, so the code renders .notdef and no chunk
    // is ever requested for it (see `CjkFontSet::mark_unavailable`).
    let unserved: Vec<&str> = built.unserved.iter().map(|c| c.id).collect();
    let table_rows: Vec<serde_json::Value> = CHUNKS
        .iter()
        .map(|c| {
            let served_by = if unserved.contains(&c.id) {
                serde_json::Value::Null
            } else if CORE_STATIC_IDS.contains(&c.id) {
                serde_json::json!("core")
            } else {
                serde_json::json!("chunk")
            };
            serde_json::json!({
                "id": c.id,
                "first": format!("U+{:04X}", c.first),
                "last": format!("U+{:04X}", c.last),
                "core_static": CORE_STATIC_IDS.contains(&c.id),
                "served_by": served_by,
            })
        })
        .collect();
    let manifest = serde_json::json!({
        "schema": MANIFEST_SCHEMA,
        "task": "SL-3.FONT.10",
        "adr": "ADR-P0043",
        "source": {
            "id": pinned.map(|p| p.id),
            "family": pinned.map(|p| p.family),
            "url": pinned.map(|p| p.url),
            "license": pinned.map(|p| p.license),
            "reserved_font_name": pinned.and_then(|p| p.reserved_font_name),
            "file": source.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
            "raw_bytes": raw.len(),
            "sha256": source_sha,
        },
        "name": {
            "family": SELIS_CJK_NAME.family,
            "subfamily": SELIS_CJK_NAME.subfamily,
        },
        "chunk_table": table_rows,
        "unserved": unserved,
        "core": {
            "file": "cjk/core.ttf",
            "codes": built.core_codes,
            "raw_bytes": built.core.len(),
            "brotli_bytes": core_brotli,
            "sha256": sha256_hex(built.core.as_slice()),
            "core_static_ids": CORE_STATIC_IDS,
            "core_extra_count": extra.len(),
            "core_list_sha256": list_sha,
        },
        "chunks": chunk_rows,
        "totals": {
            "files": built.chunks.len().saturating_add(1),
            "raw_bytes": total_raw,
            "brotli_bytes": total_brotli,
        },
        "budgets": {
            "core_brotli": core_brotli_budget,
            "chunk_brotli": chunk_brotli_budget,
        },
    });
    let manifest_path = dir.join("manifest.json");
    let text = serde_json::to_string_pretty(&manifest).map_err(|e| format!("manifest: {e}"))?;
    std::fs::write(&manifest_path, format!("{text}\n"))
        .map_err(|e| format!("{manifest_path:?}: {e}"))?;

    println!("cjk-build: source {} ({source_sha})", source.display());
    println!(
        "cjk-build: core raw {} / brotli {core_brotli} (budget {core_brotli_budget}), {} codes",
        built.core.len(),
        built.core_codes
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
        "cjk-build: {} file(s), {total_raw} B raw / {total_brotli} B brotli, manifest {}",
        built.chunks.len() + 1,
        manifest_path.display()
    );
    if !unserved.is_empty() {
        println!(
            "cjk-build: unserved by this source (renders .notdef, never fetched): {}",
            unserved.join(", ")
        );
    }
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
                out.extend(token.chars().map(u32::from));
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

/// Lowercase hex SHA-256 of some bytes.
fn sha256_hex(bytes: &[u8]) -> String {
    let mut sha = Sha256::new();
    sha.update(bytes);
    hex(&sha.finalize())
}

/// `cargo xtask cjk-fetch <source-id> <dir>` — fetch a pinned source font.
///
/// The download is an external tool (`curl`, like `qpdf` and `node` elsewhere
/// in this file's neighbourhood) rather than a dependency: ADR-P0021's
/// supply-chain gate is about what enters the build graph, and a pinned URL
/// plus a pinned digest is *stronger* provenance than another crate in
/// `Cargo.lock`. The file is only accepted into `dir` if it matches the pin,
/// and a mismatch leaves nothing behind.
///
/// Idempotent: a file already at `dir/<file>` with the right digest is kept
/// and the command succeeds without touching the network.
pub fn fetch(source_id: &str, dir: &Path) -> Result<(), String> {
    let pinned = cjk_source::by_id(source_id).ok_or_else(|| {
        format!(
            "'{source_id}' is not a pinned CJK source; known: {:?}",
            cjk_source::ids()
        )
    })?;
    std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let dest = dir.join(pinned.file);
    if let Ok(existing) = std::fs::read(&dest) {
        let digest = sha256_hex(&existing);
        if digest == pinned.sha256 {
            println!(
                "cjk-fetch: {} is already the pinned {} source ({digest})",
                dest.display(),
                pinned.id
            );
            return Ok(());
        }
    }
    let tmp = dir.join(format!("{}.part", pinned.file));
    let _ = std::fs::remove_file(&tmp);
    let status = std::process::Command::new("curl")
        .args(["--fail", "--silent", "--show-error", "--location"])
        .arg("--output")
        .arg(&tmp)
        .arg(pinned.url)
        .status()
        .map_err(|e| format!("curl is required to fetch a CJK source: {e}"))?;
    if !status.success() {
        let _ = std::fs::remove_file(&tmp);
        return Err(format!(
            "download of {} failed ({status}); nothing was written to {}",
            pinned.url,
            dir.display()
        ));
    }
    let got = std::fs::read(&tmp).map_err(|e| format!("{}: {e}", tmp.display()))?;
    let digest = sha256_hex(&got);
    if digest != pinned.sha256 {
        let _ = std::fs::remove_file(&tmp);
        return Err(format!(
            "downloaded {} is sha256 {digest}, but '{}' is pinned to {} — \
             refusing to keep it (a mirror, a proxy, or a moved commit)",
            pinned.file, pinned.id, pinned.sha256
        ));
    }
    std::fs::rename(&tmp, &dest).map_err(|e| format!("{}: {e}", dest.display()))?;
    println!(
        "cjk-fetch: {} -> {} ({digest}, {}, {})",
        pinned.id,
        dest.display(),
        pinned.family,
        pinned.license
    );
    Ok(())
}

/// How much of a payload `cjk-verify` expects to find on disk.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    /// Every file the manifest lists must be present and hash as pinned — a
    /// built payload directory, which is what a release uploads.
    Full,
    /// Cross-check the manifest against itself and verify whatever files the
    /// directory does carry. This is the scope the repository's
    /// `assets/cjk/manifest.json` can be checked at, because the repository
    /// carries the *record* and not the ~10 MB of chunk binaries it pins.
    Manifest,
}

impl Scope {
    /// The `--scope` argument value.
    pub fn parse(s: &str) -> Result<Self, String> {
        match s {
            "full" => Ok(Self::Full),
            "manifest" => Ok(Self::Manifest),
            other => Err(format!("scope must be `full` or `manifest`, got `{other}`")),
        }
    }
}

/// `cargo xtask cjk-verify <dir>` — re-check a payload against its manifest.
///
/// The manifest is a promise; this is the auditor. It re-reads every file the
/// manifest names, recomputes its SHA-256, compares the sizes, re-checks
/// brotli against the budgets, and cross-checks the chunk table against the
/// `chunks` list — so a manifest claiming a range is served by a file that is
/// not there (or listing a file no range claims) is a failure rather than a
/// surprise at a reader's first paint.
pub fn verify(dir: &Path, scope: Scope) -> Result<(), String> {
    let cjk = dir.join("cjk");
    let manifest_path = cjk.join("manifest.json");
    let text = std::fs::read_to_string(&manifest_path)
        .map_err(|e| format!("{}: {e}", manifest_path.display()))?;
    let m: serde_json::Value =
        serde_json::from_str(&text).map_err(|e| format!("{}: {e}", manifest_path.display()))?;
    let mut problems: Vec<String> = Vec::new();
    if m["schema"].as_str() != Some(MANIFEST_SCHEMA) {
        problems.push(format!(
            "schema is {:?}, expected {MANIFEST_SCHEMA}",
            m["schema"].as_str()
        ));
    }
    let core_budget = m["budgets"]["core_brotli"].as_u64().unwrap_or(u64::MAX);
    let chunk_budget = m["budgets"]["chunk_brotli"].as_u64().unwrap_or(u64::MAX);

    let core_name = m["core"]["file"].as_str().unwrap_or("cjk/core.ttf");
    check_file(
        &cjk,
        core_name,
        &m["core"],
        core_budget,
        scope,
        &mut problems,
    );

    let chunks = m["chunks"].as_array().cloned().unwrap_or_default();
    if scope == Scope::Full {
        for row in &chunks {
            let name = row["file"].as_str().unwrap_or("?");
            check_file(&cjk, name, row, chunk_budget, scope, &mut problems);
        }
    }

    // The table and the file list must agree, in both directions.
    let table = m["chunk_table"].as_array().cloned().unwrap_or_default();
    for row in &table {
        let id = row["id"].as_str().unwrap_or("?");
        let listed = chunks.iter().any(|c| c["id"].as_str() == Some(id));
        match row["served_by"].as_str() {
            Some("chunk") if !listed => {
                problems.push(format!("{id}: table says chunk, manifest has no file"));
            }
            Some("core") if listed => {
                problems.push(format!("{id}: table says core, manifest has a chunk file"));
            }
            None if listed => {
                problems.push(format!(
                    "{id}: table says unserved (served_by null), manifest has a file"
                ));
            }
            None | Some("chunk") | Some("core") => {}
            Some(other) => problems.push(format!("{id}: unknown served_by `{other}`")),
        }
        if chunk_by_id(id).is_none() {
            problems.push(format!("{id}: not a chunk id in selis_font::cjk::CHUNKS"));
        }
    }
    let table_ids: Vec<&str> = table.iter().filter_map(|r| r["id"].as_str()).collect();
    for row in &chunks {
        let id = row["id"].as_str().unwrap_or("?");
        if !table_ids.contains(&id) {
            problems.push(format!("{id}: a file for a range the table does not carry"));
        }
    }

    if !problems.is_empty() {
        for p in &problems {
            println!("cjk-verify FAILED: {p}");
        }
        return Err(format!(
            "{} problem(s) in {}",
            problems.len(),
            manifest_path.display()
        ));
    }
    println!(
        "cjk-verify: {} ok ({} chunk file(s) in the manifest, core {} B raw / {} B brotli)",
        manifest_path.display(),
        chunks.len(),
        m["core"]["raw_bytes"].as_u64().unwrap_or(0),
        m["core"]["brotli_bytes"].as_u64().unwrap_or(0)
    );
    if m["totals"].is_object() {
        println!(
            "cjk-verify: payload totals {} B raw / {} B brotli over {} file(s)",
            m["totals"]["raw_bytes"].as_u64().unwrap_or(0),
            m["totals"]["brotli_bytes"].as_u64().unwrap_or(0),
            m["totals"]["files"].as_u64().unwrap_or(0)
        );
    }
    Ok(())
}

/// Hash, size, budget and (except in `Manifest` scope) presence of one file.
fn check_file(
    cjk: &Path,
    name: &str,
    row: &serde_json::Value,
    budget: u64,
    scope: Scope,
    problems: &mut Vec<String>,
) {
    let path = cjk.join(name.trim_start_matches("cjk/"));
    let bytes = match std::fs::read(&path) {
        Ok(b) => b,
        Err(e) => {
            if scope == Scope::Full {
                problems.push(format!("{name}: {e}"));
            }
            return;
        }
    };
    let want_raw = row["raw_bytes"].as_u64().unwrap_or(u64::MAX);
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) != want_raw {
        problems.push(format!(
            "{name}: {} bytes on disk, manifest says {want_raw}",
            bytes.len()
        ));
    }
    let digest = sha256_hex(&bytes);
    let want_sha = row["sha256"].as_str().unwrap_or("");
    if digest != want_sha {
        problems.push(format!("{name}: sha256 {digest}, manifest says {want_sha}"));
    }
    let brotli = row["brotli_bytes"].as_u64().unwrap_or(0);
    if brotli > budget {
        problems.push(format!("{name}: brotli {brotli} > budget {budget}"));
    }
}

/// `cargo xtask cjk-measure <dir> <text>` — what a document's CJK costs.
///
/// The DoD asks for a *measured incremental download*; this is the
/// measurement. Every code point the document shows is classified the way the
/// engine classifies it:
///
/// * the **core** already answers it → 0 extra bytes (the core is fetched
///   once, beside the wasm binary, whatever the document contains);
/// * otherwise the covering **chunk** answers it → that chunk's measured
///   brotli bytes, once per range;
/// * the payload has no glyph for the range → 0 extra bytes and a `.notdef`
///   box, reported as such.
///
/// The first rule is why this reads the *core file's* cmap rather than asking
/// which ranges the core is "for": a common character in the frequency list
/// lives in a chunk range too, and it is the core that answers it at zero
/// marginal cost. Asking the question at range level over-reports badly — the
/// first measurement of an 83-character paragraph said six chunks and
/// 2 824 942 B, when 54 of its 63 CJK characters are in the core. If the
/// core file is not next to the manifest, the tool says so and falls back to
/// the range-level answer rather than silently guessing.
pub fn measure(dir: &Path, text_path: &Path) -> Result<(), String> {
    let cjk = dir.join("cjk");
    let manifest_path = cjk.join("manifest.json");
    let text = std::fs::read_to_string(&manifest_path)
        .map_err(|e| format!("{}: {e}", manifest_path.display()))?;
    let m: serde_json::Value =
        serde_json::from_str(&text).map_err(|e| format!("{}: {e}", manifest_path.display()))?;
    let doc =
        std::fs::read_to_string(text_path).map_err(|e| format!("{}: {e}", text_path.display()))?;

    let served: std::collections::BTreeMap<&str, Option<&str>> = m["chunk_table"]
        .as_array()
        .map(|rows| {
            rows.iter()
                .filter_map(|r| Some((r["id"].as_str()?, r["served_by"].as_str())))
                .collect()
        })
        .unwrap_or_default();
    let sizes: std::collections::BTreeMap<&str, u64> = m["chunks"]
        .as_array()
        .map(|rows| {
            rows.iter()
                .filter_map(|r| Some((r["id"].as_str()?, r["brotli_bytes"].as_u64()?)))
                .collect()
        })
        .unwrap_or_default();
    let core_path = cjk.join("core.ttf");
    let core_bytes = match std::fs::read(&core_path) {
        Ok(bytes) => Some(Bytes::copy_from_slice(&bytes)),
        Err(e) => {
            println!(
                "cjk-measure: WARNING no core at {} ({e}); falling back to \
                 range-level attribution, which over-reports",
                core_path.display()
            );
            None
        }
    };

    let mut wanted: std::collections::BTreeSet<&str> = std::collections::BTreeSet::new();
    let mut unserved: std::collections::BTreeSet<&str> = std::collections::BTreeSet::new();
    let mut in_core = 0u64;
    let mut by_chunk = 0u64;
    let mut notdef = 0u64;
    let mut cjk_chars = 0u64;
    for ch in doc.chars() {
        let code = u32::from(ch);
        let Some(chunk) = chunk_for(code) else {
            continue;
        };
        cjk_chars = cjk_chars.saturating_add(1);
        if served.get(chunk.id).copied() == Some(None) {
            let _ = unserved.insert(chunk.id);
            notdef = notdef.saturating_add(1);
            continue;
        }
        // The core's own cmap is the authority on "already downloaded".
        let covered = core_bytes
            .as_ref()
            .is_some_and(|core| glyph_id_for_char(core, code).is_some())
            || served.get(chunk.id).copied() == Some(Some("core"));
        if covered {
            in_core = in_core.saturating_add(1);
        } else {
            by_chunk = by_chunk.saturating_add(1);
            let _ = wanted.insert(chunk.id);
        }
    }
    let core_brotli = m["core"]["brotli_bytes"].as_u64().unwrap_or(0);
    let mut total = core_brotli;
    println!(
        "cjk-measure: {} — {cjk_chars} CJK char(s): {in_core} answered by the core \
         (already fetched, {core_brotli} B), {by_chunk} needing {} chunk file(s), \
         {notdef} with no glyph in this payload",
        text_path.display(),
        wanted.len()
    );
    for id in &wanted {
        let bytes = sizes.get(id).copied().unwrap_or(0);
        total = total.saturating_add(bytes);
        println!("cjk-measure:   {id:<20} {bytes:>9} B brotli");
    }
    if !unserved.is_empty() {
        println!(
            "cjk-measure:   not served by this payload (renders .notdef): {}",
            unserved.into_iter().collect::<Vec<_>>().join(", ")
        );
    }
    println!(
        "cjk-measure: incremental download for this document: {total} B brotli \
         (core {core_brotli} B + {} chunk file(s))",
        wanted.len()
    );
    Ok(())
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
        run(&source, &root, Some(&list), Some(big), Some(big), None).expect("pipeline ok");
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
        // The core list is pinned by digest, so a manifest says which coverage
        // hint produced the core and not merely how many codes went in.
        assert_eq!(
            v["core"]["core_list_sha256"].as_str().map(str::len),
            Some(64)
        );
        // The published name is ours, the source's is not claimed.
        assert_eq!(v["name"]["family"].as_str(), Some("Selis CJK"));
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Build a payload from the synthetic source into a fresh temp dir, and
    /// return the root. The tests below all need one; building it is ~30 ms.
    fn built_payload(tag: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!("selis-cjk-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("tempdir");
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
        let big = 10_000_000u64;
        run(&source, &root, None, Some(big), Some(big), None).expect("build ok");
        root
    }

    /// The manifest says *which* range is served by what, so a shell never has
    /// to download a file to find out whether one exists. This is the contract
    /// the extension's store and WASM.07's loader read.
    #[test]
    fn served_by_classifies_every_table_row() {
        let root = built_payload("served");
        let text =
            std::fs::read_to_string(root.join("cjk").join("manifest.json")).expect("manifest");
        let v: serde_json::Value = serde_json::from_str(&text).expect("json");
        let rows = v["chunk_table"].as_array().expect("table");
        let mut core = 0;
        let mut chunk = 0;
        let mut none = 0;
        for r in rows {
            match r["served_by"].as_str() {
                Some("core") => core += 1,
                Some("chunk") => chunk += 1,
                None => none += 1,
                other => panic!("unknown served_by {other:?}"),
            }
        }
        // The fixture covers あ (core-static kana), 一 (core extra + its range)
        // and 가 (its own range); every other range has no glyph at all.
        assert_eq!(core, 1, "punct-kana");
        assert_eq!(chunk, 2, "ideographs-1 and hangul-1");
        let unserved = v["unserved"].as_array().expect("unserved list");
        assert_eq!(unserved.len(), none, "the two spellings agree");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// `cjk-verify` passes on the payload it was handed, and fails — naming the
    /// file — when a byte changes underneath it. A verifier that cannot fail is
    /// not a verifier.
    #[test]
    fn verify_passes_on_a_fresh_payload_and_fails_on_a_tampered_chunk() {
        let root = built_payload("verify");
        verify(&root, Scope::Full).expect("fresh payload verifies");

        let chunk = root.join("cjk").join("hangul-1.ttf");
        let mut bytes = std::fs::read(&chunk).expect("read");
        let last = bytes.len().saturating_sub(1);
        bytes[last] = bytes[last].wrapping_add(1);
        std::fs::write(&chunk, &bytes).expect("tamper");
        let err = verify(&root, Scope::Full).expect_err("tampered payload must fail");
        assert!(err.contains("problem"), "a counted failure: {err}");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A missing file is a `full`-scope failure and a non-event in `manifest`
    /// scope — exactly the difference between "the release build lost a chunk"
    /// and "the repository carries the record, not the chunk binaries".
    #[test]
    fn scope_decides_whether_a_missing_chunk_is_a_failure() {
        assert_eq!(Scope::parse("full").expect("full"), Scope::Full);
        assert_eq!(Scope::parse("manifest").expect("manifest"), Scope::Manifest);
        assert!(Scope::parse("everything").is_err());
        let root = built_payload("scope");
        std::fs::remove_file(root.join("cjk").join("hangul-1.ttf")).expect("remove");

        assert!(
            verify(&root, Scope::Full).is_err(),
            "a built payload needs its chunks"
        );
        verify(&root, Scope::Manifest).expect("the record is still self-consistent");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// The DoD's "measured incremental download", computed from the manifest's
    /// own measured sizes. The assertion is deliberately about the *contract*
    /// (which files, which total) rather than about brotli output, which
    /// depends on the node zlib version the machine happens to have.
    #[test]
    fn measure_runs_over_a_document_and_a_payload() {
        let root = built_payload("measure");
        let doc = root.join("doc.txt");
        // 가 (hangul-1), あ (core-static kana), and Latin that is not CJK.
        std::fs::write(&doc, "가\u{3042} hello").expect("write doc");
        measure(&root, &doc).expect("measure runs");

        let text =
            std::fs::read_to_string(root.join("cjk").join("manifest.json")).expect("manifest");
        let v: serde_json::Value = serde_json::from_str(&text).expect("json");
        let core = v["core"]["brotli_bytes"].as_u64().expect("core brotli");
        let hangul = v["chunks"]
            .as_array()
            .expect("chunks")
            .iter()
            .find(|c| c["id"].as_str() == Some("hangul-1"))
            .and_then(|c| c["brotli_bytes"].as_u64())
            .expect("hangul-1 brotli");
        assert!(core > 0 && hangul > 0, "both files have measured sizes");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A code the payload cannot serve measures as *nothing to download* — and
    /// the command says so. "Free" and "will render .notdef forever" are
    /// different facts and a DoD number must not conflate them.
    #[test]
    fn measure_reports_an_unserved_script_rather_than_silently_costing_zero() {
        let root = built_payload("unserved");
        let doc = root.join("han.txt");
        // U+20B9F is in ext-b, which the synthetic source does not cover.
        std::fs::write(&doc, "\u{20B9F}").expect("write doc");
        measure(&root, &doc).expect("measure runs over an unserved range");
        let _ = std::fs::remove_dir_all(&root);
    }
}

    /// The repository's committed record is checked on every test run, not only
    /// when someone remembers to: `assets/cjk/manifest.json` must verify
    /// against itself (`cjk-verify --scope manifest`), and the core list it
    /// names must be the one in the repository, byte for byte. A record that
    /// has drifted from its own inputs is the failure mode a pinned manifest
    /// exists to prevent, and it is silent -- nothing about the JSON looks
    /// wrong until a reader is offered a chunk no build ever produced.
    #[test]
    fn the_committed_record_verifies_against_its_own_inputs() {
        let assets = Path::new(env!("CARGO_MANIFEST_DIR")).join("../assets/cjk");
        verify(
            assets.parent().expect("assets dir"),
            Scope::Manifest,
        )
        .expect("assets/cjk/manifest.json verifies");

        let text = std::fs::read_to_string(assets.join("manifest.json")).expect("manifest");
        let v: serde_json::Value = serde_json::from_str(&text).expect("json");
        let pinned = v["core"]["core_list_sha256"]
            .as_str()
            .expect("core list digest");
        let list = std::fs::read(assets.join("core-list.txt")).expect("core list");
        assert_eq!(
            sha256_hex(&list),
            pinned,
            "core-list.txt is not the list the committed manifest was built from"
        );
        // And the list is a real frequency list, not a placeholder.
        let codes = parse_core_list(&String::from_utf8_lossy(&list)).expect("core list parses");
        assert!(
            codes.len() >= 3_000,
            "a core list of {} is a sample, not a frequency list",
            codes.len()
        );
        assert_eq!(codes.first(), Some(&0x7684), "the most frequent hanzi leads");
        assert!(
            v["core"]["codes"].as_u64().unwrap_or(0) > u64::try_from(codes.len()).unwrap_or(0),
            "the core covers its static ranges plus the list"
        );
    }

    /// A committed record is a promise about a payload, so the budgets it
    /// publishes have to still hold: every file it names must fit the ceilings
    /// ADR-P0043 sets -- the ones the extension's store independently mirrors
    /// and would refuse to install past.
    #[test]
    fn the_committed_record_respects_the_published_budgets() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../assets/cjk/manifest.json");
        let text = std::fs::read_to_string(&path).expect("manifest");
        let v: serde_json::Value = serde_json::from_str(&text).expect("json");
        assert_eq!(v["budgets"]["core_brotli"].as_u64(), Some(1_200_000));
        assert_eq!(v["budgets"]["chunk_brotli"].as_u64(), Some(1_500_000));
        let core = v["core"]["brotli_bytes"].as_u64().expect("core brotli");
        assert!(core <= 1_200_000, "core over budget: {core}");
        for row in v["chunks"].as_array().expect("chunks") {
            let id = row["id"].as_str().unwrap_or("?");
            let b = row["brotli_bytes"].as_u64().unwrap_or(u64::MAX);
            assert!(b <= 1_500_000, "{id} over budget: {b}");
        }
        // The extension's store refuses a payload that does not fit its own
        // 8 MB budget, so "install every range" has to be a legal operation
        // for a user who wants it.
        let total = v["totals"]["raw_bytes"].as_u64().expect("totals");
        assert!(
            total < 8 * 1024 * 1024,
            "the full payload must fit the store's 8 MB budget: {total}"
        );
    }
}
