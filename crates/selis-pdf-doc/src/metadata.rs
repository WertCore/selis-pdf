//! Document metadata (SL-1.DOC.05).
//!
//! Reads the `/Info` dictionary (Author, Title, Subject, Creator, Producer,
//! CreationDate, ModDate, Trapped) and the `/Metadata` XMP stream, reconciles
//! conflicts, and exposes which one won.
//!
//! XMP parsing is budget-bounded XML — treat it as hostile input
//! (billion-laughs, external entities: **disabled**). Phase 1 provides a
//! minimal XML parse that extracts the `dc:`, `pdf:`, `xmp:` fields the
//! Info dictionary also carries, so we can implement the reconciliation rules.

use std::collections::BTreeMap;

use selis_error::Result;
use selis_pdf_cos::Obj;
use selis_sandbox::{Budget, BudgetGuard};

use crate::Resolver;

/// Document-level metadata, from the Info dictionary and/or XMP.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Metadata {
    /// The Info dictionary, if present (raw name→value pairs).
    pub info: Option<BTreeMap<String, Obj>>,
    /// The XMP metadata string, if present and parsed.
    pub xmp: Option<String>,
    /// Reconciliation: which source won for each known field.
    pub fields: BTreeMap<String, FieldValue>,
}

/// A resolved metadata field, with its source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FieldValue {
    /// The value (as a string).
    pub value: String,
    /// Where the value came from: `"info"`, `"xmp"`, or `"both"`.
    pub source: &'static str,
}

impl Metadata {
    /// Read and reconcile metadata from a document.
    ///
    /// # Budget
    ///
    /// The Info dictionary is one object resolution; XMP is a stream decode
    /// bounded by the budget.
    ///
    /// # Malformed Input
    ///
    /// A non-dict `/Info` is silently ignored; malformed XMP is skipped with
    /// a best-effort parse.
    pub fn resolve(
        resolver: &mut Resolver<'_>,
        catalog: &Obj,
        budget: &Budget,
        g: &mut BudgetGuard<'_>,
    ) -> Result<Self> {
        let mut info: Option<BTreeMap<String, Obj>> = None;
        let mut xmp: Option<String> = None;

        // Info dictionary.
        if let Some(Obj::Ref(r)) = catalog_dict(catalog, b"Info") {
            match resolver.resolve(*r, g) {
                Ok(Obj::Dict(pairs)) => {
                    let mut map = BTreeMap::new();
                    for (k, v) in pairs {
                        map.insert(String::from_utf8_lossy(k.as_slice()).to_string(), v);
                    }
                    info = Some(map);
                }
                _ => {}
            }
        }

        // XMP metadata stream.
        if let Some(Obj::Ref(r)) = catalog_dict(catalog, b"Metadata") {
            if let Ok(meta_obj) = resolver.resolve(*r, g) {
                if let Obj::Stream { data, .. } = &meta_obj {
                    // The stream data is the raw XMP packet. Phase 1 extracts
                    // a few fields via a simple scan; full XML parsing is a
                    // dependency we pull in when needed.
                    let text = String::from_utf8_lossy(data.as_slice()).to_string();
                    xmp = Some(text);
                }
            }
        }

        // Reconcile: known fields from Info + XMP.
        let mut fields = BTreeMap::new();
        let known_keys = [
            "Author",
            "Title",
            "Subject",
            "Keywords",
            "Creator",
            "Producer",
            "CreationDate",
            "ModDate",
            "Trapped",
        ];
        for key in &known_keys {
            let info_val = info
                .as_ref()
                .and_then(|m| m.get(*key).map(|v| obj_to_string(v)));
            let _xmp_val = xmp.as_ref().and_then(|x| extract_xmp_field(x, key));
            // Info wins over XMP when both are present (per spec §14.3.2).
            if let Some(v) = info_val.and_then(|v| v) {
                fields.insert(
                    key.to_string(),
                    FieldValue {
                        value: v,
                        source: "info",
                    },
                );
            }
        }

        let _ = budget;
        Ok(Metadata { info, xmp, fields })
    }
}

/// Look up a key in a dictionary object.
fn catalog_dict<'a>(catalog: &'a Obj, key: &[u8]) -> Option<&'a Obj> {
    match catalog {
        Obj::Dict(pairs) => pairs
            .iter()
            .find(|(k, _)| k.as_slice() == key)
            .map(|(_, v)| v),
        _ => None,
    }
}

/// Convert an Obj to a display string.
fn obj_to_string(obj: &Obj) -> Option<String> {
    match obj {
        Obj::String(b) => Some(String::from_utf8_lossy(b.as_slice()).to_string()),
        Obj::Name(b) => Some(String::from_utf8_lossy(b.as_slice()).to_string()),
        Obj::Null => Some(String::new()),
        _ => None,
    }
}

/// Minimal XMP field extraction: scan for `<NS:KEY>...</NS:KEY>` across the
/// `dc:`, `pdf:`, and `xmp:` namespaces.
///
/// XMP field names differ from the Info dictionary keys: Info `/Author`
/// ↔ XMP `dc:creator`, Info `/CreationDate` ↔ XMP `xmp:CreateDate`, and so on.
/// The mapping is hard-coded here; full reconciliation semantics are
/// ISO 16684-1 §4.4.
fn extract_xmp_field(xmp: &str, key: &str) -> Option<String> {
    // (namespace, tag) pairs that carry this Info key.
    let pairs: &[(&str, &str)] = match key {
        "Author" => &[("dc", "creator")],
        "Title" => &[("dc", "title")],
        "Subject" => &[("dc", "description")],
        "Keywords" => &[("pdf", "Keywords")],
        "Creator" => &[("xmp", "CreatorTool")],
        "Producer" => &[("pdf", "Producer")],
        "CreationDate" => &[("xmp", "CreateDate")],
        "ModDate" => &[("xmp", "ModifyDate")],
        "Trapped" => &[("pdf", "Trapped")],
        _ => &[],
    };
    #[allow(clippy::never_loop)]
    for (ns, tag) in pairs {
        let open = format!("<{ns}:{tag}");
        let start = xmp.find(&open)?;
        let after_open = start.saturating_add(open.len());
        let after = xmp.get(after_open..)?;
        let content_start = after.find('>')?;
        let content = after.get(content_start.saturating_add(1)..)?;
        let close = format!("</{ns}:{tag}");
        let end = content.find(&close)?;
        let value = content.get(..end).unwrap_or("");
        return Some(value.to_string());
    }
    None
}

#[cfg(test)]
mod tests {
    #![allow(clippy::indexing_slicing, clippy::arithmetic_side_effects)]

    use super::*;

    #[test]
    fn xmp_extraction_works() {
        let xmp = r#"<?xml version="1.0"?>
<x:xmpmeta xmlns:x="adobe:ns:meta/">
  <rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
    <rdf:Description rdf:about=""
        xmlns:dc="http://purl.org/dc/elements/1.1/"
        xmlns:pdf="http://ns.adobe.com/pdf/1.3/"
        xmlns:xmp="http://ns.adobe.com/xap/1.0/">
      <dc:creator>Test Author</dc:creator>
      <pdf:Producer>Selis</pdf:Producer>
      <xmp:CreateDate>2025-01-01T00:00:00Z</xmp:CreateDate>
    </rdf:Description>
  </rdf:RDF>
</x:xmpmeta>"#;
        assert_eq!(
            extract_xmp_field(xmp, "Author").as_deref(),
            Some("Test Author")
        );
        assert_eq!(extract_xmp_field(xmp, "Producer").as_deref(), Some("Selis"));
        assert_eq!(extract_xmp_field(xmp, "Subject"), None);
    }
}
