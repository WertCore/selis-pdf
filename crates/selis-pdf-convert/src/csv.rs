//! CSV → PDF: the table dump.
//!
//! Splits on commas with simple quoted-field support, sizes columns evenly
//! and paginates with a repeated header row. No claim of spreadsheet fidelity:
//! this is a faithful print of the cells, nothing more. Cells are encoded to
//! WinAnsi like the rest of the converters; truncation is by characters.

use selis_bytes::Bytes;
use selis_error::Result;
use selis_pdf_cos::doc_writer::{ContentBuilder, DocumentBuilder};
use selis_pdf_cos::{Obj, Ref};
use selis_sandbox::{Budget, BudgetGuard, Resource};

use crate::blocks::encode_text;
use crate::emit::{bytes, take_num};
use crate::Options;

/// Convert CSV text to PDF bytes.
///
/// # Budget
///
/// Charges the generated object graph and output bytes against `budget`.
///
/// # Output Guarantees
///
/// A complete single-revision PDF 1.4 document this engine and any
/// conforming reader can open.
pub(crate) fn csv_to_pdf(
    input: &str,
    opts: &Options,
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
) -> Result<Vec<u8>> {
    let rows = parse_csv(input);
    let cols = rows.iter().map(Vec::len).max().unwrap_or(0);
    if cols == 0 {
        // Empty input: emit a single blank page.
        let (page_w, page_h) = opts.page_size.dims();
        let mut b = DocumentBuilder::new();
        if let Some(title) = &opts.title {
            if !title.is_empty() {
                b.set_info(b"Title", title);
            }
        }
        b.add_page(page_w, page_h, &[]);
        return b.write(budget, g);
    }

    let (page_w, page_h) = opts.page_size.dims();
    let margin = 54.0;
    let avail_w = page_w - 2.0 * margin;
    #[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation)]
    let col_w = avail_w / (cols as f64);
    let row_h = 14.0;
    let font_size = 9.0;

    let mut b = DocumentBuilder::new();
    if let Some(title) = &opts.title {
        if !title.is_empty() {
            b.set_info(b"Title", title);
        }
    }

    // Font resources: Helvetica + Helvetica-Bold.
    let f_regular = take_num(&mut b);
    b.add_object(
        f_regular,
        Obj::Dict(vec![
            (bytes(b"Type"), Obj::Name(bytes(b"Font"))),
            (bytes(b"Subtype"), Obj::Name(bytes(b"Type1"))),
            (bytes(b"BaseFont"), Obj::Name(bytes(b"Helvetica"))),
            (bytes(b"Encoding"), Obj::Name(bytes(b"WinAnsiEncoding"))),
        ]),
    );
    let f_bold = take_num(&mut b);
    b.add_object(
        f_bold,
        Obj::Dict(vec![
            (bytes(b"Type"), Obj::Name(bytes(b"Font"))),
            (bytes(b"Subtype"), Obj::Name(bytes(b"Type1"))),
            (bytes(b"BaseFont"), Obj::Name(bytes(b"Helvetica-Bold"))),
            (bytes(b"Encoding"), Obj::Name(bytes(b"WinAnsiEncoding"))),
        ]),
    );
    let res_num = take_num(&mut b);
    b.add_object(
        res_num,
        Obj::Dict(vec![(
            bytes(b"Font"),
            Obj::Dict(vec![
                (bytes(b"F1"), Obj::Ref(Ref::new(f_regular, 0))),
                (bytes(b"F2"), Obj::Ref(Ref::new(f_bold, 0))),
            ]),
        )]),
    );
    let res_ref = Ref::new(res_num, 0);

    // Rows to render, paginating with a repeated header row.
    let mut pages: Vec<Vec<(bool, Vec<String>)>> = Vec::new();
    let mut current: Vec<(bool, Vec<String>)> = Vec::new();
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        clippy::cast_precision_loss
    )]
    let rows_per_page = ((page_h - 2.0 * margin) / row_h).floor() as usize;
    let rows_per_page = if rows_per_page == 0 { 1 } else { rows_per_page };
    for (i, row) in rows.iter().enumerate() {
        let is_header = i == 0;
        if current.len() >= rows_per_page && !current.is_empty() {
            pages.push(std::mem::take(&mut current));
            if i > 0 {
                if let Some(first) = rows.first() {
                    current.push((true, first.clone()));
                }
            }
        }
        current.push((is_header, row.clone()));
    }
    if !current.is_empty() {
        pages.push(current);
    }
    if pages.is_empty() {
        pages.push(Vec::new());
    }

    for page in &pages {
        let mut cb = ContentBuilder::new();
        let mut yy = page_h - margin;
        for (is_header, cells) in page {
            for (ci, cell) in cells.iter().enumerate() {
                let x = margin + (ci as f64) * col_w + 2.0;
                let font = if *is_header { "F2" } else { "F1" };
                #[allow(
                    clippy::cast_possible_truncation,
                    clippy::cast_sign_loss,
                    clippy::cast_precision_loss
                )]
                let max_chars = ((col_w - 4.0) / (font_size * 0.5)).max(1.0) as usize;
                let display: String = cell.chars().take(max_chars).collect();
                cb.begin_text()
                    .set_font(font, font_size)
                    .set_text_matrix(x, yy)
                    .show_raw(&encode_text(&display))
                    .end_text();
            }
            yy -= row_h;
        }
        let data = cb.to_bytes();
        let content_num = take_num(&mut b);
        b.add_object(
            content_num,
            Obj::Stream {
                dict: vec![(
                    bytes(b"Length"),
                    Obj::Int(i64::try_from(data.len()).unwrap_or(i64::MAX)),
                )],
                data: Bytes::from(data),
            },
        );
        b.add_page_with(page_w, page_h, &[Ref::new(content_num, 0)], Some(res_ref));
    }

    g.charge(Resource::Objects, 10)?;
    b.write(budget, g)
}

fn parse_csv(input: &str) -> Vec<Vec<String>> {
    let mut rows: Vec<Vec<String>> = Vec::new();
    for line in input.lines() {
        if line.trim().is_empty() {
            continue;
        }
        rows.push(split_csv_line(line));
    }
    rows
}

fn split_csv_line(line: &str) -> Vec<String> {
    let mut fields = Vec::new();
    let mut current = String::new();
    let mut in_quotes = false;
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        if in_quotes {
            if c == '"' {
                if chars.peek() == Some(&'"') {
                    current.push('"');
                    chars.next();
                } else {
                    in_quotes = false;
                }
            } else {
                current.push(c);
            }
        } else if c == '"' {
            in_quotes = true;
        } else if c == ',' {
            fields.push(std::mem::take(&mut current));
        } else {
            current.push(c);
        }
    }
    fields.push(current);
    fields
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;
    use crate::PageSize;
    use selis_sandbox::{Budget, CancelToken, FixedClock};

    #[test]
    fn csv_paginates_with_a_repeated_header() {
        let mut input = String::from("h1,h2\n");
        for i in 0..80 {
            input.push_str(&format!("a{i},b{i}\n"));
        }
        let opts = Options {
            page_size: PageSize::Letter,
            title: None,
        };
        let mut g = Budget::unlimited().guard_with(&FixedClock(0), CancelToken::new());
        let bytes = csv_to_pdf(&input, &opts, &Budget::unlimited(), &mut g).expect("write");
        assert!(bytes.starts_with(b"%PDF-1.4"));
        let text = String::from_utf8_lossy(&bytes);
        // The header row is repeated on later pages; "a79" shows at the end.
        assert!(text.contains("a79"));
    }

    #[test]
    fn csv_empty_input_still_yields_a_blank_page() {
        let opts = Options::default();
        let mut g = Budget::unlimited().guard_with(&FixedClock(0), CancelToken::new());
        let bytes = csv_to_pdf("", &opts, &Budget::unlimited(), &mut g).expect("write");
        assert!(bytes.starts_with(b"%PDF-1.4"));
    }

    #[test]
    fn csv_quoted_fields_keep_commas() {
        let fields = split_csv_line("\"a,b\",c,\"d\"\"e\"");
        assert_eq!(
            fields,
            vec!["a,b".to_string(), "c".to_string(), "d\"e".to_string()]
        );
    }
}
