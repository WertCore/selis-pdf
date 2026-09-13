# `@selis/ui-kit` — Selis design system (SL-4.UI.10)

Design tokens, theming, and base styles for the Selis viewer UI. Zero runtime
dependencies — hand-rolled CSS over custom properties (ADR-P0021: no component
library without a licence + size justification; this package ships none and is
a few KB of CSS).

## What UI.02+ gets

| Path | Contents |
|---|---|
| `css/tokens.css` | **All** custom properties: colour roles, spacing, radius, borders, type scale, leading, weights, font stacks, durations, easings, z-order, density sizes, shadows. Import once at the app root. |
| `css/base.css` | The base component styles the viewer builds on (`.selis-*` namespace): shell, toolbar, buttons, inputs, panels/lists, page tiles, text-layer selection, search highlights, badges, dialogs, scrollbars, reduced-motion discipline. |
| `src/` | The token data as typed TypeScript — the **source of truth** — plus WCAG contrast math and DOM-free theme helpers. |

## Using the tokens

Reference custom properties only. Never hard-code a value a token defines:

```css
.my-widget {
	padding: var(--selis-space-4);              /* 8px on the 2px grid */
	border-radius: var(--selis-radius-md);
	background: var(--selis-color-surface);
	color: var(--selis-color-text-secondary);
	box-shadow: var(--selis-shadow-1);
	transition: opacity var(--selis-duration-fast) var(--selis-ease-standard);
}
```

Roles, not raw colours: `--selis-color-text` not `#171b1f`. If a role you need
is missing, add it to `src/tokens/colour.ts` (it then exists in all four
themes and passes the contrast gate) rather than reaching for a sibling role
with different semantics.

## Theming

The CSS keys on data attributes you set on `<html>` (or the viewer root):

```ts
import { applyTheme, defaultPreferences } from "@selis/ui-kit";

applyTheme(
	{ ...defaultPreferences, theme: "dark", density: "compact" },
	document.documentElement,
);
```

| Preference | Values | Attribute |
|---|---|---|
| theme | `light` / `dark` | `data-theme` (light is the `:root` default) |
| contrast | `normal` / `high` | `data-contrast="high"` |
| density | `comfortable` / `compact` | `data-density="compact"` |
| motion | `system` / `reduced` | `data-motion="reduced"` |

Reduced motion also honours `prefers-reduced-motion: reduce` automatically;
`data-motion="reduced"` is the manual override. Every duration token collapses
to `0ms` in both cases, and the placeholder pulse is gated separately — so
components get motion discipline for free by using the duration tokens.

Persist the four preferences through the platform adapter's storage port, not
`localStorage` (SL-4.UI.01; the shell owns persistence).

## Tests you inherit

- **Token integrity** — every theme defines every role; values are `#rrggbb`;
  distinct roles stay distinct.
- **Contrast gate** — every declared fg/bg pair is checked with WCAG 2.x math
  in all four effective themes (4.5:1 body text, 3:1 non-text, 7:1 text in
  high contrast). `textDisabled` is the only exempt role. If your change
  trips the gate, pick a different value — do not lower the threshold.
- **CSS sync** — `css/tokens.css` must match the generator byte-for-byte.
  After editing token TS: `UPDATE_TOKENS_CSS=1 pnpm --filter @selis/ui-kit
  test`, review the diff, commit both.
- **Namespace discipline** — every class in `base.css` is `.selis-*`; every
  referenced variable exists in the generated set.

## Rules for UI.02+ components

1. New components live in the app (`apps/ui`), styled with `@selis/ui-kit`
   tokens; only genuinely shared primitives move into `base.css`.
2. Component classes continue the `.selis-` namespace and BEM-ish
   `selis-<block>__<element>--<modifier>` shape.
3. No runtime dependencies in this package without a new ADR (ADR-P0021).
4. No platform globals here either — this package is host-agnostic; the same
   lint rule that guards `apps/ui` guards its spirit.
