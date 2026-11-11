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

The engine has always been able to write a PDF's text out as a Word
document, a PowerPoint deck, an Excel workbook, an EPUB book, a CSV of its
tables, a reflowed article, plain text, or a positioned HTML page. The
writers are in `crates/selis-pdf-text` (`docx.rs`, `pptx.rs`, `xlsx.rs`,
`epub.rs`, `html.rs`, `export.rs`) and the lazy text chunk puts them on the
wire. The tools page has had a card for them all along.

**The roadmap said "PDF to Word / Excel / PowerPoint: BLOCKED — no office
writer". That was wrong**, and finding out why is the point of this section:
the export was never an engine problem. It was a UI problem, and the fix is
what the roadmap said it would be — a panel.

| | |
|---|---|
| `apps/web/host/src/tools/html-protocol.ts` | The HTML chunk's client, extracted from the tools page so both surfaces call it. |
| `apps/web/host/src/tools/export.ts` | Which chunk writes which format, what each format is called, and what the caller must supply. |

### 2.1 What was wrong, in the order the reports found it

### 2.2 The object URL was revoked in the same turn as the click

```ts
const url = URL.createObjectURL(blob);
link.href = url;
link.click();
setTimeout(() => URL.revokeObjectURL(url), 0);   // the bug
```

Chrome resolves the blob on a **background thread**, and the download
request is created a task or two after the click. A URL revoked on the next
turn can be dead by the time the browser reads it — and the download then
fails **silently**: the page says "Wrote invoice.docx", the browser says
nothing, and the reader has no file and no error.

The fix is a lifetime, not a delay: the URL is released after a **grace
period**, the link stays in the document until the next turn, and every
pending URL is released when the page goes away.

### 2.3 The reader had no handle on the file

A download lands in a folder the reader has to go and look in. The
inspector gained a **"Last export"** panel: the file's name, its size, and a
link to take it again.

### 2.4 The file's LENGTH and the file's BYTES are both numbers

The exported file carried its length in a field called `bytes` and its bytes
in a field called `content`, and the write was handed the wrong one:

```ts
await writable.write(new Uint8Array(await file.bytes));
```

`file.bytes` is the file's LENGTH. `new Uint8Array(90135)` builds an array
of ninety-one thousand **zeros** — the right size, no content in it. The
fields are now named `size` and `content`, and the write takes the bytes
themselves so the mistake cannot compile.

### 2.5 A native dialog takes the page's activation with it

Chrome's Save dialog is a **native** dialog: it takes the focus away from
the page, and by the time the reader closes it the page is no longer "fully
active", so `createWritable()` is refused with

> The request is not allowed by the user agent or the platform in the current
> context.

Sometimes Chrome has already created the file at the path the reader chose,
which leaves a **0-byte file** there. So the File System Access route was
removed: it duplicates what the browser already does, less reliably.

### 2.6 The reason the reader never saw the fixes

**The service worker's runtime cache is CACHE-FIRST.** A module that has
been fetched once is served from the cache and the network is never
consulted again — that is what makes the offline story work. And the browser
only re-installs the worker when the **worker script's** bytes change; a
change to a module does not change them.

So a reader who loaded the editor once kept running **that day's**
`editor-page.js` for ever. Every fix went into a module their browser never
asked for again. The evidence was in the browser's own cache list:

```
selis-runtime-v5:16          ← sixteen modules, from a build long gone
```

Three fixes, and together they are the delivery system:

1. **The cache namespace follows the build.** `tools/place-sw.mjs` hashes the
   bytes the build serves — the host's compiled modules, the stylesheets, the
   WASM chunks — and stamps that hash into the worker script *and* into the
   policy module that decides the cache NAMES. The worker's bytes change, so
   the browser installs the new worker; the new worker's `activate` drops the
   caches whose names are stale; the reader's next load has the new modules.
2. **The page takes the update when it can afford it.** The new worker waits
   — it does not swap under a page mid-parse — so the page has to ask for
   it. `takeUpdateWhenSafe` does that, and asks only when a reload costs the
   reader nothing: the editor when no document is open, the tools page when
   no file is picked.
3. **The update is not missed.** A worker that installed *before* the
   watcher was attached is already waiting, and the `updatefound` event has
   been and gone by then. Without checking `registration.waiting` directly
   the update is missed and the reader keeps the old modules for ever.

After the change the same browser reads:

```
selis-runtime-v3c5fc9cfd175939d:13   ← the fresh modules, under the new namespace
```

### 2.7 The download was never the problem

The last report was the clearest one: the reader saw

> Wrote evidence.docx (6.1 KB). It is in your downloads …

and no file, and no download shelf at all.

The browser's own download list said:

```
evidence.docx | 6258 B | completed     ← eleven times
evidence.pdf | 829530 B | completed
```

**The download had completed every time.** The browser the reader was using
**intercepts downloads** and routes them into its own capture area — so there
is no shelf, no bubble, and nothing in a file manager. That is the
environment, not the editor.

**The lesson, which is worth more than any of the fixes:** a download that
completes is not the same as a download the reader can find; and a fix that
is not REACHABLE is not a fix.

The routes that always reach the disk are the browser's **own** dialogs:
right-click the link in the "Last export" panel and choose **Save as…**, or
open the link and save it. The panel now says so, and offers **Open** as
well as **Download again**.

### 2.8 The icons

Each export format carries its own mark — a Word document is a page with a
**W** on it, a deck is a presentation board, a workbook is a grid, a web page
is a pair of angle brackets, an EPUB is an open book, a reflowed article is a
document with lines, a table is a table, plain text is plain lines. The reader
should be able to tell the formats apart without reading the labels, and the
marks are drawn on the same 24-unit grid with the same stroke weight as the
rest of the set, so the ribbon reads as one row of tools rather than eight
labels.

## 3. The evidence

The browser leg's export leg checks the produced files rather than trusting
them:

- a **Word document** is a ZIP container, so its bytes start with `PK`;
- the **HTML** starts with `<!do`, and carries the document's text;
- the produced file's **URL still resolves** after the export — the
  property that was broken when it was revoked in the same turn as the click,
  and the one a revoked URL fails by downloading nothing.

