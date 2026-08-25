//! `selis-pdf-doc` — the document model (SL-1.DOC.*).
//!
//! The layer above COS: catalog, page tree with inherited attributes,
//! cycle-safe resolution, outlines, optional content, and metadata
//! (01-ARCHITECTURE.md §3). Phase 1 delivers the catalog + page-tree walk
//! (SL-1.DOC.01); the rest land in SL-1.DOC.02–09.

#![forbid(unsafe_code)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

mod attachments;
mod conformance;
mod doc;
mod doc_extra;
mod metadata;
mod oc;
mod resolve;
mod struct_tree;
mod tree;

pub use attachments::{embedded_files, Attachment};
pub use conformance::{evaluate, registry, Area, EvaluationCtx, Level, Profile, Rule, RuleResult};
pub use doc::{Document, Page};
pub use doc_extra::{article_threads, page_labels, Bead, PageLabel, ViewerPreferences};
pub use metadata::{FieldValue, Metadata};
pub use oc::{OcConfig, OcGroup, OcMembership, OcProperties};
pub use resolve::Resolver;
pub use struct_tree::{StructElement, StructKid, StructTree};
pub use tree::{
    parse_destination, walk_name_tree, walk_number_tree, Destination, NameTree, NumberTree,
};
