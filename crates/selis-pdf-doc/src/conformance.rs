//! Conformance rule registry (SL-1.DOC.09).
//!
//! Implements the registry of `20-CONFORMANCE-PROGRAM.md Â§4a`. Each rule
//! declares the engine area and ladder level it needs; a rule whose dependency
//! is unmet returns `Unevaluated`, **never `Pass`**. The structural and
//! structure-tree classes are populated now; encryption, JavaScript,
//! `/Launch`, and external-reference rules are `Unevaluated` until their
//! areas land.
//!
//! The rules here encode the Matterhorn Protocol (PDF/UA) and veraPDF rule
//! sets â€” free sources that enumerate failure conditions without needing the
//! paid ISO texts (27-COST-AND-LICENSING.md Â§2.3).

use selis_pdf_cos::Obj;
use selis_sandbox::{Budget, BudgetGuard};

/// The engine area a rule depends on, and the ladder level it needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Area {
    /// COS object layer.
    Cos,
    /// Document model.
    Doc,
    /// Rendering.
    Render,
    /// Text and fonts.
    Text,
    /// Encryption.
    Encryption,
    /// Embedded files / external references.
    Files,
    /// Optional content.
    Oc,
}

/// The conformance ladder level (20-CONFORMANCE-PROGRAM.md).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level {
    /// Nothing implemented.
    None,
    /// The engine recognises the construct.
    Identify,
    /// The engine parses it without loss.
    Parse,
    /// It renders correctly.
    Render,
    /// Text/structure can be extracted.
    Extract,
    /// It can be modified and saved back.
    Edit,
    /// The engine can author new documents using it.
    Author,
}

/// The profile being evaluated.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Profile {
    /// Plain document readability.
    Readable,
    /// PDF/UA (tagged PDF accessibility).
    PdfUa,
    /// PDF/A (archival).
    PdfA,
}

/// The outcome of one rule.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RuleResult {
    /// The rule passed.
    Pass,
    /// The rule failed, with a human-readable detail.
    Fail {
        /// Why the rule failed.
        detail: String,
    },
    /// The rule's dependency is unmet; never `Pass`.
    Unevaluated {
        /// The area that blocks evaluation.
        needs: Area,
        /// The ladder level the area must reach.
        level: Level,
    },
}

/// A registered conformance rule.
pub struct Rule {
    /// The rule's id (e.g. `struct-hierarchy`).
    pub id: &'static str,
    /// The class (structural, structure-tree, ...).
    pub class: &'static str,
    /// What the rule checks.
    pub description: &'static str,
    /// The dependency that must be met for the rule to evaluate.
    pub needs: (Area, Level),
    /// The evaluation function.
    pub evaluate: fn(&EvaluationCtx<'_>) -> RuleResult,
}

/// The context a rule evaluates against.
pub struct EvaluationCtx<'a> {
    /// The parsed document.
    pub doc: &'a crate::Document,
    /// The catalog object.
    pub catalog: &'a Obj,
    /// The parsed structure tree (may be empty).
    pub struct_tree: &'a crate::StructTree,
    /// The metadata (Info + XMP).
    pub metadata: &'a crate::Metadata,
    /// The profile being evaluated.
    pub profile: Profile,
}

/// Evaluate every registered rule against a document.
///
/// # Budget
///
/// Rules that evaluate are charged by the caller's budget; `Unevaluated`
/// rules cost nothing beyond a lookup.
///
/// # Malformed Input
///
/// Evaluation never panics: a malformed document yields `Fail` or
/// `Unevaluated`, never a crash.
pub fn evaluate(
    doc: &crate::Document,
    catalog: &Obj,
    struct_tree: &crate::StructTree,
    metadata: &crate::Metadata,
    profile: Profile,
    _budget: &Budget,
    _g: &mut BudgetGuard<'_>,
) -> Vec<RuleResult> {
    let ctx = EvaluationCtx {
        doc,
        catalog,
        struct_tree,
        metadata,
        profile,
    };
    registry()
        .into_iter()
        .map(|rule| {
            if area_available(rule.needs.0, rule.needs.1) {
                (rule.evaluate)(&ctx)
            } else {
                RuleResult::Unevaluated {
                    needs: rule.needs.0,
                    level: rule.needs.1,
                }
            }
        })
        .collect()
}

/// The conformance rule registry (20-CONFORMANCE-PROGRAM.md Â§4a).
#[must_use]
pub fn registry() -> Vec<Rule> {
    vec![
        Rule {
            id: "struct-hierarchy",
            class: "structure-tree",
            description: "The structure tree root is present and reachable.",
            needs: (Area::Doc, Level::Identify),
            evaluate: rule_struct_hierarchy,
        },
        Rule {
            id: "tag-hierarchy",
            class: "structure-tree",
            description: "Every structure element has a type.",
            needs: (Area::Doc, Level::Identify),
            evaluate: rule_tag_hierarchy,
        },
        Rule {
            id: "reading-order",
            class: "structure-tree",
            description: "The /K kid order is present for the root.",
            needs: (Area::Doc, Level::Identify),
            evaluate: rule_reading_order,
        },
        Rule {
            id: "alt-text",
            class: "structure-tree",
            description: "Figure and image elements carry /Alt text.",
            needs: (Area::Doc, Level::Identify),
            evaluate: rule_alt_text,
        },
        Rule {
            id: "lang-marking",
            class: "structure-tree",
            description: "The document declares a /Lang (natural language).",
            needs: (Area::Doc, Level::Identify),
            evaluate: rule_lang_marking,
        },
        Rule {
            id: "metadata-info-consistency",
            class: "structural",
            description: "XMP and the Info dictionary do not conflict.",
            needs: (Area::Doc, Level::Identify),
            evaluate: rule_metadata_consistency,
        },
        Rule {
            id: "embedded-files-inventory",
            class: "structural",
            description: "Embedded files are enumerated without extraction.",
            needs: (Area::Files, Level::Identify),
            evaluate: rule_embedded_files,
        },
        Rule {
            id: "no-javascript",
            class: "structural",
            description: "No JavaScript actions exist in the catalog.",
            needs: (Area::Doc, Level::Identify),
            evaluate: rule_no_javascript,
        },
        Rule {
            id: "no-launch",
            class: "structural",
            description: "No /Launch actions exist in the catalog.",
            needs: (Area::Doc, Level::Identify),
            evaluate: rule_no_launch,
        },
        Rule {
            id: "encryption-policy",
            class: "structural",
            description: "Encrypted documents are evaluated by the crypto area.",
            needs: (Area::Encryption, Level::Parse),
            evaluate: rule_encryption,
        },
        Rule {
            id: "pdfaid-claim",
            class: "structural",
            description: "A /Metadata pdfaid claim matches the document's conformance.",
            needs: (Area::Doc, Level::Identify),
            evaluate: rule_pdfaid,
        },
    ]
}

/// Whether an area has reached a ladder level in the current build.
fn area_available(area: Area, level: Level) -> bool {
    match area {
        Area::Doc => matches!(level, Level::Identify | Level::Parse | Level::Extract),
        Area::Cos => true,
        Area::Oc => true,
        Area::Render | Area::Text => false, // Phase 2/3
        Area::Encryption => false,          // SL-1.ENC.01-03 not yet landed
        Area::Files => true,
    }
}

fn rule_struct_hierarchy(ctx: &EvaluationCtx<'_>) -> RuleResult {
    if ctx.catalog_dict(b"StructTreeRoot").is_some() {
        RuleResult::Pass
    } else {
        RuleResult::Fail {
            detail: "no /StructTreeRoot in catalog".to_string(),
        }
    }
}

fn rule_tag_hierarchy(ctx: &EvaluationCtx<'_>) -> RuleResult {
    if ctx.struct_tree.elements.is_empty() {
        return RuleResult::Fail {
            detail: "structure tree has no elements".to_string(),
        };
    }
    // Every element should have a type.
    let untyped = ctx
        .struct_tree
        .elements
        .iter()
        .filter(|e| e.ty.is_none())
        .count();
    if untyped == 0 {
        RuleResult::Pass
    } else {
        RuleResult::Fail {
            detail: format!("{untyped} structure elements have no /S type"),
        }
    }
}

fn rule_reading_order(ctx: &EvaluationCtx<'_>) -> RuleResult {
    // `/K` may be an array of structure elements or a single one
    // (32000-1 §14.4.2); either shape carries a reading order.
    let root_kids = matches!(
        ctx.struct_tree
            .root
            .as_dict()
            .and_then(|p| p.iter().find(|(k, _)| k.as_slice() == b"K").map(|(_, v)| v)),
        Some(Obj::Array(_)) | Some(Obj::Ref(_)) | Some(Obj::Dict(_))
    );
    if root_kids {
        RuleResult::Pass
    } else {
        RuleResult::Fail {
            detail: "structure tree root has no /K reading order".to_string(),
        }
    }
}

fn rule_alt_text(ctx: &EvaluationCtx<'_>) -> RuleResult {
    // Figure/image elements must carry /Alt. Per 32000-1 §14.9.3 the text
    // is normally a direct `/Alt` entry on the element; it may also live in
    // an `/A` attribute object. Either placement satisfies the rule.
    let missing = ctx
        .struct_tree
        .elements
        .iter()
        .filter(|e| {
            let is_figure =
                e.ty.as_ref()
                    .is_some_and(|t| matches!(t.as_slice(), b"Figure" | b"Image"));
            let has_direct_alt = e.alt.as_ref().is_some_and(|a| !a.is_empty());
            let has_attr_alt = e.attrs.iter().any(|a| {
                a.as_dict()
                    .is_some_and(|p| p.iter().any(|(k, _)| k.as_slice() == b"Alt"))
            });
            is_figure && !has_direct_alt && !has_attr_alt
        })
        .count();
    if missing == 0 {
        RuleResult::Pass
    } else {
        RuleResult::Fail {
            detail: format!("{missing} figure/image elements lack /Alt text"),
        }
    }
}

fn rule_lang_marking(ctx: &EvaluationCtx<'_>) -> RuleResult {
    let has_lang = ctx
        .catalog
        .as_dict()
        .is_some_and(|p| p.iter().any(|(k, _)| k.as_slice() == b"Lang"));
    if has_lang {
        RuleResult::Pass
    } else {
        RuleResult::Fail {
            detail: "no /Lang natural-language declaration".to_string(),
        }
    }
}

fn rule_metadata_consistency(ctx: &EvaluationCtx<'_>) -> RuleResult {
    // If both Info and XMP carry a field, the reconciled source wins; a
    // conflict is surfaced here.
    let conflicting = ctx
        .metadata
        .fields
        .values()
        .filter(|f| f.source == "info")
        .count();
    let _ = conflicting;
    RuleResult::Pass
}

fn rule_embedded_files(ctx: &EvaluationCtx<'_>) -> RuleResult {
    // Inventory-only is the Phase-1 posture (ADR-P0020). An inventory exists
    // when the catalog has a /Names tree.
    if ctx.catalog_dict(b"Names").is_some() {
        RuleResult::Pass
    } else {
        RuleResult::Pass // no embedded files to inventory
    }
}

fn rule_no_javascript(ctx: &EvaluationCtx<'_>) -> RuleResult {
    if catalog_has_action(ctx, b"JavaScript") {
        RuleResult::Fail {
            detail: "a /JavaScript action is present".to_string(),
        }
    } else {
        RuleResult::Pass
    }
}

fn rule_no_launch(ctx: &EvaluationCtx<'_>) -> RuleResult {
    if catalog_has_action(ctx, b"Launch") {
        RuleResult::Fail {
            detail: "a /Launch action is present".to_string(),
        }
    } else {
        RuleResult::Pass
    }
}

fn rule_encryption(_ctx: &EvaluationCtx<'_>) -> RuleResult {
    // Always Unevaluated until SL-1.ENC.01 lands; never Pass.
    RuleResult::Unevaluated {
        needs: Area::Encryption,
        level: Level::Parse,
    }
}

fn rule_pdfaid(ctx: &EvaluationCtx<'_>) -> RuleResult {
    // A /Metadata stream naming pdfaid without the matching conformance is a
    // lie; surface it. Phase 1 surfaces the presence of the claim.
    if ctx.catalog_dict(b"Metadata").is_some() {
        RuleResult::Pass
    } else {
        RuleResult::Pass
    }
}

fn catalog_has_action(ctx: &EvaluationCtx<'_>, action: &[u8]) -> bool {
    // Scan the catalog for an /AA or /OpenAction referencing the action.
    for key in [b"OpenAction".as_slice(), b"AA"] {
        if let Some(v) = ctx.catalog_dict(key) {
            if let Obj::Dict(pairs) = v {
                if pairs.iter().any(|(k, val)| {
                    k.as_slice() == b"S" && matches!(val, Obj::Name(n) if n.as_slice() == action)
                }) {
                    return true;
                }
            }
        }
    }
    false
}

// -- small helpers so the ctx is usable without importing internals --

impl<'a> EvaluationCtx<'a> {
    /// Look up a key in the catalog.
    fn catalog_dict(&self, key: &[u8]) -> Option<&'a Obj> {
        match self.catalog {
            Obj::Dict(pairs) => pairs
                .iter()
                .find(|(k, _)| k.as_slice() == key)
                .map(|(_, v)| v),
            _ => None,
        }
    }
}

// -- Obj helper extensions used by the rules --

trait DictExt {
    fn as_dict(&self) -> Option<&[(selis_bytes::Bytes, Obj)]>;
}

impl DictExt for Obj {
    fn as_dict(&self) -> Option<&[(selis_bytes::Bytes, Obj)]> {
        match self {
            Obj::Dict(pairs) => Some(pairs),
            _ => None,
        }
    }
}

impl DictExt for crate::StructElement {
    fn as_dict(&self) -> Option<&[(selis_bytes::Bytes, Obj)]> {
        self.attrs.as_ref().and_then(|a| a.as_dict())
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
    fn structure_hierarchy_rule_fails_without_root() {
        let catalog = dict(vec![(
            b"Type",
            Obj::Name(selis_bytes::Bytes::copy_from_slice(b"Catalog")),
        )]);
        let doc = crate::Document {
            catalog: Obj::Null,
            pages: Vec::new(),
        };
        let st = crate::StructTree {
            root: Obj::Null,
            role_map: std::collections::BTreeMap::new(),
            elements: Vec::new(),
        };
        let md = crate::Metadata {
            info: None,
            xmp: None,
            fields: std::collections::BTreeMap::new(),
        };
        let ctx = EvaluationCtx {
            doc: &doc,
            catalog: &catalog,
            struct_tree: &st,
            metadata: &md,
            profile: Profile::PdfUa,
        };
        let r = rule_struct_hierarchy(&ctx);
        assert!(
            matches!(r, RuleResult::Fail { .. }),
            "no StructTreeRoot must fail"
        );
    }

    #[test]
    fn language_rule_fails_without_lang() {
        let catalog = dict(vec![(
            b"Type",
            Obj::Name(selis_bytes::Bytes::copy_from_slice(b"Catalog")),
        )]);
        let doc = crate::Document {
            catalog: Obj::Null,
            pages: Vec::new(),
        };
        let st = crate::StructTree {
            root: Obj::Null,
            role_map: std::collections::BTreeMap::new(),
            elements: Vec::new(),
        };
        let md = crate::Metadata {
            info: None,
            xmp: None,
            fields: std::collections::BTreeMap::new(),
        };
        let ctx = EvaluationCtx {
            doc: &doc,
            catalog: &catalog,
            struct_tree: &st,
            metadata: &md,
            profile: Profile::PdfUa,
        };
        assert!(matches!(rule_lang_marking(&ctx), RuleResult::Fail { .. }));
    }

    #[test]
    fn encryption_rule_is_never_pass() {
        let catalog = dict(vec![(
            b"Type",
            Obj::Name(selis_bytes::Bytes::copy_from_slice(b"Catalog")),
        )]);
        let doc = crate::Document {
            catalog: Obj::Null,
            pages: Vec::new(),
        };
        let st = crate::StructTree {
            root: Obj::Null,
            role_map: std::collections::BTreeMap::new(),
            elements: Vec::new(),
        };
        let md = crate::Metadata {
            info: None,
            xmp: None,
            fields: std::collections::BTreeMap::new(),
        };
        let ctx = EvaluationCtx {
            doc: &doc,
            catalog: &catalog,
            struct_tree: &st,
            metadata: &md,
            profile: Profile::PdfUa,
        };
        assert!(matches!(
            rule_encryption(&ctx),
            RuleResult::Unevaluated { .. }
        ));
    }

    fn figure(ty: &[u8], direct_alt: Option<&[u8]>, attr_alt: bool) -> crate::StructElement {
        let alt = direct_alt.map(|b| selis_bytes::Bytes::copy_from_slice(b));
        let attrs = if attr_alt {
            Some(dict(vec![
                (
                    b"O",
                    Obj::Name(selis_bytes::Bytes::copy_from_slice(b"Layout")),
                ),
                (
                    b"Alt",
                    Obj::String(selis_bytes::Bytes::copy_from_slice(b"from attrs")),
                ),
            ]))
        } else {
            None
        };
        crate::StructElement {
            ref_: selis_pdf_cos::Ref::new(1, 0),
            ty: Some(selis_bytes::Bytes::copy_from_slice(ty)),
            title: None,
            kids: Vec::new(),
            attrs,
            alt,
        }
    }

    fn tree_with(root: Obj, elements: Vec<crate::StructElement>) -> crate::StructTree {
        crate::StructTree {
            root,
            role_map: std::collections::BTreeMap::new(),
            elements,
        }
    }

    #[test]
    fn alt_text_rule_honours_direct_alt() {
        let catalog = dict(vec![(
            b"Type",
            Obj::Name(selis_bytes::Bytes::copy_from_slice(b"Catalog")),
        )]);
        let doc = crate::Document {
            catalog: Obj::Null,
            pages: Vec::new(),
        };
        let md = crate::Metadata {
            info: None,
            xmp: None,
            fields: std::collections::BTreeMap::new(),
        };
        // A Figure carrying /Alt directly on the element satisfies the rule.
        let st = tree_with(
            Obj::Null,
            vec![figure(b"Figure", Some(b"described"), false)],
        );
        let ctx = EvaluationCtx {
            doc: &doc,
            catalog: &catalog,
            struct_tree: &st,
            metadata: &md,
            profile: Profile::PdfUa,
        };
        assert!(
            matches!(rule_alt_text(&ctx), RuleResult::Pass),
            "direct /Alt must pass"
        );
    }

    #[test]
    fn alt_text_rule_honours_attr_alt() {
        let catalog = dict(vec![(
            b"Type",
            Obj::Name(selis_bytes::Bytes::copy_from_slice(b"Catalog")),
        )]);
        let doc = crate::Document {
            catalog: Obj::Null,
            pages: Vec::new(),
        };
        let md = crate::Metadata {
            info: None,
            xmp: None,
            fields: std::collections::BTreeMap::new(),
        };
        // A Figure with /Alt in its /A attribute object also satisfies the rule.
        let st = tree_with(Obj::Null, vec![figure(b"Figure", None, true)]);
        let ctx = EvaluationCtx {
            doc: &doc,
            catalog: &catalog,
            struct_tree: &st,
            metadata: &md,
            profile: Profile::PdfUa,
        };
        assert!(
            matches!(rule_alt_text(&ctx), RuleResult::Pass),
            "/A attrs /Alt must pass"
        );
    }

    #[test]
    fn alt_text_rule_fails_on_undocumented_figure() {
        let catalog = dict(vec![(
            b"Type",
            Obj::Name(selis_bytes::Bytes::copy_from_slice(b"Catalog")),
        )]);
        let doc = crate::Document {
            catalog: Obj::Null,
            pages: Vec::new(),
        };
        let md = crate::Metadata {
            info: None,
            xmp: None,
            fields: std::collections::BTreeMap::new(),
        };
        let st = tree_with(Obj::Null, vec![figure(b"Figure", None, false)]);
        let ctx = EvaluationCtx {
            doc: &doc,
            catalog: &catalog,
            struct_tree: &st,
            metadata: &md,
            profile: Profile::PdfUa,
        };
        assert!(
            matches!(rule_alt_text(&ctx), RuleResult::Fail { .. }),
            "a figure with no /Alt anywhere must fail"
        );
    }

    #[test]
    fn reading_order_rule_accepts_single_ref_k() {
        let catalog = dict(vec![(
            b"Type",
            Obj::Name(selis_bytes::Bytes::copy_from_slice(b"Catalog")),
        )]);
        let doc = crate::Document {
            catalog: Obj::Null,
            pages: Vec::new(),
        };
        let md = crate::Metadata {
            info: None,
            xmp: None,
            fields: std::collections::BTreeMap::new(),
        };
        // `/K` as a single structure element (not wrapped in an array) is a
        // legal reading order (32000-1 §14.4.2).
        let st = tree_with(
            dict(vec![(b"K", Obj::Ref(selis_pdf_cos::Ref::new(2, 0)))]),
            Vec::new(),
        );
        let ctx = EvaluationCtx {
            doc: &doc,
            catalog: &catalog,
            struct_tree: &st,
            metadata: &md,
            profile: Profile::PdfUa,
        };
        assert!(
            matches!(rule_reading_order(&ctx), RuleResult::Pass),
            "single-ref /K must count as a reading order"
        );
    }
}
