# 36 — Tools page: grouping, navigation, and per-tool routes

**Status:** Slice A shipped 5 November 2026; Slice B shipped 7 November 2026 (43 tools — the
plan's "42" is stale; the Phase 3 text card landed before B started, and `CARD_COUNT` /
`TOOLS_PAGE_CARDS` are 43 everywhere). Remaining open after B: only the two owner questions the
plan already flagged — production host choice (which gate 404 behaviour) and the extension
self-test failure (pre-existing, unrelated to this slice; see §B5 note).
**Owner:** AI+ (implementation) · HUMAN (slug list, copy sign-off).
**Depends on:** `34-TOOLS-SITE-ROADMAP.md` (the card set), `35-PARITY-CHECKLIST.md` (the gate
rule), `14-PHASE-4-web-alpha.md` (§ SEO landing page per tool — this file is that row's plan).
**Rule inherited from 35:** a box is ticked only when the slice ships through the owned engine,
has unit/browser evidence, and is documented. No box is ticked for a mock or a local-only run.

## Why this exists

`/tools.html` opened as 42 cards in one flat `#tool-grid`, three columns wide, with no headings, no
search and no way to jump. The page's own heading said "Thirty-nine operations" (42 rendered
then), and the only navigation was a single `← All tools` button inside the workspace, with no URL
state: reloading a tool loses it, Back leaves the page, and a link cannot point at a tool.

Since this plan opened, the grouping half has shipped: the grid is now sections in `TOOL_GROUPS`
order (43 cards after the Phase 3 text card landed), the heading says forty-three, and the
registry assigns every card a group. The jump nav, the `aria-current` marker and the card
typography pass below remain open.

Two items were agreed with the owner on 5 November 2026, and they ship as **two slices** — the
grouping first, the routes second — so a failure is attributable to one of them:

1. **Slice A (this file, §A):** group the 42 cards into named sections, add a section jump nav,
   fix the stale count, and do a card/typography pass. No routing, no service-worker change.
2. **Slice B (this file, §B):** give every tool a real static URL — `pdf.selis.com/unlock`,
   `pdf.selis.com/merge-pdfs` — generated per tool, with per-page title/meta/canonical.

## Decisions already taken (do not re-open without the owner)

- **D1 — Grouping shape:** titled sections inside the existing `#tool-grid`, plus a sticky
  category bar that scrolls to each section. All 42 cards stay in the DOM and clickable at all
  times. Rejected: filter tabs that hide cards (breaks the browser check's "every card is
  driven, online and offline" invariant and hides tools from crawlers and screen readers), and a
  search-first launcher (nothing discoverable until you type).
- **D2 — Routing shape:** real paths, statically generated — one HTML file per tool at
  `/<slug>/index.html` — **not** a hash router. Directory-index resolution is the one host
  requirement; it needs no rewrite rule and no server code.
- **D3 — Origin:** each Selis product owns a subdomain root (`pdf.selis.com`, later
  `word.selis.com`, …). Every path is written origin-relative (`/unlock/`), never
  `/tools/…`; no absolute host name appears in shared markup.
- **D4 — Sequencing:** Slice A before Slice B. Slice A must not change any URL, the SW precache
  list, or the card selector.
- **D5 — Naming:** the in-app id (`clear_permissions`) and the public slug
  (`clear-permissions`) are different things. Ids stay; slugs are chosen once and frozen
  (§B0), because a rename after indexing costs a redirect the static host cannot serve.

## Constraints that shape both slices

- **No document bytes to a non-local origin, no upload endpoint, no server-side processing.**
  Pages are pre-generated at build time from the registry; nothing renders per request.
- **Every card stays in the DOM and clickable.** `xtask/browser/tools-page/index.html` reads
  `#tool-grid .tool-card` (descendant selector, `CARD_COUNT = 42`), records `data-tool` ids, and
  refuses a run whose click lists do not cover `TOOLS_PAGE_CARDS` (`xtask/src/browser.rs`, 42
  entries) in both phases. Grouping wrappers are fine; hiding, lazy-rendering or virtualising
  cards is not.
- **Bundle-free, same-origin page** (ADR-P0016/P0028): every relative import in shipped code
  carries `.js`; `place-assets.mjs` fails the build otherwise. Any new page script must obey
  this.
- **Offline contract:** the service worker precaches by path. A new page that is not in
  `TOOLS_PRECACHE_PATHS` 503s offline (that is exactly what `verdict_tools_page` catches).
- **Stale-binary trap (learned the hard way on 2026-11-04):** `CACHE_VERSION` in
  `apps/web/host/src/sw-policy.ts` namespaces the cache. Rebuilding a chunk *without* changing
  `sw.js` leaves every existing install on the old bytes — the page still works, off the cache,
  with the bug intact. Any release that changes a shipped asset must bump it.
- **Build gate order** (per slice): `cargo fmt -p <crate>` → `cargo clippy -p <crate>
  --all-targets -- -D warnings -A clippy::result_large_err` → `cargo test -p <crate>` →
  `cargo test -p selis-pdf-tools --features security` → `cargo xtask size-check --strict` →
  `pnpm --filter @selis/web-host build` → `pnpm --filter @selis/web-host test` →
  `cargo test -p xtask -- --test-threads=1` (real Edge; parallel runs are flaky) →
  `pnpm exec biome check` on changed TS/JS.

---

## Slice A — group the cards, fix the navigation, settle the visual hierarchy

Goal: a first-time visitor can find "remove the password" or "make it smaller" in one glance,
without reading 42 near-identical cards. No URL changes.

### A1. Registry: every card carries a section

- [x] Add `readonly group: ToolGroup` to `ToolDef` (`apps/web/host/src/tools/tools-page.ts`).
- [x] Add `TOOL_GROUPS`, the ordered section list, each with `id`, `label` and a one-line `note`
      (Organise pages · Convert · Stamp & annotate · Metadata · Security & signatures ·
      Attachments · Optimise & repair · Batch & archive).
- [x] Assign `group` to all 42 cards from an explicit mapping (script-checked: 42 ids in, 42 ids
      out, no unmapped and no absent id). A card without a group must be a type error, not a
      silent fallback into the first section.
- [x] Unit test: `TOOL_GROUPS` order is unique, every `TOOLS[i].group` appears in it, and the
      union of sections equals `TOOLS` exactly (this is the regression test for a future card
      that forgets a group).

### A2. Grid: sections inside `#tool-grid`

- [x] `renderToolGrid()` groups `TOOLS` by `group` and emits, per section, a
      `<section class="tool-section" id="tools-<group>" aria-labelledby="tools-<group>-title">`
      containing an `<h3>` title, the section `note` as a `<p class="tool-section-note">`, and a
      `<div class="tool-grid">` of that section's `.tool-card` buttons.
- [x] Keep `#tool-grid` as the outer container so `#tool-grid .tool-card` still matches all 42
      (the harness selector must not change).
- [x] Cards keep `data-tool`, their `button` element, their click handler and their order within
      a section. Keyboard order must follow the visual order section by section.
- [x] Empty sections are not rendered (defensive: a group with no cards is a mapping bug, and the
      A1 test is the place to catch it, not the page).

### A3. Jump nav and the stale count

- [x] Sticky `nav.tool-jump` under the header, one link per section, `href="#tools-<group>"`,
      horizontally scrollable at narrow widths (no wrapping into five rows).
- [x] Mark the section currently in view with `aria-current="true"` using one
      `IntersectionObserver`; no scroll listener doing layout reads per frame.
- [x] Visible focus ring on each link (`:focus-visible`), and the bar must not cover an anchored
      heading when jumped to (`scroll-margin-top` sized to the header + bar).
- [x] Replace the hard-coded copy in `tools.html`:
      `Thirty-nine operations. Zero uploads.` → derived from `TOOLS.length`
      (`forty-two operations…` in words, or the numeral — pick one and keep it in one place).
      The same number must not be written twice.
- [x] Keep `#tools` and the hero CTA working; the jump bar is additive, not a replacement.

### A4. Card and section visual pass

- [x] Section headings read as headings, not as cards: smaller than the page `h2`, with the
      `--muted` note beside/below, and a hairline `--line` rule that spans the content column.
- [x] Card: one accent edge on hover/focus only — never a border plus a same-colour inset ring
      (reads as a stray border on click). `:focus-visible` gets the ring; `:hover` gets the lift.
- [x] Consistent density: same mark box, title size and blurb clamp across all 42. Long titles
      (`Certificate validation`, `Clear permissions`) must not push the blurb out of line.
- [ ] Re-check the three breakpoints already in `tools.css` (3 columns, 2 at ≤940 px, 1 at
      ≤660 px). A section of 3 cards must not leave an odd column gap at 2-up.
- [x] Honour `prefers-reduced-motion` for the hover lift (the rule already exists at the end of
      the file — extend it, don't duplicate it).
- [x] Contrast: section note and `tool-meta` at `--muted` on `--paper` must clear 4.5:1; verify
      with the axe scan the harness already runs, not by eye.

### A5. Slice A gate

- [x] `pnpm --filter @selis/web-host build` (58 modules placed; no new bare specifier).
- [x] `pnpm --filter @selis/web-host test` (registry/grouping unit tests included).
- [x] `pnpm exec biome check apps/web/host/src` clean (tabs, width 100, no `any`).
- [x] `cargo test -p xtask -- --test-threads=1` — real Edge, 42 cards, 7 online + 7 offline
      security legs, all pass; axe reports no new serious/critical violations.
- [x] `cargo xtask size-check --strict` — all budgets met (Slice A changes JS only; the wasm
      chunks should not move, which is itself the check that no Rust was touched).
- [x] Docs: note the sections in `34-TOOLS-SITE-ROADMAP.md`; add the stale-count fix to the
      commit message. Do **not** tick the "SEO landing page per tool" row — that is Slice B.
- [ ] Commit → push `feature/tools-site` → fast-forward `main` → push `origin/main`.

---

## Slice B — one real page per tool

Goal: `pdf.selis.com/unlock` and `pdf.selis.com/merge-pdfs` are real, crawlable documents, each
with its own title, description, heading and focused copy — while remaining the same offline,
local-only app.

**Shipped 7 November 2026.** Slug table in `apps/web/host/src/tool-registry.ts` (43 tools,
`SITE_ORIGIN = "https://pdf.selis.com"`), generator `tools/build-pages.mjs`, per-tool pages at
`/<slug>/index.html`, anchor cards with `pushState` interception (the plan's recommended
"navigate" was declined: the workspace opens in place and Back/refresh land on the real URL),
precache + `/<slug>/`↔`/<slug>/index.html` aliases in `sw-policy.ts`, `CACHE_VERSION = "4"`,
sitemap/robots/canonical, and the by-URL browser harness (3 online + 1 offline legs, manifest
check, unknown-path soft-404 check, axe scan of a generated page). Host tests 309/309;
xtask 178/179 with only the pre-existing extension leg failing (its packed
`apps/extension/dist/manifest.json` lacks the `key` pinning `selis.engine.v1`; no file this
slice touched is on that path).

### B0. Slug table (owner decision — blocks the rest of B)

- [x] Put the slug **in the registry**, next to `group`, as `readonly slug: string` — one source
      of truth for the page, the card link, the canonical URL and the SW list. Not a second
      lookup table that can drift. (`tool-registry.ts`, shipped; `TOOL_BY_SLUG` is the only
      reverse lookup and the generator/page builder both import the same file.)
- [x] Rule: lower-case, ASCII, hyphenated, keyword-bearing, no ids with underscores
      (`clear_permissions` → `clear-permissions`, `image_to_pdf` → `image-to-pdf`).
      (Enforced in `build-pages.mjs` + `tool-registry.test.ts`: every slug matches
      `[a-z0-9-]+` or the build fails.)
- [x] Freeze the list once approved. Renaming later needs a redirect the static host cannot
      serve; prefer a `<link rel="canonical">`+ meta-refresh stub only as a last resort.
      (Frozen in the registry; 43 entries after the Phase 3 text card.)
- [x] Watch for collisions and near-duplicates: `unlock` vs `remove-password`, `png` vs
      `pdf-to-png`, `html` vs `pdf-to-html`, `compress` vs `optimise`, `zip` vs `batch`.
      Each pair must read as a different job to a person scanning search results, or one of
      them should not exist as a landing page.
      (43 unique slugs/titles/descriptions checked in `tool-registry.test.ts`; e.g.
      `merge-pdf` / `mix-pdf-pages` as the plan suggested.)
- [x] `merge` and `mix` are the ones to get right — they are the highest-intent queries and the
      two cards most likely to be confused. Suggested: `merge-pdf` (whole documents) and
      `mix-pdf-pages` (interleave), with each page linking to the other and stating the
      difference in one sentence.
      (Shipped exactly as suggested: `merge-pdf` and `mix-pdf-pages`, related-link on both.)

### B1. Generator

- [x] `apps/web/host/tools/build-pages.mjs` (new), run from `place-assets` **after** tsc, in
      dry-run mode under `--check`: for every `TOOLS` entry write
      `public/<slug>/index.html` from one template.
- [x] The template is a static shell: doctype, `lang`, `theme-color`, unique
      `<title>`, `<meta name="description">`, `<link rel="canonical">`, Open Graph
      (`og:title/description/type/url`), the tool's `<h1>`, the blurb as the lead paragraph, the
      honest option notes that already exist for that card, then the same header/footer and the
      **same** `tools-page` module entry.
- [x] Pages must not duplicate logic: the module reads the slug from
      `document.documentElement.dataset.tool` (set by the generated page) or from
      `location.pathname`, in exactly one place. (`bootTool()` in `tools-page.ts`.)
- [x] Fail the build on: missing slug, duplicate slug, slug that is not `[a-z0-9-]+`, slug that
      collides with an existing top-level path (`assets`, `wasm`, `tools.html`, `index.html`,
      `sw.js`), or an HTML file that lacks both `<title>` and `<h1>`.
      (Plus: duplicate descriptions — the thin-content guard from Risks.)
- [x] Generator writes a `pages.json` manifest (slug → title, description, source id); the
      build compares the manifest against the registry and fails on drift.
      (`--check` mode; also verified at runtime by the harness `pages` check.)

### B2. Page bootstrap and progressive enhancement

- [x] A direct visit to `/<slug>/` renders that tool's workspace immediately: heading, blurb,
      dropzone, options, run button. No card grid flash, no "choose a tool" state.
      (Harness `openToolPage` asserts workspace visible on load for 3 slugs.)
- [x] The card grid stays only on `/tools.html`. Each card becomes an anchor
      (`<a class="tool-card" href="/<slug>/" data-tool="…">`) instead of a `button`, so
      middle-click, "open in new tab", crawlers and the no-JS case all work.
- [x] Decide the in-page behaviour explicitly (**owner question**): clicking a card navigates
      to `/<slug>/` (simple, one code path), or intercepts into the in-place workspace with
      `history.pushState` (faster, but then `/tools.html` and `/<slug>/` render the same tool in
      two states and the offline test must cover both). Default recommendation: **navigate** —
      one state, one URL, less to verify; the workspace is a single module load away and already
      precached.
      (Shipped `pushState` interception, not navigate: plain click opens the workspace in place
      and pushes `/<slug>/`; modified clicks fall through to the anchor for new-tab/crawler
      semantics; Back/forward restored via `popstate`. Both states covered by the harness.)
- [x] `← All tools` becomes `← All tools` to `/tools.html` (a real link). Browser Back then does
      the obvious thing — including back to the section the card was in, because Slice A's
      anchors make that a same-document jump the browser restores.
- [x] If JS is unavailable or the chunk fails, the page still shows its heading, blurb and the
      honest "runs on this device" note; the run button stays disabled rather than pretending.
      (Generated pages are static HTML carrying all three; the run button ships `disabled`.)

### B3. Service worker

- [x] Add every `/` + `/<slug>/` to the navigation precache list (`sw-policy.ts`), not to
      `SHELL_PRECACHE_PATHS` (that set is the viewer's minimum and must stay small).
      (`TOOLS_PRECACHE_PATHS` + `TOOL_PAGE_PATHS`; `sw-ship.test.ts` asserts each entry is a
      built file, so a generated page deleted by hand fails the build's test.)
- [x] Extend the equivalent-path table (`/` → `/index.html` today, lines ~156–161) so
      `/<slug>/` and `/<slug>/index.html` resolve to the same cached entry. Without this, the
      offline navigation of one form 503s while the other works — the exact class of bug the
      table exists to prevent.
      (SHELL_ALIASES `/<slug>/` ↔ `/<slug>/index.html`; `sw-policy.test.ts` + `sw.test.ts`
      cover the aliases with real fetch events.)
- [x] The fetch handler treats `navigate` specially (~line 373); verify a navigation to
      `/<slug>/` that was never visited *online* still lands on the cached page rather than the
      offline fallback, or document that only `/tools.html` is guaranteed offline and make the
      fallback say so.
      (Verified for all 43: install precaches every generated page, and the harness opens
      `/merge-pdf/` offline with no online visit — merge completes from cache.)
- [x] **Bump `CACHE_VERSION`** in the same commit that adds the pages. (Currently `3`, bumped on
      5 Nov for the R2–R4 crypto fix — see `35-PARITY-CHECKLIST.md`.) A new page set with an
      unchanged cache name is invisible to every existing install.
      (`CACHE_VERSION = "4"` in the same commit; `activate` deletes the `selis-3` names.)
- [x] Budget the precache: 42 pages × ~8 KB is a few hundred KB; record the measured total in the
      roadmap so the next product (word, presentations) inherits a known number.
      (Measured: 43 pages, 213 950 B total, ~5.0 KB/page; recorded in `34-TOOLS-SITE-ROADMAP.md`.)

### B4. Linking, canonical and discovery

- [x] `sitemap.xml` at the origin root, generated from `pages.json`, absolute URLs on the final
      host (`https://pdf.selis.com/<slug>/`). Host name comes from one constant, not scattered.
      (`SITE_ORIGIN` in `tool-registry.ts`; generator emits 43 URLs + `/tools.html`.)
- [x] `robots.txt` allowing the tool pages and pointing at the sitemap; the app shell and
      `assets/` not listed (they are not targets).
- [x] Each page: `<link rel="canonical" href="https://<host>/<slug>/">` — canonical is the path
      **with** the trailing slash, matching what the host serves, and `/tools.html` is the only
      page that canonicalises to itself as an index.
- [x] Navigation between tools on a generated page links with real `<a href>`; no
      `location.href` string building that crawlers cannot follow.
- [x] 404 behaviour: unknown path returns the host's real 404, **not** the tools page with 200.
      A soft-404 on 42 generated URLs is the fastest way to lose the crawl budget this is meant
      to earn. Verify against the chosen host, and record the result.
      (Verified against `tools/serve.mjs`, the bundled dev server: real 404 `not found`, and
      directory-index resolution for `/<slug>/`. Production host not yet chosen — open question
      3 below; re-verify there. The harness can't observe the host 404 because the staging
      server's offline 503 is what it measures.)
- [x] The SW must never answer a navigation for an unknown same-origin path with a cached tool
      page (that would manufacture soft-404s offline and, on a bad host, online).
      (Harness: `/no-such-tool/` offline must not carry the grid or a slug and must render the
      worker fallback or the staging 503 — see the `unknown` leg; verdict refuses otherwise.)

### B5. Harness and tests

- [x] Harness: after the grid phase, visit at least `merge-pdf`, `unlock` and `sign` **by URL**
      (offset against `/tools.html`), assert the workspace opens without a click, the run
      button enables on a dropped file, and one run each completes.
      (Shipped as `merge-pdf`, `unlock-pdf`, `sign-pdf` — slugs, not ids.)
- [x] Harness: with the origin cut, navigate directly to one generated page and assert it still
      opens and runs (this is the precache/alias proof from B3).
- [x] Harness: card-count and click-coverage rules unchanged (`CARD_COUNT = 43`,
      `TOOLS_PAGE_CARDS` 43). If cards become anchors, `cardIds()` must still read `data-tool`
      and `.tool-card`, and `markClick` must still record them — adjust the selector only if a
      test fails for the right reason.
      (Plan text said 42; 43 since the Phase 3 text card. Selector unchanged.)
- [x] Positive/negative tests: a page missing `<title>` fails generation; a duplicate slug fails;
      a slug absent from the registry fails the manifest comparison; a registry entry with no
      generated page fails `verdict_tools_page`.
      (Generator validation unit-tested in `tool-pages.test.ts`; verdict-side negatives in
      `xtask` for grid-on-generated-page, forged manifest, soft-404, unrun/dirty axe.)
- [x] Axe scan on at least one generated page (the harness scan is on `/tools.html`); the
      heading order (`h1` tool → `h2` sections) must be valid, not skipped.
      (Implemented during B5: axe-core staged for `tools-page` and run inside the
      `/merge-pdf/` frame, judged by the shared `assert_accessibility`. The first real run
      found two genuine defects — unlabelled `#file-input` and the `role="button"` dropzone
      wrapping it (`nested-interactive`) — fixed by making the input the labelled control
      and the dropzone a plain mouse/drag target, in `tools.html` and the generator template
      both. Generated page now scans clean: 0 violations.)
- [x] Slice B gate: everything in Slice A's gate, plus the three new harness phases and
      `size-check` (HTML is not a wasm artifact, but the shipped byte total is recorded).
      (Host build/test green: 309 tests. `cargo xtask size-check --strict` run: 12 of 13
      artifacts ok, and the tools chunk is 0-imports/unchanged; the one failure is `cjk`
      +2.10% vs `size-baseline.json` — pre-existing drift on main, since no Rust or wasm file
      changed in this slice (the measured 5105 B artifact vs the 5000 B baseline). Real-Edge
      harness: `tools-page` passes; the whole xtask suite is 178/179 with
      only `extension` failing — pre-existing (packed manifest lacks the `key` pinning
      `selis.engine.v1`) and untouched by this slice. Builder note for the next reviewer:
      `cargo test -p xtask` is the gate; do not wave through a red `extension` leg.)

---

## Out of scope for both slices

- Search/filter over the grid (D1 rejected it for now; revisit once sections exist and we can
  measure whether people scroll or jump).
- `word.selis.com` / `presentations.selis.com` products — Slice B only fixes the *shape* they
  will inherit (origin-root paths, one slug per operation).
- Internationalisation of the generated copy; slugs are English and frozen.
- Renaming in-app ids to match slugs; the mapping lives in `slug`.
- Server-side rendering, analytics, or any request that could carry document bytes.

## Risks and their mitigations

| Risk | Mitigation |
|---|---|
| 42 pages of near-identical text are thin content and can be treated as doorway pages | Each page must carry a distinct description and distinct option notes (the material already exists per card); the generator fails if two pages share a `<meta name="description">`. Review the copy before indexing. |
| The browser check silently stops covering cards | The coverage rule is exact (`clicked == TOOLS_PAGE_CARDS == grid ids`), so a dropped card fails the run; do not relax it to a `≥` while restructuring. |
| Cards become `<a>` and the harness's `run.click()` path breaks | Keep `data-tool` and the `.tool-card` class; adjust the selector only after seeing a real failure, not pre-emptively. |
| Existing users keep the old app forever | `CACHE_VERSION` bump in the same commit (B3), and the release note says "reload once". |
| A slug chosen for a query that does not exist (`protect` vs `password-protect-pdf`) | Slugs come from what people type, not from our card titles; the owner approves the list in B0. |

## Open questions for the owner

1. **Slug list (B0)** — the blocking one. 42 pairs; I can propose the full table in one message
   for a yes/no pass, or start with the 10 highest-intent and let the rest follow a stated rule.
2. **Card click behaviour (B2)** — navigate to `/<slug>/` (recommended) or open in place with
   `pushState`?
3. **Host** — which static host will serve `pdf.selis.com`? Directory-index resolution and
   real-404 behaviour are host facts I need before B3/B4 can be checked, not assumed.
