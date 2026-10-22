# SL-4.UI.14 stage 3 - React + `@wertkit/ui` dependency proposal

**Status:** Proposed, awaiting approval. **No package manifest has been edited.**
**Date:** 2026-10-22
**Parent:** `14-PHASE-4-web-alpha.md`, SL-4.UI.14 stage 3
**Policy:** ADR-P0021 revision accepted in stage 2; `SECURITY.md` exact-pin rule

## 1. Decision requested

Approve, as one change set:

1. `apps/ui` runtime dependencies `react@19.3.1`, `react-dom@19.3.1`, and
   `@wertkit/ui@0.1.8`, all exact pins.
2. Dev dependencies `@types/react@19.3.0`, `@types/react-dom@19.3.0`, and
   build-only `esbuild@0.28.2` (exact).
3. The ten `@radix-ui/*` packages that `@wertkit/ui` declares, as transitive
   dependencies pinned by `pnpm-lock.yaml` - not added to any manifest.
4. A **chrome-only vendor bundle**: third-party code is bundled once into
   `assets/ui/vendor/vendor.js`; app code remains native `tsc` ESM. This
   revises the earlier "no bundler" decision only for the React/Radix vendor
   closure, not for `apps/ui`'s own modules.
5. A `--wk-*` -> `--selis-*` bridge in `packages/ui-kit/css/wertkit-bridge.css`,
   with a test that every variable `@wertkit/ui` consumes is defined.
6. The lockfile/CI/validation changes in sections 8 and 9.

If item 4 is not approved, stage 3's DoD cannot be met: `@wertkit/ui` cannot be
loaded by the web host as it stands (section 5). The dependencies should then
not be added yet.

## 2. Why this is still a proposal

`00-INDEX.md`'s dependency rule says a dependency outside
`[workspace.dependencies]` needs a licence and maintenance justification.
Stage 2 recorded the policy and accepted ADR-P0021's npm revision, but stage 3
is the first actual manifest change. This document is the proposal that rule
asks for. It is deliberately separate from the implementation so the pins,
licences, bundler exception, and validation can be rejected or amended before
anything is installed.

## 3. Exact dependency set

### 3.1 Direct, in `apps/ui/package.json`

| Package | Version | Kind | Licence | Why |
| --- | --- | --- | --- | --- |
| `react` | `19.3.1` | runtime | MIT | `@wertkit/ui` peer; current stable; `^19.3.1` peer satisfied exactly |
| `react-dom` | `19.3.1` | runtime | MIT | `@wertkit/ui` peer; `createRoot` host |
| `@wertkit/ui` | `0.1.8` | runtime | `UNLICENSED`, first-party | organisation design system; pre-approved scope |
| `@types/react` | `19.3.0` | dev | MIT | TypeScript JSX/hooks |
| `@types/react-dom` | `19.3.0` | dev | MIT | `createRoot` types |
| `esbuild` | `0.28.2` | dev, build-only | MIT | vendor bundle generator; not shipped as a runtime dependency |

No caret, tilde, `workspace:*`, or `latest` on any of these. `apps/ui` keeps its
existing `@selis/ui-kit` workspace dependency and existing dev dependencies.

**Why React 19.3.1 and not 18?** `@wertkit/ui@0.1.8` peers
`react`/`react-dom` `^18.2.0 || ^19.0.0`. React 19 is current, and 19.3.1 is the
latest stable at the proposal date. Pinning the pair together avoids a
`react-dom`/`react` peer mismatch.

**Why esbuild 0.28.2 and not the already-locked 0.21.5?** 0.21.5 exists only
transitively via Vite 5.4.21. It predates the esbuild dev-server advisory; the
bundle script does not run `esbuild serve`, but a direct build-tool pin should
not be a known-old version. 0.28.2 is MIT, current, and actively maintained.
The cost is a second esbuild version in the dev tree; it is build-only and
never reaches `public/`.

### 3.2 Transitive Radix set

`@wertkit/ui@0.1.8` declares **ten**, not nine, Radix packages. All are MIT,
maintained by WorkOS in `radix-ui/primitives`, and current releases were
published 2026-10-12.

| Package | Declared range | Expected resolution |
| --- | --- | --- |
| `@radix-ui/react-checkbox` | `^1.1.4` | `1.4.1` |
| `@radix-ui/react-context-menu` | `^2.2.6` | `2.4.1` |
| `@radix-ui/react-dialog` | `^1.1.6` | `1.3.1` |
| `@radix-ui/react-dropdown-menu` | `^2.1.6` | `2.2.1` |
| `@radix-ui/react-select` | `^2.1.6` | `2.4.1` |
| `@radix-ui/react-slot` | `^1.1.2` | `1.5.0` |
| `@radix-ui/react-switch` | `^1.1.3` | `1.4.1` |
| `@radix-ui/react-tabs` | `^1.1.3` | `1.2.1` |
| `@radix-ui/react-toast` | `^1.2.6` | `1.3.1` |
| `@radix-ui/react-tooltip` | `^1.1.8` | `1.4.1` |

The resolutions come from a probe lockfile made with pnpm 12.14.0. The repo
declares **pnpm 11.21.0**, so these are expected, not final. After approval,
re-resolve with the repo's pnpm and record the actual versions. The lockfile,
not this table, is the integrity record.

The ten direct Radix packages pull internal `@radix-ui/*`, `@floating-ui/*`,
`react-remove-scroll`, `use-callback-ref`, `use-sidecar`, `aria-hidden`,
`tslib`, and `scheduler` packages. Those are permissive (MIT/ISC/0BSD) and are
also locked by `pnpm-lock.yaml`. They are not listed in a manifest.

**No `pnpm.overrides` for Radix is proposed.** The lockfile already pins the
resolved versions, and an override would assert a compatibility the upstream
package did not declare. If the browser validation finds a regression in a
newer Radix minor, pin the failing package with an override in a follow-up
proposal, with the failing interaction recorded.

### 3.3 Not added

| Package | Decision |
| --- | --- |
| `@wertkit/tokens` | **Not added.** It defines 99 `--wk-*` variables but has no `data-contrast` high-contrast theme. `ui-kit` stays the token source, as stage 1 and UI.10 require. |
| `@wertkit/ui` subpath packages | None exist. One root package plus `styles.css` only. |
| a React meta-framework or state library | Not needed for chrome. |
| `jsdom`, `happy-dom`, Testing Library | Not proposed. Unit tests can use `react-dom/server`; real interactions belong in the existing Edge harness. |

## 4. Licence and maintenance record

### 4.1 `@wertkit/ui@0.1.8`

- npm licence field: `UNLICENSED`; tarball SHA-1
  `b4ee4d6db2613014e798db73039bac26f137057a`; integrity
  `sha512-vqOGgor/diaLX411V6smH+BoDoT+pS0F/IlStOQCKkVAk0TPmaWEL3F3P3tEF9Wr6jkA6foagj9TmrCj7/9TeA==`.
- Published 2026-09-25; latest at the proposal date; 0.1.8 is pre-1.0.
- No `repository` or `homepage` in the npm metadata, no published changelog,
  one maintainer (`prasadthx`). This is a provenance/documentation gap, not a
  licence question: it is first-party and pre-approved by ADR-P0021.
- Mitigations: exact pin, committed lockfile, integrity hash, isolated chrome
  entry, no adoption of its tokens/ThemeProvider, and a bundle built from the
  pinned tarball with an input allow-list.

The npm licence gate, when it lands, must exempt the **scope** `@wertkit/*`, not
the string `UNLICENSED` generally. That is the only way this package passes
without weakening the rule for third-party packages.

### 4.2 React and Radix

- React/ReactDOM: MIT, Meta, latest 19.3.1 published 2026-10-14; very active.
- `@types/react`/`@types/react-dom`: MIT, DefinitelyTyped, 19.3.0 published
  2026-09-09; active.
- Radix: MIT, WorkOS, ten direct packages current on 2026-10-12; active.
- `esbuild`: MIT, `evanw/esbuild`, 0.28.2 published 2026-08-08; active,
  build-only.

No GPL/LGPL/AGPL/SSPL/CDDL is introduced. The one-off stage 3 validation will
record `pnpm licenses list --json` and stop on anything outside the ADR-P0021
allow-list. This is not the deferred permanent npm gate; it is evidence for
this change set.

## 5. The bundler/loader blocker (the part stage 3 did not specify)

### 5.1 What the current host assumes

`apps/web/host/tools/place-assets.mjs` copies `apps/ui/dist` and
`packages/ui-kit/dist` as **bundle-free ESM**. It rejects every bare or remote
specifier except `@selis/ui-kit`, which it rewrites to a relative URL. That is
the "decided for: native ESM, no bundler" outcome recorded in
`14-PHASE-4-web-alpha.md` (WEB.02 status note).

`@wertkit/ui@0.1.8`'s `dist/index.js` immediately violates that assumption:

```js
import { jsx, jsxs, Fragment } from "react/jsx-runtime";
import { useState, useEffect, ... } from "react";
import * as Slot from "@radix-ui/react-slot";
import * as Select from "@radix-ui/react-select";
// ... eight more @radix-ui/* imports
```

The browser has no resolver for those names. An import map alone is not enough:
React and ReactDOM are CommonJS packages, and the Radix ESM build has its own
bare imports (more than 60 package entries in the probe lockfile). A CJS loader
or a bundler is required either way.

### 5.2 Recommended: bundle the vendor closure, keep app code native ESM

- Add `apps/ui/src/chrome/vendor.ts`, a **curated** re-export surface:
  - the React 19 hooks/APIs chrome uses (`useState`, `useEffect`,
    `useLayoutEffect`, `useMemo`, `useCallback`, `useRef`, `useContext`,
    `createContext`, `useId`, `useReducer`, `forwardRef`, `memo`, `Suspense`,
    `StrictMode`, `Fragment`, ...);
  - `jsx`/`jsxs` from `react/jsx-runtime`;
  - `createRoot` from `react-dom/client`;
  - the ten wertkit component families listed in section 7.2.
- `apps/ui/tools/build-vendor.mjs` runs esbuild on that entry:
  `bundle: true`, `format: "esm"`, `platform: "browser"`, `target: "es2022"`,
  `jsx: "automatic"`, `define: { "process.env.NODE_ENV": "\"production\"" }`,
  `minify: false`, `sourcemap: false`, `legalComments: "eof"`,
  `metafile: true`.
  Output: `apps/ui/dist/vendor/vendor.js` and `vendor.meta.json`.
- `apps/ui/tools/copy-vendor-css.mjs` copies `@wertkit/ui/styles.css` to
  `apps/ui/dist/vendor/wertkit.css`. It does **not** copy `tokens.css`.
- `apps/ui`'s own chrome modules are still emitted by `tsc` as native ESM. They
  import bare `react`, `react/jsx-runtime`, `react-dom/client`, and
  `@wertkit/ui`; `place-assets.mjs` rewrites exactly those four names to
  `../vendor/vendor.js` and leaves everything else relative. The walk continues
  to reject any other bare specifier.
- `place-assets.mjs` gains a second `apps/ui/dist` root for
  `chrome/index.js` with an explicit served path (`chrome/index.js`), so its
  relative imports to `../viewer/*.js` resolve under the existing `/assets/ui/`
  tree. It also copies the vendor JS/CSS and the five CSS files explicitly.
- `vendor.js` has **no** imports of its own: it is a closed bundle. The
  app-level chrome modules remain reviewable tsc output, which is the property
  the no-bundler rule was protecting.

### 5.3 Bundle audit

The build fails, rather than shipping, if:

- `vendor.meta.json` lists an input outside the pinned allow-list (React,
  ReactDOM, scheduler, `@wertkit/ui`, the ten Radix packages and their
  permissive internal helpers, and `apps/ui/src/chrome/vendor.ts`);
- `vendor.js` contains a bare module specifier, a remote URL in code, `eval`,
  `new Function`, or `document.write`;
- `vendor.js` exceeds the measured size budget (set after the first build; this
  is a review budget, not a performance claim);
- `chrome/index.js` or any walked chrome module still contains an unresolved
  bare specifier (the existing `place-assets` failure, extended to the new
  names);
- the expected curated exports are missing (a Node import of `vendor.js`
  checks each name).

The vendor bundle is build output under `public/`, which is already ignored. It
is not committed; the pinned lockfile and the generator are.

### 5.4 CSS loading

No CSS is injected from JavaScript, which would hit the host's CSP and make
review harder. The harness and, later, the host link the sheets in this order:

1. `/assets/ui-kit/css/tokens.css` (selis themes)
2. `/assets/ui-kit/css/wertkit-bridge.css` (the `--wk-*` aliases)
3. `/assets/ui/vendor/wertkit.css` (`@wertkit/ui/styles.css`)
4. `/assets/ui/css/page-list.css`
5. `/assets/ui/css/chrome.css`

`@wertkit/ui/tokens.css` is deliberately absent.

### 5.5 Rejected alternatives

| Option | Why not |
| --- | --- |
| Import map + vendored ESM | React/ReactDOM are CJS; Radix has 60+ bare package entries; still needs a CJS loader. More files, no less supply-chain surface. |
| Bundle the whole chrome entry | Makes app code opaque and broadens the exception from "vendor closure" to "the application". Rejected. |
| Adopt `@wertkit/ui` only in tests/storybook | Does not satisfy stage 3's DoD; the shipped UI would still not use it. |
| Add React and defer the loader | Leaves a dependency no host can run, and a misleading green build. Rejected. |

If the reviewer keeps the no-bundler decision, stage 3 is blocked and this
proposal should be closed without adding dependencies.

## 6. Token bridge

### 6.1 Why a bridge, not `@wertkit/tokens`

`@wertkit/ui/styles.css` consumes **87 distinct `--wk-*` variables and defines
none of them**. `@wertkit/tokens.css` defines 99, but it is a separate theme
system with no `data-contrast` high-contrast variant and would compete with
selis's generated tokens. Stage 1 already chose `ui-kit` as the source of truth;
UI.10's 7:1 high-contrast gate must remain intact.

The bridge is `packages/ui-kit/css/wertkit-bridge.css`, exported as
`@selis/ui-kit/css/wertkit-bridge.css`. It is hand-written, token-only, and
covered by a test.

### 6.2 Theme scope and portals

Define the aliases on `:root` and `.selis-root` (and on the theme attribute
scopes, if a host puts them there). `--wk-*` values must resolve **at the
element that carries the theme**, not only at `:root`, because a
`var(--selis-color-*)` alias is substituted at computed-value time.

For chrome, set theme attributes on `document.documentElement` via
`applyTheme(preferences, document.documentElement)`. Radix Dialog, Select,
Menu, and Tooltip portal under `<body>`; a theme scoped only to a nested
`.selis-root` would leave portals on the default theme. This is a constraint of
`@wertkit/ui@0.1.8` (its Dialog has no portal-container prop), not a preference.

### 6.3 Mapping

The bridge covers all 87 used variables. The direct families are:

| wertkit | selis |
| --- | --- |
| `--wk-bg`, `--wk-bg-raised`, `--wk-bg-inset` | `--selis-color-bg*` |
| `--wk-fg`, `--wk-fg-muted`, `--wk-fg-subtle` | `--selis-color-fg*` |
| `--wk-border`, `--wk-border-strong` | `--selis-color-border*` |
| `--wk-accent*`, `--wk-danger*`, `--wk-success*`, `--wk-warn*` | the matching `--selis-color-*` role |
| `--wk-focus-ring` | `--selis-color-focus-ring` |
| `--wk-space-*` | `--selis-space-*` |
| `--wk-radius-*` | `--selis-radius-*` |
| `--wk-text-*` | `--selis-text-*` |
| `--wk-weight-*` | `--selis-weight-*` |
| `--wk-font-*` | `--selis-font-*` |
| `--wk-shadow-sm` / `-md` | `--selis-shadow-1` / `-2` |
| `--wk-motion-fast` / `-base` | `--selis-duration-fast` / `-base` |
| `--wk-motion-ease` / `-ease-out` | `--selis-ease-standard` / `--selis-ease-entrance` |

The non-mechanical decisions, each intentional:

| wertkit | selis / value | Reason |
| --- | --- | --- |
| `--wk-bg-subtle` | `var(--selis-color-bg-inset)` | selis has no separate subtle surface; inset is the role that means "recessed/subtle". |
| `--wk-fg-on-solid` | `var(--selis-color-accent-fg)` | white-on-solid in all four selis themes; used by danger buttons and switch thumbs. |
| `--wk-info` | `var(--selis-color-accent)` | selis has no info role; mapping to accent avoids inventing an ungated colour. If info semantics become a product need, add an `info` token and its contrast test first. |
| `--wk-info-fg` | `var(--selis-color-link)` | readable on `accent-subtle` in light/dark/high-contrast; checked by the browser pass. |
| `--wk-info-subtle` | `var(--selis-color-accent-subtle)` | same family as the info mapping. |
| `--wk-overlay` | `color-mix(in srgb, var(--selis-color-shadow) 55%, transparent)` | themed, no colour literal; overlay sits behind the dialog, whose own surface is token-backed. |
| `--wk-syntax-variable` | `var(--selis-color-fg)` | the only syntax variable `ui.css` consumes; selis ships no syntax palette. |
| `--wk-border-thin` | `var(--selis-border-hairline)` | 1px hairline role. |
| `--wk-focus-width` | `var(--selis-border-thick)` | 2px focus ring. |
| `--wk-leading-*` | `var(--selis-leading-normal)` | selis has one body leading scale; copying wertkit's px values would fork typography. |
| `--wk-tracking-tight` | `normal` | no selis tracking token; `normal` is the deliberate fallback. |
| `--wk-z-sticky` | `var(--selis-z-toolbar)` | sticky table header. |
| `--wk-z-overlay` | `var(--selis-z-dialog-backdrop)` | scrim. |
| `--wk-z-modal` | `var(--selis-z-dialog)` | dialog. |
| `--wk-z-popover` | `calc(var(--selis-z-toast) - 1)` | popovers above modal, below toast. |
| `--wk-z-toast` | `var(--selis-z-toast)` | toast. |
| `--wk-safe-*` | `env(safe-area-inset-*, 0px)` | safe-area values are host facts; selis has no equivalent token. |
| `--wk-control-h-sm/md/lg` | `calc(var(--selis-density-control-height) -/+ 4/8px)` / exact | preserves density and keeps three sizes distinct. |
| `--wk-control-px` | `var(--selis-density-control-pad-x)` | density. |
| `--wk-surface-p` | `var(--selis-space-8)` | 24px, matching wertkit's default surface padding. |
| `--wk-cell-py` | `var(--selis-space-4)` | 8px table cell padding. |
| `--wk-titlebar-h` | `var(--selis-density-panel-header-height)` | selis's header height, not a new literal. |
| `--wk-sidebar-w` | `240px` | no selis token; AppShell also accepts `sidebarWidth` to override. |
| `--wk-dialog-w` | `480px` | matching wertkit default; Dialog accepts `width` to override. |
| `--wk-image-ar` | `auto` | component-level fallback; no app image ratio is imposed. |
| `--wk-table-header-bg` | `var(--selis-color-bg-raised)` | sticky table header surface. |
| `--wk-table-max-block` | `60vh` | structural scroll cap; no colour or theme dependency. |

Any remaining `--wk-*` variable is a direct role/space/text/etc. alias per the
table above. The bridge defines every one; it does not rely on wertkit's
fallbacks.

### 6.4 Bridge tests

- `apps/ui/src/chrome/wertkit-bridge.test.ts` reads
  `@wertkit/ui/styles.css`, extracts every `var(--wk-*)`, reads the bridge, and
  asserts each is defined exactly once.
- It asserts no definition uses a raw hex/rgb/hsl colour; only `var(--selis-*)`,
  `color-mix(...var(--selis-*)...)`, `env(...)`, `calc(...)`, or the documented
  structural literals (`240px`, `480px`, `60vh`, `auto`, `normal`).
- It asserts the bridge is exported by `@selis/ui-kit` and copied by
  `place-assets`.
- The browser check computes `color`/`background-color` for a Button, Dialog,
  Menu, Toast, and table header in light, dark, and high-contrast; a fallback
  colour or a transparent menu would fail. It also injects the existing
  `axe-core` root dev dependency.

## 7. What stage 3 builds

### 7.1 Boundary

- `apps/ui/src/chrome/` is the only new React/DOM code. It is **not** exported
  from `apps/ui/src/index.ts`; the viewer entry stays DOM-free and React-free
  (ADR-P0044).
- `apps/ui/src/chrome/index.tsx` exports `ChromeScreen` and
  `mountChrome(element, options)`. `mountChrome` calls
  `applyTheme(options.preferences, document.documentElement)` before
  `createRoot`, so portals inherit the theme.
- `apps/ui/tsconfig.json` gains `"jsx": "react-jsx"` and keeps the existing
  strict flags. `tsconfig.base.json` is not changed.
- The screen uses UI.02's real `createPageList` controller with the existing
  mock adapter fixture (or a small fixture adapter) and renders its
  `PageListState` tiles as DOM. It does not reimplement layout or windowing.
  That is what makes the DoD's "renders a real screen (UI.02's page list)"
  literal.

### 7.2 wertkit components

| Plan wording | Actual `@wertkit/ui@0.1.8` API |
| --- | --- |
| AppShell | `AppShell` |
| Button | `Button` |
| Dialog | `Dialog`, `DialogClose` |
| DropdownMenu | **`Menu`** (Radix dropdown-menu underneath), `MenuItem`, `MenuLabel`, `MenuSeparator`, `MenuSub` |
| Tooltip | `Tooltip`, `TooltipProvider` |
| Tabs | `Tabs`, `TabsList`, `TabsTrigger`, `TabsContent` |
| Select | `Select`, `SelectItem`, `SelectGroup`, `SelectSeparator` |
| Checkbox | `Checkbox` |
| Switch | `Switch` |
| Toast | `ToastProvider`, `useToast` |

The plan's "DropdownMenu" is a category, not an export. Stage 3 consumes
`Menu`; no fork and no alias package. This correction is recorded in the plan.

`ThemeProvider` and `ThemeScript` are **not** consumed. Selis's `applyTheme`
owns `data-theme`, `data-contrast`, `data-density`, and `data-motion`; wertkit's
ThemeProvider writes its own theme/density keys and would fight UI.10.

### 7.3 Browser screen and check

Add `xtask/browser/chrome/index.html` (test page, not the deployment root) and
a `chrome` check. The page links the five CSS files, sets `<html>` theme
attributes from the query string, imports `/assets/ui/chrome/index.js`, and
posts a verdict. The check exercises:

- mount and visible AppShell landmarks;
- Button, Checkbox, Switch state changes;
- Dialog open, focus into the dialog, Escape/Cancel close, focus restored;
- Menu keyboard open/select/Escape;
- Tooltip focus/hover;
- Tabs arrow-key selection;
- Select open/choose;
- Toast appearance and dismissal;
- the same screen in light, dark, and high-contrast, with `axe-core` clean.

`cargo xtask browser-check --strict` then reports 4/4 (or more), including the
existing three checks. A check that never reports is still a failure, not a
skip.

## 8. Lockfile and CI strategy

1. After approval, edit `apps/ui/package.json` with exact strings and add the
   ui-kit CSS export. Do not hand-edit `pnpm-lock.yaml`.
2. `pnpm.cmd --version` must report `11.21.0` (the repo's `packageManager`).
3. `pnpm.cmd install` from the workspace root. Then inspect:
   - `pnpm.cmd list --depth 1` in `apps/ui`;
   - `pnpm.cmd why react`, `pnpm.cmd why @wertkit/ui`;
   - the ten Radix entries in `pnpm-lock.yaml`;
   - no `^0.1.8`, `^19.3.1`, or `^0.28.2` in `apps/ui/package.json`.
4. Commit `package.json`, `pnpm-lock.yaml`, source, tests, and plan changes in
   one local commit. Do not push.
5. `pnpm.cmd install --frozen-lockfile` must be a no-op after the commit.
6. CI: the `browser-check` job currently runs `pnpm --dir apps/web/host build`
   with **no install and no ui/ui-kit build**. Add, before the build:
   - `pnpm install --frozen-lockfile`;
   - `pnpm --filter @selis/ui-kit build`;
   - `pnpm --filter @selis/ui build`;
   - `pnpm --filter @selis/web-host build`.

   The build order matters: `place-assets` fails loudly when a dist root is
   missing, which is the correct failure. This is a pre-existing gap, but stage
   3's browser check cannot be honest without it.

## 9. Validation plan

All commands use `pnpm.cmd`.

| Gate | Command | Expected |
| --- | --- | --- |
| Install from lock | `pnpm.cmd install --frozen-lockfile` | clean, no lock drift |
| Dep tree | `pnpm.cmd -C apps/ui list --depth 1` | exact React/ReactDOM/wertkit/esbuild |
| ui-kit | `pnpm.cmd -C packages/ui-kit test` | bridge/export tests pass |
| UI types | `pnpm.cmd -C apps/ui typecheck` | passes with JSX |
| UI tests | `pnpm.cmd -C apps/ui test` | existing 206+ tests, new bridge/server-render tests |
| UI build | `pnpm.cmd -C apps/ui build` | tsc + vendor bundle + CSS copy |
| Host | `pnpm.cmd -C apps/web/host test` and `typecheck` | unchanged plus manifest/vendor assertions |
| Host build | `pnpm.cmd -C apps/web/host build` | place-assets emits chrome/vendor/CSS |
| Extension unaffected | `pnpm.cmd -C apps/extension build` | size/bundle gates still pass; root `@selis/ui` still has no React |
| Browser | `cargo xtask browser-check --strict` | 4/4 in Edge, including chrome |
| Rust harness tests | `cargo test -p xtask` | 173+ pass, real Edge check included |
| Lint | `pnpm.cmd lint` | clean |
| Licence evidence | `pnpm.cmd licenses list --json` | only ADR-P0021 allow-list + `@wertkit/*` |

Also verify by inspection:

- `apps/ui/dist/index.js` and its viewer closure contain no `react` or
  `@wertkit` import;
- `public/assets/ui/chrome/index.js` has only relative imports after the
  rewrite;
- `public/assets/ui/vendor/vendor.js` is a closed ESM file;
- no `public/` build output is staged in git (it is ignored);
- the high-contrast screen still meets UI.10's 7:1 assertions.

## 10. Record corrections to make with stage 3

- ADR-P0021's stage-2 revision said "nine `@radix-ui/*` packages"; the package
  has **ten**. Corrected in the stage 3 documentation change.
- The plan says "DropdownMenu"; the package exports **`Menu`**. Correct the
  component name.
- The plan's WEB.02 note says "no bundler" and the host's tooling enforces it.
  This proposal asks for a **vendor-only** exception, recorded as such; it does
  not claim the app is still bundle-free.

## 11. Approval

Approve the whole proposal, or reject the bundler exception and leave stage 3
open. No manifest, lockfile, or source change is made until that answer is
recorded.
