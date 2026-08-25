//! TrueType cmap selection for embedded fonts (SL-3.FONT.02).
//!
//! The single messiest area of the spec (ISO 32000-2:2020 §9.6.6.4): a
//! TrueType font's character codes can map through several cmap subtables, and
//! which one applies depends on the descriptor's Symbolic flag and the font's
//! `/Encoding`. The rules implemented here, with citations:
//!
//! * **Nonsymbolic** (§9.6.6.4): codes go through the `/Encoding` (or the
//!   built-in encoding) to glyph *names*, which are matched via the font's
//!   `post` table. The cmap is used for character-level (Unicode) mapping:
//!   subtable (3,1) Windows Unicode BMP if present, else (1,0) Mac Roman.
//! * **Symbolic** (§9.6.6.4): codes map *directly* through the cmap — there
//!   is no glyph-name indirection. A `/MacRomanEncoding` (or no `/Encoding`)
//!   selects the (1,0) Mac Roman subtable; any other `/Encoding` selects the
//!   (3,0) Windows Symbol subtable.
//!
//! The selection is deliberately conservative: `None` means "no subtable
//! applies" and the caller falls back to its next strategy (the name path for
//! nonsymbolic, or `.notdef`).

use crate::encoding::BaseEncoding;

/// A TrueType cmap subtable platform/encoding pair.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CmapEncoding {
    /// (3, 1) — Windows Unicode BMP.
    WindowsUnicode,
    /// (3, 0) — Windows Symbol.
    WindowsSymbol,
    /// (1, 0) — Mac Roman.
    MacRoman,
}

/// Choose the cmap subtable for an embedded TrueType font.
///
/// # Arguments
///
/// * `symbolic` — the descriptor's Symbolic flag (bit 1 of `/Flags`).
/// * `encoding` — the resolved `/Encoding` base (the font's `Encoding.base`,
///   `None` meaning absent/builtin).
/// * `available` — the subtables present in the font's `cmap` table, in any
///   order.
#[must_use]
pub fn select_cmap(
    symbolic: bool,
    encoding: Option<BaseEncoding>,
    available: &[CmapEncoding],
) -> Option<CmapEncoding> {
    if symbolic {
        // §9.6.6.4: MacRoman /Encoding (or none/builtin) → the Mac Roman
        // subtable; any other /Encoding → the Windows Symbol subtable.
        let preference: &[CmapEncoding] = match encoding {
            Some(BaseEncoding::MacRoman) | None | Some(BaseEncoding::Builtin) => &[
                CmapEncoding::MacRoman,
                CmapEncoding::WindowsSymbol,
                CmapEncoding::WindowsUnicode,
            ],
            _ => &[
                CmapEncoding::WindowsSymbol,
                CmapEncoding::WindowsUnicode,
                CmapEncoding::MacRoman,
            ],
        };
        first_available(available, preference)
    } else {
        // §9.6.6.4: Windows Unicode BMP preferred, Mac Roman the fallback.
        // The Windows Symbol subtable is never used for a nonsymbolic font.
        first_available(
            available,
            &[CmapEncoding::WindowsUnicode, CmapEncoding::MacRoman],
        )
    }
}

/// The first entry of `preference` that is present in `available`.
fn first_available(
    available: &[CmapEncoding],
    preference: &[CmapEncoding],
) -> Option<CmapEncoding> {
    preference
        .iter()
        .copied()
        .find(|candidate| available.contains(candidate))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// DoD: table-driven test over the encoding decision matrix.
    /// (symbolic, encoding, available, expected).
    #[test]
    fn cmap_decision_matrix() {
        let all = [
            CmapEncoding::WindowsUnicode,
            CmapEncoding::WindowsSymbol,
            CmapEncoding::MacRoman,
        ];
        let cases: &[(
            &str,
            bool,
            Option<BaseEncoding>,
            &[CmapEncoding],
            Option<CmapEncoding>,
        )] = &[
            // Nonsymbolic: (3,1) preferred, (1,0) fallback, (3,0) never.
            (
                "non-sym all",
                false,
                None,
                &all,
                Some(CmapEncoding::WindowsUnicode),
            ),
            (
                "non-sym no 3,1",
                false,
                None,
                &[CmapEncoding::MacRoman, CmapEncoding::WindowsSymbol],
                Some(CmapEncoding::MacRoman),
            ),
            (
                "non-sym only symbol",
                false,
                None,
                &[CmapEncoding::WindowsSymbol],
                None,
            ),
            (
                "non-sym with winansi",
                false,
                Some(BaseEncoding::WinAnsi),
                &all,
                Some(CmapEncoding::WindowsUnicode),
            ),
            // Symbolic + MacRoman or absent: (1,0) preferred.
            (
                "sym macroman",
                true,
                Some(BaseEncoding::MacRoman),
                &all,
                Some(CmapEncoding::MacRoman),
            ),
            (
                "sym absent",
                true,
                None,
                &[CmapEncoding::MacRoman, CmapEncoding::WindowsUnicode],
                Some(CmapEncoding::MacRoman),
            ),
            // Symbolic + other encoding: (3,0) preferred.
            (
                "sym winansi",
                true,
                Some(BaseEncoding::WinAnsi),
                &all,
                Some(CmapEncoding::WindowsSymbol),
            ),
            (
                "sym standard",
                true,
                Some(BaseEncoding::Standard),
                &[CmapEncoding::WindowsUnicode, CmapEncoding::WindowsSymbol],
                Some(CmapEncoding::WindowsSymbol),
            ),
            // Symbolic + WinAnsi with no (3,0): falls to (3,1) then (1,0).
            (
                "sym winansi no 3,0",
                true,
                Some(BaseEncoding::WinAnsi),
                &[CmapEncoding::WindowsUnicode, CmapEncoding::MacRoman],
                Some(CmapEncoding::WindowsUnicode),
            ),
            // Nothing available.
            ("nothing available", true, None, &[], None),
        ];
        for (name, symbolic, encoding, available, expected) in cases {
            assert_eq!(
                select_cmap(*symbolic, *encoding, available),
                *expected,
                "case: {name}"
            );
        }
    }

    #[test]
    fn builtin_encoding_counts_as_absent_for_selection() {
        // A resolved builtin encoding (BaseEncoding::Builtin) behaves like
        // "no /Encoding" for the cmap decision.
        assert_eq!(
            select_cmap(
                true,
                Some(BaseEncoding::Builtin),
                &[CmapEncoding::MacRoman, CmapEncoding::WindowsUnicode],
            ),
            Some(CmapEncoding::MacRoman)
        );
    }
}
