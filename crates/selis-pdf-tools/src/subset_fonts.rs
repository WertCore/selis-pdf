//! The `subset_fonts` document tool (parity #3).
//!
//! Runs the shared TrueType subset pass from [`selis_pdf_optimize`] over the
//! input document, then puts the rewritten bytes through the same structural
//! verification gate and atomic commit as the other full-rewrite tools. The
//! pass only replaces embedded font programs: page order, content streams,
//! annotations, forms and outlines survive byte-identically.
//!
//! Encrypted documents, CFF programs, non-Identity-H CMaps and rewrites that
//! would not shrink the document are refused with a typed error naming the
//! reason; no output is committed in that case.

use selis_pdf_optimize::SubsetReport;
use selis_sandbox::Budget;

use crate::verify_report::Verification;
use crate::write_gate::{self, PRESERVE_ALL};
use crate::{read_file, CliError, CliResult};

/// Subset the embedded TrueType programs used by `path`'s text into `output`.
///
/// Returns the measured report and the structural verification. The output is
/// strictly smaller than the input on success and re-verified before it is
/// committed.
///
/// # Errors
///
/// [`crate::ToolError`] when the input cannot be read, the document is
/// malformed or encrypted, no supported font is used by the text, the font
/// shape is unsupported, or the rewrite fails verification.
pub fn subset_fonts(path: &str, output: &str) -> CliResult<(SubsetReport, Verification)> {
    let src = read_file(path)?;
    let in_bytes = src.len();
    let budget = Budget::unlimited();
    let clock = crate::shell_clock();
    let mut g = crate::runtime::cli_guard(&budget, &clock);

    let (bytes, report) = selis_pdf_optimize::subset_fonts(&src, &budget, &mut g)
        .map_err(|e| CliError(format!("{path}: {e}")))?;

    // Structural verification: the output must reparse, every reference must
    // resolve, and the input's surveyed counts must hold. Commit is atomic.
    let observed = selis_pdf_cos::verify::survey(&src, &budget, &mut g)
        .map_err(|e| CliError(format!("{path}: input survey failed: {e}")))?;
    let expected = write_gate::expectations_from(&observed, &PRESERVE_ALL);
    let verdict = write_gate::write_verified(&bytes, output, &expected, &budget, &mut g)?;

    // The output must expose a usable page tree, not just a parseable root.
    let out_startxref = selis_pdf_cos::xref::find_startxref(&bytes, 4096).unwrap_or(0);
    let out_doc = selis_pdf_cos::parse_revisions(&bytes, out_startxref, &budget, &mut g)
        .map_err(|e| CliError(format!("{path}: output does not reparse: {e}")))?;
    crate::tools::page_refs_of(&bytes, &out_doc, &budget, &mut g)
        .map_err(|e| CliError(format!("{path}: output has no usable page tree: {e}")))?;

    let mut verification = Verification::from_gate(
        Some(&observed),
        &verdict,
        &expected,
        u64::try_from(in_bytes).unwrap_or(u64::MAX),
        u64::try_from(bytes.len()).unwrap_or(u64::MAX),
    );
    // Surface the measured subset report on the machine-readable verification
    // object too, so the CLI's `--json` twin and the browser card report the
    // same fonts/glyphs counts (the standard verification rows already carry
    // bytes_in -> bytes_out).
    verification.fonts_subset = u64::try_from(report.fonts_subset).ok();
    verification.glyphs_kept = u64::try_from(report.glyphs_kept).ok();
    Ok((report, verification))
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::arithmetic_side_effects,
        clippy::cast_possible_truncation,
        clippy::expect_used,
        clippy::indexing_slicing,
        clippy::integer_division,
        clippy::panic,
        clippy::unwrap_used
    )]

    use super::*;
    use selis_bytes::Bytes;
    use selis_pdf_cos::doc_writer::DocumentBuilder;
    use selis_pdf_cos::{Obj, Ref};
    use selis_sandbox::Budget;

    const LIBERATION: &[u8] = include_bytes!("../../../assets/fonts/LiberationSans-Regular.ttf");
    const DEVANAGARI: &[u8] =
        include_bytes!("../../../corpus/fixtures/fonts/NotoSansDevanagari-Regular.ttf");

    fn name(value: &[u8]) -> Obj {
        Obj::Name(Bytes::copy_from_slice(value))
    }

    fn stream(dict: Vec<(Bytes, Obj)>, data: &[u8]) -> Obj {
        Obj::Stream {
            dict,
            data: Bytes::copy_from_slice(data),
        }
    }

    fn temp_dir(name: &str) -> std::path::PathBuf {
        let dir =
            std::env::temp_dir().join(format!("selis-subset-fonts-{}-{name}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).expect("create temp dir");
        dir
    }

    /// A one-page PDF with an embedded WinAnsi TrueType font showing
    /// "Hello, world!" — a real 139 KiB font program, so a subset is much
    /// smaller than the source.
    fn winansi_pdf() -> Vec<u8> {
        let budget = Budget::unlimited();
        let clock = selis_sandbox::shell_clock();
        let mut g = budget.guard_with(&clock, selis_sandbox::CancelToken::new());
        let mut builder = DocumentBuilder::new();

        let font_bytes = LIBERATION;
        let file_num = builder.allocate();
        builder.add_object(
            file_num,
            stream(
                vec![
                    (
                        Bytes::copy_from_slice(b"Length"),
                        Obj::Int(i64::try_from(font_bytes.len()).unwrap_or(i64::MAX)),
                    ),
                    (
                        Bytes::copy_from_slice(b"Length1"),
                        Obj::Int(i64::try_from(font_bytes.len()).unwrap_or(i64::MAX)),
                    ),
                ],
                font_bytes,
            ),
        );
        let font = builder.allocate();
        builder.add_object(
            font,
            Obj::Dict(vec![
                (Bytes::copy_from_slice(b"Type"), name(b"Font")),
                (Bytes::copy_from_slice(b"Subtype"), name(b"TrueType")),
                (
                    Bytes::copy_from_slice(b"BaseFont"),
                    name(b"ABCDEE+LiberationSans"),
                ),
                (
                    Bytes::copy_from_slice(b"Encoding"),
                    name(b"WinAnsiEncoding"),
                ),
                (Bytes::copy_from_slice(b"FirstChar"), Obj::Int(32)),
                (Bytes::copy_from_slice(b"LastChar"), Obj::Int(126)),
                (
                    Bytes::copy_from_slice(b"FontDescriptor"),
                    Obj::Dict(vec![
                        (
                            Bytes::copy_from_slice(b"FontName"),
                            name(b"ABCDEE+LiberationSans"),
                        ),
                        (Bytes::copy_from_slice(b"Flags"), Obj::Int(4)),
                        (
                            Bytes::copy_from_slice(b"FontFile2"),
                            Obj::Ref(Ref::new(file_num, 0)),
                        ),
                    ]),
                ),
            ]),
        );
        let resources = builder.allocate();
        builder.add_object(
            resources,
            Obj::Dict(vec![(
                Bytes::copy_from_slice(b"Font"),
                Obj::Dict(vec![(
                    Bytes::copy_from_slice(b"F1"),
                    Obj::Ref(Ref::new(font, 0)),
                )]),
            )]),
        );
        let content = b"BT /F1 24 Tf 72 700 Td (Hello, world!) Tj ET\n";
        let content_num = builder.allocate();
        builder.add_object(
            content_num,
            stream(
                vec![(
                    Bytes::copy_from_slice(b"Length"),
                    Obj::Int(i64::try_from(content.len()).unwrap_or(i64::MAX)),
                )],
                content,
            ),
        );
        builder.add_page_with(
            612.0,
            792.0,
            &[Ref::new(content_num, 0)],
            Some(Ref::new(resources, 0)),
        );
        builder.write(&budget, &mut g).expect("write fixture")
    }

    /// A one-page PDF showing "ABC" with an embedded CFF Type1C program that
    /// has real local and global subroutines.
    fn cff_pdf(program: &[u8]) -> Vec<u8> {
        let budget = Budget::unlimited();
        let clock = selis_sandbox::shell_clock();
        let mut g = budget.guard_with(&clock, selis_sandbox::CancelToken::new());
        let mut builder = DocumentBuilder::new();
        let file_num = builder.allocate();
        builder.add_object(
            file_num,
            stream(
                vec![
                    (
                        Bytes::copy_from_slice(b"Length"),
                        Obj::Int(i64::try_from(program.len()).unwrap_or(i64::MAX)),
                    ),
                    (
                        Bytes::copy_from_slice(b"Length1"),
                        Obj::Int(i64::try_from(program.len()).unwrap_or(i64::MAX)),
                    ),
                    (Bytes::copy_from_slice(b"Subtype"), name(b"Type1C")),
                ],
                program,
            ),
        );
        let font = builder.allocate();
        builder.add_object(
            font,
            Obj::Dict(vec![
                (Bytes::copy_from_slice(b"Type"), name(b"Font")),
                (Bytes::copy_from_slice(b"Subtype"), name(b"Type1")),
                (Bytes::copy_from_slice(b"BaseFont"), name(b"SelisCffSubr")),
                (
                    Bytes::copy_from_slice(b"Encoding"),
                    name(b"WinAnsiEncoding"),
                ),
                (Bytes::copy_from_slice(b"FirstChar"), Obj::Int(32)),
                (Bytes::copy_from_slice(b"LastChar"), Obj::Int(126)),
                (
                    Bytes::copy_from_slice(b"FontDescriptor"),
                    Obj::Dict(vec![
                        (Bytes::copy_from_slice(b"FontName"), name(b"SelisCffSubr")),
                        (
                            Bytes::copy_from_slice(b"FontFile3"),
                            Obj::Ref(Ref::new(file_num, 0)),
                        ),
                    ]),
                ),
            ]),
        );
        let resources = builder.allocate();
        builder.add_object(
            resources,
            Obj::Dict(vec![(
                Bytes::copy_from_slice(b"Font"),
                Obj::Dict(vec![(
                    Bytes::copy_from_slice(b"F1"),
                    Obj::Ref(Ref::new(font, 0)),
                )]),
            )]),
        );
        let content = b"BT /F1 48 Tf 72 700 Td (ABC) Tj ET\n";
        let content_num = builder.allocate();
        builder.add_object(
            content_num,
            stream(
                vec![(
                    Bytes::copy_from_slice(b"Length"),
                    Obj::Int(i64::try_from(content.len()).unwrap_or(i64::MAX)),
                )],
                content,
            ),
        );
        builder.add_page_with(
            612.0,
            792.0,
            &[Ref::new(content_num, 0)],
            Some(Ref::new(resources, 0)),
        );
        builder.write(&budget, &mut g).expect("write CFF fixture")
    }

    /// A one-page Identity-H PDF backed by a CID-keyed CFF program.
    fn cff_cid_pdf(program: &[u8]) -> Vec<u8> {
        let budget = Budget::unlimited();
        let clock = selis_sandbox::shell_clock();
        let mut g = budget.guard_with(&clock, selis_sandbox::CancelToken::new());
        let mut builder = DocumentBuilder::new();
        let file_num = builder.allocate();
        builder.add_object(
            file_num,
            stream(
                vec![
                    (
                        Bytes::copy_from_slice(b"Length"),
                        Obj::Int(i64::try_from(program.len()).unwrap_or(i64::MAX)),
                    ),
                    (
                        Bytes::copy_from_slice(b"Length1"),
                        Obj::Int(i64::try_from(program.len()).unwrap_or(i64::MAX)),
                    ),
                    (Bytes::copy_from_slice(b"Subtype"), name(b"CIDFontType0C")),
                ],
                program,
            ),
        );
        let descendant = builder.allocate();
        builder.add_object(
            descendant,
            Obj::Dict(vec![
                (Bytes::copy_from_slice(b"Type"), name(b"Font")),
                (Bytes::copy_from_slice(b"Subtype"), name(b"CIDFontType0")),
                (Bytes::copy_from_slice(b"BaseFont"), name(b"SelisCidSubr")),
                (
                    Bytes::copy_from_slice(b"FontDescriptor"),
                    Obj::Dict(vec![
                        (Bytes::copy_from_slice(b"FontName"), name(b"SelisCidSubr")),
                        (
                            Bytes::copy_from_slice(b"FontFile3"),
                            Obj::Ref(Ref::new(file_num, 0)),
                        ),
                    ]),
                ),
                (Bytes::copy_from_slice(b"CIDToGIDMap"), name(b"Identity")),
                (Bytes::copy_from_slice(b"DW"), Obj::Int(600)),
            ]),
        );
        let font = builder.allocate();
        builder.add_object(
            font,
            Obj::Dict(vec![
                (Bytes::copy_from_slice(b"Type"), name(b"Font")),
                (Bytes::copy_from_slice(b"Subtype"), name(b"Type0")),
                (Bytes::copy_from_slice(b"BaseFont"), name(b"SelisCidSubr")),
                (Bytes::copy_from_slice(b"Encoding"), name(b"Identity-H")),
                (
                    Bytes::copy_from_slice(b"DescendantFonts"),
                    Obj::Array(vec![Obj::Ref(Ref::new(descendant, 0))]),
                ),
            ]),
        );
        let resources = builder.allocate();
        builder.add_object(
            resources,
            Obj::Dict(vec![(
                Bytes::copy_from_slice(b"Font"),
                Obj::Dict(vec![(
                    Bytes::copy_from_slice(b"F1"),
                    Obj::Ref(Ref::new(font, 0)),
                )]),
            )]),
        );
        let content = b"BT /F1 48 Tf 72 700 Td <000100020004> Tj ET\n";
        let content_num = builder.allocate();
        builder.add_object(
            content_num,
            stream(
                vec![(
                    Bytes::copy_from_slice(b"Length"),
                    Obj::Int(i64::try_from(content.len()).unwrap_or(i64::MAX)),
                )],
                content,
            ),
        );
        builder.add_page_with(
            612.0,
            792.0,
            &[Ref::new(content_num, 0)],
            Some(Ref::new(resources, 0)),
        );
        builder
            .write(&budget, &mut g)
            .expect("write CID CFF fixture")
    }

    /// A one-page Type0/Identity-H document embedding NotoSans Devanagari,
    /// showing two CIDs as a 2-byte hex string.
    fn identity_h_pdf() -> Vec<u8> {
        let budget = Budget::unlimited();
        let clock = selis_sandbox::shell_clock();
        let mut g = budget.guard_with(&clock, selis_sandbox::CancelToken::new());
        let mut builder = DocumentBuilder::new();

        let font_bytes = DEVANAGARI;
        let file_num = builder.allocate();
        builder.add_object(
            file_num,
            stream(
                vec![
                    (
                        Bytes::copy_from_slice(b"Length"),
                        Obj::Int(i64::try_from(font_bytes.len()).unwrap_or(i64::MAX)),
                    ),
                    (
                        Bytes::copy_from_slice(b"Length1"),
                        Obj::Int(i64::try_from(font_bytes.len()).unwrap_or(i64::MAX)),
                    ),
                ],
                font_bytes,
            ),
        );
        let to_unicode = b"/CIDInit /ProcSet findresource begin\n12 dict begin\nbegincmap\n/CIDSystemInfo <</Registry (Adobe)/Ordering (UCS)/Supplement 0>> def\n/CMapName /Adobe-Identity-UCS def\n/CMapType 2 def\n1 begincodespacerange\n<0000> <FFFF>\nendcodespacerange\n2 beginbfchar\n<0038> <0915>\n<0020> <093F>\nendbfchar\nendcmap\nCMapName currentdict /CMap defineresource pop\nend\nend\n";
        let to_unicode_num = builder.allocate();
        builder.add_object(
            to_unicode_num,
            stream(
                vec![(
                    Bytes::copy_from_slice(b"Length"),
                    Obj::Int(i64::try_from(to_unicode.len()).unwrap_or(i64::MAX)),
                )],
                to_unicode,
            ),
        );
        let descendant = builder.allocate();
        builder.add_object(
            descendant,
            Obj::Dict(vec![
                (Bytes::copy_from_slice(b"Type"), name(b"Font")),
                (Bytes::copy_from_slice(b"Subtype"), name(b"CIDFontType2")),
                (
                    Bytes::copy_from_slice(b"BaseFont"),
                    name(b"NotoSansDevanagari"),
                ),
                (
                    Bytes::copy_from_slice(b"FontDescriptor"),
                    Obj::Dict(vec![
                        (
                            Bytes::copy_from_slice(b"FontName"),
                            name(b"NotoSansDevanagari"),
                        ),
                        (
                            Bytes::copy_from_slice(b"FontFile2"),
                            Obj::Ref(Ref::new(file_num, 0)),
                        ),
                    ]),
                ),
                (Bytes::copy_from_slice(b"CIDToGIDMap"), name(b"Identity")),
                (Bytes::copy_from_slice(b"DW"), Obj::Int(1000)),
            ]),
        );
        let font = builder.allocate();
        builder.add_object(
            font,
            Obj::Dict(vec![
                (Bytes::copy_from_slice(b"Type"), name(b"Font")),
                (Bytes::copy_from_slice(b"Subtype"), name(b"Type0")),
                (
                    Bytes::copy_from_slice(b"BaseFont"),
                    name(b"NotoSansDevanagari"),
                ),
                (Bytes::copy_from_slice(b"Encoding"), name(b"Identity-H")),
                (
                    Bytes::copy_from_slice(b"DescendantFonts"),
                    Obj::Array(vec![Obj::Ref(Ref::new(descendant, 0))]),
                ),
                (
                    Bytes::copy_from_slice(b"ToUnicode"),
                    Obj::Ref(Ref::new(to_unicode_num, 0)),
                ),
            ]),
        );
        let resources = builder.allocate();
        builder.add_object(
            resources,
            Obj::Dict(vec![(
                Bytes::copy_from_slice(b"Font"),
                Obj::Dict(vec![(
                    Bytes::copy_from_slice(b"F1"),
                    Obj::Ref(Ref::new(font, 0)),
                )]),
            )]),
        );
        let content = b"BT /F1 48 Tf 72 700 Td <00380020> Tj ET\n";
        let content_num = builder.allocate();
        builder.add_object(
            content_num,
            stream(
                vec![(
                    Bytes::copy_from_slice(b"Length"),
                    Obj::Int(i64::try_from(content.len()).unwrap_or(i64::MAX)),
                )],
                content,
            ),
        );
        builder.add_page_with(
            612.0,
            792.0,
            &[Ref::new(content_num, 0)],
            Some(Ref::new(resources, 0)),
        );
        builder.write(&budget, &mut g).expect("write fixture")
    }

    /// Recover all page text through the engine's own extraction pipeline.
    fn extracted_text(bytes: &[u8]) -> String {
        use selis_pdf_text::{apply_unicode_recovery, assemble, gather_glyphs, to_text};
        let budget = Budget::unlimited();
        let clock = selis_sandbox::shell_clock();
        let session =
            selis_pdf_engine::Session::open(bytes.to_vec(), &budget, &clock).expect("open output");
        let mut g = budget.guard_with(&clock, selis_sandbox::CancelToken::new());
        let mut out = String::new();
        for page in 0..session.len() {
            let dl = session
                .page_text_display_list(page, &budget, &mut g)
                .expect("display list");
            let mut glyphs = gather_glyphs(&dl, session.page_visible_box(page));
            apply_unicode_recovery(&mut glyphs, &mut |font, code| {
                session.text_unicode(page, font, code, &budget, &mut g)
            });
            let lines = assemble(glyphs);
            let texts: Vec<String> = lines
                .iter()
                .map(|line| {
                    let mut text = String::new();
                    for word in &line.words {
                        for run in &word.runs {
                            for glyph in &run.glyphs {
                                if let Some(ch) = char::from_u32(u32::from(glyph.code)) {
                                    text.push(ch);
                                }
                            }
                        }
                    }
                    text
                })
                .collect();
            out.push_str(&to_text(&lines, &texts, false));
        }
        out
    }

    /// Render page 0 of both documents and compare the actual pixels. The
    /// raster path is deterministic for a given byte stream (ADR-P0012), so
    /// a faithful subset must produce an identical pixmap.
    fn render_check(source: &[u8], output: &[u8]) -> (usize, usize, usize) {
        let render = |bytes: &[u8]| {
            let budget = Budget::unlimited();
            let clock = selis_sandbox::shell_clock();
            let session =
                selis_pdf_engine::Session::open(bytes.to_vec(), &budget, &clock).expect("open");
            let mut g = budget.guard_with(&clock, selis_sandbox::CancelToken::new());
            session
                .render_page_rgba(0, 72.0, &budget, &mut g)
                .expect("render")
        };
        let before = render(source);
        let after = render(output);
        assert_eq!(before.width, after.width);
        assert_eq!(before.height, after.height);
        let inked = |rgba: &[u8]| {
            rgba.chunks(4)
                .filter(|pixel| pixel.first().copied().unwrap_or(255) < 250)
                .count()
        };
        let before_inked = inked(&before.rgba);
        let after_inked = inked(&after.rgba);
        let differing = before
            .rgba
            .iter()
            .zip(after.rgba.iter())
            .filter(|(a, b)| a != b)
            .count();
        assert_eq!(differing, 0, "subset renders pixel-identically");
        (before_inked, after_inked, differing)
    }

    #[test]
    fn winansi_subset_is_smaller_valid_and_keeps_text() {
        let dir = temp_dir("winansi");
        let input = dir.join("in.pdf");
        let output = dir.join("out.pdf");
        let source = winansi_pdf();
        std::fs::write(&input, &source).expect("write input");
        let before = extracted_text(&source);
        assert!(
            before.contains("Hello") && before.contains("world"),
            "fixture extracts before subset: {before:?}"
        );

        let (report, verification) =
            subset_fonts(input.to_str().unwrap(), output.to_str().unwrap()).expect("subset fonts");
        assert_eq!(verification.verdict, "ok");
        assert_eq!(verification.pages_out, 1);
        assert_eq!(report.fonts_subset, 1);
        // The measured report is mirrored onto the verification object that
        // both the CLI `--json` twin and the web card consume.
        assert_eq!(verification.fonts_subset, Some(1));
        assert_eq!(
            verification.glyphs_kept,
            u64::try_from(report.glyphs_kept).ok()
        );
        assert!(
            report.glyphs_kept >= 8,
            "kept at least the distinct letters of the text"
        );
        assert!(
            report.out_bytes < report.in_bytes,
            "subset {} < {} bytes",
            report.out_bytes,
            report.in_bytes
        );
        let bytes = std::fs::read(&output).expect("read output");
        // Re-open the OUTPUT and extract again — never match source bytes.
        let after = extracted_text(&bytes);
        let (ink_before, ink_after, differing) = render_check(&source, &bytes);
        eprintln!(
            "WINANSI EVIDENCE: input={} output={} kept={} inked={}->{} diffpx={} before={:?} after={:?}",
            source.len(),
            bytes.len(),
            report.glyphs_kept,
            ink_before,
            ink_after,
            differing,
            before,
            after
        );
        assert_eq!(ink_before, ink_after, "same number of inked pixels");
        assert!(bytes.len() < source.len());
        assert_eq!(after, before, "text survives the subset: {after:?}");

        // The font stream in the OUTPUT is a parseable subset with a much
        // smaller glyph count than the 139 KiB source program.
        let subset_program = embedded_font_program(&bytes);
        assert!(
            subset_program.len() < LIBERATION.len() / 4,
            "subset program {} bytes vs source {}",
            subset_program.len(),
            LIBERATION.len()
        );
        let count = selis_font::glyph_count(&Bytes::copy_from_slice(&subset_program))
            .expect("subset program parses as a TrueType font");
        assert!(count < 100, "subset keeps a small glyph set, got {count}");
        // At least one kept glyph outlines correctly through the engine.
        let outline_budget = Budget::unlimited();
        let mut g = outline_budget.guard();
        let program = Bytes::copy_from_slice(&subset_program);
        let outlined = (0..count).any(|gid| {
            selis_font::outline_glyph(&program, gid, &mut g)
                .ok()
                .flatten()
                .and_then(|outline| outline.bbox)
                .is_some()
        });
        assert!(outlined, "a kept glyph has a drawable outline");
    }

    #[test]
    fn identity_h_subset_is_smaller_valid_and_keeps_text() {
        let dir = temp_dir("identity-h");
        let input = dir.join("in.pdf");
        let output = dir.join("out.pdf");
        let source = identity_h_pdf();
        std::fs::write(&input, &source).expect("write input");
        let before = extracted_text(&source);
        assert!(
            !before.trim().is_empty(),
            "fixture extracts before subset: {before:?}"
        );

        let (report, verification) =
            subset_fonts(input.to_str().unwrap(), output.to_str().unwrap()).expect("subset fonts");
        assert_eq!(verification.verdict, "ok");
        assert_eq!(report.fonts_subset, 1);
        assert!(report.out_bytes < report.in_bytes);
        let bytes = std::fs::read(&output).expect("read output");
        let after = extracted_text(&bytes);
        let (ink_before, ink_after, differing) = render_check(&source, &bytes);
        eprintln!(
            "IDENTITY-H EVIDENCE: input={} output={} kept={} inked={}->{} diffpx={} before={:?} after={:?}",
            source.len(),
            bytes.len(),
            report.glyphs_kept,
            ink_before,
            ink_after,
            differing,
            before,
            after
        );
        assert_eq!(ink_before, ink_after, "same number of inked pixels");
        assert_eq!(after, before, "CIDs still recover Unicode");

        let subset_program = embedded_font_program(&bytes);
        assert!(subset_program.len() < DEVANAGARI.len() / 4);
        let count = selis_font::glyph_count(&Bytes::copy_from_slice(&subset_program))
            .expect("subset program parses");
        assert!(
            count < 100,
            "CID subset keeps a small glyph set, got {count}"
        );
        // The rewritten /CIDToGIDMap is present: codes still select outlines.
        let text = String::from_utf8_lossy(&bytes);
        assert!(
            text.contains("/CIDToGIDMap"),
            "a CIDToGIDMap stream was written"
        );
    }

    #[test]
    fn a_pdf_without_an_used_embedded_font_is_refused_loudly() {
        let dir = temp_dir("no-fonts");
        let input = dir.join("in.pdf");
        let output = dir.join("out.pdf");
        let budget = Budget::unlimited();
        let clock = selis_sandbox::shell_clock();
        let mut g = budget.guard_with(&clock, selis_sandbox::CancelToken::new());
        let mut builder = DocumentBuilder::new();
        let content = b"BT (plain) Tj ET\n".to_vec();
        let content_num = builder.allocate();
        builder.add_object(
            content_num,
            stream(
                vec![(
                    Bytes::copy_from_slice(b"Length"),
                    Obj::Int(i64::try_from(content.len()).unwrap_or(i64::MAX)),
                )],
                &content,
            ),
        );
        builder.add_page(200.0, 300.0, &content);
        std::fs::write(&input, builder.write(&budget, &mut g).expect("write")).expect("input");

        let err = subset_fonts(input.to_str().unwrap(), output.to_str().unwrap())
            .expect_err("no eligible font must fail");
        assert!(
            err.0.contains("no embedded TrueType font"),
            "typed reason surfaced: {}",
            err.0
        );
        assert!(!output.exists(), "no output committed on failure");
    }

    /// Pull the first `/FontFile2` stream from the OUTPUT document bytes,
    /// independent of any source-string matching.
    fn embedded_font_program(bytes: &[u8]) -> Vec<u8> {
        let budget = Budget::unlimited();
        let clock = selis_sandbox::shell_clock();
        let mut g = budget.guard_with(&clock, selis_sandbox::CancelToken::new());
        let start = selis_pdf_cos::xref::find_startxref(bytes, 4096).unwrap_or(0);
        let doc = selis_pdf_cos::parse_revisions(bytes, start, &budget, &mut g).expect("parse");
        let mut best = Vec::new();
        for num in 1..64u32 {
            let reference = Ref::new(num, 0);
            let Ok(obj) = selis_pdf_cos::copy::resolve_ref(bytes, &doc, reference, &budget, &mut g)
            else {
                continue;
            };
            if let Obj::Stream { dict, data } = obj {
                let decoded = if dict.iter().any(|(key, value)| {
                    key.as_slice() == b"Filter"
                        && matches!(value, Obj::Name(n) if n.as_slice() == b"FlateDecode")
                }) {
                    selis_pdf_filter::flate_decode_bounded(&data, 256 * 1024 * 1024, &mut g)
                        .unwrap_or_default()
                } else {
                    data.as_slice().to_vec()
                };
                if decoded.len() > best.len() && decoded.starts_with(b"\x00\x01\x00\x00") {
                    best = decoded;
                }
            }
        }
        assert!(
            !best.is_empty(),
            "an embedded TrueType program exists in output"
        );
        best
    }

    /// Pull the largest `OTTO`-wrapped CFF program out of a rewritten document.
    fn embedded_otf_program(bytes: &[u8]) -> Vec<u8> {
        let budget = Budget::unlimited();
        let clock = selis_sandbox::shell_clock();
        let mut g = budget.guard_with(&clock, selis_sandbox::CancelToken::new());
        let start = selis_pdf_cos::xref::find_startxref(bytes, 4096).unwrap_or(0);
        let doc = selis_pdf_cos::parse_revisions(bytes, start, &budget, &mut g).expect("parse");
        let mut best = Vec::new();
        for num in 1..64u32 {
            let Ok(obj) =
                selis_pdf_cos::copy::resolve_ref(bytes, &doc, Ref::new(num, 0), &budget, &mut g)
            else {
                continue;
            };
            if let Obj::Stream { dict, data } = obj {
                let decoded = if dict.iter().any(|(key, value)| {
                    key.as_slice() == b"Filter"
                        && matches!(value, Obj::Name(n) if n.as_slice() == b"FlateDecode")
                }) {
                    selis_pdf_filter::flate_decode_bounded(&data, 256 * 1024 * 1024, &mut g)
                        .unwrap_or_default()
                } else {
                    data.as_slice().to_vec()
                };
                if decoded.len() > best.len() && decoded.starts_with(b"OTTO") {
                    best = decoded;
                }
            }
        }
        assert!(
            !best.is_empty(),
            "an embedded OTF/CFF program exists in output"
        );
        best
    }

    /// CFF (FontFile3 / Type1C) subsetting end-to-end: the output's embedded
    /// font is re-opened with skrifa and the page is rendered pixel-identically
    /// against the source. This is the same evidence the TrueType path ships.
    #[test]
    fn cff_subset_is_smaller_valid_and_pixel_identical() {
        let dir = temp_dir("cff");
        let input = dir.join("in.pdf");
        let output = dir.join("out.pdf");
        let source = cff_pdf(include_bytes!(
            "../../../crates/selis-font/tests/fixtures/cff-subr.otf"
        ));
        std::fs::write(&input, &source).expect("write input");

        let (report, verification) =
            subset_fonts(input.to_str().unwrap(), output.to_str().unwrap()).expect("subset CFF");
        assert_eq!(verification.verdict, "ok");
        assert_eq!(report.fonts_subset, 1);
        assert!(report.out_bytes < report.in_bytes);
        let bytes = std::fs::read(&output).expect("read output");
        assert!(bytes.len() < source.len());

        // Re-open the OUTPUT font with skrifa through the engine's own font
        // loader, not by matching source bytes.
        let subset_program = embedded_otf_program(&bytes);
        let font = skrifa::FontRef::new(&subset_program).expect("output CFF reopens in skrifa");
        use skrifa::MetadataProvider;
        assert!(
            font.metrics(
                skrifa::instance::Size::unscaled(),
                skrifa::instance::LocationRef::default()
            )
            .glyph_count
                > 1
        );

        let (ink_before, ink_after, differing) = render_check(&source, &bytes);
        eprintln!(
            "CFF EVIDENCE: input={} output={} kept={} subset_font={}->{} inked={}->{} diffpx={}",
            source.len(),
            bytes.len(),
            report.glyphs_kept,
            include_bytes!("../../../crates/selis-font/tests/fixtures/cff-subr.otf").len(),
            subset_program.len(),
            ink_before,
            ink_after,
            differing
        );
        assert_eq!(differing, 0, "CFF subset renders pixel-identically");
        assert_eq!(ink_before, ink_after);
        assert!(
            subset_program.len()
                < include_bytes!("../../../crates/selis-font/tests/fixtures/cff-subr.otf").len(),
            "the embedded CFF stream is strictly smaller"
        );
    }

    /// CID-keyed CFF (FDArray/FDSelect) subsetting, verified the same way.
    #[test]
    fn cid_cff_subset_is_smaller_valid_and_pixel_identical() {
        let source_fixture =
            include_bytes!("../../../crates/selis-font/tests/fixtures/cff-cid-subr.otf");
        let dir = temp_dir("cff-cid");
        let input = dir.join("in.pdf");
        let output = dir.join("out.pdf");
        let source = cff_cid_pdf(source_fixture);
        std::fs::write(&input, &source).expect("write input");

        let (report, verification) =
            subset_fonts(input.to_str().unwrap(), output.to_str().unwrap())
                .expect("subset CID CFF");
        assert_eq!(verification.verdict, "ok");
        assert_eq!(report.fonts_subset, 1);
        let bytes = std::fs::read(&output).expect("read output");
        assert!(bytes.len() < source.len());
        let subset_program = embedded_otf_program(&bytes);
        let font = skrifa::FontRef::new(&subset_program).expect("output CID CFF reopens");
        use skrifa::MetadataProvider;
        assert!(
            font.metrics(
                skrifa::instance::Size::unscaled(),
                skrifa::instance::LocationRef::default()
            )
            .glyph_count
                > 1
        );
        let (ink_before, ink_after, differing) = render_check(&source, &bytes);
        eprintln!(
            "CID CFF EVIDENCE: input={} output={} kept={} subset_font={}->{} inked={}->{} diffpx={}",
            source.len(),
            bytes.len(),
            report.glyphs_kept,
            source_fixture.len(),
            subset_program.len(),
            ink_before,
            ink_after,
            differing
        );
        assert_eq!(differing, 0, "CID CFF subset renders pixel-identically");
        assert_eq!(ink_before, ink_after);
        assert!(subset_program.len() < source_fixture.len());
        assert!(
            String::from_utf8_lossy(&bytes).contains("/CIDToGIDMap"),
            "the CIDToGIDMap stream was rewritten"
        );
    }
}
