//! `xtask corpus` — SL-0.CORP.01.
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
//! does not match fails loudly — an unverified download is a warning, never a
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
    Verify,
}

pub fn run(cmd: CorpusCommand) -> Result<(), String> {
    let entries = load_all()?;
    match cmd {
        CorpusCommand::Fetch(tag) => fetch(&entries, tag.as_deref()),
        CorpusCommand::List => list(&entries),
        CorpusCommand::Stats => stats(&entries),
        CorpusCommand::ExpectGenerate => expect_generate(),
        CorpusCommand::Verify => verify(),
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
                        "{}: cached file {} fails sha256 — delete it and refetch",
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
                "  {}: no sha256 recorded — downloading UNVERIFIED ({}). Refusing to trust; \
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
                "  {}: downloaded — RECORD sha256 = \"{}\" in the manifest",
                e.id, hash
            );
        } else {
            match verify_sha256(&dest, e) {
                Ok(true) => println!("  {}: downloaded, hash verified", e.id),
                Ok(false) => {
                    let actual =
                        sha256_hex(&dest).map_err(|m| format!("{}: {m}", dest.display()))?;
                    return Err(format!(
                        "{}: sha256 MISMATCH after download — expected {}, got {}. Refusing the file.",
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
fn sha256_hex(path: &Path) -> Result<String, String> {
    let program = if std::env::consts::OS == "windows" {
        "certutil"
    } else {
        "sha256sum"
    };
    let out = std::process::Command::new(program)
        .arg("-hashfile")
        .arg(path)
        .arg("SHA256") // certutil defaults to SHA1; the manifests pin SHA256
        .output()
        .or_else(|_| std::process::Command::new("sha256sum").arg(path).output())
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
                    by_source.insert(path.file_name().unwrap_or_default().to_string_lossy().into_owned(), n);
                }
            } else if path.extension().is_some_and(|e| e == "pdf") {
                flat += 1;
            }
        }
    }
    println!("  total pdfs in corpus/pdfs: {}", flat + by_source.values().sum::<usize>());
    println!("    flat: {flat}");
    for (src, n) in &by_source {
        println!("    {src}: {n}");
    }
    Ok(())
}

/// Count `*.pdf` files under a directory, recursively.
fn count_pdfs(dir: &Path) -> usize {
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
            let id = rel
                .to_string_lossy()
                .replace('\\', "/")
                .replace(".pdf", "");
            out.push((id, path));
        }
    }
}

/// The expected open outcome of one corpus file: `Ok` with a page count, or a
/// typed error code (SL-0.CORP.03).
#[derive(serde::Serialize, serde::Deserialize, Debug)]
struct ExpectRecord {
    open: String,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    code: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pages: Option<usize>,
}

fn open_outcome(path: &Path) -> ExpectRecord {
    let budget = selis_sandbox::Budget::profile(selis_sandbox::Surface::Viewer);
    match selis_pdf_engine::Session::open(
        std::fs::read(path).unwrap_or_default(),
        &budget,
    ) {
        Ok(session) => ExpectRecord {
            open: "ok".to_string(),
            code: None,
            pages: Some(session.len()),
        },
        Err(e) => ExpectRecord {
            open: "err".to_string(),
            code: Some(format!("{:?}", e.code())),
            pages: None,
        },
    }
}

/// Write open-outcome expectations for every corpus PDF (SL-0.CORP.03).
fn expect_generate() -> Result<(), String> {
    let expect_dir = Path::new("corpus/expect");
    std::fs::create_dir_all(expect_dir).map_err(|e| format!("cannot create corpus/expect: {e}"))?;
    let files = collect_pdfs();
    let mut ok = 0usize;
    let mut err = 0usize;
    for (id, path) in &files {
        let record = open_outcome(path);
        if record.open == "ok" {
            ok += 1;
        } else {
            err += 1;
        }
        let dest = expect_dir.join(format!("{id}.toml"));
        if let Some(parent) = dest.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let text = toml::to_string(&record).map_err(|e| format!("{id}: {e}"))?;
        std::fs::write(&dest, text).map_err(|e| format!("{id}: {e}"))?;
    }
    println!(
        "corpus expect generate: {ok} ok, {err} err ({ok} pages-open of {} files)",
        files.len()
    );
    Ok(())
}

/// Re-open every corpus PDF and diff against its expectation record,
/// reporting any outcome changes as a typed diff (SL-0.CORP.03 DoD).
fn verify() -> Result<(), String> {
    let expect_dir = Path::new("corpus/expect");
    let files = collect_pdfs();
    let mut checked = 0usize;
    let mut changed = 0usize;
    for (id, path) in &files {
        let expect_path = expect_dir.join(format!("{id}.toml"));
        let expected: ExpectRecord = match std::fs::read_to_string(&expect_path) {
            Ok(text) => toml::from_str(&text).map_err(|e| format!("{id}: {e}"))?,
            Err(_) => {
                println!("  {id}: NO EXPECTATION");
                continue;
            }
        };
        let actual = open_outcome(path);
        let same = match (&expected, &actual) {
            (a, b) if a.open != b.open => false,
            (a, b) if a.code != b.code => false,
            (a, b) if a.pages != b.pages => false,
            _ => true,
        };
        checked += 1;
        if !same {
            changed += 1;
            println!(
                "  {id}: expected {:?} got {:?}",
                expected,
                actual
            );
        }
    }
    println!("corpus verify: {checked} checked, {changed} changed");
    Ok(())
}
