//! SL-5.FORM.08 — XFA: render static, refuse dynamic honestly.
//!
//! An XFA form stores its definition in an `/XFA` packet (an XDP/XML document,
//! sometimes split across several streams) rather than only in AcroForm
//! dictionaries. A *static* XFA form still ships a readable `<template>`; a
//! *dynamic* one needs a full XFA layout engine and scripting runtime to
//! produce a page.
//!
//! This crate does not fake a layout. What it can do honestly is read the
//! packet and expose the **static template text** — the field names and the
//! captions in the template — as a plain-text representation, clearly labelled
//! as such. When the packet declares a dynamic form (`<dynamic>`, `<event>` or
//! `<script>`), it returns a typed [`FormError::XfaDynamic`] with a precise
//! reason instead of a blank "upgrade your reader" page. A packet with no
//! readable static template is [`FormError::XfaUnsupported`], also with a
//! reason.

use super::{is_encrypted, open_doc, resolve_deep, FormError};
use selis_bytes::Bytes;
use selis_pdf_cos::reconcile::catalog_entry;
use selis_pdf_cos::Obj;
use selis_pdf_filter::decode_stream;
use selis_sandbox::{Budget, BudgetGuard};
use serde::{Deserialize, Serialize};

/// An honest static representation of an XFA form.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct XfaRenderReport {
    /// `static-text` when the packet's static template was rendered.
    pub status: String,
    /// Always `false` for a rendered report: a dynamic packet is refused.
    pub dynamic: bool,
    /// How many stream objects made up the `/XFA` packet.
    pub packets: usize,
    /// Plain-text lines extracted from the static XFA `<template>`.
    pub lines: Vec<String>,
    /// An explicit disclosure of what this representation is (and is not).
    pub note: String,
}

impl XfaRenderReport {
    /// The report as pretty JSON.
    ///
    /// # Errors
    ///
    /// [`FormError::Malformed`] when the report cannot be encoded (never for a
    /// report this crate built).
    pub fn to_json(&self) -> Result<Vec<u8>, FormError> {
        serde_json::to_vec_pretty(self)
            .map_err(|e| FormError::Malformed(format!("cannot encode the XFA report: {e}")))
    }
}

/// Maximum decoded bytes for the whole XFA packet.
const XFA_LIMIT: u64 = 16 * 1024 * 1024;
/// Maximum number of extracted lines. A pathological template is truncated
/// with a trailing note rather than flooding the caller.
const MAX_LINES: usize = 2_000;

/// Render the static part of an XFA form as an honest plain-text report.
///
/// This reads the `/XFA` packet, decodes its streams, and extracts the text and
/// field names from the `<template>`. It does **not** lay out the form or run
/// scripts. A dynamic packet is refused with [`FormError::XfaDynamic`]; a
/// packet with no static template is refused with
/// [`FormError::XfaUnsupported`].
///
/// # Budget
///
/// One document parse plus decoding of the packet streams, charged to
/// `budget`; decoded bytes are capped at [`XFA_LIMIT`] and extracted lines at
/// [`MAX_LINES`].
///
/// # Malformed Input
///
/// An encrypted document is refused. A packet whose streams cannot be decoded,
/// or whose XML has no readable `<template>`, is a typed [`FormError`] with a
/// precise reason rather than a blank page.
///
/// # Errors
///
/// [`FormError::Encrypted`] for an encrypted document,
/// [`FormError::XfaUnsupported`] when there is no usable `/XFA` packet,
/// [`FormError::XfaDynamic`] for an interactive/dynamic form, and
/// [`FormError::Malformed`]/[`FormError::Unresolvable`] for unreadable bytes.
pub fn render_xfa(
    src: &[u8],
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
) -> Result<XfaRenderReport, FormError> {
    let doc = open_doc(src, budget, g)?;
    if is_encrypted(&doc) {
        return Err(FormError::Encrypted);
    }
    let acro = catalog_entry(src, &doc, b"AcroForm", budget, g)
        .map_err(|e| FormError::Unresolvable(format!("cannot read /AcroForm: {e}")))?
        .ok_or_else(|| {
            FormError::XfaUnsupported(
                "the document has no /AcroForm, so it has no /XFA packet".to_string(),
            )
        })?;
    let acro = resolve_deep(src, &doc, acro, 0, budget, g);
    let Obj::Dict(pairs) = &acro else {
        return Err(FormError::XfaUnsupported(
            "the document's /AcroForm is not a dictionary".to_string(),
        ));
    };
    let Some(xfa) = pairs
        .iter()
        .find(|(key, _)| key.as_slice() == b"XFA")
        .map(|(_, v)| v.clone())
    else {
        return Err(FormError::XfaUnsupported(
            "the document's /AcroForm carries no /XFA packet to render".to_string(),
        ));
    };
    let xfa = resolve_deep(src, &doc, xfa, 0, budget, g);

    let streams = match xfa {
        Obj::Stream { .. } => vec![xfa],
        Obj::Array(items) => items,
        _ => {
            return Err(FormError::XfaUnsupported(
                "the /XFA value is neither a stream nor an array of streams".to_string(),
            ))
        }
    };
    if streams.is_empty() {
        return Err(FormError::XfaUnsupported(
            "the /XFA array is empty; there is no packet to render".to_string(),
        ));
    }

    let mut xml = Vec::new();
    let mut packets = 0usize;
    for item in streams {
        let stream = resolve_deep(src, &doc, item, 0, budget, g);
        let Obj::Stream { dict, data } = stream else {
            continue;
        };
        packets = packets.saturating_add(1);
        let filter = dict
            .iter()
            .find(|(key, _)| key.as_slice() == b"Filter")
            .and_then(|(_, v)| match v {
                Obj::Name(name) => Some(name.as_slice().to_vec()),
                Obj::Array(items) => items.first().and_then(|item| match item {
                    Obj::Name(name) => Some(name.as_slice().to_vec()),
                    _ => None,
                }),
                _ => None,
            });
        let decoded = match filter {
            None => data.as_slice().to_vec(),
            Some(filter) => decode_stream(
                &Bytes::copy_from_slice(&filter),
                None,
                data.as_slice(),
                XFA_LIMIT,
                g,
            )
            .map_err(|e| {
                FormError::XfaUnsupported(format!(
                    "an /XFA stream could not be decoded ({}): {e}",
                    String::from_utf8_lossy(&filter)
                ))
            })?,
        };
        let remaining = XFA_LIMIT.saturating_sub(u64::try_from(xml.len()).unwrap_or(XFA_LIMIT));
        let take = usize::try_from(remaining)
            .unwrap_or(usize::MAX)
            .min(decoded.len());
        if let Some(slice) = decoded.get(..take) {
            xml.extend_from_slice(slice);
        }
        g.charge(
            selis_sandbox::Resource::Bytes,
            u64::try_from(decoded.len()).unwrap_or(u64::MAX),
        )
        .map_err(|e| FormError::Limit(format!("XFA decode budget exhausted: {e}")))?;
        if u64::try_from(xml.len()).unwrap_or(XFA_LIMIT) >= XFA_LIMIT {
            break;
        }
    }
    if packets == 0 {
        return Err(FormError::XfaUnsupported(
            "none of the /XFA entries resolve to a readable stream".to_string(),
        ));
    }

    let text = String::from_utf8_lossy(&xml);
    if contains_ci(&text, "<dynamic")
        || contains_ci(&text, "<event")
        || contains_ci(&text, "<script")
    {
        return Err(FormError::XfaDynamic(
            "this XFA form is dynamic (it declares <dynamic>, <event> or <script>); \
             a faithful render needs a full XFA layout and scripting engine, which this crate does not implement (ADR-P0021). \
             Refusing honestly rather than showing an empty placeholder page."
                .to_string(),
        ));
    }
    let template = template_body(&text).ok_or_else(|| {
        FormError::XfaUnsupported(
            "the /XFA packet has no readable <template> element; a static text render is not possible"
                .to_string(),
        )
    })?;
    let mut lines = template_lines(template);
    if lines.is_empty() {
        return Err(FormError::XfaUnsupported(
            "the XFA <template> carries no readable text or field names".to_string(),
        ));
    }
    let truncated = lines.len() > MAX_LINES;
    lines.truncate(MAX_LINES);
    if truncated {
        lines.push(format!("… truncated at {MAX_LINES} lines"));
    }

    Ok(XfaRenderReport {
        status: "static-text".to_string(),
        dynamic: false,
        packets,
        lines,
        note: "Plain text extracted from the XFA <template> (captions and field names). \
               This is an honest static representation, not a pixel-accurate XFA layout render; \
               dynamic XFA and scripts are not supported."
            .to_string(),
    })
}

/// Case-insensitive substring search.
fn contains_ci(haystack: &str, needle: &str) -> bool {
    let haystack = haystack.to_ascii_lowercase();
    haystack.contains(&needle.to_ascii_lowercase())
}

/// The substring between `<template …>` and `</template>` (case-insensitive).
fn template_body(xml: &str) -> Option<&str> {
    let lower = xml.to_ascii_lowercase();
    let start = lower.find("<template")?;
    let open_end = lower.get(start..)?.find('>')?;
    let body_start = start.saturating_add(open_end).saturating_add(1);
    let close = lower.get(body_start..)?.find("</template")?;
    let body_end = body_start.saturating_add(close);
    xml.get(body_start..body_end)
}

/// Extract readable lines from a template: XML character data, plus explicit
/// `name="…"` attributes of `<field>` elements so the field structure is not
/// lost when the template has little prose.
fn template_lines(template: &str) -> Vec<String> {
    let mut lines = Vec::new();
    let mut rest = template;
    while let Some(at) = rest.find('<') {
        let chunk = rest.get(..at).unwrap_or("");
        push_line(&mut lines, decode_entities(chunk));
        let Some(after) = rest.get(at..) else { break };
        let Some(end) = after.find('>') else { break };
        let tag = after.get(..end.saturating_add(1)).unwrap_or("");
        if contains_ci(tag, "<field") {
            if let Some(name) = attribute_ci(tag, "name") {
                push_line(&mut lines, format!("field: {name}"));
            }
        }
        rest = after.get(end.saturating_add(1)..).unwrap_or("");
    }
    push_line(&mut lines, decode_entities(rest));
    lines
}

/// Append a line if it is non-empty and short enough to be useful.
fn push_line(lines: &mut Vec<String>, text: String) {
    let trimmed = text.trim();
    if !trimmed.is_empty() && trimmed.len() <= 4_096 && lines.len() < MAX_LINES.saturating_add(1) {
        lines.push(trimmed.to_string());
    }
}

/// Read one attribute from a tag, case-insensitively on the attribute name.
fn attribute_ci(tag: &str, wanted: &str) -> Option<String> {
    let lower = tag.to_ascii_lowercase();
    let needle = format!("{wanted}=");
    let at = lower.find(&needle)?;
    let after = tag.get(at.saturating_add(needle.len())..)?;
    let after = after.trim_start();
    let quote = after.chars().next()?;
    if quote != '"' && quote != '\'' {
        return None;
    }
    let rest = after.get(quote.len_utf8()..)?;
    let end = rest.find(quote)?;
    Some(decode_entities(rest.get(..end)?))
}

/// Decode the XML entities that matter for a text render.
fn decode_entities(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut rest = value;
    while let Some(at) = rest.find('&') {
        out.push_str(rest.get(..at).unwrap_or(""));
        let after = rest.get(at.saturating_add(1)..).unwrap_or("");
        let Some(end) = after.find(';') else {
            out.push('&');
            rest = after;
            continue;
        };
        let entity = after.get(..end).unwrap_or("");
        match entity {
            "amp" => out.push('&'),
            "lt" => out.push('<'),
            "gt" => out.push('>'),
            "quot" => out.push('"'),
            "apos" => out.push('\''),
            _ if entity.starts_with("#x") || entity.starts_with("#X") => {
                if let Ok(code) = u32::from_str_radix(entity.get(2..).unwrap_or(""), 16) {
                    if let Some(ch) = char::from_u32(code) {
                        out.push(ch);
                    }
                }
            }
            _ if entity.starts_with('#') => {
                if let Ok(code) = entity.get(1..).unwrap_or("").parse::<u32>() {
                    if let Some(ch) = char::from_u32(code) {
                        out.push(ch);
                    }
                }
            }
            _ => {
                out.push('&');
                out.push_str(entity);
                out.push(';');
            }
        }
        rest = after.get(end.saturating_add(1)..).unwrap_or("");
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing,
        clippy::arithmetic_side_effects,
        clippy::cast_possible_truncation,
        clippy::type_complexity
    )]

    use super::*;
    use crate::FormError as FE;

    /// A one-page document whose `/AcroForm` carries a single XFA stream.
    fn xfa_pdf(xml: &[u8]) -> Vec<u8> {
        let objects: &[(u32, Vec<u8>)] = &[
            (
                1,
                b"<< /Type /Catalog /Pages 2 0 R /AcroForm 5 0 R >>".to_vec(),
            ),
            (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec()),
            (
                3,
                b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R >>".to_vec(),
            ),
            (4, b"<< /Length 0 >>\nstream\n\nendstream".to_vec()),
            (5, b"<< /Fields [] /XFA 6 0 R >>".to_vec()),
            (
                6,
                [
                    format!("<< /Length {} >>\nstream\n", xml.len()).into_bytes(),
                    xml.to_vec(),
                    b"\nendstream".to_vec(),
                ]
                .concat(),
            ),
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

    fn render(fixture: &[u8]) -> Result<XfaRenderReport, FE> {
        let budget = Budget::unlimited();
        let mut g = budget.guard();
        render_xfa(fixture, &budget, &mut g)
    }

    #[test]
    fn renders_a_static_template_as_text() {
        let xml = br#"<?xml version="1.0"?>
<xdp xmlns="http://www.xfa.org/schema/xdp/1.0/">
  <template>
    <subform name="form">
      <field name="FullName"><ui><textEdit/></ui></field>
      <field name="Agree"><ui><checkButton/></ui></field>
      <draw>Application &amp; declaration</draw>
    </subform>
  </template>
</xdp>"#;
        let report = render(&xfa_pdf(xml)).expect("static render");
        assert_eq!(report.status, "static-text");
        assert!(!report.dynamic);
        assert_eq!(report.packets, 1);
        assert!(
            report.lines.iter().any(|line| line == "field: FullName"),
            "lines: {:?}",
            report.lines
        );
        assert!(report.lines.iter().any(|line| line == "field: Agree"));
        assert!(
            report
                .lines
                .iter()
                .any(|line| line == "Application & declaration"),
            "entities decoded: {:?}",
            report.lines
        );
        assert!(report.note.contains("not a pixel-accurate"), "honest note");
    }

    #[test]
    fn refuses_dynamic_xfa_with_a_precise_reason() {
        let dynamic = b"<xdp><dynamic><template><field name=\"A\"/></template></dynamic></xdp>";
        let error = render(&xfa_pdf(dynamic)).expect_err("dynamic refused");
        match &error {
            FE::XfaDynamic(reason) => {
                assert!(reason.contains("dynamic"), "reason: {reason}");
                assert!(reason.contains("layout"), "reason: {reason}");
            }
            other => panic!("wrong error: {other:?}"),
        }
        let scripted = b"<xdp><template><event><script>x=1</script></event></template></xdp>";
        assert!(matches!(render(&xfa_pdf(scripted)), Err(FE::XfaDynamic(_))));
    }

    #[test]
    fn refuses_packets_without_a_static_template() {
        // The legacy `xfa_fixture` packet is just `<x/>`.
        let error = render(&xfa_pdf(b"<x/>")).expect_err("no template");
        match &error {
            FE::XfaUnsupported(reason) => assert!(reason.contains("<template"), "reason: {reason}"),
            other => panic!("wrong error: {other:?}"),
        }
        // No XFA at all.
        let no_form = {
            let objects: &[(u32, &[u8])] = &[
                (1, b"<< /Type /Catalog /Pages 2 0 R >>"),
                (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
                (
                    3,
                    b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>",
                ),
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
                out.extend_from_slice(
                    format!("{:010} 00000 n \n", offsets[index as usize]).as_bytes(),
                );
            }
            out.extend_from_slice(
                format!(
                    "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref_at}\n%%EOF\n",
                    max + 1
                )
                .as_bytes(),
            );
            out
        };
        assert!(matches!(render(&no_form), Err(FE::XfaUnsupported(_))));
    }
}
