# CJK payload provenance — SL-3.FONT.10

This directory holds the **record** of the CJK payload, not the payload. The
payload is a release artifact; the record is what makes a release artifact
checkable, and it is the thing a shell, a store reviewer, or a future build
needs in order to trust a set of files it did not produce.

`manifest.json` is `cargo xtask cjk-build`'s output, verbatim. Every number in
it was measured on the machine that wrote it, and `cargo xtask cjk-verify`
re-derives every claim it makes.

## The source

| | |
|---|---|
| Pinned id | `noto-sans-sc` |
| Upstream family | Noto Sans SC (Google Fonts' TrueType build of Adobe's Source Han Sans) |
| Size | 17 772 300 B |
| SHA-256 | `a3041811a78c361b1de50f953c805e0244951c21c5bd412f7232ef0d899af0da` |
| Licence | SIL Open Font License 1.1 (`OFL.txt` in this directory) |
| Reserved Font Name | `Source` |
| Coverage | Simplified Chinese, Latin, kana, CJK punctuation. **No Hangul syllables.** |

The URL is pinned to an immutable commit
(`google/fonts@a85815a42757630ce188fdad368c2dfc444d4773`,
`ofl/notosanssc/NotoSansSC%5Bwght%5D.ttf`) and the bytes to a digest, because a
manifest that promises immutable files cannot be built from a moving ref.
`cargo xtask cjk-fetch noto-sans-sc target/cjk-src` performs the download and
refuses anything that does not hash as pinned; `cjk-build --source-id
noto-sans-sc` re-checks the digest and refuses to write a manifest naming a
source it was not built from.

### Why this source and not "Noto Sans CJK"

ADR-P0043 names "a Noto Sans CJK static TTF". The `notofonts/noto-cjk`
releases are CFF (`.otf`/`.ttc`), and the FONT.11 subsetter subsets `glyf`
only — it refuses a CFF source rather than emitting an empty payload. The
Google Fonts builds of the same family are TrueType-flavoured and cover 30 890
code points, so they are the pinned input. CFF subsetting remains the
FONT.11 follow-up that would let a pan-CJK `.otf` be used directly.

### Two consequences that are stated, not discovered later

1. **The source is a variable font** (`[wght]`, 100–900). The subsetter emits
   static SFNT and drops `fvar`/`gvar`, so every payload file carries the
   default instance's outlines (weight 400) and has no weight axis. Bold CJK
   is synthesised by the raster walk, as it already is for the standard-14
   substitution. A multi-weight payload is a FONT.11 task, not a flag here.
2. **The source has no Korean.** `jamo`, `hangul-1`…`hangul-4`, `jamo-ext-a`,
   `jamo-ext-b` and `ext-g` are `served_by: null` in `chunk_table` and listed
   in `unserved`. A Korean document's 가 (U+AC00) renders `.notdef` — and the
   shell is told *why*: it feeds those ids to
   `CjkFontSet::mark_unavailable`, so no chunk is requested for a range that
   has no file, and the user is told "this payload has no Korean" rather than
   watching a spinner. Japanese kana *are* covered (`punct-kana`).

### Why the subsets are renamed

A subset is a **modified version** of its source, and the OFL reserves the
source's family name. Every file the builder emits therefore carries the name
`Selis CJK` / `Regular` (`SELIS_CJK_NAME`), written as a fresh four-record
`name` table — the source's own copyright, vendor and licence records are
*not* copied, because a subset has no standing to reissue them. The source's
identity lives here and in the manifest's `source` row, which is where a
licence claim belongs.


## Measured payload

Source 17 772 300 B → **16 files, 11 162 268 B raw / 5 204 258 B brotli.**

| file | raw | brotli | codes |
|---|---:|---:|---:|
| `cjk/core.ttf` | 1 290 100 | 641 043 | 5 013 |
| `cjk/ext-a.ttf` | 2 449 132 | 1 093 755 | 6 582 |
| `cjk/ideographs-1.ttf` | 263 840 | 150 235 | 1 024 |
| `cjk/ideographs-2.ttf` | 411 712 | 231 249 | 1 536 |
| `cjk/ideographs-3.ttf` | 551 672 | 296 755 | 1 792 |
| `cjk/ideographs-4.ttf` | 1 366 916 | 645 043 | 4 160 |
| `cjk/ideographs-5.ttf` | 1 563 488 | 694 493 | 4 160 |
| `cjk/ideographs-6.ttf` | 1 429 460 | 671 468 | 4 160 |
| `cjk/ideographs-7.ttf` | 1 716 884 | 705 637 | 4 144 |
| `cjk/compat-ideographs.ttf` | 38 148 | 24 058 | 122 |
| `cjk/ext-b.ttf` | 17 196 | 11 453 | 54 |
| `cjk/ext-c.ttf` | 17 520 | 11 141 | 44 |
| `cjk/ext-d.ttf` | 3 880 | 2 794 | 8 |
| `cjk/ext-e.ttf` | 40 312 | 23 844 | 108 |
| `cjk/ext-f.ttf` | 728 | 429 | 1 |
| `cjk/ext-sup.ttf` | 1 280 | 861 | 2 |

These are `manifest.json`'s own `raw_bytes` / `brotli_bytes` / `codes`, row by
row; the manifest is the authority and this table is a reading of it.

Budgets (ADR-P0043): core ≤ 1 200 000 brotli, chunk ≤ 1 500 000 brotli. The
build **fails** over budget, and the largest file here has 27 % headroom.

This is the "no 100 MB payload" claim, measured: the 100 MB is the whole Noto
CJK family (five languages, nine weights, CFF). One weight of one language,
split by Unicode range, is 5.2 MB on the wire if a reader somehow needed every
range — and `cargo xtask cjk-measure <payload> <text>` prints what one
document actually costs, which is the core plus the one or two chunks its
characters fall in.

## FONT.10-F1 — the full payload does not fit the extension store

**This is a real finding about the payload, not a test that was made to pass.**

The extension's store (`apps/extension/src/ext/cjk-payload.ts`) keeps the
**raw, decompressed** TTF in `chrome.storage.local` and charges it against
`CJK_STORAGE_BUDGET_BYTES` = 8 388 608 B, deliberately under Chrome's
10 485 760 B quota. This payload is 11 162 268 B raw. So:

| | bytes | vs 8 MiB store budget | vs 10 MiB Chrome quota |
|---|---:|---|---|
| whole payload, brotli (wire) | 5 204 258 | fits, 38 % headroom | fits |
| whole payload, raw (resident) | 11 162 268 | **over by 2 773 660** | **over by 676 508** |
| core + largest chunk (`ext-a`) | 3 739 232 | fits | fits |
| core + the seven `ideographs-*` blocks | 8 594 072 | **over by 205 464** | fits |
| core + `ext-a` + the seven `ideographs-*` blocks | 11 043 204 | **over** | **over** |

`installCjkChunk` refuses with `cjk-over-budget` rather than throwing a quota
error, which is the right behaviour — but it means **"install every CJK range"
is not a legal operation**, for a user who wants it, in the browser that
shipped it. Walking the manifest in order (core, then each chunk), the store
accepts through `ideographs-5` (7 896 860 B resident) and refuses
`ideographs-6` at 9 326 320 B.

What this does *not* mean: it does not mean the payload is too big to use.
The design is per-range and on demand, `cjk-measure` shows that an ordinary
Chinese document costs the core plus one or two chunks, and core + any single
chunk is comfortably inside the budget. What it means is that the ceiling is
real and the payload is above it, so the *store* must grow an eviction policy
(or store brotli and inflate on use) before "everything" is a thing a user can
ask for. Until then the honest statement is the one in the gate: `cjk-verify`
prints `FINDING FONT.10-F1` on every run, and
`the_committed_record_respects_the_published_budgets` in `xtask` asserts the
per-file budgets, the wire total and the per-install total, reproduces the
store's refusal point, and **fails loudly if the payload ever does fit** — so
the finding cannot be quietly forgotten when someone re-chunks it.

## Reproducing and checking

```sh
cargo xtask cjk-fetch noto-sans-sc target/cjk-src
cargo xtask cjk-build target/cjk-src/NotoSansSC-wght.ttf \
    --out target/cjk --source-id noto-sans-sc
cargo xtask cjk-verify target/cjk --scope full
cargo xtask cjk-verify assets --scope manifest   # the record in this repo
```

The build is deterministic: the same source bytes produce the same payload
bytes, and `manifest.json`'s SHA-256s are therefore stable across machines and
runs (`cjk-verify --scope full` is what proves a *published* payload is the one
this pipeline produces).

## What a consumer is expected to do with it

* **Web / WASM shell (SL-4.WASM.07)**: fetch `cjk/core.ttf` beside the wasm
  binary, then `cjk/<id>.ttf` on demand, keyed by `<id>+sha256` in the HTTP
  cache. `chunk_table[].served_by` tells you, before any fetch, whether a code
  can be served at all.
* **Extension (SL-4.EXT.05)**: `parseCjkManifest` reads `schema`, `core` and
  `chunks[]` — every field this build adds (`served_by`, `unserved`, `totals`,
  `name`, `source`) is additive, so an EXT.05 consumer keeps working. The
  store's own per-file budget check re-reads `brotli_bytes`, and every row in
  this manifest is inside the budgets it mirrors. Its *aggregate* budget does
  not hold for the whole set: see FONT.10-F1 above.
* **Desktop (native)**: may ship every chunk as local files; it resolves
  through the same `CjkFontSet` API.
