//! Embedded files + attachments (SL-1.DOC.07).
//!
//! **Inventory only**: enumerate attachments without extracting them.
//! Extraction is gated by policy (ADR-P0020). The inventory feeds the
//! privacy scanner (SL-1A.TOOL.09) — a document's embedded files are a
//! privacy/security surface worth surfacing even when we never open them.

use selis_error::Result;
use selis_pdf_cos::Obj;
use selis_sandbox::{Budget, BudgetGuard};

use crate::Resolver;

/// An embedded-file inventory entry.
#[derive(Debug, Clone, PartialEq)]
pub struct Attachment {
    /// The key under which it appears in `/EmbeddedFiles` (may be empty).
    pub key: String,
    /// The embedded file's `/Name` (the display name).
    pub name: Option<selis_bytes::Bytes>,
    /// The embedded file's `/Desc` (description), when present.
    pub desc: Option<selis_bytes::Bytes>,
    /// The declared size (`/EF /F /Length`), when present.
    pub size: Option<i64>,
    /// The `/Type` of the embedded file stream.
    pub subtype: Option<selis_bytes::Bytes>,
}

/// Enumerate embedded files from the catalog's `/Names /EmbeddedFiles` tree.
///
/// # Budget
///
/// Bounded by the name-tree walk and the budget.
///
/// # Malformed Input
///
/// A missing tree yields an empty list; malformed entries are skipped.
pub fn embedded_files(
    resolver: &mut Resolver<'_>,
    catalog: &Obj,
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
) -> Result<Vec<Attachment>> {
    let mut out = Vec::new();
    // `/Names` is usually a dict whose `/EmbeddedFiles` is a tree root ref,
    // but may itself be a ref to the tree.
    let names = match catalog_dict(catalog, b"Names") {
        Some(Obj::Ref(r)) => resolver.resolve(*r, g)?,
        Some(o) => o.clone(),
        None => return Ok(out),
    };
    let Some(Obj::Ref(embedded_root)) = dict_get(&names, b"EmbeddedFiles") else {
        return Ok(out);
    };
    let tree = crate::walk_name_tree(resolver, *embedded_root, budget, g)?;
    for (key, value) in tree {
        let Some(dict) = as_dict(resolver, &value, g) else {
            continue;
        };
        // /EF /F points at the embedded-file stream.
        let stream_ref = dict_get(&dict, b"EF").and_then(|ef| match ef {
            Obj::Dict(pairs) => {
                pairs
                    .iter()
                    .find(|(k, _)| k.as_slice() == b"F")
                    .and_then(|(_, v)| match v {
                        Obj::Ref(r) => Some(*r),
                        _ => None,
                    })
            }
            _ => None,
        });
        // The display name and description live in the Filespec dict; the
        // declared size lives in the embedded-file stream's `/Length`.
        let name = dict_value(&dict, b"Name").or_else(|| dict_value(&dict, b"F"));
        let desc = dict_value(&dict, b"Desc");
        let size = match stream_ref {
            Some(sr) => resolver
                .resolve(sr, g)
                .ok()
                .and_then(|o| stream_length(&o)),
            None => None,
        };
        let subtype = match stream_ref {
            Some(sr) => resolver.resolve(sr, g).ok().and_then(|o| match o {
                Obj::Stream { dict, .. } | Obj::Dict(dict) => {
                    dict_value(&Obj::Dict(dict), b"Subtype")
                }
                _ => None,
            }),
            None => None,
        };
        out.push(Attachment {
            key,
            name,
            desc,
            size,
            subtype,
        });
    }
    Ok(out)
}

/// The `/Length` of a stream object (or `None`).
fn stream_length(obj: &Obj) -> Option<i64> {
    let pairs = match obj {
        Obj::Stream { dict, .. } => dict,
        Obj::Dict(pairs) => pairs,
        _ => return None,
    };
    dict_value_int(&Obj::Dict(pairs.clone()), b"Length")
}

fn as_dict<'a>(
    resolver: &mut Resolver<'_>,
    value: &'a Obj,
    g: &mut BudgetGuard<'_>,
) -> Option<Obj> {
    match value {
        Obj::Dict(_) => Some(value.clone()),
        Obj::Ref(r) => resolver.resolve(*r, g).ok(),
        _ => None,
    }
}

fn dict_get<'a>(dict: &'a Obj, key: &[u8]) -> Option<&'a Obj> {
    match dict {
        Obj::Dict(pairs) => pairs
            .iter()
            .find(|(k, _)| k.as_slice() == key)
            .map(|(_, v)| v),
        _ => None,
    }
}

fn dict_value(dict: &Obj, key: &[u8]) -> Option<selis_bytes::Bytes> {
    dict_get(dict, key).and_then(|v| match v {
        Obj::Name(n) | Obj::String(n) => Some(n.clone()),
        _ => None,
    })
}

fn dict_value_int(dict: &Obj, key: &[u8]) -> Option<i64> {
    dict_get(dict, key).and_then(|v| match v {
        Obj::Int(i) => Some(*i),
        _ => None,
    })
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
    use selis_pdf_cos::{Ref, XrefEntry};
    use selis_sandbox::{Budget, CancelToken, FixedClock};

    fn guard() -> BudgetGuard<'static> {
        Budget::unlimited().guard_with(&FixedClock(0), CancelToken::new())
    }

    /// A catalog with /Names → /EmbeddedFiles → a name tree of one file.
    #[test]
    fn inventories_embedded_files() {
        let mut src = Vec::new();
        let mut offs = Vec::new();
        let mut push_obj = |bytes: &[u8], num: u32| {
            let off = src.len() as u64;
            src.extend_from_slice(bytes);
            offs.push((num, off));
        };
        push_obj(b"1 0 obj\n<< /Names 2 0 R >>\nendobj\n", 1);
        push_obj(b"2 0 obj\n<< /EmbeddedFiles 3 0 R >>\nendobj\n", 2);
        push_obj(b"3 0 obj\n<< /Names [(report.pdf) 4 0 R] >>\nendobj\n", 3);
        push_obj(
            b"4 0 obj\n<< /Type /Filespec /F (report.pdf) /EF << /F 5 0 R >> >>\nendobj\n",
            4,
        );
        push_obj(
            b"5 0 obj\n<< /Type /EmbeddedFile /Subtype /application#2Fpdf /Length 42 >>\nendobj\n",
            5,
        );

        let mut xref = std::collections::BTreeMap::new();
        for (num, off) in &offs {
            xref.insert(
                *num,
                XrefEntry::InUse {
                    offset: *off,
                    gen: 0,
                },
            );
        }
        let trailer = vec![(
            selis_bytes::Bytes::copy_from_slice(b"Root"),
            Obj::Ref(Ref::new(1, 0)),
        )];
        let doc = selis_pdf_cos::Doc::from_single_revision(xref, trailer);
        let budget = Budget::unlimited();
        let mut resolver = crate::Resolver::new(&doc, &src, &budget);
        let mut g = guard();

        // Resolve the catalog (object 1 is /Names → ...; we treat object 1 as
        // the catalog-shaped object here).
        let catalog_obj = resolver.resolve(Ref::new(1, 0), &mut g).expect("resolve");
        let files =
            embedded_files(&mut resolver, &catalog_obj, &budget, &mut g).expect("inventory");
        assert_eq!(files.len(), 1, "one embedded file");
        assert_eq!(files[0].key, "report.pdf");
        assert_eq!(files[0].size, Some(42));
        assert_eq!(
            files[0].subtype.as_ref().map(|s| s.as_slice()),
            Some(&b"application/pdf"[..])
        );
    }
}
