//! The Worker-protocol conformance harness (SL-4.WASM.01, `cargo xtask
//! wasm-protocol`).
//!
//! Builds the `selis-pdf-wasm` guest for `wasm32-unknown-unknown`, loads it
//! into the embedded wasmtime (the same embedding `perf-wasm` uses), and
//! drives the **wire format as a black box**: requests are constructed as
//! JSON (no Rust protocol types — a conformance harness that shared the
//! guest's types could not catch a wire break), responses are navigated as
//! JSON values.
//!
//! Legs, in order:
//!
//! 1. **Open/page/render/text/search round-trips** — every op of schema v1
//!    dispatches and answers `ok:true`.
//! 2. **guest == native checksum** — the rendered page's RGBA8 pixels hash
//!    identically in the guest and natively (the PERF.03 discipline applied
//!    to the protocol surface: ADR-P0042).
//! 3. **Tile path** — a tile equals the matching sub-rectangle of the full
//!    canvas (ADR-P0011's tile path, deterministic composition).
//! 4. **Budget exhaustion over the boundary** — a tiny byte budget yields a
//!    typed `BUDGET_BYTES` response carrying the registry's `docState`.
//! 5. **Cancellation over the boundary** — a pre-cancel (the message
//!    channel a single-threaded Worker can actually deliver) answers
//!    `CANCELLED` without executing. The in-flight channel (the exported
//!    cancel slot, written out-of-band) is proven natively by the crate's
//!    deterministic clock test; the shared-memory watcher lands with
//!    SL-4.WASM.03.
//! 6. **Malformed-message containment** — a wrong schema version, an
//!    unknown op, a stale handle: each answers a typed error and the next
//!    well-formed message still succeeds.
//! 7. **Phase 5 surfaces** — `mutate`/`save` answer
//!    `BINDING_UNSUPPORTED_OP` (schema locked, capability deferred).
//! 8. **Progress slot** — the exported telemetry slot reflects the last
//!    stage boundary (request id, stage, fraction).

use serde_json::{json, Value};

use crate::perf_wasm::{build_driver, checksum};

/// Registry ids (pinned by `crates/selis-error/codes.toml` — numbers are
/// never reused, so hardcoding them here *is* the conformance check).
const CODE_BAD_HANDLE: u32 = 6000;
const CODE_BAD_ARGUMENT: u32 = 6001;
const CODE_BUDGET_BYTES: u32 = 4000;
const CODE_CANCELLED: u32 = 4020;
const CODE_UNSUPPORTED_OP: u32 = 6017;

/// A fixture from the engine's test set.
fn fixture(name: &str) -> Result<Vec<u8>, String> {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../crates/selis-pdf-engine/src/fixtures")
        .join(name);
    std::fs::read(&path).map_err(|e| format!("read {}: {e}", path.display()))
}

/// The guest exports the harness drives.
struct Exports {
    input_alloc: wasmtime::TypedFunc<u32, u32>,
    free: wasmtime::TypedFunc<(u32, u32), ()>,
    dispatch: wasmtime::TypedFunc<(u32, u32, u32, u32, u32), u32>,
    progress_slot: wasmtime::TypedFunc<(), u32>,
    memory: wasmtime::Memory,
    store: wasmtime::Store<crate::perf_wasm::HostState>,
}

impl Exports {
    fn new(wasm_path: &std::path::Path) -> Result<Self, String> {
        let engine = wasmtime::Engine::default();
        let module = wasmtime::Module::from_file(&engine, wasm_path)
            .map_err(|e| format!("wasmtime compile {}: {e}", wasm_path.display()))?;
        let mut linker = wasmtime::Linker::new(&engine);
        crate::perf_wasm::define_shims(&mut linker, &module)?;
        let limiter = wasmtime::StoreLimitsBuilder::new()
            .memory_size(crate::perf_wasm::GUEST_MEMORY_CAP)
            .build();
        let mut store = wasmtime::Store::new(&engine, crate::perf_wasm::HostState { limiter });
        store.limiter(|state| &mut state.limiter);
        let instance = linker
            .instantiate(&mut store, &module)
            .map_err(|e| format!("wasmtime instantiate: {e}"))?;
        let memory = instance
            .get_memory(&mut store, "memory")
            .ok_or_else(|| "guest exports no `memory`".to_string())?;
        Ok(Self {
            input_alloc: instance
                .get_typed_func::<u32, u32>(&mut store, "selis_input_alloc")
                .map_err(|e| format!("missing export selis_input_alloc: {e}"))?,
            free: instance
                .get_typed_func::<(u32, u32), ()>(&mut store, "selis_free")
                .map_err(|e| format!("missing export selis_free: {e}"))?,
            dispatch: instance
                .get_typed_func::<(u32, u32, u32, u32, u32), u32>(&mut store, "selis_dispatch")
                .map_err(|e| format!("missing export selis_dispatch: {e}"))?,
            progress_slot: instance
                .get_typed_func::<(), u32>(&mut store, "selis_progress_slot")
                .map_err(|e| format!("missing export selis_progress_slot: {e}"))?,
            memory,
            store,
        })
    }

    /// Copy bytes into guest memory via `selis_input_alloc`.
    fn copy_in(&mut self, bytes: &[u8]) -> Result<(u32, u32), String> {
        let len = u32::try_from(bytes.len())
            .map_err(|_| "payload exceeds the guest ABI length".to_string())?;
        let ptr = self
            .input_alloc
            .call(&mut self.store, len)
            .map_err(|e| format!("input_alloc trap: {e}"))?;
        if ptr == 0 {
            return Err("input_alloc returned null".to_string());
        }
        let off = usize::try_from(ptr).map_err(|_| "input pointer out of range".to_string())?;
        self.memory
            .write(&mut self.store, off, bytes)
            .map_err(|e| format!("input write OOB: {e}"))?;
        Ok((ptr, len))
    }

    fn read_u32(&mut self, addr: u32) -> Result<u32, String> {
        let off = usize::try_from(addr).map_err(|_| "out-word out of range".to_string())?;
        let mut buf = [0u8; 4];
        self.memory
            .read(&mut self.store, off, &mut buf)
            .map_err(|e| format!("out-word read OOB: {e}"))?;
        Ok(u32::from_le_bytes(buf))
    }

    fn read_bytes(&mut self, ptr: u32, len: u32) -> Result<Vec<u8>, String> {
        let off = usize::try_from(ptr).map_err(|_| "payload pointer out of range".to_string())?;
        let n = usize::try_from(len).map_err(|_| "payload length out of range".to_string())?;
        let mut out = vec![0u8; n];
        self.memory
            .read(&mut self.store, off, &mut out)
            .map_err(|e| format!("payload read OOB: {e}"))?;
        Ok(out)
    }

    /// Dispatch one request with an optional binary payload; returns the
    /// parsed response JSON and the attachment. All guest buffers are freed
    /// before return.
    fn send(
        &mut self,
        request: &Value,
        payload: Option<&[u8]>,
    ) -> Result<(Value, Vec<u8>), String> {
        let req_bytes =
            serde_json::to_vec(request).map_err(|e| format!("serialise request: {e}"))?;
        let (req_ptr, req_len) = self.copy_in(&req_bytes)?;
        let (payload_ptr, payload_len) = match payload {
            Some(bytes) => self.copy_in(bytes)?,
            None => (0, 0),
        };
        // Three out-word slots for [resp_len, payload_ptr, payload_len].
        let (out_ptr, out_len) = self.copy_in(&[0u8; 12])?;
        let resp_ptr = self
            .dispatch
            .call(
                &mut self.store,
                (req_ptr, req_len, payload_ptr, payload_len, out_ptr),
            )
            .map_err(|e| format!("dispatch trap: {e}"))?;
        let resp_len = self.read_u32(out_ptr)?;
        let resp_payload_ptr = self.read_u32(out_ptr + 4)?;
        let resp_payload_len = self.read_u32(out_ptr + 8)?;
        self.free
            .call(&mut self.store, (req_ptr, req_len))
            .map_err(|e| format!("free trap: {e}"))?;
        if payload_len > 0 {
            self.free
                .call(&mut self.store, (payload_ptr, payload_len))
                .map_err(|e| format!("free trap: {e}"))?;
        }
        self.free
            .call(&mut self.store, (out_ptr, out_len))
            .map_err(|e| format!("free trap: {e}"))?;
        if resp_ptr == 0 {
            return Err("dispatch returned null (an ABI failure, see the export docs)".to_string());
        }
        let resp_bytes = self.read_bytes(resp_ptr, resp_len)?;
        self.free
            .call(&mut self.store, (resp_ptr, resp_len))
            .map_err(|e| format!("free trap: {e}"))?;
        let attachment = if resp_payload_len > 0 {
            self.read_bytes(resp_payload_ptr, resp_payload_len)?
        } else {
            Vec::new()
        };
        if resp_payload_len > 0 {
            self.free
                .call(&mut self.store, (resp_payload_ptr, resp_payload_len))
                .map_err(|e| format!("free trap: {e}"))?;
        }
        let response: Value = serde_json::from_slice(&resp_bytes).map_err(|e| {
            format!(
                "response is not JSON: {e}: {}",
                String::from_utf8_lossy(&resp_bytes)
            )
        })?;
        Ok((response, attachment))
    }

    /// The progress slot's three words (request id low bits, stage, fraction).
    fn progress(&mut self) -> Result<(u32, u32, u32), String> {
        let base = self
            .progress_slot
            .call(&mut self.store, ())
            .map_err(|e| format!("progress_slot trap: {e}"))?;
        Ok((
            self.read_u32(base)?,
            self.read_u32(base + 4)?,
            self.read_u32(base + 8)?,
        ))
    }
}

/// A convenience wrapper so each leg reads like the wire it drives: the
/// caller names the op body, the wrapper stamps the schema version and a
/// fresh correlation id.
struct Session<'a> {
    x: &'a mut Exports,
    next_id: u64,
}

impl Session<'_> {
    fn rpc(&mut self, body: Value, payload: Option<&[u8]>) -> Result<(Value, Vec<u8>), String> {
        let id = self.next_id;
        self.next_id = id + 1;
        let mut request = body;
        request["v"] = json!(1);
        request["id"] = json!(id);
        self.x.send(&request, payload)
    }

    fn progress(&mut self) -> Result<(u32, u32, u32), String> {
        self.x.progress()
    }
}

fn ok_or_fail(response: &Value, leg: &str) -> Result<(), String> {
    if response["ok"] == json!(true) {
        Ok(())
    } else {
        Err(format!("{leg}: expected ok:true, got {response}"))
    }
}

fn expect(response: Value, leg: &str) -> Result<Value, String> {
    ok_or_fail(&response, leg)?;
    Ok(response)
}

fn expect_code(response: &Value, code: u32, leg: &str) -> Result<(), String> {
    if response["ok"] == json!(false) && response["code"] == json!(code) {
        Ok(())
    } else {
        Err(format!("{leg}: expected code {code}, got {response}"))
    }
}

/// Run the whole conformance suite. Errors name the failing leg.
pub fn run() -> Result<(), String> {
    let wasm_path = build_driver()?;
    println!("wasm-protocol: guest {}", wasm_path.display());
    let mut x = Exports::new(&wasm_path)?;
    let mut s = Session {
        x: &mut x,
        next_id: 100,
    };

    // ── 1. open ────────────────────────────────────────────────────────────
    let doc_bytes = fixture("minimal.pdf")?;
    let (resp, _) = s.rpc(
        json!({"op":"open",
               "src": {"kind":"bytes", "len": doc_bytes.len()},
               "budget": {"surface":"viewer"}}),
        Some(&doc_bytes),
    )?;
    let resp = expect(resp, "open")?;
    let handle = resp["value"]["doc"].as_u64().ok_or("open: no doc handle")?;
    let pages = resp["value"]["pages"].as_u64().ok_or("open: no pages")?;
    println!("wasm-protocol: open ok (handle {handle}, {pages} pages)");

    // ── 2. page metadata ───────────────────────────────────────────────────
    let (resp, _) = s.rpc(json!({"op":"page", "doc": handle, "page": 0}), None)?;
    let resp = expect(resp, "page")?;
    let w_pt = resp["value"]["widthPt"]
        .as_f64()
        .ok_or("page: no widthPt")?;
    if w_pt <= 0.0 {
        return Err("page: widthPt must be positive".to_string());
    }
    println!("wasm-protocol: page ok ({w_pt} pt wide)");

    // ── 3. render + guest==native checksum ────────────────────────────────
    let (resp, guest_pixels) = s.rpc(
        json!({"op":"render", "doc": handle, "page": 0, "params": {"dpi": 72}}),
        None,
    )?;
    let resp = expect(resp, "render")?;
    let width = resp["value"]["width"].as_u64().ok_or("render: no width")? as usize;
    let height = resp["value"]["height"]
        .as_u64()
        .ok_or("render: no height")? as usize;
    if guest_pixels.len() != width * height * 4 {
        return Err(format!(
            "render: payload {} != canvas {}x{}x4",
            guest_pixels.len(),
            width,
            height
        ));
    }
    let native_pixels = native_render(&doc_bytes)?;
    let (guest_sum, native_sum) = (checksum(&guest_pixels), checksum(&native_pixels));
    if guest_sum != native_sum {
        return Err(format!(
            "render: guest checksum {guest_sum} != native {native_sum}: cross-architecture determinism violated"
        ));
    }
    println!("wasm-protocol: render ok (checksums match: {guest_sum})");

    // A tile is the matching sub-rectangle of the full canvas.
    let (tile_w, tile_h) = (width.min(16), height.min(8));
    let (resp, tile_pixels) = s.rpc(
        json!({"op":"render", "doc": handle, "page": 0,
               "params": {"dpi": 72, "tile": {"x":0, "y":0, "w": tile_w, "h": tile_h}}}),
        None,
    )?;
    let resp = expect(resp, "render-tile")?;
    if resp["value"]["tile"]["w"] != json!(tile_w) {
        return Err("render-tile: geometry echo missing".to_string());
    }
    let stride = width * 4;
    for row in 0..tile_h {
        let tile_start = row * tile_w * 4;
        let tile_end = tile_start + tile_w * 4;
        let full_start = row * stride;
        let full_end = full_start + tile_w * 4;
        if tile_pixels[tile_start..tile_end] != guest_pixels[full_start..full_end] {
            return Err(format!(
                "render-tile: row {row} differs from the full canvas"
            ));
        }
    }
    println!("wasm-protocol: render-tile ok ({tile_w}x{tile_h} matches the full canvas)");

    // ── 4. text + search (the text fixture) ───────────────────────────────
    let text_bytes = fixture("text.pdf")?;
    let (resp, _) = s.rpc(
        json!({"op":"open", "src": {"kind":"bytes", "len": text_bytes.len()}}),
        Some(&text_bytes),
    )?;
    let text_doc = expect(resp, "open-text")?["value"]["doc"]
        .as_u64()
        .ok_or("open-text: no handle")?;
    let (resp, text_payload) = s.rpc(
        json!({"op":"text", "doc": text_doc, "page": 0, "format":"json"}),
        None,
    )?;
    let resp = expect(resp, "text")?;
    if resp["value"]["format"] != json!("json") || text_payload.is_empty() {
        return Err("text: json format or payload missing".to_string());
    }
    // Plain text too, to find a query word for search.
    let (resp, text_plain) = s.rpc(json!({"op":"text", "doc": text_doc, "page": 0}), None)?;
    expect(resp, "text-plain")?;
    let text = String::from_utf8(text_plain).map_err(|_| "text: payload is not utf8")?;
    let word = text
        .split_whitespace()
        .next()
        .ok_or("text: fixture carries no text")?
        .to_string();
    let (resp, _) = s.rpc(json!({"op":"search", "doc": text_doc, "query": word}), None)?;
    let resp = expect(resp, "search")?;
    if resp["value"]["total"].as_u64().unwrap_or(0) < 1 {
        return Err(format!("search: no matches for {word:?}"));
    }
    // The progress slot reflects the last stage boundary of the search.
    let search_id = resp["id"].as_u64().unwrap_or(0);
    let (slot_request, slot_stage, slot_fraction) = s.progress()?;
    if slot_stage != 4 || slot_fraction != 10_000 {
        return Err(format!(
            "progress slot: expected stage 4 fraction 10000, got {slot_stage}/{slot_fraction}"
        ));
    }
    if slot_request != u32::try_from(search_id).unwrap_or(0) {
        return Err(format!(
            "progress slot: request id {slot_request} != search id {search_id}"
        ));
    }
    println!(
        "wasm-protocol: text/search ok ({} bytes of text, {} matches for {word:?}); progress slot ok",
        text.len(),
        resp["value"]["total"].as_u64().unwrap_or(0),
    );

    // ── 5. budget exhaustion over the boundary ────────────────────────────
    let (resp, _) = s.rpc(
        json!({"op":"open",
               "src": {"kind":"bytes", "len": doc_bytes.len()},
               "budget": {"surface":"viewer", "overrides": {"bytes": 64}}}),
        Some(&doc_bytes),
    )?;
    expect_code(&resp, CODE_BUDGET_BYTES, "budget")?;
    if resp["docState"].as_str() != Some("PartiallyLoaded") {
        return Err(format!(
            "budget: docState must be PartiallyLoaded, got {resp}"
        ));
    }
    println!("wasm-protocol: budget ok (BUDGET_BYTES crosses the boundary)");

    // ── 6. cancellation over the boundary (pre-cancel channel) ────────────
    let target = s.next_id + 10;
    let (resp, _) = s.rpc(json!({"op":"cancel", "target": target}), None)?;
    if resp["value"]["cancelled"] != json!(true) {
        return Err("cancel: the record must be acknowledged".to_string());
    }
    let mut cancel_request = json!({"v":1, "op":"open",
                                    "src": {"kind":"bytes", "len": doc_bytes.len()}});
    cancel_request["id"] = json!(target);
    let (resp, _) = s.x.send(&cancel_request, Some(&doc_bytes))?;
    expect_code(&resp, CODE_CANCELLED, "cancel-target")?;
    println!("wasm-protocol: cancel ok (pre-cancelled target answers CANCELLED)");

    // ── 7. malformed containment ──────────────────────────────────────────
    let mut wrong_version = json!({"op":"close", "doc": 1});
    wrong_version["v"] = json!(99);
    wrong_version["id"] = json!(s.next_id);
    s.next_id += 1;
    let (resp, _) = s.x.send(&wrong_version, None)?;
    expect_code(&resp, CODE_UNSUPPORTED_OP, "version")?;
    let (resp, _) = s.rpc(json!({"op":"teleport"}), None)?;
    expect_code(&resp, CODE_BAD_ARGUMENT, "unknown-op")?;
    let (resp, _) = s.rpc(json!({"op":"close", "doc": 424_242}), None)?;
    expect_code(&resp, CODE_BAD_HANDLE, "stale-handle")?;
    // …and the next well-formed message still succeeds.
    let (resp, _) = s.rpc(json!({"op":"close", "doc": handle}), None)?;
    expect(resp, "close-after-malformed")?;
    println!("wasm-protocol: malformed ok (version/op/handle contained, worker healthy)");

    // ── 8. Phase 5 surfaces: schema locked, capability deferred ───────────
    let (resp, _) = s.rpc(
        json!({"op":"open", "src": {"kind":"bytes", "len": doc_bytes.len()}}),
        Some(&doc_bytes),
    )?;
    let handle2 = expect(resp, "reopen")?["value"]["doc"]
        .as_u64()
        .ok_or("reopen: no handle")?;
    let (resp, _) = s.rpc(
        json!({"op":"mutate", "doc": handle2, "mutation": {"schema":"selis-mutate/1", "ops":[]}}),
        None,
    )?;
    expect_code(&resp, CODE_UNSUPPORTED_OP, "mutate")?;
    let (resp, _) = s.rpc(
        json!({"op":"save", "doc": handle2, "mode":"incremental"}),
        None,
    )?;
    expect_code(&resp, CODE_UNSUPPORTED_OP, "save")?;
    println!("wasm-protocol: phase-5 surfaces ok (typed BINDING_UNSUPPORTED_OP)");

    println!("wasm-protocol: all legs passed");
    Ok(())
}

/// Native render of page 0 at 72 DPI (the guest-vs-native checksum
/// reference; the same steady state `perf-render` measures).
fn native_render(doc: &[u8]) -> Result<Vec<u8>, String> {
    let budget = selis_sandbox::Budget::profile(selis_sandbox::Surface::Viewer);
    let clock = selis_sandbox::shell_clock();
    let session = selis_pdf_engine::Session::open(doc.to_vec(), &budget, &clock)
        .map_err(|e| format!("native open: {e}"))?;
    let view = session
        .page_view(0, 72.0)
        .ok_or("native render: no media box")?;
    let mut backend = selis_pdf_engine::TinySkiaBackend::new(view.width, view.height)
        .ok_or("native render: cannot create the canvas")?;
    let mut g = budget.guard_with(&clock, selis_sandbox::CancelToken::new());
    session
        .render_page(0, &mut backend, view.ctm, &budget, &mut g)
        .map_err(|e| format!("native render: {e}"))?;
    Ok(backend.pixmap().data().to_vec())
}
