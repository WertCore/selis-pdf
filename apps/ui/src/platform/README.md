# `PlatformAdapter` — the host seam (SL-4.UI.01)

The one seam between the Selis UI and whatever hosts it: the web app
(`apps/web/host`, SL-4.WEB.01+), the MV3 extension (SL-4.EXT.06), the desktop
shell (SL-6.SHELL.01), and the CLI/test harness. UI code in `apps/ui` receives
an adapter at startup and touches **no platform globals** — a lint-style test
in this directory fails the build if `window`, `document`, `fetch`,
`localStorage`, `chrome`, etc. appear in production code, so the same bundle
runs behind every host (ADR-P0022).

## The three seams (keep them distinct)

| Seam | Where | Owns |
|---|---|---|
| **Host** | `PlatformAdapter` ports (`files`, `storage`, `clipboard`, `print`, `telemetry`, `window`) | What the OS/browser can do: pickers, KV settings, clipboard, print trigger, titles, deep links. |
| **Engine transport** | `PlatformAdapter.engine` (`EnginePort`) | open / renderTile / extractText / search / close against the document engine. Transport-agnostic: the WASM.01 Worker protocol implements this behind a worker transport; Tauri commands do the same natively; the mock runs in-process. |
| **Logic** | `selis-viewmodel` (ADR-P0035) | App state — current page, zoom, tool, dirty state — published as diffs. Never here. |

## Conventions

- **Async everywhere, cancellation always.** Long operations take
  `AdapterRequestOptions { signal?: AbortSignal; onProgress?; budget? }`.
  Cancelling surfaces `AdapterError` code **4020 `CANCELLED`** with
  `docState: "Unchanged"`.
- **Errors are registry codes, never invented** (24-BINDINGS-SPEC §1). Every
  rejection is an `AdapterError { code, docState, retryable }`; `docState`
  answers *"what happened to the user's document?"* (03-CONVENTIONS §3).
  Codes used here are registered in `crates/selis-error/codes.toml`
  (4020, 4000–4004, 5000, 6000, 6001); if you need a new one, register it
  there first.
- **Handles, not structures.** `DocHandle` is opaque beyond `pageCount` /
  `pageSizes`; tiles arrive as caller-owned RGBA8 buffers.
- **No phone-home.** Telemetry is opt-in and `TelemetryEvent` has no field
  that could carry document bytes, text, names, or URLs (ADR-P0017). Sources
  stream into the local engine; nothing is uploaded (ADR-P0016).

## Document sources

`DocumentSourceDescriptor` names where bytes come from: `bytes`, `blob`
(picked file), `opfs`, `fsa` (File System Access handle), `http-range`
(remote *viewing* over range requests — still local processing), `file`
(native path). The names mirror the bindings spec's adapter set; the shapes
here are **not** the WASM.01 wire format — transports map them.

## For UI.02–UI.13

- Get the adapter injected (shells compose it; `apps/ui` never imports a
  concrete adapter).
- Gate affordances on `adapter.capabilities` — never on host sniffing.
- Persist settings (theme/density from `@selis/ui-kit`) via
  `adapter.storage`, not `localStorage`.
- Route every engine call through `adapter.engine`; UI.02 (page list) calls
  `renderTile` with cancellation per tile; UI.05 consumes `search`'s
  progressive batches; UI.08 wires `print`; UI.12 renders `AdapterError`s
  using `code` + `docState` + the ui-kit error-state styles.

## For transport authors (WASM.01, extension, desktop)

1. Implement `PlatformAdapter`; reuse the in-process `MockAdapter` behaviour
   as the reference semantics.
2. Run the shared contract suite — `definePlatformAdapterContract(name,
   fixture)` in `contract.ts` — against your transport. It asserts open/
   render/text/search round-trips, handle lifetime, page bounds,
   cancellation (`4020`/`Unchanged`), budget (`BUDGET_PIXELS`), storage,
   clipboard, and telemetry-off-by-default. A behavioural difference from
   other transports is a build failure.
3. Map `DocumentSourceDescriptor` to your wire `SourceDescriptor` inside the
   transport, not in the UI.

## The mock

`createMockAdapter()` (`mock-adapter.ts`) implements the full surface
in-process: declarative `MockDocumentSpec`s instead of real parsing,
deterministic microtask scheduling (no timers — cancellation is observable at
await points), recording of every host interaction (`recording.telemetryEvents`,
`printedDocs`, `deepLinks`, …), and queueing for `pickOpen`/`pickSave`. It is
the reference implementation for the contract suite and the harness for
UI.02+ development before transports exist.
