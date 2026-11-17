#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects
)]

//! Form-data import tests (SL-5.FORM.05).

use super::formdata::{import_fdf, import_xfdf, ImportError};

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
