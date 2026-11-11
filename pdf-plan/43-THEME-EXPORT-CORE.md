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

### 2.4 What the second report found, and what it changed

The report said the file was still not there. The evidence said otherwise, and
it is worth being exact about it, because the difference matters:

- the browser's download list held a completed `electricity-bill.docx`,
  90 135 bytes;
- unzipping it showed a **valid Word package** — `[Content_Types].xml`,
  `_rels/.rels`, `word/document.xml` with 222 text runs;
- a plain blob download, a `data:` URL download and a visible link's click
  all completed the same way.

So the file was produced, it was handed to the browser, and the browser
finished with it. What the reader did not have was a **handle on it**: a
download lands in a folder they have to go and look in, and in an automated
browser it lands somewhere the reader never sees.

That is a design gap rather than a bug, and it is now closed:

- the export says **where the file went** — "Wrote electricity-bill.docx
  (90 KB). It is in your downloads — the panel on the right can put it
  somewhere you choose";
- the "Last export" panel offers **"Save as…"** as well as "Download again".
  That is the File System Access API: it opens a Save dialog and writes the
  bytes where the reader chooses, so the file ends up in a place they picked
  rather than in a folder they have to hunt for. It is not in every browser,
  so the button is only offered where it works, and the download is always
  there underneath it.

**The lesson worth keeping:** a download that completes is not the same as a
download the reader can find. The second one is the one that matters.


### 2.5 The empty file, and the field that caused it

The reader reported the download as **0 KB**, and that was the clue that
settled it. A file that is 0 KB is not a file that was never handed over: it
is a file that WAS handed over and was **empty**.

And it was empty because of a field name:

```ts
interface ExportedFile {
	readonly bytes: number;      // how many bytes the file IS
	readonly content: Uint8Array; // the bytes themselves
}
...
await writable.write(new Uint8Array(await file.bytes));
```

`file.bytes` is the file's **LENGTH**. `new Uint8Array(90135)` builds an
array of ninety-one thousand **zeros** — the right size, no content in it. The
reader got a file that Word opens and finds nothing in.

The fix is not a guard, it is a **type**: the write takes the bytes
themselves, and the length has its own name (`size`), so the two cannot be
confused again. The write lives in `src/editor/save-as.ts`, on its own,
because it is the one piece of the export that can be tested without a
browser — and it has to be tested, because the mistake it is prone to is
silent.

### 2.6 A second way to write an empty file

The `pagehide` handler released the downloads' object URLs when the page went
away — **including the URL the "Download again" link pointed at**. A link whose
URL has been revoked does not fail: the browser starts the download and
writes the empty result, so the reader got a **0 KB** file from clicking
"Download again".

The URL the reader can still click is now released only when the NEXT export
replaces it; the automatic download's URL is released by its own grace timer.
The browser drops the blobs with the document anyway.


### 2.7 The Save dialog is native, and that is why it failed

The reader's error was Chrome's:

> Failed to execute 'createWritable' on 'FileSystemFileHandle': The request is
> not allowed by the user agent or the platform in the current context.

The phrase "in the current context" is the clue. `createWritable()` needs the
page to be **fully active** — and the Save dialog is a **NATIVE** dialog: it
takes the focus away from the page, and by the time the reader has chosen a
folder and closed it, the page is no longer the active document. The write is
refused, and the browser's own wording for it says nothing a reader can act
on.

**A failure must never be a dead end.** The file is already produced at that
point, so the refused write now falls back to the ordinary download and says
so in words the reader can act on:

> Your browser would not write the file directly, so electricity-bill.docx is
> in your downloads instead.

The two cases are told apart as well: a reader who **cancels** the dialog is
not told they failed — they are told the file is still in their downloads.

And the panel says the thing that was missing all along: **the browser can
save the file wherever the reader wants, from its own menu.** Every browser
offers "Save as…" on a download and on a link; that route is always
available, needs no API, and cannot be refused the way a direct write can.

### 2.8 The icons

Each export format carries its own mark — a Word document is a page with a
"W" on it, a deck is a presentation board, a workbook is a grid, a web page is
a pair of angle brackets, an EPUB is an open book, and so on. The reader
should be able to tell the formats apart without reading the labels, and the
marks are drawn on the same 24-unit grid with the same stroke weight as the
rest of the set, so the ribbon reads as one row of tools.

### 2.9 What the check proves

The browser leg reads the produced files rather than trusting them:

- a **Word document** is a ZIP container, so its bytes start with `PK`;
- the **HTML** starts with `<!do`, and carries the document's text;
- the **Bates prefix** the reader typed is in the produced PDF.

And one more, which is the quiet bug above: the export's file **URL still
resolves** after the export. That is the property that was broken — the URL
was revoked in the same turn as the click — and a check that reads the
produced file's URL back is the only thing in this codebase that could have
caught it.

And two that came out of the reader's reports: the **Save dialog is handed
the file's own bytes** (a file written from its LENGTH is the right size and
completely empty), and a **refused write falls back to the download** rather
than dead-ending the reader with a browser error. The check drives both: it
stubs the Save dialog with a stand-in that records what it was given, and then
stubs it again with one that refuses, and asserts the file still reaches the
reader.

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
