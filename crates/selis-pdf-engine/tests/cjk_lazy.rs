//! SL-3.FONT.10 — the lazy-CJK engine contract, end to end.
//!
//! Proves on the engine side (with an in-memory [`CjkChunkSource`] standing
//! in for the web shell that arrives in SL-4.WASM.07) that:
//!
//! 1. an unembedded `Uni…UCS2…` Chinese document renders `.notdef` boxes
//!    for codes its chunks do not yet cover — and the render itself
//!    performs **zero** fetches (it cannot block on one);
//! 2. draining the returned `needs` queue through the injected source and
//!    repainting turns the `.notdef` boxes into real glyph outlines;
//! 3. the repainted page is byte-identical to the same render with the
//!    chunks pre-loaded — arrival order cannot change pixels (ADR-P0012);
//! 4. two renders over one resident state hash identically (determinism);
//! 5. the plain `render_page` path is unchanged (no set attached: the same
//!    document still paints nothing).

#![allow(clippy::panic, clippy::unwrap_used, clippy::expect_used)]
#![allow(clippy::arithmetic_side_effects, clippy::integer_division)]

use std::collections::BTreeMap;
use std::hash::{DefaultHasher, Hash as _, Hasher};

use selis_bytes::Bytes;
use selis_font::add_glyph;
use selis_font::cjk::build::build_set;
use selis_font::cjk::{CjkChunkSource, CjkFontSet};
use selis_font::OutlineCmd;
use selis_geom::Matrix;
use selis_pdf_cos::doc_writer::DocumentBuilder;
use selis_pdf_cos::{Obj, Ref};
use selis_pdf_engine::{CjkRenderOutcome, Session, TinySkiaBackend};
use selis_sandbox::{Budget, CancelToken, FixedClock, Surface};

const MINI_TTF: &[u8] = include_bytes!("../../../crates/selis-font/tests/fixtures/mini.ttf");

/// The document's shown codes: 一 (in the core via the frequency list),
/// a late ideograph (U+9BCA → the `ideographs-4` chunk), a Hangul syllable
/// (U+AC00 → the `hangul-1` chunk), and あ (core-static kana).
const CODES: [u32; 4] = [0x4E00, 0x9BCA, 0xAC00, 0x3042];

fn rect_outline() -> Vec<OutlineCmd> {
    vec![
        OutlineCmd::Move { x: 120.0, y: 80.0 },
        OutlineCmd::Line { x: 920.0, y: 80.0 },
        OutlineCmd::Line { x: 920.0, y: 920.0 },
        OutlineCmd::Line { x: 120.0, y: 920.0 },
        OutlineCmd::Close,
    ]
}

/// The built payload: the bundled core plus every chunk file.
struct Payload {
    core: Bytes,
    chunks: BTreeMap<&'static str, Bytes>,
}

/// The synthetic "Noto CJK" source (a `mini.ttf` with one rectangle glyph
/// merged at each shown code — the FONT.11 path) run through `build_set`.
fn build_payload() -> Payload {
    let mut g = Budget::unlimited().guard();
    let mut source = Bytes::copy_from_slice(MINI_TTF);
    for code in CODES {
        source = add_glyph(&source, code, &rect_outline(), 1000, &mut g)
            .unwrap()
            .expect("source font merges");
    }
    let built = build_set(&source, &[0x4E00], &mut g)
        .unwrap()
        .expect("TrueType source");
    assert!(
        built.core_codes >= 2,
        "core covers static ranges the code too"
    );
    let mut chunks = BTreeMap::new();
    for chunk in &built.chunks {
        chunks.insert(chunk.chunk.id, chunk.data.clone());
    }
    Payload {
        core: built.core,
        chunks,
    }
}

/// An in-memory stand-in for the shell's chunk loader/bundle cache.
struct MapSource {
    map: BTreeMap<&'static str, Bytes>,
    fetches: usize,
}

impl CjkChunkSource for MapSource {
    fn fetch(&mut self, chunk_id: &str) -> Option<Bytes> {
        self.fetches += 1;
        self.map.get(chunk_id).cloned()
    }
}

fn bytes(v: &[u8]) -> Bytes {
    Bytes::copy_from_slice(v)
}

fn name(v: &[u8]) -> Obj {
    Obj::Name(bytes(v))
}

fn pair(k: &[u8], v: Obj) -> (Bytes, Obj) {
    (bytes(k), v)
}

/// An unembedded Type0 font over `/UniGB-UCS2-H` (2-byte Unicode codes)
/// with a `/DW` of 1000 (full-width CJK advance) — the classic
/// STSong-Light-class document shape, minus the font program.
fn cjk_pdf() -> Vec<u8> {
    let budget = Budget::profile(Surface::Viewer);
    let mut g = budget.guard_with(&FixedClock(0), CancelToken::new());
    let mut builder = DocumentBuilder::new();
    let descendant = builder.allocate();
    builder.add_object(
        descendant,
        Obj::Dict(vec![
            pair(b"Type", name(b"Font")),
            pair(b"Subtype", name(b"CIDFontType2")),
            pair(b"BaseFont", name(b"STSong-Light")),
            (
                bytes(b"CIDSystemInfo"),
                Obj::Dict(vec![
                    pair(b"Registry", Obj::String(bytes(b"Adobe"))),
                    pair(b"Ordering", Obj::String(bytes(b"GB1"))),
                    pair(b"Supplement", Obj::Int(5)),
                ]),
            ),
            (
                bytes(b"FontDescriptor"),
                Obj::Dict(vec![
                    pair(b"FontName", name(b"STSong-Light")),
                    pair(b"Flags", Obj::Int(4)),
                    pair(b"ItalicAngle", Obj::Int(0)),
                    pair(b"Ascent", Obj::Int(880)),
                    pair(b"Descent", Obj::Int(-120)),
                ]),
            ),
            pair(b"CIDToGIDMap", name(b"Identity")),
            pair(b"DW", Obj::Int(1000)),
        ]),
    );
    let font = builder.allocate();
    builder.add_object(
        font,
        Obj::Dict(vec![
            pair(b"Type", name(b"Font")),
            pair(b"Subtype", name(b"Type0")),
            pair(b"BaseFont", name(b"STSong-Light")),
            pair(b"Encoding", name(b"UniGB-UCS2-H")),
            (
                bytes(b"DescendantFonts"),
                Obj::Array(vec![Obj::Ref(Ref::new(descendant, 0))]),
            ),
        ]),
    );
    let resources = builder.allocate();
    builder.add_object(
        resources,
        Obj::Dict(vec![(
            bytes(b"Font"),
            Obj::Dict(vec![pair(b"F1", Obj::Ref(Ref::new(font, 0)))]),
        )]),
    );
    let shown: String = CODES.iter().map(|c| format!("{c:04X}")).collect();
    let content = format!("BT /F1 24 Tf 72 700 Td <{shown}> Tj ET\n");
    let content_num = builder.allocate();
    builder.add_object(
        content_num,
        Obj::Stream {
            dict: vec![pair(
                b"Length",
                Obj::Int(i64::try_from(content.len()).unwrap_or(i64::MAX)),
            )],
            data: bytes(content.as_bytes()),
        },
    );
    builder.add_page_with(
        612.0,
        792.0,
        &[Ref::new(content_num, 0)],
        Some(Ref::new(resources, 0)),
    );
    builder.write(&budget, &mut g).expect("write")
}

/// The observable result of one raster pass.
struct Pass {
    outcome: CjkRenderOutcome,
    hash: u64,
    /// Dark (ink-covered) pixels: both `.notdef` boxes and glyph outlines
    /// paint black on the white page backdrop.
    inked: usize,
}

fn summarize(backend: &TinySkiaBackend) -> (u64, usize) {
    let data = backend.pixmap().data();
    let mut h = DefaultHasher::new();
    data.hash(&mut h);
    let inked = data
        .chunks(4)
        .filter(|px| px.first().is_some_and(|r| *r < 128))
        .count();
    (h.finish(), inked)
}

fn render_cjk(session: &Session, budget: &Budget, cjk: &mut CjkFontSet) -> Pass {
    let mut g = budget.guard_with(&FixedClock(0), CancelToken::new());
    let mut backend = TinySkiaBackend::new(612, 792).expect("pixmap");
    let outcome = session
        .render_page_cjk(0, &mut backend, Matrix::IDENTITY, budget, &mut g, cjk)
        .expect("render");
    let (hash, inked) = summarize(&backend);
    Pass {
        outcome,
        hash,
        inked,
    }
}

fn render_plain(session: &Session, budget: &Budget) -> (u64, usize) {
    let mut g = budget.guard_with(&FixedClock(0), CancelToken::new());
    let mut backend = TinySkiaBackend::new(612, 792).expect("pixmap");
    session
        .render_page(0, &mut backend, Matrix::IDENTITY, budget, &mut g)
        .expect("render");
    summarize(&backend)
}

/// The DoD sequence on the engine side: notdef → fetch (in-memory) →
/// repaint, with the render never touching the source.
#[test]
fn lazy_cjk_renders_notdef_then_repaints_after_in_memory_fetch() {
    let payload = build_payload();
    let mut set = CjkFontSet::new(payload.core.clone());
    let budget = Budget::profile(Surface::Viewer);
    let session = Session::open(cjk_pdf(), &budget, &FixedClock(0)).expect("open");

    let mut source = MapSource {
        map: payload.chunks.clone(),
        fetches: 0,
    };

    // Pass 1: the two chunk-gated codes queue their chunks; nothing is
    // fetched and the `.notdef` boxes still paint ink.
    let pass1 = render_cjk(&session, &budget, &mut set);
    assert_eq!(source.fetches, 0, "the render must not touch the source");
    assert_eq!(
        pass1.outcome.needs,
        vec!["ideographs-4", "hangul-1"],
        "needs in deterministic chunk-table order"
    );
    assert_eq!(pass1.outcome.revision, 0, "nothing adopted yet");
    assert!(
        pass1.inked > 50,
        "notdef boxes are painted, got {}",
        pass1.inked
    );

    // The queue is sticky: the same unresolved state re-reports the same
    // needs (no re-fetch storms, no silent drops).
    let pass1b = render_cjk(&session, &budget, &mut set);
    assert_eq!(
        pass1b.outcome.needs,
        vec!["ideographs-4", "hangul-1"],
        "the queue survives a repaint"
    );
    assert_eq!(pass1b.hash, pass1.hash, "notdef pass is deterministic");

    // The shell fetches (synchronously here; the WASM shell will do it via
    // its own async loader in SL-4.WASM.07) and provides through the queue.
    let adopted = set.drain_requested(&mut source);
    assert_eq!(adopted, 2);
    assert_eq!(source.fetches, 2, "one fetch per needed chunk");
    assert_eq!(set.revision(), 2, "invalidation revision bumped per chunk");

    // Pass 2 (repaint): chunks resident → real glyph outlines, needs empty.
    let pass2 = render_cjk(&session, &budget, &mut set);
    assert!(pass2.outcome.needs.is_empty());
    assert_ne!(pass1.hash, pass2.hash, "repaint must change pixels");
    assert!(pass2.inked > pass1.inked, "glyphs ink more than the boxes");

    // Pass 3: two renders over one resident state are byte-identical.
    let pass3 = render_cjk(&session, &budget, &mut set);
    assert!(pass3.outcome.needs.is_empty());
    assert_eq!(pass2.hash, pass3.hash, "determinism over one resident set");
}

/// Pixels depend only on the resident set — not the order (or timing) the
/// chunks arrived (ADR-P0012 across fetch schedules).
#[test]
fn arrival_order_does_not_change_pixels() {
    let payload = build_payload();
    let budget = Budget::profile(Surface::Viewer);
    let session = Session::open(cjk_pdf(), &budget, &FixedClock(0)).expect("open");

    let mut lazy = CjkFontSet::new(payload.core.clone());
    let mut source = MapSource {
        map: payload.chunks.clone(),
        fetches: 0,
    };
    let _ = render_cjk(&session, &budget, &mut lazy);
    lazy.drain_requested(&mut source);
    let lazy_hash = render_cjk(&session, &budget, &mut lazy).hash;

    // A set fed in the reverse arrival order paints the same pixels…
    let mut reverse = CjkFontSet::new(payload.core.clone());
    for id in ["hangul-1", "ideographs-4"] {
        let data = source.map.get(id).cloned().expect("chunk built");
        assert!(reverse.provide(id, data));
    }
    let reverse_hash = render_cjk(&session, &budget, &mut reverse).hash;

    // …as one fed in table order, and as the not-yet-loaded core-only set
    // would never equal them (the notdef pass differs — pinned above).
    let mut preloaded = CjkFontSet::new(payload.core.clone());
    for id in ["ideographs-4", "hangul-1"] {
        let data = source.map.get(id).cloned().expect("chunk built");
        assert!(preloaded.provide(id, data));
    }
    let pre_hash = render_cjk(&session, &budget, &mut preloaded).hash;

    assert_eq!(lazy_hash, pre_hash, "lazy-then-provided == pre-provided");
    assert_eq!(lazy_hash, reverse_hash, "arrival order is irrelevant");
}

/// The default walk (no lazy set) keeps its legacy behavior on the very
/// same document: the unresolvable unembedded font paints nothing — no
/// tofu, no queue — so `render_page` is byte-for-byte the old path.
#[test]
fn plain_render_path_is_unchanged() {
    let budget = Budget::profile(Surface::Viewer);
    let session = Session::open(cjk_pdf(), &budget, &FixedClock(0)).expect("open");
    let (hash_a, inked_a) = render_plain(&session, &budget);
    let (hash_b, inked_b) = render_plain(&session, &budget);
    assert_eq!(inked_a, 0, "legacy path draws no tofu");
    assert_eq!(inked_b, 0);
    assert_eq!(hash_a, hash_b, "and it stays deterministic");
}
