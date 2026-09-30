//! `cargo xtask fallback-assets` — the reproducible fallback-font payload.
//!
//! Produces two things from the twelve Liberation faces in `assets/fonts/`,
//! both as build artifacts rather than committed hand-edits:
//!
//! 1. **The built-in subset** (`assets/fonts/subset/`) — Serif Regular and Bold,
//!    reduced to the built-in code-point set. These are the only fallback
//!    faces compiled into the web module; see `selis_font::fallback`.
//! 2. **The lazy payload** (`<out>/fallback/`) — every *other* face, brotli-
//!    compressed, plus `manifest.json` in the `selis-fallback/2` format with the
//!    size, SHA-256 and published `url` of each. This is what the web shell
//!    fetches on demand.
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

/// The manifest version this generator writes, matching
/// `selis_pdf_wasm::fallback_manifest::MANIFEST_VERSION`.
///
/// **Duplicated on purpose, and that is a real trade-off.** Sharing the type
/// would guarantee the generator cannot emit a shape its own reader rejects -
/// which is why it was written that way first. But xtask's wasm32 build is the
/// `engine-viewer` size canary, and depending on `selis-pdf-wasm` to reach one
/// struct pulled the whole engine in: 185 845 B -> 769 997 B, over the
/// 400 000 B budget. So the wire format is restated here and pinned by a test
/// that parses it with the real reader, which catches drift where it matters
/// without making the canary pay for the whole engine.
const MANIFEST_VERSION: u32 = 2;

/// The directory the lazy payload is published under, relative to the payload
/// root the shell resolves the manifest's `url` against.
///
/// One constant because it is written in two places that must agree: the file
/// is written here, and the `url` recorded in the manifest is the path *to* that
/// file. Splitting them would let a future rename update one and not the other,
/// which is precisely the drift the `url` field exists to prevent.
const LAZY_DIR: &str = "fallback";

/// The extension `xtask fallback-assets` publishes each compressed face under,
/// and the suffix every recorded `url` must end in. The reader requires it,
/// because it is what says the bytes are brotli-compressed TrueType.
const FACE_SUFFIX: &str = ".ttf.br";

/// One face entry, as written to the manifest.
struct FallbackFace {
    /// The `family + style` face name.
    name: String,
    /// Compressed transfer size, bytes.
    transfer_size: u32,
    /// Decompressed size, bytes.
    raw_size: u32,
    /// Lowercase hex SHA-256 of the decompressed font.
    sha256: String,
    /// The path the compressed file was published at, relative to the payload
    /// root. **Relative, always** - see `fallback_manifest::FACE_SUFFIX` and the
    /// module docs for why an absolute URL is not an option here.
    url: String,
}

/// The manifest, as written. Field order here is the JSON field order.
struct FallbackManifest {
    /// Format version.
    version: u32,
    /// The faces on offer, sorted by name.
    faces: Vec<FallbackFace>,
}

impl FallbackManifest {
    /// Render the `selis-fallback/2` document.
    fn to_json(&self) -> String {
        let mut out = String::from("{\n  \"version\": ");
        out.push_str(&self.version.to_string());
        out.push_str(",\n  \"faces\": [");
        for (i, f) in self.faces.iter().enumerate() {
            out.push_str(if i == 0 { "\n" } else { ",\n" });
            out.push_str("    {\n      \"name\": ");
            out.push_str(&json_string(&f.name));
            out.push_str(",\n      \"transfer_size\": ");
            out.push_str(&f.transfer_size.to_string());
            out.push_str(",\n      \"raw_size\": ");
            out.push_str(&f.raw_size.to_string());
            out.push_str(",\n      \"sha256\": ");
            out.push_str(&json_string(&f.sha256));
            out.push_str(",\n      \"url\": ");
            out.push_str(&json_string(&f.url));
            out.push_str("\n    }");
        }
        out.push_str(if self.faces.is_empty() {
            "]\n}\n"
        } else {
            "\n  ]\n}\n"
        });
        out
    }
}

/// A minimal JSON string escape, for the two string fields.
///
/// Face names are ASCII identifiers and digests are hex, so this is
/// belt-and-braces rather than load-bearing - but a generator that can emit
/// invalid JSON should be impossible rather than merely unlikely.
fn json_string(v: &str) -> String {
    let mut out = String::with_capacity(v.len() + 2);
    out.push('"');
    for c in v.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
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
    let lazy_dir = out.join(LAZY_DIR);
    std::fs::create_dir_all(&lazy_dir)
        .map_err(|e| format!("create {}: {e}", lazy_dir.display()))?;
    for name in lazy_faces() {
        let full = src.join(format!("{name}.ttf"));
        let bytes =
            std::fs::read(&full).map_err(|e| format!("cannot read {}: {e}", full.display()))?;
        let transfer = brotli_compress(&bytes)?;
        let file = format!("{name}{FACE_SUFFIX}");
        let path = lazy_dir.join(&file);
        if !check {
            write_if_changed(&path, &transfer)?;
        }
        faces.push(FallbackFace {
            name: name.to_string(),
            transfer_size: u32::try_from(transfer.len()).unwrap_or(u32::MAX),
            raw_size: u32::try_from(bytes.len()).unwrap_or(u32::MAX),
            sha256: hex_sha256(&bytes),
            // **The path this run actually wrote to**, joined from the same
            // directory name and file name the write above used. That is the
            // whole point of the field: the shell reads this instead of
            // re-deriving `fallback/<name>.ttf.br` from a convention it
            // happens to share with this generator.
            url: format!("{LAZY_DIR}/{file}"),
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
    let json = manifest.to_json().into_bytes();
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

#[cfg(test)]
mod tests {
    use super::*;

    fn face(name: &str) -> FallbackFace {
        FallbackFace {
            name: name.to_string(),
            transfer_size: 61_971,
            raw_size: 105_460,
            sha256: "ca7f64dc81567369a64998ef274f4dc97fae3f6367cc87b215421189e68c87b2".to_string(),
            url: format!("{LAZY_DIR}/{name}{FACE_SUFFIX}"),
        }
    }

    /// The drift guard that replaced the shared type.
    ///
    /// The generator restates the manifest format rather than importing it, so
    /// this asserts the emitted document is what the *real* reader accepts. If
    /// a field is renamed, retyped or reordered on either side this fails -
    /// the guarantee the shared-type approach gave, without making the wasm32
    /// size canary depend on the whole engine.
    #[test]
    fn the_emitted_manifest_matches_what_the_reader_expects() {
        let m = FallbackManifest {
            version: MANIFEST_VERSION,
            faces: vec![face("LiberationMono-Bold"), face("LiberationSans-Regular")],
        };
        let parsed =
            selis_pdf_wasm::fallback_manifest::FallbackManifest::parse(m.to_json().as_bytes())
                .expect("the real reader must accept what we emit");

        assert_eq!(parsed.version, MANIFEST_VERSION);
        assert_eq!(parsed.faces.len(), 2);
        let b = parsed.face("LiberationMono-Bold").expect("face present");
        assert_eq!(b.transfer_size, 61_971);
        assert_eq!(b.raw_size, 105_460);
        // The digest must survive the round trip byte for byte, or the whole
        // verification story is decorative.
        assert_eq!(b.sha256, face("LiberationMono-Bold").sha256);
        // And so must the url - it is the one field the shell cannot reconstruct
        // for itself, so a rename on this side that did not reach the reader
        // would be invisible here if it were not asserted.
        assert_eq!(b.url, "fallback/LiberationMono-Bold.ttf.br");
    }

    /// The version this generator writes is the version the reader reads. The
    /// two are separate constants in separate crates on purpose (the size canary
    /// forbids the shared type), so the coupling has to be asserted rather than
    /// inherited - a generator still writing `1` would emit a document every
    /// reader refuses, and `--check` would happily agree with it.
    #[test]
    fn the_version_matches_the_reader() {
        assert_eq!(
            MANIFEST_VERSION,
            selis_pdf_wasm::fallback_manifest::MANIFEST_VERSION,
            "the generator and the reader must pin the same manifest version"
        );
    }

    /// The recorded url is the path the generator wrote to, so the two are made
    /// from one directory constant and one suffix constant rather than two
    /// literals. This is the assertion that says the emitted document and the
    /// published file agree.
    #[test]
    fn the_recorded_url_is_built_from_the_published_layout() {
        let m = FallbackManifest {
            version: MANIFEST_VERSION,
            faces: vec![face("LiberationSans-Regular")],
        };
        let parsed =
            selis_pdf_wasm::fallback_manifest::FallbackManifest::parse(m.to_json().as_bytes())
                .expect("valid");
        let face = parsed.face("LiberationSans-Regular").expect("face present");
        // Exactly what `run` writes under `out`, and exactly what the shell must
        // ask for: no leading slash, no absolute URL, no trailing slash.
        assert_eq!(
            face.url,
            format!("{LAZY_DIR}/LiberationSans-Regular{FACE_SUFFIX}")
        );
        assert!(!face.url.starts_with('/'));
        assert!(!face.url.contains('\\'));
    }

    /// An empty manifest is still valid JSON, not a truncated fragment.
    #[test]
    fn an_empty_manifest_is_still_valid() {
        let m = FallbackManifest {
            version: MANIFEST_VERSION,
            faces: Vec::new(),
        };
        let parsed =
            selis_pdf_wasm::fallback_manifest::FallbackManifest::parse(m.to_json().as_bytes())
                .expect("empty but valid");
        assert!(parsed.faces.is_empty());
    }

    /// Strings containing a quote, a backslash or a control character must
    /// not be able to produce invalid JSON.
    #[test]
    fn json_string_escapes_quote_backslash_and_control_characters() {
        assert_eq!(json_string("plain"), r#""plain""#);
        assert_eq!(json_string("q\"b"), r#""q\"b""#);
        assert_eq!(json_string("b\\s"), r#""b\\s""#);
        assert_eq!(json_string("n\nl"), r#""n\u000al""#);
    }
}
