//! Excel (`.xlsx`) → PDF: the sheet's cells, as a table per sheet.
//!
//! # What this does
//!
//! The workbook's sheet order comes from `workbook.xml`. Each sheet's used
//! range is mapped onto one layout table per sheet — no `calcChain`, no
//! formulas evaluated (cached formula values are printed as stored), shared
//! and inline strings resolved. A sheet is a level-2 heading followed by one
//! table; every sheet starts on a new page.
//!
//! # What this does not do (said plainly)
//!
//! It is not a spreadsheet renderer. Cell formats are not applied, so dates
//! print as their stored serial numbers and currency/percent/percent formats
//! are dropped; column widths, merged cells, text wrapping inside cells,
//! images, charts, conditional formatting, comments, frozen panes, sheet tab
//! colour, and anything the file stores in a chart part are not carried. A
//! formula is printed with its cached value; a formula with no cached value
//! prints empty. An encrypted or genuinely malformed workbook is refused by
//! the archive reader itself.

#![allow(clippy::arithmetic_side_effects, clippy::indexing_slicing)]

use selis_error::Result;
use selis_sandbox::{Budget, BudgetGuard, Resource};

use crate::blocks::{Block, Span, Style};
use crate::ooxml::{
    attr, child, children, deep_text, malformed, parse_part, part, rel_attr, relationships,
    resolve_target, split_path,
};
use crate::xmlmini::Element;
use crate::{emit, Options};

/// The workbook part's path.
const WORKBOOK: &str = "xl/workbook.xml";
/// The shared strings part's path.
const SHARED_STRINGS: &str = "xl/sharedStrings.xml";
/// The fallback scan prefix for worksheets when the workbook declares none.
const SHEET_PREFIX: &str = "xl/worksheets/sheet";

/// Convert an Excel workbook to PDF bytes.
///
/// # Budget
///
/// Charges the input bytes, the expanded package bytes and the generated object
/// graph against `budget`.
///
/// # Malformed Input
///
/// A package that is not a ZIP, has no `xl/workbook.xml`, or whose parts are
/// not parseable XML is refused with a message naming the part. A workbook
/// with sheets but all empty produces one empty page per sheet.
///
/// # Output Guarantees
///
/// The bytes are a complete single-revision PDF 1.4 document this engine and
/// any conforming reader can open.
pub(crate) fn xlsx_to_pdf(
    input: &[u8],
    opts: &Options,
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
) -> Result<Vec<u8>> {
    g.charge(
        Resource::Bytes,
        u64::try_from(input.len()).unwrap_or(u64::MAX),
    )?;
    let entries = selis_archive::read_zip(input)
        .map_err(|message| malformed("xlsx", format!("not an Excel workbook: {message}")))?;
    let expanded = entries.iter().fold(0u64, |total, (_, data)| {
        total.saturating_add(u64::try_from(data.len()).unwrap_or(u64::MAX))
    });
    g.charge(Resource::Bytes, expanded)?;
    let main = part(&entries, WORKBOOK).ok_or_else(|| {
        malformed(
            "xlsx",
            "the package has no xl/workbook.xml (is this an Excel file?)".to_string(),
        )
    })?;
    let root = parse_part(main, "xlsx", WORKBOOK)?;
    let shared = shared_strings(&entries, &root);
    let sheets = sheet_parts(&entries, &root)?;

    let mut blocks = Vec::new();
    for (index, (name, path)) in sheets.iter().enumerate() {
        if index > 0 {
            blocks.push(Block::PageBreak);
        }
        blocks.push(Block::Heading {
            level: 2,
            spans: vec![Span {
                style: Style::Bold,
                text: if name.is_empty() {
                    format!("Sheet {}", index + 1)
                } else {
                    name.clone()
                },
            }],
        });
        let bytes = part(&entries, path)
            .ok_or_else(|| malformed("xlsx", format!("{path}: the sheet part is missing")))?;
        let sheet = parse_part(bytes, "xlsx", path)?;
        let rows = sheet_rows(&sheet, &shared);
        if rows
            .iter()
            .any(|row| row.iter().any(|cell| !cell.is_empty()))
        {
            blocks.push(Block::Table {
                head: Vec::new(),
                rows,
            });
        }
    }
    emit::build(&blocks, opts, budget, g)
}

/// One sheet's name and resolved part path, in workbook order.
fn sheet_parts(entries: &[(String, Vec<u8>)], root: &Element) -> Result<Vec<(String, String)>> {
    let rels = relationships(entries, WORKBOOK, "xlsx");
    let (dir, _) = split_path(WORKBOOK);
    let mut by_id = std::collections::BTreeMap::new();
    for rel in rels {
        by_id.insert(rel.id, resolve_target(dir, &rel.target));
    }
    let mut ordered = Vec::new();
    if let Some(list) = root.find_first("sheets") {
        for sheet in children(list, "sheet") {
            let name = attr(sheet, "name").unwrap_or("").trim().to_string();
            let Some(id) = rel_attr(sheet) else {
                continue;
            };
            let Some(target) = by_id.get(id) else {
                continue;
            };
            ordered.push((name, target.clone()));
        }
    }
    if !ordered.is_empty() {
        return Ok(ordered);
    }
    // A workbook that declares no sheets is still a workbook: enumerate the
    // worksheet parts that exist and order them by their numeric suffix.
    let mut scanned: Vec<(u32, String)> = Vec::new();
    for (name, _) in entries {
        let Some(rest) = name.strip_prefix(SHEET_PREFIX) else {
            continue;
        };
        let Some(num) = rest.strip_suffix(".xml") else {
            continue;
        };
        if let Ok(num) = num.parse::<u32>() {
            scanned.push((num, name.clone()));
        }
    }
    scanned.sort_by_key(|(num, _)| *num);
    Ok(scanned
        .into_iter()
        .map(|(num, name)| (format!("Sheet {num}"), name))
        .collect())
}

/// The shared string table, or an empty table when the part is absent.
fn shared_strings(entries: &[(String, Vec<u8>)], _root: &Element) -> Vec<String> {
    let Some(bytes) = part(entries, SHARED_STRINGS) else {
        return Vec::new();
    };
    let Ok(root) = parse_part(bytes, "xlsx", SHARED_STRINGS) else {
        return Vec::new();
    };
    root.find_all("si").into_iter().map(si_text).collect()
}

/// One shared string, with its `rPh` runs excluded.
fn si_text(si: &Element) -> String {
    fn walk(node: &Element, out: &mut String) {
        for child in &node.children {
            match child.tag.as_str() {
                "rph" => {}
                "t" => out.push_str(&deep_text(child)),
                _ => walk(child, out),
            }
        }
    }
    let mut out = String::new();
    walk(si, &mut out);
    out
}

/// The used range of one sheet as rows of cells.
fn sheet_rows(sheet: &Element, shared: &[String]) -> Vec<Vec<Vec<Span>>> {
    let Some(data) = sheet.find_first("sheetdata") else {
        return Vec::new();
    };
    let mut rows: Vec<Vec<Vec<Span>>> = Vec::new();
    for row in children(data, "row") {
        let mut cells: Vec<(usize, Vec<Span>)> = Vec::new();
        let mut sequential = 0usize;
        for cell in children(row, "c") {
            let (col, consumed) = cell_column(cell, sequential);
            sequential = consumed;
            let text = cell_text(cell, shared);
            let spans = if text.is_empty() {
                Vec::new()
            } else {
                vec![Span {
                    style: Style::Regular,
                    text,
                }]
            };
            cells.push((col, spans));
        }
        let width = cells.iter().map(|(col, _)| *col).max().map_or(0, |m| m + 1);
        let mut expanded: Vec<Vec<Span>> = vec![Vec::new(); width];
        for (col, spans) in cells {
            if let Some(slot) = expanded.get_mut(col) {
                *slot = spans;
            }
        }
        rows.push(expanded);
    }
    // Rows may disagree on the used width; pad every row to the widest.
    let width = rows.iter().map(Vec::len).max().unwrap_or(0);
    for row in &mut rows {
        row.resize(width, Vec::new());
    }
    rows
}

/// The column index of one cell, and the next sequential column.
///
/// The `r` attribute (`B12`) wins when present and well-formed; otherwise the
/// caller's running column is used and advanced.
fn cell_column(cell: &Element, sequential: usize) -> (usize, usize) {
    if let Some(reference) = attr(cell, "r") {
        let mut column = 0usize;
        let mut seen = false;
        for byte in reference.bytes() {
            if !byte.is_ascii_alphabetic() {
                break;
            }
            // A-Z (any case) → 0..25; this cannot overflow for sane refs.
            column = column
                .saturating_mul(26)
                .saturating_add(usize::from(byte.to_ascii_uppercase() - b'A' + 1));
            seen = true;
        }
        if seen {
            let index = column.saturating_sub(1);
            return (index, index.saturating_add(1));
        }
    }
    (sequential, sequential.saturating_add(1))
}

/// One cell's text.
fn cell_text(cell: &Element, shared: &[String]) -> String {
    let kind = attr(cell, "t").unwrap_or("n").trim().to_ascii_lowercase();
    match kind.as_str() {
        "s" => child(cell, "v")
            .and_then(|node| deep_text(node).trim().parse::<usize>().ok())
            .and_then(|index| shared.get(index).cloned())
            .unwrap_or_default(),
        "inlinestr" => child(cell, "is").map(deep_text).unwrap_or_default(),
        "str" | "e" => child(cell, "v").map(deep_text).unwrap_or_default(),
        "b" => child(cell, "v")
            .map(|node| deep_text(node).trim().to_string())
            .map(|value| {
                if value == "1" {
                    "TRUE".into()
                } else {
                    "FALSE".into()
                }
            })
            .unwrap_or_else(|| "FALSE".to_string()),
        _ => child(cell, "v").map(deep_text).unwrap_or_default(),
    }
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing,
        clippy::arithmetic_side_effects
    )]

    use super::*;
    use crate::{testkit::opens_with_page_count, Options};
    use selis_sandbox::{Budget, CancelToken, FixedClock};

    fn guard() -> BudgetGuard<'static> {
        Budget::unlimited().guard_with(&FixedClock(0), CancelToken::new())
    }

    const NS_X: &str = r#"http://schemas.openxmlformats.org/spreadsheetml/2006/main"#;

    fn workbook() -> Vec<u8> {
        let workbook = format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<workbook xmlns="{NS_X}" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><sheets>
  <sheet name="First" sheetId="1" r:id="rId1"/>
  <sheet name="Second" sheetId="2" r:id="rId2"/>
</sheets></workbook>"#
        );
        let rels = r#"<?xml version="1.0" encoding="UTF-8"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
  <Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet1.xml"/>
  <Relationship Id="rId2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet2.xml"/>
  <Relationship Id="rId9" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/sharedStrings" Target="sharedStrings.xml"/>
</Relationships>"#;
        let shared = format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<sst xmlns="{NS_X}" count="4" uniqueCount="4"><si><t>Quarter</t></si><si><t>Amount</t></si><si><r><rPr><b/></rPr><t>Rich</t></r><r><t> text</t></r></si><si><t>Inline</t></si></sst>"#
        );
        let sheet1 = format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<worksheet xmlns="{NS_X}"><sheetData>
  <row r="1"><c r="A1" t="s"><v>0</v></c><c r="B1" t="s"><v>1</v></c></row>
  <row r="2"><c r="A2" t="s"><v>2</v></c><c r="B2"><v>42</v></c></row>
  <row r="3"><c r="A3" t="b"><v>1</v></c><c r="B3" t="str"><v>cached</v></c></row>
</sheetData></worksheet>"#
        );
        let sheet2 = format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<worksheet xmlns="{NS_X}"><sheetData>
  <row r="2"><c r="B2" t="inlineStr"><is><t>Inline</t></is></c></row>
</sheetData></worksheet>"#
        );
        selis_archive::store_zip(&[
            ("xl/workbook.xml", workbook.as_bytes()),
            ("xl/_rels/workbook.xml.rels", rels.as_bytes()),
            ("xl/sharedStrings.xml", shared.as_bytes()),
            ("xl/worksheets/sheet1.xml", sheet1.as_bytes()),
            ("xl/worksheets/sheet2.xml", sheet2.as_bytes()),
        ])
    }

    #[test]
    fn sheets_render_in_workbook_order_one_page_each() {
        let bytes = xlsx_to_pdf(
            &workbook(),
            &Options::default(),
            &Budget::unlimited(),
            &mut guard(),
        )
        .expect("convert");
        assert!(bytes.starts_with(b"%PDF-1.4"));
        let text = String::from_utf8_lossy(&bytes);
        for word in [
            "Quarter", "Amount", "Rich", "text", "TRUE", "cached", "Inline",
        ] {
            assert!(text.contains(word), "missing {word}");
        }
        let first = text.find("Quarter").expect("marker Q");
        let second = text.find("Inline").expect("marker I");
        assert!(first < second);
        opens_with_page_count(&bytes, 2);
    }

    #[test]
    fn a_package_without_the_workbook_part_is_named() {
        let broken = selis_archive::store_zip(&[("xl/worksheets/sheet1.xml", b"<x/>")]);
        let Err(error) = xlsx_to_pdf(
            &broken,
            &Options::default(),
            &Budget::unlimited(),
            &mut guard(),
        ) else {
            panic!("expected a refusal");
        };
        assert_eq!(error.ctx().during, Some("xlsx"));
        assert!(error.to_string().contains("workbook.xml"));
    }

    #[test]
    fn a_malformed_zip_is_refused() {
        let Err(_) = xlsx_to_pdf(
            b"not a zip",
            &Options::default(),
            &Budget::unlimited(),
            &mut guard(),
        ) else {
            panic!("expected a refusal");
        };
    }
}
