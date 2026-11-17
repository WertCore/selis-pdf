//! TrueType font subsetting as a whole-document optimise step (SL-3.FONT.11).
//!
//! This is the *lossless* counterpart of the image recompression pass: it
//! rewrites only the embedded font programs that the document's text actually
//! references, leaving content streams, annotations, forms and page order
//! byte-identical. The subsetter itself lives in `selis-font`
//! ([`selis_font::subset_ttf_with_map`]); this module is the COS-side
//! orchestration the parity spec assigns to `selis-pdf-optimize`.
//!
//! # Supported shapes
//!
//! * **Simple WinAnsi fonts** (`/Subtype /TrueType` or `/Type1`, `/Encoding
//!   /WinAnsiEncoding`, an embedded `/FontFile2`): each shown single byte is
//!   resolved to a glyph id through `selis_font::glyph_id_for_simple_code`,
//!   the program is subset to those ids, and the byte codes and `/W`-style
//!   byte-keyed tables stay untouched. The subset's format-12 `cmap` maps the
//!   same Unicode scalars to the new glyph ids, so extraction keeps working.
//! * **Identity-H CID fonts** (`/Type0` → `/Encoding /Identity-H` →
//!   `/CIDFontType2` with `/CIDToGIDMap /Identity` or absent, embedded
//!   `/FontFile2`): each shown 2-byte code is a CID equal to a glyph id. The
//!   program is subset to the used CIDs and a fresh `/CIDToGIDMap` stream is
//!   written to carry the subsetter's renumbering, so the original codes keep
//!   selecting the right outlines.
//!
//! # Deliberate deviations (fail loudly, never pass through)
//!
//! CFF programs (`/FontFile3`, `CIDFontType0`, OpenType/CFF), non-Identity-H
//! CMaps, stream-valued `/CIDToGIDMap`, `/Differences` or non-WinAnsi simple
//! encodings, non-Flate stream filters, unembedded fonts, and documents where
//! no embedded TrueType program is used by text all produce a typed
//! [`SubsetError`]. A rewrite that is not strictly smaller than the input is
//! also refused with [`SubsetError::WouldGrow`].
//!
//! # Budget
//!
//! Stream decoding, tokenising, subsetting and rewriting are charged through
//! the caller's guard. The subset program is bounded by the source program.
//!
//! # Malformed Input
//!
//! A document that cannot be parsed, has no root, is encrypted, or whose font
//! graph cannot be resolved yields a typed [`SubsetError`]; a broken font
//! program yields [`SubsetError::UnsupportedFont`].

#![cfg_attr(
    test,
    allow(
        clippy::indexing_slicing,
        clippy::arithmetic_side_effects,
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::cast_possible_truncation,
        clippy::integer_division
    )
)]

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use selis_bytes::Bytes;
use selis_font::{
    glyph_id_for_simple_code, subset_ttf_with_map, BaseEncoding, FontEncoding, GlyphSet,
};
use selis_pdf_content::{tokenise, Tok};
use selis_pdf_cos::copy::collect_objects;
use selis_pdf_cos::doc_writer::write_objects_as_document_with_trailer;
use selis_pdf_cos::{Obj, Ref};
use selis_sandbox::{Budget, BudgetGuard};

/// The largest single stream this pass will expand while decoding a font
/// program or a page's content stream.
const DECODE_LIMIT: u64 = 256 * 1024 * 1024;

/// A typed failure of the font-subset pass.
///
/// Every variant is a reason to *refuse* the input; the pass never returns an
/// unmodified copy in place of an error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SubsetError {
    /// The document could not be parsed or has no usable root/pages.
    Malformed(String),
    /// Encrypted documents are refused; unlocking is a separate operation.
    Encrypted,
    /// No embedded TrueType program is referenced by the document's text.
    NoEligibleFonts,
    /// An embedded program exists but is a shape this pass cannot subset.
    UnsupportedFont(String),
    /// A supported program was selected but no shown code resolved to a glyph.
    NoGlyphsUsed(String),
    /// The rewritten document is not strictly smaller than the input.
    WouldGrow {
        /// Input size in bytes.
        input: usize,
        /// Rewritten size in bytes.
        output: usize,
    },
    /// A shared backend step (parse, decode, subset, write) failed.
    Backend(String),
}

impl fmt::Display for SubsetError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Malformed(detail) => write!(f, "malformed document: {detail}"),
            Self::Encrypted => f.write_str("document is encrypted; unlock it before subsetting"),
            Self::NoEligibleFonts => {
                f.write_str("no embedded TrueType font is used by this document's text")
            }
            Self::UnsupportedFont(detail) => write!(f, "unsupported font: {detail}"),
            Self::NoGlyphsUsed(detail) => write!(f, "no shown glyphs resolved for {detail}"),
            Self::WouldGrow { input, output } => write!(
                f,
                "subsetting would grow the document ({input} -> {output} bytes)"
            ),
            Self::Backend(detail) => write!(f, "font-subset backend failure: {detail}"),
        }
    }
}

impl std::error::Error for SubsetError {}

/// What one rewrite actually did.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SubsetReport {
    /// Embedded font programs replaced by a subset.
    pub fonts_subset: usize,
    /// Distinct glyph ids kept across all programs (before composite closure).
    pub glyphs_kept: usize,
    /// Input size in bytes.
    pub in_bytes: usize,
    /// Output size in bytes (always strictly smaller than `in_bytes`).
    pub out_bytes: usize,
}

/// How a font object's shown codes map to glyph ids.
#[derive(Debug, Clone, PartialEq, Eq)]
enum FontKind {
    /// Single-byte WinAnsi codes resolved through the font's cmap/encoding.
    Simple(FontEncoding),
    /// Two-byte Identity-H CIDs used directly as glyph ids.
    IdentityH,
}

impl FontKind {
    fn tag(&self) -> &'static str {
        match self {
            Self::Simple(_) => "simple/WinAnsi",
            Self::IdentityH => "Identity-H/CIDFontType2",
        }
    }
}

/// Where a font object's program lives and how to interpret its codes.
#[derive(Debug, Clone)]
struct FontTarget {
    /// Object number of the embedded `/FontFile2` stream.
    file_num: u32,
    /// Object number of the descendant CIDFont, for `/CIDToGIDMap` rewriting.
    descendant_num: Option<u32>,
    kind: FontKind,
    label: String,
}

/// Subset every embedded TrueType program used by the document's text and
/// return the rewritten document plus a measurement report.
///
/// Object collection and the final write use the same COS helpers as the
/// other full-rewrite tools. Nothing is committed to a filesystem here; the
/// caller owns the destination and its verification gate.
///
/// # Errors
///
/// A typed [`SubsetError`] for every input this pass cannot improve: malformed
/// or encrypted documents, unsupported embedded programs, fonts with no
/// resolvable glyphs, and rewrites that would not shrink the file.
///
/// # Budget
///
/// Charges stream decoding, tokenising, subsetting and the output document
/// through `g`. A hostile stream cannot make the pass unbounded.
///
/// # Malformed Input
///
/// A broken font program is [`SubsetError::UnsupportedFont`]; a broken
/// document is [`SubsetError::Malformed`] or [`SubsetError::Backend`].
pub fn subset_fonts(
    src: &[u8],
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
) -> Result<(Vec<u8>, SubsetReport), SubsetError> {
    let in_bytes = src.len();

    // --- Parse and collect the reachable object graph. ---
    let startxref = selis_pdf_cos::xref::find_startxref(src, 4096).unwrap_or(0);
    let doc = selis_pdf_cos::parse_revisions(src, startxref, budget, g)
        .map_err(|e| SubsetError::Malformed(format!("cannot parse: {e}")))?;
    let rev = doc
        .revisions()
        .last()
        .ok_or_else(|| SubsetError::Malformed("no revisions".to_string()))?;
    if rev.encrypt.is_some() {
        return Err(SubsetError::Encrypted);
    }
    let root = rev
        .root
        .ok_or_else(|| SubsetError::Malformed("no /Root in trailer".to_string()))?;
    let info = rev
        .trailer
        .iter()
        .find(|(key, _)| key.as_slice() == b"Info")
        .and_then(|(_, value)| match value {
            Obj::Ref(reference) => Some(*reference),
            _ => None,
        });

    let mut roots = vec![root];
    if let Some(info) = info {
        roots.push(info);
    }
    let mut next_num = 1u32;
    let (mut objects, remap) = collect_objects(src, &roots, &mut next_num, budget, g)
        .map_err(|e| SubsetError::Backend(format!("object collection: {e}")))?;
    let new_root = remap.get(&root.num).copied().unwrap_or(root.num);
    let info_num = info.and_then(|reference| remap.get(&reference.num).copied());

    // --- Read-only analysis: which programs, which glyph ids. ---
    let by_num: BTreeMap<u32, &Obj> = objects.iter().map(|(num, obj)| (*num, obj)).collect();
    let pages = page_objects(&by_num, new_root);
    if pages.is_empty() {
        return Err(SubsetError::Malformed("no usable page tree".to_string()));
    }
    let mut programs: BTreeMap<u32, Bytes> = BTreeMap::new();
    let mut plans: BTreeMap<u32, FontTarget> = BTreeMap::new();
    let mut gids: BTreeMap<u32, BTreeSet<u16>> = BTreeMap::new();
    let mut unsupported: Vec<String> = Vec::new();

    for page_num in &pages {
        let Some(resources) = resources_of(*page_num, &by_num) else {
            continue;
        };
        let fonts = fonts_of(resources);
        for content_num in content_streams(*page_num, &by_num) {
            let raw = match decode_stream_num(content_num, &by_num, g) {
                Ok(raw) => raw,
                Err(reason) => {
                    unsupported.push(reason);
                    continue;
                }
            };
            let shows = scan_shows(&raw, g)
                .map_err(|e| SubsetError::Backend(format!("content tokenise: {e}")))?;
            for (font_name, bytes) in shows {
                let Some(font_num) = fonts.get(font_name.as_slice()) else {
                    // A font selected by name but not declared in the
                    // resources is a viewer-substituted face, not our program.
                    continue;
                };
                let label = String::from_utf8_lossy(font_name.as_slice()).into_owned();
                let target = match resolve_font(*font_num, &by_num) {
                    Ok(Some(target)) => target,
                    Ok(None) => continue, // unembedded: nothing to subset
                    Err(reason) => {
                        unsupported.push(format!("{label}: {reason}"));
                        continue;
                    }
                };
                if let Some(existing) = plans.get(&target.file_num) {
                    if existing.kind.tag() != target.kind.tag() {
                        unsupported.push(format!(
                            "{label}: program {} shared by {} and {}",
                            target.file_num,
                            existing.kind.tag(),
                            target.kind.tag()
                        ));
                        continue;
                    }
                } else {
                    // Load and decode the program once per file object.
                    match decode_stream_num(target.file_num, &by_num, g) {
                        Ok(program) => {
                            programs.insert(target.file_num, program);
                        }
                        Err(reason) => {
                            unsupported.push(format!("{label}: {reason}"));
                            continue;
                        }
                    }
                    plans.insert(target.file_num, target.clone());
                }
                let keep = gids.entry(target.file_num).or_default();
                let program = match programs.get(&target.file_num) {
                    Some(program) => program,
                    None => continue,
                };
                match &target.kind {
                    FontKind::Simple(encoding) => {
                        for byte in &bytes {
                            if let Some(gid) =
                                glyph_id_for_simple_code(program, encoding, u16::from(*byte))
                            {
                                keep.insert(gid);
                            }
                        }
                    }
                    FontKind::IdentityH => {
                        for pair in bytes.chunks_exact(2) {
                            let Some(&hi) = pair.first() else { continue };
                            let Some(&lo) = pair.get(1) else { continue };
                            keep.insert(u16::from_be_bytes([hi, lo]));
                        }
                    }
                }
            }
        }
    }

    if let Some(reason) = unsupported.first() {
        return Err(SubsetError::UnsupportedFont(reason.clone()));
    }
    if plans.is_empty() || gids.is_empty() {
        return Err(SubsetError::NoEligibleFonts);
    }

    // --- Mutating pass: subset each program and rewrite its stream. ---
    let position: BTreeMap<u32, usize> = objects
        .iter()
        .enumerate()
        .map(|(index, (num, _))| (*num, index))
        .collect();
    let mut report = SubsetReport {
        in_bytes,
        ..SubsetReport::default()
    };
    let mut allocated = objects
        .iter()
        .map(|(num, _)| *num)
        .max()
        .unwrap_or(0)
        .saturating_add(1);

    for (file_num, keep_set) in &gids {
        let plan = match plans.get(file_num) {
            Some(plan) => plan,
            None => continue,
        };
        let program = match programs.get(file_num) {
            Some(program) => program,
            None => continue,
        };
        let keep: Vec<u16> = keep_set.iter().copied().collect();
        if keep.is_empty() {
            return Err(SubsetError::NoGlyphsUsed(plan.label.clone()));
        }
        let subset = subset_ttf_with_map(program, &GlyphSet { keep: keep.clone() }, g)
            .map_err(|e| SubsetError::Backend(format!("subset {}: {e}", plan.label)))?;
        let Some(subset) = subset else {
            return Err(SubsetError::UnsupportedFont(format!(
                "{}: embedded program is not a subsettable TrueType font",
                plan.label
            )));
        };

        let raw_len = subset.font.len();
        let flate = selis_pdf_filter::flate_encode(subset.font.as_slice(), 9);
        let (stored, filtered) = if flate.len() < raw_len {
            (Bytes::from(flate), true)
        } else {
            (subset.font.clone(), false)
        };
        let stored_len = stored.len();

        let index = match position.get(file_num) {
            Some(index) => *index,
            None => return Err(SubsetError::Backend("lost font stream".to_string())),
        };
        let slot = match objects.get_mut(index) {
            Some((_, obj)) => obj,
            None => return Err(SubsetError::Backend("lost font stream".to_string())),
        };
        let Obj::Stream { dict, data } = slot else {
            return Err(SubsetError::UnsupportedFont(format!(
                "{}: /FontFile2 is not a stream",
                plan.label
            )));
        };
        *data = stored;
        set_key(
            dict,
            b"Length",
            Obj::Int(i64::try_from(stored_len).unwrap_or(i64::MAX)),
        );
        set_key(
            dict,
            b"Length1",
            Obj::Int(i64::try_from(raw_len).unwrap_or(i64::MAX)),
        );
        if filtered {
            set_key(
                dict,
                b"Filter",
                Obj::Name(Bytes::copy_from_slice(b"FlateDecode")),
            );
        } else {
            remove_key(dict, b"Filter");
        }

        // Identity-H: the original 2-byte codes are CIDs, and the subsetter
        // compacted the glyph ids. Publish the new mapping as a CIDToGIDMap
        // stream so the unchanged codes keep selecting the right outlines.
        if let (FontKind::IdentityH, Some(descendant_num)) = (&plan.kind, plan.descendant_num) {
            let max_cid = keep_set.iter().copied().max().unwrap_or(0);
            let mut map = Vec::new();
            for cid in 0..=max_cid {
                let gid = subset.gid_map.get(&cid).copied().unwrap_or(0);
                map.extend_from_slice(&gid.to_be_bytes());
            }
            let map_num = allocated;
            allocated = allocated.saturating_add(1);
            let map_dict = vec![(
                Bytes::copy_from_slice(b"Length"),
                Obj::Int(i64::try_from(map.len()).unwrap_or(i64::MAX)),
            )];
            objects.push((
                map_num,
                Obj::Stream {
                    dict: map_dict,
                    data: Bytes::from(map),
                },
            ));
            let d_index = match position.get(&descendant_num) {
                Some(index) => *index,
                None => return Err(SubsetError::Backend("lost CIDFont object".to_string())),
            };
            if let Some((_, Obj::Dict(descendant))) = objects.get_mut(d_index) {
                set_key(descendant, b"CIDToGIDMap", Obj::Ref(Ref::new(map_num, 0)));
            } else {
                return Err(SubsetError::UnsupportedFont(format!(
                    "{}: descendant is not a dictionary",
                    plan.label
                )));
            }
        }

        report.fonts_subset = report.fonts_subset.saturating_add(1);
        report.glyphs_kept = report.glyphs_kept.saturating_add(keep.len());
    }

    if report.fonts_subset == 0 {
        return Err(SubsetError::NoEligibleFonts);
    }

    // --- Write and enforce "never larger than the input". ---
    let mut trailer = Vec::new();
    if let Some(num) = info_num {
        trailer.push((b"Info".to_vec(), Obj::Ref(Ref::new(num, 0))));
    }
    let out = write_objects_as_document_with_trailer(
        &objects,
        Ref::new(new_root, 0),
        &trailer,
        budget,
        g,
    )
    .map_err(|e| SubsetError::Backend(format!("write: {e}")))?;
    if out.len() >= in_bytes {
        return Err(SubsetError::WouldGrow {
            input: in_bytes,
            output: out.len(),
        });
    }
    report.out_bytes = out.len();
    Ok((out, report))
}

/// The object numbers of every page under `root`, in tree order (best effort).
fn page_objects(objects: &BTreeMap<u32, &Obj>, root: u32) -> Vec<u32> {
    let mut pages = Vec::new();
    let mut stack = vec![root];
    let mut seen = BTreeSet::new();
    while let Some(num) = stack.pop() {
        if !seen.insert(num) {
            continue;
        }
        let Some(dict) = objects.get(&num).and_then(|obj| as_dict(obj)) else {
            continue;
        };
        if matches!(name(dict, b"Type"), Some(b"Page")) {
            pages.push(num);
            continue;
        }
        // The catalog reaches the tree through /Pages; intermediate nodes
        // through /Kids. Follow both so either shape works.
        if let Some(Obj::Ref(reference)) = get(dict, b"Pages") {
            stack.push(reference.num);
        }
        if let Some(Obj::Array(kids)) = get(dict, b"Kids") {
            for kid in kids {
                if let Obj::Ref(reference) = kid {
                    stack.push(reference.num);
                }
            }
        }
    }
    pages.sort_unstable();
    pages
}

/// A page's resources, inherited through `/Parent` when the page omits them.
fn resources_of<'a>(
    page_num: u32,
    objects: &'a BTreeMap<u32, &'a Obj>,
) -> Option<&'a [(Bytes, Obj)]> {
    let mut current = page_num;
    for _ in 0..64usize {
        let dict = objects.get(&current).and_then(|obj| as_dict(obj))?;
        if let Some(value) = get(dict, b"Resources") {
            return match value {
                Obj::Dict(resources) => Some(resources),
                Obj::Ref(reference) => objects.get(&reference.num).and_then(|obj| as_dict(obj)),
                _ => None,
            };
        }
        current = match get(dict, b"Parent") {
            Some(Obj::Ref(reference)) => reference.num,
            _ => return None,
        };
    }
    None
}

/// The named font resources of a page: resource name → object number.
fn fonts_of(resources: &[(Bytes, Obj)]) -> BTreeMap<Vec<u8>, u32> {
    let mut fonts = BTreeMap::new();
    if let Some(Obj::Dict(dict)) = get(resources, b"Font") {
        for (name, value) in dict {
            if let Obj::Ref(reference) = value {
                fonts.insert(name.as_slice().to_vec(), reference.num);
            }
        }
    }
    fonts
}

/// The content stream object numbers referenced by a page.
fn content_streams(page_num: u32, objects: &BTreeMap<u32, &Obj>) -> Vec<u32> {
    let Some(dict) = objects.get(&page_num).and_then(|obj| as_dict(obj)) else {
        return Vec::new();
    };
    match get(dict, b"Contents") {
        Some(Obj::Ref(reference)) => vec![reference.num],
        Some(Obj::Array(entries)) => entries
            .iter()
            .filter_map(|entry| match entry {
                Obj::Ref(reference) => Some(reference.num),
                _ => None,
            })
            .collect(),
        _ => Vec::new(),
    }
}

/// Decode one content or font stream. A filter other than Flate is a refusal
/// reason rather than a panic.
fn decode_stream_num(
    num: u32,
    objects: &BTreeMap<u32, &Obj>,
    g: &mut BudgetGuard<'_>,
) -> Result<Bytes, String> {
    let obj = objects
        .get(&num)
        .ok_or_else(|| format!("missing object {num}"))?;
    let Obj::Stream { dict, data } = obj else {
        return Err(format!("object {num} is not a stream"));
    };
    match get(dict, b"Filter") {
        None => Ok(data.clone()),
        Some(Obj::Name(filter))
            if filter.as_slice() == b"FlateDecode" || filter.as_slice() == b"Fl" =>
        {
            let decoded = selis_pdf_filter::flate_decode_bounded(data, DECODE_LIMIT, g)
                .map_err(|e| format!("object {num}: {e}"))?;
            Ok(Bytes::from(decoded))
        }
        Some(Obj::Name(filter)) => Err(format!(
            "object {num}: unsupported stream filter {}",
            String::from_utf8_lossy(filter.as_slice())
        )),
        Some(_) => Err(format!("object {num}: unsupported stream filter array")),
    }
}

/// The (font resource name, raw shown bytes) pairs of one content stream.
fn scan_shows(data: &[u8], g: &mut BudgetGuard<'_>) -> Result<Vec<(Bytes, Vec<u8>)>, String> {
    let tokens = tokenise(data, g).map_err(|e| e.to_string())?;
    let mut shows = Vec::new();
    let mut current: Option<Bytes> = None;
    for index in 0..tokens.len() {
        let Some(token) = tokens.get(index) else {
            continue;
        };
        match token {
            Tok::Name(op) if op.as_slice() == b"Tf" => {
                if let Some(Tok::OperandName(font)) =
                    index.checked_sub(2).and_then(|i| tokens.get(i))
                {
                    current = Some(font.clone());
                }
            }
            Tok::Name(op) if matches!(op.as_slice(), b"Tj" | b"'" | b"\"") => {
                if let (Some(font), Some(Tok::Str(bytes))) = (
                    current.as_ref(),
                    index.checked_sub(1).and_then(|i| tokens.get(i)),
                ) {
                    shows.push((font.clone(), bytes.as_slice().to_vec()));
                }
            }
            Tok::Name(op) if op.as_slice() == b"TJ" => {
                if let Some(font) = current.as_ref() {
                    let mut bytes = Vec::new();
                    let mut cursor = index;
                    while cursor > 0 {
                        cursor = cursor.saturating_sub(1);
                        match tokens.get(cursor) {
                            Some(Tok::Str(string)) => bytes.extend_from_slice(string.as_slice()),
                            Some(Tok::ArrEnd) | Some(Tok::Num(_)) => continue,
                            Some(Tok::ArrStart) | None => break,
                            Some(_) => break,
                        }
                    }
                    if !bytes.is_empty() {
                        shows.push((font.clone(), bytes));
                    }
                }
            }
            _ => {}
        }
    }
    Ok(shows)
}

/// Resolve a font object to its embedded program and code semantics.
///
/// `Ok(None)` means "no embedded program": a standard-14 or unsubstituted
/// face that this pass has nothing to subset.
fn resolve_font(
    font_num: u32,
    objects: &BTreeMap<u32, &Obj>,
) -> Result<Option<FontTarget>, String> {
    let dict = objects
        .get(&font_num)
        .and_then(|obj| as_dict(obj))
        .ok_or_else(|| format!("font object {font_num} is not a dictionary"))?;
    let subtype =
        name(dict, b"Subtype").ok_or_else(|| format!("font object {font_num} has no /Subtype"))?;
    match subtype {
        b"Type0" => {
            let encoding =
                name(dict, b"Encoding").ok_or_else(|| "Type0 font has no /Encoding".to_string())?;
            if encoding != b"Identity-H" {
                return Err(format!(
                    "Type0 encoding {} is not Identity-H",
                    String::from_utf8_lossy(encoding)
                ));
            }
            let descendant_num = match get(dict, b"DescendantFonts") {
                Some(Obj::Array(entries)) => match entries.first() {
                    Some(Obj::Ref(reference)) => reference.num,
                    _ => return Err("Type0 has no descendant reference".to_string()),
                },
                _ => return Err("Type0 has no /DescendantFonts array".to_string()),
            };
            let descendant = objects
                .get(&descendant_num)
                .and_then(|obj| as_dict(obj))
                .ok_or_else(|| "descendant is not a dictionary".to_string())?;
            if name(descendant, b"Subtype") != Some(b"CIDFontType2") {
                return Err("descendant is not CIDFontType2 (CFF is not subset)".to_string());
            }
            match get(descendant, b"CIDToGIDMap") {
                None => {}
                Some(Obj::Name(map)) if map.as_slice() == b"Identity" => {}
                Some(Obj::Name(map)) => {
                    return Err(format!(
                        "CIDToGIDMap {} is not /Identity",
                        String::from_utf8_lossy(map.as_slice())
                    ))
                }
                Some(_) => return Err("stream CIDToGIDMap is not supported".to_string()),
            }
            let file_num = font_file2(descendant, objects)?;
            Ok(Some(FontTarget {
                file_num,
                descendant_num: Some(descendant_num),
                kind: FontKind::IdentityH,
                label: String::from("Identity-H CIDFont"),
            }))
        }
        b"TrueType" | b"Type1" | b"MMType1" => {
            // A standard-14 face has no descriptor at all: there is no program
            // to subset, so it is ineligible rather than unsupported.
            if get(dict, b"FontDescriptor").is_none() {
                return Ok(None);
            }
            let file_num = font_file2(dict, objects)?;
            let encoding = match get(dict, b"Encoding") {
                Some(Obj::Name(enc)) if enc.as_slice() == b"WinAnsiEncoding" => {
                    FontEncoding::Named(BaseEncoding::WinAnsi)
                }
                Some(Obj::Dict(enc)) => {
                    if get(enc, b"Differences").is_some() {
                        return Err("/Differences encoding is not supported".to_string());
                    }
                    match get(enc, b"BaseEncoding") {
                        Some(Obj::Name(base)) if base.as_slice() == b"WinAnsiEncoding" => {
                            FontEncoding::Named(BaseEncoding::WinAnsi)
                        }
                        _ => return Err("only WinAnsiEncoding is supported".to_string()),
                    }
                }
                _ => return Err("only WinAnsiEncoding is supported".to_string()),
            };
            Ok(Some(FontTarget {
                file_num,
                descendant_num: None,
                kind: FontKind::Simple(encoding),
                label: String::from("WinAnsi simple font"),
            }))
        }
        b"CIDFontType0" => Err("CIDFontType0 (CFF) is not subset".to_string()),
        b"Type3" => Err("Type3 fonts are not subset".to_string()),
        other => Err(format!(
            "font subtype {} is not subset",
            String::from_utf8_lossy(other)
        )),
    }
}

/// The `/FontFile2` object number on a font descriptor reached from `dict`.
fn font_file2(dict: &[(Bytes, Obj)], objects: &BTreeMap<u32, &Obj>) -> Result<u32, String> {
    let descriptor = match get(dict, b"FontDescriptor") {
        Some(Obj::Ref(reference)) => objects
            .get(&reference.num)
            .and_then(|obj| as_dict(obj))
            .ok_or_else(|| "FontDescriptor is not a dictionary".to_string())?,
        Some(Obj::Dict(descriptor)) => descriptor,
        _ => return Err("font has no FontDescriptor".to_string()),
    };
    match font_file_num(descriptor) {
        Some(num) => Ok(num),
        None if has_font_file3(descriptor) => {
            Err("embedded CFF (/FontFile3) program is not subset".to_string())
        }
        None => Err("font has no embedded /FontFile2 program".to_string()),
    }
}

/// The `/FontFile2` reference on a font dictionary (or descriptor).
fn font_file_num(dict: &[(Bytes, Obj)]) -> Option<u32> {
    match get(dict, b"FontFile2") {
        Some(Obj::Ref(reference)) => Some(reference.num),
        _ => None,
    }
}

fn has_font_file3(dict: &[(Bytes, Obj)]) -> bool {
    get(dict, b"FontFile3").is_some()
}

fn as_dict(obj: &Obj) -> Option<&[(Bytes, Obj)]> {
    match obj {
        Obj::Dict(dict) | Obj::Stream { dict, .. } => Some(dict),
        _ => None,
    }
}

fn get<'a>(dict: &'a [(Bytes, Obj)], key: &[u8]) -> Option<&'a Obj> {
    dict.iter()
        .find(|(entry, _)| entry.as_slice() == key)
        .map(|(_, value)| value)
}

fn name<'a>(dict: &'a [(Bytes, Obj)], key: &[u8]) -> Option<&'a [u8]> {
    match get(dict, key) {
        Some(Obj::Name(value)) => Some(value.as_slice()),
        _ => None,
    }
}

fn set_key(dict: &mut Vec<(Bytes, Obj)>, key: &[u8], value: Obj) {
    if let Some(slot) = dict.iter_mut().find(|(entry, _)| entry.as_slice() == key) {
        slot.1 = value;
    } else {
        dict.push((Bytes::copy_from_slice(key), value));
    }
}

fn remove_key(dict: &mut Vec<(Bytes, Obj)>, key: &[u8]) {
    dict.retain(|(entry, _)| entry.as_slice() != key);
}

#[cfg(test)]
mod tests {
    use super::*;
    use selis_pdf_cos::doc_writer::DocumentBuilder;

    const TTF: &[u8] = include_bytes!("../../../assets/fonts/LiberationSans-Regular.ttf");

    fn name(value: &[u8]) -> Obj {
        Obj::Name(Bytes::copy_from_slice(value))
    }

    /// A one-page document whose text shows one embedded WinAnsi TrueType font.
    fn embedded_font_pdf(program: &[u8]) -> Vec<u8> {
        let budget = Budget::unlimited();
        let mut g = budget.guard();
        let mut builder = DocumentBuilder::new();
        let file = builder.allocate();
        builder.add_object(
            file,
            Obj::Stream {
                dict: vec![
                    (
                        Bytes::copy_from_slice(b"Length"),
                        Obj::Int(i64::try_from(program.len()).unwrap_or(i64::MAX)),
                    ),
                    (
                        Bytes::copy_from_slice(b"Length1"),
                        Obj::Int(i64::try_from(program.len()).unwrap_or(i64::MAX)),
                    ),
                ],
                data: Bytes::copy_from_slice(program),
            },
        );
        let font = builder.allocate();
        builder.add_object(
            font,
            Obj::Dict(vec![
                (Bytes::copy_from_slice(b"Type"), name(b"Font")),
                (Bytes::copy_from_slice(b"Subtype"), name(b"TrueType")),
                (
                    Bytes::copy_from_slice(b"Encoding"),
                    name(b"WinAnsiEncoding"),
                ),
                (
                    Bytes::copy_from_slice(b"FontDescriptor"),
                    Obj::Dict(vec![(
                        Bytes::copy_from_slice(b"FontFile2"),
                        Obj::Ref(Ref::new(file, 0)),
                    )]),
                ),
            ]),
        );
        let resources = builder.allocate();
        builder.add_object(
            resources,
            Obj::Dict(vec![(
                Bytes::copy_from_slice(b"Font"),
                Obj::Dict(vec![(
                    Bytes::copy_from_slice(b"F1"),
                    Obj::Ref(Ref::new(font, 0)),
                )]),
            )]),
        );
        let content = b"BT /F1 24 Tf 72 700 Td (Hello) Tj ET\n";
        let content_num = builder.allocate();
        builder.add_object(
            content_num,
            Obj::Stream {
                dict: vec![(
                    Bytes::copy_from_slice(b"Length"),
                    Obj::Int(i64::try_from(content.len()).unwrap_or(i64::MAX)),
                )],
                data: Bytes::copy_from_slice(content),
            },
        );
        builder.add_page_with(
            612.0,
            792.0,
            &[Ref::new(content_num, 0)],
            Some(Ref::new(resources, 0)),
        );
        builder
            .write(&budget, &mut g)
            .expect("write embedded-font fixture")
    }

    #[test]
    fn subsets_an_embedded_program_and_returns_smaller_reparseable_bytes() {
        let budget = Budget::unlimited();
        let mut g = budget.guard();
        let src = embedded_font_pdf(TTF);
        let (out, report) = subset_fonts(&src, &budget, &mut g).expect("subset");
        assert_eq!(report.fonts_subset, 1);
        assert!(report.glyphs_kept >= 4);
        assert_eq!(report.in_bytes, src.len());
        assert_eq!(report.out_bytes, out.len());
        assert!(out.len() < src.len(), "{} < {}", out.len(), src.len());
        // The output is a real, reparseable document.
        let start = selis_pdf_cos::xref::find_startxref(&out, 4096).unwrap_or(0);
        let doc = selis_pdf_cos::parse_revisions(&out, start, &budget, &mut g).expect("reparse");
        assert!(doc.revisions().last().and_then(|r| r.root).is_some());
    }

    #[test]
    fn a_document_without_an_embedded_font_is_refused() {
        let budget = Budget::unlimited();
        let mut g = budget.guard();
        let mut builder = DocumentBuilder::new();
        let content = b"BT (plain) Tj ET\n".to_vec();
        builder.add_page(200.0, 300.0, &content);
        let src = builder.write(&budget, &mut g).expect("write");
        assert_eq!(
            subset_fonts(&src, &budget, &mut g).expect_err("refused"),
            SubsetError::NoEligibleFonts
        );
    }

    #[test]
    fn a_cff_program_is_refused_loudly() {
        let budget = Budget::unlimited();
        let mut g = budget.guard();
        let cff = include_bytes!("../../../crates/selis-font/tests/fixtures/cff.otf");
        let src = embedded_font_pdf(cff);
        let err = subset_fonts(&src, &budget, &mut g).expect_err("CFF refused");
        assert!(
            matches!(
                err,
                SubsetError::UnsupportedFont(_) | SubsetError::NoGlyphsUsed(_)
            ),
            "typed refusal instead of pass-through: {err:?}"
        );
    }
}
