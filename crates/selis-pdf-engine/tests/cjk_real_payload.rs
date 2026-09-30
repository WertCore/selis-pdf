//! SL-3.FONT.10 DoD measurement against the **real** Noto Sans SC payload.
//!
//! `cjk_lazy.rs` pins the contract with a synthetic font; this measures the
//! DoD's two halves with the bytes a release would actually ship. It needs the
//! payload, so it is `#[ignore]`d and opt-in:
//!
//! ```sh
//! cargo xtask cjk-fetch noto-sans-sc target/cjk-src
//! cargo xtask cjk-build target/cjk-src/NotoSansSC-wght.ttf --out target/cjk \
//!     --source-id noto-sans-sc --core-list assets/cjk/core-list.txt
//! cargo test -p selis-pdf-engine --test cjk_real_payload -- --ignored --nocapture
//! ```
//!
//! `--core-list` is not optional: without it `cjk-build` emits a *different*
//! payload — one that verifies clean and passes every budget gate — and the
//! numbers below do not reproduce. `assets/cjk/PROVENANCE.md` says so at
//! length.
//!
//! The payload is found by walking up from the test's working directory to the
//! first `target/cjk/cjk/manifest.json`, so there is no environment variable
//! to set — and no environment lookup for the purity gate to have to be
//! widened for. Every number printed is measured here, not read back from
//! the manifest.

#![allow(clippy::panic, clippy::unwrap_used, clippy::expect_used)]
#![allow(clippy::arithmetic_side_effects, clippy::integer_division)]

use std::collections::BTreeMap;
use std::hash::{DefaultHasher, Hash as _, Hasher};
use std::path::PathBuf;

use selis_bytes::Bytes;
use selis_font::cjk::{CjkChunkSource, CjkFontSet};
use selis_geom::Matrix;
use selis_pdf_cos::doc_writer::DocumentBuilder;
use selis_pdf_cos::{Obj, Ref};
use selis_pdf_engine::{CjkRenderOutcome, Session, TinySkiaBackend};
use selis_sandbox::{Budget, CancelToken, FixedClock, Surface};

/// An ordinary Simplified-Chinese paragraph: common characters, one line.
const ORDINARY: &str = "本文件用于测量中文分片字体的增量下载。一段普通的中文文本通常只需要核心字集和一两个分片；缺字先画方框，字体到位后重绘即可，页面不会等待网络。";

/// One character chosen to land in each of several chunk ranges, plus two
/// Hangul syllables, which this payload does not serve at all.
///
/// Every code is **BMP**, deliberately: the document declares `/UniGB-UCS2-H`,
/// whose codes are 2 bytes, so a supplementary-plane character (Ext-B and
/// above, U+20000+) cannot be encoded as one code at all. That gate is
/// ADR-P0043's "`Uni…UCS2…` gate" and it is real — writing U+20000 into a
/// UCS2 string silently yields two meaningless codes, not a deferred glyph.
const RARE: &str = "伀圀屳旐錒顳龘㐀豈︐Ａ한글";

fn bytes(v: &[u8]) -> Bytes {
    Bytes::copy_from_slice(v)
}
fn name(v: &[u8]) -> Obj {
    Obj::Name(bytes(v))
}
fn pair(k: &[u8], v: Obj) -> (Bytes, Obj) {
    (bytes(k), v)
}

/// The classic unembedded `UniGB-UCS2-H` Chinese document shape, showing
/// `text` as 2-byte Unicode codes with `/DW` 1000.
fn cjk_pdf(text: &str) -> Vec<u8> {
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
    // One line per 24 codes, so a paragraph lays out as a paragraph. Each
    // `Tj` gets its own hex string: a `T*` cannot go inside one.
    let codes: Vec<String> = text
        .chars()
        .map(|ch| format!("{:04X}", u32::from(ch)))
        .collect();
    let mut content = String::from("BT /F1 18 Tf 40 740 Td 20 TL\n");
    for (i, code) in codes.iter().enumerate() {
        if i > 0 && i % 24 == 0 {
            content.push_str("T*\n");
        }
        content.push_str(&format!("<{code}> Tj\n"));
    }
    content.push_str("ET\n");
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

/// The payload on disk: the real `cjk/` release layout.
struct Payload {
    core: Bytes,
    core_brotli: u64,
    chunks: BTreeMap<String, Bytes>,
    brotli: BTreeMap<String, u64>,
}

/// `(id, brotli_bytes)` for every chunk row of the manifest, plus the core's
/// own `brotli_bytes`.
///
/// Hand-rolled rather than pulled from `serde_json`: this crate has no JSON
/// dependency, and adding one to read a measurement tool's input is a worse
/// trade than a few lines that only look for three keys. The manifest is
/// generated with sorted keys inside each row, so a row is one `{...}` block.
fn manifest_brotli(manifest: &str) -> (u64, BTreeMap<String, u64>) {
    let number = |s: &str| {
        s.trim_start()
            .split(|c: char| !c.is_ascii_digit())
            .next()
            .filter(|d| !d.is_empty())
            .and_then(|d| d.parse::<u64>().ok())
    };
    let start = manifest.find("\"chunks\"").expect("a chunks array");
    let body = manifest.get(start..).expect("a char boundary");
    let end = body.find("\"totals\"").unwrap_or(body.len());
    let mut out = BTreeMap::new();
    for row in body.get(..end).expect("a char boundary").split('}') {
        let (Some(id), Some(bytes)) = (
            row.split("\"id\": \"")
                .nth(1)
                .and_then(|s| s.split('"').next()),
            row.split("\"brotli_bytes\": ").nth(1).and_then(number),
        ) else {
            continue;
        };
        out.insert(id.to_owned(), bytes);
    }
    assert!(!out.is_empty(), "no chunk rows parsed from the manifest");
    let core = manifest
        .split("\"core\": {")
        .nth(1)
        .and_then(|s| s.split("\"brotli_bytes\": ").nth(1))
        .and_then(number)
        .expect("the core row's brotli_bytes");
    (core, out)
}

/// The first `target/cjk` at or above the working directory.
///
/// Cargo runs an integration test with the *package* directory as its working
/// directory, and the payload is built at the *workspace* root, so the search
/// walks up rather than assuming either. The starting point is canonicalised
/// because `Path::new(".").parent()` is `Some("")`, not `None` — walking up from
/// a bare `.` steps off the filesystem after one level and never reaches the
/// root. Relative paths only: asking the process environment where we are would be a
/// worse answer than walking.
fn find_payload() -> PathBuf {
    let mut dir = std::fs::canonicalize(".").unwrap_or_else(|_| PathBuf::from("."));
    for _ in 0..8 {
        let candidate = dir.join("target").join("cjk").join("cjk");
        if candidate.join("manifest.json").is_file() {
            return dir.join("target").join("cjk");
        }
        let Some(up) = dir.parent() else {
            break;
        };
        if up == dir {
            break;
        }
        dir = up.to_path_buf();
    }
    panic!(
        "no target/cjk/cjk/manifest.json at or above the working directory; run \
         `cargo xtask cjk-fetch noto-sans-sc target/cjk-src` then `cjk-build --out target/cjk` \
         first (see this file's module docs)"
    );
}

fn payload() -> Payload {
    let cjk = find_payload().join("cjk");
    let core = std::fs::read(cjk.join("core.ttf")).expect("core.ttf");
    let text = std::fs::read_to_string(cjk.join("manifest.json")).expect("manifest");
    let (core_brotli, brotli) = manifest_brotli(&text);
    let mut chunks = BTreeMap::new();
    for id in brotli.keys() {
        let path = cjk.join(format!("{id}.ttf"));
        if path.exists() {
            let raw = std::fs::read(&path).expect("chunk");
            chunks.insert(id.clone(), Bytes::copy_from_slice(&raw));
        }
    }
    assert!(!chunks.is_empty(), "no chunk files beside the manifest");
    Payload {
        core: Bytes::copy_from_slice(&core),
        core_brotli,
        chunks,
        brotli,
    }
}

/// Counts the bytes a shell would move, and how many files.
struct DiskSource {
    core_brotli: u64,
    payload: Payload,
    fetched: Vec<String>,
    raw: u64,
    wire: u64,
}

impl CjkChunkSource for DiskSource {
    fn fetch(&mut self, chunk_id: &str) -> Option<Bytes> {
        let bytes = self.payload.chunks.get(chunk_id)?.clone();
        self.fetched.push(chunk_id.to_owned());
        self.raw = self
            .raw
            .saturating_add(u64::try_from(bytes.len()).unwrap_or(u64::MAX));
        self.wire = self
            .wire
            .saturating_add(self.payload.brotli.get(chunk_id).copied().unwrap_or(0));
        Some(bytes)
    }
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

struct Pass {
    outcome: CjkRenderOutcome,
    hash: u64,
    inked: usize,
}

fn render(session: &Session, budget: &Budget, cjk: &mut CjkFontSet) -> Pass {
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

/// `U+XXXX -> <slot>#<gid>` for every code the document shows, or `-` when
/// nothing resident covers it (which is what paints `.notdef`).
fn probe(set: &CjkFontSet, text: &str) -> String {
    let snap = set.snapshot();
    let mut out = String::new();
    for ch in text.chars() {
        let code = u32::from(ch);
        if !selis_font::cjk::chunk_for(code).is_some() {
            continue;
        }
        out.push_str(&format!(
            "U+{code:04X}={} ",
            snap.resolve(code)
                .map_or_else(|| "-".to_owned(), |g| format!("{}#{}", g.slot.key(), g.gid))
        ));
    }
    out
}

fn measure(label: &str, text: &str) {
    let payload = payload();
    let mut set = CjkFontSet::new(payload.core.clone());
    let budget = Budget::profile(Surface::Viewer);
    let session = Session::open(cjk_pdf(text), &budget, &FixedClock(0)).expect("open");
    let mut source = DiskSource {
        core_brotli: payload.core_brotli,
        payload,
        fetched: Vec::new(),
        raw: 0,
        wire: 0,
    };

    let pass1 = render(&session, &budget, &mut set);
    println!("\n== {label}: {} CJK characters ==", text.chars().count());
    println!("slots before drain: {}", probe(&set, text));
    println!(
        "pass 1 (core only): needs={:?} revision={} inked_px={} hash={:016x}",
        pass1.outcome.needs, pass1.outcome.revision, pass1.inked, pass1.hash
    );
    assert!(source.fetched.is_empty(), "the render must not fetch");

    let adopted = set.drain_requested(&mut source);
    println!(
        "drain: adopted={adopted} files={:?} raw={} B wire(brotli)={} B",
        source.fetched, source.raw, source.wire
    );
    println!(
        "incremental download: core {} B brotli (fetched once, whatever the document) + {} B brotli of chunks = {} B on the wire over {} chunk file(s)",
        source.core_brotli,
        source.wire,
        source.core_brotli.saturating_add(source.wire),
        source.fetched.len()
    );

    let pass2 = render(&session, &budget, &mut set);
    println!("slots after drain:  {}", probe(&set, text));
    println!(
        "pass 2 (repaint):     needs={:?} revision={} inked_px={} hash={:016x}",
        pass2.outcome.needs, pass2.outcome.revision, pass2.inked, pass2.hash
    );
    println!(
        "ink delta: {} -> {} px ({:+.1}%)",
        pass1.inked,
        pass2.inked,
        100.0 * (pass2.inked as f64 - pass1.inked as f64) / (pass1.inked.max(1) as f64)
    );
    let pass3 = render(&session, &budget, &mut set);
    assert_eq!(pass2.hash, pass3.hash, "a settled set is deterministic");
    // A document the core alone can serve must paint real ink with no fetch at
    // all; one that needed chunks must ink strictly more after they land.
    assert!(pass1.inked > 0, "nothing painted at all: {label}");
    if adopted > 0 {
        assert!(
            pass2.inked > pass1.inked,
            "the repaint must ink more than the notdef boxes"
        );
        assert!(pass2.hash != pass1.hash, "the repaint must change pixels");
        assert!(
            pass2
                .outcome
                .needs
                .iter()
                .all(|id| !source.payload.chunks.contains_key(*id)),
            "a settled set wants only ranges this payload has no file for: {:?}",
            pass2.outcome.needs
        );
    } else {
        assert_eq!(
            pass1.hash, pass2.hash,
            "nothing was fetched, so nothing moved"
        );
    }
    println!(
        "resident after drain: {} B raw over {} chunk file(s)",
        set.resident_bytes(),
        set.loaded_ids().len()
    );
}

#[test]
#[ignore = "needs the built Noto payload; see the module docs"]
fn a_chinese_document_renders_with_a_measured_incremental_download() {
    measure("ordinary paragraph", ORDINARY);
    measure("rare characters across many ranges", RARE);
}
