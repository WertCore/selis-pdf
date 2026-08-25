# Malformation catalogue (SL-1.COS.01 starts this; SL-1.ROB.05 completes it)

Every real-world deviation the engine encounters, how we handle it, and which
competitor does what. This document is a genuine competitive asset and the
onboarding text for every future parser engineer.

The engine rule (SL-1.COS.01): **permissive in what you accept, explicit about
what you accepted.** Each malformation below is tolerated with a
`selis_pdf_cos::Deviation` recorded against the document, never silently
normalised and never a hard error (except where noted).

## 1. Lexer-level malformations

| # | Malformation | Example | Handling | Deviation |
|---|---|---|---|---|
| 1 | Double sign on a number | `--5` | Tolerate; value = 5 | `DoubleSign` |
| 2 | Trailing decimal point | `6.` | Tolerate; value = 6 | `TrailingDot` |
| 3 | Unterminated literal string | `(abc` EOF | Typed error `LEX_UNTERMINATED_STRING` | `UnterminatedString` |
| 4 | Unbalanced closing paren | `abc)` | Tolerate; stray `)` is a normal closing | `UnexpectedClosingParen` |
| 5 | Odd-length hex string | `<abc>` | Pad last nibble with 0 | `OddLengthHex` |
| 6 | Non-hex char in hex string | `<ab!cd>` | Skip the bad byte | `InvalidHexDigit` |
| 7 | Lone `>` not `>>` | `>` | Skip | `UnexpectedGt` |
| 8 | `#` not followed by 2 hex digits | `/Name#Zz` | Keep literal `#` | `BadNameEscape` |
| 9 | Empty name | `/` followed by whitespace | Accept empty name | `EmptyName` |
| 10 | Reserved delimiters | `{` `}` | Skip | `ReservedDelimiter` |
| 11 | Unknown bare word | `whatever` | Tolerate; parser decides | `UnknownWord` |

## 2. Structure-level malformations (SL-1.COS.03–06)

| # | Malformation | Handling | Deviation |
|---|---|---|---|
| 12 | Wrong xref subsection count | Re-read using actual entries | `BadXrefOffset` |
| 13 | Object offset off by N bytes | Recover the object header | `BadXrefOffset` |
| 14 | Missing `endobj` | Tolerate; object ends at next `N G obj` | `MissingEndobj` |
| 15 | Cyclic `/Prev` chain | Terminate with typed error `XREF_PREV_CYCLE` | — |
| 16 | Stream `/Length` mismatch | Tolerate; scan for `endstream` | `LengthMismatch` |
| 17 | Unusable xref | Rebuild index by scanning `N G obj` | `ReconstructedIndex` |
| 18 | Non-conforming text encoding | Report, don't corrupt | `NonConformingEncoding` |

## 3. Robustness behaviours (SL-1.ROB.03)

The following are hostile *inputs*, not deviations. Each must terminate with a
`BUDGET_*` error in bounded memory and time (regression suite:
`crates/selis-pdf-cos/tests/budget_exhaustion.rs`).

| # | Hostile input | Terminal outcome |
|---|---|---|
| 19 | 10,000-hop `/Prev` chain | `BUDGET_DEPTH` |
| 20 | 20,000-deep array nesting | `BUDGET_DEPTH` (never stack overflow) |
| 21 | Xref bomb (`/Size 2^32`) | `XREF_MALFORMED` at end of file |
| 22 | 1 MB unterminated `(` | `LEX_UNTERMINATED_STRING` |
| 23 | Flate bomb (100 MB → tiny) | `BUDGET_BYTES` at the output bound |
| 24 | 100 MB `#`-escaped name | `BUDGET_BYTES` before the buffer is built |

## 4. Filter-level malformations (SL-1.FILT.01–05)

| # | Malformation | Handling | Code |
|---|---|---|---|
| 25 | Flate stream with leading garbage | miniz skips leading bytes | — |
| 26 | Raw deflate without zlib header | Detected and retried | — |
| 27 | Truncated Flate stream | Yields the decoded prefix, then errors | `FLATE_CORRUPT` |
| 28 | LZW early-code-reuse ("KwKwK") | Synthesise `prev + first(prev)` | — |
| 29 | LZW code out of dictionary | Typed error | `LZW_CORRUPT` |
| 30 | ASCII85 `z` mid-group | Four zero bytes (spec) | — |
| 31 | ASCII85 invalid char | Typed error | `ASCII_CORRUPT` |
| 32 | RunLength no EOD marker | Typed error | `RUNLENGTH_CORRUPT` |
| 33 | JPEG not decodable | Typed error | `DCT_CORRUPT` |

## Competitor comparison (to be filled as tested)

| Malformation | Acrobat | PDFium | qpdf | Selis |
|---|---|---|---|---|
| `--5` number | opens | opens | warns+fixes | `DoubleSign` deviation |
| Truncated Flate | partial | partial | errors | partial + `FLATE_CORRUPT` |
| Cyclic `/Prev` | ? | ? | errors | `XREF_PREV_CYCLE` |
| Damaged xref | rebuilds | rebuilds | rebuilds | `ReconstructedIndex` |

The comparison cells marked `?` are filled when the oracle harness
(SL-0.ORACLE.01) is running against the corpus.
