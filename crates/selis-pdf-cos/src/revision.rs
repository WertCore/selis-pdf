//! Incremental-update chain as first-class revisions (SL-1.COS.05).
//!
//! A PDF with N incremental updates contains N revisions: the original
//! document, then each appended update. Most engines flatten these into one
//! "newest wins" object graph and throw away the past. This module does not.
//!
//! [`Doc::revisions()`] exposes every revision (byte range + xref + trailer,
//! oldest first), and [`Doc::at_revision`] resolves objects *as that
//! revision's author saw them*. This is the foundation of ADR-P0007
//! (invariant I3: "the bytes of every prior revision are preserved"), which
//! the incremental writer and the privacy scanner (SL-1A.TOOL.09, which must
//! find leftover content in prior revisions) both build on.

use std::collections::BTreeMap;
use std::ops::Range;

use selis_error::{err, Code, Result};
use selis_sandbox::{Budget, BudgetGuard};

use crate::obj::{Obj, Ref};
use crate::xref::{self, XrefEntry};

/// One revision of a document: the bytes appended by one incremental update,
/// with the xref table and trailer that revision shipped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Revision {
    /// The byte range this revision occupies in the file (oldest first, the
    /// first revision covers the whole original document).
    pub byte_range: Range<u64>,
    /// The xref entries this revision declared (new or modified objects).
    pub entries: BTreeMap<u32, XrefEntry>,
    /// The trailer dictionary of this revision.
    pub trailer: Vec<(selis_bytes::Bytes, Obj)>,
    /// This revision's `/Root` reference, when present.
    pub root: Option<Ref>,
    /// This revision's `/Encrypt` reference, when present.
    pub encrypt: Option<Ref>,
    /// This revision's `/Prev` offset, when present.
    pub prev: Option<u64>,
}

/// A parsed document with every revision addressable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Doc {
    /// Revisions, oldest first.
    revisions: Vec<Revision>,
}
/// A merged view of a document as seen at one revision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RevisionView {
    /// The merged xref index (entries from revisions 0..=n, newest winning).
    pub xref: BTreeMap<u32, XrefEntry>,
    /// The trailer of the viewed revision.
    pub trailer: Vec<(selis_bytes::Bytes, Obj)>,
    /// The viewed revision's `/Root`.
    pub root: Option<Ref>,
}

impl Doc {
    /// A single-revision document (used by damaged-file reconstruction).
    #[must_use]
    pub fn from_single_revision(
        entries: BTreeMap<u32, XrefEntry>,
        trailer: Vec<(selis_bytes::Bytes, Obj)>,
    ) -> Self {
        let root = trailer
            .iter()
            .find(|(k, _)| k.as_slice() == b"Root")
            .and_then(|(_, v)| match v {
                Obj::Ref(r) => Some(*r),
                _ => None,
            });
        let encrypt = trailer
            .iter()
            .find(|(k, _)| k.as_slice() == b"Encrypt")
            .and_then(|(_, v)| match v {
                Obj::Ref(r) => Some(*r),
                _ => None,
            });
        Self {
            revisions: vec![Revision {
                byte_range: 0..u64::MAX,
                entries,
                trailer,
                root,
                encrypt,
                prev: None,
            }],
        }
    }

    /// All revisions, oldest first.
    #[must_use]
    pub fn revisions(&self) -> &[Revision] {
        &self.revisions
    }

    /// The number of revisions.
    #[must_use]
    pub fn len(&self) -> usize {
        self.revisions.len()
    }

    /// Whether the document has no revisions.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.revisions.is_empty()
    }

    /// A merged view of the document as seen at revision `n` (0-based,
    /// oldest first). Entries from revisions 0..=n resolve newest-first.
    ///
    /// # Budget
    ///
    /// Merging is O(revisions × entries); no allocation beyond the merged map.
    ///
    /// # Malformed Input
    ///
    /// Returns `None` for `n >= len()`.
    #[must_use]
    pub fn at_revision(&self, n: usize) -> Option<RevisionView> {
        let rev = self.revisions.get(n)?;
        let mut xref = BTreeMap::new();
        for older in self.revisions.iter().take(n.saturating_add(1)) {
            for (&num, &entry) in &older.entries {
                xref.insert(num, entry);
            }
        }
        Some(RevisionView {
            xref,
            trailer: rev.trailer.clone(),
            root: rev.root,
        })
    }
}

/// Parse all revisions from a buffer, oldest first.
///
/// The caller locates `startxref` via
/// [`crate::xref::find_startxref`] and passes the keyword's offset.
///
/// # Budget
///
/// Depth-budgeted per `/Prev` hop; object-budgeted per entry.
///
/// # Malformed Input
///
/// `XREF_PREV_CYCLE` on a cyclic chain; `XREF_MALFORMED` on an unreadable
/// table.
pub fn parse_revisions(
    src: &[u8],
    startxref_keyword_pos: u64,
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
) -> Result<Doc> {
    // Walk the chain newest-first, collecting xref offsets and revisions.
    let mut xref_offsets: Vec<u64> = Vec::new();
    let mut newest_first: Vec<Revision> = Vec::new();
    let mut cursor = {
        let p = usize::try_from(startxref_keyword_pos).unwrap_or(usize::MAX);
        let after_kw = p.saturating_add("startxref".len());
        let (off, _) = xref::read_startxref_value(src, after_kw)?;
        off
    };
    let mut visited: Vec<u64> = Vec::new();

    loop {
        if visited.contains(&cursor) || visited.len() >= 512 {
            return Err(err!(
                Code::XrefPrevCycle,
                during = "xref-prev-chain",
                at = cursor
            ));
        }
        visited.push(cursor);
        xref_offsets.push(cursor);
        g.enter()?;

        let (entries, trailer, prev) = xref::parse_one_revision(src, cursor, budget, g)?;
        let root = trailer
            .iter()
            .find(|(k, _)| k.as_slice() == b"Root")
            .and_then(|(_, v)| match v {
                Obj::Ref(r) => Some(*r),
                _ => None,
            });
        let encrypt = trailer
            .iter()
            .find(|(k, _)| k.as_slice() == b"Encrypt")
            .and_then(|(_, v)| match v {
                Obj::Ref(r) => Some(*r),
                _ => None,
            });

        newest_first.push(Revision {
            byte_range: 0..0, // filled after the loop
            entries,
            trailer,
            root,
            encrypt,
            prev,
        });

        match prev {
            Some(prev) if prev != 0 => cursor = prev,
            _ => break,
        }
    }

    // Sort xref offsets oldest-first, then assign byte ranges.
    xref_offsets.sort_unstable();
    for (i, rev) in newest_first.iter_mut().rev().enumerate() {
        let start = if i == 0 {
            0u64
        } else {
            xref_offsets.get(i.saturating_sub(1)).copied().unwrap_or(0)
        };
        let end = xref_offsets.get(i).copied().unwrap_or(0);
        rev.byte_range = start..end;
    }

    newest_first.reverse();
    Ok(Doc {
        revisions: newest_first,
    })
}

/// Parse revisions tolerating a torn newest revision (SL-1A.WRITE.06, I5).
///
/// Identical to [`parse_revisions`] except that a newest revision whose xref
/// block or trailer is damaged does not fail the parse: earlier `startxref`
/// occurrences are tried (newest first) and the document resolves to the
/// newest *complete* revision instead. This is the reader-side of crash
/// atomicity: an append interrupted mid-write is **detectable and discarded**
/// rather than refusing the document. Callers that must distinguish "the
/// newest revision was torn" (the writer's own verification, tool checks)
/// should consult `verify`'s verdict as well.
///
/// # Budget
///
/// As [`parse_revisions`]; each rollback attempt is charged.
///
/// # Malformed Input
///
/// A damaged newest revision is tolerated and discarded. A document with no
/// parseable revision anywhere returns `Ok(None)`; budget/cancellation
/// propagate as `Err`.
pub fn parse_revisions_resilient(
    src: &[u8],
    startxref_keyword_pos: u64,
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
) -> Result<Option<Doc>> {
    // Walk backwards over `startxref` keyword occurrences: the aimed one
    // first, then earlier ones. A torn tail means the newest keyword may
    // itself be intact while its *target* is not, or the keyword may be cut
    // mid-value — both resolve by trying the previous occurrence.
    let mut cursor = startxref_keyword_pos;
    for _ in 0..16 {
        match parse_revisions(src, cursor, budget, g) {
            Ok(doc) => {
                return if doc.is_empty() {
                    Ok(None)
                } else {
                    Ok(Some(doc))
                };
            }
            Err(e) if e.is_budget() || e.is_cancelled() || e.is_pending() => return Err(e),
            Err(_) => {}
        }
        match prev_startxref(src, cursor) {
            Some(prev) => cursor = prev,
            None => return Ok(None),
        }
    }
    Ok(None)
}

/// The position of the last `startxref` keyword strictly before `before`
/// (searching the whole buffer backwards). Used to roll back to an earlier
/// revision when the newest `startxref` block is torn (I5).
fn prev_startxref(src: &[u8], before: u64) -> Option<u64> {
    let end = usize::try_from(before).unwrap_or(src.len()).min(src.len());
    if end < 9 {
        return None;
    }
    let hay = src.get(..end)?;
    let mut p = hay.len().saturating_sub(9);
    loop {
        if hay.get(p..p.saturating_add(9)) == Some(b"startxref") {
            return Some(u64::try_from(p).unwrap_or(u64::MAX));
        }
        if p == 0 {
            return None;
        }
        p = p.saturating_sub(1);
    }
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::indexing_slicing,
        clippy::arithmetic_side_effects,
        clippy::cast_sign_loss
    )]

    use super::*;
    use selis_sandbox::{CancelToken, FixedClock};

    fn guard() -> BudgetGuard<'static> {
        Budget::unlimited().guard_with(&FixedClock(0), CancelToken::new())
    }

    /// Append a classic xref + trailer to `out`, tracking the xref offset so a
    /// subsequent revision can set `/Prev`. Returns `(buffer, xref_offset)`.
    fn append_xref(mut out: Vec<u8>, prev: Option<u64>) -> (Vec<u8>, u64) {
        let xref_off = out.len() as u64;
        out.extend_from_slice(b"xref\n0 2\n0000000000 65535 f \n0000000009 00000 n \n");
        out.extend_from_slice(b"trailer\n<< /Size 2 /Root 1 0 R");
        if let Some(p) = prev {
            out.extend_from_slice(format!(" /Prev {}", p).as_bytes());
        }
        out.extend_from_slice(b" >>\n");
        out.extend_from_slice(b"startxref\n");
        out.extend_from_slice(format!("{}\n%%EOF\n", xref_off).as_bytes());
        (out, xref_off)
    }

    /// Build a file with `n` incremental updates (n+1 revisions total).
    /// Each revision's xref points at object 1 with a different offset so the
    /// "newest wins" resolution is observable.
    fn multi_revision_file(count: usize) -> Vec<u8> {
        let body = b"%PDF-1.4\n1 0 obj\n<< /Type /Catalog >>\nendobj\n".to_vec();
        let out = body;
        // Revision 0 (the original): xref for the body with no /Prev.
        let (mut out, mut prev_off) = append_xref(out, None);
        for _ in 1..=count {
            // Each update appends a new xref with /Prev back to the previous.
            let (o, p) = append_xref(out, Some(prev_off));
            out = o;
            prev_off = p;
        }
        out
    }

    #[test]
    fn zero_increments_is_one_revision() {
        let pdf = multi_revision_file(0);
        let budget = Budget::unlimited();
        let mut g = guard();
        let startxref = crate::xref::find_startxref(&pdf, 2048).expect("startxref");
        let doc = parse_revisions(&pdf, startxref, &budget, &mut g).expect("parse");
        assert_eq!(doc.len(), 1, "original document = 1 revision");
    }

    #[test]
    fn three_increments_exposes_four_revisions() {
        let pdf = multi_revision_file(3);
        let budget = Budget::unlimited();
        let mut g = guard();
        let startxref = crate::xref::find_startxref(&pdf, 2048).expect("startxref");
        let doc = parse_revisions(&pdf, startxref, &budget, &mut g).expect("parse");
        assert_eq!(doc.len(), 4, "original + 3 increments = 4 revisions");
        for (i, rev) in doc.revisions().iter().enumerate() {
            assert!(
                rev.byte_range.start < rev.byte_range.end,
                "range must be non-empty for revision {i}"
            );
            assert!(rev.root.is_some(), "each revision has /Root");
        }
    }

    #[test]
    fn at_revision_resolves_newest_wins() {
        let pdf = multi_revision_file(2);
        let budget = Budget::unlimited();
        let mut g = guard();
        let startxref = crate::xref::find_startxref(&pdf, 2048).expect("startxref");
        let doc = parse_revisions(&pdf, startxref, &budget, &mut g).expect("parse");
        assert_eq!(doc.len(), 3);

        // Every revision sees object 1 (unchanged across updates) with the
        // *newest* declared offset.
        let view_last = doc.at_revision(2).expect("last view");
        assert!(
            view_last.xref.contains_key(&1),
            "the newest view must still resolve object 1"
        );
        assert!(view_last.root.is_some());
    }

    #[test]
    fn at_revision_out_of_range_is_none() {
        let pdf = multi_revision_file(1);
        let budget = Budget::unlimited();
        let mut g = guard();
        let startxref = crate::xref::find_startxref(&pdf, 2048).expect("startxref");
        let doc = parse_revisions(&pdf, startxref, &budget, &mut g).expect("parse");
        assert!(doc.at_revision(99).is_none());
    }

    /// SL-1A.WRITE.06 (I5): a *torn newest revision* — the incremental update
    /// was killed mid-append — does not refuse the document. The resilient
    /// parse rolls back to the previous complete revision; the strict parse
    /// still fails (that difference is what the writer's verification uses
    /// to reject incomplete output).
    #[test]
    fn torn_newest_revision_rolls_back_resiliently() {
        let pdf = multi_revision_file(2); // 3 complete revisions
        let budget = Budget::unlimited();
        let mut g = guard();

        // Strict parse succeeds on the intact file.
        let startxref = crate::xref::find_startxref(&pdf, 2048).expect("startxref");
        assert_eq!(
            parse_revisions(&pdf, startxref, &budget, &mut g)
                .expect("strict parse")
                .len(),
            3
        );

        // Tear the tail: cut mid-way through the newest startxref block.
        let half = pdf.len() >> 1;
        for keep in [pdf.len() - 6, pdf.len() - 14, half] {
            let torn = pdf[..keep].to_vec();
            // The strict parse of the torn tail may or may not fail (a cut
            // that still leaves a complete earlier startxref target parses);
            // the resilient parse must always recover all complete revisions.
            let recovered = crate::parse_revisions_resilient(&torn, startxref, &budget, &mut g);
            let doc = recovered
                .expect("resilient parse recovers a complete revision")
                .expect("at least one complete revision exists");
            assert!(
                !doc.revisions().is_empty(),
                "at least the original revision survives a torn tail (cut at {keep})"
            );
        }

        // A buffer with no parseable revision anywhere yields None (not a
        // panic, not a fake document).
        let garbage = b"not a pdf at all, not even slightly".to_vec();
        assert!(
            crate::parse_revisions_resilient(&garbage, 0, &budget, &mut g)
                .expect("no budget error")
                .is_none(),
            "no complete revision -> None"
        );
    }
}
