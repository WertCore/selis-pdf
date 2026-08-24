# Malformation catalogue (SL-1.COS.01 starts this; SL-1.ROB.05 finishes it)

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

## 2. Structure-level malformations (Phase 1, SL-1.COS.03–06)

| # | Malformation | Handling | Deviation |
|---|---|---|---|
| 12 | Wrong xref subsection count | Re-read using actual entries | `BadXrefOffset` |
| 13 | Object offset off by N bytes | Recover the object header | `BadXrefOffset` |
| 14 | Missing `endobj` | Tolerate; object ends at next `N G obj` | `MissingEndobj` |
| 15 | Cyclic `/Prev` chain | Terminate with typed error | — |
| 16 | Stream `/Length` mismatch | Tolerate; scan for `endstream` | `LengthMismatch` |
| 17 | Unusable xref | Rebuild index by scanning `N G obj` | `ReconstructedIndex` |
| 18 | Non-conforming text encoding | Report, don't corrupt | `NonConformingEncoding` |
