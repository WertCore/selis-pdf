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
//!    dispatches and answers `ok:true`. The text layer (SL-4.EXT.03) is
//!    checked here rather than in a shell: its `chars` must be index-aligned
//!    with its `text`, which is the invariant every selection rests on.
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
//! 9. **Memory strategy (SL-4.WASM.04)** — a 1.5 GiB claim answers a typed
//!    `BUDGET_BYTES` (never a trap), `memoryStats` reports the live/peak
//!    tallies and the 4 GiB ceiling, `close` releases live bytes, and
//!    `memoryPressure` answers with clamped levels.
//! 10. **The `HttpRangeSource` fetch driver (SL-4.WASM.06)** — the range
//!    exchange driven over the real ABI with a *scripted hostile origin*: a
//!    cooperative two-range fetch reassembles a document whose render
//!    hash-matches the native one, an origin that ignores `Range` degrades to
//!    the whole body, and a missing `Content-Range`, a `Content-Range` for
//!    another offset, a `200` mid-transfer and a server that never answers
//!    are each the registry code the driver's decision table promises. The
//!    last of those is the denial-of-service bound, and it is the leg that
//!    would notice if the bound were only on paper.

use serde_json::{json, Value};

use crate::perf_wasm::{build_driver, checksum};

/// Registry ids (pinned by `crates/selis-error/codes.toml` — numbers are
/// never reused, so hardcoding them here *is* the conformance check).
const CODE_BAD_HANDLE: u32 = 6000;
const CODE_BAD_ARGUMENT: u32 = 6001;
const CODE_BUDGET_BYTES: u32 = 4000;
const CODE_CANCELLED: u32 = 4020;
const CODE_UNSUPPORTED_OP: u32 = 6017;
/// `BINDING_BAD_HANDLE` — also the code for an unknown range transfer, which
/// is a handle across the boundary like any other.
const CODE_BAD_HANDLE_OR_TRANSFER: u32 = 6000;
const CODE_SOURCE_CHANGED: u32 = 2804;
const CODE_IO_READ_FAILED: u32 = 5000;

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
    // Captured here because the text layer below rebinds `resp`, and a report
    // that prints the wrong leg's number is worse than no report.
    let search_matches = resp["value"]["total"].as_u64().unwrap_or(0);
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
    // The text layer (SL-4.EXT.03): the same page, asked for its characters.
    // The invariant this leg exists for is index alignment - `chars[i]`
    // describes `text[i]` - because that is what a selection and a copied
    // string both rest on, and it is the one property a shell cannot recover
    // if the guest gets it wrong.
    let (resp, _) = s.rpc(json!({"op":"textLayer", "doc": text_doc, "page": 0}), None)?;
    let resp = expect(resp, "textLayer")?;
    let layer_text = resp["value"]["text"].as_str().ok_or("textLayer: no text")?;
    let lines = resp["value"]["lines"]
        .as_array()
        .ok_or("textLayer: no lines")?;
    let mut quads = 0usize;
    for line in lines {
        let line_text = line["text"].as_str().ok_or("textLayer: line has no text")?;
        let chars = line["chars"]
            .as_array()
            .ok_or("textLayer: line has no chars")?;
        if chars.len() != line_text.encode_utf16().count() {
            return Err(format!(
                "textLayer: {} quads for {} code units - index alignment is broken",
                chars.len(),
                line_text.encode_utf16().count()
            ));
        }
        for c in chars {
            for field in ["x", "y", "width", "height"] {
                if c["rect"][field].as_f64().is_none() {
                    return Err(format!("textLayer: rect.{field} is not a number"));
                }
            }
            if c["advance"].as_f64().is_none() || !c["inked"].is_boolean() {
                return Err("textLayer: a char entry is missing advance or inked".to_string());
            }
            quads += 1;
        }
    }
    if layer_text.is_empty() {
        return Err("textLayer: the fixture drew no text".to_string());
    }
    println!(
        "wasm-protocol: textLayer ok ({quads} quads over {} lines)",
        lines.len()
    );

    println!(
        "wasm-protocol: text/search ok ({} bytes of text, {} matches for {word:?}); progress slot ok",
        text.len(),
        search_matches,
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

    // ── 9. memory strategy (SL-4.WASM.04) ─────────────────────────────────
    // The text fixture's document is still open: close it so the live tally
    // is exactly the reopened minimal.pdf (423 bytes).
    let (resp, _) = s.rpc(json!({"op":"close", "doc": text_doc}), None)?;
    expect(resp, "memory-close-text")?;

    // A 1.5 GiB claim is a typed BUDGET_BYTES response, not a trap/crash; the
    // guest validates before touching the payload, so no 1.5 GiB copy exists.
    let big: u64 = 1_610_612_736;
    let (resp, _) = s.rpc(
        json!({"op":"open", "src": {"kind":"bytes", "len": big}}),
        None,
    )?;
    expect_code(&resp, CODE_BUDGET_BYTES, "memory-1.5gb")?;

    // memoryStats reports the live/peak tallies (minimal.pdf = 423 bytes).
    let (resp, _) = s.rpc(json!({"op":"memoryStats"}), None)?;
    let resp = expect(resp, "memory-stats")?;
    if resp["value"]["wasmMaxBytes"] != json!(4_294_967_296u64) {
        return Err(format!("memory-stats: wasmMaxBytes wrong: {resp}"));
    }
    if resp["value"]["liveBytes"] != json!(423u64) {
        return Err(format!("memory-stats: liveBytes must be 423: {resp}"));
    }

    // Close releases: live returns to zero, peak is retained.
    let (resp, _) = s.rpc(json!({"op":"close", "doc": handle2}), None)?;
    expect(resp, "memory-close")?;
    let (resp, _) = s.rpc(json!({"op":"memoryStats"}), None)?;
    let resp = expect(resp, "memory-stats-after-close")?;
    if resp["value"]["liveBytes"] != json!(0u64) || resp["value"]["liveDocs"] != json!(0u64) {
        return Err(format!("memory-stats: close must release: {resp}"));
    }
    // Peak retains the high-water mark (minimal.pdf 423 + text.pdf 1243 were
    // open together earlier in the run).
    if resp["value"]["peakBytes"] != json!(1666u64) {
        return Err(format!("memory-stats: peak must be retained: {resp}"));
    }

    // memoryPressure answers and clamps.
    let (resp, _) = s.rpc(json!({"op":"memoryPressure", "level": 99}), None)?;
    let resp = expect(resp, "memory-pressure")?;
    if resp["value"]["level"] != json!(2) {
        return Err(format!("memory-pressure: level must clamp to 2: {resp}"));
    }
    println!("wasm-protocol: memory ok (typed 1.5 GiB, stats, release, pressure)");

    // ── 10. the HttpRangeSource fetch driver (SL-4.WASM.06) ────────────────
    //
    // The guest plans and judges; this leg plays the host *and* the origin, so
    // it is the only place the decision table is checked over the real ABI
    // rather than in-crate. Every request the guest names is a bodyless GET
    // for a byte range, and there is no message field a document could be
    // uploaded through — asserted below, because it is the invariant.
    let total = u64::try_from(doc_bytes.len()).map_err(|_| "fixture length".to_string())?;
    let (resp, _) = s.rpc(
        json!({"op": "rangeOpen",
               "url": "https://example.test/minimal.pdf",
               "size": total, "chunk": 64}),
        None,
    )?;
    let resp = expect(resp, "rangeOpen")?;
    let transfer = resp["value"]["transfer"]
        .as_u64()
        .ok_or("rangeOpen: no transfer id")?;
    let first_start = resp["value"]["request"]["start"]
        .as_u64()
        .ok_or("rangeOpen: no first range")?;
    let first_end = resp["value"]["request"]["end"]
        .as_u64()
        .ok_or("rangeOpen: no first range end")?;
    if first_start != 0 || first_end == 0 || first_end >= total {
        return Err(format!(
            "rangeOpen: first range {first_start}-{first_end} is not a sane slice of {total}"
        ));
    }
    // The planned request is a `Range` header and two offsets. Nothing else.
    let planned = serde_json::to_string(&resp["value"]["request"]).map_err(|e| e.to_string())?;
    for forbidden in ["\"body\"", "\"method\"", "\"upload\"", "\"post\"", "\"formData\""] {
        if planned.contains(forbidden) {
            return Err(format!(
                "rangeOpen: the request shape grew a {forbidden} field"
            ));
        }
    }

    // The first range, served cooperatively.
    let cut = usize::try_from(first_end).map_err(|_| "range".to_string())?;
    let head = &doc_bytes[..cut];
    let (resp, _) = s.rpc(
        json!({"op": "rangeChunk", "transfer": transfer, "start": first_start,
               "status": 206,
               "contentRange": format!("bytes {first_start}-{}/{total}", head.len() - 1),
               "contentLength": head.len(), "len": head.len()}),
        Some(head),
    )?;
    let resp = expect(resp, "rangeChunk-head")?;
    if resp["value"]["complete"] != json!(false) {
        return Err(format!("rangeChunk-head: must not be complete yet: {resp}"));
    }
    if resp["value"]["received"].as_u64() != Some(head.len() as u64) {
        return Err(format!("rangeChunk-head: received is wrong: {resp}"));
    }
    let next_start = resp["value"]["request"]["start"]
        .as_u64()
        .ok_or("rangeChunk-head: no next range")?;
    if next_start != head.len() as u64 {
        return Err(format!("rangeChunk-head: next range starts at {next_start}"));
    }

    // The rest, as a legitimate short final range.
    let tail = &doc_bytes[cut..];
    let (resp, _) = s.rpc(
        json!({"op": "rangeChunk", "transfer": transfer, "start": next_start,
               "status": 206,
               "contentRange": format!("bytes {next_start}-{}/{total}", total - 1),
               "contentLength": tail.len(), "len": tail.len()}),
        Some(tail),
    )?;
    let resp = expect(resp, "rangeChunk-tail")?;
    if resp["value"]["complete"] != json!(true)
        || resp["value"]["degraded"] != json!(false)
        || resp["value"]["randomAccess"] != json!(true)
        || resp["value"]["received"].as_u64() != Some(total)
    {
        return Err(format!("rangeChunk-tail: wrong completion: {resp}"));
    }
    let remote_doc = resp["value"]["doc"]
        .as_u64()
        .ok_or("rangeChunk-tail: no document handle")?;
    if resp["value"]["pageSizes"].as_array().map(Vec::len) != Some(1) {
        return Err(format!("rangeChunk-tail: pageSizes missing: {resp}"));
    }

    // The reassembled document is byte-exact: its render hash-matches the
    // native render of the same fixture. This is the check that would notice a
    // splice, a dropped byte or an off-by-one in the range arithmetic.
    let (resp, remote_pixels) = s.rpc(
        json!({"op":"render", "doc": remote_doc, "page": 0, "params": {"dpi": 72.0}}),
        None,
    )?;
    expect(resp, "range-render")?;
    let remote_sum = checksum(&remote_pixels);
    if remote_sum != guest_sum {
        return Err(format!(
            "range: the range-fetched document renders to {remote_sum}, the inline one to {guest_sum}: reassembly is not byte-exact"
        ));
    }
    let (resp, _) = s.rpc(json!({"op":"close", "doc": remote_doc}), None)?;
    expect(resp, "range-close-doc")?;
    println!(
        "wasm-protocol: httpRange ok (two ranges reassemble; render checksum {remote_sum} matches)"
    );

    // A scripted origin helper: open a fresh transfer and hand back its id
    // and first planned range, so each hostile case starts from zero.
    macro_rules! open_transfer {
        () => {{
            let (r, _) = s.rpc(
                json!({"op": "rangeOpen",
                       "url": "https://example.test/hostile.pdf",
                       "size": total, "chunk": 64}),
                None,
            )?;
            let v = expect(r, "rangeOpen-hostile")?;
            let t = v["value"]["transfer"].as_u64().ok_or("no transfer id")?;
            let st = v["value"]["request"]["start"].as_u64().ok_or("no start")?;
            (t, st)
        }};
    }

    // (a) The origin ignores `Range` and sends the whole document: the
    //     documented fallback, complete and degraded.
    let (t, st) = open_transfer!();
    let (resp, _) = s.rpc(
        json!({"op": "rangeChunk", "transfer": t, "start": st, "status": 200,
               "contentLength": doc_bytes.len(), "len": doc_bytes.len()}),
        Some(&doc_bytes),
    )?;
    let resp = expect(resp, "range-degraded")?;
    if resp["value"]["complete"] != json!(true)
        || resp["value"]["degraded"] != json!(true)
        || resp["value"]["randomAccess"] != json!(false)
    {
        return Err(format!("range-degraded: not reported as degraded: {resp}"));
    }
    let degraded_doc = resp["value"]["doc"].as_u64().ok_or("no handle")?;
    let (resp, _) = s.rpc(json!({"op":"close", "doc": degraded_doc}), None)?;
    expect(resp, "range-degraded-close")?;

    // (b) `206` with no readable `Content-Range` — the cross-origin case where
    //     the origin did not expose it. Typed, and the transfer is released.
    let (t, st) = open_transfer!();
    let (resp, _) = s.rpc(
        json!({"op": "rangeChunk", "transfer": t, "start": st, "status": 206,
               "contentLength": 4, "len": 4}),
        Some(&doc_bytes[..4]),
    )?;
    expect_code(&resp, CODE_IO_READ_FAILED, "range-no-content-range")?;
    if resp["detail"].as_str() != Some("origin-refused-ranges") {
        return Err(format!(
            "range-no-content-range: detail must name the fallback: {resp}"
        ));
    }
    let (resp, _) = s.rpc(json!({"op":"rangeClose", "transfer": t}), None)?;
    expect_code(
        &resp,
        CODE_BAD_HANDLE_OR_TRANSFER,
        "range-no-content-range-release",
    )?;

    // (c) A `Content-Range` for an offset nobody asked for: the origin is
    //     serving something else, and splicing it would be silent corruption.
    let (t, st) = open_transfer!();
    let (resp, _) = s.rpc(
        json!({"op": "rangeChunk", "transfer": t, "start": st, "status": 206,
               "contentRange": format!("bytes {}-{}/{total}", total - 4, total - 1),
               "contentLength": 4, "len": 4}),
        Some(&doc_bytes[..4]),
    )?;
    expect_code(&resp, CODE_SOURCE_CHANGED, "range-wrong-offset")?;

    // (d) A `200` *after* a range was accepted: the origin changed its mind,
    //     and the only safe answer is to refuse.
    let (t, st) = open_transfer!();
    let (resp, _) = s.rpc(
        json!({"op": "rangeChunk", "transfer": t, "start": st, "status": 206,
               "contentRange": format!("bytes 0-{}/{total}", cut - 1),
               "contentLength": cut, "len": cut}),
        Some(&doc_bytes[..cut]),
    )?;
    expect(resp, "range-then-head")?;
    let (resp, _) = s.rpc(
        json!({"op": "rangeChunk", "transfer": t, "start": cut, "status": 200,
               "contentLength": doc_bytes.len(), "len": doc_bytes.len()}),
        Some(&doc_bytes),
    )?;
    expect_code(&resp, CODE_SOURCE_CHANGED, "range-200-mid-transfer")?;

    // (e) An origin that never answers: the retry bound stops it, and it stops
    //     it *here*, over the wire, where the host is the one asking.
    let (t, st) = open_transfer!();
    let mut retried = 0u32;
    let last = loop {
        let (resp, _) = s.rpc(
            json!({"op": "rangeChunk", "transfer": t, "start": st, "status": 0, "len": 0}),
            None,
        )?;
        if resp["ok"] == json!(false) {
            break resp;
        }
        retried += 1;
        if retried > 16 {
            return Err(
                "range-never-answers: the guest kept asking for a retry; the bound is not real"
                    .to_string(),
            );
        }
    };
    expect_code(&last, CODE_IO_READ_FAILED, "range-never-answers")?;
    if retried < 2 {
        return Err(format!(
            "range-never-answers: only {retried} retries; the allowance should be spent"
        ));
    }
    println!(
        "wasm-protocol: httpRange hostile ok (degrade, no Content-Range, wrong offset, 200 mid-transfer, {retried} retries then a bound)"
    );

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
