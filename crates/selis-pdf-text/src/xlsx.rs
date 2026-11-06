//! Excel (`.xlsx`) packaging for reconstructed tables (Phase 3 — PDF → Excel).
//!
//! One worksheet per table, plus a `Report` sheet that states each table's page,
//! size and confidence — the same number the CSV export writes in its comment
//! line, so a reader can judge the reconstruction instead of trusting it. Cells
//! are inline strings: a cell that looks like `1,234` stays the string the page
//! drew rather than becoming a number a spreadsheet would silently reinterpret.
//!
//! Packaging and the read-back check live here, in the core crate, so the CLI,
//! the browser chunk and the desktop app produce the same workbook.

#![allow(
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation,
    clippy::integer_division
)]

use selis_archive::{read_zip, store_zip};

use crate::table::Table;
use crate::xml::xml_text;

/// Excel's per-cell character limit.
pub const MAX_CELL_CHARS: usize = 32_767;

/// The media type an `.xlsx` carries.
pub const XLSX_MEDIA_TYPE: &str =
    "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet";

/// A spreadsheet column name: `A`, `Z`, `AA`, `AB`.
fn column_name(mut index: usize) -> String {
    let mut name = String::new();
    loop {
        let digit = index % 26;
        name.insert(0, char::from(b'A'.saturating_add(digit as u8)));
        if index < 26 {
            break;
        }
        index = index / 26 - 1;
    }
    name
}

/// Escape and bound one cell's text.
fn cell_text(text: &str) -> String {
    if text.chars().count() <= MAX_CELL_CHARS {
        return xml_text(text);
    }
    let mut out: String = text
        .chars()
        .take(MAX_CELL_CHARS.saturating_sub(1))
        .collect();
    out.push('\u{2026}');
    xml_text(&out)
}

fn sheet_name(index: usize) -> String {
    format!("Table {}", index.saturating_add(1))
}

fn row_xml(row: usize, cells: &[String]) -> String {
    let mut out = format!("<row r=\"{}\">", row.saturating_add(1));
    for (column, value) in cells.iter().enumerate() {
        out.push_str(&format!(
            "<c r=\"{}{}\" t=\"inlineStr\"><is><t xml:space=\"preserve\">{}</t></is></c>",
            column_name(column),
            row.saturating_add(1),
            cell_text(value)
        ));
    }
    out.push_str("</row>");
    out
}

fn sheet(rows: &[Vec<String>]) -> String {
    let mut out = String::from(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n\
<worksheet xmlns=\"http://schemas.openxmlformats.org/spreadsheetml/2006/main\"><sheetData>",
    );
    for (index, row) in rows.iter().enumerate() {
        out.push_str(&row_xml(index, row));
    }
    out.push_str("</sheetData></worksheet>\n");
    out
}

fn report_rows(tables: &[Table]) -> Vec<Vec<String>> {
    let mut rows = vec![vec![
        "Page".to_string(),
        "Table".to_string(),
        "Columns".to_string(),
        "Rows".to_string(),
        "Confidence".to_string(),
    ]];
    for (index, table) in tables.iter().enumerate() {
        rows.push(vec![
            table.page.to_string(),
            sheet_name(index),
            table.columns.to_string(),
            table.rows.len().to_string(),
            format!("{:.2}", table.confidence),
        ]);
    }
    rows
}

fn content_types(sheets: usize) -> String {
    let mut out = String::from(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n\
<Types xmlns=\"http://schemas.openxmlformats.org/package/2006/content-types\">\
<Default Extension=\"rels\" ContentType=\"application/vnd.openxmlformats-package.relationships+xml\"/>\
<Default Extension=\"xml\" ContentType=\"application/xml\"/>\
<Override PartName=\"/xl/workbook.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml\"/>\
<Override PartName=\"/xl/styles.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.spreadsheetml.styles+xml\"/>\
<Override PartName=\"/docProps/core.xml\" ContentType=\"application/vnd.openxmlformats-package.core-properties+xml\"/>",
    );
    for index in 1..=sheets {
        out.push_str(&format!(
            "<Override PartName=\"/xl/worksheets/sheet{index}.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml\"/>"
        ));
    }
    out.push_str("</Types>\n");
    out
}

fn workbook(sheets: &[String]) -> String {
    let mut out = String::from(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n\
<workbook xmlns=\"http://schemas.openxmlformats.org/spreadsheetml/2006/main\" \
xmlns:r=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships\"><sheets>",
    );
    for (index, name) in sheets.iter().enumerate() {
        out.push_str(&format!(
            "<sheet name=\"{}\" sheetId=\"{}\" r:id=\"rId{}\"/>",
            xml_text(name),
            index.saturating_add(1),
            index.saturating_add(1)
        ));
    }
    out.push_str("</sheets></workbook>\n");
    out
}

fn workbook_rels(sheets: usize) -> String {
    let mut out = String::from(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n\
<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\">",
    );
    for index in 1..=sheets {
        out.push_str(&format!(
            "<Relationship Id=\"rId{index}\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet\" Target=\"worksheets/sheet{index}.xml\"/>"
        ));
    }
    out.push_str("</Relationships>\n");
    out
}

fn styles() -> String {
    String::from(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n\
<styleSheet xmlns=\"http://schemas.openxmlformats.org/spreadsheetml/2006/main\">\
<fonts count=\"1\"><font><sz val=\"11\"/><name val=\"Calibri\"/></font></fonts>\
<fills count=\"1\"><fill><patternFill patternType=\"none\"/></fill></fills>\
<borders count=\"1\"><border/></borders>\
<cellStyleXfs count=\"1\"><xf/></cellStyleXfs>\
<cellXfs count=\"1\"><xf/></cellXfs>\
</styleSheet>\n",
    )
}

fn root_rels() -> String {
    String::from(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n\
<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\">\
<Relationship Id=\"rId1\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument\" Target=\"xl/workbook.xml\"/>\
<Relationship Id=\"rId2\" Type=\"http://schemas.openxmlformats.org/package/2006/relationships/metadata/core-properties\" Target=\"docProps/core.xml\"/>\
</Relationships>\n",
    )
}

fn core(title: &str) -> String {
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n\
<cp:coreProperties xmlns:cp=\"http://schemas.openxmlformats.org/package/2006/metadata/core-properties\" \
xmlns:dc=\"http://purl.org/dc/elements/1.1/\" xmlns:dcterms=\"http://purl.org/dc/terms/\" \
xmlns:xsi=\"http://www.w3.org/2001/XMLSchema-instance\">\
<dc:title>{}</dc:title><dc:creator>Selis</dc:creator></cp:coreProperties>\n",
        xml_text(title)
    )
}

/// Build and verify a workbook for the reconstructed tables.
///
/// # Errors
///
/// A package that fails [`verify_xlsx`]; the check runs before the bytes are
/// returned, so a caller never ships a workbook it has not read back.
pub fn to_xlsx(title: &str, tables: &[Table]) -> Result<Vec<u8>, String> {
    let names: Vec<String> = (0..tables.len()).map(sheet_name).collect();
    let content = content_types(tables.len().saturating_add(1));
    let root = root_rels();
    let book = workbook(&names);
    let rels = workbook_rels(tables.len().saturating_add(1));
    let style_sheet = styles();
    let core_props = core(title);
    let report = sheet(&report_rows(tables));
    let bodies: Vec<String> = tables.iter().map(|table| sheet(&table.rows)).collect();

    let mut owned: Vec<(String, &[u8])> = Vec::with_capacity(tables.len().saturating_add(7));
    owned.push(("[Content_Types].xml".to_string(), content.as_bytes()));
    owned.push(("_rels/.rels".to_string(), root.as_bytes()));
    owned.push(("docProps/core.xml".to_string(), core_props.as_bytes()));
    owned.push(("xl/workbook.xml".to_string(), book.as_bytes()));
    owned.push(("xl/_rels/workbook.xml.rels".to_string(), rels.as_bytes()));
    owned.push(("xl/styles.xml".to_string(), style_sheet.as_bytes()));
    owned.push(("xl/worksheets/sheet1.xml".to_string(), report.as_bytes()));
    for (index, body) in bodies.iter().enumerate() {
        owned.push((
            format!("xl/worksheets/sheet{}.xml", index.saturating_add(2)),
            body.as_bytes(),
        ));
    }
    let entries: Vec<(&str, &[u8])> = owned
        .iter()
        .map(|(name, data)| (name.as_str(), *data))
        .collect();
    let xlsx = store_zip(&entries);
    verify_xlsx(&xlsx, tables)?;
    Ok(xlsx)
}

/// Check that every table and every cell survived the package.
///
/// # Errors
///
/// A sentence naming the first table or cell the workbook does not carry.
pub fn verify_xlsx(xlsx: &[u8], tables: &[Table]) -> Result<(), String> {
    let entries = read_zip(xlsx)?;
    let has = |name: &str| entries.iter().any(|(entry, _)| entry == name);
    for required in [
        "[Content_Types].xml",
        "xl/workbook.xml",
        "xl/worksheets/sheet1.xml",
    ] {
        if !has(required) {
            return Err(format!("the package is missing {required}"));
        }
    }
    for (index, table) in tables.iter().enumerate() {
        let name = format!("xl/worksheets/sheet{}.xml", index.saturating_add(2));
        let body = entries
            .iter()
            .find(|(entry, _)| entry == &name)
            .map(|(_, data)| String::from_utf8_lossy(data).into_owned())
            .ok_or_else(|| {
                format!(
                    "table {} is missing from the workbook",
                    index.saturating_add(1)
                )
            })?;
        let mut cursor = 0usize;
        for row in &table.rows {
            for cell in row {
                let needle = cell_text(cell);
                if needle.is_empty() {
                    continue;
                }
                let rest = body
                    .get(cursor..)
                    .ok_or_else(|| format!("table {} ended early", index.saturating_add(1)))?;
                let at = rest.find(&needle).ok_or_else(|| {
                    format!("table {} lost a cell: {:?}", index.saturating_add(1), cell)
                })?;
                cursor = cursor.saturating_add(at).saturating_add(needle.len());
            }
        }
    }
    let report = entries
        .iter()
        .find(|(entry, _)| entry == "xl/worksheets/sheet1.xml")
        .map(|(_, data)| String::from_utf8_lossy(data).into_owned())
        .unwrap_or_default();
    if !report.contains("Confidence") {
        return Err("the report sheet does not name confidence".to_string());
    }
    Ok(())
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
    use selis_geom::Rect;

    fn table(page: u32, rows: Vec<Vec<&str>>) -> Table {
        let cells: Vec<Vec<String>> = rows
            .into_iter()
            .map(|row| row.into_iter().map(str::to_string).collect())
            .collect();
        let columns = cells.first().map_or(0, Vec::len);
        Table {
            page,
            columns,
            rows: cells,
            confidence: 0.92,
            bbox: Rect::new(0.0, 0.0, 100.0, 100.0),
        }
    }

    #[test]
    fn column_names_follow_the_sheet_grammar() {
        assert_eq!(column_name(0), "A");
        assert_eq!(column_name(25), "Z");
        assert_eq!(column_name(26), "AA");
        assert_eq!(column_name(27), "AB");
        assert_eq!(column_name(51), "AZ");
        assert_eq!(column_name(52), "BA");
    }

    #[test]
    fn a_workbook_round_trips_and_keeps_its_cells() {
        let tables = vec![
            table(1, vec![vec!["Item", "Total"], vec!["Tea & cake", "4"]]),
            table(2, vec![vec!["Name", "Score"], vec!["Ada", "99"]]),
        ];
        let xlsx = to_xlsx("Ledger", &tables).expect("the workbook should verify");
        verify_xlsx(&xlsx, &tables).expect("the check should pass");
        let entries = read_zip(&xlsx).expect("read back");
        let body = &entries
            .iter()
            .find(|(name, _)| name == "xl/worksheets/sheet2.xml")
            .expect("first table")
            .1;
        let body = String::from_utf8_lossy(body);
        assert!(body.contains("Tea &amp; cake"));
        assert!(body.contains("r=\"B2\""));
        let report = &entries
            .iter()
            .find(|(name, _)| name == "xl/worksheets/sheet1.xml")
            .expect("report")
            .1;
        assert!(String::from_utf8_lossy(report).contains("0.92"));
    }

    #[test]
    fn an_empty_document_still_writes_a_report() {
        let xlsx = to_xlsx("Empty", &[]).expect("write");
        verify_xlsx(&xlsx, &[]).expect("verify");
    }

    #[test]
    fn verification_refuses_a_dropped_cell() {
        let tables = vec![table(1, vec![vec!["a"], vec!["b"]])];
        let xlsx = to_xlsx("T", &tables).expect("write");
        let mut broken = tables.clone();
        broken[0].rows.push(vec!["never written".to_string()]);
        assert!(verify_xlsx(&xlsx, &broken).is_err());
    }
}
