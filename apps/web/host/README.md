# `@selis/web-host` — the web deployment shell (WEB.01, WEB.02, WEB.03)

Static hosting with cross-origin isolation, an offline service worker whose
caching policy cannot persist a document, and local-only document handoff. The
one thing this app must never do is upload a document.

## Offline (SL-4.WEB.02)

| Module | Role |
|---|---|
| `sw-policy.ts` | The caching policy as pure data and functions: precache list, cache names, the request/response allow-lists, the budgets. No DOM, no `caches`, no `fetch`. |
| `sw.ts` | The worker. `install` precaches the shell, `activate` purges our own stale cache names and claims clients, `fetch` classifies then answers. Owns the only two cache-write call sites in the package. |
| `sw-register.ts` | Registration, scope `/`, the refusal conditions, and the update handoff. |
| `tools/place-sw.mjs` | Copies the compiled `sw.js` + `sw-register.js` into `public/` (the deploy root) after `tsc`, refusing any bare or remote specifier. |
| `public/assets/boot.js` | Registers the worker on load and records the outcome on `globalThis.__selisServiceWorker` (registered, or which refusal applied). |

### What is cached, and what is not

**Precached (install):** `/`, `/index.html`, `/assets/style.css`,
`/assets/boot.js` — the boot minimum, and nothing else. `addAll` is atomic, so
one missing asset fails the install and the browser keeps the previous worker
rather than activating a half-cached app.

**Runtime-cached (first use, cache-first):** `/assets/…` (the built UI and its
code-split chunks) and `/wasm/…` (the WASM.02 chunks the manifest points at).
The core module is up to 3 MB brotli (`03-CONVENTIONS.md` §12), so it is
fetched once, on demand, rather than installed for every visitor — the same
reasoning as EXT.05, which keeps CJK fonts an optional download rather than a
bundled asset. Caps: 12 MB per entry (4× the brotli core budget, because Cache
Storage keeps the *decoded* body) and 32 entries, oldest evicted first.

**Never cached:** anything that is not a bodyless same-origin `GET` with no
`Range` header, no query string, and an allow-listed path; and any response
whose content type is not an allow-listed asset type. `application/pdf` and
`application/octet-stream` are absent from that list, so a document is refused
by URL *and* by content type.

### Why a document cannot be cached

Structurally, in three layers rather than by intent:

1. **Request.** `?src=` is refused three times over: it is cross-origin (or
   same-origin with a `Range` header, which is how WEB.03 pulls bytes), and it
   carries a query string. A path that is not on the fixed allow-list is
   refused by default, and `..`/`%2e%2e` cannot climb into one.
2. **Response.** Even from an allow-listed path a document is refused: the
   content type is checked against an allow-list, and so are
   `Content-Disposition: attachment`, `Cache-Control: no-store`, `Vary: *`,
   opaque bodies, and any non-200.
3. **Call sites.** There are exactly two writes to a cache in the whole
   package — `install` (the fixed list) and the runtime branch — and a test
   counts them in the sources, so a third `put`/`addAll` fails the build.

The suite is verified falsifiable: planting a policy that drops the `Range`
rule, widens the path allow-list to everything, and makes `application/pdf`
storable makes the real `?src=` intake test fail.

### The cache does not weaken the no-upload gate

The page's wrapper runs **before** the browser dispatches a request; the
worker's `fetch` event fires **after** that dispatch. The worker is downstream
of the gate, not upstream of it, so a cached response changes what comes back
and cannot change what the page already recorded going out. `sw.test.ts` drives
the real `installRequestRecorder` with the worker in the path and asserts every
request is still seen, still bodyless, and still passes `assertNoUpload` — with
a planted `POST` of the same bytes as the negative control.

The one thing the page's gate cannot see is a request the *worker* originates
on its own initiative. There are none: a test asserts the worker's own fetch log
during a document session is empty, and that the install's own fetches are the
shell, all bodyless `GET`s.

### COOP/COEP and updates

Cached navigations are returned as the **cached `Response` object**, never
rebuilt, so WEB.01's `COOP: same-origin` + `COEP: require-corp` survive and the
threaded engine path (WASM.03) still works offline. The only synthesised
response is the offline 503, which sets the isolation headers itself. Runtime
entries are keyed by `Request` rather than by URL so the Cache API honours
`Vary` — a brotli-served asset varies on `Accept-Encoding`, and a URL-only key
would replay compressed bytes to a client that cannot decode them.

Updates: `updateViaCache: "none"`, and the worker does **not** call
`skipWaiting` on install. A replacement waits until the page posts
`selis:skip-waiting` (`applyUpdate`), so the engine is never swapped underneath
an open document. `CACHE_VERSION` namespaces the caches; `activate` deletes our
own previous names and nothing else.

### Not proven in a browser

A service worker cannot register under `file://`, and this package ships no
browser harness (ADR-P0021 keeps the JS tree dependency-free), so the worker is
driven in Node with real `Request`/`Response` objects and a Cache Storage double
that implements `addAll` atomicity, `Vary`-aware `match`, and insertion-ordered
`keys`. **Not** covered here, and to be confirmed at deploy or in a real
browser: real `install`/`activate` event delivery, `clients.claim()`,
browser-enforced `respondWith` semantics, real Cache Storage quota behaviour,
and the DoD's manual airplane-mode pass (open a local PDF, view, search,
print). The logic those tests assert is in this package; the browser's
adherence to it is not.

## Entry points (all local)

| Entry | Module | Behaviour |
|---|---|---|
| Drag-drop | `handoff.ts` `filesFromDrop` | Filters to `.pdf` under the 64 MiB bound (mirrors the Worker's `MAX_INPUT_BYTES`), caps at 32 files. No bytes are read on the UI thread. |
| File picker | `filesFromPicker` | Same filter as drop — every entry point enforces the same bound. |
| Paste | `filesFromPaste` | Same filter over `clipboardData.files`. |
| `?src=` URL | `parseSrcParam` | `http:`/`https:` only, resolved to `{ kind: "http-range" }` — the Worker's range fetcher streams bytes into the *local* engine (WASM.06 driver). `javascript:`/`data:`/`file:`/`blob:`/`opfs:` rejected. Same-origin preferred, CORS gated at fetch time. |

`handoff-flow.ts` is the other half: `planIntake`/`runIntake` take an
`Intake` (`drop` / `picker` / `paste` / `url`) and carry it to a Worker open.

- drop, pick, and paste resolve to `{ kind: "blob", sourceId }`. The bytes
  never move: the Worker reads the `Blob` with `FileReaderSync` (WASM.05
  `BlobSource`), so `downloaded` is 0 and the path issues no request at all.
- `?src=` pulls the document in with `GET <url>` + a `Range` header and **no
  body**, into a `RangeSink` (local memory by default, `collectingSink()`).
  An origin that ignores `Range` and sends the whole document is handled; one
  past `maxRangeBytes` (default the same 64 MiB) is refused rather than
  silently truncated; a dead or refusing origin is reported, never retried
  against a server-side renderer (ADR-P0016).

Accepted files map to `{ kind: "blob" }` via `sourceForFile` — the Worker
glue (`worker-glue.ts`) registers the `Blob` with the Worker
(`FileReaderSync` in the Worker, never the main thread) and dispatches the
WASM.01 `open` (`v:1`, `id`, `op:"open"`, kebab-case `src`, lowercase
`budget.surface` — byte-identical to `protocol.rs`).

OPFS (`{ kind: "opfs", path }`) and File System Access (`{ kind: "fsa",
handleId }`) follow the same register-then-open shape; the Rust adapters
(`selis-io` `OpfsSource`/`FsaSource`/`BlobSource`, SL-4.WASM.05) drain
through `DocSource::read_at` so the conformance suite is the path the
engine takes.

## The no-upload guarantee

`no-upload.ts` is the gate, and it works on requests the app really issued:
`installRequestRecorder()` wraps the live `fetch` **and**
`XMLHttpRequest.prototype.send`, so no request can route around it.
`assertNoUpload(requests, documents)` then throws when a request carries a
window of an opened document's bytes — in the body *or* in the URL, on any
method (`fetch` refuses a body on GET/HEAD, `send` does not), and it fails
closed on a body it cannot read as bytes (a `ReadableStream`).

The Vitest suite is the CI gate:

- `handoff-flow.test.ts` drives all four paths through the recorder and
  asserts that every request they issue is a bodyless `GET`, that the run
  really did reach the network (so "no upload" is an observation, not an
  absence of evidence), and that `assertNoUpload` passes.
- The same file's negative controls upload the same bytes on purpose — a
  `POST` of the raw document and a `FormData` multipart of it — and require
  the gate to reject them. Without those legs a recorder that recorded
  nothing would pass everything else.
- `no-upload.test.ts` covers the matcher itself: prefix and mid-document
  slices, base64 in the query string, a `GET` with a body, every body shape
  `inspectBody` can and cannot read, and both intercepted transports.

Range GETs (downloads into the local engine) and opt-in telemetry pings
(no document fields by type, ADR-P0017) pass; any request carrying PDF bytes —
whole, sliced, or multipart — fails.

## For transport authors

1. Implement the DOM adapters (event listeners) thinly over `handoff-flow.ts`
   — no filtering logic in the listeners.
2. Map to the wire with `worker-glue.ts`; never hand raw bytes the UI parsed.
3. Run `assertNoUpload` over the recorded requests in every handoff test.