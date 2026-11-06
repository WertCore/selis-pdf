//! Shared reader helpers for the Office Open XML inputs (`.docx`, `.pptx`,
//! `.xlsx`) — Phase 3, OOXML → PDF.
//!
//! An OOXML file is a ZIP package of XML parts, so all three readers start the
//! same way: open the package with the shared archive reader, find the part the
//! format names, parse it with the crate's XML scanner (which drops namespace
//! prefixes and lower-cases names, so `w:p` and `p:p` are both `p`), and walk
//! the local names. Relationships are read from the `_rels` part beside the
//! part that owns them.
//!
//! Nothing here interprets styling beyond what the layout engine can express:
//! bold, italic, headings, list markers and tables. Colour, themes, fonts,
//! images, shapes and page decoration are not carried, and the three readers
//! say so in their own module docs.

#![allow(clippy::arithmetic_side_effects, clippy::indexing_slicing)]

use selis_error::{Code, Ctx, Error, Result};

use crate::xmlmini::{parse, Element};

/// One input the converter cannot use, with the detail sanitised so a
/// document-derived name can appear in a diagnostic safely (ADR-P0017).
pub(crate) fn malformed(during: &'static str, message: String) -> Error {
    Error::with(
        Code::ConvertMalformed,
        Ctx::new()
            .during(during)
            .detail(Ctx::sanitise(message.as_bytes())),
    )
}

/// Read one package part by its full path.
pub(crate) fn part<'a>(entries: &'a [(String, Vec<u8>)], name: &str) -> Option<&'a [u8]> {
    entries
        .iter()
        .find(|(entry, _)| entry == name)
        .map(|(_, data)| data.as_slice())
}

/// Parse one XML part, naming the part in a malformed-XML diagnostic.
pub(crate) fn parse_part(bytes: &[u8], during: &'static str, name: &str) -> Result<Element> {
    let text = String::from_utf8_lossy(bytes);
    parse(&text).map_err(|message| malformed(during, format!("{name}: {message}")))
}

/// All text directly below `node`, in document order.
pub(crate) fn deep_text(node: &Element) -> String {
    let mut out = String::new();
    collect_text(node, &mut out);
    out
}

fn collect_text(node: &Element, out: &mut String) {
    out.push_str(&node.text);
    for child in &node.children {
        collect_text(child, out);
    }
}

/// The first attribute whose local name matches, whatever its prefix.
///
/// The parser lower-cases attribute names but keeps the prefix (`w:val`,
/// `r:id`), and different parts of one package are free to bind different
/// prefixes to the same namespace, so matching is by the name after the colon.
pub(crate) fn attr<'a>(node: &'a Element, local: &str) -> Option<&'a str> {
    node.attrs
        .iter()
        .find(|(key, _)| local_name(key).eq_ignore_ascii_case(local))
        .map(|(_, value)| value.as_str())
}

/// The local name of a possibly prefixed tag or attribute.
pub(crate) fn local_name(name: &str) -> &str {
    name.rsplit(':').next().unwrap_or(name)
}

/// A relationship as the package declares it.
pub(crate) struct Relationship {
    pub id: String,
    pub target: String,
}

/// Read the relationships that belong to `part_path` (`ppt/presentation.xml`
/// reads `ppt/_rels/presentation.xml.rels`).
pub(crate) fn relationships(
    entries: &[(String, Vec<u8>)],
    part_path: &str,
    during: &'static str,
) -> Vec<Relationship> {
    let (dir, file) = split_path(part_path);
    let rels_path = if dir.is_empty() {
        format!("_rels/{file}.rels")
    } else {
        format!("{dir}/_rels/{file}.rels")
    };
    let Some(bytes) = part(entries, &rels_path) else {
        return Vec::new();
    };
    let Ok(root) = parse_part(bytes, during, &rels_path) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for rel in root.find_all("relationship") {
        let id = attr(rel, "id").unwrap_or("").trim().to_string();
        let target = attr(rel, "target").unwrap_or("").trim().to_string();
        if !id.is_empty() && !target.is_empty() {
            out.push(Relationship { id, target });
        }
    }
    out
}

/// Resolve a relationship target relative to the directory of the part that
/// declared it. A leading `/` makes the target package-absolute.
pub(crate) fn resolve_target(owner_dir: &str, target: &str) -> String {
    let target = target.trim();
    if let Some(absolute) = target.strip_prefix('/') {
        return normalise(absolute);
    }
    if owner_dir.is_empty() {
        normalise(target)
    } else {
        normalise(&format!("{owner_dir}/{target}"))
    }
}

/// Collapse `.` and `..` segments in a package path.
fn normalise(path: &str) -> String {
    let mut parts: Vec<&str> = Vec::new();
    for segment in path.split('/') {
        match segment {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            other => parts.push(other),
        }
    }
    parts.join("/")
}

/// Split a package path into its directory and file name.
pub(crate) fn split_path(path: &str) -> (&str, &str) {
    match path.rfind('/') {
        Some(at) => (&path[..at], &path[at + 1..]),
        None => ("", path),
    }
}

/// Parse a boolean attribute the DrawingML way (`b="1"`).
///
/// DrawingML does not carry an `on`/`off` vocabulary like Word's properties:
/// its booleans are `1`/`0` (and, from some producers, `true`/`false`).
/// `None` means the attribute is absent, which for a toggle means off.
pub(crate) fn bool_attr(node: &Element, local: &str) -> Option<bool> {
    let value = attr(node, local)?.trim().to_ascii_lowercase();
    Some(matches!(value.as_str(), "1" | "true" | "on"))
}

/// Whether an OOXML boolean property says "on".
///
/// The property's presence is enough unless it carries a false value: Word and
/// PowerPoint both write `<w:b/>` without a value and `<w:b w:val="0"/>` when a
/// style turned it off.
pub(crate) fn flag(node: &Element) -> bool {
    match attr(node, "val") {
        None => true,
        Some(value) => {
            let value = value.trim();
            !(value.eq_ignore_ascii_case("0")
                || value.eq_ignore_ascii_case("false")
                || value.eq_ignore_ascii_case("off"))
        }
    }
}

/// The relationship id of a node that references one, whatever its prefix.
///
/// `<p:sldId id="256" r:id="rId4"/>` carries two attributes whose local
/// name is `id`; the relationship reference is the prefixed one, so a plain
/// `id` match would silently read the literal `256`. Prefer a prefixed
/// attribute; fall back to a bare `id` when nothing else is present.
pub(crate) fn rel_attr(node: &Element) -> Option<&str> {
    node.attrs
        .iter()
        .find(|(key, _)| local_name(key).eq_ignore_ascii_case("id") && key.contains(':'))
        .or_else(|| {
            node.attrs
                .iter()
                .find(|(key, _)| key.eq_ignore_ascii_case("id"))
        })
        .map(|(_, value)| value.trim())
        .filter(|value| !value.is_empty())
}

/// The children of `node` whose local tag name is `tag`, in order.
pub(crate) fn children<'a>(node: &'a Element, tag: &str) -> Vec<&'a Element> {
    node.children
        .iter()
        .filter(|child| child.tag.eq_ignore_ascii_case(tag))
        .collect()
}

/// The direct child of `node` whose local tag name is `tag`.
pub(crate) fn child<'a>(node: &'a Element, tag: &str) -> Option<&'a Element> {
    node.children
        .iter()
        .find(|child| child.tag.eq_ignore_ascii_case(tag))
}
