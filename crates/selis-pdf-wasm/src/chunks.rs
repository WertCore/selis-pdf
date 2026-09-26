//! Chunk manifest (SL-4.WASM.02).
//!
//! The WASM binding is split into a **core viewer bundle** (`selis-pdf-wasm`:
//! COS + render + text, ≤ 3 MB brotli) and five **lazily-loaded chunks**.
//! Each chunk is a separate `cdylib` (separate `.wasm` module, not dead code
//! in one binary). The JS loader fetches a chunk only when the document or
//! UI actually needs it; a viewer session that never edits never downloads the
//! editor bytes.

use serde::{Deserialize, Serialize};

/// Stable chunk identifiers (wire-stable, used in the manifest and the loader).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ChunkId {
    /// Core viewer bundle (`selis_pdf_wasm.wasm`): COS + `selis-pdf-content`
    /// + `selis-raster` + `selis-pdf-text` (SL-4.WASM.02 "core").
    Core,
    /// JPXDecode codec (OpenJPEG 2.5.3, Tier-2 sandbox).
    Jpx,
    /// CJK font payload + FONT.10/11 subsetting (WASM.07).
    Cjk,
    /// OCR / Tesseract (Phase 5).
    Ocr,
    /// PDF→Office/HTML/Markdown conversion (Phase 8, `selis-pdf-convert`).
    Convert,
    /// Editor: mutation journal + incremental save (`selis-pdf-edit`, Phase 5).
    Editor,
}

impl ChunkId {
    /// File stem for the linked wasm artifact (cargo normalises `-` to `_`).
    #[must_use]
    pub const fn artifact(self) -> &'static str {
        match self {
            ChunkId::Core => "selis_pdf_wasm",
            ChunkId::Jpx => "selis_pdf_wasm_jpx",
            ChunkId::Ocr => "selis_pdf_wasm_ocr",
            ChunkId::Convert => "selis_pdf_wasm_convert",
            ChunkId::Editor => "selis_pdf_wasm_editor",
            ChunkId::Cjk => "selis_pdf_wasm_cjk",
        }
    }

    /// Human-readable chunk name (matches `xtask/size-budgets.toml` `chunk`).
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            ChunkId::Core => "core",
            ChunkId::Jpx => "jpx",
            ChunkId::Cjk => "cjk",
            ChunkId::Ocr => "ocr",
            ChunkId::Convert => "convert",
            ChunkId::Editor => "editor",
        }
    }

    /// Whether the viewer session is allowed to fetch this chunk without an
    /// explicit user action. `false` for editor (WASM.02 DoD: viewer never
    /// downloads editor).
    #[must_use]
    pub const fn viewer_auto_load(self) -> bool {
        match self {
            ChunkId::Core => true,
            ChunkId::Jpx => true, // fetched lazily when a JPX stream is encountered
            ChunkId::Cjk => true, // fetched lazily when a CJK codepoint is shaped
            ChunkId::Ocr => false,
            ChunkId::Convert => false,
            ChunkId::Editor => false,
        }
    }
}

/// One manifest entry: which chunk, where it lives, its SRI and budget.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChunkEntry {
    /// Chunk identifier.
    pub id: ChunkId,
    /// Relative URL from the app shell (e.g. `wasm/selis_pdf_wasm_jpx.wasm`).
    pub url: String,
    /// Brotli size budget in bytes (from `xtask/size-budgets.toml`).
    pub budget: u64,
    /// Whether this chunk may be auto-fetched by the viewer (see `viewer_auto_load`).
    pub viewer_auto: bool,
    /// Optional SRI hash (populated at build time; `None` in tests).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub integrity: Option<String>,
}

/// The full chunk manifest emitted by the build and consumed by the loader.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Manifest {
    /// Manifest version (1).
    pub v: u32,
    /// Engine version that produced it.
    pub engine: String,
    /// All chunks (core first).
    pub chunks: Vec<ChunkEntry>,
}

impl Manifest {
    /// The canonical manifest for the 6-chunk split (core + 5 lazy).
    #[must_use]
    pub fn canonical() -> Self {
        let engine = env!("CARGO_PKG_VERSION").to_string();
        Self {
            v: 1,
            engine,
            chunks: vec![
                ChunkEntry {
                    id: ChunkId::Core,
                    url: "wasm/selis_pdf_wasm.wasm".to_string(),
                    budget: 3_000_000,
                    viewer_auto: true,
                    integrity: None,
                },
                ChunkEntry {
                    id: ChunkId::Jpx,
                    url: "wasm/selis_pdf_wasm_jpx.wasm".to_string(),
                    budget: 400_000,
                    viewer_auto: true,
                    integrity: None,
                },
                ChunkEntry {
                    id: ChunkId::Cjk,
                    url: "wasm/selis_pdf_wasm_cjk.wasm".to_string(),
                    budget: 500_000,
                    viewer_auto: true,
                    integrity: None,
                },
                ChunkEntry {
                    id: ChunkId::Ocr,
                    url: "wasm/selis_pdf_wasm_ocr.wasm".to_string(),
                    budget: 800_000,
                    viewer_auto: false,
                    integrity: None,
                },
                ChunkEntry {
                    id: ChunkId::Convert,
                    url: "wasm/selis_pdf_wasm_convert.wasm".to_string(),
                    budget: 600_000,
                    viewer_auto: false,
                    integrity: None,
                },
                ChunkEntry {
                    id: ChunkId::Editor,
                    url: "wasm/selis_pdf_wasm_editor.wasm".to_string(),
                    budget: 900_000,
                    viewer_auto: false,
                    integrity: None,
                },
            ],
        }
    }

    /// Returns `true` iff the viewer may fetch `id` without user action.
    #[must_use]
    pub fn viewer_may_load(&self, id: ChunkId) -> bool {
        self.chunks
            .iter()
            .find(|e| e.id == id)
            .is_some_and(|e| e.viewer_auto)
    }

    /// Validates the "viewer never downloads editor" invariant (WASM.02 DoD).
    ///
    /// # Errors
    /// `crate::chunks_viewer_isolation` when violated (never silently).
    pub fn validate_viewer_isolation(&self) -> Result<(), &'static str> {
        for e in &self.chunks {
            if e.id == ChunkId::Editor && e.viewer_auto {
                return Err("viewer must not auto-load the editor chunk (SL-4.WASM.02)");
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;

    #[test]
    fn manifest_viewer_never_downloads_editor() {
        let m = Manifest::canonical();
        assert!(m.validate_viewer_isolation().is_ok());
        assert!(!m.viewer_may_load(ChunkId::Editor));
        assert!(m.viewer_may_load(ChunkId::Core));
        assert!(m.viewer_may_load(ChunkId::Jpx));
    }

    #[test]
    fn chunk_ids_match_artifacts() {
        assert_eq!(ChunkId::Core.artifact(), "selis_pdf_wasm");
        assert_eq!(ChunkId::Jpx.artifact(), "selis_pdf_wasm_jpx");
        assert_eq!(ChunkId::Editor.artifact(), "selis_pdf_wasm_editor");
    }

    #[test]
    fn manifest_round_trips() {
        let m = Manifest::canonical();
        let json = serde_json::to_string(&m).expect("serialise");
        let back: Manifest = serde_json::from_str(&json).expect("deserialise");
        assert_eq!(m, back);
    }

    #[test]
    fn core_budget_is_3mb() {
        let m = Manifest::canonical();
        let core = m
            .chunks
            .iter()
            .find(|e| e.id == ChunkId::Core)
            .expect("core");
        assert_eq!(core.budget, 3_000_000);
    }
}
