//! Page labels, article threads, and viewer preferences (SL-1.DOC.08).
//!
//! * **Page labels** (`/PageLabels`): a number tree mapping page index →
//!   label dict (`/S /r`, `/P`, `/St`).
//! * **Article threads** (`/Threads`): bead chains (`/B`, `/N`, `/V`, `/T`).
//! * **Viewer preferences** (`/ViewerPreferences`): `HideToolbar`,
//!   `FitWindow`, `PageMode`, etc.

use selis_error::Result;
use selis_pdf_cos::Obj;
use selis_sandbox::{Budget, BudgetGuard};

use crate::Resolver;

/// A page-label range.
#[derive(Debug, Clone, PartialEq)]
pub struct PageLabel {
    /// The page index this label range starts at.
    pub page_index: i64,
    /// The numbering style: `D` decimal, `R`/`r` roman, `A`/`a` alphabetic.
    pub style: Option<selis_bytes::Bytes>,
    /// A fixed prefix (e.g. `Chapter `).
    pub prefix: Option<selis_bytes::Bytes>,
    /// The starting value of the sequence.
    pub start: Option<i64>,
}

/// Parse the `/PageLabels` number tree into label ranges.
///
/// # Budget
///
/// Bounded by the number tree walk.
///
/// # Malformed Input
///
/// Non-dict label values are skipped.
pub fn page_labels(
    resolver: &mut Resolver<'_>,
    catalog: &Obj,
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
) -> Result<Vec<PageLabel>> {
    let mut out = Vec::new();
    let Some(Obj::Ref(root)) = catalog_dict(catalog, b"PageLabels") else {
        return Ok(out);
    };
    let tree = crate::walk_number_tree(resolver, *root, budget, g)?;
    for (idx, value) in tree {
        let Some(Obj::Dict(pairs)) = (match value {
            Obj::Ref(r) => resolver.resolve(r, g).ok(),
            other => Some(other),
        }) else {
            continue;
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
        let get_int = |key: &[u8]| -> Option<i64> {
            pairs
                .iter()
                .find(|(k, _)| k.as_slice() == key)
                .and_then(|(_, v)| match v {
                    Obj::Int(i) => Some(*i),
                    _ => None,
                })
        };
        out.push(PageLabel {
            page_index: idx,
            style: get(b"S"),
            prefix: get(b"P"),
            start: get_int(b"St"),
        });
    }
    Ok(out)
}

/// A bead in an article thread.
#[derive(Debug, Clone, PartialEq)]
pub struct Bead {
    /// The bead's page reference.
    pub page: Option<selis_pdf_cos::Ref>,
    /// The next bead reference.
    pub next: Option<selis_pdf_cos::Ref>,
    /// The previous bead reference.
    pub prev: Option<selis_pdf_cos::Ref>,
}

/// Parse the `/Threads` array into bead lists (one per thread).
///
/// # Budget
///
/// Bounded by the thread count and budget.
///
/// # Malformed Input
///
/// Non-dict threads are skipped.
pub fn article_threads(
    resolver: &mut Resolver<'_>,
    catalog: &Obj,
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
) -> Result<Vec<Vec<Bead>>> {
    let mut out = Vec::new();
    let Some(Obj::Array(threads)) = catalog_dict(catalog, b"Threads") else {
        return Ok(out);
    };
    for thread in threads {
        let thread_dict = match thread {
            Obj::Ref(r) => resolver.resolve(*r, g).ok(),
            other => Some(other.clone()),
        };
        let Some(Obj::Dict(pairs)) = thread_dict else {
            continue;
        };
        let first = pairs
            .iter()
            .find(|(k, _)| k.as_slice() == b"F")
            .and_then(|(_, v)| match v {
                Obj::Ref(r) => Some(*r),
                _ => None,
            });
        let Some(first) = first else {
            continue;
        };
        let beads = follow_beads(resolver, first, budget, g)?;
        out.push(beads);
    }
    Ok(out)
}

/// Follow a bead chain from its first bead, cycle-guarded.
fn follow_beads(
    resolver: &mut Resolver<'_>,
    first: selis_pdf_cos::Ref,
    _budget: &Budget,
    g: &mut BudgetGuard<'_>,
) -> Result<Vec<Bead>> {
    let mut out = Vec::new();
    let mut visited = std::collections::BTreeSet::new();
    let mut cur = Some(first);
    while let Some(r) = cur {
        if !visited.insert(r.num) {
            break; // cyclic chain: stop, keep what we have
        }
        let Some(Obj::Dict(pairs)) = resolver.resolve(r, g).ok() else {
            break;
        };
        let get_ref = |key: &[u8]| -> Option<selis_pdf_cos::Ref> {
            pairs
                .iter()
                .find(|(k, _)| k.as_slice() == key)
                .and_then(|(_, v)| match v {
                    Obj::Ref(r) => Some(*r),
                    _ => None,
                })
        };
        let bead = Bead {
            page: get_ref(b"P"),
            next: get_ref(b"N"),
            prev: get_ref(b"V"),
        };
        let next = bead.next;
        out.push(bead);
        cur = next;
    }
    Ok(out)
}

/// Viewer preferences.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ViewerPreferences {
    /// `/HideToolbar`.
    pub hide_toolbar: bool,
    /// `/HideMenubar`.
    pub hide_menubar: bool,
    /// `/HideWindowUI`.
    pub hide_window_ui: bool,
    /// `/FitWindow`.
    pub fit_window: bool,
    /// `/CenterWindow`.
    pub center_window: bool,
    /// `/PageMode` (e.g. `/UseNone`, `/UseOutlines`).
    pub page_mode: Option<selis_bytes::Bytes>,
}

impl ViewerPreferences {
    /// Parse from the catalog.
    ///
    /// # Malformed Input
    ///
    /// A missing or non-dict `/ViewerPreferences` yields defaults.
    pub fn from_catalog(catalog: &Obj) -> Self {
        let mut out = Self::default();
        let Some(Obj::Dict(pairs)) = catalog_dict(catalog, b"ViewerPreferences") else {
            return out;
        };
        let flag = |key: &[u8]| -> bool {
            pairs
                .iter()
                .find(|(k, _)| k.as_slice() == key)
                .is_some_and(|(_, v)| matches!(v, Obj::Bool(true)))
        };
        out.hide_toolbar = flag(b"HideToolbar");
        out.hide_menubar = flag(b"HideMenubar");
        out.hide_window_ui = flag(b"HideWindowUI");
        out.fit_window = flag(b"FitWindow");
        out.center_window = flag(b"CenterWindow");
        out.page_mode = pairs
            .iter()
            .find(|(k, _)| k.as_slice() == b"PageMode")
            .and_then(|(_, v)| match v {
                Obj::Name(n) => Some(n.clone()),
                _ => None,
            });
        out
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
    fn viewer_preferences_parse() {
        let catalog = dict(vec![(
            b"ViewerPreferences",
            dict(vec![
                (b"HideToolbar", Obj::Bool(true)),
                (b"FitWindow", Obj::Bool(true)),
                (
                    b"PageMode",
                    Obj::Name(selis_bytes::Bytes::copy_from_slice(b"UseOutlines")),
                ),
            ]),
        )]);
        let prefs = ViewerPreferences::from_catalog(&catalog);
        assert!(prefs.hide_toolbar);
        assert!(prefs.fit_window);
        assert_eq!(
            prefs.page_mode.as_ref().map(|n| n.as_slice()),
            Some(&b"UseOutlines"[..])
        );
    }

    #[test]
    fn viewer_preferences_default_when_absent() {
        let catalog = dict(vec![(
            b"Type",
            Obj::Name(selis_bytes::Bytes::copy_from_slice(b"Catalog")),
        )]);
        let prefs = ViewerPreferences::from_catalog(&catalog);
        assert!(!prefs.hide_toolbar);
        assert!(!prefs.fit_window);
        assert_eq!(prefs.page_mode, None);
    }
}
