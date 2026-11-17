//! The mutation journal (SL-5.EDIT.01, 23-EDIT-MODEL-SPEC.md).
//!
//! The web workspace keeps byte snapshots so an undo is an exact restore, but
//! the spec's model is a typed journal: the in-memory document is
//! `materialise(source, journal)` and every change carries its own inverse.
//! This module is the engine-side foundation for that model — the types, the
//! apply/inverse rules, undo groups and coalescing, and the `.selis` bundle —
//! so the native CLI and SDK can materialise the same session the browser
//! editor produced.
//!
//! # Why document-level mutations
//!
//! The spec's full taxonomy (SetObject, ReplaceStream, InsertPage, …) needs an
//! object-level COS editor in this crate, which is a separate task (EDIT.03).
//! The mutations here are the document-level ones the tools actually emit:
//! a whole-file replacement with its before and after bytes. Their inverse is
//! total, their blast radius is the document, and materialisation is exact —
//! model equality (I6) holds because the bytes are preserved verbatim.

use selis_bytes::Bytes;

/// The version of the serialised session bundle.
pub const SESSION_SCHEMA: &str = "selis-session/1";

/// A monotonic identity for one journal entry.
pub type MutationId = u64;

/// Nanoseconds from the caller's clock.
pub type Nanos = u64;

/// Who made a change. The local reader is the only actor today.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActorId(pub String);

impl ActorId {
    /// The local reader.
    #[must_use]
    pub fn local() -> Self {
        Self("local".to_string())
    }
}

/// A typed change to the document, with everything needed to invert it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Mutation {
    /// Replace the whole document. The inverse swaps `before` and `after`.
    ReplaceDocument {
        /// The engine operation that produced the new state.
        op: String,
        /// The before bytes.
        before: Bytes,
        /// The after bytes.
        after: Bytes,
    },
}

impl Mutation {
    /// The mutation that undoes this one. Total for every variant today.
    #[must_use]
    pub fn inverse(&self) -> Self {
        match self {
            Self::ReplaceDocument { op, before, after } => Self::ReplaceDocument {
                op: op.clone(),
                before: after.clone(),
                after: before.clone(),
            },
        }
    }

    /// The operation's stable name, for the oplog.
    #[must_use]
    pub fn op(&self) -> &str {
        match self {
            Self::ReplaceDocument { op, .. } => op,
        }
    }

    /// Whether a mutation is reversible. All document-level ones are.
    #[must_use]
    pub const fn reversible(&self) -> bool {
        true
    }
}

/// A source fingerprint: size and a 64-bit FNV-1a hash prefix.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SourceFingerprint {
    /// The source's length in bytes.
    pub size: u64,
    /// The hash prefix.
    pub hash: u64,
}

/// Compute a source fingerprint.
///
/// # Budget
///
/// O(length of the input); no allocation.
///
/// # Malformed Input
///
/// Any bytes are a valid fingerprint input.
#[must_use]
pub fn fingerprint(bytes: &[u8]) -> SourceFingerprint {
    let mut hash = 0x811c9dc5u64;
    for &byte in bytes {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x01000193);
    }
    SourceFingerprint {
        size: bytes.len() as u64,
        hash,
    }
}

/// How undo coalesces consecutive mutations into one group.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Coalesce {
    /// Every mutation is its own undo group.
    Never,
    /// Consecutive typing within the idle window (milliseconds) is one group.
    Typing {
        /// The idle window in milliseconds.
        idle: u64,
    },
    /// A drag is one group.
    Dragging,
    /// An explicitly grouped gesture.
    Explicit,
}

/// One append-only entry of the journal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JournalEntry {
    /// The entry's id.
    pub id: MutationId,
    /// The change.
    pub mutation: Mutation,
    /// Its inverse.
    pub inverse: Mutation,
    /// When it was recorded.
    pub at: Nanos,
    /// Who recorded it.
    pub actor: ActorId,
    /// A human-readable label.
    pub label: String,
    /// The undo group this entry belongs to.
    pub group: usize,
}

/// A group of mutations that undo as one gesture.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UndoGroup {
    /// The group's label.
    pub label: String,
    /// Half-open range of entries.
    pub entries: std::ops::Range<usize>,
    /// The group's coalescing rule.
    pub coalescing: Coalesce,
}

/// The error a journal refuses to materialise a changed source with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SourceChanged;

impl std::fmt::Display for SourceChanged {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("the source document changed while the session was open")
    }
}

impl std::error::Error for SourceChanged {}

/// The mutation journal (append-only).
#[derive(Debug, Clone)]
pub struct Journal {
    /// The source the journal materialises over.
    pub source: SourceFingerprint,
    /// All entries, including a discarded redo branch.
    pub entries: Vec<JournalEntry>,
    /// The undo groups.
    pub groups: Vec<UndoGroup>,
    cursor: isize,
    next_id: MutationId,
}

impl Journal {
    /// Start a journal over `source`.
    ///
    /// # Budget
    ///
    /// One hash pass over the source; nothing else is allocated.
    ///
    /// # Malformed Input
    ///
    /// Any bytes are a valid source.
    #[must_use]
    pub fn new(source: &[u8]) -> Self {
        Self {
            source: fingerprint(source),
            entries: Vec::new(),
            groups: Vec::new(),
            cursor: -1,
            next_id: 1,
        }
    }

    /// The index of the last applied entry, or -1 at the source.
    #[must_use]
    pub const fn position(&self) -> isize {
        self.cursor
    }

    /// Whether there is a group to undo.
    #[must_use]
    pub const fn can_undo(&self) -> bool {
        self.cursor >= 0
    }

    /// Whether there is a discarded group to redo.
    #[must_use]
    pub fn can_redo(&self) -> bool {
        self.cursor.saturating_add(1) < self.entries.len() as isize
    }

    /// Rebuild the document from the source and the applied entries (I4).
    ///
    /// # Budget
    ///
    /// One fingerprint pass and one clone of the current bytes.
    ///
    /// # Malformed Input
    ///
    /// A source that no longer matches the fingerprint is refused with
    /// [`SourceChanged`] rather than silently reapplied.
    ///
    /// # Errors
    ///
    /// [`SourceChanged`] when `source` no longer matches the fingerprint.
    pub fn materialise(&self, source: &[u8]) -> Result<Bytes, SourceChanged> {
        if fingerprint(source) != self.source {
            return Err(SourceChanged);
        }
        let entry = usize::try_from(self.cursor)
            .ok()
            .and_then(|i| self.entries.get(i));
        Ok(entry.map_or_else(
            || Bytes::copy_from_slice(source),
            |entry| match &entry.mutation {
                Mutation::ReplaceDocument { after, .. } => after.clone(),
            },
        ))
    }

    /// Append a change and move the cursor to it. Returns the new entry id.
    pub fn apply(
        &mut self,
        mutation: Mutation,
        label: impl Into<String>,
        at: Nanos,
        actor: ActorId,
        coalescing: Coalesce,
    ) -> MutationId {
        let last_index = self.groups.len().saturating_sub(1);
        let can_coalesce = self.groups.last().is_some_and(|last| {
            last.entries.end == self.entries.len()
                && last.coalescing == coalescing
                && match coalescing {
                    Coalesce::Dragging | Coalesce::Explicit => true,
                    Coalesce::Typing { idle } => {
                        let last_at = self
                            .entries
                            .iter()
                            .rev()
                            .find(|entry| entry.group == last_index)
                            .map_or(0, |entry| entry.at);
                        at.saturating_sub(last_at) <= idle
                    }
                    Coalesce::Never => false,
                }
        });
        if can_coalesce {
            if let Some(group) = self.groups.get_mut(last_index) {
                group.entries.end = group.entries.end.saturating_add(1);
            }
        } else {
            self.groups.push(UndoGroup {
                label: label.into(),
                entries: self.entries.len()..self.entries.len().saturating_add(1),
                coalescing,
            });
        }
        // A new change discards the redo tail; the oplog keeps the memory.
        if self.cursor.saturating_add(1) < self.entries.len() as isize {
            let keep = usize::try_from(self.cursor.saturating_add(1)).unwrap_or(0);
            self.entries.truncate(keep);
            self.groups.retain(|group| group.entries.start < keep);
        }
        let group = if can_coalesce {
            last_index
        } else {
            self.groups.len().saturating_sub(1)
        };
        let id = self.next_id;
        self.next_id = self.next_id.saturating_add(1);
        self.entries.push(JournalEntry {
            id,
            inverse: mutation.inverse(),
            mutation,
            at,
            actor,
            label: self
                .groups
                .get(group)
                .map_or_else(String::new, |g| g.label.clone()),
            group,
        });
        self.cursor = (self.entries.len() as isize).saturating_sub(1);
        id
    }

    /// Undo one group, returning the restored bytes.
    ///
    /// # Budget
    ///
    /// One fingerprint pass and a scan of one undo group.
    ///
    /// # Malformed Input
    ///
    /// A changed source is refused with [`SourceChanged`].
    ///
    /// # Errors
    ///
    /// [`SourceChanged`] when the source no longer matches.
    pub fn undo(&mut self, source: &[u8]) -> Result<Bytes, SourceChanged> {
        let Some(current) = usize::try_from(self.cursor)
            .ok()
            .and_then(|i| self.entries.get(i).cloned())
        else {
            return self.materialise(source);
        };
        let group = current.group;
        let state = self
            .entries
            .iter()
            .enumerate()
            .rev()
            .filter(|(_, entry)| entry.group == group)
            .find_map(|(_, entry)| match &entry.inverse {
                Mutation::ReplaceDocument { after, .. } => Some(after.clone()),
            })
            .unwrap_or_else(|| Bytes::copy_from_slice(source));
        self.cursor = self
            .entries
            .iter()
            .position(|entry| entry.group == group)
            .map_or(-1, |index| (index as isize).saturating_sub(1));
        Ok(state)
    }

    /// Redo one group, returning the restored bytes.
    ///
    /// # Budget
    ///
    /// One fingerprint pass and a scan of one undo group.
    ///
    /// # Malformed Input
    ///
    /// A changed source is refused with [`SourceChanged`].
    ///
    /// # Errors
    ///
    /// [`SourceChanged`] when the source no longer matches.
    pub fn redo(&mut self, source: &[u8]) -> Result<Bytes, SourceChanged> {
        let next = usize::try_from(self.cursor.saturating_add(1))
            .ok()
            .and_then(|i| self.entries.get(i).cloned());
        let Some(next) = next else {
            return self.materialise(source);
        };
        let group = next.group;
        let state = self
            .entries
            .iter()
            .filter(|entry| entry.group == group)
            .find_map(|entry| match &entry.mutation {
                Mutation::ReplaceDocument { after, .. } => Some(after.clone()),
            })
            .unwrap_or_else(|| Bytes::copy_from_slice(source));
        self.cursor = self
            .entries
            .iter()
            .rposition(|entry| entry.group == group)
            .map_or(-1, |index| index as isize);
        Ok(state)
    }
}

/// A serialisable entry of the `.selis` bundle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BundleEntry {
    /// The entry's id.
    pub id: MutationId,
    /// The change.
    pub mutation: Mutation,
    /// Its inverse.
    pub inverse: Mutation,
    /// When it was recorded.
    pub at: Nanos,
    /// Who recorded it.
    pub actor: ActorId,
    /// A human-readable label.
    pub label: String,
    /// The undo group.
    pub group: usize,
}

/// The serialisable `.selis` session bundle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionBundle {
    /// The schema tag.
    pub schema: String,
    /// The source fingerprint.
    pub source: SourceFingerprint,
    /// The journal entries.
    pub entries: Vec<BundleEntry>,
    /// The undo groups.
    pub groups: Vec<UndoGroup>,
    /// The cursor.
    pub cursor: isize,
}

/// The error a bundle with an unknown schema gets.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnknownSchema(pub String);

impl std::fmt::Display for UnknownSchema {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "unknown session bundle schema: {}", self.0)
    }
}

impl std::error::Error for UnknownSchema {}

impl Journal {
    /// Export the journal for persistent storage.
    #[must_use]
    pub fn to_bundle(&self) -> SessionBundle {
        SessionBundle {
            schema: SESSION_SCHEMA.to_string(),
            source: self.source,
            entries: self
                .entries
                .iter()
                .map(|entry| BundleEntry {
                    id: entry.id,
                    mutation: entry.mutation.clone(),
                    inverse: entry.inverse.clone(),
                    at: entry.at,
                    actor: entry.actor.clone(),
                    label: entry.label.clone(),
                    group: entry.group,
                })
                .collect(),
            groups: self.groups.clone(),
            cursor: self.cursor,
        }
    }

    /// Rebuild a journal from a bundle, revalidating the source (I4, §3).
    ///
    /// # Budget
    ///
    /// One fingerprint pass plus a clone of the bundle's entries.
    ///
    /// # Malformed Input
    ///
    /// An unknown schema or a changed source is refused; no journal is built.
    ///
    /// # Errors
    ///
    /// [`UnknownSchema`] or [`SourceChanged`].
    pub fn from_bundle(bundle: &SessionBundle, source: &[u8]) -> Result<Self, BundleError> {
        if bundle.schema != SESSION_SCHEMA {
            return Err(BundleError::Schema(UnknownSchema(bundle.schema.clone())));
        }
        if fingerprint(source) != bundle.source {
            return Err(BundleError::SourceChanged);
        }
        Ok(Self {
            source: bundle.source,
            entries: bundle
                .entries
                .iter()
                .map(|entry| JournalEntry {
                    id: entry.id,
                    mutation: entry.mutation.clone(),
                    inverse: entry.inverse.clone(),
                    at: entry.at,
                    actor: entry.actor.clone(),
                    label: entry.label.clone(),
                    group: entry.group,
                })
                .collect(),
            groups: bundle.groups.clone(),
            cursor: bundle.cursor,
            next_id: bundle
                .entries
                .iter()
                .map(|entry| entry.id.saturating_add(1))
                .max()
                .unwrap_or(1),
        })
    }
}

/// Why a bundle could not be loaded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BundleError {
    /// An unknown schema tag.
    Schema(UnknownSchema),
    /// The source changed.
    SourceChanged,
}

impl std::fmt::Display for BundleError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Schema(error) => error.fmt(f),
            Self::SourceChanged => SourceChanged.fmt(f),
        }
    }
}

impl std::error::Error for BundleError {}
