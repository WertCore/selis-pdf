#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects
)]

//! Form-data import tests (SL-5.FORM.05).

use super::formdata::{
    export_csv, export_json, import_csv, import_fdf, import_json, import_xfdf, ImportError,
    SkipReason,
};
use super::{
    fill_form, read_form_fields, FieldKind, FillFormsOptions, FormField, FormFieldsReport,
};

#[test]
fn xfdf_imports_field_values() {
    let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<xfdf xmlns="http://ns.adobe.com/xfdf/">
  <fields>
    <field name="firstName" value="Ada"/>
    <field name="agree" value="Yes"/>
  </fields>
</xfdf>"#;
    let edits = import_xfdf(xml).expect("the XFDF");
    assert_eq!(edits.len(), 2);
    assert_eq!(edits[0].name, "firstName");
    assert_eq!(edits[0].value, "Ada");
    assert_eq!(edits[1].name, "agree");
    assert_eq!(edits[1].value, "Yes");
}

#[test]
fn xfdf_decodes_entities() {
    let xml = r#"<xfdf xmlns="http://ns.adobe.com/xfdf/"><fields>
      <field name="name" value="A &amp; B &lt;C&gt; &quot;q&quot; &#65;"/>
    </fields></xfdf>"#;
    let edits = import_xfdf(xml).expect("the XFDF");
    assert_eq!(edits[0].value, "A & B <C> \"q\" A");
}

#[test]
fn xfdf_joins_hierarchical_field_names() {
    let xml = r#"<xfdf xmlns="http://ns.adobe.com/xfdf/"><fields>
      <field name="parent">
        <field name="child" value="nested"/>
      </field>
    </fields></xfdf>"#;
    let edits = import_xfdf(xml).expect("the XFDF");
    assert_eq!(edits[0].name, "parent.child");
    assert_eq!(edits[0].value, "nested");
}

#[test]
fn xfdf_rejects_non_xfdf_and_empty_imports() {
    assert_eq!(import_xfdf("<html/>"), Err(ImportError::NotXfdf));
    assert_eq!(
        import_xfdf(r#"<xfdf xmlns="http://ns.adobe.com/xfdf/"><annots/></xfdf>"#),
        Err(ImportError::Unsupported(
            "the XFDF carries no field values".to_string()
        ))
    );
}

#[test]
fn fdf_imports_field_values() {
    let fdf = b"%FDF-1.2\n1 0 obj\n<< /FDF << /Fields [\n<< /T (firstName) /V (Ada) >>\n<< /T (agree) /V /Yes >>\n] >> >>\nendobj\ntrailer\n<< /Root 1 0 R >>\n%%EOF";
    let edits = import_fdf(fdf).expect("the FDF");
    assert_eq!(edits.len(), 2);
    assert_eq!(edits[0].name, "firstName");
    assert_eq!(edits[0].value, "Ada");
    assert_eq!(edits[1].name, "agree");
    assert_eq!(edits[1].value, "Yes");
}

#[test]
fn fdf_decodes_escapes_and_rejects_bad_files() {
    let fdf = b"%FDF-1.2\n<< /FDF << /Fields [ << /T (a\\(b\\)c) /V (line\\nnext) >> ] >> >>";
    let edits = import_fdf(fdf).expect("the FDF");
    assert_eq!(edits[0].name, "a(b)c");
    assert_eq!(edits[0].value, "line\nnext");
    assert_eq!(import_fdf(b"%PDF-1.4"), Err(ImportError::NotFdf));
    assert_eq!(
        import_fdf(b"%FDF-1.2\n<< /FDF << /XFA << >> >> >>"),
        Err(ImportError::Unsupported(
            "FDF appearances and XFA packets are not imported".to_string()
        ))
    );
}

// --- JSON / CSV export + import (SL-5.FORM.05) -------------------------------

fn field(name: &str, kind: FieldKind, value: Option<&str>, values: &[&str]) -> FormField {
    FormField {
        name: name.to_string(),
        kind,
        value: value.map(str::to_string),
        values: values.iter().map(|v| (*v).to_string()).collect(),
        options: Vec::new(),
        labels: Vec::new(),
        editable: false,
        multi_select: !values.is_empty(),
        max_len: None,
        states: Vec::new(),
        read_only: false,
        required: false,
        page: None,
    }
}

fn sample_report() -> FormFieldsReport {
    FormFieldsReport {
        status: "ok".to_string(),
        pages: Some(1),
        has_signature: true,
        has_scripts: false,
        calculations: Vec::new(),
        validations: Vec::new(),
        formats: Vec::new(),
        fields: vec![
            field("firstName", FieldKind::Text, Some("Ada"), &[]),
            field("colors", FieldKind::Choice, None, &["Red", "Blue"]),
            field("agree", FieldKind::Checkbox, Some("Yes"), &[]),
            field("SignHere", FieldKind::Signature, None, &[]),
            field("Reset", FieldKind::PushButton, None, &[]),
            field("Empty", FieldKind::Text, None, &[]),
        ],
    }
}

#[test]
fn json_export_round_trips_and_notes_skipped_fields() {
    let exported = export_json(&sample_report());
    let text = String::from_utf8(exported.bytes.clone()).expect("UTF-8 JSON");
    assert!(text.contains("\"schema\": \"selis-form-data/1\""), "{text}");
    assert_eq!(
        exported.skipped,
        vec![
            super::formdata::SkippedField {
                name: "SignHere".to_string(),
                reason: SkipReason::SignatureField,
            },
            super::formdata::SkippedField {
                name: "Reset".to_string(),
                reason: SkipReason::PushButton,
            },
            super::formdata::SkippedField {
                name: "Empty".to_string(),
                reason: SkipReason::NoValue,
            },
        ]
    );
    let edits = import_json(exported.bytes.as_slice()).expect("re-import");
    assert_eq!(edits.len(), 3);
    let get = |name: &str| edits.iter().find(|edit| edit.name == name).expect(name);
    assert_eq!(get("firstName").value, "Ada");
    assert_eq!(get("colors").value, "Red");
    assert_eq!(get("colors").values, ["Blue"]);
    assert_eq!(get("agree").value, "Yes");
}

#[test]
fn csv_export_round_trips_including_quoted_cells() {
    let mut report = sample_report();
    // A value with a comma and a quote must be RFC-4180 quoted and survive.
    report.fields[0] = field("first,name", FieldKind::Text, Some("A\"da, L"), &[]);
    report.fields.truncate(3);
    let exported = export_csv(&report);
    let csv = String::from_utf8(exported.bytes.clone()).expect("UTF-8 CSV");
    assert!(csv.starts_with("\"first,name\",colors,agree\n"), "{csv}");
    assert!(csv.contains("\"A\"\"da, L\""), "{csv}");
    let edits = import_csv(exported.bytes.as_slice()).expect("re-import");
    assert_eq!(edits.len(), 3);
    assert_eq!(edits[0].name, "first,name");
    assert_eq!(edits[0].value, "A\"da, L");
    assert_eq!(edits[1].value, "Red");
    assert_eq!(edits[1].values, ["Blue"]);
    assert_eq!(edits[2].value, "Yes");
}

#[test]
fn csv_skips_a_single_value_with_a_reserved_semicolon() {
    let report = FormFieldsReport {
        fields: vec![field("odd", FieldKind::Text, Some("a;b"), &[])],
        ..sample_report()
    };
    let exported = export_csv(&report);
    assert_eq!(exported.bytes, b"\n\n");
    assert_eq!(
        exported.skipped,
        vec![super::formdata::SkippedField {
            name: "odd".to_string(),
            reason: SkipReason::BinaryValue,
        }]
    );
}

#[test]
fn json_import_rejects_bad_schema_and_non_string_values() {
    assert!(matches!(
        import_json(br#"{"schema":"other/9","fields":{}}"#),
        Err(ImportError::Unsupported(_))
    ));
    assert!(matches!(
        import_json(br#"{"fields":{"a":123}}"#),
        Err(ImportError::InvalidField(_))
    ));
    // A bare object is accepted.
    let edits = import_json(br#"{"a":"one","b":["x","y"]}"#).expect("bare object");
    assert_eq!(edits.len(), 2);
    assert_eq!(edits[1].values, ["y"]);
}

#[test]
fn csv_import_rejects_mismatched_columns() {
    assert!(matches!(
        import_csv(b"a,b\nonly-one\n"),
        Err(ImportError::InvalidField(_))
    ));
    assert!(matches!(
        import_csv(b"only-one\n"),
        Err(ImportError::InvalidField(_))
    ));
}

/// A tiny one-field form used to prove the data round trip reaches the writer.
fn one_field_pdf() -> Vec<u8> {
    let objects: &[(u32, &str)] = &[
        (1, "<< /Type /Catalog /Pages 2 0 R /AcroForm 5 0 R >>"),
        (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
        (3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << >> /Contents 4 0 R /Annots [6 0 R] >>"),
        (4, "<< /Length 0 >>\nstream\n\nendstream"),
        (5, "<< /Fields [6 0 R] /DA (/Helv 0 Tf 0 g) /DR << /Font << /Helv 7 0 R >> >> >>"),
        (6, "<< /FT /Tx /T (Name) /Subtype /Widget /Rect [10 10 200 30] /P 3 0 R >>"),
        (7, "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>"),
    ];
    let max = 7u32;
    let mut out = Vec::new();
    out.extend_from_slice(b"%PDF-1.4\n");
    let mut offsets = vec![0u64; max as usize + 1];
    for (num, body) in objects {
        offsets[*num as usize] = out.len() as u64;
        out.extend_from_slice(format!("{num} 0 obj\n").as_bytes());
        out.extend_from_slice(body.as_bytes());
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

#[test]
fn json_export_import_fill_round_trips_to_the_pdf() {
    let pdf = one_field_pdf();
    let budget = super::Budget::unlimited();
    // 1. Fill once with a name, then read it back as the report to export.
    let filled = fill_form(
        &pdf,
        &FillFormsOptions {
            fields: vec![super::FieldEdit {
                name: "Name".to_string(),
                value: "Grace".to_string(),
                values: Vec::new(),
            }],
            synthesize_appearances: true,
        },
        &budget,
        &mut budget.guard(),
    )
    .expect("first fill")
    .0;
    let report = read_form_fields(&filled, &budget, &mut budget.guard()).expect("read fields");
    // 2. Export JSON and re-import to the FieldEdit list.
    let exported = export_json(&report);
    let edits = import_json(&exported.bytes).expect("re-import");
    // 3. Feed those edits into the same writer the FDF/XFDF importer uses.
    let (out, info) = fill_form(
        &pdf,
        &FillFormsOptions {
            fields: edits,
            synthesize_appearances: true,
        },
        &budget,
        &mut budget.guard(),
    )
    .expect("round-trip fill");
    assert_eq!(info.filled, 1);
    let final_report = read_form_fields(&out, &budget, &mut budget.guard()).expect("final read");
    assert_eq!(final_report.fields[0].value.as_deref(), Some("Grace"));
}
