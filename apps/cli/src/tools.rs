//! The `selis` document tools: merge, split, set-metadata, redact.
//!
//! Built on the full-document writer + object-graph copy (WRITE.01/03/04).

#![allow(clippy::indexing_slicing, clippy::arithmetic_side_effects)]

use selis_pdf_cos::{self, Obj, Ref};
use selis_sandbox::{Budget, BudgetGuard};

use crate::{read_file, CliError, CliResult};

/// Merge several PDFs into one (SL-1A.TOOL.01).
pub(crate) fn merge(inputs: &[String], output: &str) -> CliResult<()> {
    if inputs.len() < 2 {
        return Err(CliError("merge needs at least two input files".to_string()));
    }
    let budget = Budget::unlimited();
    let mut g = budget.guard();
    let mut merged = selis_pdf_cos::doc_writer::DocumentBuilder::new();
    let mut next_num = 3u32;

    for path in inputs {
        let src = read_file(path)?;
        let page_refs = open_page_refs(&src, &budget, &mut g)
            .map_err(|e| CliError(format!("{path}: {e}")))?;
        for page_ref in page_refs {
            copy_page(&mut merged, &src, page_ref, &budget, &mut g, &mut next_num)
                .map_err(|e| CliError(format!("{path}: {e}")))?;
        }
    }

    let bytes = merged
        .write(&budget, &mut g)
        .map_err(|e| CliError(format!("write failed: {e}")))?;
    std::fs::write(output, &bytes).map_err(|e| CliError(format!("cannot write {output}: {e}")))?;
    eprintln!("merged {} file(s) into {output}", inputs.len());
    Ok(())
}

/// Split a PDF: extract the pages in `page_range` (inclusive, 0-based) into a
/// new file (SL-1A.TOOL.02).
pub(crate) fn split(path: &str, first: usize, last: usize, output: &str) -> CliResult<()> {
    let src = read_file(path)?;
    let budget = Budget::unlimited();
    let mut g = budget.guard();
    let page_refs = open_page_refs(&src, &budget, &mut g)
        .map_err(|e| CliError(format!("{path}: {e}")))?;
    if last >= page_refs.len() || first > last {
        return Err(CliError(format!(
            "page range {first}..{last} out of range (document has {} pages)",
            page_refs.len()
        )));
    }
    let mut out = selis_pdf_cos::doc_writer::DocumentBuilder::new();
    let mut next_num = 3u32;
    for page_ref in page_refs[first..=last].iter() {
        copy_page(&mut out, &src, *page_ref, &budget, &mut g, &mut next_num)
            .map_err(|e| CliError(format!("{path}: {e}")))?;
    }
    let bytes = out
        .write(&budget, &mut g)
        .map_err(|e| CliError(format!("write failed: {e}")))?;
    std::fs::write(output, &bytes).map_err(|e| CliError(format!("cannot write {output}: {e}")))?;
    eprintln!("split pages {first}..{last} into {output}");
    Ok(())
}

/// Set metadata fields (Info dictionary) and rewrite the document
/// (SL-1A.TOOL.09).
pub(crate) fn set_metadata(path: &str, fields: &[(&str, &str)], output: &str) -> CliResult<()> {
    let src = read_file(path)?;
    let budget = Budget::unlimited();
    let mut g = budget.guard();
    let page_refs = open_page_refs(&src, &budget, &mut g)
        .map_err(|e| CliError(format!("{path}: {e}")))?;
    let mut out = selis_pdf_cos::doc_writer::DocumentBuilder::new();
    let mut next_num = 3u32;
    for (k, v) in fields {
        out.set_info(k.as_bytes(), v);
    }
    for page_ref in &page_refs {
        copy_page(&mut out, &src, *page_ref, &budget, &mut g, &mut next_num)
            .map_err(|e| CliError(format!("{path}: {e}")))?;
    }
    let bytes = out
        .write(&budget, &mut g)
        .map_err(|e| CliError(format!("write failed: {e}")))?;
    std::fs::write(output, &bytes).map_err(|e| CliError(format!("cannot write {output}: {e}")))?;
    eprintln!("set {} metadata field(s) -> {output}", fields.len());
    Ok(())
}

/// Redact regions: cover them with black and strip text that falls within
/// them (SL-0.LEGAL.05).
pub(crate) fn redact(
    path: &str,
    rects: &[(f64, f64, f64, f64)],
    output: &str,
) -> CliResult<()> {
    let src = read_file(path)?;
    let budget = Budget::unlimited();
    let mut g = budget.guard();
    let page_refs = open_page_refs(&src, &budget, &mut g)
        .map_err(|e| CliError(format!("{path}: {e}")))?;
    let mut out = selis_pdf_cos::doc_writer::DocumentBuilder::new();
    let mut next_num = 3u32;
    for page_ref in &page_refs {
        copy_page_redacted(
            &mut out,
            &src,
            *page_ref,
            rects,
            &budget,
            &mut g,
            &mut next_num,
        )
        .map_err(|e| CliError(format!("{path}: {e}")))?;
    }
    let bytes = out
        .write(&budget, &mut g)
        .map_err(|e| CliError(format!("write failed: {e}")))?;
    std::fs::write(output, &bytes).map_err(|e| CliError(format!("cannot write {output}: {e}")))?;
    eprintln!("redacted {} region(s) -> {output}", rects.len());
    Ok(())
}

/// Open a document and return its leaf page references.
fn open_page_refs(src: &[u8], budget: &Budget, g: &mut BudgetGuard<'_>) -> Result<Vec<Ref>, String> {
    let startxref = selis_pdf_cos::xref::find_startxref(src, 4096).unwrap_or(0);
    let doc = selis_pdf_cos::parse_revisions(src, startxref, budget, g)
        .map_err(|e| format!("cannot open: {e}"))?;
    let rev = doc
        .revisions()
        .last()
        .ok_or_else(|| "no revisions".to_string())?;
    let catalog = selis_pdf_cos::copy::resolve_ref(src, &doc, rev.root.ok_or_else(|| "no /Root".to_string())?, budget, g)
        .map_err(|e| format!("resolve /Root: {e}"))?;
    let pages_ref = match catalog {
        Obj::Dict(ref pairs) => pairs
            .iter()
            .find(|(k, _)| k.as_slice() == b"Pages")
            .and_then(|(_, v)| match v {
                Obj::Ref(r) => Some(*r),
                _ => None,
            }),
        _ => None,
    }
    .ok_or_else(|| "catalog has no /Pages".to_string())?;
    walk_pages(src, &doc, pages_ref, budget, g, 0)
}

/// Resolve an object by its revision xref entry (handles compressed objects).
fn resolve_ref_obj(
    src: &[u8],
    doc: &selis_pdf_cos::Doc,
    r: Ref,
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
) -> Result<Obj, String> {
    selis_pdf_cos::copy::resolve_ref(src, doc, r, budget, g).map_err(|e| e.to_string())
}

/// Walk a pages-tree node, collecting leaf page refs.
fn walk_pages(
    src: &[u8],
    doc: &selis_pdf_cos::Doc,
    node: Ref,
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
    depth: usize,
) -> Result<Vec<Ref>, String> {
    if depth > 32 {
        return Err("page tree too deep".to_string());
    }
    let obj = selis_pdf_cos::copy::resolve_ref(src, doc, node, budget, g).map_err(|e| e.to_string())?;
    let Obj::Dict(pairs) = &obj else {
        return Ok(Vec::new());
    };
    let ty = pairs
        .iter()
        .find(|(k, _)| k.as_slice() == b"Type")
        .and_then(|(_, v)| match v {
            Obj::Name(n) => Some(n.as_slice().to_vec()),
            _ => None,
        });
    match ty.as_deref() {
        Some(b"Page") => Ok(vec![node]),
        Some(b"Pages") | None => {
            let kids: Vec<Ref> = pairs
                .iter()
                .find(|(k, _)| k.as_slice() == b"Kids")
                .and_then(|(_, v)| match v {
                    Obj::Array(items) => Some(
                        items
                            .iter()
                            .filter_map(|o| match o {
                                Obj::Ref(r) => Some(*r),
                                _ => None,
                            })
                            .collect(),
                    ),
                    _ => None,
                })
                .unwrap_or_default();
            let mut out = Vec::new();
            for kid in kids {
                out.extend(walk_pages(src, doc, kid, budget, g, depth + 1)?);
            }
            Ok(out)
        }
        _ => Ok(Vec::new()),
    }
}

/// Copy a leaf page (content + resources) into the output document.
fn copy_page(
    merged: &mut selis_pdf_cos::doc_writer::DocumentBuilder,
    src: &[u8],
    page_ref: Ref,
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
    _next_num: &mut u32,
) -> Result<(), String> {
    let startxref = selis_pdf_cos::xref::find_startxref(src, 4096).unwrap_or(0);
    let doc = selis_pdf_cos::parse_revisions(src, startxref, budget, g)
        .map_err(|e| format!("cannot open: {e}"))?;
    let rev = doc.revisions().last().ok_or_else(|| "no revisions".to_string())?;
    let (media, content_refs, resources_ref) = page_info(src, &doc, page_ref, budget, g)?;
    let mut roots: Vec<Ref> = content_refs.clone();
    if let Some(r) = resources_ref {
        roots.push(r);
    }
    let (objects, remap) =
        selis_pdf_cos::copy::collect_objects(src, &roots, merged.next_num_mut(), budget, g)
            .map_err(|e| format!("copy objects: {e}"))?;
    for (num, obj) in objects {
        merged.add_object(num, obj);
    }
    let new_contents: Vec<Ref> = content_refs
        .iter()
        .map(|r| Ref::new(remap.get(&r.num).copied().unwrap_or(r.num), r.gen))
        .collect();
    let new_resources = resources_ref
        .map(|r| Ref::new(remap.get(&r.num).copied().unwrap_or(r.num), r.gen));
    merged.add_page_with(media.0, media.1, &new_contents, new_resources);
    Ok(())
}

/// The page's (media box, content refs, resources ref).
fn page_info(
    src: &[u8],
    doc: &selis_pdf_cos::Doc,
    page_ref: Ref,
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
) -> Result<((f64, f64), Vec<Ref>, Option<Ref>), String> {
    let page = resolve_ref_obj(src, doc, page_ref, budget, g)?;
    let Obj::Dict(pairs) = &page else {
        return Err("page is not a dict".to_string());
    };
    let media = match pairs.iter().find(|(k, _)| k.as_slice() == b"MediaBox") {
        Some((_, Obj::Array(items))) if items.len() >= 4 => {
            let n = |i: usize| match items.get(i) {
                Some(Obj::Int(v)) => Some(*v as f64),
                Some(Obj::Real { scaled, scale }) => Some(*scaled as f64 / 10f64.powi(*scale as i32)),
                _ => None,
            };
            (
                n(2).unwrap_or(0.0).max(n(0).unwrap_or(0.0)),
                n(3).unwrap_or(0.0).max(n(1).unwrap_or(0.0)),
            )
        }
        _ => (612.0, 792.0),
    };
    let contents = match pairs.iter().find(|(k, _)| k.as_slice() == b"Contents") {
        Some((_, Obj::Ref(r))) => vec![*r],
        Some((_, Obj::Array(items))) => items
            .iter()
            .filter_map(|o| match o {
                Obj::Ref(r) => Some(*r),
                _ => None,
            })
            .collect(),
        _ => Vec::new(),
    };
    let resources = pairs
        .iter()
        .find(|(k, _)| k.as_slice() == b"Resources")
        .and_then(|(_, v)| match v {
            Obj::Ref(r) => Some(*r),
            _ => None,
        });
    Ok((media, contents, resources))
}

/// Copy a page, appending black rects over `rects` and stripping text that
/// falls within them.
fn copy_page_redacted(
    merged: &mut selis_pdf_cos::doc_writer::DocumentBuilder,
    src: &[u8],
    page_ref: Ref,
    rects: &[(f64, f64, f64, f64)],
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
    next_num: &mut u32,
) -> Result<(), String> {
    let startxref = selis_pdf_cos::xref::find_startxref(src, 4096).unwrap_or(0);
    let doc = selis_pdf_cos::parse_revisions(src, startxref, budget, g)
        .map_err(|e| format!("cannot open: {e}"))?;
    let rev = doc.revisions().last().ok_or_else(|| "no revisions".to_string())?;
    let (media, content_refs, resources_ref) = page_info(src, &doc, page_ref, budget, g)?;

    // Rebuild the content streams with redaction: strip text inside the
    // regions, then append black fills over them.
    let mut rebuilt = Vec::new();
    for content_ref in &content_refs {
        let content_obj = resolve_ref_obj(src, &doc, *content_ref, budget, g)?;
        if let Obj::Stream { data, .. } = &content_obj {
            rebuilt.extend(strip_text_in_regions(data.as_slice(), rects));
            rebuilt.push(b'\n');
        }
    }
    // Append black rects over the regions.
    let mut cb = selis_pdf_cos::doc_writer::ContentBuilder::new();
    cb.set_fill(0.0, 0.0, 0.0);
    for (x, y, w, h) in rects {
        cb.fill_rect(*x, *y, *w, *h);
    }
    rebuilt.extend_from_slice(&cb.to_bytes());

    // Copy the (rebuilt) content as a fresh stream, then the resources.
    let content_num = *merged.next_num_mut();
    *merged.next_num_mut() = merged.next_num_mut().saturating_add(1);
    merged.add_object(
        content_num,
        Obj::Stream {
            dict: vec![(
                selis_bytes::Bytes::copy_from_slice(b"Length"),
                Obj::Int(i64::try_from(rebuilt.len()).unwrap_or(i64::MAX)),
            )],
            data: selis_bytes::Bytes::from(rebuilt),
        },
    );
    let mut roots = Vec::new();
    if let Some(r) = resources_ref {
        roots.push(r);
    }
    let (objects, remap) =
        selis_pdf_cos::copy::collect_objects(src, &roots, merged.next_num_mut(), budget, g)
            .map_err(|e| format!("copy resources: {e}"))?;
    for (num, obj) in objects {
        merged.add_object(num, obj);
    }
    let new_resources = resources_ref.map(|r| Ref::new(remap.get(&r.num).copied().unwrap_or(r.num), r.gen));
    merged.add_page_with(media.0, media.1, &[Ref::new(content_num, 0)], new_resources);
    Ok(())
}

/// A minimal content-stream rewriter: drop `Tj`/`TJ`/`'`/`"` show operations
/// whose text origin is inside one of `rects`. The token stream is walked
/// tracking the text line matrix; `BT`/`ET` and state operators pass through.
fn strip_text_in_regions(content: &[u8], rects: &[(f64, f64, f64, f64)]) -> Vec<u8> {
    use selis_pdf_cos::{Number, Token};
    let budget = Budget::unlimited();
    let mut g = budget.guard();
    let mut lexer = selis_pdf_cos::Lexer::new(content);
    let mut out: Vec<u8> = Vec::new();
    let mut tm = [1.0f64, 0.0, 0.0, 1.0, 0.0, 0.0];
    let mut in_text = false;
    let mut tokens: Vec<Token> = Vec::new();
    while let Ok(Some(t)) = lexer.next_token(&mut g) {
        tokens.push(t);
    }
    let n = |t: &Token| -> Option<f64> {
        match t {
            Token::Number(Number::Int(v)) => Some(*v as f64),
            Token::Number(Number::Real { scaled, scale }) => {
                Some(*scaled as f64 / 10f64.powi(i32::from(*scale)))
            }
            _ => None,
        }
    };
    let mut i = 0usize;
    while i < tokens.len() {
        let t = &tokens[i];
        match t {
            Token::Name(name) => {
                let op = String::from_utf8_lossy(name.as_slice()).to_string();
                match op.as_str() {
                    "BT" => {
                        in_text = true;
                        tm = [1.0, 0.0, 0.0, 1.0, 0.0, 0.0];
                        emit(&mut out, &tokens[i..=i]);
                        i += 1;
                    }
                    "ET" => {
                        in_text = false;
                        emit(&mut out, &tokens[i..=i]);
                        i += 1;
                    }
                    "Td" | "TD" => {
                        if let (Some(x), Some(y)) = (tokens.get(i + 1).and_then(n), tokens.get(i + 2).and_then(n)) {
                            tm[4] += x;
                            tm[5] += y;
                            emit(&mut out, &tokens[i..=i.saturating_add(2)]);
                            i += 3;
                        } else {
                            i += 1;
                        }
                    }
                    "Tm" => {
                        if let (Some(a), Some(b), Some(c), Some(d), Some(e), Some(f)) = (
                            tokens.get(i + 1).and_then(n),
                            tokens.get(i + 2).and_then(n),
                            tokens.get(i + 3).and_then(n),
                            tokens.get(i + 4).and_then(n),
                            tokens.get(i + 5).and_then(n),
                            tokens.get(i + 6).and_then(n),
                        ) {
                            tm = [a, b, c, d, e, f];
                            emit(&mut out, &tokens[i..=i.saturating_add(6)]);
                            i += 7;
                        } else {
                            i += 1;
                        }
                    }
                    "Tj" => {
                        let (x, y) = (tm[4], tm[5]);
                        if in_text && inside_any(rects, x, y) {
                            i += 2;
                        } else {
                            emit(&mut out, &tokens[i..=i.saturating_add(1)]);
                            i += 2;
                        }
                    }
                    "'" | "\"" => {
                        let (x, y) = (tm[4], tm[5]);
                        if in_text && inside_any(rects, x, y) {
                            i += 2;
                        } else {
                            emit(&mut out, &tokens[i..=i.saturating_add(1)]);
                            i += 2;
                        }
                    }
                    "TJ" => {
                        let (x, y) = (tm[4], tm[5]);
                        if in_text && inside_any(rects, x, y) {
                            i += 2;
                        } else {
                            emit(&mut out, &tokens[i..=i.saturating_add(1)]);
                            i += 2;
                        }
                    }
                    _ => {
                        let mut j = i.saturating_add(1);
                        while j < tokens.len() && !matches!(tokens[j], Token::Name(_)) {
                            j = j.saturating_add(1);
                        }
                        emit(&mut out, &tokens[i..j]);
                        i = j;
                    }
                }
            }
            _ => {
                i += 1;
            }
        }
    }
    out
}

/// Whether a point is inside any rect.
fn inside_any(rects: &[(f64, f64, f64, f64)], x: f64, y: f64) -> bool {
    rects
        .iter()
        .any(|(rx, ry, rw, rh)| x >= *rx && x <= rx + rw && y >= *ry && y <= ry + rh)
}

/// Emit a token slice back to bytes (via the object writer for strings/names).
fn emit(out: &mut Vec<u8>, toks: &[selis_pdf_cos::Token]) {
    use selis_pdf_cos::{Number, Token};
    let mut buf: Vec<u8> = Vec::new();
    for t in toks {
        match t {
            Token::Number(n) => {
                let s = match n {
                    Number::Int(v) => format!("{v}"),
                    Number::Real { scaled, scale } => {
                        let v = *scaled as f64 / 10f64.powi(i32::from(*scale));
                        format!("{v:.3}").trim_end_matches('0').trim_end_matches('.').to_string()
                    }
                };
                buf.extend_from_slice(s.as_bytes());
                buf.push(b' ');
            }
            Token::Name(b) => {
                buf.extend_from_slice(b"/");
                buf.extend_from_slice(b.as_slice());
                buf.push(b' ');
            }
            Token::String(b) => {
                buf.push(b'(');
                for &c in b.as_slice() {
                    if c == b'(' || c == b')' || c == b'\\' {
                        buf.push(b'\\');
                    }
                    buf.push(c);
                }
                buf.push(b')');
                buf.push(b' ');
            }
            _ => {}
        }
    }
    out.extend_from_slice(&buf);
}