//! `cargo xtask fallback-assets` — the reproducible fallback-font payload.
//!
//! Produces two things from the twelve Liberation faces in `assets/fonts/`,
//! both as build artifacts rather than committed hand-edits:
//!
//! 1. **The built-in subset** (`assets/fonts/subset/`) — Serif Regular and Bold,
//!    reduced to the built-in code-point set. These are the only fallback
//!    faces compiled into the web module; see `selis_font::fallback`.
//! 2. **The lazy payload** (`<out>/fallback/`) — every *other* face, brotli-
//!    compressed, plus `manifest.json` in the `selis-fallback/1` format with the
//!    size and SHA-256 of each. This is what the web shell fetches on demand.
//!
//! ## Why the subsets are generated here and not by a font tool
//!
//! `selis_font::subset::subset_ttf` is already in the tree and is what CJK
//! assets are built with. Reusing it keeps one subsetter, so the fallback and
//! CJK paths cannot diverge, and it keeps the build pure-Rust — adding
//! fontTools to CI would be a new toolchain dependency for two faces.
//!
//! ## Reproducibility is the point
//!
//! The committed subsets and the committed digests must be re-derivable from
//! the committed source faces. `--check` verifies exactly that and is a CI
//! gate: a Liberation update that changed the upstream bytes without
//! regenerating the assets fails loudly, instead of silently leaving a
//! manifest that names digests nobody ships.

use std::collections::BTreeSet;
use std::io::Write;
use std::path::{Path, PathBuf};

use selis_pdf_wasm::fallback_manifest::{FallbackFace, FallbackManifest, MANIFEST_VERSION};

/// The faces built into the web module.
///
/// Serif because it is metric-compatible with Times New Roman — the base-14
/// font most often referenced without embedding. Regular *and* Bold because a
/// single face would draw bold text in regular advances and reflow the line.
/// See `selis_font::fallback` for the full argument.
pub const BUILTIN_FACES: &[&str] = &["LiberationSerif-Regular", "LiberationSerif-Bold"];

/// Every face the lazy payload carries, in manifest order.
///
/// Sorted rather than hand-ordered so the manifest is byte-stable across runs
/// on any platform; an unstable manifest would churn the commit and defeat
/// `--check`.
fn lazy_faces() -> Vec<&'static str> {
    let mut v: Vec<&'static str> = ["LiberationSans", "LiberationSerif", "LiberationMono"]
        .iter()
        .flat_map(|fam| {
            ["Regular", "Bold", "Italic", "BoldItalic"]
                .iter()
                .map(move |s| {
                    // The concat needs to be a literal to be 'static.
                    let leaked: &'static str = Box::leak(format!("{fam}-{s}").into_boxed_str());
                    leaked
                })
        })
        .filter(|n| !BUILTIN_FACES.contains(n))
        .collect();
    v.sort_unstable();
    v
}

/// The built-in code-point set: ASCII, Latin-1 letters, and the typographic
/// punctuation a document actually uses.
///
/// Deliberately not "everything Latin-1": 0x80-0x9F are control characters,
/// and including them would enlarge every subset for glyphs no text run ever
/// asks for. The typographic tail is the curly quotes, dashes, ellipsis and
/// the handful of symbols that appear in ordinary prose — 211 code points in
/// all.
#[must_use]
pub fn builtin_codepoints() -> BTreeSet<u32> {
    let mut s = BTreeSet::new();
    s.extend(0x20..0x7F); // printable ASCII
    s.extend(0xA0..0x100); // Latin-1 Supplement, letters and symbols
    for c in [
        0x00A0, 0x00AB, 0x00B0, 0x00B1, 0x00B5, 0x00B7, 0x00D7, 0x00F7, 0x2013, 0x2014, 0x2018,
        0x2019, 0x201C, 0x201D, 0x2020, 0x2021, 0x2022, 0x2026, 0x2030, 0x2039, 0x203A, 0x20AC,
        0x2122, 0x2190, 0x2192, 0x2212, 0xFB01, 0xFB02,
    ] {
        s.insert(c);
    }
    s
}

/// The glyph ids in `font` for the built-in code points.
///
/// `GlyphSet` holds glyph ids, not code points, so this is the step that
/// turns the specification above into something `subset_ttf` accepts. A code
/// point the face has no glyph for is skipped rather than fatal: the face may
/// legitimately lack, say, a rare Latin-1 symbol, and dropping it from the
/// subset is correct — there is no glyph to keep. **A silently missing glyph is
/// the failure mode worth watching here**, so `fallback.rs` has a test that
/// every claimed code point is present in the built subsets, which is what
/// catches a subset that lost coverage without anyone noticing.
fn builtin_glyph_ids(font: &selis_bytes::Bytes) -> Vec<u16> {
    let mut gids: Vec<u16> = builtin_codepoints()
        .into_iter()
        .filter_map(|c| selis_font::glyph_id_for_char(font, c))
        .collect();
    gids.sort_unstable();
    gids.dedup();
    gids
}

/// Generate the subsets and the manifest.
pub fn run(src: &Path, out: &Path, check: bool) -> Result<(), String> {
    let subset_dir = src.join("subset");
    let mut faces = Vec::new();

    for name in BUILTIN_FACES {
        let full = src.join(format!("{name}.ttf"));
        let bytes =
            std::fs::read(&full).map_err(|e| format!("cannot read {}: {e}", full.display()))?;
        let src_bytes = selis_bytes::Bytes::from(bytes.clone());
        let gids = builtin_glyph_ids(&src_bytes);
        let mut guard = selis_sandbox::Budget::unlimited().guard();
        let sub = selis_font::subset::subset_ttf(
            &src_bytes,
            &selis_font::GlyphSet { keep: gids.clone() },
            &mut guard,
        )
        .map_err(|e| format!("subsetting {name} failed: {e}"))?
        .ok_or_else(|| format!("{name} did not parse as a TrueType font"))?;
        let sub = sub.as_slice().to_vec();
        let sub_path = subset_dir.join(format!("{name}.ttf"));
        if check {
            let have = std::fs::read(&sub_path).unwrap_or_default();
            if have != sub {
                return Err(format!(
                    "{} is stale — re-run without --check",
                    sub_path.display()
                ));
            }
        } else {
            std::fs::create_dir_all(&subset_dir)
                .map_err(|e| format!("cannot create {}: {e}", subset_dir.display()))?;
            write_if_changed(&sub_path, &sub)?;
        }
        eprintln!(
            "  subset {name}: {} B, {} glyphs -> {} B",
            bytes.len(),
            gids.len(),
            sub.len()
        );
    }

    // The lazy payload: every non-built-in face, compressed.
    let lazy_dir = out.join("fallback");
    std::fs::create_dir_all(&lazy_dir)
        .map_err(|e| format!("create {}: {e}", lazy_dir.display()))?;
    for name in lazy_faces() {
        let full = src.join(format!("{name}.ttf"));
        let bytes =
            std::fs::read(&full).map_err(|e| format!("cannot read {}: {e}", full.display()))?;
        let transfer = brotli_compress(&bytes)?;
        let path = lazy_dir.join(format!("{name}.ttf.br"));
        if !check {
            write_if_changed(&path, &transfer)?;
        }
        faces.push(FallbackFace {
            name: name.to_string(),
            transfer_size: u32::try_from(transfer.len()).unwrap_or(u32::MAX),
            raw_size: u32::try_from(bytes.len()).unwrap_or(u32::MAX),
            sha256: hex_sha256(&bytes),
        });
        eprintln!(
            "  lazy   {name}: {} B raw -> {} B",
            bytes.len(),
            transfer.len()
        );
    }

    let manifest = FallbackManifest {
        version: MANIFEST_VERSION,
        faces,
    };
    let json = serde_json::to_vec_pretty(&manifest).map_err(|e| e.to_string())?;
    let mpath = lazy_dir.join("manifest.json");
    if check {
        let have = std::fs::read(&mpath).unwrap_or_default();
        if have != json {
            return Err(format!(
                "{} is stale — re-run without --check",
                mpath.display()
            ));
        }
    } else {
        write_if_changed(&mpath, &json)?;
    }
    eprintln!("  manifest: {} faces", manifest.faces.len());
    Ok(())
}

/// Write only when the content differs, so a no-op run leaves mtimes alone and
/// does not invalidate downstream caches.
fn write_if_changed(path: &Path, bytes: &[u8]) -> Result<(), String> {
    if std::fs::read(path).is_ok_and(|h| h == bytes) {
        return Ok(());
    }
    let tmp = path.with_extension("tmp");
    let mut f =
        std::fs::File::create(&tmp).map_err(|e| format!("create {}: {e}", tmp.display()))?;
    f.write_all(bytes).map_err(|e| e.to_string())?;
    f.sync_all().map_err(|e| e.to_string())?;
    drop(f);
    std::fs::rename(&tmp, path).map_err(|e| e.to_string())
}

/// Brotli-compress through the same helper the size gate uses, so a budget
/// quoted here means what `cargo xtask size-check` will actually measure.
///
/// Reusing `size_check::brotli_compress` rather than adding a brotli dependency
/// to xtask also means the two cannot disagree about what "brotli size" means,
/// which is the same reason the CJK builder measures through it.
fn brotli_compress(data: &[u8]) -> Result<Vec<u8>, String> {
    let tmp = std::env::temp_dir().join(format!("selis-fallback-{}.tmp", std::process::id()));
    std::fs::write(&tmp, data).map_err(|e| format!("write {}: {e}", tmp.display()))?;
    let out = crate::size_check::brotli_compress(&tmp);
    let _ = std::fs::remove_file(&tmp);
    out
}

fn hex_sha256(data: &[u8]) -> String {
    use sha2::Digest as _;
    let d = sha2::Sha256::digest(data);
    let mut s = String::with_capacity(64);
    for b in d {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

/// The source directory holding the twelve upstream faces.
#[must_use]
pub fn default_src() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("assets")
        .join("fonts")
}
