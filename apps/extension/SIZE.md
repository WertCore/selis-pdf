# Extension size budget (SL-4.EXT.05)

The rule, in one sentence: **the package carries the engine, the engine is held
to a smaller budget than the web app's, and nothing that is not needed to open
a PDF is in it.**

`pnpm --filter @selis/extension build` compiles, assembles the package, runs the
bundled-only gate (`BUNDLING.md`), and then runs this gate. Both are on the
build path, so a package that is too heavy, or that has lost its engine, cannot
be produced by a green build.

## What is measured

The bytes in `dist/` after `tools/pack.mjs` has run: raw, uncompressed, every
declared row, nothing else. `dist/pkg/` (the raw `tsc` output) is excluded by
name, so test files and source maps are neither counted nor countable.

**Raw, not brotli**, because that is what a store counts. A store takes an
upload and unpacks it on the user's machine; the install is paid in the bytes
that land on disk, whatever the transport did to them. Brotli is measured too,
and one rule uses it, but a packaging budget expressed in compressed bytes would
be a budget about a transport this package does not control.

`size-check.ts` prints the brotli size of every file, because the report is more
useful with both numbers in it. Those figures come from
`zlib.brotliCompressSync` with no options — the same call
`xtask/src/size_check.rs` makes — so the number here and the WASM.02 baseline
describe the same artefact the same way.

## The four numbers

| Budget | Value | Enforced on | Why that value |
|---|---|---|---|
| package | 8,000,000 | the whole package, raw | Below WEB.02's 12 MB per-entry runtime-cache cap, which WEB.02 derived from §12 *and this task*. A cache entry is evictable; a package install is not. |
| file | 6,000,000 | any single file, raw | The per-file half of the same rule, so one file cannot eat the total and hide inside a passing sum. |
| shell | 1,000,000 | every file except the engine, raw | Without it, a viewer quietly growing its own JavaScript is a regression the engine's number would absorb. |
| engine (core chunk) | 2,000,000 | `wasm/selis_pdf_wasm.wasm`, **brotli** | Tighter than the web app's 3,000,000 (`03-CONVENTIONS.md` §12, `xtask/size-budgets.toml`). The same engine ships in both shells, so holding it to a smaller number here means growth has to be decided twice rather than inherited once. |

## What else the gate refuses

Three rules are not size rules and live here because they are the same decision
wearing a different hat:

- **`missing-core-wasm`.** The package must contain `wasm/selis_pdf_wasm.wasm`.
  This is the inversion that matters: a package *without* an engine is a
  failure, not an absence of one. EXT.03 shipped a viewer whose engine could
  not load, reported a typed failure naming the path, and had every gate green.
  The build can no longer produce that state.
- **`bundled-font`.** Any `.ttf` / `.otf` / `.ttc` / `.otc` / `.woff` /
  `.woff2` / `.eot` in the package. The CJK payload is an optional post-install
  download into extension storage, and this is what makes that falsifiable
  rather than a sentence in a design document.
- **`forbidden-chunk`.** `jpx`, `cjk`, `ocr`, `convert` and `editor` must not
  ship. The last three are `viewerAuto: false` in the WASM.02 manifest; the
  first two are auto-loadable in the web app and unloadable here, because the
  extension holds no host permission and so has no origin to fetch them from. A
  future task that decides to ship one has to delete a line in
  `size-budget.ts` on purpose.

## The CJK payload, and why it is not a number above

`src/ext/cjk-payload.ts` is the store: a `selis-cjk/1` manifest is validated,
each chunk is checked against its declared brotli budget, the resident total is
checked against an 8 MiB budget, and only then are the bytes length-checked,
SHA-256-checked and written to `chrome.storage.local`. Budget before bytes,
bytes before integrity, write last.

It has **no producer**, and that is reported rather than faked. The producer is
`cargo xtask cjk-build` (SL-3.FONT.10), which is still open;
`CJK_PAYLOAD_SOURCE` is a typed `cjk-no-producer` refusal and a test asserts
callers get it. The transport that would fill the store is WASM.07's row under
ADR-P0043 §3 ("the shell owns transport"), and it cannot be a URL fetch today
because `host_permissions` is `[]`.

`storage` was added to the manifest for this, and only for this. It is the plain
permission, not `unlimitedStorage`: the store is budgeted at 8 MiB against
`storage.local`'s 10 MB default quota, so the wider permission has not been
earned. `PERMISSIONS.md` carried the forward reference from EXT.01.

## Where the artefact comes from

`pack.mjs` reads the engine from
`$CARGO_TARGET_DIR/wasm32-unknown-unknown/release/selis_pdf_wasm.opt.wasm`
(defaulting to `<repo>/target`), which is the `wasm-opt -O3` output of the same
build `cargo xtask size-check` produces. If it is not there the build fails and
prints the two commands that make it, so on a fresh clone:

```
cargo build -p selis-pdf-wasm --target wasm32-unknown-unknown --release
wasm-opt -O3 <target>/wasm32-unknown-unknown/release/selis_pdf_wasm.wasm \
          -o <target>/wasm32-unknown-unknown/release/selis_pdf_wasm.opt.wasm
pnpm --filter @selis/extension build
```

There is no JavaScript CI job in this repository today, so the build is not yet
wired to a workflow that has the artefact. When one lands it must run
`cargo xtask size-check` (which builds and optimises every chunk) before the
extension build; a job that gets that order wrong fails here rather than
shipping a package with no engine.

## Verifying it, both ways

Every rule has a planted violation in `src/size-budget.test.ts` that crosses
it, and the same file runs the rules over the **real** built package, then over
that package with a font added and with the engine removed. It also asserts that
the multi-megabyte binary did not neuter the bundled-only gate: a `dynamic-code`
plant in a module beside the `.wasm` still fails `scanBundle`.

To watch the gate itself fail, from `apps/extension`:

```
node dist/pkg/extension/src/size-check.js
# then plant a chunk the viewer may not load, and a font:
cp <target>/wasm32-unknown-unknown/release/selis_pdf_wasm.opt.wasm dist/wasm/selis_pdf_wasm_editor.wasm
node dist/pkg/extension/src/size-check.js     # forbidden-chunk, exit 1
mkdir dist/cjk; cmd /c "echo x > dist\cjk\core.ttf"
node dist/pkg/extension/src/size-check.js     # bundled-font, exit 1
```

Deleting the `wasm/selis_pdf_wasm.wasm` row from `PACKAGE_ENTRIES` and
rebuilding fails first in `pack.mjs` (which will not produce the file), and the
size gate reports `missing-core-wasm` if some other route produced it.

## Gates

```
pnpm --filter @selis/extension build          # tsc + pack + bundle gate + this gate
pnpm --filter @selis/extension check:size     # this gate alone, on the last build
pnpm --filter @selis/extension check:bundle   # the build, named
pnpm --filter @selis/extension test           # builds, then the full suite
```


Measured on the build this was written against: package 4,328,181 raw (54% of
budget), engine 4,135,676 raw / 1,326,454 brotli (66% of the engine budget),
shell 192,505 raw (19%).

Re-measured after SL-4.EXT.07 (the options page) and SL-3.FONT.10 (the bundled
CJK core): **package 4,520,630 raw (56.5%), engine 4,279,475 raw / 1,357,411
brotli (67.9% of the engine budget), shell 241,155 raw (24.1%)** — 31 files.

The two movements are different and are worth keeping apart:

- **The engine** grew because SL-3.FONT.10 put the CJK core in the WASM. It is
  still well inside this package's tighter 2 MB core budget, but it is now
  3.36% over the *regression baseline* recorded in `xtask/size-budgets.toml`,
  which is why `cargo xtask size-check` fails on a clean tree and why the
  extension build has to be run after it (see "Where the artefact comes from").
  Re-baselining is FONT.10's or WASM's call, not this task's.
- **The shell** grew by ~49 KB, all of it the options page: two root files
  (`options.html`, `options.css`) and three compiled modules
  (`options-boot.js`, `options-state.js`, `options-strings.js`). The gate did
  its job — a page that had quietly pulled in the viewer catalogue would have
  shown up here, and the test that keeps it out is in `REUSE.md`.

The CJK payload appears in **none** of these numbers, because it is not in the
package. See below.
