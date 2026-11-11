# 43 — The theme, the export, and where the core code lives

**Status:** shipped. **Owner:** AI+ · **Companion to:** `41-EDITOR-ROADMAP.md`,
`42-EDITOR-UI-REDESIGN.md`.

**Why this file exists:** three questions came up together — how the reader
switches between the light and the dark theme, how a document is exported to
another format, and where code that is not UI belongs. This records the
answers, because the answers are decisions rather than features, and the next
change to any of them should be made against a reason.

---

## 1. The theme

### 1.1 Why the dark theme did not work, and what it took

The design system's tokens have carried a dark theme since the kit was
written (`packages/ui-kit/src/tokens/colour.ts` → `tokens.css`'s
`[data-theme="dark"]` block). It never took effect, and the reason was a
selector:

```css
/* assets-src/viewer-shell.css, before */
:root {
	--selis-color-bg: #f4f1e8;   /* the site's warm palette */
	...
}
```

`:root` and `[data-theme="dark"]` have the **same specificity**, and
`viewer-shell.css` is concatenated **last** into the assembled
`style.css`. So the light remap won, whatever the reader chose, on every
surface.

The fix was the selector, not the palette:

```css
:root:not([data-theme="dark"]) { /* the light palette */ }
:root[data-theme="dark"]       { /* the dark palette  */ }
```

Both outrank the kit's own `:root`, so the site's palette is what applies in
either theme — and the kit's dark theme is free for anything the remap does
not name (the success and warning roles, the shadows).

**The dark palette is the same product at night.** The paper goes dark and
warm rather than blue-black, so it still belongs to the same design; the
green is lightened from `#0b6b5e` to `#3fbfa6`, because the deep green of the
light theme has nothing to read against on a dark ground; and the page of a
PDF stays white in both, because paper is paper.

### 1.2 Where the logic lives

The decisions — which theme, given a stored choice and a system's
preference — are in **`@selis/ui-kit`**, in `preferences.ts`, because they
are the same decisions for every surface:

| | |
|---|---|
| The core | `packages/ui-kit/src/preferences.ts` — the storage interface, the system's answer, the default, the toggles. DOM-free and platform-free. |
| The web | `apps/web/host/src/theme.ts` — `localStorage`, `matchMedia`, the root element. |
| The entry | `apps/web/host/src/theme-boot.ts` — the module a page loads to apply the theme. |

The kit is the design system, and the design system is the piece every
surface shares: the web app, a desktop app, an SDK embedding the kit. A
decision about the theme that lived in the web host's TypeScript would be a
decision the other two would have to write again.

### 1.3 The rule about the default

A reader who has **chosen** keeps their choice. A reader who has **not
chosen** gets the operating system's answer, and keeps getting it — the
system's colour scheme is watched, and the page follows it as it changes at
sunset. But the watch is only installed while the reader has not chosen, so
a reader who has picked a theme is not overridden by their laptop.

The choice is remembered in `localStorage` under `selis-theme`, so it follows
the reader across the site and across sessions.

### 1.4 The switch

Every surface carries the same button, with the same id
(`selis-theme-toggle`), so one piece of wiring serves all of them:

- the **editor**'s app bar;
- the **tools grid**'s navigation (and every card page's header);
- the **viewer's** bar.

The **glyph** is chosen by the **stylesheet** — both glyphs are in the
markup, and the dark theme's is hidden in the light one — so the switch is
right before any script has run. Only the label is set by the script.

## 2. The export

### 2.1 The engine has always been able to do this

This is the finding that matters: **the writers were already there.** The
roadmap had "PDF to Word / Excel / PowerPoint" marked **BLOCKED — no office
writer**, and that was wrong:

| | |
|---|---|
| `crates/selis-pdf-text/src/docx.rs` | PDF → a Word document |
| `crates/selis-pdf-text/src/pptx.rs` | PDF → a PowerPoint deck |
| `crates/selis-pdf-text/src/xlsx.rs` | PDF → an Excel workbook |
| `crates/selis-pdf-text/src/html.rs` | PDF → a positioned HTML page |
| `crates/selis-pdf-text/src/export.rs` | PDF → text, Markdown, JSON |
| `crates/selis-pdf-text/src/epub.rs` | PDF → an EPUB book |

and the lazy **text chunk** (`crates/selis-pdf-wasm-text`) put them on the
wire as the `word`, `slides`, `workbook`, `epub`, `reflow`, `tables` and
`text` operations, with a separate **HTML chunk** for the positioned web
page. The tools page has had a card for them all along.

So the export in the editor was never an engine problem. It was a **UI
problem**, and the fix is what the roadmap said it would be: a panel.

### 2.2 The shared layer

The HTML writer's client was written inline in the tools page, which made it
the one export the editor could not have — a client buried in another page's
function body is not a client. So:

| | |
|---|---|
| `apps/web/host/src/tools/html-protocol.ts` | The HTML chunk's client, extracted. Both surfaces call it. |
| `apps/web/host/src/tools/export.ts` | Which chunk answers which format, what each format is called, and what the caller has to supply. |

The editor's **Export** tab offers eight formats, and the export is made from
the document **as it is** — with every change the reader has made — and
written here, on this device.

A deck carries each page as its own picture (the editor renders the pages it
already has), so the deck looks like the document rather than like a list of
text boxes.

### 2.3 The download, and the bug that made it not happen

The export produced the file and the page said so, and **the file never
arrived**. The cause was the object URL:

```ts
const url = URL.createObjectURL(blob);
link.href = url;
link.click();
setTimeout(() => URL.revokeObjectURL(url), 0);   // <-- the bug
```

Chrome resolves the blob on a **background thread**, and the download request
is created a task or two after the click. A URL revoked on the next turn is
already dead by the time the browser reads it, and the download fails
**silently**: the page says "Wrote invoice.docx", the browser says nothing,
and the reader has no file and no error. The same pattern was in the PDF
download, so the editor's own "Download" button was broken the same way.

The fix is a lifetime, not a delay:

- the object URL is **released after a grace period** (thirty seconds), not
  in the next turn — long enough for the background read, short enough not
  to hold anything;
- the link stays in the document until the next turn, because a link removed
  in the same turn as its click can be missed by the browser;
- every pending URL is released when the page goes away (`pagehide`).

And the export is offered **twice on purpose**: the button starts the
download, and the inspector's "Last export" panel keeps a link to the file,
with its own URL, that lives until the next export replaces it. A browser that
refuses an automatic download still gives the reader the file.

### 2.4 The icons

Each export format carries its own mark — a Word document is a page with a
"W" on it, a deck is a presentation board, a workbook is a grid, a web page is
a pair of angle brackets, an EPUB is an open book, and so on. The reader
should be able to tell the formats apart without reading the labels, and the
marks are drawn on the same 24-unit grid with the same stroke weight as the
rest of the set, so the ribbon reads as one row of tools.

### 2.5 What the check proves

The browser leg reads the produced files rather than trusting them:

- a **Word document** is a ZIP container, so its bytes start with `PK`;
- the **HTML** starts with `<!do`, and carries the document's text;
- the **Bates prefix** the reader typed is in the produced PDF.

And one more, which is the quiet bug above: the export's file **URL still
resolves** after the export. That is the property that was broken — the URL
was revoked in the same turn as the click — and a check that reads the
produced file's URL back is the only thing in this codebase that could have
caught it.

## 3. Where the core code lives

This is the question that shaped all of the above, and the answer is a
layering rather than a place:

| Layer | What lives there | Reached by |
|---|---|---|
| **The PDF core** (`crates/selis-pdf-*`) | The writers, the rewriters, the readers, the arithmetic. Every byte a document is made of. | The WASM chunks — the same bytes in the web app, a desktop app, and an SDK. |
| **The design system** (`packages/ui-kit`) | The tokens, the themes, and the decisions about a preference set. | `@selis/ui-kit` — the web app, a desktop app, an SDK. |
| **The web layer** (`apps/web/host/src`) | The wire: the chunk protocols, the export's names, the DOM. | The web app. |

The rule that follows: **a decision that is not about the DOM belongs
upstairs.** The export's *writers* are Rust and were always going to be; the
export's *names* are the web layer's, because a desktop app names its own
buttons. The theme's *decisions* are the kit's, because every surface makes
them; the theme's *storage* is the web's.

And the thing that is easiest to get wrong: **a second implementation of the
engine's arithmetic.** The placement geometry, the stamp's width table and
the redaction's text matrix all come from the engine. A second copy is a
second set of bugs, and it is the one thing in the editor that is silently
wrong rather than loudly broken.

---

## 4. The evidence

- `packages/ui-kit/src/preferences.test.ts` — the preference decisions, with
  the storage and the system's answer supplied as arguments.
- `apps/web/host/src/tools/export.test.ts` — the format list and the mapping
  from a format to the chunk that writes it.
- `apps/web/host/src/editor/operations.test.ts` — the new operation builders.
- The editor's browser leg grew three legs: `theme` (the switch changed the
  page and was remembered), `export` (a Word document is a ZIP, the HTML
  carries the text) and `bates` (the prefix is in the file).
