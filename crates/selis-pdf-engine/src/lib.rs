//! The Selis PDF engine (SL-2.PERF.01, ADR-P0025).
//!
//! The single crate every shell asks for pages, display lists, and tiles.
//! The engine owns the caches (memory-budgeted LRU keyed by page/matrix/
//! params/revision, exact invalidation on edit) so no shell reimplements them.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]
#![forbid(unsafe_code)]

pub mod cache;
pub mod render;
pub mod session;

pub use cache::{matrix_id, CacheKey, LruCache};
pub use render::{render_display_list, render_display_list_with_stats, RenderStats};
pub use selis_raster::TinySkiaBackend;
pub use session::Session;
