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

`no-upload.ts` `assertNoUpload(requests, documents)` throws when any
non-GET/HEAD request body contains a document's 64-byte prefix. The Vitest
suite (`no-upload.test.ts`) is the CI gate: range GETs (downloads into the
local engine) and opt-in telemetry pings (no document fields by type,
ADR-P0017) pass; any POST/PUT carrying PDF bytes — whole or sliced — fails.

The shell entry point wires `createFetchRecorder()` around `fetch` in tests
and asserts the recording after every handoff flow. Telemetry stays opt-in;
`http-range` is local processing (ADR-P0016), never an upload.

## For transport authors

1. Implement the DOM adapters (event listeners) thinly over `handoff.ts` —
   no filtering logic in the listeners.
2. Map to the wire with `worker-glue.ts`; never hand raw bytes the UI parsed.
3. Run `assertNoUpload` over the recorded requests in every handoff test.
