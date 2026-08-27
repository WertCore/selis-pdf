//! `selis inspect` (SL-1.COS.10): the structural dump the qpdf oracle
//! compares against. Revisions, xref entries, and deviations, as JSON.

use std::collections::BTreeMap;

use super::{CliError, CliResult, InspectDoc, InspectRevision};
use selis_pdf_cos::{self, Deviation};
/// Run `inspect` on a file.
///
/// # Budget
///
/// Uses the Viewer profile.
///
/// # Malformed Input
///
/// Whatever the file contains, inspect reports it — the only errors are
/// unreadable files and budget exhaustion.
pub(super) fn run(path: &str, json: bool) -> CliResult<()> {
    let data = super::read_file(path)?;
    let budget = super::parse_budget();
    let mut g = selis_sandbox::BudgetGuard::new(budget, &NO_CLOCK, Default::default());

    // 1. Try the classic xref + revisions path.
    let doc = match selis_pdf_cos::parse_revisions(
        &data,
        selis_pdf_cos::xref::find_startxref(&data, 4096).unwrap_or(0),
        &budget,
        &mut g,
    ) {
        Ok(doc) => doc,
        Err(_) => {
            // 2. Fall back to damaged-file reconstruction.
            let (doc, _dev) = match selis_pdf_cos::reconstruct(&data, &budget, &mut g) {
                Ok(pair) => pair,
                Err(e) => return Err(CliError(e.to_string())),
            };
            doc
        }
    };

    let mut out = InspectDoc {
        version: header_version(&data),
        revisions: Vec::new(),
        deviations: Vec::new(),
        root: doc
            .revisions()
            .last()
            .and_then(|r| r.root)
            .map(|r| super::ref_str(&r)),
        size: None,
        reconstructed: false,
        attachments: Vec::new(),
        structure: None,
        metadata: None,
    };

    for rev in doc.revisions() {
        out.revisions.push(InspectRevision {
            byte_range: [rev.byte_range.start, rev.byte_range.end],
            entries: rev.entries.len(),
            objects: rev.entries.keys().copied().collect(),
        });
    }

    // Attachments + tagged structure tree from the resolved catalog.
    if let Some(root_ref) = doc.revisions().last().and_then(|r| r.root) {
        let mut resolver = selis_pdf_doc::Resolver::new(&doc, &data, &budget);
        if let Ok(catalog) = resolver.resolve(root_ref, &mut g) {
            if let Ok(attachments) =
                selis_pdf_doc::embedded_files(&mut resolver, &catalog, &budget, &mut g)
            {
                for a in &attachments {
                    out.attachments.push(super::InspectAttachment {
                        name: a
                            .name
                            .as_ref()
                            .map(|b| String::from_utf8_lossy(b.as_slice()).to_string())
                            .unwrap_or_default(),
                        size: a.size.unwrap_or(-1),
                        key: a.key.clone(),
                    });
                }
            }
            if let Ok(tree) =
                selis_pdf_doc::StructTree::resolve(&mut resolver, &catalog, &budget, &mut g)
            {
                if !tree.elements.is_empty() {
                    let types: Vec<String> = tree
                        .elements
                        .iter()
                        .map(|e| {
                            e.ty.as_ref()
                                .map(|b| String::from_utf8_lossy(b.as_slice()).to_string())
                                .unwrap_or_default()
                        })
                        .collect();
                    out.structure = Some(super::InspectStructure {
                        elements: tree.elements.len(),
                        types,
                        mcid_order: tree.mcid_order(),
                    });
                }
            }
            if let Ok(meta) =
                selis_pdf_doc::Metadata::resolve(&mut resolver, &catalog, &budget, &mut g)
            {
                let mut fields = BTreeMap::new();
                for (k, fv) in &meta.fields {
                    fields.insert(k.clone(), fv.value.clone());
                }
                out.metadata = Some(fields);
            }
        }
    }

    // Deviations from a fresh lex (the revision walk is byte-level).
    let mut lex = selis_pdf_cos::Lexer::new(&data);
    let mut all: Vec<Deviation> = Vec::new();
    while let Some(tok) = lex.next_token(&mut g).unwrap_or(None) {
        let _ = tok;
    }
    all.extend(lex.deviations().iter().copied());
    // Deduplicate by (name, offset).
    let mut seen = std::collections::BTreeSet::new();
    for d in all {
        if seen.insert((d.name(), d.offset())) {
            out.deviations.push(super::deviation_json(&d));
        }
    }

    if json {
        let text =
            serde_json::to_string_pretty(&out).map_err(|e| CliError(format!("json: {e}")))?;
        println!("{text}");
    } else {
        println!("version: {}", out.version.as_deref().unwrap_or("unknown"));
        println!("revisions: {}", out.revisions.len());
        for (i, rev) in out.revisions.iter().enumerate() {
            println!(
                "  revision {i}: bytes {}..{}  {} entries",
                rev.byte_range[0], rev.byte_range[1], rev.entries
            );
        }
        println!("root: {}", out.root.as_deref().unwrap_or("none"));
        println!("deviations: {}", out.deviations.len());
        for d in &out.deviations {
            println!("  {} @ {}", d.name, d.offset);
        }
    }
    Ok(())
}

static NO_CLOCK: selis_sandbox::FixedClock = selis_sandbox::FixedClock(0);

/// Extract the `%PDF-x.y` header, if any.
fn header_version(data: &[u8]) -> Option<String> {
    let head = data.get(..data.len().min(16))?;
    let text = String::from_utf8_lossy(head);
    let text = text.trim_start_matches('\u{feff}');
    let version = text.strip_prefix("%PDF-")?;
    let version = version.split(|c: char| c == '\n' || c == '\r').next()?;
    Some(version.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use selis_pdf_cos::Ref;

    #[test]
    fn header_version_parses() {
        // Test plain byte slices.
        let pdf17: &[u8] = b"%PDF-1.7\n...";
        let pdf20: &[u8] = b"%PDF-2.0\n...";
        assert_eq!(header_version(pdf17), Some("1.7".to_string()));
        assert_eq!(header_version(pdf20), Some("2.0".to_string()));
        assert_eq!(header_version(b"not a pdf"), None);
    }

    #[test]
    fn ref_str_formats() {
        assert_eq!(super::super::ref_str(&Ref::new(5, 0)), "5 0");
        assert_eq!(super::super::ref_str(&Ref::new(12, 2)), "12 2");
    }
}
