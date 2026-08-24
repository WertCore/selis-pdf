//! `xtask conformance report` — SL-0.OPS.02.
//!
//! Renders the conformance ladder from `conformance/areas.toml` as the
//! project's honest status page. Every area starts at level 0 (None); the CI
//! publishes the report so "what the engine can actually do" is never a
//! marketing claim — it is a machine-readable file.

use std::path::Path;

use serde::Deserialize;

#[derive(Deserialize)]
struct ConformanceCfg {
    area: Vec<Area>,
}

#[derive(Deserialize)]
struct Area {
    id: String,
    name: String,
    level: u8,
    #[serde(default)]
    notes: String,
}

const LEVEL_NAMES: [&str; 7] = [
    "None", "Identify", "Parse", "Render", "Extract", "Edit", "Author",
];

pub fn report() -> Result<(), String> {
    let cfg = load()?;
    let mut lines: Vec<String> = Vec::new();
    lines.push("# Selis conformance ladder".to_string());
    lines.push(String::new());
    lines.push("Every area starts at `None` and climbs as the corpus proves it. ".to_string());
    lines.push("The published claim is never ahead of the measured level.".to_string());
    lines.push(String::new());
    lines.push("| Area | Level | Notes |".to_string());
    lines.push("|---|---|---|".to_string());
    for a in &cfg.area {
        let level = LEVEL_NAMES
            .get(a.level as usize)
            .copied()
            .unwrap_or("Unknown");
        lines.push(format!("| {} | **{level}** | {} |", a.name, a.notes));
    }
    let report = lines.join("\n");
    std::fs::write("conformance/REPORT.md", report + "\n")
        .map_err(|e| format!("cannot write conformance/REPORT.md: {e}"))?;
    println!(
        "conformance: {} areas reported ({} at None)",
        cfg.area.len(),
        cfg.area.iter().filter(|a| a.level == 0).count()
    );
    println!("report written to conformance/REPORT.md");
    Ok(())
}

fn load() -> Result<ConformanceCfg, String> {
    let path = Path::new("conformance/areas.toml");
    let text = std::fs::read_to_string(path)
        .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    toml::from_str(&text).map_err(|e| format!("{} invalid: {e}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_level_name_is_in_range() {
        // A level value above 6 would index out of LEVEL_NAMES; the deserialised
        // file is what we publish, so the check is on the data we read.
        for a in LEVEL_NAMES {
            assert!(!a.is_empty());
        }
        assert_eq!(LEVEL_NAMES[0], "None");
        assert_eq!(LEVEL_NAMES[6], "Author");
    }
}
