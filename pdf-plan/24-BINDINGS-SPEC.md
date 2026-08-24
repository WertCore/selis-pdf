# 24 — Bindings — Normative Specification

One engine, four bindings. This document defines each boundary so the shells cannot drift.

---

## 1. Principles common to every binding

1. **Handles, not structures.** Foreign code holds opaque handles. No engine type is ever laid out
   in foreign memory.
2. **Errors are codes.** Every failure carries the numeric code from `selis-error`'s registry
   (`03-CONVENTIONS.md §3`) plus an optional detail string. Bindings do not invent error taxonomies.
3. **No panic crosses a boundary.** `catch_unwind` at every entry point (`SL-0.ERR.03`).
4. **Cancellation is explicit and always available.** Every long operation takes a cancel handle;
   cancelling is observable within one budget tick.
5. **Progress is a callback, never polling.**
6. **No hidden allocation across the boundary.** The caller owns output buffers wherever the size
   is knowable in advance.
7. **Every binding is generated or tested from the same conformance suite**, so a behavioural
   difference between the Swift and the Node wrapper is a build failure, not a support ticket.

---

## 2. WASM binding (`selis-pdf-wasm`)

### Threading model
The engine lives in a Worker. The main thread holds handles and never parses or renders.
Render workers are additional Workers sharing memory via `SharedArrayBuffer` where COOP/COEP allow
it, and a single Worker otherwise (ADR-P0004, `SL-4.WASM.03`). Output must be identical either way.

### Message protocol

```ts
type Request =
  | { id: number; op: "open";    src: SourceDescriptor; budget: BudgetProfile }
  | { id: number; op: "close";   doc: DocHandle }
  | { id: number; op: "render";  doc: DocHandle; page: number; matrix: Matrix;
      params: RenderParams; into?: OffscreenCanvas }
  | { id: number; op: "text";    doc: DocHandle; page: number }
  | { id: number; op: "search";  doc: DocHandle; query: string; opts: SearchOpts }
  | { id: number; op: "mutate";  doc: DocHandle; mutation: MutationJson }
  | { id: number; op: "save";    doc: DocHandle; mode: "incremental" | "rewrite" }
  | { id: number; op: "cancel";  target: number }

type Response =
  | { id: number; ok: true;  value: unknown; transfer?: Transferable[] }
  | { id: number; ok: false; code: number; message: string; docState: DocState }
  | { id: number; progress: number; stage: string }
```

Rules:
- `id` correlates request and response; `cancel` targets an in-flight `id`.
- Rendered tiles are transferred, never copied — `ImageBitmap` or a transferred `ArrayBuffer`.
- `SourceDescriptor` names an adapter (`blob | opfs | fsa | http-range | bytes`); the Worker
  constructs the `DocSource`. The main thread never hands over raw document bytes it has parsed.
- Every response carries `docState` on failure, so the UI always knows whether the user's work
  survived.

### Code splitting
Chunks: `core` (COS + render + text), `editor`, `ocr`, `convert`, `jpx`, `fonts-cjk`.
Loaded on demand, cached in the Cache API / extension storage. `size-check` budgets per chunk
(`SL-0.WS.09`). A viewer session must never download `editor`.

### Memory
Explicit `close` releases document memory. A memory-pressure hook evicts caches before the browser
kills the tab. The wasm32 4 GB ceiling is a documented, tested boundary: a document that would
exceed it fails with `BudgetExceeded`, not a crash.

---

## 3. C ABI (`selis-pdf-ffi`)

```c
/* Handles */
typedef struct selis_pdf_engine  selis_pdf_engine;
typedef struct selis_pdf_doc     selis_pdf_doc;
typedef struct selis_pdf_page    selis_pdf_page;
typedef struct selis_pdf_cancel  selis_pdf_cancel;

/* Every fallible call returns int32_t: 0 = success, else the registry code. */
typedef int32_t selis_pdf_status;

/* Errors: the code is the return value; details are fetched for the calling thread. */
const char* selis_pdf_last_error_message(void);   /* thread-local, valid until the next call */
int32_t     selis_pdf_error_doc_state(int32_t code);

/* Sources are supplied by the host through a vtable — ADR-P0005 all the way out to C. */
typedef struct {
    void*   ctx;
    int64_t (*len)(void* ctx);                                  /* -1 = unknown */
    int32_t (*read_at)(void* ctx, uint64_t off, uint8_t* buf, size_t n, size_t* filled);
    void    (*release)(void* ctx);
} selis_pdf_source;

selis_pdf_status selis_pdf_engine_create(const selis_pdf_config* cfg, selis_pdf_engine** out);
selis_pdf_status selis_pdf_doc_open(selis_pdf_engine*, const selis_pdf_source*, const selis_pdf_budget*, selis_pdf_doc** out);
selis_pdf_status selis_pdf_doc_page_count(selis_pdf_doc*, uint32_t* out);
selis_pdf_status selis_pdf_page_render(selis_pdf_page*, const selis_pdf_matrix*, const selis_pdf_render_params*,
                           uint8_t* dst, size_t dst_len, size_t dst_stride, selis_pdf_cancel*);
selis_pdf_status selis_pdf_doc_save_incremental(selis_pdf_doc*, const selis_pdf_sink*, selis_pdf_cancel*);
void       selis_pdf_doc_close(selis_pdf_doc*);
void       selis_pdf_engine_destroy(selis_pdf_engine*);

/* Progress */
typedef void (*selis_pdf_progress_fn)(void* user, float fraction, const char* stage);
selis_pdf_status selis_pdf_set_progress(selis_pdf_engine*, selis_pdf_progress_fn, void* user);
```

Rules:
- `selis_pdf_page_render` writes into a caller-owned buffer — this is what makes zero-copy tile delivery
  on mobile possible (`SL-7.FFI.02`).
- Handles are not thread-safe individually; the engine is. Document the model explicitly: one
  `selis_pdf_doc` may be used from one thread at a time; distinct docs may be used concurrently.
- `selis_pdf.h` is generated (`cbindgen`) and committed, so a diff in the header is visible in review.
- ABI compatibility is checked in CI against the previous release (`SL-9.SDK.01`).

---

## 4. Language wrappers

Generated from one IDL (`bindings/selis.idl`) so they cannot drift:

| Language | Mechanism | Idioms required |
|---|---|---|
| Swift | XCFramework over the C ABI | Native types, `async` wrappers, `Sendable`, `Error` conformance |
| Kotlin | JNI + AAR | Coroutines, `AutoCloseable`, lifecycle-safe handles |
| Python | PyO3 or cffi | Context managers, exceptions, `bytes`/`memoryview` |
| Node | N-API | Promises, `Buffer`, `AbortSignal` → cancel |
| Go | cgo | `context.Context` → cancel, `io.Reader` → source |
| Java | JNI or Project Panama FFM | `AutoCloseable`, checked exceptions |
| .NET | P/Invoke | `IDisposable`, `CancellationToken` |

The same conformance suite runs against every wrapper. A wrapper that cannot pass it is not
released, and a behavioural difference is a build failure.

---

## 5. Extension messaging

```text
content script ──(minimal: just the navigation signal)──▶ service worker
service worker ──(document handle, never bytes)────────▶ offscreen document
offscreen document ── hosts the WASM engine, owns all document data
viewer page ──(the §2 Worker protocol)─────────────────▶ offscreen document
```

Rules:
- Document bytes never transit the service worker (it is killed unpredictably and is the most
  exposed surface).
- The offscreen document owns engine state and must recover from service-worker termination
  (`SL-4.EXT.03`).
- No message ever carries a document to a non-extension origin.
- The "edit this in the web app" handoff (`SL-4.EXT.08`) passes an OPFS handle or a same-origin
  transfer — never an upload, and a test asserts it.

---

## 6. Service API (`selis-pdf-service`)

- REST + gRPC over the same operation set as the CLI, so a capability exists once and is exposed
  three ways (CLI, SDK, API) with no divergence.
- Per-tenant budgets are mandatory parameters, not defaults.
- Documents are processed in isolated workers with ephemeral storage and a stated retention.
- Idempotency keys on every mutating operation.
- The API returns the same numeric error codes as every other binding.
