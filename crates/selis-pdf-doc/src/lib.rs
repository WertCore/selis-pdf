//! `selis-pdf-doc` — the document model (SL-1.DOC.*).
//!
//! The layer above COS: catalog, page tree with inherited attributes,
//! cycle-safe resolution, outlines, optional content, and metadata
//! (01-ARCHITECTURE.md §3). Phase 1 delivers the catalog + page-tree walk
//! (SL-1.DOC.01); the rest land in SL-1.DOC.02–09.

#![forbid(unsafe_code)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

mod doc;
mod doc_extra;
mod metadata;
mod resolve;
mod tree;

pub use doc::{Document, Page};
pub use doc_extra::{article_threads, page_labels, Bead, PageLabel, ViewerPreferences};
pub use metadata::{FieldValue, Metadata};
pub use resolve::Resolver;
pub use tree::{
    parse_destination, walk_name_tree, walk_number_tree, Destination, NameTree, NumberTree,
};
