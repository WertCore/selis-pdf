//! The native slice: a budgeted QuickJS runtime with a whitelisted Acrobat API.
//!
//! The engine's default feature set has no module loader, no native modules,
//! no threads, no clock, no filesystem and no network. The only functions we
//! install are the ones listed in the crate docs; everything else is absent.

use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::rc::Rc;

use rquickjs::function::Rest;
use rquickjs::object::Accessor;
use rquickjs::{Coerced, Context, Function, Object, Runtime, Value};
use selis_error::{err, Code, Result};
use selis_policy::{DocumentIdentity, ScriptAuthorization};

use crate::budget::{JsBudget, JsUsage};
use crate::forbidden::find_forbidden_api;
use crate::types::{EventKind, FieldUpdate, JsOutcome, JsRequest};

/// Per-run state shared with the installed API closures.
#[derive(Debug, Default)]
struct SharedState {
    fields: BTreeMap<String, String>,
    event_value: String,
    event_value_set: bool,
    event_rc: bool,
}

/// A budgeted QuickJS interpreter for form scripts (SL-5.JS.01).
///
/// One instance is reused across runs: each [`JsSandbox::run`] resets the field
/// table and event, installs a fresh interrupt budget, and evaluates exactly one
/// script. The document is never modified; results are returned for the caller
/// to apply (ADR-P0020).
pub struct JsSandbox {
    runtime: Runtime,
    ctx: Context,
    state: Rc<RefCell<SharedState>>,
}

impl JsSandbox {
    /// Create a native QuickJS runtime and install the whitelisted API.
    ///
    /// # Errors
    ///
    /// `JS_RUNTIME_ERROR` if the engine or its API cannot be initialised.
    pub fn new() -> Result<Self> {
        let runtime = Runtime::new().map_err(|_| setup_error())?;
        let ctx = Context::full(&runtime).map_err(|_| setup_error())?;
        let state = Rc::new(RefCell::new(SharedState::default()));
        let sandbox = Self {
            runtime,
            ctx,
            state,
        };
        sandbox.install_api()?;
        Ok(sandbox)
    }

    /// Run one script, gated by reader consent / admin origin policy (SL-5.JS.02/03).
    ///
    /// This is the entry point a native shell should call. It refuses before
    /// touching the interpreter when the gate denies, so a script cannot run
    /// without consent.
    ///
    /// # Budget
    ///
    /// The supplied [`JsBudget`] bounds instruction-proxy ticks, heap, stack,
    /// script size, field count and string size. It is validated before any
    /// engine work.
    ///
    /// # Malformed Input
    ///
    /// Empty or over-long scripts, duplicate or over-long fields, interior NUL
    /// bytes and forbidden API names are rejected with typed errors before
    /// evaluation. No script text is echoed into a diagnostic (ADR-P0017).
    ///
    /// # Errors
    ///
    /// `JS_CONSENT_REQUIRED`, or any error from [`JsSandbox::run`].
    pub fn run_authorized(
        &mut self,
        request: JsRequest,
        budget: JsBudget,
        gate: &ScriptAuthorization<'_>,
        doc: &DocumentIdentity,
    ) -> Result<JsOutcome> {
        gate.authorize(doc)?;
        self.run(request, budget)
    }

    /// Run one script directly. Callers must have gated it (prefer
    /// [`JsSandbox::run_authorized`]).
    ///
    /// # Budget
    ///
    /// All limits in [`JsBudget`] are enforced. An infinite loop trips the
    /// interrupt handler and returns `JS_INSTRUCTION_BUDGET`; heap exhaustion
    /// returns `JS_MEMORY_BUDGET`.
    ///
    /// # Malformed Input
    ///
    /// Script and field data are validated in constant time before evaluation.
    /// Document-derived text never reaches an error message.
    ///
    /// # Errors
    ///
    /// `JS_SCRIPT_REJECTED`, `JS_API_REFUSED`, `JS_INSTRUCTION_BUDGET`,
    /// `JS_MEMORY_BUDGET` or `JS_RUNTIME_ERROR`.
    pub fn run(&mut self, request: JsRequest, budget: JsBudget) -> Result<JsOutcome> {
        budget.validate()?;
        validate_request(&request, &budget)?;
        if let Some(api) = find_forbidden_api(&request.script) {
            return Err(err!(Code::JsApiRefused, during = "js-scan", detail = api));
        }

        self.reset_state(&request);
        self.set_event_fields(&request)?;

        let ticks = Rc::new(Cell::new(0u64));
        let tripped = Rc::new(Cell::new(false));
        let tick_counter = Rc::clone(&ticks);
        let trip_flag = Rc::clone(&tripped);
        let limit = budget.max_ticks;
        self.runtime.set_interrupt_handler(Some(Box::new(move || {
            let seen = tick_counter.get().saturating_add(1);
            tick_counter.set(seen);
            if seen > limit {
                trip_flag.set(true);
                true
            } else {
                false
            }
        })));
        self.runtime.set_memory_limit(budget.max_memory_bytes);
        self.runtime.set_max_stack_size(budget.max_stack_bytes);

        let evaluated: rquickjs::Result<()> = self.ctx.with(|ctx| -> rquickjs::Result<()> {
            let value = ctx.eval::<Value<'_>, _>(request.script.as_bytes())?;
            drop(value);
            Ok(())
        });

        let usage = JsUsage {
            ticks: ticks.get(),
            memory_bytes: usize::try_from(self.runtime.memory_usage().memory_used_size)
                .unwrap_or(usize::MAX),
            interrupted: tripped.get(),
        };

        if let Err(js_err) = evaluated {
            if tripped.get() {
                return Err(err!(
                    Code::JsInstructionBudget,
                    during = "js-interrupt",
                    detail = "form script instruction budget exhausted"
                ));
            }
            if matches!(js_err, rquickjs::Error::Allocation) {
                return Err(err!(
                    Code::JsMemoryBudget,
                    during = "js-heap",
                    detail = "form script heap budget exhausted"
                ));
            }
            return Err(err!(
                Code::JsRuntimeError,
                during = "js-eval",
                detail = "form script raised an uncaught exception"
            ));
        }

        self.collect_outcome(&request, usage)
    }

    fn reset_state(&self, request: &JsRequest) {
        let mut state = self.state.borrow_mut();
        state.fields.clear();
        for field in &request.fields {
            let _ = state.fields.insert(field.name.clone(), field.value.clone());
        }
        state.event_value = match request.event {
            EventKind::Calculate => String::new(),
            EventKind::Validate | EventKind::Format => state
                .fields
                .get(&request.target)
                .cloned()
                .unwrap_or_default(),
        };
        state.event_value_set = false;
        state.event_rc = true;
    }

    fn set_event_fields(&self, request: &JsRequest) -> Result<()> {
        self.ctx.with(|ctx| -> Result<()> {
            let globals = ctx.globals();
            let event: Object<'_> = globals.get("event").map_err(|_| setup_error())?;
            // `event.value` is an accessor over SharedState (installed once in
            // `install_api`); reset_state seeded it, so setting it here would
            // falsely mark it as script-written.
            event
                .set("type", request.event.as_str())
                .map_err(|_| setup_error())?;
            event
                .set("targetName", request.target.clone())
                .map_err(|_| setup_error())?;
            event.set("rc", true).map_err(|_| setup_error())?;
            Ok(())
        })
    }

    fn collect_outcome(&self, request: &JsRequest, usage: JsUsage) -> Result<JsOutcome> {
        let (event_value, event_value_set, event_rc, updates) = self.ctx.with(|ctx| {
            let globals = ctx.globals();
            let event: Object<'_> = match globals.get("event") {
                Ok(value) => value,
                Err(_) => return (String::new(), false, true, Vec::new()),
            };
            let event_value = match event.get::<_, Coerced<String>>("value") {
                Ok(value) => value.0,
                Err(_) => String::new(),
            };
            let event_rc = match event.get::<_, Coerced<bool>>("rc") {
                Ok(value) => value.0,
                Err(_) => true,
            };
            let state = self.state.borrow();
            let event_value_set = state.event_value_set;
            let mut updates = Vec::new();
            for field in &request.fields {
                if let Some(current) = state.fields.get(&field.name) {
                    if current != &field.value {
                        updates.push(FieldUpdate {
                            name: field.name.clone(),
                            value: current.clone(),
                        });
                    }
                }
            }
            (event_value, event_value_set, event_rc, updates)
        });
        Ok(JsOutcome {
            event_value: Some(event_value),
            event_value_set,
            event_rc,
            updates,
            usage,
        })
    }

    fn install_api(&self) -> Result<()> {
        self.ctx.with(|ctx| -> Result<()> {
            let globals = ctx.globals();

            // `event` — the calculation/validate/format event object. Its
            // `value` is an accessor over the run state so we can tell a script
            // that actually assigned it apart from one that left the seeded
            // value untouched (SL-5.FORM.03). `rc` is a plain property.
            let event = Object::new(ctx.clone()).map_err(|_| setup_error())?;
            event.set("type", "").map_err(|_| setup_error())?;
            event.set("targetName", "").map_err(|_| setup_error())?;
            let getter_state = Rc::clone(&self.state);
            let setter_state = Rc::clone(&self.state);
            event
                .prop(
                    "value",
                    Accessor::new(
                        move || -> rquickjs::Result<String> {
                            Ok(getter_state.borrow().event_value.clone())
                        },
                        move |value: Coerced<String>| -> rquickjs::Result<()> {
                            let mut shared = setter_state.borrow_mut();
                            shared.event_value = value.0;
                            shared.event_value_set = true;
                            Ok(())
                        },
                    ),
                )
                .map_err(|_| setup_error())?;
            event.set("rc", true).map_err(|_| setup_error())?;
            globals.set("event", event).map_err(|_| setup_error())?;

            // `this.getField(name)` — at the top level of a sloppy-mode script
            // `this` is `globalThis`, so installing `getField` on the globals
            // is what makes `this.getField(name)` resolve.
            let state = Rc::clone(&self.state);
            let get_field =
                Function::new(ctx.clone(), make_get_field(state)).map_err(|_| setup_error())?;
            globals
                .set("getField", get_field)
                .map_err(|_| setup_error())?;

            // `util.printf` / `util.printd` — deterministic, string-only.
            let util = Object::new(ctx.clone()).map_err(|_| setup_error())?;
            let printf = Function::new(
                ctx.clone(),
                |fmt: Coerced<String>, args: Rest<Coerced<String>>| {
                    let values: Vec<String> = args.0.into_iter().map(|a| a.0).collect();
                    printf(&fmt.0, &values)
                },
            )
            .map_err(|_| setup_error())?;
            util.set("printf", printf).map_err(|_| setup_error())?;
            let printd = Function::new(
                ctx.clone(),
                |fmt: Coerced<String>, date: Coerced<String>| printd(&fmt.0, &date.0),
            )
            .map_err(|_| setup_error())?;
            util.set("printd", printd).map_err(|_| setup_error())?;
            globals.set("util", util).map_err(|_| setup_error())?;

            // AF* helpers: present under their whitelisted names, implemented
            // as documented pass-throughs (no external side effects).
            for name in AF_FUNCTIONS {
                let noop = Function::new(ctx.clone(), || -> rquickjs::Result<()> { Ok(()) })
                    .map_err(|_| setup_error())?;
                globals.set(*name, noop).map_err(|_| setup_error())?;
            }
            Ok(())
        })
    }
}

/// The AF* format/validate helpers installed in the sandbox.
pub const AF_FUNCTIONS: &[&str] = &[
    "AFNumber_Format",
    "AFNumber_Keystroke",
    "AFPercent_Format",
    "AFPercent_Keystroke",
    "AFDate_FormatEx",
    "AFDate_KeystrokeEx",
    "AFRange_Validate",
    "AFSimple_Validate",
];

/// Build the `this.getField(name)` closure with an explicit context lifetime,
/// so the returned field object is tied to the same context as the `Ctx` param.
fn make_get_field<'js>(
    state: Rc<RefCell<SharedState>>,
) -> impl Fn(rquickjs::Ctx<'js>, Coerced<String>) -> rquickjs::Result<Option<Object<'js>>> + 'js {
    move |ctx: rquickjs::Ctx<'js>, name: Coerced<String>| -> rquickjs::Result<Option<Object<'js>>> {
        let key = name.0;
        let exists = {
            let shared = state.borrow();
            shared.fields.contains_key(&key)
        };
        if !exists {
            return Ok(None);
        }
        let object = Object::new(ctx.clone())?;
        object.set("name", key.clone())?;
        let getter_state = Rc::clone(&state);
        let setter_state = Rc::clone(&state);
        let getter_key = key.clone();
        let setter_key = key;
        object.prop(
            "value",
            Accessor::new(
                move || -> rquickjs::Result<String> {
                    let shared = getter_state.borrow();
                    Ok(shared.fields.get(&getter_key).cloned().unwrap_or_default())
                },
                move |value: Coerced<String>| -> rquickjs::Result<()> {
                    let mut shared = setter_state.borrow_mut();
                    if let Some(slot) = shared.fields.get_mut(&setter_key) {
                        *slot = value.0;
                    }
                    Ok(())
                },
            ),
        )?;
        Ok(Some(object))
    }
}

fn setup_error() -> selis_error::Error {
    err!(
        Code::JsRuntimeError,
        during = "js-setup",
        detail = "native interpreter setup failed"
    )
}

fn validate_request(request: &JsRequest, budget: &JsBudget) -> Result<()> {
    if request.script.is_empty()
        || request.script.len() > budget.max_script_bytes
        || request.script.as_bytes().contains(&0)
        || request.target.len() > budget.max_string_bytes
        || request.fields.len() > budget.max_fields
    {
        return Err(err!(
            Code::JsScriptRejected,
            during = "js-validate",
            detail = "script or request outside budget"
        ));
    }
    let mut seen = BTreeMap::new();
    for field in &request.fields {
        if field.name.is_empty()
            || field.name.len() > budget.max_string_bytes
            || field.value.len() > budget.max_string_bytes
            || field.name.as_bytes().contains(&0)
            || field.value.as_bytes().contains(&0)
            || seen.insert(&field.name, ()).is_some()
        {
            return Err(err!(
                Code::JsScriptRejected,
                during = "js-validate",
                detail = "invalid or duplicate field"
            ));
        }
    }
    Ok(())
}

/// A minimal `util.printf` supporting `%%`, `%s`, `%d`/`%i` and `%f` (with
/// `.N` precision). Anything else is emitted verbatim.
fn printf(fmt: &str, args: &[String]) -> String {
    let bytes = fmt.as_bytes();
    let mut out = String::new();
    let mut i = 0usize;
    let mut arg_index = 0usize;
    while let Some(&byte) = bytes.get(i) {
        if byte != b'%' {
            if let Some(chunk) = fmt.get(i..i.saturating_add(1)) {
                out.push_str(chunk);
            }
            i = i.saturating_add(1);
            continue;
        }
        if bytes.get(i.saturating_add(1)) == Some(&b'%') {
            out.push('%');
            i = i.saturating_add(2);
            continue;
        }
        let mut cursor = i.saturating_add(1);
        let mut spec = None;
        while let Some(&candidate) = bytes.get(cursor) {
            if candidate == b's' || candidate == b'd' || candidate == b'i' || candidate == b'f' {
                spec = Some(candidate);
                break;
            }
            if candidate == b'%' {
                break;
            }
            cursor = cursor.saturating_add(1);
        }
        let Some(code) = spec else {
            out.push('%');
            i = i.saturating_add(1);
            continue;
        };
        let value = args.get(arg_index).map_or("", String::as_str);
        arg_index = arg_index.saturating_add(1);
        match code {
            b's' => out.push_str(value),
            b'd' | b'i' => {
                let number = value.parse::<f64>().unwrap_or(0.0);
                out.push_str(&format!("{:.0}", number.trunc()));
            }
            b'f' => {
                let flags = fmt.get(i.saturating_add(1)..cursor).unwrap_or("");
                let precision = flags
                    .rsplit_once('.')
                    .and_then(|(_, digits)| digits.parse::<usize>().ok())
                    .unwrap_or(6)
                    .min(12);
                let number = value.parse::<f64>().unwrap_or(0.0);
                out.push_str(&format!("{number:.precision$}"));
            }
            _ => {}
        }
        i = cursor.saturating_add(1);
    }
    out
}

/// A minimal `util.printd`: replaces `yyyy`, `mm`, `dd` in the format using an
/// ISO-like date string passed as the argument. No clock is read.
fn printd(fmt: &str, date: &str) -> String {
    let parts: Vec<&str> = date.split(['-', 'T', ':']).collect();
    let year = parts.first().copied().unwrap_or("");
    let month = parts.get(1).copied().unwrap_or("");
    let day = parts.get(2).copied().unwrap_or("");
    let bytes = fmt.as_bytes();
    let mut out = String::new();
    let mut i = 0usize;
    while let Some(&_byte) = bytes.get(i) {
        if fmt.get(i..i.saturating_add(4)) == Some("yyyy") {
            out.push_str(year);
            i = i.saturating_add(4);
        } else if fmt.get(i..i.saturating_add(2)) == Some("mm") {
            out.push_str(month);
            i = i.saturating_add(2);
        } else if fmt.get(i..i.saturating_add(2)) == Some("dd") {
            out.push_str(day);
            i = i.saturating_add(2);
        } else {
            if let Some(chunk) = fmt.get(i..i.saturating_add(1)) {
                out.push_str(chunk);
            }
            i = i.saturating_add(1);
        }
    }
    out
}

#[cfg(test)]
mod unit_tests {
    use super::*;

    #[test]
    fn printf_handles_common_specifiers() {
        assert_eq!(printf("Total: %s", &["42".to_string()]), "Total: 42");
        assert_eq!(
            printf("%d / %d", &["3.9".to_string(), "2".to_string()]),
            "3 / 2"
        );
        assert_eq!(printf("%%%.2f", &["1.005".to_string()]), "%1.00");
        assert_eq!(printf("no args", &[]), "no args");
    }

    #[test]
    fn printd_uses_only_the_supplied_date() {
        assert_eq!(printd("yyyy-mm-dd", "2026-11-19"), "2026-11-19");
        assert_eq!(printd("dd/mm/yyyy", "2026-11-19"), "19/11/2026");
    }
}
