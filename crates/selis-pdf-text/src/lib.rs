//! Text extraction and reading order (SL-3.TEXT.*).
//!
//! Groups the positioned glyphs from the content interpreter into runs, words,
//! and lines, and provides the models for reading order, selection, search,
//! and structured output.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]
#![forbid(unsafe_code)]

pub mod assembly;

pub use assembly::{
    assemble, assemble_lines, assemble_runs, assemble_words, TextLine, TextRun, TextWord,
};
