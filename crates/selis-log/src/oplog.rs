//! The document operation log (01-ARCHITECTURE.md §11).
//!
//! A per-document, user-facing record of every mutation: what changed, when, by
//! whom, and which revision it produced. Distinct from [`crate::log`] in both
//! audience and lifetime — this one is persisted in the `.selis` session bundle
//! and is what answers *"what did this tool do to my file?"*.
//!
//! It is also the mechanism `SL-1.ENC.04` requires: a permission-bit override is
//! only defensible if it is recorded, and this is where it is recorded.
//!
//! # What it is not
//!
//! Not a debug log. It contains no byte offsets, no stack context, and no
//! engine internals. It contains the *user's* view of what happened.

use alloc::string::String;
use alloc::vec::Vec;

/// Who performed an operation.
///
/// A name, not an identity claim: this exists for collaboration attribution and
/// audit readability, and it is supplied by the shell.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ActorId(pub String);

impl ActorId {
    /// The local user, unnamed. The default for a single-user session.
    #[must_use]
    pub fn local() -> Self {
        ActorId(String::from("local"))
    }
}

/// How an operation ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// It succeeded, producing the named revision (`None` for an in-memory-only
    /// change that has not been saved).
    Ok {
        /// The revision index this operation produced, once saved.
        revision: Option<u32>,
    },
    /// It failed. The code is a [`selis_error`]-registry number, so the reason is
    /// looked up rather than restated here.
    Failed {
        /// The numeric error code.
        code: u32,
    },
    /// It was refused by policy — a permission bit, an entitlement, or consent.
    Refused {
        /// The numeric error code.
        code: u32,
    },
    /// It was undone.
    Undone,
}

/// One recorded operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpRecord {
    /// A stable, monotonically increasing sequence number within the document.
    pub seq: u64,
    /// The operation, e.g. `"rotate-page"`, `"merge"`, `"remove-encryption"`.
    ///
    /// `&'static str` so the vocabulary is closed and greppable, and so a
    /// document cannot inject an operation name.
    pub op: &'static str,
    /// A human-readable summary. Engine-authored; safe to show a user.
    pub summary: String,
    /// When, in nanoseconds from the injected clock's epoch.
    pub at: u64,
    /// Who.
    pub actor: ActorId,
    /// How it ended.
    pub outcome: Outcome,
    /// Whether this operation deliberately overrode a document restriction.
    ///
    /// `SL-1.ENC.04`: honouring `/P` bits by default and offering a *logged*
    /// override is the defensible middle. This is the log.
    pub override_used: Option<&'static str>,
}

/// The append-only operation log for one document.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OpLog {
    records: Vec<OpRecord>,
    next_seq: u64,
}

impl OpLog {
    /// An empty log.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            records: Vec::new(),
            next_seq: 0,
        }
    }

    /// Append a record, assigning its sequence number.
    ///
    /// Returns the assigned sequence number.
    pub fn record(
        &mut self,
        op: &'static str,
        summary: impl Into<String>,
        at: u64,
        actor: ActorId,
        outcome: Outcome,
    ) -> u64 {
        let seq = self.next_seq;
        self.next_seq = self.next_seq.saturating_add(1);
        self.records.push(OpRecord {
            seq,
            op,
            summary: summary.into(),
            at,
            actor,
            outcome,
            override_used: None,
        });
        seq
    }

    /// Append a record for an operation that overrode a document restriction.
    pub fn record_override(
        &mut self,
        op: &'static str,
        summary: impl Into<String>,
        at: u64,
        actor: ActorId,
        outcome: Outcome,
        override_used: &'static str,
    ) -> u64 {
        let seq = self.record(op, summary, at, actor, outcome);
        if let Some(last) = self.records.last_mut() {
            last.override_used = Some(override_used);
        }
        seq
    }

    /// Every record, oldest first.
    #[must_use]
    pub fn records(&self) -> &[OpRecord] {
        &self.records
    }

    /// How many operations are recorded.
    #[must_use]
    pub fn len(&self) -> usize {
        self.records.len()
    }

    /// Whether nothing has been recorded.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.records.is_empty()
    }

    /// Mark a recorded operation as undone.
    ///
    /// Returns `false` if `seq` is not in the log. Note that undoing does not
    /// *remove* the record: `23-EDIT-MODEL-SPEC.md §5` requires that a discarded
    /// redo branch stays answerable, so history is only ever appended to.
    #[must_use = "a false return means the sequence number was not found"]
    pub fn mark_undone(&mut self, seq: u64) -> bool {
        match self.records.iter_mut().find(|r| r.seq == seq) {
            Some(r) => {
                r.outcome = Outcome::Undone;
                true
            }
            None => false,
        }
    }

    /// Every operation that used an override. The audit view an enterprise buyer
    /// actually asks for.
    #[must_use]
    pub fn overrides(&self) -> Vec<&OpRecord> {
        self.records
            .iter()
            .filter(|r| r.override_used.is_some())
            .collect()
    }

    /// A plain-text rendering, newest last. What the "document history" panel and
    /// `selis inspect --history` both show.
    #[must_use]
    pub fn to_text(&self) -> String {
        use core::fmt::Write as _;
        let mut s = String::new();
        for r in &self.records {
            let status = match &r.outcome {
                Outcome::Ok { revision: Some(n) } => alloc::format!("ok (revision {n})"),
                Outcome::Ok { revision: None } => String::from("ok (unsaved)"),
                Outcome::Failed { code } => alloc::format!("failed (E{code})"),
                Outcome::Refused { code } => alloc::format!("refused (E{code})"),
                Outcome::Undone => String::from("undone"),
            };
            let _ = write!(s, "{:>4}  {:<22} {} — {}", r.seq, r.op, r.summary, status);
            if let Some(o) = r.override_used {
                let _ = write!(s, " [override: {o}]");
            }
            s.push('\n');
        }
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sequence_numbers_are_monotonic() {
        let mut log = OpLog::new();
        let a = log.record("rotate-page", "page 3 rotated 90°", 0, ActorId::local(), Outcome::Ok { revision: None });
        let b = log.record("save", "incremental update appended", 1, ActorId::local(), Outcome::Ok { revision: Some(2) });
        assert_eq!(a, 0);
        assert_eq!(b, 1);
        assert_eq!(log.len(), 2);
    }

    /// SL-1.ENC.04: the override must be *recorded*, and it must be findable.
    #[test]
    fn overrides_are_recorded_and_queryable() {
        let mut log = OpLog::new();
        log.record("merge", "3 documents merged", 0, ActorId::local(), Outcome::Ok { revision: None });
        log.record_override(
            "clear-permissions",
            "print and copy restrictions cleared",
            1,
            ActorId::local(),
            Outcome::Ok { revision: None },
            "owner-password-permission-bits",
        );
        assert_eq!(log.overrides().len(), 1);
        assert_eq!(
            log.overrides().first().and_then(|r| r.override_used),
            Some("owner-password-permission-bits")
        );
        assert!(log.to_text().contains("[override: owner-password-permission-bits]"));
    }

    /// 23-EDIT-MODEL-SPEC.md §5: undo does not truncate history.
    #[test]
    fn undo_marks_rather_than_removes() {
        let mut log = OpLog::new();
        let seq = log.record("add-page", "blank page inserted at 2", 0, ActorId::local(), Outcome::Ok { revision: None });
        assert!(log.mark_undone(seq));
        assert_eq!(log.len(), 1, "history is append-only");
        assert_eq!(log.records().first().map(|r| &r.outcome), Some(&Outcome::Undone));
        assert!(!log.mark_undone(999));
    }

    #[test]
    fn refusals_carry_a_registry_code_not_a_restated_reason() {
        let mut log = OpLog::new();
        log.record("edit-text", "attempted", 0, ActorId::local(), Outcome::Refused { code: 4300 });
        assert!(log.to_text().contains("refused (E4300)"));
    }
}
