//! `xtask corpus wild` — the policy gate on wild-corpus acquisition (SL-0.CORP.05,
//! `pdf-plan/06-CORPUS-POLICY.md` §7.1).
//!
//! Wild files (Common Crawl-derived, govdocs1, SAFEDOCS) carry third-party copyright and
//! possible PII, so fetching one is a policy-loaded operation, not a convenience download.
//! This subcommand enforces the policy mechanically:
//!
//! - refuses to run without `--i-have-read-the-policy` (§7.1),
//! - refuses a cache directory inside the repo (§2: wild bytes are never committed),
//! - refuses a destination volume it can prove is not encrypted, warns when it cannot
//!   verify (§3: encrypted at rest),
//! - checks free space before downloading (§3: the cache is disposable, not sacred),
//! - writes a provenance record for every artifact (§5).
//!
//! Like `corpus fetch`, downloads go through `curl` and extraction through system tools, so
//! the xtask binary stays dependency-free. Primary route is SAFEDOCS
//! (CC-MAIN-2021-31-PDF-UNTRUNCATED on Digital Corpora): untruncated, deduplicated PDFs from
//! the Common Crawl CC-MAIN-2021-31 crawl, packaged as ~1000-PDF zips with published
//! provenance metadata.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

const SAFEDOCS_BASE: &str =
    "https://downloads.digitalcorpora.org/corpora/files/CC-MAIN-2021-31-PDF-UNTRUNCATED/zipfiles";
const POLICY_DOC: &str = "pdf-plan/06-CORPUS-POLICY.md";
/// SAFEDOCS zips run 1.0–2.8 GB; a 3 GB-per-zip floor keeps a batch from filling the disk.
const FREE_GB_PER_ZIP: u64 = 3;

pub enum WildCommand {
    Fetch {
        zips: usize,
        zip_start: usize,
        ack: bool,
    },
    Status,
}

pub fn run(cmd: WildCommand) -> Result<(), String> {
    match cmd {
        WildCommand::Fetch {
            zips,
            zip_start,
            ack,
        } => fetch(zips, zip_start, ack),
        WildCommand::Status => status(),
    }
}

fn fetch(zips: usize, zip_start: usize, ack: bool) -> Result<(), String> {
    if !ack {
        return Err(format!(
            "refusing to fetch wild corpus: read {POLICY_DOC} first (fetch-only, encrypted at \
             rest, no redistribution, no human review without cause), then pass \
             --i-have-read-the-policy"
        ));
    }
    if zips == 0 {
        return Err("--zips must be at least 1".to_string());
    }

    let batch = batch_dir()?;
    ensure_outside_repo(&batch)?;
    ensure_volume_policy(&batch, zips)?;
    std::fs::create_dir_all(&batch)
        .map_err(|e| format!("cannot create {}: {e}", batch.display()))?;
    println!(
        "wild batch: {} ({} zips from #{}, policy {POLICY_DOC})",
        batch.display(),
        zips,
        zip_start
    );

    let mut total_pdfs = 0usize;
    for i in zip_start..zip_start + zips {
        let (url, name) = safedocs_zip(i)?;
        let zip_path = batch.join(&name);
        if !zip_path.exists() {
            println!("  fetching {name} ...");
            download(&url, &zip_path)?;
        }
        let hash = super::corpus::sha256_hex(&zip_path)?;
        let bytes = std::fs::metadata(&zip_path)
            .map_err(|e| format!("cannot stat {}: {e}", zip_path.display()))?
            .len();
        write_provenance(&batch, &name, &hash, bytes, &url)?;

        let extract_dir = batch.join(name.trim_end_matches(".zip"));
        if !extract_dir.exists() {
            extract(&zip_path, &extract_dir)?;
        }
        let pdfs = super::corpus::count_pdfs(&extract_dir);
        total_pdfs += pdfs;
        println!("  {name}: sha256 {hash}, {pdfs} pdfs (batch total {total_pdfs})");
    }
    println!(
        "done: {total_pdfs} pdfs in {} — cache is local-only; never commit, redistribute, or \
         upload these files",
        batch.display()
    );
    Ok(())
}

/// `batch-<unix>` under the user-level wild cache (outside any repo checkout).
fn batch_dir() -> Result<PathBuf, String> {
    let home = std::env::var("USERPROFILE")
        .or_else(|_| std::env::var("HOME"))
        .map_err(|_| "cannot resolve the user home directory".to_string())?;
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    Ok(PathBuf::from(home)
        .join(".cache")
        .join("selis-corpus")
        .join("wild")
        .join(format!("batch-{ts}")))
}

/// Refuse a destination inside the repo (policy §2: wild bytes never enter git).
fn ensure_outside_repo(batch: &Path) -> Result<(), String> {
    let mut dir = std::env::current_dir().map_err(|e| format!("cannot get cwd: {e}"))?;
    loop {
        if dir.join(".git").exists() {
            let repo = dir
                .canonicalize()
                .map_err(|e| format!("cannot canonicalize {}: {e}", dir.display()))?;
            let created = std::fs::create_dir_all(batch)
                .map_err(|e| format!("cannot create {}: {e}", batch.display()))?;
            let _ = created;
            let resolved = batch
                .canonicalize()
                .map_err(|e| format!("cannot canonicalize {}: {e}", batch.display()))?;
            if resolved.starts_with(&repo) {
                return Err(format!(
                    "policy §2: wild cache {} is inside the repo {} — wild files are never \
                     committed; use a directory outside the checkout",
                    batch.display(),
                    repo.display()
                ));
            }
            return Ok(());
        }
        if !dir.pop() {
            return Ok(()); // no repo root above us; nothing to refuse
        }
    }
}

/// §3: refuse a volume that Windows can prove is unprotected; warn when unverifiable.
fn ensure_volume_policy(batch: &Path, zips: usize) -> Result<(), String> {
    let Some(volume) = volume_of(batch) else {
        println!(
            "  warning: cannot determine the destination volume; encryption at rest unverified"
        );
        return Ok(());
    };
    match volume_encrypted(&volume) {
        Ok(false) => {
            return Err(format!(
                "policy §3: volume {volume} reports BitLocker protection OFF — wild files must \
                 be stored encrypted at rest"
            ));
        }
        Ok(true) => println!("  volume {volume}: BitLocker protection on"),
        Err(_) => println!(
            "  warning: cannot verify encryption on {volume} (manage-bde unavailable) — ensure \
             the volume is encrypted (policy §3)"
        ),
    }
    let free = free_bytes(&volume)?;
    let need = FREE_GB_PER_ZIP * zips as u64;
    let free_gb = free / (1024 * 1024 * 1024);
    if free < need * (1024 * 1024 * 1024) {
        return Err(format!(
            "policy §3: only {free_gb} GB free on {volume}; a batch of {zips} zips wants ≥ {need} GB"
        ));
    }
    println!("  volume {volume}: {free_gb} GB free (batch needs ≥ {need} GB)");
    Ok(())
}

fn volume_of(path: &Path) -> Option<String> {
    let text = path.to_str()?;
    let idx = text.find(':')?;
    let letter = text.chars().nth(idx.checked_sub(1)?)?;
    Some(format!("{letter}:"))
}

/// `Ok(false)` = provably unprotected; `Err` = could not verify (caller warns).
fn volume_encrypted(volume: &str) -> Result<bool, String> {
    let out = Command::new("manage-bde")
        .args(["-status", volume])
        .output()
        .map_err(|e| format!("manage-bde: {e}"))?;
    let text = String::from_utf8_lossy(&out.stdout);
    for line in text.lines() {
        let lower = line.trim().to_ascii_lowercase();
        let Some(rest) = lower.strip_prefix("protection status") else {
            continue;
        };
        if rest.contains("off") {
            return Ok(false);
        }
        if rest.contains("on") {
            return Ok(true);
        }
    }
    Err("no protection-status line in manage-bde output".to_string())
}

fn free_bytes(volume: &str) -> Result<u64, String> {
    let script = format!("(New-Object IO.DriveInfo('{volume}')).AvailableFreeSpace");
    let out = Command::new("powershell")
        .args(["-NoProfile", "-Command", &script])
        .output()
        .map_err(|e| format!("powershell: {e}"))?;
    String::from_utf8_lossy(&out.stdout)
        .trim()
        .parse::<u64>()
        .map_err(|e| format!("cannot parse free-space output: {e}"))
}

/// Zip index → (URL, local name). The group boundary arithmetic is verified against the
/// bucket listing (`zipfiles/0000-0999/0000.zip`, `zipfiles/1000-1999/1000.zip` → 200).
fn safedocs_zip(index: usize) -> Result<(String, String), String> {
    if index >= 7933 {
        return Err(format!(
            "SAFEDOCS has 7,933 zips (0000..7932); #{index} is out of range"
        ));
    }
    let group = (index / 1000) * 1000;
    let url = format!(
        "{SAFEDOCS_BASE}/{group:04}-{:04}/{index:04}.zip",
        group + 999
    );
    Ok((url, format!("{index:04}.zip")))
}

fn download(url: &str, dest: &Path) -> Result<(), String> {
    let status = Command::new("curl")
        .args([
            "--fail",
            "--location",
            "--silent",
            "--show-error",
            "--retry",
            "3",
            "--connect-timeout",
            "30",
            "--output",
        ])
        .arg(dest)
        .arg(url)
        .status()
        .map_err(|err| format!("cannot run curl: {err} (is curl installed?)"))?;
    if !status.success() {
        let _ = std::fs::remove_file(dest);
        return Err(format!("download failed: {url}"));
    }
    Ok(())
}

/// Provenance record (policy §5): one JSON line per artifact, written into the batch dir —
/// never committed. Fields are all tool-controlled, so string building is safe.
fn write_provenance(
    batch: &Path,
    name: &str,
    hash: &str,
    bytes: u64,
    url: &str,
) -> Result<(), String> {
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let line = format!(
        "{{\"zip\":\"{name}\",\"sha256\":\"{hash}\",\"bytes\":{bytes},\"crawl\":\
         \"CC-MAIN-2021-31\",\"source_url\":\"{url}\",\"fetched_unix\":{ts},\"policy\":\
         \"{POLICY_DOC}\"}}\n"
    );
    use std::io::Write;
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(batch.join("provenance.jsonl"))
        .map_err(|e| format!("cannot open provenance.jsonl: {e}"))?;
    file.write_all(line.as_bytes())
        .map_err(|e| format!("cannot write provenance.jsonl: {e}"))
}

fn extract(zip: &Path, dest: &Path) -> Result<(), String> {
    std::fs::create_dir_all(dest).map_err(|e| format!("cannot create {}: {e}", dest.display()))?;
    let script = format!(
        "Expand-Archive -LiteralPath '{}' -DestinationPath '{}' -Force",
        zip.display(),
        dest.display()
    );
    let status = Command::new("powershell")
        .args(["-NoProfile", "-Command", &script])
        .status()
        .map_err(|e| format!("cannot run powershell: {e}"))?;
    if !status.success() {
        return Err(format!("extraction failed for {}", zip.display()));
    }
    Ok(())
}

/// List wild batches and their PDF counts (`xtask corpus wild status`).
fn status() -> Result<(), String> {
    let home = std::env::var("USERPROFILE")
        .or_else(|_| std::env::var("HOME"))
        .map_err(|_| "cannot resolve the user home directory".to_string())?;
    let root = PathBuf::from(home)
        .join(".cache")
        .join("selis-corpus")
        .join("wild");
    if !root.is_dir() {
        println!("no wild batches under {}", root.display());
        return Ok(());
    }
    let mut batches: Vec<PathBuf> = std::fs::read_dir(&root)
        .map_err(|e| format!("cannot read {}: {e}", root.display()))?
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect();
    batches.sort();
    for batch in batches {
        let pdfs = super::corpus::count_pdfs(&batch);
        println!("{}: {pdfs} pdfs", batch.display());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zip_index_maps_to_group_layout() {
        let (url, name) = safedocs_zip(0).unwrap();
        assert!(url.ends_with("/zipfiles/0000-0999/0000.zip"), "{url}");
        assert_eq!(name, "0000.zip");
        let (url, _) = safedocs_zip(1000).unwrap();
        assert!(url.ends_with("/zipfiles/1000-1999/1000.zip"), "{url}");
        let (url, _) = safedocs_zip(7932).unwrap();
        assert!(url.ends_with("/zipfiles/7000-7999/7932.zip"), "{url}");
        assert!(safedocs_zip(7933).is_err());
    }

    #[test]
    fn volume_letter_is_parsed() {
        assert_eq!(volume_of(Path::new("C:\\Users\\x")).as_deref(), Some("C:"));
        assert_eq!(volume_of(Path::new("relative\\path")), None);
    }
}
