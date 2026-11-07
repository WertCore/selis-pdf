//! Privacy risk scanner: a read-only report of the things in a PDF that leak
//! more than the page shows. Nothing here rewrites the input and nothing is
//! auto-fixed — the report names each risk so a reader can decide.
//!
//! The scan is structural, over the document's own objects: saved revisions
//! (earlier content may be recoverable), authoring metadata and XMP trails,
//! optional-content layers (especially ones hidden by default), embedded files,
//! JavaScript (document actions and the name-tree scripts), and actions that
//! reach outside the document (links, remote go-to, launch, form submit). One
//! finding is a heuristic and says so: a page carrying both a large image and a
//! text layer may be a scan whose OCR text sits under the image.
//!
//! An encrypted document cannot be scanned without the password, so it reports
//! exactly that and nothing invented on top of ciphertext.

use serde::Serialize;

use selis_bytes::Bytes;
use selis_pdf_cos::reconcile as rec;
use selis_pdf_cos::verify::survey;
use selis_pdf_cos::Obj;
use selis_sandbox::{Budget, BudgetGuard};

use crate::fingerprint::{find_int, find_name, resolve_inline};
use crate::{read_file, CliError, ToolResult};

/// The privacy report of one document.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PrivacyReport {
    /// The versioned schema tag (`selis-privacy/1`).
    pub schema: String,
    /// `ok` when the document was scanned, `encrypted` when it could not be.
    pub status: String,
    /// Input size in bytes.
    pub bytes: u64,
    /// Leaf page count (`None` for an encrypted document).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pages: Option<u64>,
    /// The highest severity among the findings: `none`, `info`, `low`,
    /// `medium`, or `high`.
    pub risk: String,
    /// Every risk found, in a stable order.
    pub findings: Vec<PrivacyFinding>,
}

/// One risk the scan found.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PrivacyFinding {
    /// A stable machine key (`javascript`, `external_references`, ...).
    pub category: String,
    /// `info`, `low`, `medium`, or `high`.
    pub severity: String,
    /// How many of this thing were found.
    pub count: u64,
    /// A human sentence a reader can act on.
    pub detail: String,
}

/// A page's image is "large" — a photo or a scan rather than an icon — above
/// this many samples, which is what the hidden-text heuristic looks for.
const LARGE_IMAGE_SAMPLES: u64 = 10_000;

/// The actions that reach outside the document.
const EXTERNAL_ACTIONS: [&str; 5] = ["URI", "Launch", "GoToR", "SubmitForm", "ImportData"];

/// Scan the document for privacy risks without touching its bytes.
///
/// # Errors
///
/// A typed [`CliError`] when the file cannot be read or parsed.
pub fn privacy_report(path: &str) -> ToolResult<PrivacyReport> {
    let src = read_file(path)?;
    let budget = Budget::unlimited();
    let clock = crate::shell_clock();
    let mut g = crate::runtime::cli_guard(&budget, &clock);
    let startxref = selis_pdf_cos::xref::find_startxref(&src, 4096).unwrap_or(0);
    let doc = selis_pdf_cos::parse_revisions(&src, startxref, &budget, &mut g)
        .map_err(|e| CliError(format!("{path}: cannot open: {e}")))?;
    let newest = doc
        .revisions()
        .last()
        .ok_or_else(|| CliError(format!("{path}: no revisions")))?;
    let bytes = u64::try_from(src.len()).unwrap_or(u64::MAX);

    if newest.encrypt.is_some() {
        // Ciphertext cannot be scanned; the honest answer is that, not a guess.
        return Ok(PrivacyReport {
            schema: "selis-privacy/1".to_string(),
            status: "encrypted".to_string(),
            bytes,
            pages: None,
            risk: "none".to_string(),
            findings: vec![finding(
                "encrypted",
                "info",
                1,
                "The document is encrypted; its contents cannot be scanned without the password."
                    .to_string(),
            )],
        });
    }

    let revisions = u64::try_from(doc.revisions().len()).unwrap_or(u64::MAX);
    let observed = survey(&src, &budget, &mut g)
        .map_err(|e| CliError(format!("{path}: cannot survey: {e}")))?;
    let info = crate::metadata::info_fields(&src, &doc, &budget, &mut g)?;

    let mut findings = Vec::new();

    if revisions > 1 {
        findings.push(finding(
            "incremental_revisions",
            "medium",
            revisions,
            format!(
                "The file has {revisions} saved revisions; content from earlier versions may still be recoverable from the bytes."
            ),
        ));
    }

    let mut trails: Vec<&str> = Vec::new();
    for key in ["Author", "Creator", "Producer"] {
        if info
            .iter()
            .any(|field| field.key == key && !field.value.is_empty())
        {
            trails.push(key);
        }
    }
    if catalog_has(&src, &doc, b"Metadata", &budget, &mut g) {
        trails.push("XMP");
    }
    if !trails.is_empty() {
        findings.push(finding(
            "metadata_trails",
            "low",
            u64::try_from(trails.len()).unwrap_or(u64::MAX),
            format!("Authoring metadata is present: {}.", trails.join(", ")),
        ));
    }

    let (ocgs, hidden) = oc_counts(&src, &doc, &budget, &mut g);
    if ocgs > 0 {
        let severity = if hidden > 0 { "medium" } else { "low" };
        let detail = if hidden > 0 {
            format!(
                "{ocgs} optional-content layer(s), {hidden} hidden by default; a hidden layer can carry content a reader never sees."
            )
        } else {
            format!("{ocgs} optional-content layer(s); check what each one holds.")
        };
        findings.push(finding("optional_content", severity, ocgs, detail));
    }

    if observed.embedded_files > 0 {
        findings.push(finding(
            "embedded_files",
            "medium",
            observed.embedded_files,
            format!(
                "{} embedded file(s); these travel with the PDF and may hold data shown on no page.",
                observed.embedded_files
            ),
        ));
    }

    let js = javascript_sources(&src, &doc, &budget, &mut g);
    if js > 0 {
        findings.push(finding(
            "javascript",
            "high",
            js,
            "The document carries JavaScript (an open action, document actions, or name-tree scripts); active content can run when the file is opened."
                .to_string(),
        ));
    }

    let ext = external_reference_count(&src, &doc, &budget, &mut g);
    if ext > 0 {
        findings.push(finding(
            "external_references",
            "medium",
            ext,
            format!(
                "{ext} action(s) reach outside the document — a link, a remote go-to, a launch, or a form submit."
            ),
        ));
    }

    let scanned = text_over_image_pages(&src, &doc, &budget, &mut g);
    if scanned > 0 {
        findings.push(finding(
            "possible_hidden_text",
            "info",
            scanned,
            format!(
                "{scanned} page(s) carry both a large image and a text layer; in a scanned document the text may sit under the image (heuristic, not a certainty)."
            ),
        ));
    }

    let risk = overall_risk(&findings);
    Ok(PrivacyReport {
        schema: "selis-privacy/1".to_string(),
        status: "ok".to_string(),
        bytes,
        pages: Some(observed.pages),
        risk,
        findings,
    })
}

/// One finding, with owned strings.
fn finding(category: &str, severity: &str, count: u64, detail: String) -> PrivacyFinding {
    PrivacyFinding {
        category: category.to_string(),
        severity: severity.to_string(),
        count,
        detail,
    }
}

/// Whether the catalog carries a given key at all.
fn catalog_has(
    src: &[u8],
    doc: &selis_pdf_cos::Doc,
    key: &[u8],
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
) -> bool {
    matches!(rec::catalog_entry(src, doc, key, budget, g), Ok(Some(value)) if !matches!(value, Obj::Null))
}

/// `(total optional-content groups, groups hidden by the default config)`.
fn oc_counts(
    src: &[u8],
    doc: &selis_pdf_cos::Doc,
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
) -> (u64, u64) {
    let Ok(Some(raw)) = rec::catalog_entry(src, doc, b"OCProperties", budget, g) else {
        return (0, 0);
    };
    let Obj::Dict(props) = resolve_inline(src, doc, raw, budget, g) else {
        return (0, 0);
    };
    let ocgs = props
        .iter()
        .find(|(k, _)| k.as_slice() == b"OCGs")
        .map(|(_, v)| resolved_array_len(src, doc, v.clone(), budget, g))
        .unwrap_or(0);
    let hidden = match props.iter().find(|(k, _)| k.as_slice() == b"D") {
        Some((_, d)) => match resolve_inline(src, doc, d.clone(), budget, g) {
            Obj::Dict(default) => default
                .iter()
                .find(|(k, _)| k.as_slice() == b"OFF")
                .map(|(_, off)| resolved_array_len(src, doc, off.clone(), budget, g))
                .unwrap_or(0),
            _ => 0,
        },
        None => 0,
    };
    (ocgs, hidden)
}

/// The number of distinct JavaScript sources: an open action, the name-tree
/// scripts, catalog additional actions, and an XFA form (which carries scripts).
fn javascript_sources(
    src: &[u8],
    doc: &selis_pdf_cos::Doc,
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
) -> u64 {
    let mut count: u64 = 0;
    if let Ok(Some(raw)) = rec::catalog_entry(src, doc, b"OpenAction", budget, g) {
        if let Obj::Dict(action) = resolve_inline(src, doc, raw, budget, g) {
            if find_name(&action, b"S").as_deref() == Some("JavaScript") {
                count = count.saturating_add(1);
            }
        }
    }
    if let Ok(Some(raw)) = rec::catalog_entry(src, doc, b"Names", budget, g) {
        if let Obj::Dict(names) = resolve_inline(src, doc, raw, budget, g) {
            if let Some((_, js)) = names.iter().find(|(k, _)| k.as_slice() == b"JavaScript") {
                if js_name_tree_nonempty(src, doc, js.clone(), budget, g) {
                    count = count.saturating_add(1);
                }
            }
        }
    }
    if catalog_has(src, doc, b"AA", budget, g) {
        count = count.saturating_add(1);
    }
    if let Ok(Some(raw)) = rec::catalog_entry(src, doc, b"AcroForm", budget, g) {
        if let Obj::Dict(form) = resolve_inline(src, doc, raw, budget, g) {
            if form.iter().any(|(k, _)| k.as_slice() == b"XFA") {
                count = count.saturating_add(1);
            }
        }
    }
    count
}

/// Whether a `/JavaScript` name tree actually holds any scripts.
fn js_name_tree_nonempty(
    src: &[u8],
    doc: &selis_pdf_cos::Doc,
    obj: Obj,
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
) -> bool {
    let Obj::Dict(tree) = resolve_inline(src, doc, obj, budget, g) else {
        return false;
    };
    for key in [b"Names".as_slice(), b"Kids".as_slice()] {
        if let Some((_, value)) = tree.iter().find(|(k, _)| k.as_slice() == key) {
            if resolved_array_len(src, doc, value.clone(), budget, g) > 0 {
                return true;
            }
        }
    }
    false
}

/// The number of actions that reach outside the document: the catalog open
/// action plus every annotation action across the pages.
fn external_reference_count(
    src: &[u8],
    doc: &selis_pdf_cos::Doc,
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
) -> u64 {
    let mut count: u64 = 0;
    if let Ok(Some(raw)) = rec::catalog_entry(src, doc, b"OpenAction", budget, g) {
        if let Obj::Dict(action) = resolve_inline(src, doc, raw, budget, g) {
            if is_external_action(&action) {
                count = count.saturating_add(1);
            }
        }
    }
    let Ok(pages) = crate::tools::page_refs_of(src, doc, budget, g) else {
        return count;
    };
    for page in pages {
        let Ok((_, _, _, _, annots)) = crate::tools::page_info(src, doc, page, budget, g) else {
            continue;
        };
        let Some(annots_obj) = annots else {
            continue;
        };
        let Obj::Array(items) = resolve_inline(src, doc, annots_obj, budget, g) else {
            continue;
        };
        for item in items {
            let Obj::Dict(annot) = resolve_inline(src, doc, item, budget, g) else {
                continue;
            };
            if let Some((_, a)) = annot.iter().find(|(k, _)| k.as_slice() == b"A") {
                if let Obj::Dict(action) = resolve_inline(src, doc, a.clone(), budget, g) {
                    if is_external_action(&action) {
                        count = count.saturating_add(1);
                    }
                }
            }
        }
    }
    count
}

/// Whether an action's `/S` reaches outside the document.
fn is_external_action(action: &[(Bytes, Obj)]) -> bool {
    match find_name(action, b"S") {
        Some(kind) => EXTERNAL_ACTIONS.contains(&kind.as_str()),
        None => false,
    }
}

/// The number of pages that carry both a text layer and a large image — the
/// shape of a scanned page whose OCR text sits under the picture.
fn text_over_image_pages(
    src: &[u8],
    doc: &selis_pdf_cos::Doc,
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
) -> u64 {
    let Ok(pages) = crate::tools::page_refs_of(src, doc, budget, g) else {
        return 0;
    };
    let mut count: u64 = 0;
    for page in pages {
        let Ok((_, _, resources, _, _)) = crate::tools::page_info(src, doc, page, budget, g) else {
            continue;
        };
        let Some(resource_obj) = resources else {
            continue;
        };
        let Obj::Dict(res) = resolve_inline(src, doc, resource_obj, budget, g) else {
            continue;
        };
        let has_text = res.iter().any(|(k, _)| k.as_slice() == b"Font");
        if has_text && page_has_large_image(src, doc, &res, budget, g) {
            count = count.saturating_add(1);
        }
    }
    count
}

/// Whether a page's resources include an image above [`LARGE_IMAGE_SAMPLES`].
fn page_has_large_image(
    src: &[u8],
    doc: &selis_pdf_cos::Doc,
    res: &[(Bytes, Obj)],
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
) -> bool {
    let Some((_, value)) = res.iter().find(|(k, _)| k.as_slice() == b"XObject") else {
        return false;
    };
    let Obj::Dict(xobjects) = resolve_inline(src, doc, value.clone(), budget, g) else {
        return false;
    };
    for (_, xobject) in xobjects {
        if let Obj::Stream { dict, .. } = resolve_inline(src, doc, xobject, budget, g) {
            let is_image = dict.iter().any(|(k, v)| {
                k.as_slice() == b"Subtype" && matches!(v, Obj::Name(n) if n.as_slice() == b"Image")
            });
            if is_image {
                let width = find_int(&dict, b"Width").unwrap_or(0);
                let height = find_int(&dict, b"Height").unwrap_or(0);
                if width.saturating_mul(height) >= LARGE_IMAGE_SAMPLES {
                    return true;
                }
            }
        }
    }
    false
}

/// The array length of a (possibly indirect) value, else 0.
fn resolved_array_len(
    src: &[u8],
    doc: &selis_pdf_cos::Doc,
    value: Obj,
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
) -> u64 {
    match resolve_inline(src, doc, value, budget, g) {
        Obj::Array(items) => u64::try_from(items.len()).unwrap_or(u64::MAX),
        _ => 0,
    }
}

/// The highest severity among the findings, as the report's `risk`.
fn overall_risk(findings: &[PrivacyFinding]) -> String {
    let max = findings
        .iter()
        .map(|finding| severity_rank(&finding.severity))
        .max()
        .unwrap_or(0);
    match max {
        4 => "high",
        3 => "medium",
        2 => "low",
        1 => "info",
        _ => "none",
    }
    .to_string()
}

/// A severity's numeric rank for comparison.
fn severity_rank(severity: &str) -> u8 {
    match severity {
        "high" => 4,
        "medium" => 3,
        "low" => 2,
        "info" => 1,
        _ => 0,
    }
}

impl PrivacyReport {
    /// The report as indented JSON: the one export shape, owned here so a reader
    /// sees the same fields whether a CLI or the browser asked.
    ///
    /// # Errors
    ///
    /// [`CliError`] when serialization fails.
    pub fn to_json(&self) -> ToolResult<Vec<u8>> {
        serde_json::to_vec_pretty(self)
            .map_err(|e| CliError(format!("cannot encode the privacy report: {e}")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{insert_input, MemorySession};

    fn stream_object(number: usize, dict: &str, data: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(format!("{number} 0 obj\n{dict}\nstream\n").as_bytes());
        out.extend_from_slice(data);
        out.extend_from_slice(b"\nendstream\nendobj\n");
        out
    }

    fn build(objects: Vec<Vec<u8>>, trailer_extra: &str) -> Vec<u8> {
        let mut out = b"%PDF-1.7\n".to_vec();
        let mut offsets = Vec::with_capacity(objects.len());
        for object in objects {
            offsets.push(out.len());
            out.extend_from_slice(&object);
        }
        let xref = out.len();
        let size = offsets.len().saturating_add(1);
        out.extend_from_slice(format!("xref\n0 {size}\n").as_bytes());
        out.extend_from_slice(b"0000000000 65535 f \n");
        for offset in offsets {
            out.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
        }
        out.extend_from_slice(
            format!(
                "trailer\n<< /Size {size} /Root 1 0 R {trailer_extra} >>\nstartxref\n{xref}\n%%EOF\n"
            )
            .as_bytes(),
        );
        out
    }

    fn page(media_resources: &str) -> Vec<u8> {
        format!("3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] {media_resources} >>\nendobj\n").into_bytes()
    }

    fn pages_and_page(media_resources: &str) -> Vec<Vec<u8>> {
        vec![
            b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n".to_vec(),
            page(media_resources),
        ]
    }

    fn find<'a>(report: &'a PrivacyReport, category: &str) -> Option<&'a PrivacyFinding> {
        report.findings.iter().find(|f| f.category == category)
    }

    #[test]
    fn detects_document_javascript() {
        let _session = MemorySession::activate();
        let mut objects = vec![
            b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R /OpenAction 4 0 R >>\nendobj\n".to_vec(),
        ];
        objects.extend(pages_and_page(""));
        objects.push(b"4 0 obj\n<< /S /JavaScript /JS (app.alert\\(1\\);) >>\nendobj\n".to_vec());
        insert_input("privacy/js/in.pdf", build(objects, ""));

        let report = privacy_report("privacy/js/in.pdf").expect("report");
        assert_eq!(report.status, "ok");
        let js = find(&report, "javascript").expect("javascript finding");
        assert_eq!(js.severity, "high");
        assert_eq!(report.risk, "high");
    }

    #[test]
    fn detects_external_references() {
        let _session = MemorySession::activate();
        let mut objects = vec![b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n".to_vec()];
        objects.push(b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n".to_vec());
        objects.push(b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Annots [4 0 R] >>\nendobj\n".to_vec());
        objects.push(b"4 0 obj\n<< /Type /Annot /Subtype /Link /Rect [0 0 10 10] /A << /S /URI /URI (http://example.com) >> >>\nendobj\n".to_vec());
        insert_input("privacy/uri/in.pdf", build(objects, ""));

        let report = privacy_report("privacy/uri/in.pdf").expect("report");
        let ext = find(&report, "external_references").expect("external finding");
        assert_eq!(ext.severity, "medium");
        assert_eq!(ext.count, 1);
    }

    #[test]
    fn detects_hidden_layers() {
        let _session = MemorySession::activate();
        let mut objects = vec![
            b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R /OCProperties 4 0 R >>\nendobj\n".to_vec(),
        ];
        objects.extend(pages_and_page(""));
        objects.push(b"4 0 obj\n<< /OCGs [5 0 R] /D << /OFF [5 0 R] >> >>\nendobj\n".to_vec());
        objects.push(b"5 0 obj\n<< /Type /OCG /Name (Secret) >>\nendobj\n".to_vec());
        insert_input("privacy/oc/in.pdf", build(objects, ""));

        let report = privacy_report("privacy/oc/in.pdf").expect("report");
        let oc = find(&report, "optional_content").expect("optional-content finding");
        assert_eq!(oc.severity, "medium", "a hidden layer is medium, not low");
        assert_eq!(oc.count, 1);
        assert!(
            oc.detail.contains("hidden"),
            "the detail names the hidden layer"
        );
    }

    #[test]
    fn reports_metadata_trails() {
        let _session = MemorySession::activate();
        let mut objects = vec![b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n".to_vec()];
        objects.extend(pages_and_page(""));
        objects.push(b"4 0 obj\n<< /Author (Jane) /Producer (Selis) >>\nendobj\n".to_vec());
        insert_input("privacy/meta/in.pdf", build(objects, "/Info 4 0 R"));

        let report = privacy_report("privacy/meta/in.pdf").expect("report");
        let trails = find(&report, "metadata_trails").expect("metadata finding");
        assert_eq!(trails.severity, "low");
        assert_eq!(trails.count, 2, "Author and Producer");
    }

    #[test]
    fn detects_embedded_files() {
        let _session = MemorySession::activate();
        let mut objects =
            vec![b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R /Names 4 0 R >>\nendobj\n".to_vec()];
        objects.extend(pages_and_page(""));
        objects.push(b"4 0 obj\n<< /EmbeddedFiles 5 0 R >>\nendobj\n".to_vec());
        objects.push(b"5 0 obj\n<< /Names [(f.txt) 6 0 R] >>\nendobj\n".to_vec());
        objects.push(
            b"6 0 obj\n<< /Type /Filespec /F (f.txt) /EF << /F 7 0 R >> >>\nendobj\n".to_vec(),
        );
        objects.push(stream_object(7, "<< /Length 3 >>", b"abc"));
        insert_input("privacy/embed/in.pdf", build(objects, ""));

        let report = privacy_report("privacy/embed/in.pdf").expect("report");
        let embed = find(&report, "embedded_files").expect("embedded finding");
        assert_eq!(embed.count, 1);
    }

    #[test]
    fn flags_possible_hidden_text_as_a_heuristic() {
        let _session = MemorySession::activate();
        let mut objects = vec![b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n".to_vec()];
        objects.extend(pages_and_page("/Resources 4 0 R"));
        objects.push(
            b"4 0 obj\n<< /Font << /F1 5 0 R >> /XObject << /Im1 6 0 R >> >>\nendobj\n".to_vec(),
        );
        objects.push(
            b"5 0 obj\n<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>\nendobj\n".to_vec(),
        );
        objects.push(stream_object(
            6,
            "<< /Type /XObject /Subtype /Image /Width 300 /Height 300 /BitsPerComponent 8 /ColorSpace /DeviceRGB /Length 3 >>",
            b"rgb",
        ));
        insert_input("privacy/scan/in.pdf", build(objects, ""));

        let report = privacy_report("privacy/scan/in.pdf").expect("report");
        let hidden = find(&report, "possible_hidden_text").expect("hidden-text finding");
        assert_eq!(hidden.severity, "info", "a heuristic is info, never high");
        assert!(
            hidden.detail.contains("heuristic"),
            "the detail says it is a heuristic"
        );
    }

    #[test]
    fn a_clean_document_has_no_findings() {
        let _session = MemorySession::activate();
        let mut objects = vec![b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n".to_vec()];
        objects.extend(pages_and_page(""));
        insert_input("privacy/clean/in.pdf", build(objects, ""));

        let report = privacy_report("privacy/clean/in.pdf").expect("report");
        assert!(
            report.findings.is_empty(),
            "a plain page is clean: {:?}",
            report.findings
        );
        assert_eq!(report.risk, "none");

        let json = String::from_utf8(report.to_json().expect("json")).expect("UTF-8");
        assert!(json.contains("selis-privacy/1"));
    }

    #[test]
    fn an_encrypted_document_is_reported_as_unscannable() {
        let _session = MemorySession::activate();
        let mut objects = vec![b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n".to_vec()];
        objects.extend(pages_and_page(""));
        insert_input(
            "privacy/enc/in.pdf",
            build(objects, "/Encrypt 99 0 R /ID [<00> <11>]"),
        );

        let report = privacy_report("privacy/enc/in.pdf").expect("report");
        assert_eq!(report.status, "encrypted");
        assert_eq!(report.pages, None);
        assert_eq!(report.risk, "none");
        assert!(
            find(&report, "encrypted").is_some(),
            "says it could not scan"
        );
    }
}
