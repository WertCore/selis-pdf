//! Model-independent byte-region reflow writer (SL-5.TEXTEDIT.02).
//!
//! This is the byte-level half of a model-based text edit. The caller (the
//! engine, which owns the paragraph model and the shaper) hands it:
//!
//! * one or more [`ContentRegion`]s — byte spans in the **decoded** page content
//!   stream covering the show-text operators of a single paragraph, each with
//!   the replacement operators to write there; and
//! * zero or more raw [`ReflowMutation::SetObject`]s — for example a re-embedded
//!   font program and the font dictionary that points at it.
//!
//! The writer splices the regions into the decoded stream, leaves **every other
//! byte of that stream identical**, and commits the change with
//! [`crate::incremental::save_incremental`] so the original file is a verbatim
//! byte prefix of the result (I3/I7). Nothing here shapes text, breaks lines
//! or reads a paragraph model — those are the engine's job; this module only
//! guarantees that an edit touches exactly the spans it was told to.
//!
//! # Budget / Malformed Input
//!
//! The decoded stream is bounded by [`crate::MAX_CONTENT_BYTES`]. Regions that
//! are out of bounds, reversed or overlapping are refused with a typed error
//! before a single byte is written. An encrypted document is refused. All
//! allocations are charged to the caller's [`BudgetGuard`] through the parse,
//! materialise and incremental-save path.
#![forbid(unsafe_code)]

use std::fmt;
use std::ops::Range;

use selis_bytes::Bytes;
use selis_pdf_cos::Obj;
use selis_sandbox::{Budget, BudgetGuard};

use crate::incremental::{self, ObjectMutation};
use crate::{
    content_refs, decode_content, encode_content, is_encrypted, page_refs, rebuild_stream,
    resolve_ref, stream_filter, EditError,
};

/// The provisional confidence a paragraph must clear before the engine offers
/// model-based reflow (SL-5.TEXTEDIT.01/02).
///
/// This mirrors `selis_pdf_text::PROVISIONAL_REFLOW_THRESHOLD` without making the
/// edit crate depend on the text crate (they are L2 siblings). It is the
/// midpoint of the small *synthetic* labelled set, **not** the DoD's 500-page
/// correlation — see the honesty note in `pdf-plan/15-PHASE-5-edit.md`.
pub const MIN_REFLOW_CONFIDENCE: f64 = 0.80;

/// A byte span in the decoded content stream and the operators replacing it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContentRegion {
    /// The exact byte span (in the decoded stream) of the paragraph's
    /// show-text operators, delimiters included.
    pub span: Range<usize>,
    /// The replacement operators, written in the span's place.
    pub replacement: Vec<u8>,
}

/// One object-level change that accompanies a reflow (a re-embedded font
/// program and its dictionary, typically).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReflowMutation {
    /// Replace a paragraph's show-text byte region in the decoded stream.
    Content(ContentRegion),
    /// Redefine a COS object (e.g. a `/FontFile2` stream or a font dictionary
    /// with a widened `/Widths` and `/LastChar`).
    SetObject {
        /// Object number to redefine.
        num: u32,
        /// New value.
        value: Obj,
    },
}

/// What a model-based reflow wrote.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReflowEditReport {
    /// The edited 0-based page.
    pub page: u32,
    /// Input size in bytes.
    pub bytes_in: u64,
    /// Output size in bytes (≥ `bytes_in`: an incremental update only appends).
    pub bytes_out: u64,
    /// The decoded-stream byte spans that were replaced, in write order.
    pub changed_regions: Vec<Range<usize>>,
    /// How many COS objects were redefined in the appended revision.
    pub objects_changed: usize,
}

/// A reflow-level refusal, including the low-confidence gate.
#[derive(Debug, Clone, PartialEq)]
pub enum ReflowEditError {
    /// The paragraph model's confidence is below [`MIN_REFLOW_CONFIDENCE`]: the
    /// caller must offer the box-level fallback (SL-5.TEXTEDIT.03) instead.
    LowConfidence {
        /// The confidence the model reported.
        confidence: f64,
        /// The threshold that was not cleared.
        threshold: f64,
    },
    /// The byte write itself failed.
    Edit(EditError),
}

impl fmt::Display for ReflowEditError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::LowConfidence {
                confidence,
                threshold,
            } => write!(
                f,
                "paragraph confidence {confidence:.3} is below the reflow threshold {threshold:.3}; use box-level editing (SL-5.TEXTEDIT.03) instead"
            ),
            Self::Edit(e) => fmt::Display::fmt(e, f),
        }
    }
}

impl std::error::Error for ReflowEditError {}

/// Refuse a paragraph whose reconstruction confidence is too low to reflow.
///
/// A non-finite confidence fails closed. The threshold is the provisional one
/// from [`MIN_REFLOW_CONFIDENCE`]; when it is not cleared the caller is told to
/// use the box-level path (`selis_pdf_edit::edit_box`, SL-5.TEXTEDIT.03), which
/// edits one run without silently re-laying-out the paragraph.
///
/// # Budget
///
/// None: this is a pure comparison of two `f64`s.
///
/// # Malformed Input
///
/// A non-finite confidence fails closed (it is treated as 0 and refused).
pub fn ensure_reflow_confidence(confidence: f64) -> Result<(), ReflowEditError> {
    if confidence.is_finite() && confidence >= MIN_REFLOW_CONFIDENCE {
        Ok(())
    } else {
        Err(ReflowEditError::LowConfidence {
            confidence: if confidence.is_finite() {
                confidence
            } else {
                0.0
            },
            threshold: MIN_REFLOW_CONFIDENCE,
        })
    }
}

/// Replace the given decoded-stream regions, apply the object mutations and
/// save the result as an incremental update.
///
/// `regions` must be sorted ascending and non-overlapping; they are checked
/// against the decoded stream length before anything is written. The content
/// stream itself is re-encoded in its original filter and redefined as one
/// object; a page whose content was split across several streams is collapsed
/// onto the first (the same normalisation the in-place edit performs).
///
/// # Errors
///
/// [`ReflowEditError::Edit`] with [`EditError::Invalid`] for out-of-range or
/// overlapping regions, [`EditError::Encrypted`] for an encrypted document,
/// [`EditError::Unsupported`] for a page/stream shape outside this writer, and
/// [`EditError::Malformed`]/[`EditError::Unresolvable`] for unreadable bytes.
///
/// # Budget
///
/// The decoded stream is bounded by [`crate::MAX_CONTENT_BYTES`]. Every parse,
/// resolve and write is charged to `g`.
///
/// # Malformed Input
///
/// Out-of-range or overlapping regions, an encrypted document, an unreadable
/// object graph or a page with no content stream are typed refusals — no bytes
/// are produced.
pub fn reflow_regions(
    src: &[u8],
    page_index: u32,
    mutations: &[ReflowMutation],
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
) -> Result<(Vec<u8>, ReflowEditReport), ReflowEditError> {
    let mut regions: Vec<&ContentRegion> = Vec::new();
    for m in mutations {
        if let ReflowMutation::Content(region) = m {
            regions.push(region);
        }
    }
    // Order is part of the contract: a caller builds the replacement stream in
    // this order, so a region list that is not sorted is a programming error.
    let sorted = regions.windows(2).all(|pair| {
        pair.first()
            .is_some_and(|a| pair.get(1).is_some_and(|b| a.span.end <= b.span.start))
    });
    if !sorted {
        return Err(ReflowEditError::Edit(EditError::Invalid(
            "reflow content regions must be sorted and non-overlapping".to_string(),
        )));
    }

    let startxref = selis_pdf_cos::xref::find_startxref(src, 4096).unwrap_or(0);
    let doc = selis_pdf_cos::parse_revisions(src, startxref, budget, g)
        .map_err(|e| EditError::Malformed(format!("cannot open: {e}")))
        .map_err(ReflowEditError::Edit)?;
    if is_encrypted(&doc) {
        return Err(ReflowEditError::Edit(EditError::Encrypted));
    }

    let pages = page_refs(src, &doc, budget, g).map_err(ReflowEditError::Edit)?;
    let page_ref = pages.get(page_index as usize).copied().ok_or_else(|| {
        ReflowEditError::Edit(EditError::Invalid(format!(
            "page {page_index} is out of range; the document has {} page(s)",
            pages.len()
        )))
    })?;
    let page_obj = resolve_ref(src, &doc, page_ref, budget, g)
        .map_err(|e| EditError::Unresolvable(format!("cannot read the page: {e}")))
        .map_err(ReflowEditError::Edit)?;
    let Obj::Dict(page_dict) = page_obj else {
        return Err(ReflowEditError::Edit(EditError::Malformed(
            "the page is not a dictionary".to_string(),
        )));
    };
    let refs = content_refs(&page_dict).map_err(ReflowEditError::Edit)?;

    let mut decoded: Vec<u8> = Vec::new();
    let mut filter: Option<Bytes> = None;
    let mut stream_dict: Vec<(Bytes, Obj)> = Vec::new();
    for (index, content_ref) in refs.iter().enumerate() {
        let content_obj = resolve_ref(src, &doc, *content_ref, budget, g)
            .map_err(|e| EditError::Unresolvable(format!("cannot read the page content: {e}")))
            .map_err(ReflowEditError::Edit)?;
        let Obj::Stream { dict, data } = content_obj else {
            return Err(ReflowEditError::Edit(EditError::Malformed(
                "the page content is not a stream".to_string(),
            )));
        };
        let part_filter = stream_filter(&dict).map_err(ReflowEditError::Edit)?;
        let part = decode_content(part_filter.clone(), data.as_slice(), budget, g)
            .map_err(ReflowEditError::Edit)?;
        if index == 0 {
            filter = part_filter;
            stream_dict = dict;
        }
        if !decoded.is_empty() {
            decoded.push(b'\n');
        }
        decoded.extend_from_slice(&part);
    }
    let decoded_len = decoded.len();
    for region in &regions {
        if region.span.start > region.span.end || region.span.end > decoded_len {
            return Err(ReflowEditError::Edit(EditError::Invalid(format!(
                "reflow region {:?} is outside the {} byte decoded stream",
                region.span, decoded_len
            ))));
        }
    }

    // Splice every region into the decoded stream, copying the untouched bytes
    // verbatim. Regions are disjoint and sorted, so a single forward pass is
    // enough and no byte outside them can change.
    let mut rewritten: Vec<u8> = Vec::with_capacity(decoded_len.saturating_add(64));
    let mut copied = 0usize;
    for region in &regions {
        if let Some(prefix) = decoded.get(copied..region.span.start) {
            rewritten.extend_from_slice(prefix);
        }
        rewritten.extend_from_slice(&region.replacement);
        copied = region.span.end;
    }
    if let Some(suffix) = decoded.get(copied..) {
        rewritten.extend_from_slice(suffix);
    }

    let (encoded, filter_name) = encode_content(filter, &rewritten);
    let new_stream = rebuild_stream(&stream_dict, encoded, filter_name);

    let first_content = refs.first().copied().ok_or_else(|| {
        ReflowEditError::Edit(EditError::Malformed(
            "page has no content stream to reflow".to_string(),
        ))
    })?;
    let model = incremental::materialise(src, budget, g).map_err(|e| {
        ReflowEditError::Edit(EditError::Unresolvable(format!(
            "cannot materialise the document: {e}"
        )))
    })?;
    let mut objs: Vec<(u32, Obj)> = Vec::with_capacity(mutations.len().saturating_add(2));
    objs.push((first_content.num, new_stream));
    if refs.len() > 1 {
        let mut page: Vec<(Bytes, Obj)> = Vec::with_capacity(page_dict.len());
        for (key, value) in &page_dict {
            if key.as_slice() == b"Contents" {
                page.push((key.clone(), Obj::Ref(first_content)));
            } else {
                page.push((key.clone(), value.clone()));
            }
        }
        objs.push((page_ref.num, Obj::Dict(page)));
    }
    for m in mutations {
        if let ReflowMutation::SetObject { num, value } = m {
            if objs.iter().any(|(n, _)| *n == *num) {
                return Err(ReflowEditError::Edit(EditError::Invalid(format!(
                    "object {num} is redefined twice by the reflow"
                ))));
            }
            objs.push((*num, value.clone()));
        }
    }
    let object_mutations: Vec<ObjectMutation> = objs
        .into_iter()
        .map(|(num, value)| ObjectMutation::SetObject { num, value })
        .collect();
    let objects_changed = object_mutations.len();
    let (out, _save) = incremental::save_incremental(src, &model, &object_mutations, budget, g)
        .map_err(|e| {
            ReflowEditError::Edit(EditError::Unresolvable(format!(
                "cannot write the reflow update: {e}"
            )))
        })?;

    let bytes_in = u64::try_from(src.len()).unwrap_or(u64::MAX);
    let bytes_out = u64::try_from(out.len()).unwrap_or(u64::MAX);
    Ok((
        out,
        ReflowEditReport {
            page: page_index,
            bytes_in,
            bytes_out,
            changed_regions: regions.iter().map(|r| r.span.clone()).collect(),
            objects_changed,
        },
    ))
}
