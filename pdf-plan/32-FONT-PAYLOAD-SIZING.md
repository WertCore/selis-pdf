# Font payload sizing — what is actually inside the shipped module

Recorded 2026-09-29 from a DWARF-level attribution of the core
`selis_pdf_wasm` module. This is a measurement note, not a task.

## The finding

The module's `$.rodata` is **1 914 752 B**, and **84.9% of it — 1 625 684 B
across 12 files — is the Liberation fallback fonts embedded verbatim by
`include_bytes!` in `crates/selis-font/src/fallback.rs`**.

Verified by locating each source file's bytes inside the compiled module. They
form ONE contiguous block with zero gaps, which is what twelve `include_bytes!`
calls in the arms of a single `match` produce and nothing else does:

```
@  240531  151452  LiberationSerif-BoldItalic.ttf
@  391983  145028  LiberationSerif-Italic.ttf
@  537011  147132  LiberationSerif-Bold.ttf
@  684143  152408  LiberationSerif-Regular.ttf
@  836551  135124  LiberationSans-BoldItalic.ttf
@  971675  162036  LiberationSans-Italic.ttf
@ 1133711  137052  LiberationSans-Bold.ttf
@ 1270763  139512  LiberationSans-Regular.ttf
@ 1410275  118296  LiberationMono-BoldItalic.ttf
@ 1528571  124012  LiberationMono-Italic.ttf
@ 1652583  105460  LiberationMono-Bold.ttf
@ 1758043  108172  LiberationMono-Regular.ttf
```

Brotli, the fonts alone: **908 348 B** of a 1 275 596 B module.

Secondary contributors, measured the same way: `unicode-normalization`
**147 121 B** (its lookup tables — it is in the graph via `selis-font` ->
`skrifa`). `skrifa` itself carries 941 B, and the Standard-14 AFM width tables
in `standard14.rs` are ~29 kB. Neither is worth acting on; the Liberation set
is the whole story.

## Why this is not a "just subset them" change

Two call sites use `fallback_bytes` only to ask a **question about a font
name**, never for the bytes:

- `selis-pdf-engine/src/session.rs:3699` — `fallback_bytes(s).is_some()`
  inside `is_standard14_fallback`
- `selis-pdf-engine/src/session.rs:1972` — `fallback_bytes(name)?;`

So the byte-carrying signature is load-bearing for a predicate. Until the API
is split into a policy function and a data function, shrinking the embedded set
silently changes the answer to "is this a standard-14 name?" — and a font that
answers `false` stops being substituted at all.

The other two call sites (`session.rs:1725`, `:1758`) genuinely rasterize with
the bytes, so a subset is a real fidelity decision, not just a size one.

## Measured options

Subset to Latin-1 + typographic punctuation (211 code points), brotli:

| built-in set | brotli | module total |
|---|---|---|
| none (all lazy) | 0 | ~367 248 |
| Serif Regular | 39 282 | ~406 530 |
| **Serif Regular + Bold** | **73 196** | **~440 444** |
| Serif x4 (R/B/I/BI) | 143 236 | ~510 484 |
| all 12, subsetted | 477 195 | ~844 443 |
| all 12, full (**today**) | 908 348 | 1 275 596 |

## Recommended shape

- **Split the API.** Add `fallback::is_standard14(name) -> bool` as pure policy
  and point `is_standard14_fallback` at it. `fallback_bytes` keeps the byte
  signature but serves only built-in faces. Delivery changes; **selection
  policy does not** — one policy, one code path, feature-gated delivery, so
  desktop and web cannot disagree about which face substitutes for Times.
- **Built in everywhere: Serif Regular + Serif Bold, subsetted** (73 196 B).
  Serif metric-matches Times New Roman, the base-14 font most often referenced
  without embedding. Two faces rather than one so bold keeps bold metrics;
  four would add italic for 70 kB more, which is the wrong trade while the
  target is first-load size.
- **Everything else lazy**, through the chunk mechanism WASM.07 already built
  for CJK: a `selis-fallback/1` manifest pairing each face with its SHA-256, and
  a runtime set mirroring `CjkFontSet`.
- **Subset with `selis_font::subset::subset_ttf`**, not Python/fontTools — it
  is the same Rust subsetter `cjk-build` already uses (FONT.11), so the payload
  is reproducible with no extra toolchain dependency.
- **Desktop** enables a `builtin-fallback-fonts` feature and keeps all 12
  faces plus CJK in the binary. The subset stays built in on both: it is small
  enough that shipping it everywhere removes a whole class of blank-page
  failure on documents that embed no fonts.

## Open decision, deliberately not made here

When a document needs a lazy face on a cold cache: block on the fetch, or paint
with the built-in subset and swap when the real face arrives. Swapping
reflows, because advance widths differ from the subset. The working answer
agreed for the web build is **block, then render** — a face is ~50 kB brotli,
so the wait is not perceptible, and reflow after paint is worse than a short
wait.
