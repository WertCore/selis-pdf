//! CLI-level evidence for choice/list-box appearance synthesis (parity #3).
//!
//! The fill path a CLI user takes is `fill_from_form_data`: it imports an XFDF
//! file and calls `fill_forms`, which sets `synthesize_appearances`. This test
//! drives that exact path against a fixture PDF on disk, then RE-OPENS the
//! committed output and reads the widget's `/AP /N` stream to prove the
//! selected option's display label was really drawn — not just written to
//! `/V`, and not just claimed through `/NeedAppearances`.

#![cfg(test)]

use super::{fill_from_form_data, form_fields};

/// A minimal one-page PDF with a single-select combo and a single-select list
/// box. `/Opt` entries are `[export, label]` pairs so the drawn text differs
/// from the stored value.
fn choice_fixture() -> Vec<u8> {
    let objects: &[(u32, &[u8])] = &[
        (1, b"<< /Type /Catalog /Pages 2 0 R /AcroForm 5 0 R >>"),
        (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
        (
            3,
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << >> /Contents 4 0 R /Annots [6 0 R 7 0 R] >>",
        ),
        (4, b"<< /Length 0 >>\nstream\n\nendstream"),
        (
            5,
            b"<< /Fields [6 0 R 7 0 R] /DA (/Helv 0 Tf 0 g) /DR << /Font << /Helv 8 0 R >> >> >>",
        ),
        (
            6,
            b"<< /FT /Ch /Ff 131072 /T (Combo) /V (r) /Opt [[(r) (Red Label)] [(g) (Green Label)]] /Subtype /Widget /Rect [10 10 120 30] /P 3 0 R >>",
        ),
        (
            7,
            b"<< /FT /Ch /T (List) /V (a) /Opt [[(a) (Alpha)] [(b) (Beta)]] /Subtype /Widget /Rect [10 50 120 70] /P 3 0 R >>",
        ),
        (8, b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>"),
    ];
    let max = objects.iter().map(|(num, _)| *num).max().unwrap_or(0);
    let mut out = Vec::new();
    out.extend_from_slice(b"%PDF-1.4\n");
    let mut offsets = vec![0u64; max as usize + 1];
    for (num, body) in objects {
        offsets[*num as usize] = out.len() as u64;
        out.extend_from_slice(format!("{num} 0 obj\n").as_bytes());
        out.extend_from_slice(body);
        out.extend_from_slice(b"\nendobj\n");
    }
    let xref_at = out.len() as u64;
    out.extend_from_slice(format!("xref\n0 {}\n", max + 1).as_bytes());
    out.extend_from_slice(b"0000000000 65535 f \n");
    for index in 1..=max {
        out.extend_from_slice(format!("{:010} 00000 n \n", offsets[index as usize]).as_bytes());
    }
    out.extend_from_slice(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref_at}\n%%EOF\n",
            max + 1
        )
        .as_bytes(),
    );
    out
}

fn dict_get<'a>(
    pairs: &'a [(selis_bytes::Bytes, selis_pdf_cos::Obj)],
    key: &[u8],
) -> Option<&'a selis_pdf_cos::Obj> {
    pairs
        .iter()
        .find(|(name, _)| name.as_slice() == key)
        .map(|(_, value)| value)
}

/// Drive the real `fill_from_form_data` CLI path and return the output bytes.
fn fill_through_cli(fixture: &[u8], xfdf: &str) -> Vec<u8> {
    let dir = std::env::temp_dir().join(format!("selis-choice-ap-cli-{}", std::process::id()));
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(&dir).expect("create temp dir");
    let input = dir.join("in.pdf");
    let data = dir.join("in.xfdf");
    let output = dir.join("out.pdf");
    std::fs::write(&input, fixture).expect("write fixture");
    std::fs::write(&data, xfdf).expect("write XFDF");
    fill_from_form_data(
        &input.to_string_lossy(),
        &data.to_string_lossy(),
        &output.to_string_lossy(),
    )
    .expect("CLI fill succeeds");
    let bytes = std::fs::read(&output).expect("read output");
    std::fs::remove_dir_all(&dir).ok();
    bytes
}

#[test]
fn cli_fill_draws_choice_labels_in_real_appearance_streams() {
    let xfdf = r#"<xfdf xmlns="http://ns.adobe.com/xfdf/"><fields>
  <field name="Combo" value="g"/>
  <field name="List" value="b"/>
</fields></xfdf>"#;
    let bytes = fill_through_cli(&choice_fixture(), xfdf);

    // The values round-trip through the engine's existing read-back.
    let report = form_fields_from_bytes(&bytes);
    let value_of = |name: &str| {
        report
            .fields
            .iter()
            .find(|field| field.name == name)
            .unwrap_or_else(|| panic!("field {name}"))
            .value
            .as_deref()
            .unwrap_or("")
    };
    assert_eq!(value_of("Combo"), "g");
    assert_eq!(value_of("List"), "b");

    // RE-OPEN the output and read the widget appearance streams. The content
    // must draw the display label, not the export value.
    for (name, drawn) in [("Combo", "(Green Label) Tj"), ("List", "(Beta) Tj")] {
        let content = appearance_content(&bytes, name);
        assert!(
            content.contains("/Tx BMC"),
            "variable text for {name}: {content}"
        );
        assert!(
            content.contains(drawn),
            "draws {drawn} for {name}: {content}"
        );
    }

    // No viewer rebuild was requested for these fields.
    let budget = selis_sandbox::Budget::unlimited();
    let mut g = budget.guard();
    let acro = acroform_of(&bytes, &budget, &mut g);
    assert_ne!(
        dict_get(&acro, b"NeedAppearances"),
        Some(&selis_pdf_cos::Obj::Bool(true)),
        "synthesized fields do not need a viewer rebuild"
    );
}

/// `form_fields` reads a path, so write the bytes and use the public report.
fn form_fields_from_bytes(bytes: &[u8]) -> selis_pdf_form::FormFieldsReport {
    let dir = std::env::temp_dir().join(format!("selis-choice-ap-read-{}", std::process::id()));
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(&dir).expect("create temp dir");
    let path = dir.join("out.pdf");
    std::fs::write(&path, bytes).expect("write output for read-back");
    let report = form_fields(&path.to_string_lossy()).expect("read fields");
    std::fs::remove_dir_all(&dir).ok();
    report
}

/// Resolve a named field's `/AP /N` stream data from committed output bytes.
fn appearance_content(bytes: &[u8], field_name: &str) -> String {
    use selis_pdf_cos::copy::resolve_ref;

    let budget = selis_sandbox::Budget::unlimited();
    let mut g = budget.guard();
    let startxref = selis_pdf_cos::xref::find_startxref(bytes, 4096).unwrap_or(0);
    let doc =
        selis_pdf_cos::parse_revisions(bytes, startxref, &budget, &mut g).expect("output reparses");
    let acro = acroform_of(bytes, &budget, &mut g);
    let Some(selis_pdf_cos::Obj::Array(fields)) = dict_get(&acro, b"Fields") else {
        panic!("/AcroForm has no /Fields");
    };
    for field in fields {
        let selis_pdf_cos::Obj::Ref(fr) = field else {
            continue;
        };
        let selis_pdf_cos::Obj::Dict(fd) = resolve_ref(bytes, &doc, *fr, &budget, &mut g).unwrap()
        else {
            continue;
        };
        let named = dict_get(&fd, b"T")
            .and_then(|value| match value {
                selis_pdf_cos::Obj::String(s) => Some(s.as_slice()),
                _ => None,
            })
            .map(String::from_utf8_lossy)
            .map(|text| text.into_owned());
        if named.as_deref() != Some(field_name) {
            continue;
        }
        let ap = dict_get(&fd, b"AP").expect("the widget carries /AP");
        let ap = match ap {
            selis_pdf_cos::Obj::Dict(d) => d.clone(),
            selis_pdf_cos::Obj::Ref(r) => {
                match resolve_ref(bytes, &doc, *r, &budget, &mut g).unwrap() {
                    selis_pdf_cos::Obj::Dict(d) => d,
                    _ => panic!("/AP is not a dict"),
                }
            }
            _ => panic!("/AP is neither a dict nor a reference"),
        };
        let selis_pdf_cos::Obj::Ref(nr) = dict_get(&ap, b"N").expect("/AP /N") else {
            panic!("/AP /N is not a reference");
        };
        let selis_pdf_cos::Obj::Stream { dict, data } =
            resolve_ref(bytes, &doc, *nr, &budget, &mut g).unwrap()
        else {
            panic!("/AP /N does not resolve to a stream");
        };
        assert_eq!(
            dict_get(&dict, b"Subtype").and_then(|value| match value {
                selis_pdf_cos::Obj::Name(n) => Some(n.as_slice() == b"Form"),
                _ => None,
            }),
            Some(true),
            "the appearance is a Form XObject"
        );
        return String::from_utf8_lossy(data.as_slice()).into_owned();
    }
    panic!("no field named {field_name}");
}

fn acroform_of(
    bytes: &[u8],
    budget: &selis_sandbox::Budget,
    g: &mut selis_sandbox::BudgetGuard<'_>,
) -> Vec<(selis_bytes::Bytes, selis_pdf_cos::Obj)> {
    use selis_pdf_cos::copy::resolve_ref;

    let startxref = selis_pdf_cos::xref::find_startxref(bytes, 4096).unwrap_or(0);
    let doc = selis_pdf_cos::parse_revisions(bytes, startxref, budget, g).expect("parse");
    let root = doc.revisions().last().unwrap().root.unwrap();
    let selis_pdf_cos::Obj::Dict(cat) = resolve_ref(bytes, &doc, root, budget, g).unwrap() else {
        panic!("catalog is not a dict");
    };
    match dict_get(&cat, b"AcroForm")
        .expect("catalog has /AcroForm")
        .clone()
    {
        selis_pdf_cos::Obj::Dict(d) => d,
        selis_pdf_cos::Obj::Ref(r) => match resolve_ref(bytes, &doc, r, budget, g).unwrap() {
            selis_pdf_cos::Obj::Dict(d) => d,
            _ => panic!("/AcroForm is not a dict"),
        },
        _ => panic!("/AcroForm is neither a dict nor a reference"),
    }
}
