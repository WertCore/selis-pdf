# Bundled-only build (SL-4.EXT.04, ADR-P0028)

The rule, in one sentence: **every byte the browser executes ships inside the
package, and nothing in the package points at a remote origin.**

Chrome Web Store policy forbids remotely-hosted code in MV3 outright, and
punishes anything that resembles it. So this is enforced mechanically rather
than held as a review habit: `pnpm --filter @selis/extension build` compiles,
assembles the package, and then scans it. A finding fails the build.

## What ships, and how that is decided

`src/bundle-paths.ts` holds `PACKAGE_ENTRIES`: the complete list of files in the
shipped package, each mapped to where it comes from.

- `tools/pack.mjs` copies **exactly** those entries out of the build into
  `dist/`, and deletes everything else that was there.
- The gate scans `dist/` and fails on any file that is not on the list
  (`unpackaged-file`) or any listed file that is missing.
- `dist/pkg/` is the raw `tsc` output. It is excluded **by name** so that test
  files, source maps and the gate's own modules can never reach a store upload
  by being next to the code.

Adding a module to the package therefore means adding a row to
`PACKAGE_ENTRIES`, which is the moment somebody has to ask "is this remote
code?".

### The second page (SL-4.EXT.07)

`options.html` and `options.css` are the only other root rows, and they are the
first rows a *manifest field* points at besides the worker and the popup.
`options_page` is in `PACKAGE_PATH_FIELDS`, so `findManifestViolations` resolves
it against the ship list exactly as it resolves `default_popup`: a manifest
naming a page this list forgot fails the gate instead of 404ing in the browser
with every other gate green.

`options.css` is a `root` row rather than a `shared-asset` because it is this
package's own layout, not `@selis/ui-kit`'s. It may not hard-code a value the
tokens define, and `options-page.test.ts` enforces both halves of that: no
colour literal, and every `--selis-*` name it uses is one `tokens.css` defines.

Three compiled rows came with the page (`options-boot.js`, `options-state.js`,
`options-strings.js`). `options-state.js` is also imported by the service
worker, so it is the one row two contexts load; it touches `chrome` only inside
a factory, never at module scope, which is what lets a worker with no DOM
import it.

### The local-file flow's four rows (SL-4.EXT.09)

A second page stylesheet and three compiled modules, added the same way:

| Row | Why it ships |
|---|---|
| `viewer.css` | The viewer's own layout, for the same reason and under the same rules as `options.css`. It styles one panel, and `viewer-page.test.ts` enforces the same two halves — no colour literal, every token real. |
| `extension/src/local-files.js` | The `file://` flow's decisions, with no `chrome.*` and no DOM in it. Pure is what makes the whole state space of a permission flow testable without a browser. |
| `extension/src/viewer-strings.js` | The viewer page's catalogue, registered against the shared runtime below. |
| `ui/src/i18n/message.js`, `ui/src/i18n/runtime.js` | SL-4.UI.11's runtime, shipped **instead of** a third hand-rolled formatter. `apps/ui/src/i18n/i18n-boundary.test.ts` is the gate that keeps these two rows importable without dragging the viewer in, and it names this package as the consumer they were split for. They cost ~20 KB of the 1 MB shell budget and displace none of it. |

Four new rows is four more chances to ship something remote, so the four plants
below are re-run against them: a `url(https://…)` appended to `dist/viewer.css`
fails as `remote-url-literal` (a sheet is neither a module, a page nor the
manifest), a remote literal appended to `dist/ui/src/i18n/runtime.js` fails as
`remote-url-literal` with the URL quoted, a `<link href="https://…">` appended
to `dist/viewer.html` fails as `remote-resource-ref`, and deleting the
`local-files.js` row from `PACKAGE_ENTRIES` makes the two modules that import it
fail as `unresolved-import`. All four exit 1.

The `local-files.js` case is the one worth having: it is a *structural* check
firing on a real importer, which is the difference between "the gate read the
new file" and "the gate read the file the new code depends on".

### The one file nothing else implies (SL-4.EXT.03)

`extension/src/ext/wasm-worker.js` is loaded by
`new Worker(chrome.runtime.getURL(...))`, not by an import, so no other row
implies it and no import scan would notice it missing. A Worker script the
ship list forgot is a 404 in the browser with every gate green, which is why
`engine-host.test.ts` asserts the path the code uses is a row here.

The core `.wasm` **is** on the list, as of SL-4.EXT.05. It is the one row whose
bytes no other build step produces, so `pack.mjs` reads it from
`$CARGO_TARGET_DIR/wasm32-unknown-unknown/release/selis_pdf_wasm.opt.wasm` —
the `wasm-opt -O3` output of the same `wasm32-unknown-unknown` release build
`cargo xtask size-check` measures — and a missing artefact is a **build
failure**, with the two commands that produce it, rather than a skipped row.
Before EXT.05 the engine reported a typed failure naming the path it looked
for; that failure is now unreachable from a successful build.

The five lazy chunks are not rows, and `size-budget.ts` fails the build if one
of them turns up in the package. See `SIZE.md`.

No font is fetched, and none is bundled: the CJK payload is an optional
post-install download into extension storage (`src/ext/cjk-payload.ts`), and
the size gate fails on any font extension in the package.

## What the gate rejects

| Class | What it is |
|---|---|
| `remote-resource-ref` | A remote or protocol-relative URL in any attribute the browser fetches (`src`, `srcset`, `href` on `link`/`use`/`image`, `poster`, `action`, `formaction`, `data`), or in a manifest field that names a packaged file (`service_worker`, `default_popup`, `content_scripts[].js`, `icons`, …) |
| `unresolved-local-ref` | A relative reference that resolves to a file not in the package |
| `inline-script` | A `<script>` with no `src`: MV3 extension pages may not carry inline script |
| `remote-csp-source` / `loose-csp-source` | A remote host in the `extension_pages` CSP, or a `script-src` source other than `'self'` / `'wasm-unsafe-eval'` / `'none'` |
| `remote-manifest-url` | A remote URL in any other manifest string |
| `remote-url-literal` | A remote URL in shipped code, or in a packaged file that is no module, page or manifest |
| `remote-import`, `unresolved-import`, `node-builtin-import`, `unresolvable-dynamic-import` | A module specifier that is remote, resolves to nothing, names a Node builtin, or is computed |
| `dynamic-code` | `eval`, `new Function`, `Function("…")`, `importScripts`, a string-bodied `setTimeout`/`setInterval`, `document.write` |
| `remote-redirect-target` | A DNR redirect whose `extensionPath` is not a packaged file |
| `unpackaged-file` | A file in the package that is not on `PACKAGE_ENTRIES` |


## What it deliberately does not flag

A gate that flags every `https://` in the tree gets deleted the first day it
blocks a real commit, so each exemption below is deliberate and each has a test
in `src/bundle.test.ts`:

- **Comments.** A JSDoc example, an HTML comment, a "do not do this" note.
- **DNR `regexFilter`.** `"^https?://…\.pdf…$"` is a *match pattern* for
  navigations the browser makes, never something the extension fetches. It is
  the most remote-looking string the package legitimately ships.
- **The reviewed data-only exports** (`DATA_ONLY_EXPORTS`: `PATTERN_MATRIX`,
  `DENIED_HOST_PATTERNS`). Remote URLs the ruleset must *recognise*. The
  exemption is **structural, not a name match**: it holds only while the
  initializer contains no `(` and no `)`, i.e. while it is a plain array or
  object of text. The moment a call expression appears in there, the name stops
  being exempt and every URL inside it is reported like any other.
- **`upgrade-insecure-requests`, `block-all-mixed-content`, `report-to`.** CSP
  directive keywords, not hosts. Same false positive, same fix as
  `apps/web/host`.
- **`<a href>`.** A link the user has to click is not remote code.
- **Store-listing links and permission patterns.** `homepage_url`,
  `support_url`, `search_provider`, `host_permissions` — Chrome never loads
  these as executable content. (Permission *breadth* is `manifest.test.ts`'s
  job, not this gate's.)
- **`blob:` / `data:` / `chrome-extension:`.** Local by construction. A
  `blob:` URL embeds the origin that created it, but the bytes it names were
  made in this process.
- **Binary assets.** A file with a NUL byte in its first 8 KB — the core
  `.wasm`, which SL-4.EXT.05 added to the package — is data in the package by
  construction, and is not utf8-decoded and scanned. The exemption is
  width-blind, so it is asserted rather than assumed: `size-budget.test.ts`
  plants a violation in the real package and in a text file beside it, and
  fails if the presence of the binary stops the gate reporting anything.


## How far a lexical scan goes, and where it stops

There is no JS parser in this repository and none may be added, so the module
scan is lexical. `maskJs` makes two length-preserving views of each module in a
single pass — `text` (comments blanked) and `code` (comments blanked, string and
regex *bodies* blanked, quote characters kept) — and each stage reads whichever
one it needs. Offsets are valid in both, so a match found in one can be quoted
from the other.

Stating the limits plainly, because a gate that overstates itself is worse than
no gate:

- **Template literals are opaque.** A `${…}` interpolation is blanked with the
  rest of the literal, and a backtick inside one desynchronises the scanner. In
  this package that means a URL assembled inside a template is not reported by
  the URL scan. The dynamic-code scan is unaffected (a sink inside a template is
  not valid JavaScript anyway).
- **The regex/division heuristic is the classic one** — a `/` opens a regex
  where a value may start and divides where one has just ended. A crafted module
  can confuse it. Both directions are pinned by tests, and the failure mode is
  a *reported* file rather than a silently clean one in the common case, but it
  is a heuristic.
- **It is a string-level gate, not a dataflow analysis.** A remote URL
  assembled at runtime from fragments (`"https" + "://" + host`) is not seen.
  What *is* enforced is the structural half: every module specifier must be a
  literal that resolves inside the package, and a computed `import(…)` is a
  build failure precisely because it cannot be checked.

The exemptions above are the real risk surface, and they are why
`src/bundle.test.ts` asserts the *exempt* count on the built package rather
than only asserting that the scan is clean. A gate that finds nothing because
it looks at nothing is the failure mode this file is written against.

## Gates

```
pnpm --filter @selis/extension build          # tsc + pack + this gate
pnpm --filter @selis/extension check:bundle   # the same thing, named
pnpm --filter @selis/extension test           # builds, then the full suite
pnpm --filter @selis/extension typecheck      # tsc --noEmit
```

### Re-verifying it, and one way to verify it wrong

The gate was re-checked against this build by planting violations in the
**built** package (not the sources) and running the gate's own entry point:

```
cd apps/extension
pnpm run build                                   # tsc + pack + gate
# plant, then: node dist/pkg/extension/src/bundle-check.js
```

A remote URL literal appended to `dist/offscreen.js` fails with
`remote-url-literal`, file, line and URL; `eval("1+1")` appended to
`dist/extension/src/ext/wasm-worker.js` fails with `dynamic-code` at the line
it was planted on.

**The way to verify it wrong:** append to a built file *without a leading
newline*. `tsc`'s last line in every emitted module is
`//# sourceMappingURL=...`, so the planted statement joins that comment and
the gate - correctly - reports nothing, because it is a comment. A gate that
"passed" a plant that was never code has told you nothing at all.
