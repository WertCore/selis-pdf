//! Text extraction and reading order (SL-3.TEXT.*).
//!
//! Groups the positioned glyphs from the content interpreter into runs, words,
//! and lines, orders them per the document's structure tree (or geometrically),
//! and provides the models for selection, search, and structured output.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]
#![forbid(unsafe_code)]

pub mod assembly;
pub mod docx;
pub mod epub;
pub mod export;
pub mod html;
pub mod order;
pub mod pptx;
pub mod quality;
pub mod reflow;
pub mod search;
pub mod selection;
pub mod table;
pub mod xlsx;
mod xml;

pub use docx::{to_docx, verify_docx, DOCX_MEDIA_TYPE};
pub use epub::{read_stored_zip, store_zip, to_epub, verify_epub};
pub mod text_layer;

pub use assembly::{
    apply_unicode_recovery, assemble, assemble_lines, assemble_runs, assemble_words, gather_glyphs,
    TextLine, TextRun, TextWord,
};
pub use export::{
    structured, to_json, to_markdown, to_text, to_text_from_line_texts, Line, Span, Structured,
    LOW_CONFIDENCE_MARKER,
};
pub use html::{
    read_positioned_html, to_positioned_html, verify_positioned_html, HtmlPage, ReadDocument,
    ReadPage, ReadSpan, MAX_COORDINATE_PX, MAX_POSITION_ERROR_PX,
};
pub use order::{order_lines, LineWithMcid, OrderResult, ReadingOrder};
pub use pptx::{slide_from_lines, to_pptx, verify_pptx, Slide, TextShape, PPTX_MEDIA_TYPE};
pub use quality::{page_separator, to_text_with_page_separators, ExtractionQuality, PageQuality};
pub use reflow::{
    escape_html, paragraphs, to_reflow_html, verify_reflow_html, ParagraphKind, ReflowPage,
    ReflowParagraph,
};
pub use search::{search, search_lines, SearchMatch};
pub use selection::{caret_at, glyph_rect, line_glyphs, quads, select_range, Caret, GlyphQuad};
pub use table::{
    tables_on_page, to_csv as tables_to_csv, Table, MIN_TABLE_COLUMNS, MIN_TABLE_ROWS,
};
pub use text_layer::{page_layer, LayerChar, LayerLine, PageTextLayer, TextDirection};
pub use xlsx::{to_xlsx, verify_xlsx, XLSX_MEDIA_TYPE};
