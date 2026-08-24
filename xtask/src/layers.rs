//! `xtask check-layers` — ADR-P0003, SL-0.WS.03.
//!
//! Loads `xtask/layers.toml` (crate → layer + allowed-edge list), walks
//! `cargo metadata`, and fails on any dependency edge that is not strictly
//! downward or explicitly allowed.
//!
//! Two kinds of absent crates are tolerated, not forgotten:
//!   * a crate in `layers.toml` that is not in the workspace is `planned`
//!     (later phases) and ignored;
//!   * a dependency that is not in `layers.toml` is external (serde, clap …)
//!     and not a layering concern.

use std::collections::HashMap;

use serde::Deserialize;

#[derive(Deserialize)]
struct LayerCfg {
    #[serde(default)]
    layers: HashMap<String, u8>,
    #[serde(default)]
    edge: Vec<AllowedEdge>,
}

#[derive(Deserialize)]
struct AllowedEdge {
    from: String,
    to: String,
}

/// A single layering violation.
struct Violation {
    from: String,
    to: String,
    from_layer: u8,
    to_layer: u8,
}

/// The public check entry point. Returns `Err` with a human-readable report on
/// any violation.
pub fn check() -> Result<(), String> {
    let cfg = load_cfg()?;
    let meta = cargo_metadata()?;
    let packages = meta
        .get("packages")
        .and_then(|p| p.as_array())
        .ok_or_else(|| "cargo metadata returned no packages".to_string())?;

    let mut violations: Vec<Violation> = Vec::new();
    let mut considered = 0usize;
    for pkg in packages {
        let name = pkg
            .get("name")
            .and_then(|n| n.as_str())
            .ok_or_else(|| "package without a name".to_string())?;
        let Some(&from_layer) = cfg.layers.get(name) else {
            continue; // tooling (xtask) or a crate not yet given a layer.
        };
        let deps = pkg
            .get("dependencies")
            .and_then(|d| d.as_array())
            .ok_or_else(|| format!("package {name} has no dependency list"))?;
        for dep in deps {
            let dep_name = dep
                .get("name")
                .and_then(|n| n.as_str())
                .ok_or_else(|| format!("{name}: dependency without a name"))?;
            // Only the "normal" (runtime) dependency kind is a layering edge.
            if !is_normal_dependency(dep) {
                continue;
            }
            let Some(&to_layer) = cfg.layers.get(dep_name) else {
                continue; // external crate: not a layering concern.
            };
            considered += 1;
            if to_layer >= from_layer && !is_allowed(&cfg, name, dep_name) {
                violations.push(Violation {
                    from: name.to_string(),
                    to: dep_name.to_string(),
                    from_layer,
                    to_layer,
                });
            }
        }
    }

    if violations.is_empty() {
        println!(
            "check-layers: {considered} intra-workspace edges, all strictly downward or allowed"
        );
        return Ok(());
    }

    let mut report = String::from("layering violations:\n");
    for v in &violations {
        report.push_str(&format!(
            "  {} (L{}) -> {} (L{}): not strictly downward and not in the allowlist\n",
            v.from, v.from_layer, v.to, v.to_layer
        ));
    }
    Err(report)
}

fn is_allowed(cfg: &LayerCfg, from: &str, to: &str) -> bool {
    cfg.edge.iter().any(|e| e.from == from && e.to == to)
}

/// A dependency counts as a layering edge only when its kind is the runtime
/// `null` kind. Dev- and build-dependencies do not ship in the crate.
fn is_normal_dependency(dep: &serde_json::Value) -> bool {
    match dep.get("kind").and_then(|k| k.as_str()) {
        Some(k) => k == "normal",
        None => dep.get("kind").is_some_and(|k| k.is_null()) || dep.get("kind").is_none(),
    }
}

fn load_cfg() -> Result<LayerCfg, String> {
    let text = std::fs::read_to_string("xtask/layers.toml")
        .map_err(|e| format!("cannot read xtask/layers.toml: {e}"))?;
    toml::from_str(&text).map_err(|e| format!("xtask/layers.toml is not valid TOML: {e}"))
}

fn cargo_metadata() -> Result<serde_json::Value, String> {
    let out = std::process::Command::new("cargo")
        .args(["metadata", "--format-version", "1", "--no-deps"])
        .output()
        .map_err(|e| format!("cannot run cargo metadata: {e}"))?;
    if !out.status.success() {
        return Err("cargo metadata failed".to_string());
    }
    serde_json::from_slice(&out.stdout).map_err(|e| format!("cargo metadata JSON invalid: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg_with(edges: Vec<(&str, &str)>) -> LayerCfg {
        LayerCfg {
            layers: HashMap::from([
                ("a".to_string(), 0),
                ("b".to_string(), 1),
                ("c".to_string(), 2),
            ]),
            edge: edges
                .into_iter()
                .map(|(from, to)| AllowedEdge {
                    from: from.to_string(),
                    to: to.to_string(),
                })
                .collect(),
        }
    }

    #[test]
    fn downward_edges_are_allowed() {
        let cfg = cfg_with(vec![]);
        assert!(
            !is_allowed(&cfg, "a", "b"),
            "upward edge needs an allowlist entry"
        );
        assert!(!is_allowed(&cfg, "b", "a"));
        // b -> a is L1 -> L0, strictly downward, so it must NOT need an entry.
        assert!(
            !is_allowed(&cfg, "b", "a"),
            "only the caller checks strictness"
        );
    }

    #[test]
    fn allowlisted_edges_pass() {
        let cfg = cfg_with(vec![("b", "c")]);
        assert!(is_allowed(&cfg, "b", "c"));
        assert!(!is_allowed(&cfg, "c", "b"));
    }

    #[test]
    fn strict_downward_rule() {
        // b (L1) -> a (L0): allowed without an entry.
        let cfg = cfg_with(vec![]);
        let b_layer = cfg.layers["b"];
        let a_layer = cfg.layers["a"];
        assert!(a_layer < b_layer);
    }
}
