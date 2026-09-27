# `@selis/web-host` — the web deployment shell (SL-4.WEB.03)

Local-only document handoff over the WASM.01 Worker protocol. The one thing
this app must never do is upload a document.

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
