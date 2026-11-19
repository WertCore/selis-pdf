#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]
//! Budgeted, consent-gated form-script interpreter (SL-5.JS.01/02/03).
//!
//! # Two environments, one policy (ADR-P0020)
//!
//! Active content is off by default. This crate is the **native** slice: a
//! budgeted QuickJS (`rquickjs`, MIT) interpreter for CLI / desktop / mobile.
//! The browser slice lives in TypeScript under `apps/web/host/src/editor/` and
//! uses the browser's own engine inside a sandboxed `Worker` — it adds no WASM
//! bytes and no new dependency. Both slices expose the *same* Acrobat subset
//! and the *same* consent/origin policy (mirrored in TypeScript), and both refuse
//! `app.launchURL`, `submitForm`, `exportDataObject`, `Net.*` and `Collab.*`.
//!
//! # NATIVE-ONLY, honestly
//!
//! `rquickjs` 0.11.0 ships pre-generated QuickJS bindings for native targets
//! only. `wasm32-unknown-unknown` would need `bindgen` + libclang + a
//! wasm-targeting C toolchain, none present here, and QuickJS's C runtime
//! expects a libc that `wasm32-unknown-unknown` does not provide. This crate is
//! therefore gated off on `wasm32` and is in no WASM artifact. See README.md.
//!
//! # The boundary
//!
//! The engine has no filesystem, network, process, clock or module-loader
//! capability in the default `rquickjs` feature set. The whitelisted API is the
//! *only* surface installed. A static deny-scan gives a typed `JS_API_REFUSED`,
//! but it is **not** the security boundary: the dangerous APIs are simply never
//! installed, so even a spelling the scan misses is a `ReferenceError`.
//!
//! ```
//! use selis_pdf_js::{Field, EventKind, JsBudget, JsRequest, JsSandbox};
//!
//! let mut sandbox = JsSandbox::new().expect("native QuickJS runtime");
//! let req = JsRequest {
//!     script: "event.value = this.getField('A').value * 2;".to_string(),
//!     event: EventKind::Calculate,
//!     target: "B".to_string(),
//!     fields: vec![Field { name: "A".to_string(), value: "21".to_string() }],
//! };
//! let outcome = sandbox.run(req, JsBudget::conservative()).expect("script runs");
//! assert_eq!(outcome.event_value.as_deref(), Some("42"));
//! ```

#![forbid(unsafe_code)]

#[cfg(not(target_arch = "wasm32"))]
mod budget;
#[cfg(not(target_arch = "wasm32"))]
mod forbidden;
#[cfg(not(target_arch = "wasm32"))]
mod sandbox;
#[cfg(not(target_arch = "wasm32"))]
mod types;

#[cfg(not(target_arch = "wasm32"))]
pub use budget::{JsBudget, JsUsage};
#[cfg(not(target_arch = "wasm32"))]
pub use forbidden::{find_forbidden_api, FORBIDDEN_ACROBAT_APIS};
#[cfg(not(target_arch = "wasm32"))]
pub use sandbox::{JsSandbox, AF_FUNCTIONS};
#[cfg(not(target_arch = "wasm32"))]
pub use types::{EventKind, Field, FieldUpdate, JsOutcome, JsRequest};
