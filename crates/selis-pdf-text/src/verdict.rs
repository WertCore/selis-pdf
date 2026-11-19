//! Typed extraction verdicts (SL-3.TEXT.15, SL-3.TEXT.10).
//!
//! A page-level extraction result is never a bare string: an empty result can
//! mean a genuinely blank page, an undecodable font (TEXT.10), or a page where
//! most glyphs were recovered but a meaningful tail could not be decoded
//! (TEXT.15). Those are different answers and a caller must be able to tell
//! them apart without re-deriving them from the text. This module is the typed
//! verdict the sweep's `text_err` channel and the engine's low-confidence path
//! share.
//!
//! # Local proxy, honest scope
//!
//! The plan's literal TEXT.15 clause — ">5 % of characters diverge with zero
//! recovery" — is a *differential* measurement: it needs both selis and an
//! oracle on the same file. The corpus and the oracle binaries are absent in
//! this tree, so that comparison cannot be run here. What **is** measurable
//! locally is the fraction of a page's drawn glyphs that render no character
//! at all (recovery returned `None` *and* the raw code is not a valid Unicode
//! scalar, e.g. a surrogate CID) — a valid byte the chain cannot improve still
//! renders under the legacy reading and is not a divergence signal.
//! [`classify_verdict`] turns that local count into a typed
//! signal; a `>5 %` tail is flagged as [`ExtractionVerdict::DivergentNoRecovery`]
//! even when some text was recovered. Whether a flagged page is a false alarm
//! against a real oracle is a cohort verdict the full sweep must supply — this
//! module never claims a corpus result.

use crate::export::LOW_CONFIDENCE_MARKER;

/// A marker for the partial-divergence case (TEXT.15): text was recovered, but
/// more than 5 % of the drawn glyphs were undecodable. Distinct from
/// [`LOW_CONFIDENCE_MARKER`] (the fully-silent case) so a consumer never
/// conflates "nothing read" with "most read, a tail not".
pub const DIVERGENT_NO_RECOVERY_MARKER: &str =
    "[low-confidence: >5% of drawn glyphs were unreadable]";

/// The typed verdict for one page's extraction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExtractionVerdict {
    /// The page drew no text at all — a genuinely blank page, not a failure.
    Blank,
    /// Text was drawn and recovered; no recovery gap worth flagging.
    Recovered,
    /// The page drew text but the recovery chain decoded none of it (TEXT.10).
    /// The formatter emits [`LOW_CONFIDENCE_MARKER`]; a silent empty string is
    /// never returned for a text-bearing page.
    ZeroRecovery,
    /// Text was drawn and mostly recovered, but at least 5 % of the drawn
    /// glyphs were undecodable (TEXT.15). A typed signal, never a blank
    /// "success".
    DivergentNoRecovery,
}

impl ExtractionVerdict {
    /// The marker a plain-text/Markdown formatter must emit, if any.
    #[must_use]
    pub fn marker(self) -> Option<&'static str> {
        match self {
            Self::Blank | Self::Recovered => None,
            Self::ZeroRecovery => Some(LOW_CONFIDENCE_MARKER),
            Self::DivergentNoRecovery => Some(DIVERGENT_NO_RECOVERY_MARKER),
        }
    }

    /// A stable string for the JSON `verdict` field.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Blank => "blank",
            Self::Recovered => "recovered",
            Self::ZeroRecovery => "zero_recovery",
            Self::DivergentNoRecovery => "divergent_no_recovery",
        }
    }

    /// Whether this verdict must surface a marker instead of passing as clean.
    #[must_use]
    pub fn is_flagged(self) -> bool {
        matches!(self, Self::ZeroRecovery | Self::DivergentNoRecovery)
    }
}

/// The local counts the verdict is derived from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RecoveryCounts {
    /// Glyphs the display list drew on the page.
    pub drawn: usize,
    /// Glyphs that produced no character at all: recovery returned `None` and
    /// the raw code is not a valid Unicode scalar (the assembler drops them).
    pub undecodable: usize,
}

impl RecoveryCounts {
    /// Glyphs with a recovered Unicode value (`drawn - undecodable`).
    #[must_use]
    pub fn recovered(&self) -> usize {
        self.drawn.saturating_sub(self.undecodable)
    }
}

/// The integer threshold: at least 5 % of drawn glyphs undecodable.
///
/// Expressed as `undecodable * 100 >= drawn * 5` to avoid floating-point drift
/// at the boundary; both products are `usize` and saturate rather than wrap.
const DIVERGENCE_PERCENT: usize = 5;

/// Classify a page's extraction from its local recovery counts.
///
/// The >5 % test only applies when at least something was recovered: a page
/// where *nothing* was recovered is [`ExtractionVerdict::ZeroRecovery`] (TEXT.10),
/// not the partial-divergence verdict (TEXT.15).
#[must_use]
pub fn classify_verdict(counts: &RecoveryCounts) -> ExtractionVerdict {
    if counts.drawn == 0 {
        return ExtractionVerdict::Blank;
    }
    if counts.recovered() == 0 {
        return ExtractionVerdict::ZeroRecovery;
    }
    let scaled = counts.undecodable.saturating_mul(100);
    let bar = counts.drawn.saturating_mul(DIVERGENCE_PERCENT);
    if scaled >= bar {
        ExtractionVerdict::DivergentNoRecovery
    } else {
        ExtractionVerdict::Recovered
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blank_page_is_not_a_failure() {
        assert_eq!(
            classify_verdict(&RecoveryCounts::default()),
            ExtractionVerdict::Blank
        );
        assert_eq!(ExtractionVerdict::Blank.marker(), None);
        assert!(!ExtractionVerdict::Blank.is_flagged());
    }

    #[test]
    fn fully_silent_page_is_zero_recovery() {
        let v = classify_verdict(&RecoveryCounts {
            drawn: 12,
            undecodable: 12,
        });
        assert_eq!(v, ExtractionVerdict::ZeroRecovery);
        assert_eq!(v.marker(), Some(LOW_CONFIDENCE_MARKER));
        assert_eq!(v.as_str(), "zero_recovery");
    }

    #[test]
    fn five_percent_tail_is_divergent() {
        // 1 of 20 = 5.0 % — exactly on the boundary must flag.
        let v = classify_verdict(&RecoveryCounts {
            drawn: 20,
            undecodable: 1,
        });
        assert_eq!(v, ExtractionVerdict::DivergentNoRecovery);
        assert_eq!(v.marker(), Some(DIVERGENT_NO_RECOVERY_MARKER));
        assert_eq!(v.as_str(), "divergent_no_recovery");
    }

    #[test]
    fn below_five_percent_is_clean() {
        // 1 of 21 ≈ 4.76 % — just under the boundary stays clean.
        let v = classify_verdict(&RecoveryCounts {
            drawn: 21,
            undecodable: 1,
        });
        assert_eq!(v, ExtractionVerdict::Recovered);
        assert_eq!(v.marker(), None);
    }

    #[test]
    fn all_recovered_is_clean() {
        let v = classify_verdict(&RecoveryCounts {
            drawn: 100,
            undecodable: 0,
        });
        assert_eq!(v, ExtractionVerdict::Recovered);
    }

    #[test]
    fn the_two_markers_are_distinct() {
        assert_ne!(LOW_CONFIDENCE_MARKER, DIVERGENT_NO_RECOVERY_MARKER);
    }
}
