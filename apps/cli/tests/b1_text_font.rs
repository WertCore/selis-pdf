//! Bucket 1 extraction pins — SL-3.TEXT.10/15/20/21/23/24.
//!
//! Every PDF here is **synthetic**, built in-test with `DocumentBuilder`: the
//! conformance corpus (`corpus/pdfs`) is fetch-only and absent from worktrees,
//! and the text oracles (mutool / PDFium / pdf.js) are not installed locally, so
//! no per-file tail numbers are claimed. These tests verify the *mechanism* for
//! each font/encoding class with a hand-built fixture and a before/after proof
//! where a code change was made (TEXT.15). Cohort closure against the real
//! files is recorded honestly in `corpus/expect/*/verdicts.jsonl` with
//! `measured: false` and is owned by the next oracle sweep.
//!
//! ```text
//! cargo test -p selis-cli --test b1_text_font
//! ```
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::PathBuf;
use std::process::Command;

use selis_bytes::Bytes;
use selis_pdf_cos::doc_writer::DocumentBuilder;
use selis_pdf_cos::{Obj, Ref};

fn bytes(v: &[u8]) -> Bytes {
    Bytes::copy_from_slice(v)
}
fn name(v: &[u8]) -> Obj {
    Obj::Name(bytes(v))
}
fn int(n: i64) -> Obj {
    Obj::Int(n)
}
fn dict(pairs: &[(&[u8], Obj)]) -> Obj {
    Obj::Dict(pairs.iter().map(|(k, v)| (bytes(k), v.clone())).collect())
}
fn stream(pairs: &[(&[u8], Obj)], data: &[u8]) -> Obj {
    let mut p: Vec<(Bytes, Obj)> = pairs.iter().map(|(k, v)| (bytes(k), v.clone())).collect();
    p.push((
        bytes(b"Length"),
        Obj::Int(i64::try_from(data.len()).unwrap_or(0)),
    ));
    Obj::Stream {
        dict: p,
        data: bytes(data),
    }
}

/// Finish a one-page document: add the content stream and the page, write the
/// bytes. The write is test-local (the engine crates themselves do no I/O).
fn finish(mut builder: DocumentBuilder, content: &[u8], resources: Ref) -> Vec<u8> {
    let budget = selis_sandbox::Budget::unlimited();
    let clock = selis_sandbox::shell_clock();
    let mut g = budget.guard_with(&clock, selis_sandbox::CancelToken::new());
    let c = builder.allocate();
    builder.add_object(c, stream(&[], content));
    builder.add_page_with_extra(400.0, 200.0, &[Ref::new(c, 0)], Some(resources), Vec::new());
    builder.write(&budget, &mut g).expect("write")
}

fn extract(path: &PathBuf, format: &str) -> (i32, String, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_selis"))
        .arg("extract")
        .arg("--format")
        .arg(format)
        .arg("--page")
        .arg("0")
        .arg(path)
        .output()
        .expect("run selis extract");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn dump(tag: &str, pdf: Vec<u8>) -> PathBuf {
    let mut path = std::env::temp_dir();
    path.push(format!("selis-b1-{tag}.pdf"));
    std::fs::write(&path, &pdf).expect("write pdf");
    path
}

/// A CID font dict (descendant) shared by the Type0 fixtures.
fn cid_font(builder: &mut DocumentBuilder, base: &[u8], ordering: &[u8]) -> Ref {
    let n = builder.allocate();
    builder.add_object(
        n,
        dict(&[
            (b"Type", name(b"Font")),
            (b"Subtype", name(b"CIDFontType2")),
            (b"BaseFont", name(base)),
            (
                b"CIDSystemInfo",
                dict(&[
                    (b"Registry", Obj::String(bytes(b"Adobe"))),
                    (b"Ordering", Obj::String(bytes(ordering))),
                    (b"Supplement", int(0)),
                ]),
            ),
        ]),
    );
    Ref::new(n, 0)
}

/// A Type0 font with a fixed encoding and an optional ToUnicode stream.
fn type0_font(
    builder: &mut DocumentBuilder,
    base: &[u8],
    encoding: &[u8],
    descendant: Ref,
    tounicode: Option<Ref>,
) -> Ref {
    let mut pairs = vec![
        (b"Type" as &[u8], name(b"Font")),
        (b"Subtype", name(b"Type0")),
        (b"BaseFont", name(base)),
        (b"Encoding", name(encoding)),
        (b"DescendantFonts", Obj::Array(vec![Obj::Ref(descendant)])),
    ];
    if let Some(tu) = tounicode {
        pairs.push((b"ToUnicode", Obj::Ref(tu)));
    }
    let n = builder.allocate();
    builder.add_object(n, dict(&pairs));
    Ref::new(n, 0)
}

fn font_resources(builder: &mut DocumentBuilder, font: Ref) -> Ref {
    let n = builder.allocate();
    builder.add_object(n, dict(&[(b"Font", dict(&[(b"F1", Obj::Ref(font))]))]));
    Ref::new(n, 0)
}

// ---------------------------------------------------------------------------
// TEXT.20 — CJK / Asian font extraction.
// ---------------------------------------------------------------------------

/// A `Uni…UCS2…` predefined CMap's 2-byte codes ARE UCS-2 scalars. With no
/// `/ToUnicode` the codes still recover the right CJK characters, so an
/// unembedded `STSong-Light` / `UniGB-UCS2-H` document is not a silent empty.
#[test]
fn text20_ucs2_predefined_cmap_recovers_cjk() {
    let mut builder = DocumentBuilder::new();
    let cid = cid_font(&mut builder, b"STSong-Light", b"GB1");
    let font = type0_font(&mut builder, b"STSong-Light", b"UniGB-UCS2-H", cid, None);
    let res = font_resources(&mut builder, font);
    let path = dump(
        "ucs2-cjk",
        finish(builder, b"BT /F1 12 Tf 10 100 Td <4E2D6587> Tj ET\n", res),
    );
    let (code, text, err) = extract(&path, "text");
    assert_eq!(code, 0, "extract failed: {err}");
    assert!(
        text.contains("中文"),
        "UCS2 code-as-Unicode recovery lost: {text}"
    );
    assert!(
        !text.contains("low-confidence"),
        "CJK page must not be flagged: {text}"
    );
}

/// The control neighbour: the same CMap with an explicit `/ToUnicode` maps the
/// codes too, and must not double-apply or drift.
#[test]
fn text20_ucs2_with_tounicode_matches() {
    let mut builder = DocumentBuilder::new();
    let tu = builder.allocate();
    builder.add_object(tu, stream(&[], b"/CIDInit /ProcSet findresource begin\nbegincmap\n1 beginbfchar\n<4E2D> <4E2D>\n<6587> <6587>\nendbfchar\nendcmap\n"));
    let cid = cid_font(&mut builder, b"STSong-Light", b"GB1");
    let font = type0_font(
        &mut builder,
        b"STSong-Light",
        b"UniGB-UCS2-H",
        cid,
        Some(Ref::new(tu, 0)),
    );
    let res = font_resources(&mut builder, font);
    let path = dump(
        "ucs2-cjk-tu",
        finish(builder, b"BT /F1 12 Tf 10 100 Td <4E2D6587> Tj ET\n", res),
    );
    let (code, text, err) = extract(&path, "text");
    assert_eq!(code, 0, "extract failed: {err}");
    assert!(text.contains("中文"), "ToUnicode CJK recovery lost: {text}");
}

// ---------------------------------------------------------------------------
// TEXT.21 — LaTeX / Computer-Modern font family extraction.
// ---------------------------------------------------------------------------

/// CM fonts derive their character from glyph names in `/Encoding/Differences`
/// (alpha, beta, Delta, summation). The TEXT.02 recovery chain's glyph-name →
/// AGL step must turn those into Greek/math Unicode, not raw byte codes.
#[test]
fn text21_cm_glyph_names_recover_greek_math() {
    let mut builder = DocumentBuilder::new();
    let font = builder.allocate();
    builder.add_object(
        font,
        dict(&[
            (b"Type", name(b"Font")),
            (b"Subtype", name(b"Type1")),
            (b"BaseFont", name(b"CMTest")),
            (
                b"Encoding",
                dict(&[
                    (b"Type", name(b"Encoding")),
                    (b"BaseEncoding", name(b"WinAnsiEncoding")),
                    (
                        b"Differences",
                        Obj::Array(vec![
                            int(97),
                            name(b"alpha"),
                            int(98),
                            name(b"beta"),
                            int(65),
                            name(b"Delta"),
                            int(83),
                            name(b"summation"),
                        ]),
                    ),
                ]),
            ),
            (b"FirstChar", int(0)),
            (b"LastChar", int(255)),
        ]),
    );
    let res = font_resources(&mut builder, Ref::new(font, 0));
    let path = dump(
        "cm-glyphnames",
        finish(builder, b"BT /F1 12 Tf 10 100 Td (abAS) Tj ET\n", res),
    );
    let (code, text, err) = extract(&path, "text");
    assert_eq!(code, 0, "extract failed: {err}");
    assert!(
        text.contains('α') && text.contains('β'),
        "Greek CM names lost: {text}"
    );
    assert!(
        text.contains('∆') && text.contains('∑'),
        "math CM names lost: {text}"
    );
}

// ---------------------------------------------------------------------------
// TEXT.23 — Identity-H/V CID font encoding.
// ---------------------------------------------------------------------------

/// Identity-H with a `/ToUnicode` bfchar table recovers the mapped text through
/// the whole parse → display-list → assembly pipeline.
#[test]
fn text23_identity_h_tounicode_extracts() {
    let mut builder = DocumentBuilder::new();
    let tu = builder.allocate();
    builder.add_object(tu, stream(&[], b"/CIDInit /ProcSet findresource begin\nbegincmap\n1 beginbfchar\n<0001> <0041>\n<0002> <0042>\nendbfchar\nendcmap\n"));
    let cid = cid_font(&mut builder, b"IdentityFont", b"Identity");
    let font = type0_font(
        &mut builder,
        b"IdentityFont",
        b"Identity-H",
        cid,
        Some(Ref::new(tu, 0)),
    );
    let res = font_resources(&mut builder, font);
    let path = dump(
        "identity-h",
        finish(builder, b"BT /F1 12 Tf 10 100 Td <00010002> Tj ET\n", res),
    );
    let (code, text, err) = extract(&path, "text");
    assert_eq!(code, 0, "extract failed: {err}");
    assert!(
        text.trim() == "AB",
        "Identity-H bfchar recovery lost: {text:?}"
    );
}

// ---------------------------------------------------------------------------
// TEXT.24 — Type3 font text extraction.
// ---------------------------------------------------------------------------

/// Type3 glyph procedures draw shapes, but the *text* of a Type3 run comes from
/// the font's `/Encoding` glyph names through the same recovery chain as any
/// simple font. Two named procedures `A`/`B` must extract as "AB".
#[test]
fn text24_type3_encoding_text_extracts() {
    let mut builder = DocumentBuilder::new();
    let ga = builder.allocate();
    builder.add_object(ga, stream(&[], b"600 0 d0\n0 0 600 600 re f\n"));
    let gb = builder.allocate();
    builder.add_object(gb, stream(&[], b"600 0 d0\n0 0 600 600 re f\n"));
    let font = builder.allocate();
    builder.add_object(
        font,
        dict(&[
            (b"Type", name(b"Font")),
            (b"Subtype", name(b"Type3")),
            (
                b"FontBBox",
                Obj::Array(vec![int(0), int(0), int(600), int(600)]),
            ),
            (
                b"FontMatrix",
                Obj::Array(vec![
                    Obj::Real {
                        scaled: 1,
                        scale: 3,
                    },
                    int(0),
                    int(0),
                    Obj::Real {
                        scaled: 1,
                        scale: 3,
                    },
                    int(0),
                    int(0),
                ]),
            ),
            (
                b"CharProcs",
                dict(&[
                    (b"A", Obj::Ref(Ref::new(ga, 0))),
                    (b"B", Obj::Ref(Ref::new(gb, 0))),
                ]),
            ),
            (
                b"Encoding",
                dict(&[(
                    b"Differences",
                    Obj::Array(vec![int(65), name(b"A"), int(66), name(b"B")]),
                )]),
            ),
            (b"FirstChar", int(65)),
            (b"LastChar", int(66)),
            (b"Widths", Obj::Array(vec![int(600), int(600)])),
        ]),
    );
    let res = font_resources(&mut builder, Ref::new(font, 0));
    let path = dump(
        "type3",
        finish(builder, b"BT /F1 12 Tf 10 100 Td (AB) Tj ET\n", res),
    );
    let (code, text, err) = extract(&path, "text");
    assert_eq!(code, 0, "extract failed: {err}");
    assert_eq!(text.trim(), "AB", "Type3 encoding recovery lost: {text:?}");
}

// ---------------------------------------------------------------------------
// SL-1.ROB.06 — malformed embedded font cannot panic/abort the engine.
// ---------------------------------------------------------------------------

/// A simple font whose `/FontFile2` is hostile noise. Parsing/rendering must
/// contain the malformed program as a typed deviation (notdef / no recovery),
/// never a panic or abort. A panic in the CLI process would surface as exit
/// code 101, so this pins the whole open → interpret path.
#[test]
fn rob06_malformed_embedded_font_is_contained() {
    let mut builder = DocumentBuilder::new();
    let ff = builder.allocate();
    builder.add_object(ff, stream(&[(b"Length1", int(64))], &[0xAB; 512]));
    let desc = builder.allocate();
    builder.add_object(
        desc,
        dict(&[
            (b"Type", name(b"FontDescriptor")),
            (b"FontName", name(b"Broken")),
            (b"Flags", int(4)),
            (b"FontFile2", Obj::Ref(Ref::new(ff, 0))),
        ]),
    );
    let font = builder.allocate();
    builder.add_object(
        font,
        dict(&[
            (b"Type", name(b"Font")),
            (b"Subtype", name(b"TrueType")),
            (b"BaseFont", name(b"Broken")),
            (b"FontDescriptor", Obj::Ref(Ref::new(desc, 0))),
            (b"Encoding", name(b"WinAnsiEncoding")),
            (b"FirstChar", int(65)),
            (b"LastChar", int(65)),
            (b"Widths", Obj::Array(vec![int(500)])),
        ]),
    );
    let res = font_resources(&mut builder, Ref::new(font, 0));
    let path = dump(
        "malformed-font",
        finish(builder, b"BT /F1 12 Tf 10 100 Td (A) Tj ET\n", res),
    );
    for format in ["text", "json"] {
        let (code, out, err) = extract(&path, format);
        assert_ne!(
            code, 101,
            "engine panicked on malformed font ({format}): {err}"
        );
        assert_eq!(
            code, 0,
            "typed refusal expected, got exit {code}: {err}\n{out}"
        );
    }
}

// ---------------------------------------------------------------------------
// SL-3.TEXT.22 — RTL reading order in the tail.
// ---------------------------------------------------------------------------

/// Hebrew `שלום` shown in logical order through an Identity-H `/ToUnicode`
/// table: extraction must preserve the logical scalar sequence, not scramble
/// it. (A producer that paints the same word in *visual* order is a separate
/// cohort question — whether to apply UAX #9 visual→logical reordering needs
/// the corpus + oracle, so it is recorded as an honest verdict, not assumed.)
#[test]
fn text22_rtl_logical_order_is_preserved() {
    let mut builder = DocumentBuilder::new();
    let tu = builder.allocate();
    builder.add_object(tu, stream(&[], b"/CIDInit /ProcSet findresource begin\nbegincmap\n1 beginbfchar\n<0001> <05E9>\n<0002> <05DC>\n<0003> <05D5>\n<0004> <05DD>\nendbfchar\nendcmap\n"));
    let cid = cid_font(&mut builder, b"HebrewFont", b"Identity");
    let font = type0_font(
        &mut builder,
        b"HebrewFont",
        b"Identity-H",
        cid,
        Some(Ref::new(tu, 0)),
    );
    let res = font_resources(&mut builder, font);
    let path = dump(
        "rtl-logical",
        finish(
            builder,
            b"BT /F1 20 Tf 300 100 Td <0001000200030004> Tj ET\n",
            res,
        ),
    );
    let (code, text, err) = extract(&path, "text");
    assert_eq!(code, 0, "extract failed: {err}");
    assert_eq!(text.trim(), "שלום", "RTL logical order scrambled: {text:?}");
}

// ---------------------------------------------------------------------------
// SL-3.TEXT.10 — silent empty extraction (typed, never blank success).
// ---------------------------------------------------------------------------

/// A page that drew surrogate CID codes (decode to nothing) must emit the
/// low-confidence marker, and its JSON verdict must be typed `zero_recovery`
/// — never a silent empty string that looks like a blank page.
#[test]
fn text10_drawn_undecodable_text_is_flagged_not_silent() {
    let mut builder = DocumentBuilder::new();
    let cid = cid_font(&mut builder, b"TestCid", b"Identity");
    let font = type0_font(&mut builder, b"TestCid", b"Identity-H", cid, None);
    let res = font_resources(&mut builder, font);
    let path = dump(
        "zero-recovery",
        finish(builder, b"BT /F1 24 Tf 10 100 Td <D800DFFF> Tj ET\n", res),
    );
    let (code, text, err) = extract(&path, "text");
    assert_eq!(code, 0, "extract failed: {err}");
    assert!(
        text.contains("[low-confidence: text drawn, nothing recovered]"),
        "silent empty extraction: {text:?}"
    );
    let (code, json, _) = extract(&path, "json");
    assert_eq!(code, 0);
    assert!(
        json.contains("\"verdict\": \"zero_recovery\""),
        "typed verdict missing: {json}"
    );
    assert!(
        !json.contains("[low-confidence:"),
        "bare marker line corrupted the JSON document: {json}"
    );
}

// ---------------------------------------------------------------------------
// SL-3.TEXT.15 — divergent extractions that recover nothing (typed signal).
// ---------------------------------------------------------------------------

/// A page with one recoverable code and two undecodable surrogates: 2/3 of the
/// drawn glyphs are unreadable, so the typed verdict is
/// `divergent_no_recovery` and a distinct marker is emitted — never a blank
/// "success". This is the local, measurable proxy for the plan's >5 % tail;
/// the oracle-side divergence fraction needs the corpus (see verdict ledger).
#[test]
fn text15_partial_undecodable_tail_is_typed() {
    let mut builder = DocumentBuilder::new();
    let tu = builder.allocate();
    builder.add_object(tu, stream(&[], b"/CIDInit /ProcSet findresource begin\nbegincmap\n1 beginbfchar\n<0001> <0054>\nendbfchar\nendcmap\n"));
    let cid = cid_font(&mut builder, b"TestCid", b"Identity");
    let font = type0_font(
        &mut builder,
        b"TestCid",
        b"Identity-H",
        cid,
        Some(Ref::new(tu, 0)),
    );
    let res = font_resources(&mut builder, font);
    let path = dump(
        "divergent-tail",
        finish(
            builder,
            b"BT /F1 24 Tf 10 100 Td <0001D800DFFF> Tj ET\n",
            res,
        ),
    );
    let (code, text, err) = extract(&path, "text");
    assert_eq!(code, 0, "extract failed: {err}");
    assert!(text.contains('T'), "recoverable code lost: {text:?}");
    assert!(
        text.contains("[low-confidence: >5% of drawn glyphs were unreadable]"),
        "typed divergence marker missing: {text:?}"
    );
    let (code, json, _) = extract(&path, "json");
    assert_eq!(code, 0);
    assert!(
        json.contains("\"verdict\": \"divergent_no_recovery\""),
        "typed verdict missing: {json}"
    );
    assert!(
        !json.contains("[low-confidence:"),
        "bare marker line corrupted the JSON document: {json}"
    );
}

/// The exact-match neighbour the TEXT.15 DoD asks for: the same page with only
/// recoverable codes stays `recovered` and carries no marker, so the 5 %
/// threshold cannot silently drift.
#[test]
fn text15_exact_match_neighbour_is_clean() {
    let mut builder = DocumentBuilder::new();
    let tu = builder.allocate();
    builder.add_object(tu, stream(&[], b"/CIDInit /ProcSet findresource begin\nbegincmap\n1 beginbfchar\n<0001> <0054>\n<0002> <0065>\n<0003> <0073>\n<0004> <0074>\nendbfchar\nendcmap\n"));
    let cid = cid_font(&mut builder, b"TestCid", b"Identity");
    let font = type0_font(
        &mut builder,
        b"TestCid",
        b"Identity-H",
        cid,
        Some(Ref::new(tu, 0)),
    );
    let res = font_resources(&mut builder, font);
    let path = dump(
        "exact-neighbour",
        finish(
            builder,
            b"BT /F1 24 Tf 10 100 Td <0001000200030004> Tj ET\n",
            res,
        ),
    );
    let (code, text, err) = extract(&path, "text");
    assert_eq!(code, 0, "extract failed: {err}");
    assert_eq!(text.trim(), "Test", "clean neighbour lost: {text:?}");
    assert!(
        !text.contains("low-confidence"),
        "clean neighbour must not be flagged: {text:?}"
    );
    let (code, json, _) = extract(&path, "json");
    assert_eq!(code, 0);
    assert!(
        json.contains("\"verdict\": \"recovered\""),
        "clean verdict missing: {json}"
    );
}
