# selis-pdf-js — native budgeted form-script sandbox (SL-5.JS.01/02/03)

This crate is the **native** slice (CLI / desktop / mobile) of a two-environment
design. The browser slice lives in TypeScript under
`apps/web/host/src/editor/js-*.ts` and uses the browser's own engine inside a
sandboxed `Worker`. Active content is off by default (ADR-P0020): a script runs
only when the reader consents for that document or an admin origin policy
pre-approves it.

## Engine choice (native)

`rquickjs` `=0.11.0` (MIT), wrapping QuickJS (MIT). Both are permissively
licensed and maintained; QuickJS is a small (~200 KB of C), deterministic engine
with a C API that has *no* filesystem, network, process or module-loader
capability. `boa` was rejected as substantially larger; GPL-licensed engines are
ruled out by ADR-P0021. `0.11.0` is the last release with `rust-version = 1.85`,
the workspace MSRV; the version is pinned exactly and fetched from crates.io.

Features deliberately **not** enabled: `loader`, `dyn-load`, `futures`,
`parallel`. There is therefore no dynamic `import`, no native module loading, no
event loop and no thread from the embedded engine.

## Sandbox API surface (both slices)

| Name | Present? | Notes |
|---|---|---|
| `event` (`type`, `targetName`, `value`, `rc`) | yes | calculation / validate / format events |
| `this.getField(name)` | yes | read/write field `.value`, read `.name` |
| `util.printf`, `util.printd` | yes | deterministic, string-only, documented subset |
| `AFNumber_Format`, `AFNumber_Keystroke`, `AFPercent_Format`, `AFPercent_Keystroke`, `AFDate_FormatEx`, `AFDate_KeystrokeEx`, `AFRange_Validate`, `AFSimple_Validate` | yes | pass-through helpers, no external side effects |
| `app` / `app.launchURL` | **absent, refused** | static deny scan + runtime `ReferenceError`/`undefined` |
| `this.submitForm` / `this.exportDataObject` | **absent, refused** | static deny scan + runtime absence |
| `Net.*` / `Collab.*` | **absent, refused** | static deny scan + runtime absence |
| filesystem, network, process, clocks, native modules | **absent** | no such capability exists |

The static deny scan exists for a clear typed refusal (`JS_API_REFUSED`) and for
diagnostics. **It is not the security boundary.** The boundary is that the
dangerous APIs are never installed and the engine has no host capability to
reach even if a script found another spelling.

## Budget (native)

`JsBudget` bounds interrupt-handler invocations (an instruction proxy — QuickJS
calls the handler at loop back-edges and function calls), the QuickJS heap and
stack, the accepted script length, and field/string counts. An infinite loop
trips the interrupt handler and is converted to `JS_INSTRUCTION_BUDGET`; heap
exhaustion becomes `JS_MEMORY_BUDGET`.

## Why there is no wasm build of rquickjs

`rquickjs` 0.11.0 ships pre-generated QuickJS bindings for native targets only.
`wasm32-unknown-unknown` would need `bindgen` + libclang + a wasm-targeting C
compiler, none present here, and QuickJS's C runtime expects a libc that
`wasm32-unknown-unknown` does not provide. We deliberately do **not** attempt it.
This crate is gated off on `wasm32` (`cfg(not(target_arch = "wasm32"))`) and is
in no WASM artifact — the size delta is zero. In the browser the platform's own
V8 engine is used instead, which is both smaller (no shipped engine bytes) and
strictly isolated by `Worker` realm + `terminate()`.
