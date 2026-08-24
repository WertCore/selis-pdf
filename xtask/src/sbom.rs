//! `xtask sbom` — SL-0.WS.07. Emits a CycloneDX 1.5 SBOM from `cargo metadata`.
//!
//! Dependency-free (no `cargo cyclonedx` needed in phase 0): parses the
//! metadata JSON and writes a components list with licences. Fills the
//! supply-chain file the release train ships.

use std::collections::BTreeMap;

pub fn sbom() -> Result<(), String> {
    let meta = cargo_metadata()?;
    let packages = meta
        .get("packages")
        .and_then(|p| p.as_array())
        .ok_or_else(|| "cargo metadata returned no packages".to_string())?;

    // Only external (registry) packages ship in the SBOM; workspace members are
    // our own code and carry the proprietary licence already.
    let mut externals: Vec<(&serde_json::Value, &str)> = Vec::new();
    let mut licence_counts: BTreeMap<String, usize> = BTreeMap::new();
    for pkg in packages {
        let name = pkg.get("name").and_then(|n| n.as_str()).unwrap_or("?");
        let source = pkg.get("source").and_then(|s| s.as_str());
        if source.is_none() {
            continue; // path dependency (workspace member)
        }
        externals.push((pkg, name));
        let lic = pkg
            .get("license")
            .and_then(|l| l.as_str())
            .unwrap_or("UNKNOWN");
        *licence_counts.entry(lic.to_string()).or_insert(0) += 1;
    }

    let mut out = String::new();
    out.push_str("{\n");
    out.push_str("  \"bomFormat\": \"CycloneDX\",\n");
    out.push_str("  \"specVersion\": \"1.5\",\n");
    out.push_str("  \"serialNumber\": \"urn:uuid:00000000-0000-0000-0000-000000000000\",\n");
    out.push_str(&format!("  \"components\": [\n",));
    for (i, (pkg, name)) in externals.iter().enumerate() {
        let version = pkg
            .get("version")
            .and_then(|v| v.as_str())
            .unwrap_or("0.0.0");
        let lic = pkg
            .get("license")
            .and_then(|l| l.as_str())
            .unwrap_or("UNKNOWN");
        let comma = if i + 1 < externals.len() { "," } else { "" };
        out.push_str(&format!(
            "    {{\"type\": \"library\", \"name\": \"{}\", \"version\": \"{}\", \"licenses\": [{{\"license\": {{\"id\": \"{}\"}}}}]}}{}\n",
            name, version, lic, comma
        ));
    }
    out.push_str("  ]\n");
    out.push_str("}\n");

    std::fs::write("target/sbom.json", out)
        .map_err(|e| format!("cannot write target/sbom.json: {e}"))?;
    println!(
        "sbom: {} external components, {} distinct licences",
        externals.len(),
        licence_counts.len()
    );
    for (lic, n) in &licence_counts {
        println!("  {lic}: {n}");
    }
    println!("written to target/sbom.json");
    Ok(())
}

fn cargo_metadata() -> Result<serde_json::Value, String> {
    let out = std::process::Command::new("cargo")
        .args(["metadata", "--format-version", "1"])
        .output()
        .map_err(|e| format!("cannot run cargo metadata: {e}"))?;
    if !out.status.success() {
        return Err("cargo metadata failed".to_string());
    }
    serde_json::from_slice(&out.stdout).map_err(|e| format!("cargo metadata JSON invalid: {e}"))
}
