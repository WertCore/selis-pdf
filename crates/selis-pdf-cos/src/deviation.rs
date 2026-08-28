//! Deviation reporting (SL-1.COS.01 seeds the catalogue; SL-1.COS.11 completes it).
//!
//! The lexer is *permissive in what it accepts, explicit about what it
//! accepted* (SL-1.COS.01 Risk). Every malformation it tolerates is recorded
//! here with its byte offset, so the UI can say "this file is malformed in
//! these ways", `selis inspect --json` can dump them, and the corpus can assert
//! on specific deviations on specific files.

/// A deviation from strict COS syntax that the engine tolerated.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Deviation {
    /// A number with more than one sign, e.g. `--5`.
    DoubleSign {
        /// Byte offset of the offending number.
        offset: u64,
    },
    /// A number with a trailing decimal point, e.g. `6.`.
    TrailingDot {
        /// Byte offset of the offending number.
        offset: u64,
    },
    /// A literal string that ran to end of input without its closing paren.
    UnterminatedString {
        /// Byte offset of the opening paren.
        offset: u64,
    },
    /// An unbalanced closing paren with no matching opener.
    UnexpectedClosingParen {
        /// Byte offset of the stray `)`.
        offset: u64,
    },
    /// A hex string with an odd number of digits; the last nibble is 0-padded.
    OddLengthHex {
        /// Byte offset of the opening `<`.
        offset: u64,
    },
    /// A hex string contained a non-hex character that was skipped.
    InvalidHexDigit {
        /// Byte offset of the bad byte.
        offset: u64,
    },
    /// A lone `>` that is not the second half of `>>`.
    UnexpectedGt {
        /// Byte offset of the stray `>`.
        offset: u64,
    },
    /// A `#` in a name that was not followed by two hex digits.
    BadNameEscape {
        /// Byte offset of the `#`.
        offset: u64,
    },
    /// An empty name (`/` immediately followed by a delimiter).
    EmptyName {
        /// Byte offset of the `/`.
        offset: u64,
    },
    /// An unknown bare word (not a keyword, not a number).
    UnknownWord {
        /// Byte offset of the word.
        offset: u64,
    },
    /// A number too large for `i64`; the lexer clamped it to `i64::MAX`
    /// instead of failing the parse (SL-1.ROB.01).
    NumberOverflow {
        /// Byte offset of the offending number.
        offset: u64,
    },
    /// A reserved delimiter (`{` or `}`) that was skipped.
    ReservedDelimiter {
        /// Byte offset of the delimiter.
        offset: u64,
    },
    /// The xref was unusable and the index was rebuilt by scanning for
    /// `N G obj` headers (SL-1.COS.06).
    ReconstructedIndex {
        /// Byte offset where reconstruction started.
        offset: u64,
    },
}

impl Deviation {
    /// A short stable identifier for `inspect --json` and log output.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Deviation::DoubleSign { .. } => "double-sign",
            Deviation::TrailingDot { .. } => "trailing-dot",
            Deviation::UnterminatedString { .. } => "unterminated-string",
            Deviation::UnexpectedClosingParen { .. } => "unexpected-closing-paren",
            Deviation::OddLengthHex { .. } => "odd-length-hex",
            Deviation::InvalidHexDigit { .. } => "invalid-hex-digit",
            Deviation::UnexpectedGt { .. } => "unexpected-gt",
            Deviation::BadNameEscape { .. } => "bad-name-escape",
            Deviation::EmptyName { .. } => "empty-name",
            Deviation::UnknownWord { .. } => "unknown-word",
            Deviation::NumberOverflow { .. } => "number-overflow",
            Deviation::ReservedDelimiter { .. } => "reserved-delimiter",
            Deviation::ReconstructedIndex { .. } => "reconstructed-index",
        }
    }

    /// The byte offset the deviation was recorded at.
    #[must_use]
    pub const fn offset(self) -> u64 {
        match self {
            Deviation::DoubleSign { offset }
            | Deviation::TrailingDot { offset }
            | Deviation::UnterminatedString { offset }
            | Deviation::UnexpectedClosingParen { offset }
            | Deviation::OddLengthHex { offset }
            | Deviation::InvalidHexDigit { offset }
            | Deviation::UnexpectedGt { offset }
            | Deviation::BadNameEscape { offset }
            | Deviation::EmptyName { offset }
            | Deviation::UnknownWord { offset }
            | Deviation::NumberOverflow { offset }
            | Deviation::ReservedDelimiter { offset }
            | Deviation::ReconstructedIndex { offset } => offset,
        }
    }
}
