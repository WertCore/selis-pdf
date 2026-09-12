//! `xtask corpus` â€” SL-0.CORP.01.
//!
//! `corpus fetch` downloads each manifest entry to a local cache (default
//! `~/.cache/selis-corpus`, overridable with `SELIS_CORPUS_CACHE`), verifies
//! the sha256, and never commits the files. `corpus stats` reports the total
//! count and tag distribution. `corpus list` prints the manifests.
//!
//! The plan's rule is strict: the harness fetches rather than vendors anything
//! unclear (SL-0.LEGAL.04). Downloads go through `curl` (present on Windows 10+,
//! macOS, and every Linux CI image) so the xtask binary stays dependency-free;
//! the digest check is done with `certutil`/`sha256sum`. A recorded sha256 that
//! does not match fails loudly â€” an unverified download is a warning, never a
//! silent success.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;

#[derive(Deserialize)]
struct CorpusManifest {
    corpus: Vec<CorpusEntry>,
}

#[derive(Deserialize, Clone)]
struct CorpusEntry {
    id: String,
    name: String,
    source_url: String,
    sha256: String,
    licence: String,
    #[serde(default)]
    tags: Vec<String>,
    #[allow(dead_code)]
    #[serde(default)]
    notes: String,
}

pub enum CorpusCommand {
    Fetch(Option<String>),
    List,
    Stats,
    ExpectGenerate,
    ExpectMerge {
        from: PathBuf,
    },
    Verify {
        golden: bool,
        selis: Option<PathBuf>,
    },
}

pub fn run(cmd: CorpusCommand) -> Result<(), String> {
    let entries = load_all()?;
    match cmd {
        CorpusCommand::Fetch(tag) => fetch(&entries, tag.as_deref()),
        CorpusCommand::List => list(&entries),
        CorpusCommand::Stats => stats(&entries),
        CorpusCommand::ExpectGenerate => expect_generate(),
        CorpusCommand::ExpectMerge { from } => expect_merge(&from),
        CorpusCommand::Verify { golden, selis } => verify(golden, selis.as_deref()),
    }
}

fn load_all() -> Result<Vec<CorpusEntry>, String> {
    let dir = Path::new("corpus/manifests");
    let mut out = Vec::new();
    let entries =
        std::fs::read_dir(dir).map_err(|e| format!("cannot read corpus/manifests: {e}"))?;
    for entry in entries {
        let entry = entry.map_err(|e| format!("read_dir error: {e}"))?;
        let path = entry.path();
        if path.extension().is_some_and(|e| e == "toml") {
            let text = std::fs::read_to_string(&path)
                .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
            let m: CorpusManifest =
                toml::from_str(&text).map_err(|e| format!("{} invalid: {e}", path.display()))?;
            out.extend(m.corpus);
        }
    }
    Ok(out)
}

fn cache_dir() -> PathBuf {
    std::env::var("SELIS_CORPUS_CACHE")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            std::env::var("USERPROFILE")
                .or_else(|_| std::env::var("HOME"))
                .map(|home| PathBuf::from(home).join(".cache").join("selis-corpus"))
                .unwrap_or_else(|_| PathBuf::from(".corpus-cache"))
        })
}

/// Deterministic local name for an entry: `id` plus the URL's extension.
fn dest_for(e: &CorpusEntry, cache: &Path) -> PathBuf {
    let ext = Path::new(&e.source_url)
        .extension()
        .and_then(|s| s.to_str())
        .map(|s| format!(".{s}"))
        .unwrap_or_default();
    cache.join(format!("{}{}", e.id, ext))
}

fn fetch(entries: &[CorpusEntry], tag: Option<&str>) -> Result<(), String> {
    let cache = cache_dir();
    std::fs::create_dir_all(&cache).map_err(|e| format!("cannot create cache dir: {e}"))?;

    let selected: Vec<&CorpusEntry> = match tag {
        Some(t) => {
            let hits: Vec<&CorpusEntry> = entries
                .iter()
                .filter(|e| e.tags.iter().any(|candidate| candidate == t))
                .collect();
            if hits.is_empty() {
                return Err(format!("no corpus entry carries the tag `{t}`"));
            }
            hits
        }
        None => entries.iter().collect(),
    };

    let mut fetched = 0usize;
    let mut warnings = 0usize;
    for e in selected {
        let dest = dest_for(e, &cache);
        if dest.exists() {
            match verify_sha256(&dest, e) {
                Ok(true) => println!("  {}: cached, hash ok", e.id),
                Ok(false) => {
                    // DoD: a hash mismatch fails loudly, never silently trusts.
                    return Err(format!(
                        "{}: cached file {} fails sha256 â€” delete it and refetch",
                        e.id,
                        dest.display()
                    ));
                }
                Err(msg) => return Err(msg),
            }
            continue;
        }
        if e.sha256.trim().is_empty() {
            warnings += 1;
            println!(
                "  {}: no sha256 recorded â€” downloading UNVERIFIED ({}). Refusing to trust; \
                 record the printed hash after this fetch.",
                e.id, e.source_url
            );
        } else {
            println!("  {}: fetching {}", e.id, e.source_url);
        }
        download(e, &dest)?;
        let expected = e.sha256.trim().to_ascii_lowercase();
        if expected.is_empty() {
            let hash = sha256_hex(&dest).map_err(|m| format!("{}: {m}", dest.display()))?;
            println!(
                "  {}: downloaded â€” RECORD sha256 = \"{}\" in the manifest",
                e.id, hash
            );
        } else {
            match verify_sha256(&dest, e) {
                Ok(true) => println!("  {}: downloaded, hash verified", e.id),
                Ok(false) => {
                    let actual =
                        sha256_hex(&dest).map_err(|m| format!("{}: {m}", dest.display()))?;
                    return Err(format!(
                        "{}: sha256 MISMATCH after download â€” expected {}, got {}. Refusing the file.",
                        e.id,
                        e.sha256,
                        actual
                    ));
                }
                Err(msg) => return Err(msg),
            }
        }
        fetched += 1;
    }
    println!(
        "corpus fetch: {fetched} new, {} warnings (missing/unverified hashes)",
        warnings
    );
    Ok(())
}

/// Download `source_url` to `dest` with `curl`, following redirects and
/// failing on any HTTP error. A partial download is removed so it is never
/// mistaken for a verified cache hit.
fn download(e: &CorpusEntry, dest: &Path) -> Result<(), String> {
    let status = std::process::Command::new("curl")
        .arg("--fail")
        .arg("--location")
        .arg("--silent")
        .arg("--show-error")
        .arg("--connect-timeout")
        .arg("30")
        .arg("--output")
        .arg(dest)
        .arg(&e.source_url)
        .status()
        .map_err(|err| format!("cannot run curl: {err} (is curl installed?)"))?;
    if !status.success() {
        let _ = std::fs::remove_file(dest);
        return Err(format!("download failed for {}: {}", e.id, e.source_url));
    }
    Ok(())
}

fn verify_sha256(path: &Path, e: &CorpusEntry) -> Result<bool, String> {
    let expected = e.sha256.trim().to_ascii_lowercase();
    if expected.is_empty() {
        return Ok(true); // nothing to verify against yet
    }
    let actual = sha256_hex(path).map_err(|m| format!("{}: {m}", path.display()))?;
    Ok(actual == expected)
}

/// A tiny, dependency-free SHA-256 is overkill for a phase-0 harness; use the
/// system `certutil` on Windows or `sha256sum` elsewhere.
pub(crate) fn sha256_hex(path: &Path) -> Result<String, String> {
    let path_str = path.to_string_lossy().into_owned();
    let (program, args): (&str, Vec<&str>) = if std::env::consts::OS == "windows" {
        ("certutil", vec!["-hashfile", &path_str, "SHA256"])
    } else {
        ("sha256sum", vec![&path_str])
    };
    let out = std::process::Command::new(program)
        .args(&args)
        .output()
        .map_err(|e| format!("cannot run hash tool ({program}): {e}"))?;
    let stdout = String::from_utf8_lossy(&out.stdout);
    // certutil prints a hex digest line; sha256sum prints "<hex>  <path>".
    let hex = stdout
        .lines()
        .find_map(|l| {
            let t = l.trim();
            let first = t.split_whitespace().next().unwrap_or("");
            if first.len() == 64 && first.chars().all(|c| c.is_ascii_hexdigit()) {
                Some(first.to_ascii_lowercase())
            } else {
                None
            }
        })
        .ok_or_else(|| "hash tool produced no digest".to_string())?;
    Ok(hex)
}

fn list(entries: &[CorpusEntry]) -> Result<(), String> {
    for e in entries {
        println!(
            "{}  [{}]  {}  ({})",
            e.id,
            e.tags.join(","),
            e.name,
            e.licence
        );
    }
    Ok(())
}

fn stats(entries: &[CorpusEntry]) -> Result<(), String> {
    let mut tags: BTreeMap<String, usize> = BTreeMap::new();
    for e in entries {
        for t in &e.tags {
            *tags.entry(t.clone()).or_insert(0) += 1;
        }
    }
    println!("{} corpora in manifests", entries.len());
    for (tag, count) in &tags {
        println!("  {tag}: {count}");
    }
    // Report the usable corpus: the flat corpus/pdfs files plus any
    // extracted per-source subdirectories (SL-0.CORP.02 DoD: "total file
    // count ... reported by xtask corpus stats").
    let root = Path::new("corpus/pdfs");
    let mut flat = 0usize;
    let mut by_source: BTreeMap<String, usize> = BTreeMap::new();
    if let Ok(rd) = std::fs::read_dir(root) {
        for entry in rd.flatten() {
            let path = entry.path();
            if path.is_dir() {
                let n = count_pdfs(&path);
                if n > 0 {
                    by_source.insert(
                        path.file_name()
                            .unwrap_or_default()
                            .to_string_lossy()
                            .into_owned(),
                        n,
                    );
                }
            } else if path.extension().is_some_and(|e| e == "pdf") {
                flat += 1;
            }
        }
    }
    println!(
        "  total pdfs in corpus/pdfs: {}",
        flat + by_source.values().sum::<usize>()
    );
    println!("    flat: {flat}");
    for (src, n) in &by_source {
        println!("    {src}: {n}");
    }
    Ok(())
}

/// Count `*.pdf` files under a directory, recursively.
pub(crate) fn count_pdfs(dir: &Path) -> usize {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return 0;
    };
    let mut n = 0usize;
    for entry in rd.flatten() {
        let path = entry.path();
        if path.is_dir() {
            n = n.saturating_add(count_pdfs(&path));
        } else if path.extension().is_some_and(|x| x == "pdf") {
            n = n.saturating_add(1);
        }
    }
    n
}

/// Collect every `*.pdf` path under `corpus/pdfs`, returning paths relative to
/// the pdfs root (used for both expectation ids and verification lookups).
fn collect_pdfs() -> Vec<(String, PathBuf)> {
    let root = PathBuf::from("corpus/pdfs");
    let mut out = Vec::new();
    collect_pdfs_inner(&root, &root, &mut out);
    out.sort();
    out
}

fn collect_pdfs_inner(root: &Path, dir: &Path, out: &mut Vec<(String, PathBuf)>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in rd.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_pdfs_inner(root, &path, out);
        } else if path.extension().is_some_and(|x| x == "pdf") {
            let rel = path.strip_prefix(root).unwrap_or(&path);
            let id = rel.to_string_lossy().replace('\\', "/").replace(".pdf", "");
            out.push((id, path));
        }
    }
}

/// The expectation record of one corpus file (SL-0.CORP.03):
/// `corpus/expect/<id>.toml`.
///
/// The open outcome (`open`/`code`/`pages`) is the original CORP.03 record.
/// The `[render]` table (golden page-1 render hashes per DPI) and the
/// `[text]` table (the normalised page-1 extracted-text hash) are recorded
/// by `corpus expect-merge` from a CONF.01 text-sweep `golden.jsonl`, with
/// selis's own output as the regression baseline. The `[annotation]` table
/// (SL-0.ORACLE.05 triage verdicts) is preserved across regenerations.
#[derive(serde::Serialize, serde::Deserialize, Debug, Default)]
struct ExpectRecord {
    open: String,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    code: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pages: Option<usize>,
    /// Golden page-1 render hashes, DPI → sha256 hex of the PPM bytes the
    /// selis CLI wrote. Absent until `corpus expect-merge` records them.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    render: Option<BTreeMap<String, String>>,
    /// Golden normalised page-1 extracted-text hash. Absent until
    /// `corpus expect-merge` records it.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    text: Option<TextGolden>,
}

/// The golden extracted-text hash: sha256 over the normalised (N1–N6)
/// page-1 text in UTF-8.
#[derive(serde::Serialize, serde::Deserialize, Debug, Default)]
struct TextGolden {
    hash: String,
}

fn open_outcome(path: &Path) -> ExpectRecord {
    let budget = selis_sandbox::Budget::profile(selis_sandbox::Surface::Viewer);
    let clock = selis_sandbox::shell_clock();
    match selis_pdf_engine::Session::open(std::fs::read(path).unwrap_or_default(), &budget, &clock)
    {
        Ok(session) => ExpectRecord {
            open: "ok".to_string(),
            code: None,
            pages: Some(session.len()),
            render: None,
            text: None,
        },
        Err(e) => ExpectRecord {
            open: "err".to_string(),
            code: Some(format!("{:?}", e.code())),
            pages: None,
            render: None,
            text: None,
        },
    }
}

/// Record CORP.03 golden render hashes per DPI + the extracted-text hash
/// from a CONF.01 text-sweep `golden.jsonl` into `corpus/expect/<id>.toml`.
///
/// Rules, all reported in the summary:
/// * only files whose recorded open outcome is `ok` receive goldens (a
///   render/text baseline for a file that does not open is meaningless);
/// * the current open outcome must still equal the recorded one — a file
///   whose behaviour drifted between the sweep and the merge is skipped
///   rather than critiqued against a stale baseline;
/// * existing `[annotation]` tables (SL-0.ORACLE.05 verdicts) are preserved
///   byte-for-byte; golden tables are (over)written, never the annotations;
/// * files with no golden entry, or a golden with neither renders nor text,
///   are skipped and counted with their reason.
fn expect_merge(from: &Path) -> Result<(), String> {
    let golden_path = from.join("golden.jsonl");
    let text = std::fs::read_to_string(&golden_path)
        .map_err(|e| format!("{}: {e}", golden_path.display()))?;
    let mut goldens: BTreeMap<String, SweepGolden> = BTreeMap::new();
    for line in text.lines() {
        if line.trim().is_empty() {
            continue;
        }
        let g: SweepGolden =
            serde_json::from_str(line).map_err(|e| format!("{}: {e}", golden_path.display()))?;
        goldens.insert(g.id.clone(), g);
    }
    let expect_dir = Path::new("corpus/expect");
    let files = collect_pdfs();
    let mut merged = 0usize;
    let mut full = 0usize;
    let mut skipped_not_ok = 0usize;
    let mut skipped_drift = 0usize;
    let mut skipped_no_golden = 0usize;
    let mut skipped_empty = 0usize;
    let mut examples: Vec<String> = Vec::new();
    for (id, path) in &files {
        let dest = expect_dir.join(format!("{id}.toml"));
        let old = std::fs::read_to_string(&dest).unwrap_or_default();
        let mut record: ExpectRecord = if old.trim().is_empty() {
            open_outcome(path)
        } else {
            toml::from_str(&old).map_err(|e| format!("{id}: {e}"))?
        };
        if record.open != "ok" {
            skipped_not_ok += 1;
            continue;
        }
        let actual = open_outcome(path);
        if actual.open != record.open || actual.code != record.code || actual.pages != record.pages
        {
            skipped_drift += 1;
            push_example(&mut examples, &format!("{id} (open drifted)"));
            continue;
        }
        let has_sweep_baseline = match goldens.get(id) {
            Some(g) => !(g.render.is_empty() && g.text.is_none()),
            None => false,
        };
        if !has_sweep_baseline {
            // The current sweep produced no baseline. Clear stale goldens so
            // `corpus verify --golden` cannot report a spurious drift against
            // a superseded binary; the open outcome is the sole contract.
            let stale = record.render.is_some() || record.text.is_some();
            record.render = None;
            record.text = None;
            if stale && !old.trim().is_empty() {
                let mut with_annotation =
                    toml::to_string(&record).map_err(|e| format!("{id}: {e}"))?;
                if let Some(annotation) = crate::synthetic::extract_annotation_table(&old) {
                    with_annotation.push_str(&annotation);
                }
                if let Some(parent) = dest.parent() {
                    let _ = std::fs::create_dir_all(parent);
                }
                write_expect(&dest, &with_annotation, id)?;
            }
            if goldens.contains_key(id) {
                skipped_empty += 1;
                push_example(&mut examples, &format!("{id} (no baseline)"));
            } else {
                skipped_no_golden += 1;
                push_example(&mut examples, &format!("{id} (no golden)"));
            }
            continue;
        }
        let g = goldens
            .get(id)
            .expect("has_sweep_baseline only true when the sweep recorded the id");
        // Overwrite the render/text fields from the sweep — the sweep is
        // authoritative. When the sweep recorded an empty render (all DPIs
        // failed) but a text hash succeeds, we still want the file's
        // `[text]` golden and NO stale `[render]` table from a previous
        // baseline; and symmetrically for the text-only case.
        record.render = if g.render.is_empty() {
            None
        } else {
            Some(g.render.clone())
        };
        record.text = g
            .text
            .as_ref()
            .map(|hash| TextGolden { hash: hash.clone() });
        if g.render.len() >= 3 && g.text.is_some() {
            full += 1;
        }
        let mut serialised = toml::to_string(&record).map_err(|e| format!("{id}: {e}"))?;
        if let Some(annotation) = crate::synthetic::extract_annotation_table(&old) {
            serialised.push_str(&annotation);
        }
        if let Some(parent) = dest.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        write_expect(&dest, &serialised, id)?;
        merged += 1;
    }
    println!(
        "corpus expect merge: {merged} merged ({full} full render+text), \
         {skipped_not_ok} skipped (open != ok), {skipped_drift} skipped (open drifted), \
         {skipped_no_golden} skipped (no golden), {skipped_empty} skipped (no baseline)"
    );
    for e in examples.iter().take(10) {
        println!("  skipped: {e}");
    }
    Ok(())
}

fn push_example(examples: &mut Vec<String>, s: &str) {
    if examples.len() < 64 {
        examples.push(s.to_string());
    }
}

/// Write an expectation record, retrying the transient Windows file locks a
/// concurrent indexer can hold on `corpus/expect` files (os error 32/1224:
/// sharing violation / user-mapped section). Six tries with a short backoff;
/// a lock held longer than that is a genuine failure.
fn write_expect(dest: &Path, body: &str, id: &str) -> Result<(), String> {
    let mut last = None;
    for attempt in 0..6 {
        if attempt > 0 {
            std::thread::sleep(std::time::Duration::from_millis(250));
        }
        match std::fs::write(dest, body) {
            Ok(()) => return Ok(()),
            Err(e) => last = Some(e),
        }
    }
    Err(format!("{id}: {}: {last:?}", dest.display()))
}

/// One `golden.jsonl` line as the text sweep wrote it (the merge reads only
/// the baseline fields; verdicts live in `verdicts.jsonl`).
#[derive(serde::Deserialize)]
struct SweepGolden {
    id: String,
    #[serde(default)]
    render: BTreeMap<String, String>,
    #[serde(default)]
    text: Option<String>,
}

/// Shared by `expect-generate` and the synthetic generator so both record
/// *actual* outcomes â€” the expectation set tracks reality, and `corpus verify`
/// flags any later drift.
pub(crate) fn open_outcome_toml(path: &Path) -> Result<String, String> {
    toml::to_string(&open_outcome(path)).map_err(|e| format!("{}: {e}", path.display()))
}

/// Write open-outcome expectations for every corpus PDF (SL-0.CORP.03).
/// Existing records keep their golden `[render]`/`[text]` tables and their
/// `[annotation]` triage verdicts — regeneration only refreshes the open
/// outcome, so a re-run can never silently wipe the CORP.03 goldens.
fn expect_generate() -> Result<(), String> {
    let expect_dir = Path::new("corpus/expect");
    std::fs::create_dir_all(expect_dir).map_err(|e| format!("cannot create corpus/expect: {e}"))?;
    let files = collect_pdfs();
    let mut ok = 0usize;
    let mut err = 0usize;
    for (id, path) in &files {
        let fresh = open_outcome(path);
        if fresh.open == "ok" {
            ok += 1;
        } else {
            err += 1;
        }
        let dest = expect_dir.join(format!("{id}.toml"));
        let old = std::fs::read_to_string(&dest).unwrap_or_default();
        let record: ExpectRecord = if old.trim().is_empty() {
            fresh
        } else {
            let mut kept: ExpectRecord = toml::from_str(&old).map_err(|e| format!("{id}: {e}"))?;
            kept.open = fresh.open;
            kept.code = fresh.code;
            kept.pages = fresh.pages;
            kept
        };
        // A file whose open outcome changed out from under a golden baseline
        // keeps its goldens: `verify` fails on the open drift (loudly), and
        // the next merge re-baselines the goldens once the drift is
        // understood — silently dropping them here would hide the change.
        let mut text = toml::to_string(&record).map_err(|e| format!("{id}: {e}"))?;
        if let Some(annotation) = crate::synthetic::extract_annotation_table(&old) {
            text.push_str(&annotation);
        }
        if let Some(parent) = dest.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        write_expect(&dest, &text, id)?;
    }
    println!(
        "corpus expect generate: {ok} ok, {err} err ({ok} pages-open of {} files)",
        files.len()
    );
    Ok(())
}

/// Re-open every corpus PDF and diff against its expectation record,
/// reporting any outcome changes as a typed diff (SL-0.CORP.03 DoD). With
/// `golden`, additionally re-render every recorded golden DPI and
/// re-extract the page-1 text, diffing the hashes — the full CORP.03
/// regression check. Any change fails: the command passes on a clean tree.
fn verify(golden: bool, selis: Option<&Path>) -> Result<(), String> {
    let expect_dir = Path::new("corpus/expect");
    let files = collect_pdfs();
    let golden_ctx = if golden {
        Some(golden_context(selis)?)
    } else {
        None
    };
    let mut checked = 0usize;
    let mut changed = 0usize;
    let mut missing = 0usize;
    for (id, path) in &files {
        let expect_path = expect_dir.join(format!("{id}.toml"));
        let expected: ExpectRecord = match std::fs::read_to_string(&expect_path) {
            Ok(text) => toml::from_str(&text).map_err(|e| format!("{id}: {e}"))?,
            Err(_) => {
                println!("  {id}: NO EXPECTATION");
                missing += 1;
                continue;
            }
        };
        let actual = open_outcome(path);
        let same = expected.open == actual.open
            && expected.code == actual.code
            && expected.pages == actual.pages;
        checked += 1;
        if !same {
            changed += 1;
            println!("  {id}: expected {:?} got {:?}", expected, actual);
            continue;
        }
        if let Some(ctx) = &golden_ctx {
            let diffs = verify_golden(ctx, path, &expected);
            if !diffs.is_empty() {
                changed += 1;
                for d in diffs {
                    println!("  {id}: {d}");
                }
            }
        }
    }
    println!("corpus verify: {checked} checked, {changed} changed, {missing} without expectation");
    if changed > 0 {
        return Err(format!(
            "corpus verify: {changed} changed of {checked} checked"
        ));
    }
    Ok(())
}

/// The executables and scratch space a golden verification needs: the
/// canonicalised selis CLI plus a worker tmp dir outside the repo.
struct GoldenContext {
    selis: PathBuf,
    tmp: PathBuf,
}

fn golden_context(selis: Option<&Path>) -> Result<GoldenContext, String> {
    let selis = crate::sweep::resolve_selis(selis)?;
    let tmp = std::env::temp_dir().join(format!("selis-golden-verify-{}", std::process::id()));
    std::fs::create_dir_all(&tmp).map_err(|e| format!("{}: {e}", tmp.display()))?;
    Ok(GoldenContext { selis, tmp })
}

/// Re-render every recorded golden DPI and re-extract the page-1 text,
/// returning the typed diffs (empty when the file still matches its
/// goldens). Files whose open outcome already drifted never reach here.
fn verify_golden(ctx: &GoldenContext, file: &Path, expected: &ExpectRecord) -> Vec<String> {
    let mut diffs = Vec::new();
    let timeout = std::time::Duration::from_secs(120);
    if let Some(render) = &expected.render {
        let mut dpis: Vec<&String> = render.keys().collect();
        dpis.sort();
        for dpi in dpis {
            let want = &render[dpi.as_str()];
            match golden_render_hash(ctx, file, dpi, timeout) {
                Ok(got) => {
                    if &got != want {
                        diffs.push(format!("render MISMATCH @{dpi}"));
                    }
                }
                Err(detail) => diffs.push(format!("render UNREPRODUCIBLE @{dpi} ({detail})")),
            }
        }
    }
    if let Some(text) = &expected.text {
        match golden_text_hash(ctx, file, timeout) {
            Ok(got) => {
                if got.as_deref() != Some(text.hash.as_str()) {
                    diffs.push("text MISMATCH".to_string());
                }
            }
            Err(detail) => diffs.push(format!("text UNREPRODUCIBLE ({detail})")),
        }
    }
    diffs
}

/// The golden render hash of one file at one DPI: `selis render --page 0`
/// into scratch, sha256 over exactly the bytes the CLI wrote.
fn golden_render_hash(
    ctx: &GoldenContext,
    file: &Path,
    dpi: &str,
    timeout: std::time::Duration,
) -> Result<String, String> {
    let out = ctx.tmp.join("verify.ppm");
    let _ = std::fs::remove_file(&out);
    let args = vec![
        "render".to_string(),
        "--page".to_string(),
        "0".to_string(),
        "--dpi".to_string(),
        dpi.to_string(),
        crate::sweep::path_str(file),
        crate::sweep::path_str(&out),
    ];
    crate::sweep::run_with_timeout(&ctx.selis, &args, timeout)
        .map_err(|f| crate::sweep::fail_text(&f))?;
    let bytes = std::fs::read(&out).map_err(|e| e.to_string())?;
    let _ = std::fs::remove_file(&out);
    Ok(crate::text_sweep::sha256_hex_bytes(&bytes))
}

/// The golden text hash of one file: `selis extract --format text --page 0`,
/// normalised N1–N6, sha256 over the UTF-8. `Ok(None)` is a successful
/// extraction with no text (an image-only page has no text baseline).
fn golden_text_hash(
    ctx: &GoldenContext,
    file: &Path,
    timeout: std::time::Duration,
) -> Result<Option<String>, String> {
    let out = ctx.tmp.join("verify.txt");
    let _ = std::fs::remove_file(&out);
    let args = vec![
        "extract".to_string(),
        "--format".to_string(),
        "text".to_string(),
        "--page".to_string(),
        "0".to_string(),
        crate::sweep::path_str(file),
    ];
    crate::text_sweep::run_to_file(&ctx.selis, &args, &out, timeout)
        .map_err(|f| crate::sweep::fail_text(&f))?;
    let bytes = std::fs::read(&out).map_err(|e| e.to_string())?;
    let _ = std::fs::remove_file(&out);
    let normalised = crate::text_norm::normalize(&String::from_utf8_lossy(&bytes));
    if normalised.is_empty() {
        return Ok(None);
    }
    Ok(Some(crate::text_sweep::sha256_hex_bytes(
        normalised.as_bytes(),
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn golden_record() -> ExpectRecord {
        ExpectRecord {
            open: "ok".to_string(),
            code: None,
            pages: Some(3),
            render: Some(BTreeMap::from([
                ("72".to_string(), "ab".repeat(32)),
                ("150".to_string(), "cd".repeat(32)),
                ("300".to_string(), "ef".repeat(32)),
            ])),
            text: Some(TextGolden {
                hash: "01".repeat(32),
            }),
        }
    }

    #[test]
    fn golden_record_round_trips_through_toml() {
        let text = toml::to_string(&golden_record()).expect("serialise");
        assert!(text.contains("open = \"ok\""));
        assert!(text.contains("pages = 3"));
        assert!(text.contains("[render]"));
        assert!(text.contains("[text]"));
        let back: ExpectRecord = toml::from_str(&text).expect("parse");
        assert_eq!(back.open, "ok");
        assert_eq!(back.pages, Some(3));
        assert_eq!(back.render.expect("render").len(), 3);
        assert_eq!(back.text.expect("text").hash, "01".repeat(32));
    }

    #[test]
    fn golden_record_stays_within_the_hygiene_bounds() {
        // check-wild-hygiene caps expectation records at 768 bytes with no
        // line over 160 chars: golden hashes are metadata, and the record
        // must prove it fits.
        let text = toml::to_string(&golden_record()).expect("serialise");
        assert!(text.len() <= 768, "golden record is {} bytes", text.len());
        for line in text.lines() {
            assert!(line.len() <= 160, "overlong line: {line}");
        }
    }

    #[test]
    fn open_only_record_has_no_golden_tables() {
        let record = ExpectRecord {
            open: "err".to_string(),
            code: Some("TrailerMissingRoot".to_string()),
            pages: None,
            render: None,
            text: None,
        };
        let text = toml::to_string(&record).expect("serialise");
        assert!(!text.contains("[render]"));
        assert!(!text.contains("[text]"));
        let back: ExpectRecord = toml::from_str(&text).expect("parse");
        assert_eq!(back.open, "err");
        assert!(back.render.is_none() && back.text.is_none());
    }

    #[test]
    fn sweep_golden_line_parses_into_the_merge_shape() {
        let line = r#"{"id":"flat/x","render":{"72":"aa","150":"bb","300":"cc"},"render_err":{},"text":"dd","text_chars":12,"text_err":null}"#;
        let g: SweepGolden = serde_json::from_str(line).expect("parse");
        assert_eq!(g.id, "flat/x");
        assert_eq!(g.render.len(), 3);
        assert_eq!(g.text.expect("text"), "dd");
    }
}
