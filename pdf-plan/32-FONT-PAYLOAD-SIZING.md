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
**147 121 B** (its static lookup tables), `skrifa` itself 941 B, and the
Standard-14 AFM width tables in `standard14.rs` ~29 kB.

> **Correction (2026-09-29).** An earlier revision of this note gave the
> provenance of `unicode-normalization` as `selis-font` -> `skrifa`, and listed
> it as a candidate for a feature-gated reduction. That was wrong, and acting
> on it would have found nothing to turn off. Verified: `skrifa` 0.47.0
> depends only on `bytemuck` and `read-fonts` (registry `Cargo.toml:53-60`), and
> `Cargo.lock` shows the sole `unicode-normalization` edge inside the
> `selis-pdf-text` package block. It is a **direct** dependency of
> `selis-pdf-text` (`Cargo.toml:20`), used by `search.rs:39` for `.nfkd()`.
>
> It is reachable from the shipped ABI because search is a wired protocol op
> (`RequestOp::Search`), so the 147 kB is load-bearing, not dead weight. It
> also has no size-reducing feature of its own (`default`/`std` only), so the
> 147 kB stays until search is reimplemented. The Liberation set remains the
> whole story; this was a dead end, not a missed opportunity.

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

---

# As implemented

The API split came first, because the alternative was shipping a smaller
binary with a silent rendering bug in it.

## The bug this had to avoid

Four `session.rs` call sites used `fallback_bytes`. Two of them —
`is_standard14_fallback` and the resource resolver at ~L1972 — wanted a
**policy** answer about a *name* and never touched the bytes, but asked
`fallback_bytes(name).is_some()` to get it.

That made "is this name standard-14?" depend on "is this face compiled into
this build?". Reduce the embedded set and `is_standard14_fallback` starts
answering `false` for an ordinary `/Helvetica`; the engine takes the
no-standard-14 path and drops the text instead of substituting it. The bug
would have appeared only on the builds that had been slimmed, which is the
worst place to find it, and it would have looked like a font problem.

So the split is not tidiness. It is the precondition for shrinking anything:

| function | question | depends on the build? |
|---|---|---|
| `fallback::is_standard14(name)` | is this a standard-14 name? | no — pure policy |
| `fallback::fallback_bytes(name)` | are those bytes in this binary? | yes — delivery |

`fallback::substitute` is the shared mapping, so the two cannot drift.
`is_standard14_does_not_depend_on_which_faces_are_embedded` is the regression
test, and it runs in both feature configurations.

## What is built in, and what is fetched

**Always built in** — subsetted Serif Regular and Bold, 211 code points
(ASCII, Latin-1 letters, typographic punctuation). Serif is metric-compatible
with Times New Roman, the base-14 font most often referenced *without*
embedding. Regular **and** Bold, because one face would draw bold text in
regular advances and reflow the line. Italic is left lazy: it is the less
common of the two, and 70 kB brotli is a real cost for it.

| face | raw | in binary |
|---|---|---|
| LiberationSerif-Regular (subset) | 60 088 B | yes |
| LiberationSerif-Bold (subset) | 54 352 B | yes |
| other 10 faces | 70–83 kB brotli each | no — lazy payload |

**Desktop** (`builtin-fallback-fonts`) keeps all twelve faces, unsubsetted and
upstream-exact, fully offline. It does *not* use the subsets: the subsets drop
everything outside Latin-1 plus punctuation, so desktop embedding them would
render a blank for `ā` where it previously rendered correctly. Desktop has no
size problem worth that regression.

**Web** keeps only the two subsets. The other ten arrive through
`FallbackFontSet` (`selis-font`), which holds verified bytes and is
dependency-free; the `selis-fallback/1` manifest that describes them
(`selis-pdf-wasm`) is generated by the same type the shell parses, so the
generator cannot emit a shape its own reader rejects.

The manifest exists because **a font from the network is a parser input**. A
TTF is a structured binary, so a face is only handed to the engine after its
SHA-256 matches — the same discipline as the CJK payload. That is a transport
check, not a supply-chain review: a compromised upstream release would be
faithfully reproduced by a matching digest, and the manifest says so.

## The regression this caught, and why it is the most valuable result here

`apps/cli/tests/text_matrix_layout.rs` renders `fixtures/text11_tm_scaled.pdf`
— a real non-embedded-`Helvetica` document — and asserts the ink extent lands
where **MuPDF** puts it, to the pixel. After the embedded set shrank, that test
failed with "the fixture paints ink": not a shifted extent, *no ink at all*.

The cause was not the substitution policy. The CLI is a desktop binary, and
`builtin-fallback-fonts` was not switched on for it, so it was silently running
the *web* configuration — no fallback faces at all. A desktop build that cannot
render a Helvetica document offline is a shipping defect, and nothing in the
Rust unit tests noticed, because they all used fonts that happened to be
resident.

Two fixes, both load-bearing:

- `apps/cli` now requests `selis-pdf-engine/builtin-fallback-fonts`, and the
  engine forwards it to `selis-font` so a target can choose its delivery without
  depending on `selis-font` directly.
- Three test helpers had hardcoded `"Helvetica"`; they now use `"Times-Roman"`,
  the one family resident in **every** build, so they assert the same thing in
  both configurations instead of only on desktop.

The restored test passing is the strongest evidence in this work that desktop
rendering is unchanged: the expected geometry is another engine's output, not
ours, so it cannot be satisfied by us being consistently wrong.

## Reproducibility

`cargo xtask fallback-assets` regenerates the subsets, the brotli payloads and
the manifest from the twelve committed source faces, using the existing
`selis_font::subset::subset_ttf` — the same subsetter the CJK assets use, so
the two paths cannot diverge and the build stays pure-Rust (no fontTools in CI).
`--check` verifies the committed assets instead of rewriting them, so a
Liberation update that skipped regeneration fails the gate rather than leaving a
manifest that describes bytes nobody ships.

## Measured

Subsets regenerate deterministically: 209 glyphs each, and `--check` agrees
with what is committed.

**Core WASM is not re-measured yet.** The projected figures (~440 kB with Serif
Regular+Bold built in, ~367 kB with all fonts lazy) remain estimates from the
measured contribution of the embedded faces, not a `size-check` run. The
baseline is still the pre-change 1 275 596 B and must be re-recorded only from a
real measurement.
