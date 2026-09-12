//! The SL-3.CONF.01 text normaliser.
//!
//! Text comparison is subtler than pixels: two correct extractors legitimately
//! disagree on whitespace runs, line-break placement, soft hyphens, ligature
//! spellings, and bidi control characters, while still recovering the same
//! document text. Comparing raw bytes would measure formatting choices, not
//! extraction correctness. This module writes the normalisation down — every
//! rule is numbered, unit-tested, and applied identically to both sides before
//! the similarity is computed.
//!
//! Rules:
//! * **N1 (format controls):** strip U+FEFF (BOM / zero-width no-break space)
//!   and the Unicode bidi control characters (U+200E, U+200F, U+202A–U+202E,
//!   U+2066–U+2069). Rationale: extractors differ in whether they surface the
//!   directionality hints the content stream carries; the hints are not
//!   document text. U+200C/U+200D (ZWNJ/ZWJ) are *kept*: they change Indic
//!   shaping and are content.
//! * **N2 (line endings):** CRLF and CR fold to LF.
//! * **N3 (soft hyphen):** U+00AD is deleted everywhere. Rationale: whether a
//!   soft hyphen surfaces depends on line-breaking, not on the text.
//! * **N4 (hard hyphen at a line break):** a `-` immediately before a line
//!   break, with a word character on both sides of the break, is a
//!   hyphenation artefact — `word-\nwrap` becomes `wordwrap`. A hyphen anywhere
//!   else (em-dash-adjacent, minus signs, trailing punctuation) is kept.
//! * **N5 (ligatures):** the seven Latin presentation ligatures U+FB00–U+FB06
//!   fold to their ASCII expansions (U+FB01 fi ligature becomes "fi"). Rationale: ToUnicode CMaps
//!   spell these inconsistently across producers.
//! * **N6 (whitespace):** every maximal run of whitespace folds to one ASCII
//!   space, and the ends are trimmed. Rationale: line/word segmentation is a
//!   layout inference (SL-3.TEXT.03/04); penalising its line breaks as text
//!   errors would drown real recovery failures. Reading-order differences
//!   (column interleaving) are *not* forgiven — they survive as distance.
//!
//! Deliberately NOT normalised (measuring these is the point of the sweep):
//! * bidi *mirroring* is not folded (`(` stays `(`). Folding mirrored pairs
//!   would corrupt genuine content; visual-vs-logical order divergence shows
//!   up as distance and is triaged via the `rtl_heavy` flag instead.
//! * case, quotation marks, dashes, and compatibility forms outside N5 are
//!   kept — mojibake from a broken ToUnicode path must stay visible.
//!
//! Similarity is the normalised character-level Levenshtein similarity
//! `1 − dist / max(len)`, in `[0.0, 1.0]`, with common-prefix/suffix trimming
//! (near-identical texts compare in linear time). Texts longer than
//! [`MAX_COMPARE_CHARS`] are truncated for the *comparison only* — the CORP.03
//! golden hash is always over the full normalised text — and the truncation is
//! reported so the sweep can flag it.

/// Comparison budget: texts longer than this many characters are truncated
/// before the quadratic similarity step (page 1 of a text-heavy page can
/// exceed 100 KiB; the DP over two such strings is billions of cell updates).
/// The truncation is flagged, never silent.
pub const MAX_COMPARE_CHARS: usize = 32_768;

/// Fold one character per rules N1/N3/N5: returns `None` for characters the
/// normaliser deletes, `Some(expansion)` otherwise (ligatures expand to two
/// or three ASCII characters).
fn fold_char(c: char) -> Option<&'static str> {
    match c {
        // N1: BOM + bidi controls are not text.
        '\u{FEFF}' | '\u{200E}' | '\u{200F}' | '\u{202A}' | '\u{202B}' | '\u{202C}'
        | '\u{202D}' | '\u{202E}' | '\u{2066}' | '\u{2067}' | '\u{2068}' | '\u{2069}' => None,
        // N3: soft hyphen is a line-breaking hint, not text.
        '\u{00AD}' => None,
        // N5: presentation ligatures fold to ASCII.
        '\u{FB00}' => Some("ff"),
        '\u{FB01}' => Some("fi"),
        '\u{FB02}' => Some("fl"),
        '\u{FB03}' => Some("ffi"),
        '\u{FB04}' => Some("ffl"),
        '\u{FB05}' => Some("st"),
        '\u{FB06}' => Some("st"),
        _ => None,
    }
}

/// True for the word characters that license hyphenation joining (rule N4):
/// letters and numbers on both sides of a `-\n` break.
fn is_word_char(c: char) -> bool {
    c.is_alphanumeric()
}

/// Normalise extracted text per rules N1–N6 (see the module docs).
#[must_use]
pub fn normalize(text: &str) -> String {
    // Pass 1: N2 line-ending fold + N4 dehyphenation need adjacency, so work
    // on the char stream with one character of lookahead handled via a small
    // state machine rather than a regex (xtask stays dependency-free).
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let mut i = 0usize;
    while i < chars.len() {
        let Some(&c) = chars.get(i) else {
            break;
        };
        // N2: CRLF/CR → LF.
        if c == '\r' {
            if chars.get(i + 1) == Some(&'\n') {
                i += 1;
            }
            out.push('\n');
            i += 1;
            continue;
        }
        // N4: `word-\nword` → `wordword`. The hyphen dies with the break;
        // anything else keeps both characters for the later passes.
        if c == '-'
            && chars.get(i + 1) == Some(&'\n')
            && i > 0
            && chars
                .get(i.wrapping_sub(1))
                .is_some_and(|&p| is_word_char(p))
            && chars.get(i + 2).is_some_and(|&n| is_word_char(n))
        {
            i += 2;
            continue;
        }
        if let Some(expansion) = fold_char(c) {
            // N5: a ligature expands to ASCII.
            out.push_str(expansion);
        } else if !is_deleted(c) {
            out.push(c);
        }
        // N1/N3 (`is_deleted`) characters are dropped.
        i += 1;
    }
    // Pass 2 (N6): collapse whitespace runs to one space, trim the ends. A
    // space is pushed only when the next non-whitespace char arrives, so
    // leading and trailing runs vanish by construction.
    let mut collapsed = String::with_capacity(out.len());
    let mut in_ws = true;
    for c in out.chars() {
        if c.is_whitespace() {
            in_ws = true;
        } else {
            if in_ws && !collapsed.is_empty() {
                collapsed.push(' ');
            }
            in_ws = false;
            collapsed.push(c);
        }
    }
    collapsed
}

/// True when `fold_char(c)` deletes `c` (rules N1/N3) — the explicit
/// membership test that keeps the pass-1 branch above total.
fn is_deleted(c: char) -> bool {
    matches!(
        c,
        '\u{FEFF}'
            | '\u{200E}'
            | '\u{200F}'
            | '\u{202A}'
            | '\u{202B}'
            | '\u{202C}'
            | '\u{202D}'
            | '\u{202E}'
            | '\u{2066}'
            | '\u{2067}'
            | '\u{2068}'
            | '\u{2069}'
            | '\u{00AD}'
    )
}

/// Truncate over-long texts to [`MAX_COMPARE_CHARS`] for the similarity step.
/// Returns the (possibly truncated) pair and whether truncation happened.
#[must_use]
pub fn truncate_for_compare(a: &str, b: &str) -> (String, String, bool) {
    let a_count = a.chars().count();
    let b_count = b.chars().count();
    if a_count <= MAX_COMPARE_CHARS && b_count <= MAX_COMPARE_CHARS {
        return (a.to_string(), b.to_string(), false);
    }
    fn head(s: &str) -> String {
        s.chars().take(MAX_COMPARE_CHARS).collect()
    }
    (head(a), head(b), true)
}

/// The normalised character-level Levenshtein similarity of two
/// (already normalised) texts, in `[0.0, 1.0]`. Both empty is 1.0; exactly
/// one empty is 0.0. Common prefix/suffix trimming keeps near-identical
/// texts linear; the two-row DP keeps memory linear in the shorter text.
#[must_use]
pub fn similarity(a: &str, b: &str) -> f64 {
    let mut a: Vec<char> = a.chars().collect();
    let mut b: Vec<char> = b.chars().collect();
    // Trim the common prefix and suffix: identical texts short-circuit to
    // 1.0 and one-sided insertions stay linear.
    let mut prefix = 0usize;
    while prefix < a.len() && prefix < b.len() && a.get(prefix) == b.get(prefix) {
        prefix += 1;
    }
    let mut suffix = 0usize;
    while suffix < a.len().saturating_sub(prefix)
        && suffix < b.len().saturating_sub(prefix)
        && a.get(a.len() - 1 - suffix) == b.get(b.len() - 1 - suffix)
    {
        suffix += 1;
    }
    let a_mid = a.len().saturating_sub(prefix).saturating_sub(suffix);
    let b_mid = b.len().saturating_sub(prefix).saturating_sub(suffix);
    if a_mid == 0 && b_mid == 0 {
        return 1.0;
    }
    if a_mid == 0 || b_mid == 0 {
        return 0.0;
    }
    // Drain the trimmed middle (bounded copies of the differing region).
    a = a.into_iter().skip(prefix).take(a_mid).collect();
    b = b.into_iter().skip(prefix).take(b_mid).collect();
    let dist = levenshtein_rows(&a, &b);
    let max_len = a_mid.max(b_mid) + prefix + suffix;
    if max_len == 0 {
        return 1.0;
    }
    1.0 - dist as f64 / max_len as f64
}

/// Two-row Levenshtein distance over character vectors.
fn levenshtein_rows(a: &[char], b: &[char]) -> usize {
    let (short, long) = if a.len() <= b.len() { (a, b) } else { (b, a) };
    let mut prev: Vec<usize> = (0..=short.len()).collect();
    let mut curr = vec![0usize; short.len() + 1];
    for (i, &lc) in long.iter().enumerate() {
        curr[0] = i + 1;
        for (j, &sc) in short.iter().enumerate() {
            let cost = usize::from(lc != sc);
            let deletion = prev[j + 1] + 1;
            let insertion = curr[j] + 1;
            let substitution = prev[j] + cost;
            curr[j + 1] = deletion.min(insertion).min(substitution);
        }
        std::mem::swap(&mut prev, &mut curr);
    }
    prev[short.len()]
}

/// True when more than 2% of the characters are RTL-script (Hebrew, Arabic
/// and neighbours, presentation forms). Such files exercise the visual-vs-
/// logical order path both extractors may legitimately disagree on, so the
/// sweep triages them separately instead of filing them as plain recovery
/// failures. Mirrored brackets are *not* folded (see the module docs).
#[must_use]
pub fn rtl_heavy(text: &str) -> bool {
    let mut total = 0usize;
    let mut rtl = 0usize;
    for c in text.chars() {
        if c.is_whitespace() {
            continue;
        }
        total += 1;
        if matches!(c,
            '\u{0590}'..='\u{08FF}' | '\u{FB1D}'..='\u{FDFD}' | '\u{FE70}'..='\u{FEFF}')
        {
            rtl += 1;
        }
    }
    total > 0 && rtl * 50 > total
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn n1_strips_bom_and_bidi_controls_but_keeps_zwj() {
        assert_eq!(normalize("\u{FEFF}abc"), "abc");
        assert_eq!(normalize("a\u{200E}b\u{200F}c"), "abc");
        assert_eq!(normalize("a\u{202A}b\u{202C}c"), "abc");
        // ZWNJ/ZWJ are Indic content — kept.
        assert_eq!(normalize("a\u{200C}b\u{200D}c"), "a\u{200C}b\u{200D}c");
    }

    #[test]
    fn n2_folds_crlf_and_cr() {
        assert_eq!(normalize("a\r\nb\rc"), "a b c");
    }

    #[test]
    fn n3_deletes_soft_hyphens() {
        assert_eq!(normalize("ex\u{00AD}am\u{00AD}ple"), "example");
    }

    #[test]
    fn n4_joins_hyphenated_line_breaks_only() {
        // Hyphenation artefact: joined.
        assert_eq!(normalize("hyphen-\nation"), "hyphenation");
        // Not between word chars: kept.
        assert_eq!(normalize("well -\nknown"), "well - known");
        assert_eq!(normalize("-\nword"), "- word");
        assert_eq!(normalize("42 -\n7"), "42 - 7");
    }

    #[test]
    fn n5_folds_ligatures() {
        // U+FB00-U+FB06, spelled as escapes so the source stays ASCII.
        assert_eq!(
            normalize("\u{FB00}\u{FB01}\u{FB02}\u{FB03}\u{FB04}\u{FB05}\u{FB06}"),
            "fffiflffifflstst"
        );
        assert_eq!(normalize("\u{FB01}rst"), "first");
    }

    #[test]
    fn n6_collapses_whitespace_and_trims() {
        assert_eq!(normalize("  a\t\tb\n\n\rc  "), "a b c");
        assert_eq!(normalize(""), "");
        assert_eq!(normalize("   "), "");
    }

    #[test]
    fn similarity_is_one_for_identical_and_zero_for_disjoint() {
        assert_eq!(similarity("hello world", "hello world"), 1.0);
        assert_eq!(similarity("", ""), 1.0);
        assert_eq!(similarity("abc", ""), 0.0);
        assert_eq!(similarity("", "abc"), 0.0);
    }

    #[test]
    fn similarity_counts_characters_not_bytes() {
        // One substituted CJK char in four: 0.75, not byte-weighted.
        let sim = similarity(
            "\u{65E5}\u{672C}\u{8A9E}\u{6587}",
            "\u{65E5}\u{672C}X\u{6587}",
        );
        assert!((sim - 0.75).abs() < 1e-9, "got {sim}");
    }

    #[test]
    fn similarity_survives_long_common_affixes() {
        let side = "x".repeat(20_000);
        let a = format!("{side}AAA");
        let b = format!("{side}BBB");
        let t = std::time::Instant::now();
        let sim = similarity(&a, &b);
        assert!(t.elapsed() < std::time::Duration::from_secs(5));
        assert!((sim - (20_000.0 / 20_003.0)).abs() < 1e-9, "got {sim}");
    }

    #[test]
    fn truncate_flags_overlong_texts_only() {
        let (a, b, t) = truncate_for_compare("abc", "abc");
        assert!(!t && a == "abc" && b == "abc");
        let long = "y".repeat(MAX_COMPARE_CHARS + 1);
        let (a, _, t) = truncate_for_compare(&long, "short");
        assert!(t);
        assert_eq!(a.chars().count(), MAX_COMPARE_CHARS);
    }

    #[test]
    fn rtl_heavy_flags_arabic_and_hebrew_but_not_latin() {
        assert!(rtl_heavy("\u{0645}\u{0631}\u{062D}\u{0628}\u{0627} \u{0628}\u{0627}\u{0644}\u{0639}\u{0627}\u{0644}\u{0645}"));
        assert!(rtl_heavy(
            "\u{05E9}\u{05DC}\u{05D5}\u{05DD} \u{05E2}\u{05D5}\u{05DC}\u{05DD}"
        ));
        assert!(!rtl_heavy("hello world"));
        assert!(!rtl_heavy(""));
        // One RTL char in a long Latin sentence stays under the 2% bar.
        assert!(!rtl_heavy("the quick brown fox jumps over the lazy dog Pack my box with five dozen liquor jugs \u{0628}"));
    }
}
