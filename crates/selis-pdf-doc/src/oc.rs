//! Optional content (SL-1.DOC.04).
//!
//! The OCG/OCMD model: optional content groups (layers), their visibility,
//! and optional-content membership dictionaries. Needed early because it
//! affects both rendering and redaction correctness.

use selis_error::Result;
use selis_pdf_cos::{Obj, Ref};
use selis_sandbox::{Budget, BudgetGuard};

use crate::Resolver;

/// An optional content group (OCG) — a named layer.
#[derive(Debug, Clone, PartialEq)]
pub struct OcGroup {
    /// The object reference of the group.
    pub ref_: Ref,
    /// The group's `/Name`.
    pub name: Option<selis_bytes::Bytes>,
    /// The group's `/Intent` (view, design, ...).
    pub intent: Option<selis_bytes::Bytes>,
    /// The group's `/Usage` dictionary (usage-application info).
    pub usage: Option<Obj>,
}

/// An optional content membership dictionary (OCMD).
#[derive(Debug, Clone, PartialEq)]
pub struct OcMembership {
    /// The `/OCGs`: a single group or an array of groups.
    pub groups: Vec<Ref>,
    /// The `/VE` (visibility expression) array, when present.
    pub ve: Option<Vec<Obj>>,
    /// The `/P` policy: `/AllOn`, `/AnyOn`, `/AnyOff`, `/AllOff`.
    pub policy: Option<selis_bytes::Bytes>,
}

/// The document's optional-content configuration (`/OCProperties`).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct OcProperties {
    /// `/OCGs`: every optional content group in the document.
    pub groups: Vec<OcGroup>,
    /// `/D` default configuration.
    pub default: Option<OcConfig>,
    /// `/Configs`: named configurations.
    pub configs: Vec<OcConfig>,
}

/// An optional-content configuration (`/OCProperties /D` or a `/Configs` entry).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct OcConfig {
    /// The configuration name.
    pub name: Option<selis_bytes::Bytes>,
    /// The default state of every group: `/ON`, `/OFF`, or absent (inherit).
    pub base_state: Option<selis_bytes::Bytes>,
    /// `/ON` — groups explicitly on.
    pub on: Vec<Ref>,
    /// `/OFF` — groups explicitly off.
    pub off: Vec<Ref>,
}

impl OcProperties {
    /// Parse `/OCProperties` from the catalog.
    ///
    /// # Budget
    ///
    /// Bounded by the group count and the budget.
    ///
    /// # Malformed Input
    ///
    /// Missing or malformed sections yield defaults; a non-array `/OCGs` is
    /// skipped.
    pub fn resolve(
        resolver: &mut Resolver<'_>,
        catalog: &Obj,
        budget: &Budget,
        g: &mut BudgetGuard<'_>,
    ) -> Result<Self> {
        let mut out = Self::default();
        let Some(Obj::Dict(pairs)) = catalog_dict(catalog, b"OCProperties") else {
            return Ok(out);
        };
        let get = |key: &[u8]| -> Option<&Obj> {
            pairs
                .iter()
                .find(|(k, _)| k.as_slice() == key)
                .map(|(_, v)| v)
        };

        // /OCGs: array of group refs.
        if let Some(Obj::Array(refs)) = get(b"OCGs") {
            for r in refs {
                if let Obj::Ref(r) = r {
                    let group = resolve_group(resolver, *r, budget, g);
                    out.groups.push(group);
                }
            }
        }

        // /D: default configuration.
        if let Some(d) = get(b"D") {
            out.default = Some(resolve_config(resolver, d, budget, g));
        }

        // /Configs: array of named configurations.
        if let Some(Obj::Array(configs)) = get(b"Configs") {
            for c in configs {
                out.configs.push(resolve_config(resolver, c, budget, g));
            }
        }
        Ok(out)
    }

    /// Is a group visible under a given configuration?
    ///
    /// A group is visible unless it is explicitly `/OFF` in the configuration
    /// and not overridden by `/ON`.
    #[must_use]
    pub fn is_visible(&self, group: Ref, config: Option<&OcConfig>) -> bool {
        let cfg = config.or(self.default.as_ref());
        let Some(cfg) = cfg else {
            return true; // no configuration: everything visible
        };
        if cfg.on.contains(&group) {
            return true;
        }
        if cfg.off.contains(&group) {
            return false;
        }
        match cfg.base_state.as_ref().map(|n| n.as_slice()) {
            Some(b"OFF") => false,
            _ => true,
        }
    }
}

fn resolve_group(
    resolver: &mut Resolver<'_>,
    r: Ref,
    _budget: &Budget,
    g: &mut BudgetGuard<'_>,
) -> OcGroup {
    let Ok(Obj::Dict(pairs)) = resolver.resolve(r, g) else {
        return OcGroup {
            ref_: r,
            name: None,
            intent: None,
            usage: None,
        };
    };
    let get = |key: &[u8]| -> Option<selis_bytes::Bytes> {
        pairs
            .iter()
            .find(|(k, _)| k.as_slice() == key)
            .and_then(|(_, v)| match v {
                Obj::Name(n) | Obj::String(n) => Some(n.clone()),
                _ => None,
            })
    };
    OcGroup {
        ref_: r,
        name: get(b"Name"),
        intent: get(b"Intent"),
        usage: pairs
            .iter()
            .find(|(k, _)| k.as_slice() == b"Usage")
            .map(|(_, v)| v.clone()),
    }
}

fn resolve_config(
    resolver: &mut Resolver<'_>,
    obj: &Obj,
    _budget: &Budget,
    g: &mut BudgetGuard<'_>,
) -> OcConfig {
    let dict = match obj {
        Obj::Ref(r) => resolver.resolve(*r, g).ok(),
        other => Some(other.clone()),
    };
    let Some(Obj::Dict(pairs)) = dict else {
        return OcConfig::default();
    };
    let get = |key: &[u8]| -> Option<&Obj> {
        pairs
            .iter()
            .find(|(k, _)| k.as_slice() == key)
            .map(|(_, v)| v)
    };
    let refs_of = |key: &[u8]| -> Vec<Ref> {
        get(key)
            .map(|v| match v {
                Obj::Array(items) => items
                    .iter()
                    .filter_map(|i| match i {
                        Obj::Ref(r) => Some(*r),
                        _ => None,
                    })
                    .collect(),
                Obj::Ref(r) => vec![*r],
                _ => Vec::new(),
            })
            .unwrap_or_default()
    };
    OcConfig {
        name: get(b"Name").and_then(|v| match v {
            Obj::Name(n) | Obj::String(n) => Some(n.clone()),
            _ => None,
        }),
        base_state: get(b"BaseState").and_then(|v| match v {
            Obj::Name(n) => Some(n.clone()),
            _ => None,
        }),
        on: refs_of(b"ON"),
        off: refs_of(b"OFF"),
    }
}

fn catalog_dict<'a>(catalog: &'a Obj, key: &[u8]) -> Option<&'a Obj> {
    match catalog {
        Obj::Dict(pairs) => pairs
            .iter()
            .find(|(k, _)| k.as_slice() == key)
            .map(|(_, v)| v),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::indexing_slicing)]

    use super::*;

    fn dict(pairs: Vec<(&[u8], Obj)>) -> Obj {
        Obj::Dict(
            pairs
                .into_iter()
                .map(|(k, v)| (selis_bytes::Bytes::copy_from_slice(k), v))
                .collect(),
        )
    }

    #[test]
    fn visibility_respects_on_off() {
        let g1 = Ref::new(1, 0);
        let g2 = Ref::new(2, 0);
        let props = OcProperties {
            groups: Vec::new(),
            default: Some(OcConfig {
                name: None,
                base_state: Some(selis_bytes::Bytes::copy_from_slice(b"OFF")),
                on: vec![g1],
                off: vec![g2],
            }),
            configs: Vec::new(),
        };
        // g1 is /ON: visible. g2 is /OFF: hidden. A third group inherits the
        // OFF base state: hidden.
        assert!(props.is_visible(g1, None));
        assert!(!props.is_visible(g2, None));
        assert!(!props.is_visible(Ref::new(3, 0), None));
    }

    #[test]
    fn no_config_means_everything_visible() {
        let props = OcProperties::default();
        assert!(props.is_visible(Ref::new(1, 0), None));
    }
}
