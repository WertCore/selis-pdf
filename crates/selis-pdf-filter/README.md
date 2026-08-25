# selis-pdf-filter

Stream filters and codecs of the [Selis](https://github.com/WertCore/selis-pdf)
PDF engine: decode streams that PDFs compress and encode.

This crate is published open-source under Apache-2.0 (ADR-P0030) as part of
the Selis parser; the rest of the engine remains proprietary.

## What it does

* **FlateDecode** — via `miniz_oxide`; handles zlib-wrapped and raw-deflate
  streams, truncated streams (yields the decoded prefix), and output bounds
  for the Flate-bomb case.
* **LZWDecode** — with PDF's 9-bit start, CLEAR/EOD codes, code-size growth
  to 12 bits, `/EarlyChange` handling, and the early-code-reuse malformation.
* **ASCIIHexDecode / ASCII85Decode / RunLengthDecode** — the ASCII-family
  filters, each with its real-world quirks (`z` = four zero bytes, odd-nibble
  padding, run-length EOD).
* **DCTDecode** — JPEG via `zune-jpeg`, with inverted-CMYK detection for
  4-component Adobe APP14 streams.
* A `decode(filter, data, output_limit)` dispatcher for single-filter streams.

## Design rules

* **Budget-bounded**: decoded output never exceeds the limit; the caller's
  budget bounds allocation.
* **Hostile-input lint set**: no `unwrap`, no `panic`, no unchecked
  arithmetic.
* **Real-world tolerance**: truncated streams, missing zlib headers, and
  legacy encodings are handled, not rejected — with typed errors where a
  stream is genuinely corrupt.

## Dependencies

Only L0/L1 crates (`selis-error`, `selis-bytes`, `selis-sandbox`) plus
permissive codecs (`miniz_oxide`, `zune-jpeg`). No proprietary dependency.

## API stability

The public API of this crate is **not yet stable**. Between 0.1.x releases
we reserve the right to change names, signatures, and semantics as Phase 1
matures; we will call out breaking changes in the changelog. Once the crate
reaches 1.0 the API will be frozen under semver.
