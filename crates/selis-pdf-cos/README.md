# selis-pdf-cos

The COS object layer of the [Selis](https://github.com/WertCore/selis-pdf) PDF
engine: a hostile-input-hardened parser for the PDF "COS" object syntax.

This crate is published open-source under Apache-2.0 (ADR-P0030) as part of
the Selis parser; the rest of the engine remains proprietary.

## What it does

* Lexes the COS syntax (`SL-1.COS.01`) — numbers (including the `--5` and
  `6.` malformations), names with `#xx` escapes, literal/hex strings, arrays,
  dicts, streams — recording every tolerated malformation as a `Deviation`.
* Parses objects into an `Obj` value type (`SL-1.COS.02`).
* Reads classic xref tables and xref streams, with `/Prev` chains exposed as
  first-class revisions (`SL-1.COS.03/04/05`).
* Reconstructs the object index of damaged files (`SL-1.COS.06`).
* Reads over partial sources (`SL-1.COS.07`): every step either yields a
  token, reports the exact byte ranges it needs, or errors at true EOF.
* Writes objects back to COS with correct escaping and locale-free numbers
  (`SL-1.COS.09`).
* Surfaces every tolerated deviation with a name and byte offset
  (`SL-1.COS.11`).

## Design rules

* **Hostile-input lint set**: no `unwrap`, no `panic`, no bare indexing, no
  unchecked arithmetic — enforced by the workspace lints.
* **Budget-charged**: every token, object, and byte-buffer push is charged
  against a `Budget`; a hostile file exhausts a budget, never memory
  (see `tests/budget_exhaustion.rs`).
* **Permissive in what you accept, explicit about what you accepted**: every
  malformation is a `Deviation`, never a silent normalisation.
* **Deterministic**: no locale, no exponent notation, no hash-order output.

## Dependencies

Only L0/L1 crates (`selis-error`, `selis-bytes`, `selis-sandbox`,
`selis-io`, `selis-crypto`, `selis-pdf-filter`) plus permissive third-party
codecs. No proprietary dependency.

## API stability

The public API of this crate is **not yet stable**. Between 0.1.x releases
we reserve the right to change names, signatures, and semantics as Phase 1
matures; we will call out breaking changes in the changelog. Once the crate
reaches 1.0 the API will be frozen under semver.
