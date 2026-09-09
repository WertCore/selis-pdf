//! The native Tier-2 codec-sandbox host: a `wasmtime` embedding
//! (SL-0.SBX.06; `01-ARCHITECTURE.md Ã‚Â§4`).
//!
//! # The containment model
//!
//! A hostile or buggy codec compiled to WASM is executed here under four
//! mechanically-enforced walls:
//!
//! 1. **A hard linear-memory cap.** [`WasmCodec`] installs a
//!    [`ResourceLimiter`](wasmtime::ResourceLimiter) that refuses every
//!    growth past the cap. A module that "allocates until OOM" is stopped
//!    with a typed [`Code::SandboxMemoryCap`] or [`Code::SandboxFuel`]
//!    error; it can never take the host process with it.
//! 2. **No imports.** There is no WASI, no filesystem, no clock, no
//!    environment. A module that declares an import section is refused
//!    before instantiation ([`Code::SandboxImportDenied`]). Even without
//!    that check the host supplies an empty import list, so instantiation
//!    would fail; the explicit check makes the contract legible.
//! 3. **Fuel instead of a clock.** Every wasm instruction burns fuel. The
//!    allotment is derived from the caller's [`Budget`] wall-clock
//!    deadline. An infinite loop exhausts fuel and traps
//!    ([`Code::SandboxFuel`]) in bounded host time; it can never hang the
//!    engine. The fuel meter is the "tick boundary" at which a spinning
//!    module is interrupted.
//! 4. **The copy-in/copy-out protocol** ([`crate::wasm`]). The module
//!    allocates its own input and output regions and reports them as
//!    pointers/lengths. Every declared region is validated against the
//!    linear memory's current size *by the host* before any access, so a
//!    forged pointer is a typed [`Code::SandboxProtocol`] error, never an
//!    out-of-bounds read into host memory. The module never receives a
//!    host pointer, so there is nothing to escape with.
//!
//! # Budget wiring (ADR-P0006)
//!
//! * `Budget.bytes` â†’ the linear-memory cap (enforced by the limiter, not
//!   charged incrementally: refusing growth must be a wasm-observable
//!   failure, not a host-side poison) plus host-side copies, which are
//!   charged through the same [`BudgetGuard`].
//! * `Budget.wall` â†’ the fuel allotment, budget-relative exactly like
//!   [`BudgetGuard::tick`], measured by the guard's injected clock â€” a
//!   clock the *module* never sees.
//! * [`CancelToken`] â†’ every hostâ†”module boundary runs
//!   [`BudgetGuard::tick`], which checks the deadline and the token with
//!   the guard's own typed errors. Between boundaries the module's worst
//!   case latency to the next boundary is bounded by its remaining fuel,
//!   so no module can outrun cancellation by more than its deadline.
//!
//! On any exhaustion the guard is poisoned (ADR-P0006) and a typed error
//! is returned; nothing partial escapes.
//!
//! # Errors vs traps
//!
//! Everything the *host* rejects is a typed `selis_error::Error`. Traps
//! inside the module (divide by zero, `unreachable`, fuel exhaustion,
//! growth refusal, stack overflow) are contained by wasmtime, surface as
//! `Err`, and are mapped onto [`Code::SandboxTrap`] or the specific
//! fuel/cap codes. The host never panics on module behaviour: the task
//! DoD's "no host crash" is structural, not aspirational.

use selis_error::{err, Code, Error, Result};
use wasmtime::{Config, Engine, Instance, Memory, Module, ResourceLimiter, Store, Trap};

use crate::budget::{BudgetGuard, Resource};
use crate::wasm::{
    CodecOutput, Status, EXPORT_DECODE, EXPORT_FINISH, EXPORT_INIT, EXPORT_MEMORY, EXPORT_OUTPUT,
    EXPORT_OUTPUT_PTR, PAGE_SIZE,
};

/// Fuel charged per nanosecond of the budget's wall-clock deadline.
///
/// Fuel is a deterministic proxy for time, not a cycle count: one wasm
/// instruction costs one unit. The ratio keeps fuel strictly *tighter*
/// than the deadline (a module never outlives its budget) while leaving
/// the host slack for the unfuelled copy-in/copy-out steps. Ten fuel per
/// budget nanosecond means a "spinning forever" module is stopped after at
/// most its deadline's worth of budget has been spent Ã¢â‚¬â€ exactly the
/// ADR-P0006 contract.
const FUEL_PER_BUDGET_NANOS: u64 = 10;

/// The hard ceiling on any codec sandbox's linear memory.
///
/// Above 4 GiB a 32-bit codec could not address its own memory anyway; the
/// ceiling also bounds the u64Ã¢â€ â€™usize conversions the limiter performs.
/// Callers get a cap equal to their budget's *remaining* byte allowance,
/// clamped to this. Nothing in the codec plan (OpenJPEG, Tesseract)
/// legitimately wants more.
const MAX_LINEAR_MEMORY_BYTES: u64 = 4 * 1024 * 1024 * 1024;

/// A host for codec-module runs under a budget.
///
/// One [`Engine`] per host, one fresh [`Store`] + instance per run: no
/// state can leak between runs, and a codec cannot even observe that
/// another codec ran before it. A host (and its engine) is cheap enough
/// to build per run; Phase-2 codecs that run per stream can hoist the
/// engine into a `OnceLock` and precompile modules Ã¢â‚¬â€ [`Module`] is
/// `Clone` and serializable.
///
/// # Budget
///
/// The host charges the guard for host-side copies (input copy-in, output
/// copy-out) and refuses to run when the budget cannot back the memory
/// cap. The cap itself is enforced by wasmtime's limiter, not by charging,
/// because refusing growth mid-instruction must be a trap the module
/// observes, not a host-side poison that would also break the copy-out.
///
/// # Malformed Input
///
/// Unparseable wat, a module with imports, a module missing required
/// exports, an oversized memory declaration, or a module that misuses the
/// buffer protocol all yield typed errors. None of them allocate
/// unboundedly or execute module code.
pub struct WasmCodec<'g, 'c> {
    engine: Engine,
    guard: &'g mut BudgetGuard<'c>,
    memory_cap_bytes: u64,
    fuel: u64,
}

impl core::fmt::Debug for WasmCodec<'_, '_> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("WasmCodec")
            .field("memory_cap_bytes", &self.memory_cap_bytes)
            .field("fuel", &self.fuel)
            .finish_non_exhaustive()
    }
}

impl<'g, 'c> WasmCodec<'g, 'c> {
    /// Build a codec host charging `guard`.
    ///
    /// Cancellation and the wall-clock deadline are taken from the guard
    /// itself: every hostâ†”module boundary runs `BudgetGuard::tick`, so a
    /// cancelled token or an expired deadline (measured by the guard's
    /// injected clock â€” the module never sees a clock) stops the run with
    /// the guard's own typed error. The linear-memory cap is the budget's
    /// remaining byte allowance clamped to [`MAX_LINEAR_MEMORY_BYTES`]; a
    /// caller wanting a tighter cap pre-charges the guard before
    /// constructing the host.
    ///
    /// # Errors
    ///
    /// * `BUDGET_BYTES` when the budget's remaining allowance cannot back
    ///   even one linear-memory page.
    /// * [`Code::SandboxHostError`] when the wasmtime engine cannot be
    ///   built (an engine bug, not a document problem).
    pub fn new(guard: &'g mut BudgetGuard<'c>) -> Result<Self> {
        guard.tick()?;
        let budget = *guard.budget();
        let remaining = budget.bytes.saturating_sub(guard.usage().bytes);
        let memory_cap_bytes = remaining.min(MAX_LINEAR_MEMORY_BYTES);
        if memory_cap_bytes < u64::from(PAGE_SIZE) {
            return Err(err!(
                Code::BudgetBytes,
                during = "wasm-host",
                detail = "budget cannot back one linear-memory page"
            ));
        }
        let fuel = budget.wall.saturating_mul(FUEL_PER_BUDGET_NANOS).max(1);

        let mut config = Config::new();
        config.consume_fuel(true);
        // Shrink the runtime surface. WASI is a feature we never enable,
        // so no filesystem or clock exists for a module to reach; threads
        // and the component model are compiled out of this build
        // (default-features = false), so no knob is needed here.
        let engine = Engine::new(&config).map_err(|_| host_error("engine build failed"))?;

        Ok(Self {
            engine,
            guard,
            memory_cap_bytes,
            fuel,
        })
    }

    /// The linear-memory cap this host enforces, in bytes.
    #[must_use]
    pub const fn memory_cap_bytes(&self) -> u64 {
        self.memory_cap_bytes
    }

    /// The fuel allotment for one run, derived from the budget deadline.
    #[must_use]
    pub const fn fuel(&self) -> u64 {
        self.fuel
    }

    /// Compile a module from `wat` text and run the buffer protocol
    /// against `input`.
    ///
    /// The test/codec-development entry point; production codecs ship
    /// precompiled binaries through [`WasmCodec::run_binary`].
    ///
    /// # Budget
    ///
    /// Memory cap from `Budget.bytes`, fuel from `Budget.wall`,
    /// cancellation from the token. The wat source itself is not charged:
    /// it is engine input, not document data.
    ///
    /// # Malformed Input
    ///
    /// Wat that does not compile yields [`Code::SandboxModuleError`]
    /// without executing anything.
    pub fn run_wat(&mut self, wat: &str, input: &[u8]) -> Result<CodecOutput> {
        let module =
            Module::new(&self.engine, wat).map_err(|_| module_error("wat text did not compile"))?;
        self.run_module(&module, input)
    }

    /// Run a module from a `.wasm` binary (the production path for
    /// Phase-2/Phase-5 codecs).
    ///
    /// `Module::new` detects binary input by magic; the behaviour is
    /// otherwise identical to [`WasmCodec::run_wat`]. For cross-process
    /// caching, `Module::serialize`/`deserialize` images can be introduced
    /// later without changing this API.
    ///
    /// # Budget
    ///
    /// As [`WasmCodec::run_wat`].
    ///
    /// # Malformed Input
    ///
    /// Bytes that are not a valid module yield
    /// [`Code::SandboxModuleError`]; wasmtime's validator rejects anything
    /// hostile before code is generated.
    pub fn run_binary(&mut self, binary: &[u8], input: &[u8]) -> Result<CodecOutput> {
        let module = Module::new(&self.engine, binary)
            .map_err(|_| module_error("wasm binary did not compile"))?;
        self.run_module(&module, input)
    }

    /// Run the buffer protocol against a compiled module.
    fn run_module(&mut self, module: &Module, input: &[u8]) -> Result<CodecOutput> {
        self.guard.tick()?;
        self.validate_module(module)?;

        // One store per run: fresh fuel, fresh limiter, zero shared state.
        let mut store = Store::new(&self.engine, StoreState::new(self.memory_cap_bytes));
        store
            .set_fuel(self.fuel)
            .map_err(|_| host_error("fuel unavailable"))?;
        store.limiter(|state: &mut StoreState| &mut state.limiter);

        let instance =
            Instance::new(&mut store, module, &[]).map_err(|e| map_call_error(e, "instantiate"))?;

        let memory = instance
            .get_memory(&mut store, EXPORT_MEMORY)
            .ok_or_else(|| protocol_error("module does not export a linear memory"))?;
        let mem_bytes = memory.data_size(&store);

        let input_len = protocol_len(input.len())?;

        // Ã¢â€â‚¬Ã¢â€â‚¬ init: the module reserves its input region Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬
        self.guard.tick()?;
        let init = instance
            .get_typed_func::<i32, i32>(&mut store, EXPORT_INIT)
            .map_err(|_| {
                protocol_error(&format!(
                    "missing export `{EXPORT_INIT}` with type (i32)->i32"
                ))
            })?;
        let ptr_raw = init
            .call(&mut store, input_len)
            .map_err(|e| map_call_error(e, "init"))?;
        if ptr_raw == 0 {
            return Err(module_error("init refused the declared input length"));
        }
        let ptr = usize::try_from(ptr_raw)
            .map_err(|_| protocol_error("init returned a negative pointer"))?;
        let in_region = Region {
            ptr,
            len: input.len(),
        };
        self.validate_region(&in_region, mem_bytes, "init input region")?;
        self.guard.charge(Resource::Bytes, in_region.len as u64)?;
        memory
            .write(&mut store, in_region.ptr, input)
            .map_err(|_| host_error("copy-in refused"))?;

        // Ã¢â€â‚¬Ã¢â€â‚¬ decode Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬
        self.guard.tick()?;
        let decode = instance
            .get_typed_func::<i32, i32>(&mut store, EXPORT_DECODE)
            .map_err(|_| {
                protocol_error(&format!(
                    "missing export `{EXPORT_DECODE}` with type (i32)->i32"
                ))
            })?;
        let status_raw = decode
            .call(&mut store, input_len)
            .map_err(|e| map_call_error(e, "decode"))?;
        let status = Status::try_from(status_raw)?;

        // Ã¢â€â‚¬Ã¢â€â‚¬ output: only when the module claims success Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬
        let output = if status == Status::Ok {
            self.copy_out(&instance, &mut store, &memory)?
        } else {
            Vec::new()
        };

        // Ã¢â€â‚¬Ã¢â€â‚¬ finish: best-effort; a hostile finish is contained anyway Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬
        if let Ok(finish) = instance.get_typed_func::<(), i32>(&mut store, EXPORT_FINISH) {
            // A trapping finish cannot undo the (already copied) output or
            // harm the host; the run's own status stands.
            let _ = finish.call(&mut store, ());
        }

        Ok(CodecOutput { status, output })
    }

    /// Retrieve the module's declared output region and copy it out.
    ///
    /// The module cannot force a copy bigger than the host agrees to: the
    /// declared maximum is the budget's remaining bytes, and the module's
    /// declared region is bounds-checked before a single byte is read.
    fn copy_out(
        &mut self,
        instance: &Instance,
        store: &mut Store<StoreState>,
        memory: &Memory,
    ) -> Result<Vec<u8>> {
        self.guard.tick()?;
        // Two plain i32 calls, not a multi-value return: C toolchains
        // (OpenJPEG is the first real client) cannot produce wasm
        // multi-value results, and the browser path benefits identically.
        let output_fn = instance
            .get_typed_func::<i32, i32>(&mut *store, EXPORT_OUTPUT)
            .map_err(|_| {
                protocol_error(&format!(
                    "missing export `{EXPORT_OUTPUT}` with type (i32)->i32"
                ))
            })?;
        let ptr_fn = instance
            .get_typed_func::<(), i32>(&mut *store, EXPORT_OUTPUT_PTR)
            .map_err(|_| {
                protocol_error(&format!(
                    "missing export `{EXPORT_OUTPUT_PTR}` with type ()->i32"
                ))
            })?;
        let remaining = self
            .guard
            .budget()
            .bytes
            .saturating_sub(self.guard.usage().bytes);
        let max_out = i32::try_from(remaining.min(i32::MAX as u64)).unwrap_or(i32::MAX);
        let len_raw = output_fn
            .call(&mut *store, max_out)
            .map_err(|e| map_call_error(e, "output"))?;
        let ptr_raw = ptr_fn
            .call(&mut *store, ())
            .map_err(|e| map_call_error(e, "output_ptr"))?;
        if len_raw == 0 && ptr_raw == 0 {
            return Ok(Vec::new());
        }
        if len_raw < 0 {
            return Err(protocol_error("output returned a negative length"));
        }
        let ptr = usize::try_from(ptr_raw)
            .map_err(|_| protocol_error("output returned a negative pointer"))?;
        let len = usize::try_from(len_raw)
            .map_err(|_| protocol_error("output returned a negative length"))?;
        let region = Region { ptr, len };
        // The module may have grown its memory during decode: re-read the
        // size now, or a valid output region would be rejected as forged.
        let mem_bytes = memory.data_size(&mut *store);
        self.validate_region(&region, mem_bytes, "output region")?;
        // vec_filled charges Resource::Bytes before allocating (the copy
        // is the allocation), so no separate charge here.
        let mut out = crate::alloc::vec_filled(&mut *self.guard, region.len, 0u8)?;
        memory
            .read(store, region.ptr, &mut out)
            .map_err(|_| host_error("copy-out refused"))?;
        Ok(out)
    }

    /// Validate a module-declared region against the memory's current size.
    ///
    /// This is the load-bearing bounds check of the copy-in/copy-out
    /// protocol: a forged pointer or length is a typed protocol error
    /// before any byte is touched.
    fn validate_region(&self, region: &Region, mem_bytes: usize, what: &'static str) -> Result<()> {
        let end = region
            .ptr
            .checked_add(region.len)
            .ok_or_else(|| protocol_error(what))?;
        if end > mem_bytes {
            return Err(protocol_error(what));
        }
        Ok(())
    }

    /// Refuse a module before instantiation.
    ///
    /// No imports (there is nothing the host would provide), exactly one
    /// exported linear memory, and a declared initial size within the cap
    /// and this host's policy.
    fn validate_module(&self, module: &Module) -> Result<()> {
        if module.imports().next().is_some() {
            return Err(err!(Code::SandboxImportDenied, during = "wasm-host"));
        }
        let mut memory_ty: Option<wasmtime::MemoryType> = None;
        for export in module.exports() {
            if export.name() != EXPORT_MEMORY {
                continue;
            }
            if memory_ty.is_some() {
                return Err(protocol_error("module exports more than one linear memory"));
            }
            let ty = export.ty();
            let Some(mem) = ty.memory() else {
                return Err(protocol_error("export named `memory` is not a memory"));
            };
            memory_ty = Some(mem.clone());
        }
        let Some(ty) = memory_ty else {
            return Err(protocol_error("module does not export a linear memory"));
        };
        if ty.is_64() {
            return Err(protocol_error("memory64 is outside the codec protocol"));
        }
        let min_bytes = ty.minimum().saturating_mul(ty.page_size());
        if min_bytes > self.memory_cap_bytes {
            return Err(err!(
                Code::SandboxMemoryCap,
                during = "wasm-host",
                detail = "declared initial memory exceeds the cap"
            ));
        }
        Ok(())
    }
}

/// Per-store host state: the resource limiter.
struct StoreState {
    limiter: CodecLimiter,
}

impl StoreState {
    fn new(cap: u64) -> Self {
        Self {
            limiter: CodecLimiter { cap },
        }
    }
}

/// The hard linear-memory cap, enforced inside wasmtime's growth path.
///
/// Denials follow wasm semantics: a *growth* failure returns `Ok(false)`
/// so the module observes `memory.grow == -1` and may fall back (a real
/// codec such as OpenJPEG must be able to try a smaller arena). A denial
/// of the *initial* allocation means the module declared more memory than
/// it may ever have Ã¢â‚¬â€ that is a policy refusal, surfaced as a trap-shaped
/// error ([`MemoryCapExceeded`]) that fails instantiation immediately.
#[derive(Debug)]
struct CodecLimiter {
    cap: u64,
}

impl ResourceLimiter for CodecLimiter {
    fn memory_growing(
        &mut self,
        current: usize,
        desired: usize,
        _maximum: Option<usize>,
    ) -> core::result::Result<bool, wasmtime::Error> {
        let desired = u64::try_from(desired).unwrap_or(u64::MAX);
        if desired > self.cap {
            if current == 0 {
                return Err(wasmtime::Error::msg(MemoryCapExceeded));
            }
            return Ok(false);
        }
        Ok(true)
    }

    fn table_growing(
        &mut self,
        current: usize,
        desired: usize,
        _maximum: Option<usize>,
    ) -> core::result::Result<bool, wasmtime::Error> {
        // A codec has no legitimate use for a large table; the same byte
        // cap bounds it (one element is one pointer's worth).
        let bytes = u64::try_from(desired)
            .unwrap_or(u64::MAX)
            .saturating_mul(core::mem::size_of::<usize>() as u64);
        if bytes > self.cap {
            if current == 0 {
                return Err(wasmtime::Error::msg(MemoryCapExceeded));
            }
            return Ok(false);
        }
        Ok(true)
    }
}

/// The limiter's refusal marker, downcast out of wasmtime errors.
#[derive(Debug)]
struct MemoryCapExceeded;

impl core::fmt::Display for MemoryCapExceeded {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("linear-memory cap exceeded")
    }
}

impl core::error::Error for MemoryCapExceeded {}

/// A (pointer, length) region a module has declared.
#[derive(Debug, Clone, Copy)]
struct Region {
    ptr: usize,
    len: usize,
}

/// The protocol's `i32` length ceiling, applied to host-side input.
fn protocol_len(len: usize) -> Result<i32> {
    i32::try_from(len).map_err(|_| protocol_error("input does not fit the protocol's i32 length"))
}

/// Map a wasmtime call error onto the typed sandbox taxonomy.
///
/// The `step` name is engine-controlled static text, never document
/// content (ADR-P0017).
fn map_call_error(e: wasmtime::Error, step: &'static str) -> Error {
    if e.downcast_ref::<MemoryCapExceeded>().is_some() {
        return err!(Code::SandboxMemoryCap, during = "wasm-host", detail = step);
    }
    if e.downcast_ref::<wasmtime::OutOfMemory>().is_some() {
        return err!(Code::SandboxMemoryCap, during = "wasm-host", detail = step);
    }
    if let Some(Trap::OutOfFuel) = e.downcast_ref::<Trap>() {
        return err!(Code::SandboxFuel, during = "wasm-host", detail = step);
    }
    if e.downcast_ref::<Trap>().is_some() {
        return err!(Code::SandboxTrap, during = "wasm-host", detail = step);
    }
    err!(Code::SandboxHostError, during = "wasm-host", detail = step)
}

/// A host-side failure at a fixed step, with static detail only.
fn host_error(detail: &'static str) -> Error {
    // Only static step/detail strings are kept; wasmtime's own message is
    // engine text and the discipline of content-free details (ADR-P0017)
    // is simpler to hold when nothing dynamic enters the string.
    err!(
        Code::SandboxHostError,
        during = "wasm-host",
        detail = detail
    )
}

fn module_error(what: &'static str) -> Error {
    err!(
        Code::SandboxModuleError,
        during = "wasm-host",
        detail = what
    )
}

fn protocol_error(what: &str) -> Error {
    err!(Code::SandboxProtocol, during = "wasm-host", detail = what)
}

#[cfg(all(test, feature = "wasm-host"))]
mod tests {
    use super::*;
    use crate::clock::ManualClock;
    use crate::{Budget, CancelToken};
    use selis_error::Code;

    /// A small, deterministic budget: 1 MiB of linear memory (16 pages),
    /// a tiny wall deadline (fuel 1 M instructions), the rest generous.
    fn test_budget() -> Budget {
        Budget {
            bytes: 1_048_576,
            wall: 100_000,
            depth: 16,
            objects: 1_000,
            pixels: 1_000,
        }
    }

    /// A well-behaved codec: reverses its input into a fresh region and
    /// reports the output pointer. This is the shape OpenJPEG will
    /// implement in Phase 2.
    const WELL_BEHAVED: &str = r#"
        (module
          (memory (export "memory") 1)
          (global $len (mut i32) (i32.const 0))
          (func (export "init") (param $n i32) (result i32)
            (global.set $len (local.get $n))
            (i32.const 1024))
          (func (export "decode") (param $n i32) (result i32)
            (local $i i32)
            (block $done
              (loop $again
                (br_if $done (i32.ge_u (local.get $i) (local.get $n)))
                (i32.store8
                  (i32.add (i32.const 4096) (local.get $i))
                  (i32.load8_u
                    (i32.add (i32.const 1024)
                      (i32.sub (local.get $n) (i32.add (local.get $i) (i32.const 1))))))
                (local.set $i (i32.add (local.get $i) (i32.const 1)))
                (br $again)))
            (i32.const 0))
          (func (export "output") (param $max i32) (result i32)
            (global.get $len))
          (func (export "output_ptr") (result i32) (i32.const 4096))
          (func (export "finish") (result i32) (i32.const 0)))"#;

    /// DoD (a): tries to allocate unbounded memory, one page at a time.
    /// The limiter denies growth past the cap (`memory.grow` returns -1),
    /// the module ignores the failure and keeps looping, and the fuel
    /// meter stops it with a typed error.
    const MEMORY_HOG: &str = r#"
        (module
          (memory (export "memory") 1)
          (func (export "init") (param $n i32) (result i32) (i32.const 1024))
          (func (export "decode") (param $n i32) (result i32)
            (loop $grow
              (drop (memory.grow (i32.const 1)))
              (br $grow))
            (unreachable))
          (func (export "output") (param $max i32) (result i32) (i32.const 0))
          (func (export "output_ptr") (result i32) (i32.const 0))
          (func (export "finish") (result i32) (i32.const 0)))"#;

    /// DoD (b): spins forever without touching memory.
    const SPINNER: &str = r#"
        (module
          (memory (export "memory") 1)
          (func (export "init") (param $n i32) (result i32) (i32.const 1024))
          (func (export "decode") (param $n i32) (result i32)
            (loop $spin (br $spin))
            (unreachable))
          (func (export "output") (param $max i32) (result i32) (i32.const 0))
          (func (export "output_ptr") (result i32) (i32.const 0))
          (func (export "finish") (result i32) (i32.const 0)))"#;

    /// DoD (b) variant: spins in the `start` section, so the fuel meter
    /// has to catch it during *instantiation*.
    const START_SPINNER: &str = r#"
        (module
          (start $spin)
          (memory (export "memory") 1)
          (func $spin (loop $l (br $l)))
          (func (export "init") (param $n i32) (result i32) (i32.const 1024))
          (func (export "decode") (param $n i32) (result i32) (i32.const 0))
          (func (export "output") (param $max i32) (result i32) (i32.const 0))
          (func (export "output_ptr") (result i32) (i32.const 0))
          (func (export "finish") (result i32) (i32.const 0)))"#;

    /// DoD (c): reaches for the host beyond the buffer protocol by
    /// importing WASI's `fd_write`. The host defines no imports and the
    /// WASI feature is not even compiled in, so the module is refused.
    const WASI_SNOOP: &str = r#"
        (module
          (import "wasi_snapshot_preview1" "fd_write"
            (func $fd_write (param i32 i32 i32 i32) (result i32)))
          (memory (export "memory") 1)
          (func (export "init") (param $n i32) (result i32) (i32.const 1024))
          (func (export "decode") (param $n i32) (result i32) (i32.const 0))
          (func (export "output") (param $max i32) (result i32) (i32.const 0))
          (func (export "output_ptr") (result i32) (i32.const 0))
          (func (export "finish") (result i32) (i32.const 0)))"#;

    /// DoD (c) variant: invents a private host backdoor.
    const BACKDOOR_SNOOP: &str = r#"
        (module
          (import "selis_host" "read_process_memory" (func $pwn (param i32) (result i32)))
          (memory (export "memory") 1)
          (func (export "init") (param $n i32) (result i32) (i32.const 1024))
          (func (export "decode") (param $n i32) (result i32) (i32.const 0))
          (func (export "output") (param $max i32) (result i32) (i32.const 0))
          (func (export "output_ptr") (result i32) (i32.const 0))
          (func (export "finish") (result i32) (i32.const 0)))"#;

    /// Protocol violation: reports an output pointer far outside linear
    /// memory.
    const FORGED_OUTPUT: &str = r#"
        (module
          (memory (export "memory") 1)
          (func (export "init") (param $n i32) (result i32) (i32.const 1024))
          (func (export "decode") (param $n i32) (result i32) (i32.const 0))
          (func (export "output") (param $max i32) (result i32) (i32.const 65536))
          (func (export "output_ptr") (result i32) (i32.const 2130706432))
          (func (export "finish") (result i32) (i32.const 0)))"#;

    /// Protocol violation: `init` refuses with the zero sentinel.
    const REFUSING_INIT: &str = r#"
        (module
          (memory (export "memory") 1)
          (func (export "init") (param $n i32) (result i32) (i32.const 0))
          (func (export "decode") (param $n i32) (result i32) (i32.const 0))
          (func (export "output") (param $max i32) (result i32) (i32.const 0))
          (func (export "output_ptr") (result i32) (i32.const 0))
          (func (export "finish") (result i32) (i32.const 0)))"#;

    /// Module-reported failure: the input is undecodable (status 1).
    const REJECTING_MODULE: &str = r#"
        (module
          (memory (export "memory") 1)
          (func (export "init") (param $n i32) (result i32) (i32.const 1024))
          (func (export "decode") (param $n i32) (result i32) (i32.const 1))
          (func (export "output") (param $max i32) (result i32) (i32.const 0))
          (func (export "output_ptr") (result i32) (i32.const 0))
          (func (export "finish") (result i32) (i32.const 0)))"#;

    /// The single-growth variant of DoD (a): one huge `memory.grow` is
    /// denied by the limiter (returns -1), and the module reports the
    /// failure through the protocol instead of looping.
    const ONE_SHOT_OVERGROW: &str = r#"
        (module
          (memory (export "memory") 1)
          (func (export "init") (param $n i32) (result i32) (i32.const 1024))
          (func (export "decode") (param $n i32) (result i32)
            (drop (memory.grow (i32.const 65536)))
            (i32.const 2))
          (func (export "output") (param $max i32) (result i32) (i32.const 0))
          (func (export "output_ptr") (result i32) (i32.const 0))
          (func (export "finish") (result i32) (i32.const 0)))"#;

    /// An oversized *declared* memory: the module is refused before any
    /// allocation happens.
    const HUGE_DECLARATION: &str = r#"
        (module
          (memory (export "memory") 4096)
          (func (export "init") (param $n i32) (result i32) (i32.const 1024))
          (func (export "decode") (param $n i32) (result i32) (i32.const 0))
          (func (export "output") (param $max i32) (result i32) (i32.const 0))
          (func (export "output_ptr") (result i32) (i32.const 0))
          (func (export "finish") (result i32) (i32.const 0)))"#;

    #[test]
    fn well_behaved_module_round_trips() {
        let clock = ManualClock::new();
        let mut guard = test_budget().guard_with(&clock, CancelToken::new());
        let mut codec = WasmCodec::new(&mut guard).expect("host builds");
        let input = b"selis-jpx-codestream";
        let out = codec.run_wat(WELL_BEHAVED, input).expect("decode succeeds");
        assert_eq!(out.status, Status::Ok);
        let reversed: Vec<u8> = input.iter().rev().copied().collect();
        assert_eq!(
            out.output, reversed,
            "the codec's copy-out must match the copy-in"
        );
    }

    #[test]
    fn budget_charges_track_the_copy_protocol() {
        let clock = ManualClock::new();
        let mut guard = test_budget().guard_with(&clock, CancelToken::new());
        let mut codec = WasmCodec::new(&mut guard).expect("host builds");
        codec.run_wat(WELL_BEHAVED, b"abcd").expect("succeeds");
        // input copy + output copy, nothing else (module memory is capped,
        // not charged).
        assert_eq!(guard.usage().bytes, 8);
        assert_eq!(guard.poisoned_by(), None);
    }

    #[test]
    fn unbounded_memory_is_stopped_by_the_fuel_meter() {
        let clock = ManualClock::new();
        let mut guard = test_budget().guard_with(&clock, CancelToken::new());
        let mut codec = WasmCodec::new(&mut guard).expect("host builds");
        let e = codec
            .run_wat(MEMORY_HOG, b"x")
            .expect_err("the memory hog must be contained");
        assert_eq!(e.code(), Code::SandboxFuel);
        // Containment, not catastrophe: the guard is still healthy.
        assert_eq!(guard.poisoned_by(), None);
        assert!(guard.charge(crate::Resource::Bytes, 1).is_ok());
    }

    #[test]
    fn a_single_oversized_growth_is_denied_to_the_module() {
        // The one-shot module reports status 2 after its `memory.grow` is
        // denied (the test module records the failure as status 2); the
        // host surfaces it as a module error, not a crash.
        let clock = ManualClock::new();
        let mut guard = test_budget().guard_with(&clock, CancelToken::new());
        let mut codec = WasmCodec::new(&mut guard).expect("host builds");
        let out = codec
            .run_wat(ONE_SHOT_OVERGROW, b"x")
            .expect("no trap: growth denial is -1, not a trap");
        assert_eq!(out.status, Status::InputTruncated);
        assert!(out.output.is_empty());
    }

    #[test]
    fn spin_forever_is_fuel_bounded_and_deterministic() {
        for _ in 0..2 {
            let clock = ManualClock::new();
            let mut guard = test_budget().guard_with(&clock, CancelToken::new());
            let mut codec = WasmCodec::new(&mut guard).expect("host builds");
            let e = codec
                .run_wat(SPINNER, b"x")
                .expect_err("an infinite loop must be interrupted");
            assert_eq!(e.code(), Code::SandboxFuel);
        }
    }

    #[test]
    fn spin_in_start_is_metered_during_instantiation() {
        let clock = ManualClock::new();
        let mut guard = test_budget().guard_with(&clock, CancelToken::new());
        let mut codec = WasmCodec::new(&mut guard).expect("host builds");
        let e = codec
            .run_wat(START_SPINNER, b"x")
            .expect_err("a spinning start section must be interrupted");
        assert_eq!(e.code(), Code::SandboxFuel);
    }

    #[test]
    fn wasi_imports_are_refused() {
        let clock = ManualClock::new();
        let mut guard = test_budget().guard_with(&clock, CancelToken::new());
        let mut codec = WasmCodec::new(&mut guard).expect("host builds");
        let e = codec
            .run_wat(WASI_SNOOP, b"x")
            .expect_err("no WASI, no filesystem");
        assert_eq!(e.code(), Code::SandboxImportDenied);
    }

    #[test]
    fn invented_host_backdoors_are_refused() {
        let clock = ManualClock::new();
        let mut guard = test_budget().guard_with(&clock, CancelToken::new());
        let mut codec = WasmCodec::new(&mut guard).expect("host builds");
        let e = codec
            .run_wat(BACKDOOR_SNOOP, b"x")
            .expect_err("no imports are provided at all");
        assert_eq!(e.code(), Code::SandboxImportDenied);
    }

    #[test]
    fn a_forged_output_pointer_is_a_protocol_error() {
        let clock = ManualClock::new();
        let mut guard = test_budget().guard_with(&clock, CancelToken::new());
        let mut codec = WasmCodec::new(&mut guard).expect("host builds");
        let e = codec
            .run_wat(FORGED_OUTPUT, b"x")
            .expect_err("a pointer outside linear memory must be rejected before the read");
        assert_eq!(e.code(), Code::SandboxProtocol);
    }

    #[test]
    fn an_init_refusal_is_a_module_error() {
        let clock = ManualClock::new();
        let mut guard = test_budget().guard_with(&clock, CancelToken::new());
        let mut codec = WasmCodec::new(&mut guard).expect("host builds");
        let e = codec
            .run_wat(REFUSING_INIT, b"x")
            .expect_err("init refused the input");
        assert_eq!(e.code(), Code::SandboxModuleError);
    }

    #[test]
    fn module_reported_failure_surfaces_its_status() {
        let clock = ManualClock::new();
        let mut guard = test_budget().guard_with(&clock, CancelToken::new());
        let mut codec = WasmCodec::new(&mut guard).expect("host builds");
        let out = codec.run_wat(REJECTING_MODULE, b"x").expect("contained");
        assert_eq!(out.status, Status::InputMalformed);
        assert!(out.output.is_empty());
    }

    #[test]
    fn an_oversized_memory_declaration_is_refused_before_instantiation() {
        let clock = ManualClock::new();
        let mut guard = test_budget().guard_with(&clock, CancelToken::new());
        let mut codec = WasmCodec::new(&mut guard).expect("host builds");
        // 4096 pages = 256 MiB, against a 1 MiB cap: refused with no
        // allocation at all.
        let e = codec
            .run_wat(HUGE_DECLARATION, b"x")
            .expect_err("declaration exceeds the cap");
        assert_eq!(e.code(), Code::SandboxMemoryCap);
        assert_eq!(guard.poisoned_by(), None);
    }

    #[test]
    fn an_expired_deadline_stops_the_run_at_the_first_boundary() {
        let clock = ManualClock::new();
        let mut guard = test_budget().guard_with(&clock, CancelToken::new());
        clock.advance(100_001);
        let e = WasmCodec::new(&mut guard).expect_err("the deadline is already past");
        assert_eq!(e.code(), Code::BudgetWall);
    }

    #[test]
    fn cancellation_lands_at_the_next_boundary() {
        let clock = ManualClock::new();
        let token = CancelToken::new();
        let mut guard = test_budget().guard_with(&clock, token.clone());
        let mut codec = WasmCodec::new(&mut guard).expect("host builds");
        token.cancel();
        let e = codec
            .run_wat(WELL_BEHAVED, b"x")
            .expect_err("a cancelled run must not execute");
        assert_eq!(e.code(), Code::Cancelled);
    }

    #[test]
    fn a_budget_that_cannot_back_one_page_is_refused() {
        let clock = ManualClock::new();
        let mut guard = Budget {
            bytes: 100,
            ..test_budget()
        }
        .guard_with(&clock, CancelToken::new());
        let e = WasmCodec::new(&mut guard).expect_err("no page, no codec");
        assert_eq!(e.code(), Code::BudgetBytes);
    }

    #[test]
    fn invalid_wat_is_a_module_error_without_execution() {
        let clock = ManualClock::new();
        let mut guard = test_budget().guard_with(&clock, CancelToken::new());
        let mut codec = WasmCodec::new(&mut guard).expect("host builds");
        let e = codec
            .run_wat("(module (memory 1) (func $f (i32.add)))", b"x")
            .expect_err("unbalanced wat");
        assert_eq!(e.code(), Code::SandboxModuleError);
    }

    #[test]
    fn the_host_survives_a_barrage_of_hostile_modules() {
        // DoD, final clause: after every containment above, the host is
        // still healthy and a well-behaved codec still succeeds.
        let clock = ManualClock::new();
        let mut guard = test_budget().guard_with(&clock, CancelToken::new());
        let mut codec = WasmCodec::new(&mut guard).expect("host builds");
        let hostile = [
            MEMORY_HOG,
            SPINNER,
            START_SPINNER,
            WASI_SNOOP,
            BACKDOOR_SNOOP,
            FORGED_OUTPUT,
            REFUSING_INIT,
        ];
        for wat in hostile {
            let result = codec.run_wat(wat, b"hostile");
            assert!(result.is_err(), "module must be contained");
        }
        let out = codec
            .run_wat(WELL_BEHAVED, b"still alive")
            .expect("host intact");
        assert_eq!(out.status, Status::Ok);
        assert_eq!(out.output, b"evila llits");
    }

    #[test]
    fn empty_input_and_empty_output_round_trip() {
        let clock = ManualClock::new();
        let mut guard = test_budget().guard_with(&clock, CancelToken::new());
        let mut codec = WasmCodec::new(&mut guard).expect("host builds");
        let out = codec
            .run_wat(WELL_BEHAVED, b"")
            .expect("empty decode is legal");
        assert_eq!(out.status, Status::Ok);
        assert!(out.output.is_empty());
    }

    #[test]
    fn input_beyond_the_protocol_length_is_refused() {
        let e = protocol_len((i32::MAX as usize) + 1).expect_err("beyond i32");
        assert_eq!(e.code(), Code::SandboxProtocol);
        assert_eq!(protocol_len(0).expect("zero is legal"), 0);
    }
}
