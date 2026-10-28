//! The real-browser harness (`cargo xtask browser-check`, WEB.02).
//!
//! # The gap this closes
//!
//! Some DoDs are claims only a browser engine can settle, and until now the
//! repo had no committed way to run one, so they could only be checked by hand
//! and stayed unticked. SL-4.WEB.02 is the sharpest case: its status note
//! lists "real `install`/`activate` event delivery, `clients.claim()`,
//! browser-enforced `respondWith` semantics, real Cache Storage quota
//! behaviour" as unverified, because `apps/web/host` drives its worker in Node
//! against a Cache Storage double (ADR-P0021 keeps the JS tree dependency-free).
//! This harness is the missing half: a page, in a real engine, on a real
//! origin.
//!
//! # The mechanism, and why this one
//!
//! The repo already has two working browser-driving precedents and this
//! combines the parts of each that were needed:
//!
//! * `xtask/bench/run.mjs` serves the page from `127.0.0.1` and lets the page
//!   **POST its result back**, because `--dump-dom` snapshots at load
//!   completion and cannot observe anything that takes real time.
//! * `xtask/src/wasm_browser.rs` (SL-4.WASM.08) drives `--headless --dump-dom`
//!   and reads a marker out of the dumped DOM — but only because its page is
//!   fully synchronous.
//!
//! A service worker is not synchronous: registration is a real network fetch of
//! the worker script, and `clients.claim()` is a real round trip to the
//! browser. Measured on the development machine while building this: with
//! `--dump-dom --virtual-time-budget=15000`, the virtual clock runs to expiry
//! while the worker is still being fetched and the page is dumped still
//! reading `PENDING` — a green light for a browser that proved nothing. So the
//! **POST-back** path is the one that is load-bearing here, and the wait is a
//! wall-clock timeout rather than a virtual one.
//!
//! The engine is an external tool, discovered at runtime and never vendored:
//! no `Cargo.toml` entry, no `pnpm-lock.yaml` change, no browser download
//! (ADR-P0009, ADR-P0021). This is the same shape as `qpdf` in the
//! write05-oracle job. Microsoft Edge is the default engine for the same
//! reason it is there: a Chromium fork, preinstalled on the machines this is
//! developed on, so the gate needs no download step.
//!
//! # What a green run does and does not mean
//!
//! Each check under `xtask/browser/` is a committed page plus a Rust verdict
//! over what it reported. The pages here assert **browser behaviours** — that
//! this engine delivers worker lifecycle events, hands the page a controller,
//! and enforces `respondWith`. They do **not** assert that
//! `apps/web/host/public/sw.js` is correct; that is a Node suite's job and it
//! stays there. Pointing this harness at the built web app is the obvious next
//! step and is deliberately not taken here (see WEB.02's status note).
//!
//! # The fail-loud rules
//!
//! * **No engine is a skip, not a pass and not a failure.** This runs in
//!   `cargo test` on a developer machine, and a machine without Edge must
//!   still build. The skip is printed loudly, names every path probed, and is
//!   recorded in the report as `"skipped": true` so a green run can never be
//!   mistaken for a measured one. `--strict` turns it into a failure, for the
//!   runner that is expected to have an engine.
//! * **Zero checks reporting is a failure**, never a green run — the
//!   SL-3.CONF.06 rule, for the same reason the two `render-conf` legs could
//!   stay green while measuring nothing.
//! * **A page that never reports is a failure of that check**, not a skip: a
//!   browser that cannot finish a check is a browser fact, and a silent one is
//!   the failure mode this repo has already paid for once.
//!
//! # Two ways this went wrong first, measured here
//!
//! Both were found by running it, and both are commented at their fix, because
//! each one produced a *green-looking* harness that proved nothing:
//!
//! * A fixed `--user-data-dir` per check. The engine does not always keep the
//!   process it was launched as, so `Child::kill` leaves a live one holding the
//!   profile lock — and every later run then handed off to that zombie and
//!   loaded nothing. See [`unique_profile`].
//! * Serving the pages and the profile out of the repository's `target/`. A
//!   browser profile is thousands of small files, and writing one there was
//!   slow enough that the engine had not finished starting inside a 30 s
//!   budget, while the identical check passed from `%TEMP%` on the same
//!   machine seconds later. See [`scratch_dir`].

use std::io::{ErrorKind, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::{json, Value as Json};

use crate::wasm_browser::{browser_candidates, browser_name};

/// The path a page POSTs its JSON verdict to.
///
/// A path no committed asset can occupy, and one that is checked before the
/// filesystem is consulted, so a check can never be mistaken for a result.
const RESULT_PATH: &str = "/__selis_result";

/// Control endpoint that flips the origin into its offline state.
const OFFLINE_PATH: &str = "/__selis_offline";

/// How long to wait for one check's page to report, unless `--timeout-ms` says
/// otherwise. Also the subcommand's `--timeout-ms` default, so the number is
/// written once.
///
/// A cold engine start on the development machine is a few seconds; a service
/// worker register-plus-claim-plus-fetch is a few more. Generous enough not to
/// flake, short enough that a hung engine fails inside `cargo test` instead of
/// hanging it.
pub(crate) const DEFAULT_TIMEOUT_MS: u64 = 30_000;

/// Grace between the verdict arriving and the engine being killed, so the
/// browser can finish the response instead of being torn down mid-write.
/// `xtask/bench/run.mjs` takes the same pause.
const SHUTDOWN_GRACE_MS: u64 = 150;

/// The subcommand's inputs.
pub(crate) struct Config {
    /// Explicit engine binary (`--browser`).
    pub(crate) browser: Option<PathBuf>,
    /// Run this page instead of the committed check set (`--page`).
    pub(crate) page: Option<PathBuf>,
    /// Run only this check id (`--only`).
    pub(crate) only: Option<String>,
    /// Wall-clock milliseconds to wait for a page to report (`--timeout-ms`).
    pub(crate) timeout_ms: u64,
    /// Report JSON path (`--out`); also the CI artifact.
    pub(crate) out: PathBuf,
    /// Keep the served directory and the engine profile for inspection.
    pub(crate) keep: bool,
    /// Fail instead of skipping when no engine is found.
    pub(crate) strict: bool,
}

/// How one check ended.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Status {
    /// The page reported, and the verdict accepted what it reported.
    Pass,
    /// The page reported, and the verdict rejected it.
    Fail,
    /// The page never reported inside the timeout. Not a pass, and not a skip
    /// either: a browser that cannot finish a check is a browser fact.
    Unmeasured,
}

impl Status {
    /// The name used in the report and on the console.
    fn name(self) -> &'static str {
        match self {
            Status::Pass => "pass",
            Status::Fail => "fail",
            Status::Unmeasured => "unmeasured",
        }
    }
}

/// One check's outcome, and the report it produced.
struct Row {
    /// The check id (or the ad-hoc page's file name).
    id: String,
    /// How it ended.
    status: Status,
    /// Why — the verdict's complaint, or the harness's own error.
    detail: String,
    /// What the page reported, if it reported at all.
    report: Option<Json>,
}

/// One committed check: a directory of files to serve, and the verdict applied
/// to what its page reports.
struct Check {
    /// Stable id: the command line, the report, and any failure name it.
    id: &'static str,
    /// Directory name under `xtask/browser/`.
    dir: &'static str,
    /// What the page's report must show for the check to pass.
    verdict: fn(&Json) -> Result<(), String>,
}

/// The committed checks, in report order.
///
/// Six now. `service-worker` is the WEB.02 payload: real lifecycle event
/// delivery, `clients.claim()`, and browser-enforced `respondWith`, over a real
/// origin. `async-wasm` is the cheap proof that this harness itself works end
/// to end — a page whose verdict is only written after an awaited
/// `WebAssembly.instantiate` of a module committed beside it, run by a
/// different code generator than the native engine. It costs a second or two,
/// so unlike SL-4.WASM.08's 25-minute leg it can run on every commit.
/// `app-shell` proves the app's own worker; `chrome` proves the stage 3 React
/// screen against the built UI and token bridge. Both need the host build, so
/// they run last. `tools-page` drives the real `/tools.html` through an online
/// merge and an offline reload, reads the downloaded Blob bytes both times, and
/// checks the page's no-upload assertion.
/// `extension` (SL-4.UI.06) is the only one that loads an unpacked extension;
/// it needs the extension build and a fixed key so the launcher can name the
/// page it opens.
const CHECKS: [Check; 6] = [
    Check {
        id: "service-worker",
        dir: "service-worker",
        verdict: verdict_service_worker,
    },
    Check {
        id: "async-wasm",
        dir: "async-wasm",
        verdict: verdict_async_wasm,
    },
    // WEB.02's remaining step, and the one that needed the real app: the check
    // above proves the ENGINE, this proves the APP'S OWN worker. Run last,
    // because it is the only check that needs `apps/web/host` to have been
    // built (see `stage_app_shell`).
    Check {
        id: "app-shell",
        dir: "app-shell",
        verdict: verdict_app_shell,
    },
    // The UI.14 stage 3 screen. It shares `stage_app` and the axe staging with
    // `app-shell`, but its page imports the built chrome entry from
    // `/assets/ui/chrome/index.js` and exercises the real component library.
    Check {
        id: "chrome",
        dir: "chrome",
        verdict: verdict_chrome,
    },
    // The tool-first product surface (SL-4.TOOLS.02 + SL-4.TOOLS.05). It stages
    // the same built app as `app-shell`/`chrome`, opens the real `/tools.html`
    // in a same-origin frame, and drives a merge end to end. Then the harness
    // cuts the origin, reloads the page, and merges again from the worker's
    // cache. The downloaded Blob is fetched back and its bytes checked both
    // times, so "a result appeared" is not enough.
    Check {
        id: "tools-page",
        dir: "tools-page",
        verdict: verdict_tools_page,
    },
    // SL-4.UI.06's extension leg. It stages the packed extension as an
    // unpacked one instead of serving a page, and it is opened through a
    // loopback launcher rather than at the extension URL directly; both are
    // explained at `stage_extension`.
    Check {
        id: "extension",
        dir: "extension",
        verdict: verdict_extension,
    },
];

/// The body `xtask/browser/service-worker/asset.txt` is committed to hold.
///
/// Checked by a unit test against the file itself, so the constant and the
/// asset cannot drift apart into a check that passes on the wrong bytes.
const ASSET_BODY: &str = "selis-browser-harness-precached-asset";

/// What `xtask/browser/async-wasm/probe.wat` computes, and what the page must
/// therefore report. `20 * 2 + 1 * 2`, with the imported function doubling
/// each argument.
const WASM_PROBE_SUM: i64 = 42;

/// Schema version of a page's verdict document. Bump on any shape change; the
/// host refuses a version it does not know rather than guessing.
const SCHEMA: u32 = 1;

/// The public key the extension check stages into `manifest.json`.
///
/// Chromium derives an unpacked extension's id from this key (SHA-256 over the
/// DER bytes, first 16 bytes hex-mapped `a`..`p`), and an unpacked extension
/// without one gets an id from the profile path — which changes every run. The
/// launcher page has to name the id in a URL *before* the extension can answer
/// anything, so a fixed key is what makes that URL knowable. It is a public
/// key for a test-only extension and signs nothing; no private half is used or
/// committed anywhere.
///
/// The key and [`EXTENSION_ID`] are a pair. Changing one without the other
/// sends the launcher at a page that does not exist, and the check records
/// `unmeasured` with no viewer loaded — which is honest, but slow to diagnose.
const EXTENSION_KEY: &str = "MIIBIjANBgkqhkiG9w0BAQEFAAOCAQ8AMIIBCgKCAQEAu53Cglv8Zrob4k2VgkWwZ42pfF67TGS4sSdontZVzJBRuL3zat1TLwiUM611rL0iuZx0cJA+dwPTwfAzxVydKWv0yJj7ZiwcLt7j6bo4mJUJTPT9uUu4RYqwEKEZKz/FBLeVDAu/sV78iYe7zrtEuhfYE9+dEfRq+R8XBqCzpTkkBNGtXRncaK3DI9VyrVz2McxJ1IG2u0SO07a06uwahxpgIpsz6PWC32C5wH3TM7GoOqg3lU+vn4a4S5LFskoQHVP3382X5SjWCeakvy6/ZO5vN3tshh+2hjU0En59gFBROv2GECHalhTO57gmxnjLUJfLiOR2DyjScsbzlSMQuwIDAQAB";

/// The id [`EXTENSION_KEY`] produces. See its docs for why this is fixed.
const EXTENSION_ID: &str = "ppekgfikbjkfgckpcjibplnhhaaonaem";

/// The most request head the origin will buffer before giving up on a client.
/// A request line plus headers is hundreds of bytes; this is the point at which
/// something is not a browser talking to us.
const MAX_REQUEST_HEAD: usize = 64 * 1024;

/// Check the parts of a report that are the same for every check: the schema
/// version, and the page's own fatal error.
///
/// The version is checked rather than assumed, so a page that grows a field
/// and bumps the schema fails here with a name, instead of being read by a
/// verdict written for the old shape. A page that threw has proved nothing,
/// whatever else it managed to set, so its words are surfaced rather than a
/// generic failure.
fn well_formed(report: &Json) -> Result<(), String> {
    match report.get("schema").and_then(Json::as_u64) {
        Some(version) if version == u64::from(SCHEMA) => {}
        Some(other) => {
            return Err(format!(
                "the page reported schema {other}, this harness speaks {SCHEMA}"
            ))
        }
        None => return Err(format!("the page reported no schema (report: {report})")),
    }
    match report.get("fatal") {
        Some(Json::String(why)) => Err(format!("the page reported a fatal: {why}")),
        _ => Ok(()),
    }
}

/// Read a string field, naming it when it is absent or not a string.
fn string_field(report: &Json, key: &str) -> Result<String, String> {
    report
        .get(key)
        .and_then(Json::as_str)
        .map(str::to_string)
        .ok_or_else(|| format!("the page reported no `{key}` (report: {report})"))
}

/// Read a boolean field, naming it when absent or not a boolean.
fn bool_field(report: &Json, key: &str) -> Result<bool, String> {
    report
        .get(key)
        .and_then(Json::as_bool)
        .ok_or_else(|| format!("the page reported no boolean `{key}` (report: {report})"))
}

/// Read a boolean inside a named object.
fn nested_bool(report: &Json, object: &str, key: &str) -> Result<bool, String> {
    let value = report
        .get(object)
        .and_then(Json::as_object)
        .ok_or_else(|| format!("the page reported no `{object}` object (report: {report})"))?;
    value
        .get(key)
        .and_then(Json::as_bool)
        .ok_or_else(|| format!("`{object}` reported no boolean `{key}` (report: {report})"))
}

/// `chrome`: React, `@wertkit/ui`, the selis token bridge and UI.02's page list
/// are one live screen in a real engine.
///
/// The page exercises each component family through its public DOM surface, and
/// scans light, dark and high-contrast appearances with axe-core. This verdict
/// refuses a report that merely rendered: every control has to have changed the
/// document or the visible status, the dialog has to trap and restore focus,
/// and the page list has to carry UI.02's geometry and labels.
fn verdict_chrome(report: &Json) -> Result<(), String> {
    well_formed(report)?;
    if string_field(report, "check")? != "chrome" {
        return Err("the page did not identify itself as the chrome check".into());
    }
    if !bool_field(report, "mounted")? {
        return Err("the chrome never mounted in the engine".into());
    }
    for (object, key) in [
        ("landmarks", "header"),
        ("landmarks", "main"),
        ("landmarks", "nav"),
        ("focus", "dialogTrapped"),
        ("focus", "dialogRestored"),
    ] {
        if !nested_bool(report, object, key)? {
            return Err(format!(
                "`{object}.{key}` was false; the check did not observe it"
            ));
        }
    }
    if !bool_field(report, "bridge")? {
        return Err(
            "the `--wk-*` bridge produced no value for `--wk-bg`; the chrome is unstyled".into(),
        );
    }
    if !bool_field(report, "densityChanged")? {
        return Err(
            "compact density did not change the rendered control height; the token layer is \
             not reaching the components"
                .into(),
        );
    }

    let page_list = report
        .get("pageList")
        .and_then(Json::as_object)
        .ok_or_else(|| format!("the page reported no `pageList` object (report: {report})"))?;
    let tiles = page_list.get("tiles").and_then(Json::as_u64).unwrap_or(0);
    if tiles == 0 {
        return Err("UI.02's page list rendered zero page tiles".into());
    }
    let labels = page_list.get("labels").and_then(Json::as_u64).unwrap_or(0);
    if labels == 0 {
        return Err("the page tiles had no visible labels".into());
    }
    if page_list
        .get("current")
        .and_then(Json::as_str)
        .unwrap_or_default()
        .is_empty()
    {
        return Err("no page tile carried aria-current=\"page\"".into());
    }
    for key in ["positioned", "hasGeometry"] {
        if page_list.get(key).and_then(Json::as_bool) != Some(true) {
            return Err(format!(
                "UI.02's tile geometry was not applied (`pageList.{key}` was not true)"
            ));
        }
    }

    for key in [
        "button", "checkbox", "switch", "dialog", "menu", "tooltip", "tabs", "select", "toast",
    ] {
        if !nested_bool(report, "controls", key)? {
            return Err(format!(
                "the `{key}` component family did not pass its interaction leg"
            ));
        }
    }

    let themes = report
        .get("themes")
        .and_then(Json::as_array)
        .ok_or_else(|| format!("the page reported no `themes` array (report: {report})"))?;
    for (name, expected_theme) in [
        ("light", "light"),
        ("dark", "dark"),
        ("high-contrast", "light"),
    ] {
        let entry = themes
            .iter()
            .find(|entry| entry.get("name").and_then(Json::as_str) == Some(name))
            .ok_or_else(|| format!("the page reported no `{name}` theme scan"))?;
        if entry.get("theme").and_then(Json::as_str) != Some(expected_theme) {
            return Err(format!(
                "the `{name}` scan ran under theme {:?}, expected {expected_theme:?}",
                entry.get("theme").and_then(Json::as_str)
            ));
        }
        if entry.get("available").and_then(Json::as_bool) != Some(true) {
            return Err(format!(
                "axe-core did not run for the `{name}` appearance; an unrun scan is not a pass"
            ));
        }
        let violations = entry
            .get("violations")
            .and_then(Json::as_array)
            .ok_or_else(|| format!("the `{name}` scan reported no violations array"))?;
        if !violations.is_empty() {
            let first = &violations[0];
            let id = first.get("id").and_then(Json::as_str).unwrap_or("unknown");
            let help = first.get("help").and_then(Json::as_str).unwrap_or("");
            return Err(format!(
                "axe-core found {} violation(s) in the `{name}` appearance; first is {id}: \
                 {help}",
                violations.len()
            ));
        }
    }

    if let Some(errors) = report.get("errors").and_then(Json::as_array) {
        if !errors.is_empty() {
            return Err(format!(
                "the page recorded {} interaction error(s)",
                errors.len()
            ));
        }
    }
    Ok(())
}

struct SplitExpectation {
    pdf_names: &'static [&'static str],
    zip_name: &'static str,
    title: &'static str,
    pages_row: &'static str,
    total_pages: u64,
    over_target: bool,
}

const SPLIT_EVERY: SplitExpectation = SplitExpectation {
    pdf_names: &[
        "report-pages-1-1.pdf",
        "report-pages-2-2.pdf",
        "report-pages-3-3.pdf",
    ],
    zip_name: "report-split.zip",
    title: "3 PDFs and a ZIP",
    pages_row: "3 \u{2192} 3",
    total_pages: 3,
    over_target: false,
};

const SPLIT_SIZE: SplitExpectation = SplitExpectation {
    pdf_names: &["outlined-size-1-1.pdf", "outlined-size-2-2.pdf"],
    zip_name: "outlined-split.zip",
    title: "2 PDFs and a ZIP",
    pages_row: "2 \u{2192} 2",
    total_pages: 2,
    over_target: true,
};

const SPLIT_BOOKMARKS: SplitExpectation = SplitExpectation {
    pdf_names: &["outlined-bookmarks-1-1.pdf", "outlined-bookmarks-2-2.pdf"],
    zip_name: "outlined-split.zip",
    title: "2 PDFs and a ZIP",
    pages_row: "2 \u{2192} 2",
    total_pages: 2,
    over_target: false,
};

fn check_split_leg(leg: &Json, label: &str, expected: &SplitExpectation) -> Result<(), String> {
    if leg.get("fileRows").and_then(Json::as_u64) != Some(1) {
        return Err(format!(
            "the {label} split did not show one selected PDF (fileRows = {:?})",
            leg.get("fileRows")
        ));
    }
    if string_field(leg, "noUpload")? != "verified" {
        return Err(format!(
            "the {label} split never set data-no-upload=verified; the local request check did \
             not pass"
        ));
    }
    let run_label = string_field(leg, "runLabel")?;
    if run_label != "Split PDF" {
        return Err(format!(
            "the {label} split button was labelled {run_label:?}, expected \"Split PDF\""
        ));
    }
    let status = string_field(leg, "status")?;
    if !status.contains("verified") {
        return Err(format!(
            "the {label} split status did not say the result was verified (status = {status:?})"
        ));
    }

    let result = leg
        .get("result")
        .and_then(Json::as_object)
        .ok_or_else(|| format!("the {label} split reported no result object"))?;
    if result.get("title").and_then(Json::as_str) != Some(expected.title) {
        return Err(format!(
            "the {label} split result was named {:?}, expected {:?}",
            result.get("title"),
            expected.title
        ));
    }
    let expected_files = expected.pdf_names.len().saturating_add(1);
    let meta = result
        .get("meta")
        .and_then(Json::as_str)
        .ok_or_else(|| format!("the {label} split result had no summary"))?;
    if !meta.contains(&format!("{expected_files} files")) || !meta.contains("verified locally") {
        return Err(format!(
            "the {label} split summary did not describe {expected_files} verified files (meta = \
             {meta:?})"
        ));
    }
    let rows = result
        .get("rows")
        .and_then(Json::as_array)
        .ok_or_else(|| format!("the {label} split result had no verification rows"))?;
    let row_value = |wanted: &str| {
        rows.iter()
            .find(|row| row.get("label").and_then(Json::as_str) == Some(wanted))
            .and_then(|row| row.get("value"))
            .and_then(Json::as_str)
            .unwrap_or("")
    };
    if row_value("Pages") != expected.pages_row {
        return Err(format!(
            "the {label} split reported Pages {:?}, expected {:?}",
            row_value("Pages"),
            expected.pages_row
        ));
    }
    if row_value("Verification") != "ok" {
        return Err(format!(
            "the {label} split did not report a passing structural verification (got {:?})",
            row_value("Verification")
        ));
    }

    let outputs = leg
        .get("outputs")
        .and_then(Json::as_array)
        .ok_or_else(|| format!("the {label} split reported no output list"))?;
    if outputs.len() != expected_files {
        return Err(format!(
            "the {label} split produced {} outputs, expected {} PDFs and one ZIP",
            outputs.len(),
            expected.pdf_names.len()
        ));
    }
    let mut pdf_names = Vec::new();
    let mut zip_names = Vec::new();
    let mut total_pages = 0u64;
    for output in outputs {
        let kind = output
            .get("kind")
            .and_then(Json::as_str)
            .ok_or_else(|| format!("the {label} split output had no kind"))?;
        let name = output.get("name").and_then(Json::as_str).unwrap_or("");
        let download_name = output
            .get("downloadName")
            .and_then(Json::as_str)
            .unwrap_or("");
        if output.get("blob").and_then(Json::as_bool) != Some(true) {
            return Err(format!(
                "the {label} split output {name:?} was not a Blob URL"
            ));
        }
        let bytes = output.get("bytes").and_then(Json::as_u64).unwrap_or(0);
        if output.get("declaredBytes").and_then(Json::as_u64) != Some(bytes) || bytes == 0 {
            return Err(format!(
                "the {label} split output {name:?} had inconsistent byte counts (declared = {:?}, \
                 actual = {bytes})",
                output.get("declaredBytes")
            ));
        }
        match kind {
            "pdf" => {
                if !name.ends_with(".pdf") || download_name != name {
                    return Err(format!(
                        "the {label} split PDF {name:?} did not use a matching PDF download name \
                         (download = {download_name:?})"
                    ));
                }
                if output.get("previewBlob").and_then(Json::as_bool) != Some(true) {
                    return Err(format!(
                        "the {label} split PDF {name:?} was not previewable from a Blob URL"
                    ));
                }
                if output.get("header").and_then(Json::as_str) != Some("%PDF-") {
                    return Err(format!(
                        "the {label} split PDF {name:?} did not begin with %PDF- (header = {:?})",
                        output.get("header")
                    ));
                }
                let pages = output.get("pages").and_then(Json::as_u64).unwrap_or(0);
                if pages != 1 {
                    return Err(format!(
                        "the {label} split PDF {name:?} reported {pages} pages, expected one"
                    ));
                }
                let over_target = output
                    .get("overTarget")
                    .and_then(Json::as_bool)
                    .ok_or_else(|| {
                        format!(
                            "the {label} split PDF {name:?} had no overTarget flag; the page did \
                             not surface the size result"
                        )
                    })?;
                if over_target != expected.over_target {
                    return Err(format!(
                        "the {label} split PDF {name:?} reported overTarget = {over_target}, \
                         expected {}",
                        expected.over_target
                    ));
                }
                total_pages = total_pages.saturating_add(pages);
                pdf_names.push(name.to_string());
            }
            "zip" => {
                if !name.ends_with(".zip") || download_name != name {
                    return Err(format!(
                        "the {label} split ZIP {name:?} did not use a matching ZIP download name \
                         (download = {download_name:?})"
                    ));
                }
                if output.get("previewBlob") != Some(&Json::Null) {
                    return Err(format!(
                        "the {label} split ZIP {name:?} should not have a preview link"
                    ));
                }
                if output.get("zip").and_then(Json::as_bool) != Some(true) {
                    return Err(format!(
                        "the {label} split ZIP {name:?} did not begin with a ZIP local-file header"
                    ));
                }
                if output.get("pages").and_then(Json::as_u64) != Some(0) {
                    return Err(format!(
                        "the {label} split ZIP {name:?} reported {:?} pages, expected 0",
                        output.get("pages")
                    ));
                }
                zip_names.push(name.to_string());
            }
            other => {
                return Err(format!(
                    "the {label} split returned an unknown output kind {other:?}"
                ));
            }
        }
    }
    if pdf_names.len() != expected.pdf_names.len() || zip_names.len() != 1 {
        return Err(format!(
            "the {label} split returned {} PDF(s) and {} ZIP(s), expected {} and one",
            pdf_names.len(),
            zip_names.len(),
            expected.pdf_names.len()
        ));
    }
    for (index, expected_name) in expected.pdf_names.iter().enumerate() {
        if pdf_names.get(index).map(String::as_str) != Some(*expected_name) {
            return Err(format!(
                "the {label} split output #{index} was named {:?}, expected {:?}",
                pdf_names.get(index),
                expected_name
            ));
        }
    }
    if zip_names.first().map(String::as_str) != Some(expected.zip_name) {
        return Err(format!(
            "the {label} split ZIP was named {:?}, expected {:?}",
            zip_names.first(),
            expected.zip_name
        ));
    }
    if total_pages != expected.total_pages {
        return Err(format!(
            "the {label} split PDFs carried {total_pages} pages, expected {}",
            expected.total_pages
        ));
    }

    let network = leg
        .get("network")
        .and_then(Json::as_object)
        .ok_or_else(|| format!("the {label} split reported no network object"))?;
    let foreign = network
        .get("foreign")
        .and_then(Json::as_array)
        .ok_or_else(|| format!("the {label} split reported no foreign-resource list"))?;
    if !foreign.is_empty() {
        return Err(format!(
            "the {label} split fetched {} resource(s) from another origin: {:?}",
            foreign.len(),
            foreign.iter().filter_map(Json::as_str).collect::<Vec<_>>()
        ));
    }
    Ok(())
}

/// One page-organization leg: a duplicate or blank insertion run against the
/// two-page outlined fixture. Both add exactly one page and produce one PDF.
fn check_organize_leg(leg: &Json, label: &str, expected_name: &str) -> Result<(), String> {
    if leg.get("fileRows").and_then(Json::as_u64) != Some(1) {
        return Err(format!(
            "the {label} organize run did not show one selected PDF (fileRows = {:?})",
            leg.get("fileRows")
        ));
    }
    if string_field(leg, "noUpload")? != "verified" {
        return Err(format!(
            "the {label} organize run never set data-no-upload=verified; the local request check \
             did not pass"
        ));
    }
    let run_label = string_field(leg, "runLabel")?;
    if run_label != "Organize pages" {
        return Err(format!(
            "the {label} organize button was labelled {run_label:?}, expected \"Organize pages\""
        ));
    }
    let status = string_field(leg, "status")?;
    if !status.contains("verified") {
        return Err(format!(
            "the {label} organize status did not say the result was verified (status = {status:?})"
        ));
    }

    let result = leg
        .get("result")
        .and_then(Json::as_object)
        .ok_or_else(|| format!("the {label} organize run reported no result object"))?;
    if result.get("title").and_then(Json::as_str) != Some(expected_name) {
        return Err(format!(
            "the {label} organize result was named {:?}, expected {expected_name:?}",
            result.get("title")
        ));
    }
    let meta = result
        .get("meta")
        .and_then(Json::as_str)
        .ok_or_else(|| format!("the {label} organize result had no summary"))?;
    if !meta.contains("1 file") || !meta.contains("verified locally") {
        return Err(format!(
            "the {label} organize summary did not describe one verified file (meta = {meta:?})"
        ));
    }
    let rows = result
        .get("rows")
        .and_then(Json::as_array)
        .ok_or_else(|| format!("the {label} organize result had no verification rows"))?;
    let row_value = |wanted: &str| {
        rows.iter()
            .find(|row| row.get("label").and_then(Json::as_str) == Some(wanted))
            .and_then(|row| row.get("value"))
            .and_then(Json::as_str)
            .unwrap_or("")
    };
    if row_value("Pages") != "2 \u{2192} 3" {
        return Err(format!(
            "the {label} organize run reported Pages {:?}, expected \"2 \u{2192} 3\"",
            row_value("Pages")
        ));
    }
    if row_value("Verification") != "ok" {
        return Err(format!(
            "the {label} organize run did not report a passing structural verification (got {:?})",
            row_value("Verification")
        ));
    }

    let outputs = leg
        .get("outputs")
        .and_then(Json::as_array)
        .ok_or_else(|| format!("the {label} organize run reported no output list"))?;
    if outputs.len() != 1 {
        return Err(format!(
            "the {label} organize run produced {} outputs, expected one PDF",
            outputs.len()
        ));
    }
    let output = &outputs[0];
    if output.get("kind").and_then(Json::as_str) != Some("pdf") {
        return Err(format!(
            "the {label} organize output had kind {:?}, expected \"pdf\"",
            output.get("kind")
        ));
    }
    if output.get("name").and_then(Json::as_str) != Some(expected_name) {
        return Err(format!(
            "the {label} organize output was named {:?}, expected {expected_name:?}",
            output.get("name")
        ));
    }
    if output.get("pages").and_then(Json::as_u64) != Some(3) {
        return Err(format!(
            "the {label} organize output reported {:?} pages, expected 3",
            output.get("pages")
        ));
    }
    if output.get("blob").and_then(Json::as_bool) != Some(true)
        || output.get("previewBlob").and_then(Json::as_bool) != Some(true)
    {
        return Err(format!(
            "the {label} organize output did not use Blob URLs for preview and download"
        ));
    }
    if output.get("header").and_then(Json::as_str) != Some("%PDF-") {
        return Err(format!(
            "the {label} organize output did not begin with %PDF- (header = {:?})",
            output.get("header")
        ));
    }
    if output.get("downloadName").and_then(Json::as_str) != Some(expected_name) {
        return Err(format!(
            "the {label} organize download was named {:?}, expected {expected_name:?}",
            output.get("downloadName")
        ));
    }
    let bytes = output.get("bytes").and_then(Json::as_u64).unwrap_or(0);
    if bytes <= 500 {
        return Err(format!(
            "the {label} organize PDF is only {bytes} bytes; a two-page result should be larger"
        ));
    }
    let top_output = leg
        .get("output")
        .and_then(Json::as_object)
        .ok_or_else(|| format!("the {label} organize run exposed no selected PDF"))?;
    if top_output.get("name").and_then(Json::as_str) != Some(expected_name) {
        return Err(format!(
            "the {label} organize result exposed {:?}, expected {expected_name:?}",
            top_output.get("name")
        ));
    }
    if top_output.get("downloadName").and_then(Json::as_str) != Some(expected_name) {
        return Err(format!(
            "the {label} organize result download was named {:?}, expected {expected_name:?}",
            top_output.get("downloadName")
        ));
    }

    let network = leg
        .get("network")
        .and_then(Json::as_object)
        .ok_or_else(|| format!("the {label} organize run reported no network object"))?;
    let foreign = network
        .get("foreign")
        .and_then(Json::as_array)
        .ok_or_else(|| format!("the {label} organize run reported no foreign-resource list"))?;
    if !foreign.is_empty() {
        return Err(format!(
            "the {label} organize run fetched {} resource(s) from another origin: {:?}",
            foreign.len(),
            foreign.iter().filter_map(Json::as_str).collect::<Vec<_>>()
        ));
    }
    Ok(())
}

/// One overlay or compression leg. These operations preserve the page count,
/// so the proof is the same shape as the organize legs plus the marker text the
/// operation itself appended to the real output bytes.
fn check_stamp_leg(
    leg: &Json,
    label: &str,
    expected_name: &str,
    expected_run_label: &str,
    expected_pages: u64,
    needles: &[&str],
) -> Result<(), String> {
    if leg.get("fileRows").and_then(Json::as_u64) != Some(1) {
        return Err(format!(
            "the {label} leg did not show one selected PDF (fileRows = {:?})",
            leg.get("fileRows")
        ));
    }
    if string_field(leg, "noUpload")? != "verified" {
        return Err(format!(
            "the {label} leg never set data-no-upload=verified; the local request check did not pass"
        ));
    }
    let run_label = string_field(leg, "runLabel")?;
    if run_label != expected_run_label {
        return Err(format!(
            "the {label} button was labelled {run_label:?}, expected {expected_run_label:?}"
        ));
    }
    let status = string_field(leg, "status")?;
    if !status.contains("verified") {
        return Err(format!(
            "the {label} status did not say the result was verified (status = {status:?})"
        ));
    }

    let result = leg
        .get("result")
        .and_then(Json::as_object)
        .ok_or_else(|| format!("the {label} leg reported no result object"))?;
    if result.get("title").and_then(Json::as_str) != Some(expected_name) {
        return Err(format!(
            "the {label} result was named {:?}, expected {expected_name:?}",
            result.get("title")
        ));
    }
    let meta = result
        .get("meta")
        .and_then(Json::as_str)
        .ok_or_else(|| format!("the {label} result had no summary"))?;
    if !meta.contains("1 file") || !meta.contains("verified locally") {
        return Err(format!(
            "the {label} summary did not describe one verified file (meta = {meta:?})"
        ));
    }
    let rows = result
        .get("rows")
        .and_then(Json::as_array)
        .ok_or_else(|| format!("the {label} result had no verification rows"))?;
    let row_value = |wanted: &str| {
        rows.iter()
            .find(|row| row.get("label").and_then(Json::as_str) == Some(wanted))
            .and_then(|row| row.get("value"))
            .and_then(Json::as_str)
            .unwrap_or("")
    };
    let expected_pages_row = format!("{expected_pages} \u{2192} {expected_pages}");
    if row_value("Pages") != expected_pages_row {
        return Err(format!(
            "the {label} run reported Pages {:?}, expected {expected_pages_row:?}",
            row_value("Pages")
        ));
    }
    if row_value("Verification") != "ok" {
        return Err(format!(
            "the {label} run did not report a passing structural verification (got {:?})",
            row_value("Verification")
        ));
    }

    let outputs = leg
        .get("outputs")
        .and_then(Json::as_array)
        .ok_or_else(|| format!("the {label} run reported no output list"))?;
    if outputs.len() != 1 {
        return Err(format!(
            "the {label} run produced {} outputs, expected one PDF",
            outputs.len()
        ));
    }
    let output = &outputs[0];
    if output.get("kind").and_then(Json::as_str) != Some("pdf") {
        return Err(format!(
            "the {label} output had kind {:?}, expected \"pdf\"",
            output.get("kind")
        ));
    }
    if output.get("name").and_then(Json::as_str) != Some(expected_name) {
        return Err(format!(
            "the {label} output was named {:?}, expected {expected_name:?}",
            output.get("name")
        ));
    }
    if output.get("pages").and_then(Json::as_u64) != Some(expected_pages) {
        return Err(format!(
            "the {label} output reported {:?} pages, expected {expected_pages}",
            output.get("pages")
        ));
    }
    if output.get("blob").and_then(Json::as_bool) != Some(true)
        || output.get("previewBlob").and_then(Json::as_bool) != Some(true)
    {
        return Err(format!(
            "the {label} output did not use Blob URLs for preview and download"
        ));
    }
    if output.get("header").and_then(Json::as_str) != Some("%PDF-") {
        return Err(format!(
            "the {label} output did not begin with %PDF- (header = {:?})",
            output.get("header")
        ));
    }
    if output.get("downloadName").and_then(Json::as_str) != Some(expected_name) {
        return Err(format!(
            "the {label} download was named {:?}, expected {expected_name:?}",
            output.get("downloadName")
        ));
    }
    let bytes = output.get("bytes").and_then(Json::as_u64).unwrap_or(0);
    if bytes <= 300 {
        return Err(format!(
            "the {label} PDF is only {bytes} bytes; a {expected_pages}-page result should be larger"
        ));
    }
    let top_output = leg
        .get("output")
        .and_then(Json::as_object)
        .ok_or_else(|| format!("the {label} run exposed no selected PDF"))?;
    if top_output.get("name").and_then(Json::as_str) != Some(expected_name) {
        return Err(format!(
            "the {label} result exposed {:?}, expected {expected_name:?}",
            top_output.get("name")
        ));
    }

    if !needles.is_empty() {
        let found = output
            .get("found")
            .and_then(Json::as_array)
            .ok_or_else(|| format!("the {label} output did not report its marker checks"))?;
        if found.len() != needles.len() || !found.iter().all(|value| value.as_bool() == Some(true))
        {
            return Err(format!(
                "the {label} output did not contain every operation marker {needles:?} (found = {:?})",
                found
            ));
        }
    }

    let network = leg
        .get("network")
        .and_then(Json::as_object)
        .ok_or_else(|| format!("the {label} run reported no network object"))?;
    let foreign = network
        .get("foreign")
        .and_then(Json::as_array)
        .ok_or_else(|| format!("the {label} run reported no foreign-resource list"))?;
    if !foreign.is_empty() {
        return Err(format!(
            "the {label} run fetched {} resource(s) from another origin: {:?}",
            foreign.len(),
            foreign.iter().filter_map(Json::as_str).collect::<Vec<_>>()
        ));
    }
    Ok(())
}

/// One page-mixing leg: two selected PDFs become one interleaved PDF with the
/// first input's name, and the result reports the sum of the input pages.
fn check_mix_leg(
    leg: &Json,
    label: &str,
    expected_name: &str,
    expected_pages: u64,
) -> Result<(), String> {
    if leg.get("fileRows").and_then(Json::as_u64) != Some(2) {
        return Err(format!(
            "the {label} leg did not show two selected PDFs (fileRows = {:?})",
            leg.get("fileRows")
        ));
    }
    if string_field(leg, "noUpload")? != "verified" {
        return Err(format!(
            "the {label} leg never set data-no-upload=verified; the local request check did not pass"
        ));
    }
    if string_field(leg, "runLabel")? != "Mix pages" {
        return Err(format!(
            "the {label} button was labelled {:?}, expected \"Mix pages\"",
            leg.get("runLabel")
        ));
    }
    let status = string_field(leg, "status")?;
    if !status.contains("verified") {
        return Err(format!(
            "the {label} status did not say the result was verified (status = {status:?})"
        ));
    }

    let result = leg
        .get("result")
        .and_then(Json::as_object)
        .ok_or_else(|| format!("the {label} leg reported no result object"))?;
    if result.get("title").and_then(Json::as_str) != Some(expected_name) {
        return Err(format!(
            "the {label} result was named {:?}, expected {expected_name:?}",
            result.get("title")
        ));
    }
    let meta = result.get("meta").and_then(Json::as_str).unwrap_or("");
    if !meta.contains("1 file") || !meta.contains("verified locally") {
        return Err(format!(
            "the {label} summary did not describe one verified file (meta = {meta:?})"
        ));
    }
    let rows = result
        .get("rows")
        .and_then(Json::as_array)
        .ok_or_else(|| format!("the {label} result had no verification rows"))?;
    let row_value = |wanted: &str| {
        rows.iter()
            .find(|row| row.get("label").and_then(Json::as_str) == Some(wanted))
            .and_then(|row| row.get("value"))
            .and_then(Json::as_str)
            .unwrap_or("")
    };
    let expected_pages_row = format!("{expected_pages} \u{2192} {expected_pages}");
    if row_value("Pages") != expected_pages_row {
        return Err(format!(
            "the {label} run reported Pages {:?}, expected {expected_pages_row:?}",
            row_value("Pages")
        ));
    }
    if row_value("Verification") != "ok" {
        return Err(format!(
            "the {label} run did not report a passing structural verification (got {:?})",
            row_value("Verification")
        ));
    }

    let output = leg
        .get("output")
        .and_then(Json::as_object)
        .ok_or_else(|| format!("the {label} run exposed no selected PDF"))?;
    if output.get("kind").and_then(Json::as_str) != Some("pdf") {
        return Err(format!(
            "the {label} output had kind {:?}, expected \"pdf\"",
            output.get("kind")
        ));
    }
    if output.get("name").and_then(Json::as_str) != Some(expected_name) {
        return Err(format!(
            "the {label} output was named {:?}, expected {expected_name:?}",
            output.get("name")
        ));
    }
    if output.get("pages").and_then(Json::as_u64) != Some(expected_pages) {
        return Err(format!(
            "the {label} output reported {:?} pages, expected {expected_pages}",
            output.get("pages")
        ));
    }
    if output.get("blob").and_then(Json::as_bool) != Some(true)
        || output.get("previewBlob").and_then(Json::as_bool) != Some(true)
    {
        return Err(format!(
            "the {label} output did not use Blob URLs for preview and download"
        ));
    }
    if output.get("header").and_then(Json::as_str) != Some("%PDF-") {
        return Err(format!(
            "the {label} output did not begin with %PDF- (header = {:?})",
            output.get("header")
        ));
    }
    if output.get("downloadName").and_then(Json::as_str) != Some(expected_name) {
        return Err(format!(
            "the {label} download was named {:?}, expected {expected_name:?}",
            output.get("downloadName")
        ));
    }
    if output.get("bytes").and_then(Json::as_u64).unwrap_or(0) <= 500 {
        return Err(format!(
            "the {label} PDF is only {:?} bytes; a four-page result should be larger",
            output.get("bytes")
        ));
    }

    let network = leg
        .get("network")
        .and_then(Json::as_object)
        .ok_or_else(|| format!("the {label} run reported no network object"))?;
    let foreign = network
        .get("foreign")
        .and_then(Json::as_array)
        .ok_or_else(|| format!("the {label} run reported no foreign-resource list"))?;
    if !foreign.is_empty() {
        return Err(format!(
            "the {label} run fetched {} resource(s) from another origin: {:?}",
            foreign.len(),
            foreign.iter().filter_map(Json::as_str).collect::<Vec<_>>()
        ));
    }
    Ok(())
}

/// One attachment-extraction leg. The page must expose two decoded files and
/// one ZIP, and the file rows must not be presented as PDFs or previews.
fn check_attachments_leg(leg: &Json, label: &str, expected: &[(&str, &str)]) -> Result<(), String> {
    if leg.get("fileRows").and_then(Json::as_u64) != Some(1) {
        return Err(format!(
            "the {label} leg did not show one selected PDF (fileRows = {:?})",
            leg.get("fileRows")
        ));
    }
    if string_field(leg, "noUpload")? != "verified" {
        return Err(format!(
            "the {label} leg never set data-no-upload=verified; the local request check did not pass"
        ));
    }
    if string_field(leg, "runLabel")? != "Extract attachments" {
        return Err(format!(
            "the {label} button was labelled {:?}, expected \"Extract attachments\"",
            leg.get("runLabel")
        ));
    }
    let status = string_field(leg, "status")?;
    if !status.contains("verified") {
        return Err(format!(
            "the {label} status did not say the result was verified (status = {status:?})"
        ));
    }

    let result = leg
        .get("result")
        .and_then(Json::as_object)
        .ok_or_else(|| format!("the {label} leg reported no result object"))?;
    let title = result.get("title").and_then(Json::as_str).unwrap_or("");
    if title != "2 attachments and a ZIP" {
        return Err(format!(
            "the {label} result was named {title:?}, expected \"2 attachments and a ZIP\""
        ));
    }
    let meta = result.get("meta").and_then(Json::as_str).unwrap_or("");
    if !meta.contains("3 files") || !meta.contains("verified locally") {
        return Err(format!(
            "the {label} summary did not describe three verified files (meta = {meta:?})"
        ));
    }
    let rows = result
        .get("rows")
        .and_then(Json::as_array)
        .ok_or_else(|| format!("the {label} result had no verification rows"))?;
    let row_value = |wanted: &str| {
        rows.iter()
            .find(|row| row.get("label").and_then(Json::as_str) == Some(wanted))
            .and_then(|row| row.get("value"))
            .and_then(Json::as_str)
            .unwrap_or("")
    };
    if row_value("Embedded files") != "2 \u{2192} 2" {
        return Err(format!(
            "the {label} run reported Embedded files {:?}, expected \"2 \u{2192} 2\"",
            row_value("Embedded files")
        ));
    }
    if row_value("Verification") != "ok" {
        return Err(format!(
            "the {label} run did not report a passing extraction verification (got {:?})",
            row_value("Verification")
        ));
    }
    if !row_value("Pages").is_empty() {
        return Err(format!(
            "the {label} attachment result showed a Pages row ({:?}); no PDF was produced",
            row_value("Pages")
        ));
    }

    let outputs = leg
        .get("outputs")
        .and_then(Json::as_array)
        .ok_or_else(|| format!("the {label} run reported no output list"))?;
    if outputs.len() != expected.len() + 1 {
        return Err(format!(
            "the {label} run produced {} outputs, expected {} files and a ZIP",
            outputs.len(),
            expected.len()
        ));
    }
    let find = |name: &str| {
        outputs
            .iter()
            .find(|output| output.get("name").and_then(Json::as_str) == Some(name))
    };
    for (name, contents) in expected {
        let output = find(name).ok_or_else(|| format!("the {label} run lost {name}"))?;
        if output.get("kind").and_then(Json::as_str) != Some("file") {
            return Err(format!(
                "the {label} output {name} had kind {:?}, expected \"file\"",
                output.get("kind")
            ));
        }
        if output.get("pages").and_then(Json::as_u64) != Some(0) {
            return Err(format!(
                "the {label} output {name} reported pages, but it is not a PDF"
            ));
        }
        if output.get("blob").and_then(Json::as_bool) != Some(true) {
            return Err(format!("the {label} output {name} was not a Blob URL"));
        }
        if output.get("previewBlob").and_then(Json::as_bool).is_some() {
            return Err(format!("the {label} output {name} offered a PDF preview"));
        }
        if output.get("downloadName").and_then(Json::as_str) != Some(name) {
            return Err(format!(
                "the {label} download for {name} was named {:?}",
                output.get("downloadName")
            ));
        }
        if output.get("text").and_then(Json::as_str) != Some(contents) {
            return Err(format!(
                "the {label} output {name} contained {:?}, expected {contents:?}",
                output.get("text")
            ));
        }
    }
    let zip = find("bundle-attachments.zip")
        .ok_or_else(|| format!("the {label} run produced no attachment ZIP"))?;
    if zip.get("kind").and_then(Json::as_str) != Some("zip") {
        return Err(format!(
            "the {label} bundle had kind {:?}, expected \"zip\"",
            zip.get("kind")
        ));
    }
    if zip.get("zip").and_then(Json::as_bool) != Some(true) {
        return Err(format!(
            "the {label} bundle did not begin with a ZIP local-file header"
        ));
    }
    if zip.get("downloadName").and_then(Json::as_str) != Some("bundle-attachments.zip") {
        return Err(format!(
            "the {label} bundle download was named {:?}",
            zip.get("downloadName")
        ));
    }

    let network = leg
        .get("network")
        .and_then(Json::as_object)
        .ok_or_else(|| format!("the {label} run reported no network object"))?;
    let foreign = network
        .get("foreign")
        .and_then(Json::as_array)
        .ok_or_else(|| format!("the {label} run reported no foreign-resource list"))?;
    if !foreign.is_empty() {
        return Err(format!(
            "the {label} run fetched {} resource(s) from another origin: {:?}",
            foreign.len(),
            foreign.iter().filter_map(Json::as_str).collect::<Vec<_>>()
        ));
    }
    Ok(())
}

/// `tools-page` (SL-4.TOOLS.02 + SL-4.TOOLS.05 + the Phase 1A.1 split and
/// page-organization modes): the real `/tools.html` merges and mixes two PDFs,
/// splits a three-page PDF one page at a time, duplicates a page, and inserts a
/// blank page online, then the harness cuts the origin, reloads the same page,
/// and repeats those runs with every byte coming from the worker's cache.
///
/// Every claim is read from the page the user gets, not from a helper:
///
/// * thirteen cards render, and choosing Merge opens the workspace;
/// * two real `File` objects produce two rows and enable the button;
/// * the run reaches `data-no-upload="verified"`, which the page only sets after
///   its request recorder asserts that no request carried the input bytes;
/// * the merge result shows Pages `2 → 2` and an `ok` verdict, and its Blob
///   begins `%PDF-` and is larger than the two 423-byte inputs;
/// * the mix leg interleaves the three-page report with the one-page fixture,
///   naming the result `report-mixed.pdf` and reporting Pages `4 → 4`;
/// * the split-every leg shows one selected PDF, Pages `3 → 3`, three PDF rows
///   with one page each, and a fourth ZIP row whose actual Blob starts `PK`;
/// * the size leg measures the outlined fixture against a 1 KiB target and
///   shows two over-target PDFs plus the ZIP, while the bookmark leg shows two
///   bookmark-bounded PDFs with no over-target flag;
/// * the duplicate leg and the blank-page leg each add one page to the outlined
///   fixture, naming the one PDF result and proving page mode dispatch;
/// * page numbers, watermark, header/footer, and Bates legs each stamp the real
///   output stream, read back from the Blob, and the compress leg rewrites the
///   report without changing its page count;
/// * the attachment leg downloads both embedded files with their decoded bytes
///   and a ZIP, without offering either file as a PDF preview;
/// * every resource the iframe's own Resource Timing saw is same-origin, and the
///   `selis_pdf_wasm_tools.wasm` chunk is one of them;
/// * the offline leg repeats the card grid, merge, every-size-bookmark splits,
///   and both organize runs after the origin has answered 503, while the page is
///   worker-controlled and still sees zero foreign resources.
///
/// The last two points are why the page cannot pass on a fixture: the fixture
/// would not fetch the real module graph, and a page that silently skipped the
/// WASM chunk would still be able to render a card grid. The offline leg is
/// what proves the precache list is complete rather than merely present.
fn verdict_tools_page(report: &Json) -> Result<(), String> {
    well_formed(report)?;
    if string_field(report, "check")? != "tools-page" {
        return Err("the page did not identify itself as the tools check".into());
    }
    if report.get("cards").and_then(Json::as_u64) != Some(13) {
        return Err(format!(
            "the tools grid did not render thirteen cards (cards = {:?})",
            report.get("cards")
        ));
    }
    if report.get("fileRows").and_then(Json::as_u64) != Some(2) {
        return Err(format!(
            "the page did not show two selected PDFs (fileRows = {:?})",
            report.get("fileRows")
        ));
    }
    if string_field(report, "noUpload")? != "verified" {
        return Err(
            "the page never set data-no-upload=verified; the local request check did not pass"
                .into(),
        );
    }

    let result = report
        .get("result")
        .and_then(Json::as_object)
        .ok_or_else(|| "the page reported no result object".to_string())?;
    let title = result
        .get("title")
        .and_then(Json::as_str)
        .ok_or_else(|| "the result had no title".to_string())?;
    if title != "merged.pdf" {
        return Err(format!(
            "the merge result was named {title:?}, expected \"merged.pdf\""
        ));
    }
    let rows = result
        .get("rows")
        .and_then(Json::as_array)
        .ok_or_else(|| "the result had no verification rows".to_string())?;
    let row_value = |label: &str| {
        rows.iter()
            .find(|row| row.get("label").and_then(Json::as_str) == Some(label))
            .and_then(|row| row.get("value"))
            .and_then(Json::as_str)
            .unwrap_or("")
    };
    let pages = row_value("Pages");
    if pages != "2 \u{2192} 2" {
        return Err(format!(
            "the merge of two one-page PDFs reported Pages {pages:?}, expected \"2 \u{2192} 2\""
        ));
    }
    let verdict = row_value("Verification");
    if verdict != "ok" {
        return Err(format!(
            "the merge result did not report a passing structural verification (got {verdict:?})"
        ));
    }

    let output = report
        .get("output")
        .and_then(Json::as_object)
        .ok_or_else(|| "the page reported no output object".to_string())?;
    if output.get("blob").and_then(Json::as_bool) != Some(true) {
        return Err("the result download was not a Blob URL".into());
    }
    if output.get("previewBlob").and_then(Json::as_bool) != Some(true) {
        return Err("the result preview was not a Blob URL".into());
    }
    if output.get("header").and_then(Json::as_str) != Some("%PDF-") {
        return Err(format!(
            "the downloaded bytes did not begin with %PDF- (header = {:?})",
            output.get("header")
        ));
    }
    let bytes = output.get("bytes").and_then(Json::as_u64).unwrap_or(0);
    if bytes <= 500 {
        return Err(format!(
            "the downloaded PDF is only {bytes} bytes; two 423-byte inputs should produce more"
        ));
    }
    let name = output
        .get("downloadName")
        .and_then(Json::as_str)
        .unwrap_or("");
    if !name.ends_with(".pdf") {
        return Err(format!("the download name {name:?} is not a PDF name"));
    }

    let network = report
        .get("network")
        .and_then(Json::as_object)
        .ok_or_else(|| "the page reported no network object".to_string())?;
    let foreign = network
        .get("foreign")
        .and_then(Json::as_array)
        .ok_or_else(|| "the page reported no foreign-resource list".to_string())?;
    if !foreign.is_empty() {
        return Err(format!(
            "the tools page fetched {} resource(s) from another origin: {:?}",
            foreign.len(),
            foreign.iter().filter_map(Json::as_str).collect::<Vec<_>>()
        ));
    }
    let resources = network.get("resources").and_then(Json::as_u64).unwrap_or(0);
    if resources < 6 {
        return Err(format!(
            "only {resources} same-origin resources were observed; the page graph and lazy chunk did not all load"
        ));
    }
    let paths = network
        .get("paths")
        .and_then(Json::as_array)
        .ok_or_else(|| "the page reported no resource paths".to_string())?;
    if !paths.iter().any(|path| {
        path.as_str()
            .is_some_and(|path| path.ends_with("/wasm/selis_pdf_wasm_tools.wasm"))
    }) {
        return Err(
            "the lazy tools WASM chunk was not among the page's resources; the merge could not have run"
                .into(),
        );
    }

    let mix = report
        .get("mix")
        .ok_or_else(|| "the page reported no mix leg (mix = None)".to_string())?;
    check_mix_leg(mix, "online mix", "report-mixed.pdf", 4)?;

    // Phase 1A.1. The same page splits the three-page fixture one page at a
    // time, then measures a size target and the outline fixture. The proof is
    // structural: every PDF has one page, each leg has a ZIP whose local-file
    // header is read from the actual Blob bytes, and the size leg carries the
    // page's overTarget flag.
    let split = report
        .get("split")
        .ok_or_else(|| "the page reported no split-every leg (split = None)".to_string())?;
    check_split_leg(split, "online every", &SPLIT_EVERY)?;
    let split_size = report
        .get("splitSize")
        .ok_or_else(|| "the page reported no size-split leg (splitSize = None)".to_string())?;
    check_split_leg(split_size, "online size", &SPLIT_SIZE)?;
    let split_bookmarks = report.get("splitBookmarks").ok_or_else(|| {
        "the page reported no bookmark-split leg (splitBookmarks = None)".to_string()
    })?;
    check_split_leg(split_bookmarks, "online bookmarks", &SPLIT_BOOKMARKS)?;

    // Phase 1A.1 continuation: the same Organize pages workspace adds one
    // duplicate and one blank page to the two-page outlined fixture. The
    // result page count is the proof that the mode dispatch reached the real
    // backend, not just that the controls rendered.
    let duplicate = report
        .get("duplicate")
        .ok_or_else(|| "the page reported no duplicate page leg (duplicate = None)".to_string())?;
    check_organize_leg(duplicate, "online duplicate", "outlined-duplicated.pdf")?;
    let blanks = report
        .get("blanks")
        .ok_or_else(|| "the page reported no blank-page leg (blanks = None)".to_string())?;
    check_organize_leg(blanks, "online blank pages", "outlined-blanks.pdf")?;

    let page_numbers = report
        .get("pageNumbers")
        .ok_or_else(|| "the page reported no page-number leg (pageNumbers = None)".to_string())?;
    check_stamp_leg(
        page_numbers,
        "online page numbers",
        "report-numbered.pdf",
        "Add page numbers",
        3,
        &["(5/3) Tj", "(6/3) Tj"],
    )?;
    let watermark = report
        .get("watermark")
        .ok_or_else(|| "the page reported no watermark leg (watermark = None)".to_string())?;
    check_stamp_leg(
        watermark,
        "online watermark",
        "outlined-watermarked.pdf",
        "Add watermark",
        2,
        &["(CONFIDENTIAL) Tj", "/SelisOverlayGS gs"],
    )?;
    let header_footer = report.get("headerFooter").ok_or_else(|| {
        "the page reported no header/footer leg (headerFooter = None)".to_string()
    })?;
    check_stamp_leg(
        header_footer,
        "online header/footer",
        "report-header-footer.pdf",
        "Add header & footer",
        3,
        &["(Page 1 of 3) Tj", "(Center) Tj", "(Draft) Tj"],
    )?;
    let bates = report
        .get("bates")
        .ok_or_else(|| "the page reported no Bates leg (bates = None)".to_string())?;
    check_stamp_leg(
        bates,
        "online Bates",
        "report-bates.pdf",
        "Add Bates numbers",
        3,
        &["(ABC-0009) Tj", "(ABC-0010) Tj"],
    )?;
    let compress = report
        .get("compress")
        .ok_or_else(|| "the page reported no compress leg (compress = None)".to_string())?;
    check_stamp_leg(
        compress,
        "online compress",
        "report-compressed.pdf",
        "Compress PDF",
        3,
        &[],
    )?;
    let attachments = report.get("attachments").ok_or_else(|| {
        "the page reported no attachment-extraction leg (attachments = None)".to_string()
    })?;
    check_attachments_leg(
        attachments,
        "online attachments",
        &[("notes.txt", "hello attachment"), ("evil.txt", "safe name")],
    )?;

    // SL-4.TOOLS.05. The second leg is the same page with the origin cut. A
    // page that forgot to register the worker, or a precache list missing one
    // module, never reaches the card grid: the harness gets the worker's 503.
    let offline = report
        .get("offline")
        .and_then(Json::as_object)
        .ok_or_else(|| "the page reported no offline leg (offline = None)".to_string())?;
    if offline.get("controlled").and_then(Json::as_bool) != Some(true) {
        return Err(format!(
            "the offline tools page was not controlled by the worker (controlled = {:?})",
            offline.get("controlled")
        ));
    }
    let script = offline.get("script").and_then(Json::as_str).unwrap_or("");
    if !script.ends_with("/sw.js") {
        return Err(format!(
            "the offline tools page was controlled by {script:?}, not the app's /sw.js"
        ));
    }
    if offline.get("cards").and_then(Json::as_u64) != Some(13) {
        return Err(format!(
            "the offline tools page did not render thirteen cards (cards = {:?})",
            offline.get("cards")
        ));
    }
    if offline.get("noUpload").and_then(Json::as_str) != Some("verified") {
        return Err(
            "the offline merge never set data-no-upload=verified; the local request check did \
             not pass"
                .into(),
        );
    }
    let offline_result = offline
        .get("result")
        .and_then(Json::as_object)
        .ok_or_else(|| "the offline merge reported no result object".to_string())?;
    if offline_result.get("title").and_then(Json::as_str) != Some("merged.pdf") {
        return Err(format!(
            "the offline merge result was named {:?}, expected \"merged.pdf\"",
            offline_result.get("title")
        ));
    }
    let offline_rows = offline_result
        .get("rows")
        .and_then(Json::as_array)
        .ok_or_else(|| "the offline result had no verification rows".to_string())?;
    let offline_row_value = |label: &str| {
        offline_rows
            .iter()
            .find(|row| row.get("label").and_then(Json::as_str) == Some(label))
            .and_then(|row| row.get("value"))
            .and_then(Json::as_str)
            .unwrap_or("")
    };
    let offline_pages = offline_row_value("Pages");
    if offline_pages != "2 \u{2192} 2" {
        return Err(format!(
            "the offline merge of two one-page PDFs reported Pages {offline_pages:?}, expected \
             \"2 \u{2192} 2\""
        ));
    }
    if offline_row_value("Verification") != "ok" {
        return Err(format!(
            "the offline merge did not report a passing structural verification (got {:?})",
            offline_row_value("Verification")
        ));
    }
    let offline_output = offline
        .get("output")
        .and_then(Json::as_object)
        .ok_or_else(|| "the offline merge reported no output object".to_string())?;
    if offline_output.get("blob").and_then(Json::as_bool) != Some(true)
        || offline_output.get("previewBlob").and_then(Json::as_bool) != Some(true)
    {
        return Err("the offline result did not use Blob URLs for preview and download".into());
    }
    if offline_output.get("header").and_then(Json::as_str) != Some("%PDF-") {
        return Err(format!(
            "the offline downloaded bytes did not begin with %PDF- (header = {:?})",
            offline_output.get("header")
        ));
    }
    let offline_bytes = offline_output
        .get("bytes")
        .and_then(Json::as_u64)
        .unwrap_or(0);
    if offline_bytes <= 500 {
        return Err(format!(
            "the offline downloaded PDF is only {offline_bytes} bytes; two 423-byte inputs \
             should produce more"
        ));
    }
    let offline_name = offline_output
        .get("downloadName")
        .and_then(Json::as_str)
        .unwrap_or("");
    if !offline_name.ends_with(".pdf") {
        return Err(format!(
            "the offline download name {offline_name:?} is not a PDF name"
        ));
    }
    let offline_network = offline
        .get("network")
        .and_then(Json::as_object)
        .ok_or_else(|| "the offline leg reported no network object".to_string())?;
    let offline_foreign = offline_network
        .get("foreign")
        .and_then(Json::as_array)
        .ok_or_else(|| "the offline leg reported no foreign-resource list".to_string())?;
    if !offline_foreign.is_empty() {
        return Err(format!(
            "the offline tools page recorded {} foreign resource(s): {:?}",
            offline_foreign.len(),
            offline_foreign
                .iter()
                .filter_map(Json::as_str)
                .collect::<Vec<_>>()
        ));
    }
    let offline_mix = offline
        .get("mix")
        .ok_or_else(|| "the offline leg reported no mix result (mix = None)".to_string())?;
    check_mix_leg(offline_mix, "offline mix", "report-mixed.pdf", 4)?;
    let offline_split = offline
        .get("split")
        .ok_or_else(|| "the offline leg reported no split-every result".to_string())?;
    check_split_leg(offline_split, "offline every", &SPLIT_EVERY)?;
    let offline_split_size = offline
        .get("splitSize")
        .ok_or_else(|| "the offline leg reported no size-split result".to_string())?;
    check_split_leg(offline_split_size, "offline size", &SPLIT_SIZE)?;
    let offline_split_bookmarks = offline
        .get("splitBookmarks")
        .ok_or_else(|| "the offline leg reported no bookmark-split result".to_string())?;
    check_split_leg(
        offline_split_bookmarks,
        "offline bookmarks",
        &SPLIT_BOOKMARKS,
    )?;
    let offline_duplicate = offline.get("duplicate").ok_or_else(|| {
        "the offline leg reported no duplicate-page result (duplicate = None)".to_string()
    })?;
    check_organize_leg(
        offline_duplicate,
        "offline duplicate",
        "outlined-duplicated.pdf",
    )?;
    let offline_blanks = offline.get("blanks").ok_or_else(|| {
        "the offline leg reported no blank-page result (blanks = None)".to_string()
    })?;
    check_organize_leg(offline_blanks, "offline blank pages", "outlined-blanks.pdf")?;

    let offline_page_numbers = offline.get("pageNumbers").ok_or_else(|| {
        "the offline leg reported no page-number result (pageNumbers = None)".to_string()
    })?;
    check_stamp_leg(
        offline_page_numbers,
        "offline page numbers",
        "report-numbered.pdf",
        "Add page numbers",
        3,
        &["(5/3) Tj", "(6/3) Tj"],
    )?;
    let offline_watermark = offline.get("watermark").ok_or_else(|| {
        "the offline leg reported no watermark result (watermark = None)".to_string()
    })?;
    check_stamp_leg(
        offline_watermark,
        "offline watermark",
        "outlined-watermarked.pdf",
        "Add watermark",
        2,
        &["(CONFIDENTIAL) Tj", "/SelisOverlayGS gs"],
    )?;
    let offline_header_footer = offline.get("headerFooter").ok_or_else(|| {
        "the offline leg reported no header/footer result (headerFooter = None)".to_string()
    })?;
    check_stamp_leg(
        offline_header_footer,
        "offline header/footer",
        "report-header-footer.pdf",
        "Add header & footer",
        3,
        &["(Page 1 of 3) Tj", "(Center) Tj", "(Draft) Tj"],
    )?;
    let offline_bates = offline
        .get("bates")
        .ok_or_else(|| "the offline leg reported no Bates result (bates = None)".to_string())?;
    check_stamp_leg(
        offline_bates,
        "offline Bates",
        "report-bates.pdf",
        "Add Bates numbers",
        3,
        &["(ABC-0009) Tj", "(ABC-0010) Tj"],
    )?;
    let offline_compress = offline.get("compress").ok_or_else(|| {
        "the offline leg reported no compress result (compress = None)".to_string()
    })?;
    check_stamp_leg(
        offline_compress,
        "offline compress",
        "report-compressed.pdf",
        "Compress PDF",
        3,
        &[],
    )?;
    let offline_attachments = offline
        .get("attachments")
        .ok_or_else(|| "the offline leg reported no attachment-extraction result".to_string())?;
    check_attachments_leg(
        offline_attachments,
        "offline attachments",
        &[("notes.txt", "hello attachment"), ("evil.txt", "safe name")],
    )?;

    let status = string_field(report, "status")?;
    if !status.contains("verified") {
        return Err(format!(
            "the success status did not say the result was verified (status = {status:?})"
        ));
    }
    Ok(())
}

/// `extension` (SL-4.UI.06): the packed viewer, in the extension's own
/// processes, reads the fixture and a bookmark moves it.
///
/// This is the only check that drives a real extension. The viewer is a
/// `chrome-extension://` page, the engine lives in the extension's offscreen
/// document behind the WASM worker, and the two talk over a real
/// `chrome.runtime` port. Every one of the bugs this leg found first — the
/// port name's shape, the two port halves validating different directions, the
/// wasm-bindgen imports, the surface reading a `TileEntry` as a tile — was a
/// real-API difference the Node suite could not see.
///
/// The two claims are the web leg's, in the extension's terms: the panels
/// render, and a press MOVES the reader. The move is asserted through five
/// independent witnesses; one witness changing could be a measurement
/// accident, five is a navigation. The pixel witness is a sampled hash of the
/// canvas, not a count of opaque pixels: the canvas is `{ alpha: false }`, so
/// alpha is 255 everywhere and a count of it says nothing.
fn verdict_extension(report: &Json) -> Result<(), String> {
    well_formed(report)?;
    if string_field(report, "check")? != "extension" {
        return Err("the page did not identify itself as the extension check".into());
    }
    if string_field(report, "protocol")? != "chrome-extension:" {
        return Err(format!(
            "the viewer is not running as an extension page (protocol = {:?})",
            report.get("protocol")
        ));
    }
    if string_field(report, "extensionId")? != EXTENSION_ID {
        return Err(format!(
            "the viewer ran under extension id {:?}, not the staged key's {EXTENSION_ID}",
            report.get("extensionId")
        ));
    }
    if string_field(report, "stage")? != "done" {
        return Err(format!(
            "the probe stopped at stage {:?} rather than after the press",
            report.get("stage")
        ));
    }

    let origin = string_field(report, "origin")?;
    if !origin.starts_with("http://127.0.0.1:") {
        return Err(format!(
            "the viewer's source is not the loopback fixture (origin = {origin:?})"
        ));
    }
    let src = string_field(report, "src")?;
    if src != format!("{origin}/doc.pdf") {
        return Err(format!(
            "the viewer opened {src:?}, not the fixture at {origin}/doc.pdf"
        ));
    }
    let status = string_field(report, "status")?;
    if !status.starts_with("Opened doc.pdf") || !status.contains("2 pages") {
        return Err(format!(
            "the viewer did not open the two-page fixture: status = {status:?}"
        ));
    }
    if report.get("composedMs").and_then(Json::as_u64).is_none() {
        return Err("the viewer never composed its mount (composedMs = null)".into());
    }

    // ── The panels ───────────────────────────────────────────────────────
    let mount = report
        .get("mount")
        .and_then(Json::as_object)
        .ok_or_else(|| "the viewer reported no mount (mount = null)".to_string())?;
    if mount.get("hidden").and_then(Json::as_bool) != Some(false) {
        return Err(format!("the document mount is not visible: {mount:?}"));
    }
    if mount.get("children").and_then(Json::as_u64).unwrap_or(0) == 0 {
        return Err("the document mount composed no children".into());
    }
    let width = mount.get("width").and_then(Json::as_f64).unwrap_or(0.0);
    let height = mount.get("height").and_then(Json::as_f64).unwrap_or(0.0);
    if width <= 0.0 || height <= 0.0 {
        return Err(format!("the document mount has no area ({width}x{height})"));
    }

    let outline = report
        .get("outline")
        .and_then(Json::as_object)
        .ok_or_else(|| "the viewer reported no outline panel (outline = null)".to_string())?;
    if outline.get("rows").and_then(Json::as_u64) != Some(2) {
        return Err(format!(
            "the fixture declares two outline entries and the viewer rendered {:?}",
            outline.get("rows")
        ));
    }
    let titles: Vec<&str> = outline
        .get("titles")
        .and_then(Json::as_array)
        .map(|rows| rows.iter().filter_map(Json::as_str).collect())
        .unwrap_or_default();
    if titles.len() != 2 || titles[0] != "First layer" || titles[1] != "Second layer" {
        return Err(format!(
            "the outline rows do not carry the document's own titles: {titles:?}"
        ));
    }
    let pages: Vec<&str> = outline
        .get("pages")
        .and_then(Json::as_array)
        .map(|rows| rows.iter().filter_map(Json::as_str).collect())
        .unwrap_or_default();
    if pages.len() != 2 || pages[0] != "0" || pages[1] != "1" {
        return Err(format!(
            "the outline rows do not name page indexes 0 and 1: {pages:?}"
        ));
    }
    let labels: Vec<&str> = outline
        .get("labels")
        .and_then(Json::as_array)
        .map(|rows| rows.iter().filter_map(Json::as_str).collect())
        .unwrap_or_default();
    if !labels.iter().any(|label| label.ends_with("Page 2 of 2")) {
        return Err(format!(
            "no outline row announces the second page in its accessible name: {labels:?}"
        ));
    }

    let page_list = report
        .get("pageList")
        .and_then(Json::as_object)
        .ok_or_else(|| "the viewer reported no page list (pageList = null)".to_string())?;
    if page_list.get("tiles").and_then(Json::as_u64) != Some(2) {
        return Err(format!(
            "the fixture has two pages and the page list rendered {:?} tiles",
            page_list.get("tiles")
        ));
    }
    let live = page_list.get("live").and_then(Json::as_str).unwrap_or("");
    if !live.starts_with("Page 1 of 2") {
        return Err(format!(
            "the page list did not announce the first page before the press (live = {live:?})"
        ));
    }

    // ── The rail ─────────────────────────────────────────────────────────
    let rail = report
        .get("rail")
        .and_then(Json::as_object)
        .ok_or_else(|| "the viewer reported no thumbnail rail (rail = null)".to_string())?;
    if rail.get("present").and_then(Json::as_bool) != Some(true) {
        return Err("the thumbnail rail is not mounted".into());
    }
    let thumbnails = rail.get("thumbnails").and_then(Json::as_u64).unwrap_or(0);
    if thumbnails == 0 {
        return Err("the thumbnail rail painted no thumbnails".into());
    }
    if rail.get("previews").and_then(Json::as_u64) != Some(thumbnails) {
        return Err(format!(
            "the rail painted {:?} preview canvases for {thumbnails} thumbnails",
            rail.get("previews")
        ));
    }
    if rail
        .get("label")
        .and_then(Json::as_str)
        .unwrap_or("")
        .is_empty()
    {
        return Err("the thumbnail rail has no accessible label".into());
    }
    if rail.get("currentPage").and_then(Json::as_str) != Some("0") {
        return Err(format!(
            "the rail did not start on the first page: {:?}",
            rail.get("currentPage")
        ));
    }
    if rail
        .get("scrollHeight")
        .and_then(Json::as_f64)
        .unwrap_or(0.0)
        <= 0.0
    {
        return Err("the thumbnail rail has no scroll extent".into());
    }

    // ── The page link and its prompt ─────────────────────────────────────
    let link = report
        .get("link")
        .and_then(Json::as_object)
        .ok_or_else(|| "the viewer reported no page link (link = null)".to_string())?;
    if link.get("found").and_then(Json::as_bool) != Some(true) {
        return Err("the page rendered no link overlay".into());
    }
    if link.get("page").and_then(Json::as_str) != Some("0") {
        return Err(format!(
            "the probe clicked a link on page {:?}, not the fixture's first page",
            link.get("page")
        ));
    }
    if link.get("actionKind").and_then(Json::as_str) != Some("uri") {
        return Err(format!(
            "the probe clicked a {:?} link, not the fixture's external URI",
            link.get("actionKind")
        ));
    }
    if link
        .get("label")
        .and_then(Json::as_str)
        .unwrap_or("")
        .is_empty()
    {
        return Err("the page link has no accessible name".into());
    }
    if link.get("hit").and_then(Json::as_bool) != Some(true) {
        return Err(format!(
            "the link rectangle is not the topmost thing at its own centre (hit role = {:?})",
            link.get("hitRole")
        ));
    }
    let prompt = link
        .get("prompt")
        .and_then(Json::as_object)
        .ok_or_else(|| "the page link opened no external-link prompt".to_string())?;
    if prompt
        .get("title")
        .and_then(Json::as_str)
        .unwrap_or("")
        .is_empty()
    {
        return Err("the external-link prompt has no title".into());
    }
    let body = prompt.get("body").and_then(Json::as_str).unwrap_or("");
    if !body.contains("https://example.com/a") {
        return Err(format!(
            "the prompt did not name the destination verbatim: {body:?}"
        ));
    }
    if prompt.get("host").and_then(Json::as_str) != Some("example.com") {
        return Err(format!(
            "the prompt did not name example.com as the host: {:?}",
            prompt.get("host")
        ));
    }
    if link.get("closed").and_then(Json::as_bool) != Some(true) {
        return Err("cancelling the external-link prompt did not close it".into());
    }

    // ── The press ────────────────────────────────────────────────────────
    let press = report
        .get("press")
        .and_then(Json::as_object)
        .ok_or_else(|| "the viewer reported no press (press = null)".to_string())?;
    if press.get("attempted").and_then(Json::as_bool) != Some(true) {
        return Err("the probe never pressed the second outline row".into());
    }
    if press.get("page").and_then(Json::as_str) != Some("1") {
        return Err(format!(
            "the probe pressed page {:?}, not the fixture's second page",
            press.get("page")
        ));
    }
    let changed = press
        .get("changed")
        .and_then(Json::as_object)
        .ok_or_else(|| "the press carried no changed flags".to_string())?;
    for witness in [
        "ink",
        "live",
        "navLive",
        "currentRow",
        "railCurrent",
        "scrollTop",
    ] {
        if changed.get(witness).and_then(Json::as_bool) != Some(true) {
            return Err(format!(
                "pressing the bookmark did not change `{witness}`; six witnesses are what \
                 separates a move from a repaint"
            ));
        }
    }
    let before = press
        .get("before")
        .and_then(Json::as_object)
        .ok_or_else(|| "the press carried no before snapshot".to_string())?;
    let after = press
        .get("after")
        .and_then(Json::as_object)
        .ok_or_else(|| "the press carried no after snapshot".to_string())?;
    if before.get("currentRow").and_then(Json::as_i64) != Some(0)
        || after.get("currentRow").and_then(Json::as_i64) != Some(1)
    {
        return Err(format!(
            "the current outline row did not move from the first to the second: {:?} -> {:?}",
            before.get("currentRow"),
            after.get("currentRow")
        ));
    }
    if before.get("currentTile").and_then(Json::as_str) != Some("Page 1 of 2")
        || after.get("currentTile").and_then(Json::as_str) != Some("Page 2 of 2")
    {
        return Err(format!(
            "the current page tile did not move from page 1 to page 2: {:?} -> {:?}",
            before.get("currentTile"),
            after.get("currentTile")
        ));
    }
    if before.get("railCurrent").and_then(Json::as_str) != Some("0")
        || after.get("railCurrent").and_then(Json::as_str) != Some("1")
    {
        return Err(format!(
            "the rail's current page did not move from 0 to 1: {:?} -> {:?}",
            before.get("railCurrent"),
            after.get("railCurrent")
        ));
    }
    let before_top = before
        .get("scrollTop")
        .and_then(Json::as_f64)
        .unwrap_or(0.0);
    let after_top = after.get("scrollTop").and_then(Json::as_f64).unwrap_or(0.0);
    if after_top <= before_top {
        return Err(format!(
            "the reader did not scroll to the second page (scrollTop {before_top} -> {after_top})"
        ));
    }
    let announced = after.get("navLive").and_then(Json::as_str).unwrap_or("");
    if announced.is_empty() {
        return Err("the move was silent: the navigation live region announced nothing".into());
    }

    // ── The stage's key map ─────────────────────────────────────────────
    let key = report
        .get("key")
        .and_then(Json::as_object)
        .ok_or_else(|| "the viewer reported no key probe (key = null)".to_string())?;
    if key.get("attempted").and_then(Json::as_bool) != Some(true) {
        return Err("the probe never pressed a key on the stage".into());
    }
    if key.get("prevented").and_then(Json::as_bool) != Some(true) {
        return Err("the stage did not claim ArrowUp; native scrolling was left in charge".into());
    }
    if key.get("moved").and_then(Json::as_bool) != Some(true) {
        return Err("ArrowUp on the stage did not move the reader".into());
    }
    let key_before = key
        .get("before")
        .and_then(Json::as_object)
        .ok_or_else(|| "the key probe carried no before snapshot".to_string())?;
    let key_after = key
        .get("after")
        .and_then(Json::as_object)
        .ok_or_else(|| "the key probe carried no after snapshot".to_string())?;
    if key_before.get("currentRow").and_then(Json::as_i64) != Some(1)
        || key_after.get("currentRow").and_then(Json::as_i64) != Some(0)
        || key_after.get("railCurrent").and_then(Json::as_str) != Some("0")
    {
        return Err(format!(
            "ArrowUp did not return the outline and rail to the first page: rows {:?} -> {:?}, \
             rail {:?} -> {:?}",
            key_before.get("currentRow"),
            key_after.get("currentRow"),
            key_before.get("railCurrent"),
            key_after.get("railCurrent")
        ));
    }
    let key_before_top = key_before
        .get("scrollTop")
        .and_then(Json::as_f64)
        .unwrap_or(0.0);
    let key_after_top = key_after
        .get("scrollTop")
        .and_then(Json::as_f64)
        .unwrap_or(0.0);
    if key_after_top >= key_before_top {
        return Err(format!(
            "ArrowUp did not scroll back to the first page (scrollTop {key_before_top} -> \
             {key_after_top})"
        ));
    }
    let key_ink_hash = |side: &serde_json::Map<String, Json>| -> Option<String> {
        side.get("ink")
            .and_then(|ink| ink.get("hash"))
            .and_then(Json::as_str)
            .map(str::to_owned)
    };
    if key_ink_hash(key_before).is_none() || key_ink_hash(key_before) == key_ink_hash(key_after) {
        return Err(format!(
            "ArrowUp did not repaint the previous page (ink hash {:?} -> {:?})",
            key_ink_hash(key_before),
            key_ink_hash(key_after)
        ));
    }
    // The pixels. The probe samples a fixed stride and hashes the sampled
    // RGBA; `inkPixels` counts sampled pixels that differ from the page
    // background, because the canvas is opaque and alpha cannot say that.
    let ink_pixels = |side: &serde_json::Map<String, Json>| -> u64 {
        side.get("ink")
            .and_then(|ink| ink.get("inkPixels"))
            .and_then(Json::as_u64)
            .unwrap_or(0)
    };
    if ink_pixels(before) == 0 || ink_pixels(after) == 0 {
        return Err(format!(
            "the canvas was blank before or after the press (ink pixels {} -> {})",
            ink_pixels(before),
            ink_pixels(after)
        ));
    }
    let hash = |side: &serde_json::Map<String, Json>| -> Option<String> {
        side.get("ink")
            .and_then(|ink| ink.get("hash"))
            .and_then(Json::as_str)
            .map(str::to_string)
    };
    match (hash(before), hash(after)) {
        (Some(before), Some(after)) if before != after => {}
        (before, after) => {
            return Err(format!(
                "the canvas shows the same pixels after the press ({before:?} -> {after:?}) -- \
                 the page on screen did not change"
            ));
        }
    }

    Ok(())
}

/// `service-worker`: the *browser* has to make this true, not the page.
///
/// The page registers `/sw.js`, waits for the registration to become active,
/// waits for `clients.claim()` to hand it a controller, then fetches an asset
/// the worker precached during `install`. Every one of those is the engine
/// doing something the Node double in `apps/web/host` has to fake, which is
/// precisely the list WEB.02's status note called unverified.
fn verdict_service_worker(report: &Json) -> Result<(), String> {
    well_formed(report)?;
    let scope = string_field(report, "scope")?;
    if !scope.starts_with("http://127.0.0.1:") {
        return Err(format!(
            "the worker registered at {scope}, not on the loopback origin it was served from"
        ));
    }
    let controlled = report.get("controlled").and_then(Json::as_bool);
    if controlled != Some(true) {
        return Err(format!(
            "`clients.claim()` did not hand the page a controller (controlled = {controlled:?}) -- \
             the activate handler ran but the claim did not take effect"
        ));
    }
    let asset = string_field(report, "asset")?;
    if asset != ASSET_BODY {
        return Err(format!(
            "the precached asset came back as {asset:?}, expected {ASSET_BODY:?} -- \
             `addAll` at install or `respondWith` at fetch did not deliver"
        ));
    }
    Ok(())
}

/// `async-wasm`: a real engine really ran a real module, and really linked it.
///
/// Two claims, not one. The `sum` is arithmetic only a WebAssembly engine in
/// a browser can have done here, and the `linkError` is the WebAssembly
/// *link* check firing — an unsatisfied import must fail before the module
/// ever runs, which is a property of the engine rather than of the page.
fn verdict_async_wasm(report: &Json) -> Result<(), String> {
    well_formed(report)?;
    let sum = report
        .get("sum")
        .and_then(Json::as_i64)
        .ok_or_else(|| format!("the page reported no `sum` (report: {report})"))?;
    if sum != WASM_PROBE_SUM {
        return Err(format!(
            "the module returned {sum}, expected {WASM_PROBE_SUM} -- the engine did not run it"
        ));
    }
    let link = string_field(report, "linkError")?;
    if !link.contains("LinkError") {
        return Err(format!(
            "instantiating without the module's import gave {link:?}, not a LinkError -- \
             the engine is not enforcing the import list"
        ));
    }
    Ok(())
}

/// Run the harness: the committed check set, or one ad-hoc page.
///
/// Returns `Ok(())` for a clean pass *and* for a skip when no engine was
/// found (see the module docs); every other outcome is an `Err` naming what
/// failed.
pub(crate) fn run(cfg: &Config) -> Result<(), String> {
    let browser = match resolve_browser(cfg.browser.as_deref()) {
        Ok(found) => found,
        Err(probed) => return skip(cfg, &probed),
    };
    println!(
        "browser-check: engine {} ({})",
        browser_name(&browser),
        browser.display()
    );

    let scratch = scratch_dir();
    let _ = std::fs::remove_dir_all(&scratch);
    std::fs::create_dir_all(&scratch).map_err(|e| format!("{}: {e}", scratch.display()))?;

    let rows = match cfg.page.as_deref() {
        Some(page) => vec![run_ad_hoc(cfg, &browser, &scratch, page)?],
        None => run_checks(cfg, &browser, &scratch),
    };

    if !cfg.keep {
        let _ = std::fs::remove_dir_all(&scratch);
    } else {
        println!("browser-check: kept {}", scratch.display());
    }
    summarise(cfg, &browser, &rows)
}

/// The `xtask/browser/` directory the committed checks live in.
fn check_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("browser")
}

/// The transient working directory the pages are served from and the engine
/// profile is written into.
///
/// Deliberately the system temp directory rather than a sibling of the report.
/// A browser profile is thousands of small files written in a few hundred
/// milliseconds, and creating one inside the repository's `target/` was measured
/// here to be slow enough that the engine never finished starting inside a
/// 30 s budget — the page simply never loaded, on the same machine where the
/// identical check passed milliseconds later from `%TEMP%`. The report stays
/// where the caller asked for it, because that is an artifact worth keeping;
/// the scratch does not, and putting ephemeral state in a build directory is
/// the wrong trade in any case.
fn scratch_dir() -> PathBuf {
    let unique = unique_profile(Path::new(""), "run");
    let name = unique
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "scratch".to_string());
    std::env::temp_dir().join("selis-browser-check").join(name)
}

/// Resolve the engine, or hand back the message naming every path probed.
fn resolve_browser(explicit: Option<&Path>) -> Result<PathBuf, String> {
    if let Some(path) = explicit {
        return if path.is_file() {
            Ok(path.to_path_buf())
        } else {
            Err(format!(
                "--browser {} is not a file (point it at a Chrome/Chromium/Edge binary)",
                path.display()
            ))
        };
    }
    let candidates = browser_candidates();
    for path in &candidates {
        if path.is_file() {
            return Ok(path.clone());
        }
    }
    Err(format!(
        "no headless browser found. Install one (a distro `chromium` is the \
         licence-cleanest choice), or pass --browser <path> / set $SELIS_BROWSER. \
         Probed: {}",
        candidates
            .iter()
            .map(|p| p.display().to_string())
            .collect::<Vec<_>>()
            .join(", ")
    ))
}

/// Run the committed checks, or the one `--only` named.
fn run_checks(cfg: &Config, browser: &Path, scratch: &Path) -> Vec<Row> {
    let selected: Vec<&Check> = match cfg.only.as_deref() {
        Some(only) => CHECKS.iter().filter(|c| c.id == only).collect(),
        None => CHECKS.iter().collect(),
    };
    if selected.is_empty() {
        let ids: Vec<&str> = CHECKS.iter().map(|c| c.id).collect();
        return vec![Row {
            id: cfg.only.clone().unwrap_or_default(),
            status: Status::Fail,
            detail: format!("--only named no such check; the committed checks are {ids:?}"),
            report: None,
        }];
    }
    selected
        .iter()
        .map(|check| run_check(cfg, browser, scratch, check))
        .collect()
}

/// Serve one check's files on a loopback origin, drive the engine at them, and
/// turn what the page reported into a verdict.
fn run_check(cfg: &Config, browser: &Path, scratch: &Path, check: &Check) -> Row {
    let served = scratch.join("pages").join(check.dir);
    if let Err(e) = stage(&check_root().join(check.dir), &served) {
        return failed(check.id, format!("cannot stage the check: {e}"));
    }
    // `app-shell` drives the app's OWN worker; `chrome` imports the built
    // React entries. Both need the built app on the same origin as the page.
    if check.id == "app-shell" || check.id == "chrome" || check.id == "tools-page" {
        if let Err(e) = stage_app(&app_public_dir(), &served) {
            return failed(check.id, format!("cannot stage the built web app: {e}"));
        }
        // The app ships its OWN index.html, and `copy_tree` copies in file
        // order, so staging the app after the check OVERWRITES the page that
        // reports the verdict. Found by running it: the browser loaded the app
        // shell, the harness script never executed, and the check timed out
        // looking like a harness fault. The page has to win, so it is re-staged
        // last and then verified to be the page that is actually on disk.
        if let Err(e) = stage(&check_root().join(check.dir), &served) {
            return failed(check.id, format!("cannot re-stage the check page: {e}"));
        }
        let page = served.join("index.html");
        // `run_check` returns a `Row`, not a `Result`, so this read cannot use
        // `?`; an unreadable staged page is a failed check, not a propagated
        // error, and a row in the report is more useful than an abort.
        let text = match std::fs::read_to_string(&page) {
            Ok(t) => t,
            Err(e) => {
                return failed(
                    check.id,
                    format!("cannot read the staged page {}: {e}", page.display()),
                )
            }
        };
        if !text.contains("@@SELIS-RESULT@@") {
            return failed(
                check.id,
                "the staged index.html is not the harness page - the app's own \
                 index.html shadowed it, so nothing would report a verdict"
                    .to_string(),
            );
        }
        // SL-4.UI.07's axe-core scan. Copied from `node_modules` into the STAGED
        // fixture only, and never into `public/` or the committed fixture
        // directory.
        //
        // Both of those exclusions are the point rather than tidiness. Into
        // `public/` would put a third-party script on the document-handling
        // path, which ADR-P0016 forbids outright. Into the committed fixture
        // would commit a ~500 KB blob nobody reads, whose version nobody tracks
        // against the declared dependency - so the copy is made HERE, from the
        // declared `axe-core` devDependency, every run.
        if let Err(e) = stage_axe(&served) {
            return failed(
                check.id,
                format!("cannot stage axe-core for the accessibility scan: {e}"),
            );
        }
    }
    // The extension leg is the only check whose subject is a package rather
    // than a page: it loads `apps/extension/dist` as an unpacked extension and
    // serves only the fixture and the probe. Staged here, before the launch,
    // so a missing extension build is a named failure rather than a browser
    // that silently never runs the viewer.
    let mut extension: Option<PathBuf> = None;
    if check.id == "extension" {
        let package = scratch.join("packages").join("extension");
        if let Err(e) = stage_extension(
            &extension_dist_dir(),
            &package,
            &check_root().join(check.dir),
        ) {
            return failed(check.id, format!("cannot stage the packed extension: {e}"));
        }
        if let Err(e) = stage_extension_launcher(&served) {
            return failed(
                check.id,
                format!("cannot stage the extension launcher page: {e}"),
            );
        }
        extension = Some(package);
    }
    let launch_page = if check.id == "extension" {
        Some("start.html")
    } else {
        None
    };
    let mut profile = unique_profile(scratch, check.id);
    println!("browser-check: {} ...", check.id);
    let mut measured = measure(
        browser,
        &served,
        &profile,
        cfg.timeout_ms,
        launch_page,
        extension.as_deref(),
    );
    // Measured at most twice. A page that POSTs a report has been measured and
    // is judged as reported; a page that reports nothing gave this gate no
    // verdict at all, which is a measurement failure and not a judgement - so
    // the measurement (never the verdict) is taken again once, on a fresh
    // engine profile. First-time cold runners can deliver an unmeasured window
    // that a second real invocation does not (the same fixture page that timed
    // out unmeasured in CI passed its own self-test on the same runner image).
    // A page that is silent through both windows still records `unmeasured`
    // and the gate stays red.
    if matches!(&measured, Err(why) if why.contains("did not report within")) {
        println!(
            "browser-check: {} ... unmeasured; measuring once more (fresh engine profile)",
            check.id
        );
        profile = unique_profile(scratch, check.id);
        measured = measure(
            browser,
            &served,
            &profile,
            cfg.timeout_ms,
            launch_page,
            extension.as_deref(),
        );
    }
    match measured {
        Err(why) => Row {
            id: check.id.to_string(),
            status: Status::Unmeasured,
            detail: why,
            report: None,
        },
        Ok(report) => match (check.verdict)(&report) {
            Ok(()) => {
                println!("browser-check: {} ... pass", check.id);
                Row {
                    id: check.id.to_string(),
                    status: Status::Pass,
                    detail: String::new(),
                    report: Some(report),
                }
            }
            Err(why) => {
                println!("browser-check: {} ... FAIL: {why}", check.id);
                Row {
                    id: check.id.to_string(),
                    status: Status::Fail,
                    detail: why,
                    report: Some(report),
                }
            }
        },
    }
}

/// A row for a check that could not even be started.
fn failed(id: &str, detail: String) -> Row {
    println!("browser-check: {id} ... FAIL: {detail}");
    Row {
        id: id.to_string(),
        status: Status::Fail,
        detail,
        report: None,
    }
}

/// Run one page the caller named, with no verdict beyond "it reported".
///
/// This is the reusable half of the harness: point it at any page that POSTs
/// its JSON to `/__selis_result` and it prints what came back.
fn run_ad_hoc(cfg: &Config, browser: &Path, scratch: &Path, page: &Path) -> Result<Row, String> {
    if !page.exists() {
        return Err(format!("--page {} does not exist", page.display()));
    }
    let (root, _entry) = if page.is_dir() {
        (page.to_path_buf(), String::new())
    } else {
        (
            page.parent()
                .unwrap_or_else(|| Path::new("."))
                .to_path_buf(),
            page.file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default(),
        )
    };
    let id = format!("page:{}", page.display());
    let profile = unique_profile(scratch, "ad-hoc");
    println!("browser-check: {id} ...");
    match measure(browser, &root, &profile, cfg.timeout_ms, None, None) {
        Ok(report) => {
            println!("browser-check: {id} reported {report}");
            Ok(Row {
                id,
                status: Status::Pass,
                detail: String::new(),
                report: Some(report),
            })
        }
        Err(why) => {
            println!("browser-check: {id} ... {why}");
            Ok(Row {
                id,
                status: Status::Unmeasured,
                detail: why,
                report: None,
            })
        }
    }
}

/// Copy a check's committed files into the directory that gets served.
fn stage(from: &Path, to: &Path) -> Result<(), String> {
    let entries = std::fs::read_dir(from).map_err(|e| format!("{}: {e}", from.display()))?;
    std::fs::create_dir_all(to).map_err(|e| format!("{}: {e}", to.display()))?;
    for entry in entries.flatten() {
        let src = entry.path();
        // Checks are flat on purpose: a committed page, a worker, an asset.
        if !src.is_file() {
            continue;
        }
        let dest = to.join(entry.file_name());
        std::fs::copy(&src, &dest).map_err(|e| format!("{}: {e}", dest.display()))?;
    }
    Ok(())
}

/// Where `pnpm build` in `apps/web/host` leaves the servable app.
///
/// Not committed, and deliberately not built by this xtask: a Rust harness
/// shelling out to a Node toolchain would make the browser gate depend on pnpm
/// being present, and the point of this check is the browser, not the build. CI
/// builds the host first; a developer runs `pnpm build` in the host once.
/// What `xtask/browser/app-shell/index.html` must show for WEB.02's app-level
/// half to count as proven.
///
/// The split with `verdict_service_worker` is deliberate and is the whole
/// reason this second check exists. That one proves the ENGINE: that a real
/// browser delivers install/activate, hands over a controller, and enforces
/// `respondWith`. This one proves the APP'S OWN WORKER does those things, with
/// the app's real precache list, against the app's real policy module - and
/// that the shell's own boot path is what registered it.
///
/// The failure this catches is one the engine check cannot see by construction:
/// a precache list naming a path the build does not produce. `addAll` is
/// atomic, so install fails as a whole, the app stays permanently online-only,
/// and nothing anywhere reports an error - the app simply works, online, forever.
fn verdict_app_shell(report: &Json) -> Result<(), String> {
    well_formed(report)?;

    // The page must have run the APP's boot module, not a stand-in. If this is
    // null the page never loaded /assets/boot.js, and every field below would be
    // about the harness rather than about the app.
    if string_field(report, "boot")? != "web-host" {
        return Err(format!(
            "the page did not run the app's own boot module (boot = {:?}) -- this \
             check is driving the harness, not apps/web/host",
            report.get("boot")
        ));
    }

    // Registration must have SUCCEEDED, and the page must say so in the app's own
    // vocabulary. `sw-register.js` reports a refusal reason rather than
    // throwing, so a refusal here is a real, nameable condition.
    if string_field(report, "registration")? != "registered" {
        return Err(format!(
            "the app's own registration path reported {:?}, not `registered`",
            report.get("registration")
        ));
    }

    // A controller means clients.claim() ran in the APP worker, not just in the
    // harness's stand-in.
    if report.get("controlled").and_then(Json::as_bool) != Some(true) {
        return Err(format!(
            "the app worker never claimed the page (controlled = {:?}) -- \
             clients.claim() in apps/web/host/public/sw.js did not take effect",
            report.get("controlled")
        ));
    }

    // The precached shell asset came back from the worker's respondWith, and it
    // is the app's boot module. A 404 here is the atomic-addAll failure above.
    if string_field(report, "shellAsset")? != "app-boot" {
        return Err(format!(
            "the worker served a shell asset that is not the app's boot module \
             (shellAsset = {:?})",
            report.get("shellAsset")
        ));
    }

    // The second precached path, asked for the way the DoD cares about. This is
    // the closest machine-checkable form of "airplane mode" available without a
    // network-interception harness: a no-store request for a path the worker
    // precached, which only the worker's own cache can answer.
    if report.get("precacheHit").and_then(Json::as_bool) != Some(true) {
        return Err(format!(
            "the app worker did not answer for a precached asset (precacheHit = \
             {:?}) -- the shell is not offline-capable",
            report.get("precacheHit")
        ));
    }

    // The viewer must be MOUNTED, not merely shipped. This is the assertion
    // that WEB.02's blocker 2 is closed: until now `@selis/ui` was copied into
    // the deployment and imported by nobody, so the app booted, registered its
    // worker, served its precached shell and rendered a placeholder. Every
    // assertion above would have passed on that app.
    if report.get("appMounted").and_then(Json::as_bool) != Some(true) {
        return Err(format!(
            "the app never mounted the viewer (appMounted = {:?}, appError = {:?}) -- \
             the engine and UI are shipped into public/ but nothing loads them, \
             so the page is a shell with a placeholder",
            report.get("appMounted"),
            report.get("appError")
        ));
    }

    // The engine must be a real engine carrying the entry point the app calls.
    // An export count alone would pass on a stub module.
    if report.get("engineDispatch").and_then(Json::as_bool) != Some(true) {
        return Err(format!(
            "the mounted engine has no `selis_dispatch` export (engineDispatch = \
             {:?}, exports = {:?}) -- the app cannot open a PDF with it",
            report.get("engineDispatch"),
            report.get("engineExports")
        ));
    }

    // The viewer's own logic must have produced something. Zero pages means the
    // module graph imported but is not usable.
    match report.get("windowedPages").and_then(Json::as_u64) {
        Some(pages) if pages > 0 => {}
        other => {
            return Err(format!(
                "the viewer's windowing produced no pages (windowedPages = {other:?}) -- \
                 the UI modules loaded but are not usable"
            ));
        }
    }
    // The engine must OPEN A DOCUMENT and return rendered pixels. Everything
    // above proves the machinery is present; this is the DoD's first verb, and
    // it is the only assertion here an engine cannot satisfy by merely loading.
    // An engine that instantiates and then refuses every document passes all of
    // the assertions above.
    let open = report
        .get("open")
        .and_then(|value| value.as_object())
        .ok_or_else(|| "the app never reported an open result (open = None)".to_string())?;
    if open.get("status").and_then(Json::as_str) != Some("ok") {
        return Err(format!(
            "the app could not open the fixture PDF through the engine (open = {open:?}) -- \
             the engine is present but does not open documents"
        ));
    }

    // A rendered page must have INK, and here the count is EXACT rather than a
    // threshold. A page can render successfully and come back entirely white,
    // which is the most dangerous shape this check has to catch: measured on 60
    // real PDFs from a local Downloads folder, 43 opened and 17 of those
    // rendered with zero non-white pixels. `status: ok` alone is explicitly not
    // enough. The fixture is a single 200x100 filled rectangle, so the engine
    // drawing exactly 20000 pixels means it drew the geometry the document asked
    // for, and any other number means it drew something else.
    const EXPECTED_INK: u64 = 200 * 100;
    match open.get("ink").and_then(Json::as_u64) {
        Some(ink) if ink == EXPECTED_INK => {}
        Some(ink) => {
            return Err(format!(
                "the engine rendered the page with {ink} non-white pixels, expected exactly \
                 {EXPECTED_INK} - the fixture's 200x100 rectangle. A close but unequal \
                 count means something was drawn that the document did not ask for; a \
                 count of 0 is a blank page."
            ));
        }
        None => return Err("the app reported no `ink` for the rendered page".to_string()),
    }

    // The app's OWN assets must answer from the worker's cache, not just the
    // precached shell. `PRECACHE_PATHS` is four shell files; the viewer modules
    // and the engine the app actually needs are RUNTIME-cached, and those are
    // what "open a local PDF in airplane mode" depends on. Without this the
    // check passes on an app that works online and 503s offline — the exact
    // failure the DoD names.
    let offline = report.get("offlineAssets").and_then(|value| value.as_array()).ok_or_else(
		|| "the app never reported which of its own assets the worker served (offlineAssets = None)"
			.to_string(),
	)?;
    let names = ["viewer/layout.js", "viewer/windowing.js", "the engine"];
    for (index, name) in names.iter().enumerate() {
        if offline.get(index).and_then(Json::as_bool) != Some(true) {
            return Err(format!(
                "the worker did not serve {name} from its cache -- the app runs online and \
                 fails with the network cut, which is the DoD's own failure mode"
            ));
        }
    }

    // Not merely a 200: the bytes have to be the real ones. A worker answering
    // with the right length and the wrong content would pass a status-only
    // check, which is the shape of a green gate that proves nothing.
    if report.get("offlineLayoutReal").and_then(Json::as_bool) != Some(true)
        || report.get("offlineWindowingReal").and_then(Json::as_bool) != Some(true)
    {
        return Err(
            "the worker served the viewer modules from cache but the bytes are not the real \
             modules"
                .to_string(),
        );
    }
    // The optimised engine is ~2.3 MB, so the order of magnitude is itself the
    // assertion: a stub or a truncated body is far smaller.
    let engine_bytes = report
        .get("offlineEngineBytes")
        .and_then(Json::as_u64)
        .unwrap_or(0);
    if engine_bytes < 1_000_000 {
        return Err(format!(
            "the worker served only {engine_bytes} bytes of engine from cache, expected the \
             ~2.3 MB optimised module"
        ));
    }

    // The DoD's second verb, and the one that was missing until now. Every
    // previous check could be satisfied by an app that rendered into linear
    // memory and stopped: `ink` counts the ENGINE's buffer, which says nothing
    // about whether a user ever saw a page. So the page must now be on a canvas
    // in the document, and the browser's own readback must match the engine's
    // output exactly.
    let view = report
        .get("view")
        .ok_or_else(|| "the app never reported a painted page (view = None)".to_string())?;
    let screen_ink = view
        .get("screenInk")
        .and_then(Json::as_u64)
        .ok_or_else(|| "the painted page reported no screen ink".to_string())?;
    if screen_ink == 0 {
        return Err(
            "the page was painted but the canvas is blank -- the engine drew ink and the screen \
             does not show it"
                .to_string(),
        );
    }
    // Equality, not "> 0". A canvas showing one stray pixel would satisfy a
    // presence check; matching the engine's own count is what ties the screen to
    // the render, and it is the assertion that fails if `putImageData` is ever
    // given the wrong stride or row order.
    let engine_ink = report
        .get("open")
        .and_then(|open| open.get("ink"))
        .and_then(Json::as_u64)
        .unwrap_or(0);
    if screen_ink != engine_ink {
        return Err(format!(
            "the canvas shows {screen_ink} ink pixels but the engine rendered {engine_ink} -- the \
             page on screen is not the page the engine drew"
        ));
    }
    // The canvas must be the page's real size. A viewer that silently scaled the
    // page to fit would look right and measure wrong.
    let (Some(cw), Some(ch)) = (
        view.get("w").and_then(Json::as_u64),
        view.get("h").and_then(Json::as_u64),
    ) else {
        return Err("the painted page reported no dimensions".to_string());
    };
    let page_w = report
        .get("open")
        .and_then(|open| open.get("w"))
        .and_then(Json::as_u64)
        .unwrap_or(0);
    let page_h = report
        .get("open")
        .and_then(|open| open.get("h"))
        .and_then(Json::as_u64)
        .unwrap_or(0);
    if cw != page_w || ch != page_h {
        return Err(format!(
            "the canvas is {cw}x{ch} but the page is {page_w}x{page_h} -- the page is not shown at \
             its own size"
        ));
    }

    // The DoD's third verb: search, against a document that actually contains
    // text. Three legs, and each is here to stop a specific way of faking it.
    //
    // The hit must be the EXACT known count. This fixture draws one word once,
    // so "at least one result" would pass on an implementation that matched
    // everything.
    let hit = report
        .get("searchHit")
        .ok_or_else(|| "the app never ran a search (searchHit = None)".to_string())?;
    let hit_total = hit
        .get("total")
        .and_then(Json::as_u64)
        .ok_or_else(|| "the search reported no total".to_string())?;
    if hit.get("status").and_then(Json::as_str) != Some("ok") {
        return Err(format!("the search did not run: {hit}"));
    }
    if hit_total != 1 {
        return Err(format!(
            "searching for a word the document contains once returned {hit_total} results, \
             expected exactly 1 -- the query is not reaching the text layer"
        ));
    }
    // The match must carry the document's own words back, not just a count.
    // A stub could satisfy the count and return nothing to highlight.
    let matched = hit
        .get("matches")
        .and_then(Json::as_array)
        .and_then(|matches| matches.first())
        .and_then(|m| m.get("text"))
        .and_then(Json::as_str)
        .unwrap_or("");
    if !matched.contains("search") {
        return Err(format!(
            "the search found {hit_total} result(s) but the matched text is {matched:?} -- a count \
             with nothing behind it"
        ));
    }

    // The discriminating leg: a word that is NOT in the document must come back
    // empty. Without this the whole search verdict is satisfiable by an
    // implementation that ignores its query and always answers "found".
    let miss = report
        .get("searchMiss")
        .ok_or_else(|| "the app never ran the control search (searchMiss = None)".to_string())?;
    let miss_total = miss.get("total").and_then(Json::as_u64).unwrap_or(u64::MAX);
    if miss_total != 0 {
        return Err(format!(
            "searching for a word the document does NOT contain returned {miss_total} results -- \
             the search answers \"found\" regardless of the query"
        ));
    }

    // Case-insensitivity is the engine's documented default; if it silently
    // became case-sensitive this is where it shows.
    let upper = report
        .get("searchCase")
        .and_then(|reply| reply.get("total"))
        .and_then(Json::as_u64)
        .unwrap_or(0);
    if upper != 1 {
        return Err(format!(
            "an uppercase spelling of a word present in lowercase returned {upper} results, \
             expected 1 -- case folding is not working"
        ));
    }

    // The verdict is not a return value the user never sees: the result line
    // must actually say what happened, or this is an API and not a search.
    let readout = report
        .get("searchReadout")
        .and_then(Json::as_str)
        .unwrap_or("");
    if !readout.contains("1 result") {
        return Err(format!(
            "the search worked but the page shows {readout:?} -- a user would not learn that \
             their query was found"
        ));
    }

    // The DoD's fourth verb: print. The control is reached, the stylesheet that
    // decides the printed output is the real one, and - asserted further down,
    // where the generated PDF can be examined - the button produces an artifact
    // rather than handing the document to the browser's own print pipeline.
    // `printCalls` used to be checked HERE for `== 1`, which asserted the
    // opposite of UI.08's Do:; it is now checked for `== 0` alongside the
    // artifact, where both halves of the requirement sit together.

    // Read through the CSSOM, not by grepping the file: this asks what the
    // browser actually parsed, so a stylesheet that failed to load or was
    // overridden later cannot pass.
    let rules = report
        .get("printRuleCount")
        .and_then(Json::as_u64)
        .unwrap_or(0);
    if rules == 0 {
        return Err(
            "the app served no @media print rules -- printing would emit the search box, the \
             result line and the status text along with the page"
                .to_string(),
        );
    }
    if report.get("printHidesChrome").and_then(Json::as_bool) != Some(true) {
        return Err(
            "the print stylesheet never hides the shell's own controls -- a printed page would \
             carry the search box and the status line"
                .to_string(),
        );
    }
    if report.get("printKeepsPage").and_then(Json::as_bool) != Some(true) {
        return Err(
            "the print stylesheet does not keep the page visible at its own size -- printing \
             would emit a blank sheet"
                .to_string(),
        );
    }

    // THE CUT: origin refusing, worker answering, DoD's sentence re-run.
    if report.get("originRefuses").and_then(Json::as_bool) != Some(true) {
        return Err(
            "with the cut armed an uncached path still succeeded -- not an offline run".into(),
        );
    }
    let cut = report
        .get("offlineCutAssets")
        .and_then(Json::as_array)
        .ok_or_else(|| "offlineCutAssets = None".to_string())?;
    let names = [
        "the precached shell",
        "the precached index",
        "the precached stylesheet",
        "the precached boot",
        "layout.js",
        "windowing.js",
        "the engine",
    ];
    for (i, name) in names.iter().enumerate() {
        if cut.get(i).and_then(Json::as_bool) != Some(true) {
            return Err(format!("{name} is not served with the origin refusing"));
        }
    }
    let bytes = report
        .get("offlineCutEngineBytes")
        .and_then(Json::as_u64)
        .unwrap_or(0);
    if bytes < 1_000_000 {
        return Err(format!(
            "offline the engine came back as {bytes} bytes, expected ~2.3 MB"
        ));
    }
    let ink = report
        .get("offlineCutView")
        .and_then(|v| v.get("screenInk"))
        .and_then(Json::as_u64)
        .unwrap_or(0);
    let expected = report
        .get("open")
        .and_then(|o| o.get("ink"))
        .and_then(Json::as_u64)
        .unwrap_or(0);
    if ink == 0 || ink != expected {
        return Err(format!(
            "offline the canvas shows {ink} ink pixels, expected {expected}"
        ));
    }
    let hit = report
        .get("offlineCutSearch")
        .and_then(|r| r.get("total"))
        .and_then(Json::as_u64)
        .unwrap_or(u64::MAX);
    let miss = report
        .get("offlineCutMiss")
        .and_then(|r| r.get("total"))
        .and_then(Json::as_u64)
        .unwrap_or(0);
    if hit != 1 || miss != 0 {
        return Err(format!(
            "offline search returned {hit} / {miss}, expected 1 / 0"
        ));
    }
    // UI.08: the Do: forbids relying on the browser's own PDF printing, so
    // reaching `window.print` is the FAILURE this leg exists to catch, not the
    // success it used to be asserted as. Both the online button (1d) and the
    // offline one must leave it untouched and produce a real PDF instead.
    let button = report.get("printButton").ok_or_else(|| {
        "the app's Print button reported nothing (printButton = None)".to_string()
    })?;
    assert_print_artifact(button, "the Print button")?;
    if report.get("printCalls").and_then(Json::as_u64) != Some(0) {
        return Err(
            "the Print button reached window.print, which is exactly what UI.08 forbids -- \
the browser's print pipeline decides the paper, the margins and the resolution"
                .into(),
        );
    }
    if report.get("offlineCutPrintCalls").and_then(Json::as_u64) != Some(0) {
        return Err("offline the Print button reached window.print".into());
    }
    // Offline the same must hold, and through the same generated artifact:
    // `print.js` and `print-pdf.js` are dynamically imported, so a precache
    // list that missed them works online and 404s with the origin refusing.
    let offline = report
        .get("offlineCutPrint")
        .ok_or_else(|| "the app's Print button reported nothing offline".to_string())?;
    assert_print_artifact(offline, "the offline Print button")?;
    // UI.09: the health panel, asserted ROW BY ROW rather than as a boolean.
    //
    // The app chose every severity, so a verdict that only checked "the panel
    // rendered" would be checking that the app agreed with itself. Each leg
    // below is a specific claim the Do: calls honest reporting.
    let health = report
        .get("health")
        .and_then(|h| h.get("rows"))
        .and_then(Json::as_array)
        .ok_or_else(|| "the app produced no health rows (health = None)".to_string())?;
    let row = |id: &str| -> Result<&Json, String> {
        health
            .iter()
            .find(|r| r.get("id").and_then(Json::as_str) == Some(id))
            .ok_or_else(|| format!("the health panel has no {id} row"))
    };
    let severity = |id: &str| -> Result<&str, String> {
        Ok(row(id)?
            .get("severity")
            .and_then(Json::as_str)
            .ok_or_else(|| format!("health row {id} carries no severity"))?)
    };

    // The signature row is the one the Do: leans on hardest. A signed document
    // must never read as `ok`, because nothing in this project verifies a
    // signature and a green tick would be the one false thing the panel can say.
    if severity("signature")? == "ok"
        && row("signature")?
            .get("label")
            .and_then(Json::as_str)
            .unwrap_or_default()
            .contains("Signed")
    {
        return Err(
            "the health panel reported a signed document as OK -- nothing here verifies a \
signature, so that is a claim the project cannot support"
                .into(),
        );
    }
    // Untagged is a `notice`, never a `warn`: most PDFs are untagged, and a
    // warning on the commonest fact in the format trains a reader to ignore the
    // row that matters.
    if severity("tagged")? == "warn" {
        return Err(
            "the health panel warned about an untagged document; that is a fact, not a defect"
                .into(),
        );
    }
    // The panel must have measured something, not rendered an empty list.
    severity("pages")?;
    severity("deviations")?;
    // And the fixture is a clean, ordinary document, so its deviations row must
    // be `ok` -- which is the whole point of the deviation pass that skips stream
    // bodies. A clean file reporting structural problems here would be the
    // 70-phantom-rows failure arriving through the UI.
    if severity("deviations")? != "ok" {
        return Err(format!(
            "the health panel reported structural problems in a clean document: {}",
            row("deviations")?
                .get("label")
                .and_then(Json::as_str)
                .unwrap_or_default()
        ));
    }

    // UI.12: a document that will not open must not become a dead end.
    //
    //     The Do: says every `Code` maps to a user-facing state WITH a recovery
    //     action. Three separate claims are asserted, and each was a real gap in
    //     the shell that preceded this:
    //
    //       1. there is a sentence, and it is the registry's / the shell's own
    //          account of a failure the engine never classified;
    //       2. the panel says what happened to the user's DOCUMENT, which the
    //          old one-line notice never did;
    //       3. there is a control that does something.
    //
    //     Asserting only "an error appeared" would be satisfied by the old
    //     behaviour and so would prove nothing.
    let failure = report.get("failure").ok_or_else(|| {
        "the app recorded no failure state for a document that would not open".to_string()
    })?;
    if failure.is_null() {
        return Err(
            "the app never rendered its failure panel for a document that would not open -- the \
shell answered this case with one line of text and nothing to do about it"
                .into(),
        );
    }
    let what = failure
        .get("message")
        .and_then(Json::as_str)
        .ok_or_else(|| "the failure panel carries no sentence".to_string())?;
    if what.trim().is_empty() {
        return Err("the failure panel rendered an empty sentence".into());
    }
    // The document's fate. A panel that says what went wrong without saying
    // whether the file survived is half an answer, and the half that matters
    // most to someone who was in the middle of an edit.
    if failure
        .get("docState")
        .and_then(Json::as_str)
        .unwrap_or_default()
        .trim()
        .is_empty()
    {
        return Err(format!(
            "the failure panel never said what happened to the document: {what}"
        ));
    }
    // A document that did not open has NOT loaded, and must not be reported as
    // anything else. `PartiallyLoaded` here would be a claim the engine never
    // made.
    if failure.get("state").and_then(Json::as_str) != Some("NotLoaded") {
        return Err(format!(
            "a document that would not open was reported as {:?}, not NotLoaded",
            failure.get("state").and_then(Json::as_str)
        ));
    }
    if failure.get("severity").and_then(Json::as_str) != Some("blocked") {
        return Err(format!(
            "a document that would not open was shown at severity {:?}, not blocked -- the \
severity has to track whether anything is readable",
            failure.get("severity").and_then(Json::as_str)
        ));
    }
    // The recovery action, and a control that is not disabled. A dead end
    // wearing a button is worse than no button, because it looks like progress.
    let action = failure
        .get("action")
        .and_then(Json::as_str)
        .ok_or_else(|| "the failure panel offers no recovery action".to_string())?;
    if action.is_empty() {
        return Err("the failure panel offers an empty recovery action".into());
    }
    if failure.get("actionDisabled").and_then(Json::as_bool) != Some(false) {
        return Err(format!(
            "the failure panel offered `{action}` but rendered it disabled"
        ));
    }
    if failure
        .get("actionLabel")
        .and_then(Json::as_str)
        .unwrap_or_default()
        .trim()
        .is_empty()
    {
        return Err("the failure panel rendered no label for its recovery action".into());
    }

    // UI.13: a long operation must show truthful progress and honour a cancel.
    //
    // Every claim is checked at the moment it was true, because a bar that is
    // honest while busy and lies once settled is still a liar, and asserting
    // only the end state would miss exactly that.
    assert_print_cancellation(report)?;

    // UI.07: the shell's controls must be operable from the keyboard.
    assert_control_keys(report)?;

    // UI.07: "axe-core clean" is the DoD, so a scan that did not run is a
    // failure rather than a skip.
    assert_accessibility(report)?;

    // UI.07: the screen-reader walk — the DoD's remaining line.
    assert_reader_walk(report)?;

    // UI.06: the bookmark outline, from the real engine op.
    assert_bookmark_outline(report)?;

    // UI.06: the navigation reads, and a bookmark button that WORKS.
    assert_navigation(report)?;

    // UI.08: print must be a GENERATED print-ready PDF, not the browser's own
    // PDF printing - the Do: rules the latter out explicitly. Asserting the
    // artifact exists is the point; `window.print()` cannot produce one.
    let printed = report
        .get("print")
        .ok_or_else(|| "the app produced no print artifact (print = None)".to_string())?;
    assert_print_artifact(printed, "__selisPrint")?;

    // MULTI-PAGE, and the leg that can actually catch it. A one-page document
    // cannot: "render page 0 and stop" produces an identical result on one
    // page, so every assertion on it passes for an implementation that drops
    // every page after the first. The fixture here has two, so the count, the
    // per-page iteration and the second page''s own resolution are all
    // observable - and the count is the ENGINE''s, read from its `open` reply.
    if printed.get("pages").and_then(Json::as_u64) != Some(2) {
        return Err(format!(
            "the two-page document was printed as {:?} page(s); the count comes from the engine''s \
             open reply, so this is a count being dropped rather than a writer problem",
            printed.get("pages")
        ));
    }
    // Every page measured, not just the first: a job that rendered page 1 at
    // 300 DPI and page 2 at 72 is a real defect a first-page reading misses.
    let per_page = printed
        .get("dpiPerPage")
        .and_then(Json::as_array)
        .ok_or_else(|| "the two-page print reported no per-page DPI".to_string())?;
    if per_page.len() != 2 {
        return Err(format!(
            "the two-page print reported {} per-page DPI reading(s), expected 2",
            per_page.len()
        ));
    }
    for (index, value) in per_page.iter().enumerate() {
        let dpi = value.as_u64().unwrap_or(0);
        if dpi < PRINT_DPI_FLOOR {
            return Err(format!(
                "page {} of the two-page print measured {dpi} DPI, below the {PRINT_DPI_FLOOR} floor",
                index + 1
            ));
        }
    }
    // Two pages of raster means roughly twice one page''s bytes, and this
    // fixture''s pages are 833x833 at 300 DPI - about 2 MB each. A one-page
    // output for a two-page input is the truncation this leg is about, so the
    // floor sits between the two rather than at either.
    let two_bytes = printed.get("bytes").and_then(Json::as_u64).unwrap_or(0);
    if two_bytes < 3_000_000 {
        return Err(format!(
            "the two-page print is only {two_bytes} bytes; two 833x833 pages at {PRINT_DPI_FLOOR} DPI \
             carry about 4 MB between them, so this is one page of a two-page document"
        ));
    }

    Ok(())
}

/// Assert that a print report is a real, print-resolution PDF artifact.
///
/// Shared by the three places a print job can be observed - the `__selisPrint`
/// entry point, the online Print button and the offline one - because they
/// assert the SAME property. Duplicating the thresholds per call site is how
/// two of them drift and the weakest one becomes the gate.
///
/// The DPI floor is checked against `lowestDpi` where the report carries it,
/// and against `dpi` otherwise: page 1 of a mixed-resolution document is not
/// evidence about page 9, and a check that reads only page 1 is a check that
/// cannot fail for the reason it exists.
///
/// @param printed the report object
/// @param what a name for the caller, used in the failure message
/// UI.13: verify the print job's progress bar and its Cancel control.
///
/// The check drives the app's own buttons and reads the DOM it painted, so
/// what is asserted is what a reader would see - not what the model believes
/// about itself. Four claims, each at the moment it was true:
///
/// 1. **Idle is silent.** Nothing is running, so the bar is hidden and Cancel
///    is disabled. An enabled Cancel on an idle viewer is a control that
///    promises to stop a job that does not exist.
/// 2. **A running job is visible.** The bar is shown and Cancel is live. A
///    cancel reachable only through a global hook is not a user affordance.
/// 3. **A cancel is a REQUEST.** Immediately after the press the caption must
///    say it is stopping, and must NOT say the job was cancelled: the engine
///    has not answered yet, and claiming otherwise is a cancellation the shell
///    invented.
/// 4. **The engine's answer is what ends it.** The job settles `cancelled` and
///    - the claim that matters most - NO failure panel appeared. The reader
///    pressed the button; telling them their document has a problem it does not
///    have is the dead end SL-4.UI.12 exists to remove.
fn assert_print_cancellation(report: &Json) -> Result<(), String> {
    let idle = report
        .get("chromeIdle")
        .ok_or_else(|| "the app reported no print chrome (chromeIdle = None)".to_string())?;
    if idle.is_null() {
        return Err("the app reported no idle state for its print chrome".into());
    }
    if idle.get("hidden").and_then(Json::as_bool) != Some(true) {
        return Err(
            "the print progress bar was VISIBLE while no print was running -- a bar \
                    reading 0% on an idle viewer is a claim about nothing"
                .into(),
        );
    }
    if idle.get("cancelDisabled").and_then(Json::as_bool) != Some(true) {
        return Err(
            "the print Cancel control was ENABLED while no print was running -- it \
                    promises to stop a job that does not exist"
                .into(),
        );
    }

    // A started job must be observable at all. This is the whole point of a
    // progress bar, and it is the check an earlier, racy version of this gate
    // could not make: the shell reports its first moment before it awaits
    // anything, so this does not depend on catching a running job in the wild.
    let at_start = report
        .get("progressAtStart")
        .ok_or_else(|| "the app reported no progress for a print it had started".to_string())?;
    if at_start.is_null() {
        return Err("a running print reported no progress at all".into());
    }
    if at_start.get("pagesDone").and_then(Json::as_u64).is_none() {
        return Err(format!(
            "a running print reported a progress with no page count: {at_start}"
        ));
    }

    let chrome = report
        .get("chromeAtStart")
        .ok_or_else(|| "the app reported no chrome for a running print".to_string())?;
    if chrome.get("hidden").and_then(Json::as_bool) != Some(false) {
        return Err("a print was running but its progress bar was hidden".into());
    }
    // A running job's Cancel must be LIVE. The mirror image of the idle check:
    // a Cancel that stays disabled while pages are being rendered is a control
    // that does not work, and it is worse than no control because the reader
    // pressed it.
    if chrome.get("cancelDisabled").and_then(Json::as_bool) != Some(false) {
        return Err(
            "a print was running but its Cancel control was NOT enabled -- the reader has \
                    no way to stop a long job"
                .into(),
        );
    }

    // The cancellation itself was accepted.
    let request = report
        .get("cancelRequest")
        .ok_or_else(|| "the app reported no answer to a Cancel press".to_string())?;
    if request.is_null() {
        return Err(
            "pressing Cancel while a print ran was answered with nothing -- the control did \
             nothing"
                .into(),
        );
    }
    if request.get("requested").and_then(Json::as_bool) != Some(true) {
        return Err(format!(
            "a Cancel press was not recorded as a request: {request}"
        ));
    }

    // THE HONESTY CHECK. Read in the same task as the press, before the engine
    // could have answered, so "Cancelled" here could only have been invented.
    let caption = report
        .get("captionAfterCancel")
        .and_then(Json::as_str)
        .unwrap_or_default();
    if caption.trim().is_empty() {
        return Err("the print chrome rendered no caption after a Cancel press".into());
    }
    if caption.contains("Cancelled") || caption.contains("Canceled") {
        return Err(format!(
            "the caption claimed `{caption}` immediately after Cancel was pressed -- a cancel is \
             a REQUEST until the engine answers, and the engine had not answered yet"
        ));
    }

    // And the settled end state.
    let settled = report
        .get("printAfterCancel")
        .ok_or_else(|| "the app recorded no outcome for a cancelled print".to_string())?;
    if settled.is_null() {
        return Err("a cancelled print recorded no outcome".into());
    }
    if settled.get("status").and_then(Json::as_str) != Some("cancelled") {
        return Err(format!(
            "a print the reader cancelled ended as {:?}, not `cancelled`",
            settled.get("status").and_then(Json::as_str)
        ));
    }
    // A cancellation is not a failure, and the panel is how the shell says
    // "your document has a problem". Raising it here would invent a fault.
    let panel = report
        .get("failureAfterCancel")
        .and_then(Json::as_str)
        .unwrap_or_default();
    if !panel.trim().is_empty() {
        return Err(format!(
            "cancelling a print raised a failure panel saying `{panel}` -- the reader pressed \
             Cancel, so their document has no problem to report"
        ));
    }
    Ok(())
}

/// UI.07: the shell's own controls must be reachable and effective from the
/// keyboard.
///
/// Driven with real `KeyboardEvent`s at the document, so what is checked is the
/// wiring and not just the model: a test that called the action directly would
/// pass with the listener attached to the wrong element, or never installed.
///
/// Three claims, and the second is the half a keyboard test usually omits:
///
/// 1. `Ctrl+F` reaches the find field.
/// 2. `Ctrl+P` with **no document open does nothing**. A shortcut that exists
///    when there is nothing to act on is how a reader concludes the keyboard is
///    not trustworthy here.
/// 3. A chord the browser owns (`Ctrl+R`) is left alone. The viewer does not get
///    to relabel the browser's own chrome.
fn assert_control_keys(report: &Json) -> Result<(), String> {
    let keys = report
        .get("keys")
        .ok_or_else(|| "the app reported no keyboard results (keys = None)".to_string())?;
    if keys.is_null() {
        return Err("the app reported no keyboard results for the shell's controls".into());
    }
    if keys.get("findFocusedField").and_then(Json::as_bool) != Some(true) {
        return Err(
            "Ctrl+F did not move focus to the find field -- the control is not reachable \
                    from the keyboard"
                .into(),
        );
    }
    if keys.get("printStartedWithDocument").and_then(Json::as_bool) != Some(true) {
        return Err(
            "Ctrl+P did not start a print while a document was open -- the chord is \
                    declared but not wired to its control"
                .into(),
        );
    }

    let idle = keys
        .get("cancelWithNoJob")
        .ok_or_else(|| "the app reported nothing about Ctrl+. with no print running".to_string())?;
    if idle.is_null() {
        return Err("the app did not exercise Ctrl+. with no print running".into());
    }
    // The outcome must be UNCHANGED. `__selisPrintState` keeps the last print's
    // outcome on purpose, so this compares before-and-after; testing for null
    // would assert this leg ran before anything had ever printed, which is an
    // accident of ordering rather than a claim about the keyboard.
    if idle.get("stateAfter") != idle.get("stateBefore") {
        return Err(format!(
            "Ctrl+. changed the print outcome with NOTHING running: {idle} -- a shortcut that \
             exists with nothing behind it is a dead end wearing a keybinding"
        ));
    }
    if idle
        .get("progressAfter")
        .is_some_and(|value| !value.is_null())
    {
        return Err(format!(
            "Ctrl+. started a print job with NOTHING running: {idle} -- a cancel shortcut that \
             opens a job is the opposite of a cancel shortcut"
        ));
    }

    if keys.get("browserChordIgnored").and_then(Json::as_bool) != Some(true) {
        return Err(
            "Ctrl+R was claimed by the viewer -- that chord belongs to the browser, and \
                    taking it makes the app feel like it has taken over the tab"
                .into(),
        );
    }
    Ok(())
}
/// Copy `axe-core` into the staged `app-shell` fixture.
///
/// A MISSING dependency is a failed check naming the command that installs it,
/// never a skip and never a pass. A skipped accessibility scan is the worst
/// possible failure for this item: "axe-core clean" is the DoD, and a run that
/// quietly omits it reports the item as unverified while looking green.
///
/// The copy lands in the scratch directory the harness serves, so it exists
/// only for the run and cannot reach `public/` — ADR-P0016 keeps third-party
/// scripts off the document-handling path.
fn stage_axe(served: &Path) -> Result<(), String> {
    // `CARGO_MANIFEST_DIR` is `xtask/`, so the workspace root is one level up —
    // derived rather than taken from the working directory, for the same reason
    // `check_root` is: the build must not depend on where it was invoked from.
    let workspace = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .ok_or("the xtask manifest has no parent directory")?;
    let source = workspace
        .join("node_modules")
        .join("axe-core")
        .join("axe.min.js");
    if !source.exists() {
        return Err(format!(
            "{} does not exist - install it with `pnpm add -D -w axe-core`",
            source.display()
        ));
    }
    let bytes =
        std::fs::read(&source).map_err(|e| format!("cannot read {}: {e}", source.display()))?;
    if bytes.is_empty() {
        return Err(format!("{} is empty", source.display()));
    }
    std::fs::write(served.join("axe.min.js"), &bytes)
        .map_err(|e| format!("cannot write the staged axe-core: {e}"))
}

/// UI.07: the DoD's "axe-core clean".
///
/// A scan that did not RUN is a failure, never a pass and never a skip. That is
/// the whole reason this verdict exists in the shape it does: "axe-core clean"
/// is the DoD, so a check that quietly omits the scan reports the item as
/// unverified while the gate stays green — the exact failure mode this project
/// keeps having to undo.
///
/// Violations are reported with their rule id and one target, because a count
/// alone ("3 violations") sends the reader back to the page to find them.
fn assert_accessibility(report: &Json) -> Result<(), String> {
    let a11y = report
        .get("a11y")
        .ok_or_else(|| "the app reported no accessibility scan (a11y = None)".to_string())?;
    if a11y.is_null() {
        return Err(
            "the accessibility scan never ran -- \"axe-core clean\" is the DoD, and a run \
                    that omits it proves nothing"
                .into(),
        );
    }
    if a11y.get("available").and_then(Json::as_bool) != Some(true) {
        return Err(
            "axe-core did not load for the accessibility scan -- install it with \
                    `pnpm add -D -w axe-core`"
                .into(),
        );
    }
    let violations = a11y
        .get("violations")
        .and_then(Json::as_array)
        .ok_or_else(|| "the accessibility scan reported no violation list".to_string())?;
    if violations.is_empty() {
        return Ok(());
    }
    let detail = violations
        .iter()
        .map(|violation| {
            let id = violation.get("id").and_then(Json::as_str).unwrap_or("?");
            let impact = violation
                .get("impact")
                .and_then(Json::as_str)
                .unwrap_or("unknown");
            let nodes = violation.get("nodes").and_then(Json::as_u64).unwrap_or(0);
            let target = violation
                .get("firstTarget")
                .and_then(Json::as_str)
                .unwrap_or("");
            format!("{id} ({impact}, {nodes} node(s)) on {target}")
        })
        .collect::<Vec<_>>()
        .join("; ");
    Err(format!(
        "the viewer has {} accessibility violation(s) against WCAG 2.0/2.1 A+AA: {detail}",
        violations.len()
    ))
}

/// UI.07: "a screen-reader script walks a tagged document correctly".
///
/// The check walks the ACCESSIBILITY TREE the page exposes — role, accessible
/// name, and whether the node is hidden from assistive tech — which is what a
/// screen reader announces. Reading `innerText` instead would pass on a page
/// whose structure is invisible: it cannot see a role, cannot tell a nameless
/// heading from a labelled one, and cannot notice an element excluded from the
/// accessibility tree.
///
/// Four claims:
/// 1. The walk happened. A reader that never ran proves nothing.
/// 2. The disclosure is honest: `aria-expanded` agrees with what is on screen.
/// 3. Every announced node has an accessible NAME. A heading a reader cannot
///    hear is not a heading to them — it is an unlabelled stop in the outline.
/// 4. A figure with no `/Alt` is announced AS needing a description, never
///    with an invented one. This is the fabrication `a11y.ts` refuses, checked
///    at the point it would reach a reader.
fn assert_reader_walk(report: &Json) -> Result<(), String> {
    let reader = report
        .get("reader")
        .ok_or_else(|| "the app reported no screen-reader walk (reader = None)".to_string())?;
    if reader.is_null() {
        return Err(
            "the screen-reader walk never ran -- \"a screen-reader script walks a tagged \
                    document correctly\" is half the DoD, and a walk that did not happen \
                    proves nothing"
                .into(),
        );
    }
    if reader.get("expanded").and_then(Json::as_str) != Some("true") {
        return Err(
            "the outline panel was walked but its disclosure reports aria-expanded=false -- \
                    a screen-reader user would never have been told it opened"
                .into(),
        );
    }
    let announced = reader
        .get("announced")
        .and_then(Json::as_array)
        .ok_or_else(|| "the screen-reader walk reported no nodes".to_string())?;
    // An untagged document legitimately announces a sentence and nothing else.
    // That is a PASS, and it is the case this whole design exists for: no
    // fabricated headings to fill the gap.
    let untagged = reader.get("untagged").and_then(Json::as_bool) == Some(true);
    if untagged {
        return Ok(());
    }
    // A panel that could not READ the structure is not a walk, however
    // well-formed the landmark around it is.
    //
    // This leg was briefly vacuous, and the shape of that is worth recording:
    // the region itself carries `role="region"`, so the walk announced exactly
    // ONE node - the region - satisfied "every announced node has a name", and
    // went green on a page whose outline said nothing at all. A gate that walks
    // a tree must also check the tree CONTAINS what it was asked for, or it is
    // measuring the scaffolding.
    if reader.get("unreadable").and_then(Json::as_bool) == Some(true) {
        return Err(
            "the outline panel could not read the document structure, so the screen-reader walk \
             only saw the surrounding landmark -- that is not a walk of a tagged document"
                .into(),
        );
    }
    if announced.len() < 2 {
        return Err(format!(
            "the screen-reader walk announced only {} node(s) for a tagged document -- a landmark \
             is not a document structure",
            announced.len()
        ));
    }
    for node in announced {
        let tag = node.get("tag").and_then(Json::as_str).unwrap_or("?");
        let label = node.get("name").and_then(Json::as_str).unwrap_or("");
        // The marker the shell appends to a figure with no description IS its
        // accessible name, so a flagged figure legitimately has text here.
        let flagged = node.get("needsDescription").and_then(Json::as_bool) == Some(true);
        if label.trim().is_empty() && !flagged {
            return Err(format!(
                "a screen reader would announce an unlabelled <{tag}> in the outline -- a heading \
                 with no accessible name is not a heading to the person listening"
            ));
        }
    }
    Ok(())
}

/// UI.06: the bookmark outline, from a real engine op.
///
/// This leg exists mostly as EVIDENCE for a correction to the plan, which
/// recorded UI.06 as blocked on the engine: "`selis-pdf-doc` has `page_labels`
/// and `parse_destination` but no `/Outlines` walk and no annotation reader, and
/// the WASM.01 protocol has no op for either". Both halves were stale —
/// `outline.rs` is 944 lines with 17 tests, and the protocol has `outline`,
/// `pageLinks`, `pageLabels` and `destinations`. What was missing was the SHELL.
///
/// The claim under test is the one that motivated those three flags existing at
/// all: a reader must be able to tell "this document has no outline" from "this
/// host cannot read outlines" from "there is one and it is incomplete". Checking
/// the flags and the RENDERED sentence together is the only way to catch a shell
/// that honours the first and forgets the third.
fn assert_bookmark_outline(report: &Json) -> Result<(), String> {
    let marks = report
        .get("bookmarks")
        .ok_or_else(|| "the app reported no bookmark outline (bookmarks = None)".to_string())?;
    if marks.is_null() {
        return Err("the bookmark outline was never read".into());
    }
    if marks.get("status").and_then(Json::as_str) != Some("ok") {
        return Err(format!(
            "the bookmark op did not succeed: {}",
            marks.get("status").and_then(Json::as_str).unwrap_or("?")
        ));
    }
    if marks.get("expanded").and_then(Json::as_str) != Some("true") {
        return Err(
            "the bookmark panel was read but its disclosure reports aria-expanded=false".into(),
        );
    }
    let present = marks.get("present").and_then(Json::as_bool) == Some(true);
    let unavailable = marks.get("unavailable").and_then(Json::as_bool) == Some(true);

    // The host answering and the document having nothing are DIFFERENT facts.
    // A shell that renders "this document has no outline" when it could not read
    // one is telling the reader something false about their own file.
    if present && unavailable {
        return Err(
            "the host reported the outline BOTH readable and unreadable -- the reader cannot be \
             told which"
                .into(),
        );
    }
    if !present {
        if marks.get("items").and_then(Json::as_u64).unwrap_or(0) > 0 {
            return Err(format!(
                "the document has no outline but {} item(s) arrived anyway",
                marks.get("items").and_then(Json::as_u64).unwrap_or(0)
            ));
        }
        if marks
            .get("said")
            .and_then(Json::as_str)
            .unwrap_or("")
            .is_empty()
        {
            return Err(
                "the document has no outline and the panel said nothing about it -- silence is \
                 not an answer"
                    .into(),
            );
        }
        return Ok(());
    }
    // A truncated outline must SAY so. Four chapters of a ten-chapter outline,
    // unannounced, is a document the reader does not have.
    if marks.get("truncated").and_then(Json::as_bool) == Some(true)
        && marks.get("truncatedNote").and_then(Json::as_bool) != Some(true)
    {
        return Err("the outline was truncated and the panel did not tell the reader".into());
    }
    // An outline that IS present must render as something a reader can use.
    let targets = marks.get("targets").and_then(Json::as_u64).unwrap_or(0);
    if targets == 0 {
        return Err(
            "the document HAS an outline but no bookmark rendered as a control -- items the \
             reader cannot press are not navigation"
                .into(),
        );
    }
    // The nesting claim. `data-depth` is metadata a screen reader never reads,
    // so a flat list carrying it would pass every other check here while
    // telling a reader that four chapters are four siblings.
    if marks.get("nested").and_then(Json::as_u64).unwrap_or(0) == 0
        && marks.get("maxDepth").and_then(Json::as_u64).unwrap_or(0) > 0
    {
        return Err(
            "the outline has depth in its attributes but no nested list in the DOM -- the \
             hierarchy is recorded where assistive technology cannot see it"
                .into(),
        );
    }
    Ok(())
}

/// UI.06: the navigation reads, and a bookmark button that WORKS.
///
/// The bookmark check above counts the buttons. Counting them proves the panel
/// rendered; it cannot tell a working button from a dead end — and the buttons
/// used to be dead ends, because `__selisGoto` did not exist. So this asserts
/// the PAGE CHANGED after a button was pressed, in the pixels the browser
/// holds.
///
/// The fixture has two pages with visibly different content, and the button
/// pressed is the one for page 2: navigating to the page already showing is
/// indistinguishable from doing nothing.
fn assert_navigation(report: &Json) -> Result<(), String> {
    // ── The page labels ──────────────────────────────────────────────────
    let labels = report
        .get("pageLabels")
        .ok_or_else(|| "the app reported no page labels (pageLabels = None)".to_string())?;
    if labels.get("status").and_then(Json::as_str) != Some("ok") {
        return Err(format!(
            "the pageLabels read did not succeed: {:?}",
            labels.get("status")
        ));
    }
    let ranges = labels
        .get("ranges")
        .and_then(Json::as_array)
        .ok_or_else(|| "the pageLabels read carried no ranges".to_string())?;
    if ranges.len() != 2 {
        return Err(format!(
            "the fixture declares two label ranges and the app read {} -- the document's own \
             page numbering is not reaching the screen",
            ranges.len()
        ));
    }
    // The document's own words: lower-roman for the front matter, decimal for
    // the body. A viewer that reported page NUMBERS instead would show "D"
    // nowhere and "r" nowhere.
    if ranges[0].get("style").and_then(Json::as_str) != Some("r") {
        return Err(format!(
            "the first label range is not lower-roman: {:?}",
            ranges[0].get("style")
        ));
    }
    if ranges[1].get("style").and_then(Json::as_str) != Some("D") {
        return Err(format!(
            "the second label range is not decimal: {:?}",
            ranges[1].get("style")
        ));
    }

    // ── The named destinations ────────────────────────────────────────────
    let destinations = report
        .get("destinations")
        .ok_or_else(|| "the app reported no destinations (destinations = None)".to_string())?;
    if destinations.get("status").and_then(Json::as_str) != Some("ok") {
        return Err(format!(
            "the destinations read did not succeed: {:?}",
            destinations.get("status")
        ));
    }
    let names = destinations
        .get("names")
        .and_then(Json::as_array)
        .ok_or_else(|| "the destinations read carried no names".to_string())?;
    if !names
        .iter()
        .any(|name| name.as_str() == Some("chapter-two"))
    {
        return Err(format!(
            "the document names one destination and the app read {names:?} -- a bookmark that \
             NAMES a destination is not navigable until the name is resolved"
        ));
    }

    // ── The links on the page ────────────────────────────────────────────
    let links = report
        .get("pageLinks")
        .ok_or_else(|| "the app reported no page links (pageLinks = None)".to_string())?;
    if links.get("status").and_then(Json::as_str) != Some("ok") {
        return Err(format!(
            "the pageLinks read did not succeed: {:?}",
            links.get("status")
        ));
    }
    if links.get("count").and_then(Json::as_u64) != Some(3) {
        return Err(format!(
            "the fixture puts three link annotations on page 1 and the app read {:?}",
            links.get("count")
        ));
    }
    // EVERY action class crosses, including the ones ADR-0020 disables. A
    // transport that filtered `/Launch` at the boundary would make "is the
    // viewer refusing this?" unanswerable from the outside.
    let kinds: Vec<Json> = links
        .get("kinds")
        .and_then(Json::as_array)
        .cloned()
        .unwrap_or_default();
    for wanted in ["uri", "goTo", "launch"] {
        if !kinds.iter().any(|kind| kind.as_str() == Some(wanted)) {
            return Err(format!(
                "the page's links carry no `{wanted}` action (read {kinds:?}) -- the transport \
                 must report every action class, including the ones ADR-0020 disables"
            ));
        }
    }

    // ── The navigation itself: a bookmark button that WORKS ──────────────
    let navigation = report
        .get("navigation")
        .ok_or_else(|| "the app reported no navigation (navigation = None)".to_string())?;
    let before_label = navigation.get("beforeLabel").and_then(Json::as_str);
    let after_label = navigation.get("afterLabel").and_then(Json::as_str);
    match (before_label, after_label) {
        (Some(before), Some(after)) if before != after => {}
        (before, after) => {
            return Err(format!(
                "pressing the bookmark did not change the page on screen (aria-label {before:?} \
                 -> {after:?}) -- the button is a dead end"
            ));
        }
    }
    // The first paint used to say "Rendered page 1" while the document calls
    // this page "i". The host now reads /PageLabels on open, and the check
    // waits for that relabel before it presses anything.
    if before_label != Some("Rendered page i") {
        return Err(format!(
            "the first page is labelled `i` by the document, but the canvas announced \
             {before_label:?} before the first bookmark press"
        ));
    }
    // The pixels, not only the label: a shell that renamed the canvas without
    // rendering the other page would pass the label check.
    let before_ink = navigation.get("beforeInk").and_then(Json::as_u64);
    let after_ink = navigation.get("afterInk").and_then(Json::as_u64);
    match (before_ink, after_ink) {
        (Some(before), Some(after)) if before != after => {}
        (before, after) => {
            return Err(format!(
                "the canvas shows the same ink after navigating ({before:?} -> {after:?}) -- the \
                 page on screen did not change"
            ));
        }
    }
    // ONE canvas. A viewer that stacks them shows every page it has ever
    // painted, one under the next, and the reader cannot tell which one they
    // are looking at.
    if navigation.get("canvases").and_then(Json::as_u64) != Some(1) {
        return Err(format!(
            "the viewer has {:?} canvases on screen; one page is showing and one canvas is it",
            navigation.get("canvases")
        ));
    }
    let goto = navigation
        .get("goto")
        .ok_or_else(|| "the navigation report carried no goto result".to_string())?;
    if goto.get("status").and_then(Json::as_str) != Some("ok") {
        return Err(format!(
            "navigating to page 2 did not succeed: {:?}",
            goto.get("status")
        ));
    }
    if goto.get("page").and_then(Json::as_u64) != Some(1) {
        return Err(format!(
            "navigating to page 2 reported page {:?}",
            goto.get("page")
        ));
    }
    // The document's OWN name for the page, not its number. The fixture labels
    // page 2 as "1" (the body restarts at decimal 1), so a viewer reporting the
    // page NUMBER would say "2" here.
    if goto.get("label").and_then(Json::as_str) != Some("1") {
        return Err(format!(
            "page 2 is labelled `1` by the document (its body restarts at 1), and the app \
             reported {:?}",
            goto.get("label")
        ));
    }

    // ── Activating a link ────────────────────────────────────────────────
    let panel = report
        .get("linkPanel")
        .ok_or_else(|| "the app reported no link panel (linkPanel = None)".to_string())?;
    if panel.get("items").and_then(Json::as_u64) != Some(3) {
        return Err(format!(
            "the page has three link annotations and the panel rendered {:?} of them",
            panel.get("items")
        ));
    }
    // ADR-0020: an external destination prompts, and the prompt shows the
    // WHOLE thing. A shortened or normalised URL is how a reader approves one
    // destination and is sent to another - so the query string is checked, not
    // just the host.
    let shown = panel
        .get("externalUrl")
        .and_then(Json::as_str)
        .unwrap_or("");
    if !shown.starts_with("https://example.com/selis") || !shown.contains("ref=fixture") {
        return Err(format!(
            "the external link's prompt showed {shown:?} -- it must show the whole destination, \
             query string and all"
        ));
    }
    if panel.get("externalHost").and_then(Json::as_bool) != Some(true) {
        return Err("the external prompt did not name the host for the reader to check".into());
    }
    // The manual pass found that the prompt was a plain div: no dialog role,
    // no accessible name or description, and focus stayed on the link that
    // opened it. These assertions are what keep the fix from regressing into
    // the same visually-fine, keyboard-hostile control.
    let prompt = panel.get("prompt").ok_or_else(|| {
        "the app reported no external-prompt semantics (prompt = None)".to_string()
    })?;
    if prompt.get("role").and_then(Json::as_str) != Some("dialog") {
        return Err(format!(
            "the external prompt is not a dialog (role = {:?})",
            prompt.get("role")
        ));
    }
    if prompt.get("labelled").and_then(Json::as_bool) != Some(true) {
        return Err("the external prompt is not labelled by its question".into());
    }
    let described = prompt.get("described").and_then(Json::as_str).unwrap_or("");
    if !described.contains("https://example.com/selis") || !described.contains("Host: example.com")
    {
        return Err(format!(
            "the external prompt's description omits the destination or host: {described:?}"
        ));
    }
    if prompt.get("focus").and_then(Json::as_str) != Some("external-cancel") {
        return Err(format!(
            "focus did not move to the safe answer when the external prompt opened: {:?}",
            prompt.get("focus")
        ));
    }
    let escape = panel.get("escape").ok_or_else(|| {
        "the app reported no Escape result for the prompt (escape = None)".to_string()
    })?;
    if escape.get("closed").and_then(Json::as_bool) != Some(true) {
        return Err("Escape did not close the external prompt".into());
    }
    if escape.get("focusRestored").and_then(Json::as_bool) != Some(true) {
        return Err("Escape closed the prompt but did not return focus to the link".into());
    }
    let cancel = panel.get("cancel").ok_or_else(|| {
        "the app reported no Cancel result for the prompt (cancel = None)".to_string()
    })?;
    if cancel.get("closed").and_then(Json::as_bool) != Some(true) {
        return Err("Cancel did not close the external prompt".into());
    }
    if cancel.get("focusRestored").and_then(Json::as_bool) != Some(true) {
        return Err("Cancel closed the prompt but did not return focus to the link".into());
    }
    // A `/Launch` is refused, WITH A REASON. A refusal that says nothing is
    // the quiet failure this whole feature exists to prevent.
    if panel.get("refusal").and_then(Json::as_str) != Some("launch") {
        return Err(format!(
            "the /Launch link was not refused with a reason (refusal = {:?})",
            panel.get("refusal")
        ));
    }
    Ok(())
}

fn assert_print_artifact(printed: &Json, what: &str) -> Result<(), String> {
    if printed.get("status").and_then(Json::as_str) != Some("ok") {
        return Err(format!("{what} did not produce a PDF: {printed}"));
    }
    // A real PDF header. This is what separates an artifact from a summary
    // object, and it is the cheapest check that the writer actually ran.
    let header = printed.get("header").and_then(Json::as_str).unwrap_or("");
    if !header.starts_with("%PDF-") {
        return Err(format!(
            "the print output is not a PDF: header was {header:?}"
        ));
    }
    // PRINT resolution, not the screen's 72. This is the leg that catches a
    // rasteriser quietly falling back to the raw ABI, which cannot scale: the
    // output would still be a valid PDF, just soft and the wrong physical size.
    let measured = printed
        .get("lowestDpi")
        .or_else(|| printed.get("dpi"))
        .and_then(Json::as_u64)
        .unwrap_or(0);
    if measured < PRINT_DPI_FLOOR {
        return Err(format!(
            "the print raster measured {measured} DPI, at or below the {PRINT_DPI_FLOOR} floor \
             -- that is a screen render, not a print one"
        ));
    }
    // A stub - an empty page, or a page object with no image - is a few hundred
    // bytes. One 300 DPI Letter page is ~25 MB uncompressed, so anything under a
    // megabyte means the raster never reached the writer.
    let bytes = printed.get("bytes").and_then(Json::as_u64).unwrap_or(0);
    if bytes < 1_000_000 {
        return Err(format!(
            "the print output is only {bytes} bytes; a {PRINT_DPI_FLOOR} DPI page carries its \
             raster, so this did not"
        ));
    }
    Ok(())
}

/// The DPI floor the print plan enforces. Mirrored here rather than imported:
/// the Rust verdict is the gate, so it states its own threshold instead of
/// trusting a value the page also uses - if the two drift, a green gate would
/// mean only that the page agreed with itself.
const PRINT_DPI_FLOOR: u64 = 300;

fn app_public_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../apps/web/host/public")
}

/// Copy the built app onto the harness origin, recursively.
///
/// Separate from `stage` because that one is flat **on purpose** - the committed
/// checks are a page, a worker and an asset, and a recursive copier there would
/// quietly let a check grow a tree nobody notices. This one is explicit about
/// being recursive because the app genuinely is a tree, and because silently
/// skipping `assets/` would produce a check that passes against an app with no
/// boot script - a green gate for a broken build, which is worse than no gate.
fn stage_app(from: &Path, to: &Path) -> Result<(), String> {
    if !from.is_dir() {
        return Err(format!(
            "{} does not exist - build the web host first (`pnpm build` in apps/web/host)",
            from.display()
        ));
    }
    copy_tree(from, to)
}

fn copy_tree(from: &Path, to: &Path) -> Result<(), String> {
    let entries = std::fs::read_dir(from).map_err(|e| format!("{}: {e}", from.display()))?;
    std::fs::create_dir_all(to).map_err(|e| format!("{}: {e}", to.display()))?;
    for entry in entries.flatten() {
        let src = entry.path();
        let dest = to.join(entry.file_name());
        if src.is_dir() {
            copy_tree(&src, &dest)?;
        } else if src.is_file() {
            std::fs::copy(&src, &dest).map_err(|e| format!("{}: {e}", dest.display()))?;
        }
    }
    Ok(())
}

/// Where `pnpm build` leaves the packed extension.
fn extension_dist_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../apps/extension/dist")
}

/// Stage the packed extension as an *unpacked* one the engine will load.
///
/// The package on disk is the store-shaped build: no key, no loopback host
/// permission, and `viewer.html` not reachable from a page. Every edit below is
/// load-bearing, and each was found by running the check rather than by reading
/// the manifest:
///
/// * **`key`** fixes the extension id. An unpacked extension without one gets
///   an id derived from the profile path, which changes every run — and the
///   launcher has to name the id in a URL before the extension can answer
///   anything. [`EXTENSION_ID`] is the id this key produces.
/// * **`host_permissions: http://127.0.0.1/*`** lets the viewer fetch the
///   fixture from the harness origin. The store build asks for the documents a
///   user opens instead, which no CI page can grant.
/// * **`web_accessible_resources`** lets the loopback launcher navigate to
///   `viewer.html`. Without it the navigation is refused and the viewer never
///   runs.
/// * **`options_page` is removed.** SL-4.EXT.07's welcome calls
///   `chrome.runtime.openOptionsPage()` on install, and the options page takes
///   the foreground. A viewer in a background tab does not paint on animation
///   frames, so the pixel witness would stay blank while the rest of the view
///   composed — measured here. The check's subject is the viewer, not the
///   welcome.
///
/// The probe is copied in and its script tag appended to `viewer.html`, the
/// same way the page-shaped checks inject theirs: the subject is the packed
/// viewer, and a stand-in page would prove nothing about the package.
fn stage_extension(from: &Path, to: &Path, check: &Path) -> Result<(), String> {
    if !from.is_dir() {
        return Err(format!(
            "{} does not exist - build the extension first (`pnpm build` in apps/extension)",
            from.display()
        ));
    }
    copy_tree(from, to)?;

    let manifest_path = to.join("manifest.json");
    let raw = std::fs::read_to_string(&manifest_path)
        .map_err(|e| format!("{}: {e}", manifest_path.display()))?;
    let mut manifest: Json = serde_json::from_str(&raw)
        .map_err(|e| format!("{} is not JSON: {e}", manifest_path.display()))?;
    let object = manifest
        .as_object_mut()
        .ok_or_else(|| format!("{} is not a JSON object", manifest_path.display()))?;
    object.insert("key".to_string(), json!(EXTENSION_KEY));
    object.insert(
        "host_permissions".to_string(),
        json!(["http://127.0.0.1/*"]),
    );
    object.insert(
        "web_accessible_resources".to_string(),
        json!([{ "resources": ["viewer.html"], "matches": ["http://127.0.0.1/*"] }]),
    );
    object.remove("options_page");
    let staged = serde_json::to_string_pretty(&manifest)
        .map_err(|e| format!("cannot serialise the staged manifest: {e}"))?;
    std::fs::write(&manifest_path, format!("{staged}\n"))
        .map_err(|e| format!("{}: {e}", manifest_path.display()))?;

    let probe_source = check.join("probe.js");
    let probe = std::fs::read_to_string(&probe_source)
        .map_err(|e| format!("{}: {e}", probe_source.display()))?;
    let probe_path = to.join("__selis_probe.js");
    std::fs::write(&probe_path, probe).map_err(|e| format!("{}: {e}", probe_path.display()))?;

    let viewer_path = to.join("viewer.html");
    let viewer = std::fs::read_to_string(&viewer_path)
        .map_err(|e| format!("{}: {e}", viewer_path.display()))?;
    const END: &str = "</body>";
    if !viewer.contains(END) {
        return Err(format!(
            "{} has no {END}; the probe cannot be injected and nothing would report",
            viewer_path.display()
        ));
    }
    let injected = viewer.replacen(
        END,
        "<script src=\"./__selis_probe.js\"></script>\n</body>",
        1,
    );
    std::fs::write(&viewer_path, injected)
        .map_err(|e| format!("{}: {e}", viewer_path.display()))?;
    Ok(())
}

/// Write the loopback launcher the extension check is opened on.
///
/// A command-line navigation straight to `chrome-extension://<id>/viewer.html`
/// is refused by the engine with `ERR_BLOCKED_BY_CLIENT`, measured on both a
/// cold and a warm profile. A loopback page that navigates there after a beat
/// is not: by then the unpacked extension is loaded, and the viewer is a
/// web-accessible resource for this origin. The 500 ms beat is the one measured
/// on this machine; it is a scheduling pause, not a race with extension
/// loading, because the browser loads unpacked extensions before its first
/// page.
///
/// The fixture URL is derived from `location.origin` inside the page, so the
/// harness never templates a random port into staged HTML.
fn stage_extension_launcher(served: &Path) -> Result<(), String> {
    let html = format!(
        r#"<!doctype html>
<meta charset="utf-8">
<title>extension launcher</title>
<script>
setTimeout(function () {{
	location.assign("chrome-extension://{EXTENSION_ID}/viewer.html?src=" +
		encodeURIComponent(location.origin + "/doc.pdf"));
}}, 500);
</script>
"#
    );
    let path = served.join("start.html");
    std::fs::write(&path, html).map_err(|e| format!("{}: {e}", path.display()))
}

#[cfg(test)]
mod app_shell_tests {
    use super::*;

    /// The bug this pins, found by running the check rather than by reading it:
    /// the app ships its own `index.html`, so staging the built app AFTER the
    /// check page silently replaced the page that reports the verdict. The
    /// browser then loaded the app shell, the harness script never ran, and the
    /// check timed out looking like a harness fault.
    ///
    /// A test cannot assert the whole ordering through `run_check` (it needs a
    /// browser), but it can assert the property that made the bug possible and
    /// fixable: the app tree is copied RECURSIVELY, so a missing `assets/`
    /// directory is a loud error rather than a silently-skipped copy. A flat
    /// copier here would have produced a check that passes against an app with
    /// no boot script — a green gate for a broken build, which is worse than no
    /// gate at all.
    #[test]
    fn stage_app_copies_nested_directories() {
        let tmp = std::env::temp_dir().join(format!("selis-app-stage-{}", std::process::id()));
        let from = tmp.join("public");
        let to = tmp.join("served");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(from.join("assets")).unwrap();
        std::fs::write(from.join("sw.js"), b"worker").unwrap();
        std::fs::write(from.join("assets/boot.js"), b"boot").unwrap();

        stage_app(&from, &to).expect("staging a tree");

        assert_eq!(std::fs::read(to.join("sw.js")).unwrap(), b"worker");
        assert_eq!(
            std::fs::read(to.join("assets").join("boot.js")).unwrap(),
            b"boot",
            "the nested asset must be copied: skipping assets/ would let the \
             check pass against an app with no boot module"
        );
        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// A missing build is a FAILURE with a nameable cause, not a skip and not a
    /// pass. The whole value of this check is that "the app is offline-ready"
    /// is a measured claim; if the app is simply absent, the honest answer is
    /// "you did not build it".
    #[test]
    fn stage_app_refuses_a_missing_build() {
        let missing = std::env::temp_dir().join("selis-no-such-app-build-dir");
        let err = stage_app(&missing, &std::env::temp_dir().join("unused"))
            .expect_err("a missing build must not stage");
        assert!(
            err.contains("build the web host first"),
            "the error must say what to do, got: {err}"
        );
    }
}

/// The staged extension is what the browser loads, so the manifest edits are
/// the check's subject rather than packaging detail. Each one is asserted here
/// because every one of them was found by a run where the viewer never appeared
/// or never painted: the key makes the id knowable, the permission makes the
/// fixture fetchable, the resource makes the launcher's navigation legal, and
/// removing the options page keeps the welcome from taking the foreground.
#[cfg(test)]
mod extension_tests {
    use super::*;

    fn scratch(label: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("selis-extension-{label}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn fixture_dist(root: &Path, viewer: &str) -> PathBuf {
        let dist = root.join("dist");
        std::fs::create_dir_all(dist.join("extension/src")).unwrap();
        std::fs::write(
            dist.join("manifest.json"),
            r#"{"manifest_version":3,"name":"selis","version":"0.1.0","options_page":"options.html"}"#,
        )
        .unwrap();
        std::fs::write(dist.join("viewer.html"), viewer).unwrap();
        std::fs::write(dist.join("extension/src/nested.js"), b"nested").unwrap();
        dist
    }

    fn fixture_check(root: &Path) -> PathBuf {
        let check = root.join("check");
        std::fs::create_dir_all(&check).unwrap();
        std::fs::write(check.join("probe.js"), "/* probe */").unwrap();
        check
    }

    #[test]
    fn stages_the_package_as_an_unpacked_extension_with_the_probe() {
        let root = scratch("stage");
        let dist = fixture_dist(&root, "<!doctype html><body>viewer</body>");
        let check = fixture_check(&root);
        let to = root.join("package");

        stage_extension(&dist, &to, &check).expect("staging the package");

        let manifest: Json =
            serde_json::from_str(&std::fs::read_to_string(to.join("manifest.json")).unwrap())
                .unwrap();
        assert_eq!(
            manifest.get("key").and_then(Json::as_str),
            Some(EXTENSION_KEY),
            "the key is what makes the extension id knowable before launch"
        );
        assert_eq!(
            manifest.get("host_permissions"),
            Some(&json!(["http://127.0.0.1/*"])),
            "the viewer fetches the fixture from the harness origin"
        );
        assert_eq!(
            manifest.get("web_accessible_resources"),
            Some(&json!([{ "resources": ["viewer.html"], "matches": ["http://127.0.0.1/*"] }])),
            "the loopback launcher navigates to the viewer"
        );
        assert!(
            manifest.get("options_page").is_none(),
            "EXT.07's welcome opens the options page on install and takes the \
             foreground; a background viewer does not paint"
        );
        assert_eq!(
            std::fs::read_to_string(to.join("__selis_probe.js")).unwrap(),
            "/* probe */"
        );
        let viewer = std::fs::read_to_string(to.join("viewer.html")).unwrap();
        assert!(
            viewer.contains("<script src=\"./__selis_probe.js\"></script>"),
            "the probe must run inside the packed viewer, got: {viewer}"
        );
        assert_eq!(
            std::fs::read(to.join("extension/src/nested.js")).unwrap(),
            b"nested",
            "the package is a tree; a flat copier would stage a broken extension"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn refuses_a_viewer_without_a_body_to_inject_into() {
        let root = scratch("nobody");
        let dist = fixture_dist(&root, "<!doctype html><p>no body tag</p>");
        let check = fixture_check(&root);

        let err = stage_extension(&dist, &root.join("package"), &check)
            .expect_err("a viewer with no </body> cannot carry the probe");
        assert!(
            err.contains("has no </body>"),
            "the error must name the missing anchor, got: {err}"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn refuses_a_missing_extension_build() {
        let root = scratch("missing");
        let check = fixture_check(&root);
        let err = stage_extension(&root.join("no-such-dist"), &root.join("package"), &check)
            .expect_err("a missing build must not stage");
        assert!(
            err.contains("build the extension first"),
            "the error must say what to do, got: {err}"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn launcher_names_the_fixed_extension_id_and_the_fixture() {
        let served = scratch("launcher");
        stage_extension_launcher(&served).expect("writing the launcher");

        let html = std::fs::read_to_string(served.join("start.html")).unwrap();
        assert!(
            html.contains(&format!("chrome-extension://{EXTENSION_ID}/viewer.html")),
            "the launcher must open the staged id, got: {html}"
        );
        assert!(
            html.contains("location.origin + \"/doc.pdf\""),
            "the fixture URL is derived from the served origin, not templated"
        );
        assert!(
            html.contains("setTimeout"),
            "the navigation waits for the unpacked extension to be loaded"
        );

        let _ = std::fs::remove_dir_all(&served);
    }
}

/// Instantiate `bytes` with the `env.twice` import the page supplies, and call
/// the export. Test-only.
#[cfg(test)]
fn call_probe(bytes: &[u8]) -> i32 {
    let engine = wasmtime::Engine::default();
    let module = wasmtime::Module::new(&engine, bytes).expect("a valid module");
    let mut linker = wasmtime::Linker::new(&engine);
    linker
        .func_wrap("env", "twice", |x: i32| x.saturating_mul(2))
        .expect("the import declares a single i32 -> i32");
    let mut store = wasmtime::Store::new(&engine, ());
    let instance = linker
        .instantiate(&mut store, &module)
        .expect("the module links against the import the page supplies");
    let add = instance
        .get_typed_func::<(i32, i32), i32>(&mut store, "selis_probe_add")
        .expect("the module exports selis_probe_add");
    add.call(&mut store, (20, 1))
        .expect("the export is callable")
}

/// A profile directory that no other run can collide with.
///
/// The obvious choice — a fixed path per check — is wrong, and expensively
/// so. A Chromium-family engine does not necessarily keep the process it was
/// launched as: the initial process can hand off and leave a *different* PID
/// running, which `Child::kill` does not reach. A leaked engine keeps the
/// `--user-data-dir` lock, and the next run with the same path finds the
/// profile held, hands off to the zombie, and never loads the page. Measured
/// here: a fixed path made the second and every later `browser-check` time out
/// while `cargo test` (which wipes its scratch tree) kept passing. A unique
/// path per run makes that collision impossible.
fn unique_profile(scratch: &Path, label: &str) -> PathBuf {
    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let seq = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    scratch
        .join("profile")
        .join(format!("{label}-{nanos:x}-{seq:x}"))
}

/// Take the engine down, and anything it re-parented itself into.
///
/// The tree kill matters: `Child::kill` alone leaves the engine's real browser
/// process running, holding its profile lock (see [`unique_profile`]).
fn shutdown(child: &mut Child) {
    if cfg!(windows) {
        let _ = Command::new("taskkill")
            .args(["/PID", &child.id().to_string(), "/T", "/F"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
    let _ = child.kill();
    let _ = child.wait();
}

/// Serve `root` on a loopback origin, drive `browser` at it, and return what
/// the page POSTed back.
///
/// The wait is wall-clock, not virtual: see the module docs for the
/// measurement that decided it. A page that never reports is an `Err`, so the
/// caller can record it as unmeasured rather than as a pass.
///
/// `page` is a path under the served root; `None` opens the origin itself, as
/// every page-shaped check does. `extension` is an unpacked package to load —
/// only SL-4.UI.06's leg sets it, and when it does the launcher page is the
/// subject because a command-line navigation to an extension URL is refused.
fn measure(
    browser: &Path,
    root: &Path,
    profile: &Path,
    timeout_ms: u64,
    page: Option<&str>,
    extension: Option<&Path>,
) -> Result<Json, String> {
    let (server, results) = serve(root)?;
    let url = match page {
        Some(page) => format!("{}/{page}", server.origin()),
        None => server.origin(),
    };
    let mut child = spawn(browser, profile, &url, extension)?;

    // Poll rather than block on the channel, so that an engine which *died*
    // is reported as such. "The page did not report" and "the browser exited
    // with a non-zero status before it could" are different bugs, and a gate
    // that cannot tell them apart costs a debugging session every time.
    let deadline = Instant::now() + Duration::from_millis(timeout_ms);
    let reported = loop {
        match results.try_recv() {
            Ok(body) => break Ok(body),
            Err(mpsc::TryRecvError::Disconnected) => {
                break Err("the origin stopped serving before the page reported".to_string())
            }
            Err(mpsc::TryRecvError::Empty) => {}
        }
        match child.try_wait() {
            Ok(Some(status)) => {
                break Err(format!(
                    "{} exited with {status} before the page reported",
                    browser_name(browser)
                ))
            }
            Ok(None) => {}
            Err(e) => break Err(format!("cannot poll {}: {e}", browser_name(browser))),
        }
        if Instant::now() >= deadline {
            let extension_hint = if extension.is_some() {
                " -- this leg loads an unpacked extension with --load-extension; an engine that \
                 refuses it never runs the viewer, and the report is absent rather than red"
            } else {
                ""
            };
            break Err(format!(
                "the page did not report within {timeout_ms}ms -- the engine launched and the \
                 origin served, so this is the page or the browser, not the harness{extension_hint}"
            ));
        }
        std::thread::sleep(Duration::from_millis(25));
    };

    // Let the engine finish writing the 204 before it is taken down, as
    // xtask/bench/run.mjs does before killing its browser.
    std::thread::sleep(Duration::from_millis(SHUTDOWN_GRACE_MS));
    shutdown(&mut child);
    drop(server);

    let body = reported?;
    serde_json::from_str(&body)
        .map_err(|e| format!("the page reported a non-JSON body ({e}): {body}"))
}

/// Launch the engine.
///
/// `--headless=new` rather than the old `--headless`: the new headless is the
/// same code path as a real window, which is the point of driving a browser at
/// all. The profile is a fresh directory per check so no previous run's
/// service worker, cache or first-run state can answer for this one. Output is
/// discarded — a browser's stderr is noise here, and the verdict is the
/// structured report the page POSTs.
///
/// The ordinary checks pass `--disable-extensions`, because an installed
/// extension is not part of what they measure. The extension leg must not:
/// `--disable-extensions` wins over `--load-extension`, and the except list
/// keeps every other extension out without touching the one under test.
fn spawn(
    browser: &Path,
    profile: &Path,
    url: &str,
    extension: Option<&Path>,
) -> Result<Child, String> {
    std::fs::create_dir_all(profile).map_err(|e| format!("{}: {e}", profile.display()))?;
    let mut command = Command::new(browser);
    command
        .arg("--headless=new")
        .arg("--disable-gpu")
        // CI runners have no user-namespace sandbox available, and the page
        // loads only this repository's own committed files. Same rationale as
        // SL-4.WASM.08's headless leg.
        .arg("--no-sandbox")
        .arg("--no-first-run")
        .arg("--no-default-browser-check")
        .arg("--disable-background-networking")
        .arg("--disable-component-update")
        .arg("--disable-sync")
        .arg("--disable-default-apps")
        .arg("--metrics-recording-only")
        .arg(format!("--user-data-dir={}", profile.display()));
    match extension {
        Some(dir) => {
            command
                .arg(format!("--disable-extensions-except={}", dir.display()))
                .arg(format!("--load-extension={}", dir.display()));
        }
        None => {
            command.arg("--disable-extensions");
        }
    }
    command
        .arg(url)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("cannot spawn {}: {e}", browser.display()))
}

/// A loopback origin serving one directory, which stops when it is dropped.
struct Server {
    /// The port it bound; the harness only ever talks to `127.0.0.1`.
    port: u16,
    /// Tells the accept loop to finish.
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl Server {
    /// The origin a page is driven at.
    fn origin(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

/// Bind a loopback origin, serve `root` from it, and return the receiver the
/// pages' verdicts arrive on.
///
/// The first verdict wins: a page that reports twice (a retry, a stray
/// `load` handler) must not be able to make a failing check look measured.
fn serve(root: &Path) -> Result<(Server, Receiver<String>), String> {
    let listener =
        TcpListener::bind("127.0.0.1:0").map_err(|e| format!("cannot bind 127.0.0.1: {e}"))?;
    let port = listener
        .local_addr()
        .map_err(|e| format!("cannot read the bound address: {e}"))?
        .port();
    // A browser opens several connections at a time and holds them open, so the
    // accept loop polls rather than blocking, and the stop flag is what ends it.
    listener
        .set_nonblocking(true)
        .map_err(|e| format!("cannot poll the listener: {e}"))?;
    let stop = Arc::new(AtomicBool::new(false));
    let offline = Arc::new(AtomicBool::new(false));
    let (tx, rx) = mpsc::channel::<String>();
    let root = Arc::new(root.to_path_buf());
    let flag = Arc::clone(&stop);
    let handle = std::thread::spawn(move || {
        let reported = Arc::new(AtomicBool::new(false));
        while !flag.load(Ordering::SeqCst) {
            match listener.accept() {
                Ok((stream, _)) => {
                    // The listener is polled non-blocking, and on Windows an
                    // accepted socket INHERITS that flag across to a different
                    // file description. A non-blocking read ignores the read
                    // timeout: it answers `WSAEWOULDBLOCK` immediately
                    // whenever the request bytes have not arrived yet, so
                    // every fresh connection raced its own first read and the
                    // idle-close branch sealed it — in microseconds, per the
                    // connection-lifetime log. Return the connection to
                    // blocking before serving so the five-second idle window
                    // is actually what a parked socket waits, and the read
                    // fuse actually covers a request.
                    let _ = stream.set_nonblocking(false);
                    // One thread per connection, deliberately. A browser opens
                    // speculative and preconnect sockets that it may never write
                    // to, and a single-threaded loop that blocks reading one of
                    // them serves nothing else — which looks exactly like a
                    // page that never loads.
                    let root = Arc::clone(&root);
                    let tx = tx.clone();
                    let reported = Arc::clone(&reported);
                    let offline = Arc::clone(&offline);
                    let _ = std::thread::Builder::new()
                        .name("selis-browser-origin".to_string())
                        .spawn(move || {
                            if let Err(e) =
                                handle_connection(&stream, &root, &tx, &reported, &offline)
                            {
                                // A browser resets idle sockets routinely; that
                                // is not the harness's problem.
                                eprintln!("browser-check: origin: {e}");
                            }
                        });
                }
                Err(e) if e.kind() == ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(5));
                }
                Err(e) => {
                    eprintln!("browser-check: origin stopped accepting: {e}");
                    break;
                }
            }
        }
    });
    Ok((
        Server {
            port,
            stop,
            handle: Some(handle),
        },
        rx,
    ))
}

/// Serve a connection: the verdict POST, the offline switch, and files out of
/// `root`.
///
/// **Every response is the last response.** The origin used to answer request
/// after request on one keep-alive socket, and that lost to a race this loop
/// cannot fight from the serving side: the server's five-second idle close
/// retired a served socket that the browser's pool still handed out, the
/// import bursts lost half their children within ~30 ms — captured live with
/// the DevTools protocol as `net::ERR_SOCKET_NOT_CONNECTED` and
/// `net::ERR_CONNECTION_RESET` on fresh, already-started requests — and
/// because a failed module fetch is remembered in the module map, no amount
/// of same-URL retrying brought the burst back. Advertising
/// `Keep-Alive: timeout=4` did not buy the coordination back either.
///
/// Closing after every response removes the shared state altogether, and the
/// close is advertised *in the response*, so the browser never queues its next
/// request behind a socket that is being finished. That is the difference from
/// the earlier attempt documented here once before: that server stopped
/// reading a socket it had not told the client to abandon ("a queued module
/// request became responseStatus: 0" — the trace landed on `message.js`, a
/// file the server was answering with 200). Whole-graph serving has to read
/// nothing else: one head in, one head satisfied, one socket finished.
fn handle_connection(
    stream: &TcpStream,
    root: &Path,
    tx: &mpsc::Sender<String>,
    reported: &AtomicBool,
    offline: &AtomicBool,
) -> std::io::Result<()> {
    // The first-byte window is a BALANCE, and both ends of it were measured.
    //
    // Chrome opens speculative and preconnect sockets before it has a request
    // for them. Closing one turns it into a stale socket, and the next reuse
    // comes back `responseStatus: 0` with the opaque `Failed to fetch
    // dynamically imported module`. But keeping sockets for a minute is worse:
    // the browser's six-connections-per-origin cap fills with parked preconnect
    // sockets, the module script that follows the document never gets a slot,
    // and the page stops after the stylesheet. That is not a theory; it is the
    // 60 s run that failed `async-wasm` and never loaded `boot.js`.
    //
    // Five seconds releases the parked sockets long before the module graph
    // asks for them, and the stale-socket cost is paid where it can be
    // retried: the service worker retries both its install and its runtime
    // fetches, and the app's module loader retries with a distinct specifier.
    // The harness page retries its own stylesheet for the same reason.
    let mut reader = stream.try_clone()?;
    let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
    let mut head = Vec::new();
    let mut byte = [0u8; 1];
    let mut started = false;
    // Read through the blank line that ends the request head. One byte at a
    // time is not a performance concern for a harness that serves a handful of
    // files to one local browser, and it needs no buffering assumptions about
    // what the browser packs into the first packet.
    while !head.ends_with(b"\r\n\r\n") {
        match reader.read(&mut byte) {
            // The peer closed its end before a full head.
            Ok(0) => return Ok(()),
            Ok(_) => {
                // Once a request has STARTED, a slow next byte is not an
                // idle socket - it is a request the browser expects an
                // answer to. The body of a loopback request needs
                // milliseconds; 10 s is a fuse, not a budget.
                if !started {
                    started = true;
                    let _ = stream.set_read_timeout(Some(Duration::from_secs(10)));
                }
                head.push(byte[0]);
            }
            // A socket the browser opened, never wrote to, and then
            // abandoned is the normal case, not an error worth printing.
            Err(e) if is_idle(&e) => return Ok(()),
            Err(e) => return Err(e),
        }
        if head.len() > MAX_REQUEST_HEAD {
            return respond_closing(
                stream,
                "431 Request Header Fields Too Large",
                "text/plain",
                b"",
            );
        }
    }
    let head = String::from_utf8_lossy(&head).to_string();
    let mut parts = head.split_whitespace();
    let method = parts.next().unwrap_or_default().to_string();
    let target = parts.next().unwrap_or_default().to_string();

    // The verdict POST is the last thing a run does, so its response closes
    // like every other one.
    if method == "POST" && target == RESULT_PATH {
        let mut body = vec![0u8; content_length(&head)];
        reader.read_exact(&mut body)?;
        respond_closing(stream, "204 No Content", "text/plain", b"")?;
        if !reported.swap(true, Ordering::SeqCst) {
            let _ = tx.send(String::from_utf8_lossy(&body).to_string());
        }
        return Ok(());
    }

    if method == "POST" && target == OFFLINE_PATH {
        let _ = reader.read_exact(&mut vec![0u8; content_length(&head)]);
        offline.store(true, Ordering::SeqCst);
        return respond_closing(stream, "204 No Content", "text/plain", b"");
    }
    if offline.load(Ordering::SeqCst) {
        // The status is what the offline leg measures. The disposition is the
        // same one every response carries now; see this function's docs.
        return respond_closing(stream, "503 Service Unavailable", "text/plain", b"offline");
    }
    let Some(path) = resolve(root, &target) else {
        return respond_closing(stream, "403 Forbidden", "text/plain", b"forbidden");
    };
    match std::fs::read(&path) {
        Ok(bytes) => {
            let kind = content_type(&path);
            return respond_closing(stream, "200 OK", kind, &bytes);
        }
        Err(_) => {
            // A 404 is a response, not an error: the favicon is a 404 on
            // every run. Its disposition is the same as every other
            // response, so nothing lands behind a dying socket.
            return respond_closing(stream, "404 Not Found", "text/plain", b"not found");
        }
    }
}

/// Whether an I/O error just means "this socket went idle", rather than a
/// failure worth reporting.
///
/// `WouldBlock` is what a read timeout surfaces as on Windows, and a browser
/// abandoning a speculative socket is routine — printing for it would bury a
/// real error in noise on every single run.
fn is_idle(error: &std::io::Error) -> bool {
    matches!(
        error.kind(),
        ErrorKind::WouldBlock | ErrorKind::TimedOut | ErrorKind::ConnectionReset
    )
}

/// The `Content-Length` of a request head, or 0 when it declares none.
fn content_length(head: &str) -> usize {
    head.lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.trim()
                .eq_ignore_ascii_case("content-length")
                .then(|| value.trim().parse::<usize>().unwrap_or(0))
        })
        .unwrap_or(0)
}

/// Map a request target onto a file under `root`, refusing to leave it.
///
/// The served tree is a scratch copy of committed files and the caller is a
/// local browser, so this is belt-and-braces rather than a defence — but a
/// harness that hands out a path-traversal primitive is not a thing to leave
/// lying around next to a test suite.
///
/// The target is percent-decoded *first*: a browser will happily ask for
/// `/a/%2e%2e/secret` and a check that only looks for a literal `..` would
/// happily serve it.
fn resolve(root: &Path, target: &str) -> Option<PathBuf> {
    let path = target.split(['?', '#']).next()?;
    let relative = percent_decode(path.trim_start_matches('/'));
    let relative = if relative.is_empty() {
        "index.html".to_string()
    } else {
        relative
    };
    let mut out = root.to_path_buf();
    for part in relative.split('/') {
        if part.is_empty() || part == "." {
            continue;
        }
        // A separator or a drive prefix inside a part means the target is not
        // the plain relative path it claims to be.
        if part == ".." || part.contains('\\') || part.contains(':') {
            return None;
        }
        out.push(part);
    }
    out.starts_with(root).then_some(out)
}

/// Percent-decode a request target into bytes.
///
/// Only the three escapes that matter for a path are decoded — `%2F` (a
/// separator), `%5C` (a backslash) and `%2E` (a dot) — and anything else is
/// left exactly as it arrived. Decoding those before the traversal check is
/// the point: they are how a `..` hides from a naive scan. This is not a
/// general URL decoder and does not pretend to be; the served tree is
/// committed ASCII file names, and a name that needed decoding would not be
/// one of them.
fn percent_decode(target: &str) -> String {
    let bytes = target.as_bytes();
    let mut out = String::with_capacity(target.len());
    let mut index = 0;
    while index < bytes.len() {
        let byte = bytes[index];
        let escaped = match bytes.get(index + 1) {
            Some(second) => (byte, second.to_ascii_lowercase()),
            None => return finish(out, bytes, index),
        };
        let decoded = match escaped {
            (b'%', b'2') if bytes.get(index + 2) == Some(&b'f') => Some('/'),
            (b'%', b'5') if bytes.get(index + 2) == Some(&b'c') => Some('\\'),
            (b'%', b'2') if bytes.get(index + 2) == Some(&b'e') => Some('.'),
            _ => None,
        };
        match decoded {
            Some(ch) => {
                out.push(ch);
                index += 3;
            }
            None => {
                out.push(char::from(byte));
                index += 1;
            }
        }
    }
    out
}

/// Append the untouched remainder after a truncated escape.
fn finish(mut out: String, bytes: &[u8], index: usize) -> String {
    if let Some(rest) = bytes.get(index..) {
        out.push_str(&String::from_utf8_lossy(rest));
    }
    out
}

/// The content type for a served file.
///
/// `application/wasm` is the point of the `wasm` arm: it is what
/// `WebAssembly.instantiateStreaming` insists on, so a page that uses the
/// streaming API has to be served a real MIME type rather than a default.
fn content_type(path: &Path) -> &'static str {
    match path.extension().and_then(|e| e.to_str()) {
        Some("html") => "text/html; charset=utf-8",
        Some("js") => "text/javascript; charset=utf-8",
        Some("mjs") => "text/javascript; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("json") => "application/json",
        Some("wasm") => "application/wasm",
        Some("txt") => "text/plain; charset=utf-8",
        _ => "application/octet-stream",
    }
}

/// Write the only response on a connection; the caller closes after it.
///
/// With one response per connection there is no reuse to coordinate, so
/// [`write_response`] always renders the closing disposition and this name is
/// the only entry the serving paths use.
///
/// `Cache-Control: max-age=0, must-revalidate` keeps a served file from
/// answering the offline leg out of the HTTP cache without revalidation: the
/// *worker's* cache is what that check measures, and a fresh network answer
/// would prove nothing.
fn respond_closing(
    stream: &TcpStream,
    status: &str,
    kind: &str,
    body: &[u8],
) -> std::io::Result<()> {
    write_response(stream, status, kind, body)
}

/// Write one HTTP/1.1 response and finish the conversation on it.
///
/// `Connection: close` is part of the head, so the client knows the socket is
/// spent before it is done reading — no further request can be queued behind
/// the close, and no parked socket outlives the answer it served. Where the
/// trickier history started is `handle_connection`'s doc.
fn write_response(
    stream: &TcpStream,
    status: &str,
    kind: &str,
    body: &[u8],
) -> std::io::Result<()> {
    let head = format!(
        "HTTP/1.1 {status}\r\nContent-Type: {kind}\r\nContent-Length: {}\r\n\
         Cache-Control: max-age=0, must-revalidate\r\nConnection: close\r\n\r\n",
        body.len()
    );
    let mut writer = stream.try_clone()?;
    writer.write_all(head.as_bytes())?;
    writer.write_all(body)?;
    writer.flush()
}

/// Print the summary, write the report, and turn any failure into an `Err`.
///
/// The report is written on every path that reached a browser, pass or fail,
/// because it is the CI artifact and "why did it go red" is answered by it.
fn summarise(cfg: &Config, browser: &Path, rows: &[Row]) -> Result<(), String> {
    let measured = rows.iter().filter(|r| r.report.is_some()).count();
    write_report(cfg, Some(browser), rows, measured, None)?;

    // SL-3.CONF.06's rule: a gate that measured nothing is not a green gate.
    if measured == 0 {
        return Err(format!(
            "0 of {} check(s) reported a result -- refusing green on an unmeasured run",
            rows.len()
        ));
    }
    let failures: Vec<String> = rows
        .iter()
        .filter(|r| r.status != Status::Pass)
        .map(|r| format!("{} [{}]: {}", r.id, r.status.name(), r.detail))
        .collect();
    if failures.is_empty() {
        println!(
            "browser-check: {measured} check(s) measured, all pass ({} reported)",
            browser_name(browser)
        );
        return Ok(());
    }
    let mut msg = format!(
        "browser-check: {} of {measured} measured check(s) did not pass:\n",
        failures.len()
    );
    for failure in &failures {
        msg.push_str(&format!("  {failure}\n"));
    }
    Err(msg)
}

/// The no-engine path: a loud, recorded skip — or a failure under `--strict`.
///
/// It is never a silent success. The report records `"skipped": true` with the
/// paths probed, because the failure this guards against is a run that looks
/// green and measured nothing.
fn skip(cfg: &Config, probed: &str) -> Result<(), String> {
    println!("browser-check: SKIP -- nothing was measured. {probed}");
    write_report(cfg, None, &[], 0, Some(probed))?;
    if cfg.strict {
        Err(format!(
            "--strict: {probed} (the runner running this is expected to have an engine)"
        ))
    } else {
        Ok(())
    }
}

/// Write the machine-readable report, whether the run passed, failed or
/// skipped.
fn write_report(
    cfg: &Config,
    browser: Option<&Path>,
    rows: &[Row],
    measured: usize,
    skip_reason: Option<&str>,
) -> Result<(), String> {
    let checks: Vec<Json> = rows
        .iter()
        .map(|row| {
            json!({
                "id": row.id,
                "status": row.status.name(),
                "detail": row.detail,
                "report": row.report,
            })
        })
        .collect();
    let report = json!({
        "version": 1,
        "task": "SL-4.WEB.02",
        "engine": browser.map(|p| p.display().to_string()),
        "engine_name": browser.map(browser_name),
        "timeout_ms": cfg.timeout_ms,
        // A run that was skipped measured nothing. It is reported as its own
        // state, never as a pass, so an artifact reader cannot tell the
        // difference between "green" and "never ran".
        "skipped": skip_reason.is_some(),
        "skip_reason": skip_reason,
        "checks_run": rows.len(),
        "measured": measured,
        "checks": checks,
    });
    if let Some(parent) = cfg.out.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
        }
    }
    let text =
        serde_json::to_string_pretty(&report).map_err(|e| format!("serialise report: {e}"))?;
    std::fs::write(&cfg.out, text).map_err(|e| format!("{}: {e}", cfg.out.display()))?;
    println!("browser-check: wrote {}", cfg.out.display());
    Ok(())
}

#[cfg(test)]
mod tests {
    #![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]
    use super::*;

    /// The report a real Edge produced for `service-worker` on the development
    /// machine, verbatim. The verdict is written against a *recorded* run, not
    /// against a shape invented here, so a change in what the browser actually
    /// says shows up as a failing verdict rather than as a passing new one.
    fn real_service_worker_report() -> Json {
        json!({
            "schema": SCHEMA,
            "check": "service-worker",
            "scope": "http://127.0.0.1:38122/",
            "controlled": true,
            "asset": ASSET_BODY,
            "fatal": Json::Null,
        })
    }

    /// The same, for `async-wasm`.
    fn real_async_wasm_report() -> Json {
        json!({
            "schema": SCHEMA,
            "check": "async-wasm",
            "sum": WASM_PROBE_SUM,
            "linkError": "LinkError: WebAssembly.Instance(): Import #1 module=\"env\" error: module is not an object or function",
            "fatal": Json::Null,
        })
    }

    /// The report a real Edge produced for `extension` on the development
    /// machine, verbatim except for the ephemeral port. The ink hashes are
    /// the bytes the surface actually drew, and `inkPixels` counts sampled
    /// pixels that differ from the page background - the canvas is created
    /// `{ alpha: false }`, so counting alpha would call a white page inked.
    fn real_extension_report() -> Json {
        json!({
            "schema": SCHEMA,
            "check": "extension",
            "fatal": Json::Null,
            "stage": "done",
            "elapsedMs": 783,
            "href": "chrome-extension://ppekgfikbjkfgckpcjibplnhhaaonaem/viewer.html?src=http%3A%2F%2F127.0.0.1%3A51845%2Fdoc.pdf",
            "protocol": "chrome-extension:",
            "extensionId": EXTENSION_ID,
            "src": "http://127.0.0.1:51845/doc.pdf",
            "origin": "http://127.0.0.1:51845",
            "composedMs": 400,
            "status": "Opened doc.pdf — 2 pages.",
            "mount": {
                "hidden": false,
                "children": 1,
                "width": 693.0,
                "height": 669.5,
            },
            "outline": {
                "rows": 2,
                "pages": ["0", "1"],
                "titles": ["First layer", "Second layer"],
                "labels": ["First layer, Page 1 of 2", "Second layer, Page 2 of 2"],
                "currentRow": 0,
                "notes": [],
            },
            "pageList": {
                "tiles": 2,
                "live": "Page 1 of 2 — fit page",
                "scrollTop": 0,
                "scrollHeight": 866,
                "clientHeight": 441,
            },
            "rail": {
                "present": true,
                "label": "Page thumbnails",
                "thumbnails": 2,
                "previews": 2,
                "currentPage": "0",
                "scrollHeight": 292,
                "clientHeight": 195,
            },
            "link": {
                "found": true,
                "count": 1,
                "page": "0",
                "actionKind": "uri",
                "label": "Link",
                "hit": true,
                "hitRole": "page-link",
                "prompt": {
                    "title": "Open an external link?",
                    "body": "This document links to https://example.com/a on example.com.",
                    "host": "example.com",
                    "cancel": "Cancel",
                },
                "closed": true,
            },
            "press": {
                "attempted": true,
                "index": 1,
                "page": "1",
                "title": "Second layer",
                "label": "Second layer, Page 2 of 2",
                "before": {
                    "live": "Page 1 of 2 — fit page",
                    "navLive": "",
                    "currentRow": 0,
                    "currentRowPage": "0",
                    "currentTile": "Page 1 of 2",
                    "railCurrent": "0",
                    "scrollTop": 0,
                    "ink": {
                        "width": 683,
                        "height": 441,
                        "hash": "c68900ff",
                        "inkPixels": 278,
                        "sampled": 3106,
                    },
                },
                "after": {
                    "live": "Page 2 of 2 — fit page",
                    "navLive": "Moved to Page 2 of 2",
                    "currentRow": 1,
                    "currentRowPage": "1",
                    "currentTile": "Page 2 of 2",
                    "railCurrent": "1",
                    "scrollTop": 425,
                    "ink": {
                        "width": 683,
                        "height": 441,
                        "hash": "8d0e51f4",
                        "inkPixels": 278,
                        "sampled": 3106,
                    },
                },
                "changed": {
                    "ink": true,
                    "live": true,
                    "navLive": true,
                    "currentRow": true,
                    "railCurrent": true,
                    "scrollTop": true,
                },
            },
            "key": {
                "attempted": true,
                "key": "ArrowUp",
                "prevented": true,
                "moved": true,
                "before": {
                    "live": "Page 2 of 2 — fit page",
                    "navLive": "Moved to Page 2 of 2",
                    "currentRow": 1,
                    "currentRowPage": "1",
                    "currentTile": "Page 2 of 2",
                    "railCurrent": "1",
                    "scrollTop": 425,
                    "ink": {
                        "width": 683,
                        "height": 441,
                        "hash": "8d0e51f4",
                        "inkPixels": 278,
                        "sampled": 3106,
                    },
                },
                "after": {
                    "live": "Page 1 of 2 — fit page",
                    "navLive": "Moved to Page 2 of 2",
                    "currentRow": 0,
                    "currentRowPage": "0",
                    "currentTile": "Page 1 of 2",
                    "railCurrent": "0",
                    "scrollTop": 16,
                    "ink": {
                        "width": 683,
                        "height": 441,
                        "hash": "13df0979",
                        "inkPixels": 282,
                        "sampled": 3106,
                    },
                },
            },
        })
    }

    /// A split leg shared by the online and offline halves of the tools-page
    /// report. The byte counts are representative; the verdict checks headers,
    /// kinds, names, page totals, and the over-target flag rather than a
    /// particular compressor output.
    fn real_tools_split_leg_for(
        pdf_names: &[&str],
        zip_name: &str,
        title: &str,
        pages_row: &str,
        over_target: bool,
    ) -> Json {
        let mut outputs = Vec::new();
        for (index, name) in pdf_names.iter().enumerate() {
            let bytes = 601 + index;
            outputs.push(json!({
                "kind": "pdf",
                "name": name,
                "pages": 1,
                "declaredBytes": bytes,
                "bytes": bytes,
                "blob": true,
                "previewBlob": true,
                "downloadName": name,
                "header": "%PDF-",
                "zip": false,
                "overTarget": over_target,
            }));
        }
        outputs.push(json!({
            "kind": "zip",
            "name": zip_name,
            "pages": 0,
            "declaredBytes": 1200,
            "bytes": 1200,
            "blob": true,
            "previewBlob": Json::Null,
            "downloadName": zip_name,
            "header": "PK",
            "zip": true,
        }));
        let files = pdf_names.len().saturating_add(1);
        json!({
            "fileRows": 1,
            "runLabel": "Split PDF",
            "noUpload": "verified",
            "status": "Done. The result was verified on this device.",
            "result": {
                "title": title,
                "meta": format!("{files} files · 2.4 KiB · verified locally"),
                "rows": [
                    { "label": "Pages", "value": pages_row },
                    { "label": "Annotations", "value": "0 \u{2192} 0" },
                    { "label": "Verification", "value": "ok" },
                ],
            },
            "outputs": outputs,
            "network": {
                "origin": "http://127.0.0.1:1234",
                "resources": 1,
                "foreign": [],
                "paths": ["/wasm/selis_pdf_wasm_tools.wasm"],
            },
        })
    }

    fn real_tools_split_leg() -> Json {
        real_tools_split_leg_for(
            &[
                "report-pages-1-1.pdf",
                "report-pages-2-2.pdf",
                "report-pages-3-3.pdf",
            ],
            "report-split.zip",
            "3 PDFs and a ZIP",
            "3 \u{2192} 3",
            false,
        )
    }

    fn real_tools_size_split_leg() -> Json {
        real_tools_split_leg_for(
            &["outlined-size-1-1.pdf", "outlined-size-2-2.pdf"],
            "outlined-split.zip",
            "2 PDFs and a ZIP",
            "2 \u{2192} 2",
            true,
        )
    }

    fn real_tools_bookmark_split_leg() -> Json {
        real_tools_split_leg_for(
            &["outlined-bookmarks-1-1.pdf", "outlined-bookmarks-2-2.pdf"],
            "outlined-split.zip",
            "2 PDFs and a ZIP",
            "2 \u{2192} 2",
            false,
        )
    }

    /// A duplicate or blank-insertion leg shared by the online and offline
    /// halves of the tools-page report. Both add one page to the two-page
    /// outlined fixture and produce one PDF.
    fn real_tools_organize_leg(name: &str) -> Json {
        let output = json!({
            "kind": "pdf",
            "name": name,
            "pages": 3,
            "declaredBytes": 780,
            "bytes": 780,
            "blob": true,
            "previewBlob": true,
            "downloadName": name,
            "header": "%PDF-",
            "zip": false,
            "overTarget": false,
        });
        json!({
            "fileRows": 1,
            "runLabel": "Organize pages",
            "noUpload": "verified",
            "status": "Done. The result was verified on this device.",
            "result": {
                "title": name,
                "meta": "1 file \u{00b7} 780 B \u{00b7} verified locally",
                "rows": [
                    { "label": "Pages", "value": "2 \u{2192} 3" },
                    { "label": "Annotations", "value": "0 \u{2192} 0" },
                    { "label": "Verification", "value": "ok" },
                ],
            },
            "outputs": [output.clone()],
            "output": output,
            "network": {
                "origin": "http://127.0.0.1:1234",
                "resources": 1,
                "foreign": [],
                "paths": ["/wasm/selis_pdf_wasm_tools.wasm"],
            },
        })
    }

    /// A stamp or compression leg: one PDF in, one PDF out, same page count.
    fn real_tools_stamp_leg(name: &str, run_label: &str, pages: u64, needles: &[&str]) -> Json {
        let found: Vec<bool> = needles.iter().map(|_| true).collect();
        let output = json!({
            "kind": "pdf",
            "name": name,
            "pages": pages,
            "declaredBytes": 900,
            "bytes": 900,
            "blob": true,
            "previewBlob": true,
            "downloadName": name,
            "header": "%PDF-",
            "zip": false,
            "overTarget": false,
            "found": found,
        });
        json!({
            "fileRows": 1,
            "runLabel": run_label,
            "noUpload": "verified",
            "status": "Done. The result was verified on this device.",
            "result": {
                "title": name,
                "meta": "1 file \u{00b7} 900 B \u{00b7} verified locally",
                "rows": [
                    { "label": "Pages", "value": format!("{pages} \u{2192} {pages}") },
                    { "label": "Annotations", "value": "0 \u{2192} 0" },
                    { "label": "Verification", "value": "ok" },
                ],
            },
            "outputs": [output.clone()],
            "output": output,
            "network": {
                "origin": "http://127.0.0.1:1234",
                "resources": 1,
                "foreign": [],
                "paths": ["/wasm/selis_pdf_wasm_tools.wasm"],
            },
        })
    }

    /// Two decoded embedded files and the attachment ZIP, in the shape the
    /// attachment leg reports. File outputs have no preview and no page count.
    fn real_tools_attachments_leg() -> Json {
        let notes = json!({
            "kind": "file",
            "name": "notes.txt",
            "pages": 0,
            "declaredBytes": 16,
            "bytes": 16,
            "blob": true,
            "previewBlob": Json::Null,
            "downloadName": "notes.txt",
            "header": "hello",
            "zip": false,
            "text": "hello attachment",
        });
        let evil = json!({
            "kind": "file",
            "name": "evil.txt",
            "pages": 0,
            "declaredBytes": 9,
            "bytes": 9,
            "blob": true,
            "previewBlob": Json::Null,
            "downloadName": "evil.txt",
            "header": "safe ",
            "zip": false,
            "text": "safe name",
        });
        let zip = json!({
            "kind": "zip",
            "name": "bundle-attachments.zip",
            "pages": 0,
            "declaredBytes": 500,
            "bytes": 500,
            "blob": true,
            "previewBlob": Json::Null,
            "downloadName": "bundle-attachments.zip",
            "header": "PK",
            "zip": true,
        });
        json!({
            "fileRows": 1,
            "runLabel": "Extract attachments",
            "noUpload": "verified",
            "status": "Done. The result was verified on this device.",
            "result": {
                "title": "2 attachments and a ZIP",
                "meta": "3 files \u{00b7} 525 B \u{00b7} verified locally",
                "rows": [
                    { "label": "Embedded files", "value": "2 \u{2192} 2" },
                    { "label": "Size", "value": "925 B \u{2192} 25 B" },
                    { "label": "Verification", "value": "ok" },
                ],
            },
            "outputs": [notes, evil, zip],
            "network": {
                "origin": "http://127.0.0.1:1234",
                "resources": 1,
                "foreign": [],
                "paths": ["/wasm/selis_pdf_wasm_tools.wasm"],
            },
        })
    }

    fn real_tools_mix_leg() -> Json {
        json!({
            "fileRows": 2,
            "runLabel": "Mix pages",
            "noUpload": "verified",
            "status": "Done. The result was verified on this device.",
            "result": {
                "title": "report-mixed.pdf",
                "meta": "1 file \u{00b7} 1.2 KiB \u{00b7} verified locally",
                "rows": [
                    { "label": "Pages", "value": "4 \u{2192} 4" },
                    { "label": "Verification", "value": "ok" },
                ],
            },
            "outputs": [{
                "kind": "pdf",
                "name": "report-mixed.pdf",
                "pages": 4,
                "declaredBytes": 1200,
                "bytes": 1200,
                "blob": true,
                "previewBlob": true,
                "downloadName": "report-mixed.pdf",
                "header": "%PDF-",
                "zip": false,
            }],
            "output": {
                "kind": "pdf",
                "name": "report-mixed.pdf",
                "pages": 4,
                "declaredBytes": 1200,
                "bytes": 1200,
                "blob": true,
                "previewBlob": true,
                "downloadName": "report-mixed.pdf",
                "header": "%PDF-",
                "zip": false,
            },
            "network": {
                "origin": "http://127.0.0.1:1234",
                "resources": 1,
                "foreign": [],
                "paths": ["/wasm/selis_pdf_wasm_tools.wasm"],
            },
        })
    }

    /// A report in the shape `xtask/browser/tools-page/index.html` emits after
    /// a real merge and a split-every run. The numbers are representative; the
    /// assertions are about the shape and the claims, not about a particular
    /// compression ratio.
    fn real_tools_page_report() -> Json {
        let mut report = json!({
            "schema": SCHEMA,
            "check": "tools-page",
            "fatal": Json::Null,
            "cards": 13,
            "fileRows": 2,
            "noUpload": "verified",
            "status": "Done. The result was verified on this device.",
            "result": {
                "title": "merged.pdf",
                "meta": "848 B · verified locally",
                "rows": [
                    { "label": "Pages", "value": "2 \u{2192} 2" },
                    { "label": "Annotations", "value": "0 \u{2192} 0" },
                    { "label": "Verification", "value": "ok" },
                ],
            },
            "output": {
                "blob": true,
                "previewBlob": true,
                "downloadName": "merged.pdf",
                "bytes": 848,
                "header": "%PDF-",
            },
            "network": {
                "origin": "http://127.0.0.1:1234",
                "resources": 9,
                "foreign": [],
                "paths": [
                    "/tools.html",
                    "/assets/tools.css",
                    "/assets/host/sw-policy.js",
                    "/assets/host/sw-register.js",
                    "/assets/host/tools/tools-page.js",
                    "/assets/host/tools/protocol.js",
                    "/assets/host/tools/inputs.js",
                    "/assets/host/no-upload.js",
                    "/wasm/selis_pdf_wasm_tools.wasm",
                ],
            },
            "split": real_tools_split_leg(),
            "splitSize": real_tools_size_split_leg(),
            "splitBookmarks": real_tools_bookmark_split_leg(),
            "duplicate": real_tools_organize_leg("outlined-duplicated.pdf"),
            "blanks": real_tools_organize_leg("outlined-blanks.pdf"),
            "sw": {
                "registration": "registered",
                "script": "http://127.0.0.1:1234/sw.js",
                "controlled": true,
            },
            "offline": {
                "cards": 13,
                "controlled": true,
                "script": "http://127.0.0.1:1234/sw.js",
                "pageTitle": "Selis Tools — PDFs, handled here",
                "fileRows": 2,
                "runLabel": "Merge PDFs",
                "noUpload": "verified",
                "status": "Done. The result was verified on this device.",
                "result": {
                    "title": "merged.pdf",
                    "meta": "848 B · verified locally",
                    "rows": [
                        { "label": "Pages", "value": "2 \u{2192} 2" },
                        { "label": "Annotations", "value": "0 \u{2192} 0" },
                        { "label": "Verification", "value": "ok" },
                    ],
                },
                "output": {
                    "blob": true,
                    "previewBlob": true,
                    "downloadName": "merged.pdf",
                    "bytes": 848,
                    "header": "%PDF-",
                },
                "network": {
                    "origin": "http://127.0.0.1:1234",
                    "resources": 9,
                    "foreign": [],
                    "paths": [
                        "/tools.html",
                        "/assets/tools.css",
                        "/assets/host/sw-policy.js",
                        "/assets/host/sw-register.js",
                        "/assets/host/tools/tools-page.js",
                        "/assets/host/tools/protocol.js",
                        "/assets/host/tools/inputs.js",
                        "/assets/host/no-upload.js",
                        "/wasm/selis_pdf_wasm_tools.wasm",
                    ],
                },
                "split": real_tools_split_leg(),
                "splitSize": real_tools_size_split_leg(),
                "splitBookmarks": real_tools_bookmark_split_leg(),
                "duplicate": real_tools_organize_leg("outlined-duplicated.pdf"),
                "blanks": real_tools_organize_leg("outlined-blanks.pdf"),
            },
        });

        report["mix"] = real_tools_mix_leg();
        report["pageNumbers"] = real_tools_stamp_leg(
            "report-numbered.pdf",
            "Add page numbers",
            3,
            &["(5/3) Tj", "(6/3) Tj"],
        );
        report["watermark"] = real_tools_stamp_leg(
            "outlined-watermarked.pdf",
            "Add watermark",
            2,
            &["(CONFIDENTIAL) Tj", "/SelisOverlayGS gs"],
        );
        report["headerFooter"] = real_tools_stamp_leg(
            "report-header-footer.pdf",
            "Add header & footer",
            3,
            &["(Page 1 of 3) Tj", "(Center) Tj", "(Draft) Tj"],
        );
        report["bates"] = real_tools_stamp_leg(
            "report-bates.pdf",
            "Add Bates numbers",
            3,
            &["(ABC-0009) Tj", "(ABC-0010) Tj"],
        );
        report["compress"] = real_tools_stamp_leg("report-compressed.pdf", "Compress PDF", 3, &[]);
        report["attachments"] = real_tools_attachments_leg();

        report["offline"]["mix"] = report["mix"].clone();
        report["offline"]["pageNumbers"] = report["pageNumbers"].clone();
        report["offline"]["watermark"] = report["watermark"].clone();
        report["offline"]["headerFooter"] = report["headerFooter"].clone();
        report["offline"]["bates"] = report["bates"].clone();
        report["offline"]["compress"] = report["compress"].clone();
        report["offline"]["attachments"] = report["attachments"].clone();
        report
    }

    #[test]
    fn the_verdicts_accept_what_a_real_browser_reported() {
        assert!(verdict_service_worker(&real_service_worker_report()).is_ok());
        assert!(verdict_async_wasm(&real_async_wasm_report()).is_ok());
        assert!(verdict_extension(&real_extension_report()).is_ok());
        assert!(verdict_tools_page(&real_tools_page_report()).is_ok());
    }

    /// The tools verdict's witnesses, withdrawn one at a time. A merge that
    /// rendered a result but never proved the local-only claim must not pass,
    /// and neither must a same-origin page that quietly fetched a resource from
    /// somewhere else.
    #[test]
    fn the_tools_verdict_fails_when_a_witness_is_withdrawn() {
        let report = real_tools_page_report();

        let mut unverified = report.clone();
        *unverified.pointer_mut("/noUpload").unwrap() = json!("watching");
        assert!(
            verdict_tools_page(&unverified).is_err(),
            "a result shown before the no-upload assertion must not pass"
        );

        let mut foreign = report.clone();
        *foreign.pointer_mut("/network/foreign").unwrap() =
            json!(["https://example.invalid/upload"]);
        assert!(
            verdict_tools_page(&foreign).is_err(),
            "a resource from another origin must not pass"
        );

        let mut not_a_pdf = report.clone();
        *not_a_pdf.pointer_mut("/output/header").unwrap() = json!("<html");
        assert!(
            verdict_tools_page(&not_a_pdf).is_err(),
            "a download that is not a PDF must not pass"
        );

        let mut wrong_pages = report.clone();
        *wrong_pages.pointer_mut("/result/rows/0/value").unwrap() = json!("1 \u{2192} 1");
        assert!(
            verdict_tools_page(&wrong_pages).is_err(),
            "a merge that did not add the second page must not pass"
        );

        let mut missing_chunk = report.clone();
        *missing_chunk.pointer_mut("/network/paths").unwrap() = json!(["/tools.html"]);
        assert!(
            verdict_tools_page(&missing_chunk).is_err(),
            "a page that never loaded the lazy WASM chunk must not pass"
        );

        let mut no_offline = report.clone();
        no_offline
            .as_object_mut()
            .expect("report object")
            .remove("offline");
        assert!(
            verdict_tools_page(&no_offline).is_err(),
            "an online-only run must not pass the offline gate"
        );

        let mut uncontrolled = report.clone();
        *uncontrolled.pointer_mut("/offline/controlled").unwrap() = json!(false);
        assert!(
            verdict_tools_page(&uncontrolled).is_err(),
            "an offline page with no worker controller must not pass"
        );

        let mut offline_unverified = report.clone();
        *offline_unverified.pointer_mut("/offline/noUpload").unwrap() = json!("watching");
        assert!(
            verdict_tools_page(&offline_unverified).is_err(),
            "an offline result shown before the no-upload assertion must not pass"
        );

        let mut offline_foreign = report.clone();
        *offline_foreign
            .pointer_mut("/offline/network/foreign")
            .unwrap() = json!(["https://example.invalid/upload"]);
        assert!(
            verdict_tools_page(&offline_foreign).is_err(),
            "an offline resource from another origin must not pass"
        );

        let mut offline_wrong_pages = report.clone();
        *offline_wrong_pages
            .pointer_mut("/offline/result/rows/0/value")
            .unwrap() = json!("1 \u{2192} 1");
        assert!(
            verdict_tools_page(&offline_wrong_pages).is_err(),
            "an offline merge that did not add the second page must not pass"
        );

        let mut no_mix = report.clone();
        no_mix.as_object_mut().expect("report object").remove("mix");
        assert!(
            verdict_tools_page(&no_mix).is_err(),
            "a run that never interleaved two PDFs must not pass the mix gate"
        );

        let mut mix_wrong_pages = report.clone();
        *mix_wrong_pages
            .pointer_mut("/mix/result/rows/0/value")
            .unwrap() = json!("3 \u{2192} 3");
        assert!(
            verdict_tools_page(&mix_wrong_pages).is_err(),
            "a mix that dropped a page must not pass"
        );

        let mut offline_no_mix = report.clone();
        offline_no_mix
            .pointer_mut("/offline")
            .unwrap()
            .as_object_mut()
            .expect("offline object")
            .remove("mix");
        assert!(
            verdict_tools_page(&offline_no_mix).is_err(),
            "an online-only mix must not pass the offline gate"
        );

        let mut no_split = report.clone();
        no_split
            .as_object_mut()
            .expect("report object")
            .remove("split");
        assert!(
            verdict_tools_page(&no_split).is_err(),
            "a run that never split a document must not pass the Phase 1A.1 gate"
        );

        let mut split_without_zip = report.clone();
        *split_without_zip
            .pointer_mut("/split/outputs/3/zip")
            .unwrap() = json!(false);
        assert!(
            verdict_tools_page(&split_without_zip).is_err(),
            "a multi-output split without a real ZIP header must not pass"
        );

        let mut split_wrong_pages = report.clone();
        *split_wrong_pages
            .pointer_mut("/split/result/rows/0/value")
            .unwrap() = json!("2 \u{2192} 2");
        assert!(
            verdict_tools_page(&split_wrong_pages).is_err(),
            "a split that dropped a page must not pass"
        );

        let mut split_missing_output = report.clone();
        split_missing_output
            .pointer_mut("/split/outputs")
            .unwrap()
            .as_array_mut()
            .expect("outputs array")
            .pop();
        assert!(
            verdict_tools_page(&split_missing_output).is_err(),
            "a split without the ZIP output must not pass"
        );

        let mut no_size_split = report.clone();
        no_size_split
            .as_object_mut()
            .expect("report object")
            .remove("splitSize");
        assert!(
            verdict_tools_page(&no_size_split).is_err(),
            "a run that never exercised split-by-size must not pass the Phase 1A.1 gate"
        );

        let mut size_flag_lost = report.clone();
        *size_flag_lost
            .pointer_mut("/splitSize/outputs/0/overTarget")
            .unwrap() = json!(false);
        assert!(
            verdict_tools_page(&size_flag_lost).is_err(),
            "a size split that hid an over-target part must not pass"
        );

        let mut no_bookmark_split = report.clone();
        no_bookmark_split
            .as_object_mut()
            .expect("report object")
            .remove("splitBookmarks");
        assert!(
            verdict_tools_page(&no_bookmark_split).is_err(),
            "a run that never exercised split-by-bookmarks must not pass the Phase 1A.1 gate"
        );

        let mut offline_no_size_split = report.clone();
        offline_no_size_split
            .pointer_mut("/offline")
            .unwrap()
            .as_object_mut()
            .expect("offline object")
            .remove("splitSize");
        assert!(
            verdict_tools_page(&offline_no_size_split).is_err(),
            "an offline run without split-by-size must not pass the Phase 1A.1 gate"
        );

        let mut offline_no_split = report.clone();
        *offline_no_split.pointer_mut("/offline").unwrap() = json!({
            "cards": 13,
            "controlled": true,
            "script": "http://127.0.0.1:1234/sw.js",
            "pageTitle": "Selis Tools",
            "fileRows": 2,
            "runLabel": "Merge PDFs",
            "noUpload": "verified",
            "status": "Done. The result was verified on this device.",
            "result": {
                "title": "merged.pdf",
                "rows": [
                    { "label": "Pages", "value": "2 \u{2192} 2" },
                    { "label": "Verification", "value": "ok" },
                ],
            },
            "output": {
                "blob": true,
                "previewBlob": true,
                "downloadName": "merged.pdf",
                "bytes": 848,
                "header": "%PDF-",
            },
            "network": { "origin": "http://127.0.0.1:1234", "resources": 9, "foreign": [], "paths": [] },
        });
        assert!(
            verdict_tools_page(&offline_no_split).is_err(),
            "an online-only split must not pass the offline gate"
        );

        let mut no_duplicate = report.clone();
        no_duplicate
            .as_object_mut()
            .expect("report object")
            .remove("duplicate");
        assert!(
            verdict_tools_page(&no_duplicate).is_err(),
            "a run that never duplicated a page must not pass the Phase 1A.1 organize gate"
        );

        let mut duplicate_wrong_pages = report.clone();
        *duplicate_wrong_pages
            .pointer_mut("/duplicate/result/rows/0/value")
            .unwrap() = json!("2 \u{2192} 2");
        assert!(
            verdict_tools_page(&duplicate_wrong_pages).is_err(),
            "a duplicate run that did not add a page must not pass"
        );

        let mut blank_wrong_name = report.clone();
        *blank_wrong_name
            .pointer_mut("/blanks/output/downloadName")
            .unwrap() = json!("outlined-duplicated.pdf");
        assert!(
            verdict_tools_page(&blank_wrong_name).is_err(),
            "a blank-page download with the wrong name must not pass"
        );

        let mut offline_no_duplicate = report.clone();
        offline_no_duplicate
            .pointer_mut("/offline")
            .unwrap()
            .as_object_mut()
            .expect("offline object")
            .remove("duplicate");
        assert!(
            verdict_tools_page(&offline_no_duplicate).is_err(),
            "an online-only duplicate must not pass the offline organize gate"
        );

        for (field, message) in [
            (
                "pageNumbers",
                "a run that never numbered pages must not pass",
            ),
            (
                "watermark",
                "a run that never watermarked a page must not pass",
            ),
            (
                "headerFooter",
                "a run that never wrote a header/footer must not pass",
            ),
            (
                "bates",
                "a run that never applied Bates numbers must not pass",
            ),
            (
                "compress",
                "a run that never compressed a PDF must not pass",
            ),
            (
                "attachments",
                "a run that never extracted attachments must not pass",
            ),
        ] {
            let mut missing = report.clone();
            missing
                .as_object_mut()
                .expect("report object")
                .remove(field);
            assert!(verdict_tools_page(&missing).is_err(), "{message}");
        }

        let mut marker_missing = report.clone();
        *marker_missing
            .pointer_mut("/pageNumbers/outputs/0/found/0")
            .unwrap() = json!(false);
        assert!(
            verdict_tools_page(&marker_missing).is_err(),
            "a page-number output without the stamped text must not pass"
        );

        let mut bates_wrong_name = report.clone();
        *bates_wrong_name
            .pointer_mut("/bates/outputs/0/downloadName")
            .unwrap() = json!("outlined.pdf");
        assert!(
            verdict_tools_page(&bates_wrong_name).is_err(),
            "a Bates download with the input's name must not pass"
        );

        let mut compress_wrong_pages = report.clone();
        *compress_wrong_pages
            .pointer_mut("/compress/result/rows/0/value")
            .unwrap() = json!("2 \u{2192} 2");
        assert!(
            verdict_tools_page(&compress_wrong_pages).is_err(),
            "a compress run that changed the page count must not pass"
        );

        let mut attachment_as_pdf = report.clone();
        *attachment_as_pdf
            .pointer_mut("/attachments/outputs/0/kind")
            .unwrap() = json!("pdf");
        assert!(
            verdict_tools_page(&attachment_as_pdf).is_err(),
            "an attachment presented as a PDF must not pass"
        );

        let mut attachment_wrong_bytes = report.clone();
        *attachment_wrong_bytes
            .pointer_mut("/attachments/outputs/0/text")
            .unwrap() = json!("not the embedded file");
        assert!(
            verdict_tools_page(&attachment_wrong_bytes).is_err(),
            "an attachment with the wrong decoded bytes must not pass"
        );

        let mut attachment_no_zip = report.clone();
        *attachment_no_zip
            .pointer_mut("/attachments/outputs/2/zip")
            .unwrap() = json!(false);
        assert!(
            verdict_tools_page(&attachment_no_zip).is_err(),
            "an attachment leg without a real ZIP header must not pass"
        );

        let mut no_offline_attachments = report.clone();
        no_offline_attachments
            .pointer_mut("/offline")
            .unwrap()
            .as_object_mut()
            .expect("offline object")
            .remove("attachments");
        assert!(
            verdict_tools_page(&no_offline_attachments).is_err(),
            "an online-only attachment extraction must not pass the offline gate"
        );

        let mut offline_no_watermark = report.clone();
        offline_no_watermark
            .pointer_mut("/offline")
            .unwrap()
            .as_object_mut()
            .expect("offline object")
            .remove("watermark");
        assert!(
            verdict_tools_page(&offline_no_watermark).is_err(),
            "an online-only watermark must not pass the offline gate"
        );
    }

    /// The extension verdict's witnesses, withdrawn one at a time. Each control
    /// keeps every other claim, so it fails for the reason under test rather
    /// than because the report stopped being well-formed.
    #[test]
    fn the_extension_verdict_fails_when_a_witness_is_withdrawn() {
        let report = real_extension_report();

        let mut no_outline = report.clone();
        *no_outline.pointer_mut("/outline/rows").unwrap() = json!(0);
        assert!(
            verdict_extension(&no_outline).is_err(),
            "an outline panel with no rows must not pass"
        );

        let mut silent = report.clone();
        *silent.pointer_mut("/press/changed/navLive").unwrap() = json!(false);
        assert!(
            verdict_extension(&silent).is_err(),
            "a move with no announcement must not pass"
        );

        let mut same_pixels = report.clone();
        *same_pixels.pointer_mut("/press/after/ink/hash").unwrap() = json!("c68900ff");
        assert!(
            verdict_extension(&same_pixels).is_err(),
            "repainting the same page is not a move"
        );

        let mut blank = report.clone();
        *blank.pointer_mut("/press/before/ink/inkPixels").unwrap() = json!(0);
        assert!(
            verdict_extension(&blank).is_err(),
            "a blank canvas before the press must not pass"
        );

        let mut no_scroll = report.clone();
        *no_scroll.pointer_mut("/press/after/scrollTop").unwrap() = json!(0);
        assert!(
            verdict_extension(&no_scroll).is_err(),
            "a reader that did not scroll must not pass"
        );

        let mut no_key = report.clone();
        *no_key.pointer_mut("/key").unwrap() = Json::Null;
        assert!(
            verdict_extension(&no_key).is_err(),
            "a stage that never tried its key map must not pass"
        );

        let mut native_scroll = report.clone();
        *native_scroll.pointer_mut("/key/prevented").unwrap() = json!(false);
        assert!(
            verdict_extension(&native_scroll).is_err(),
            "a stage that left ArrowUp to native scrolling must not pass"
        );

        let mut no_rail = report.clone();
        *no_rail.pointer_mut("/rail/present").unwrap() = json!(false);
        assert!(
            verdict_extension(&no_rail).is_err(),
            "a viewer with no thumbnail rail must not pass"
        );

        let mut blank_rail = report.clone();
        *blank_rail.pointer_mut("/rail/previews").unwrap() = json!(0);
        assert!(
            verdict_extension(&blank_rail).is_err(),
            "a rail of empty boxes must not pass as painted previews"
        );

        let mut rail_stuck = report.clone();
        *rail_stuck
            .pointer_mut("/press/changed/railCurrent")
            .unwrap() = json!(false);
        assert!(
            verdict_extension(&rail_stuck).is_err(),
            "a rail stuck on the old page must not pass"
        );

        let mut no_link = report.clone();
        *no_link.pointer_mut("/link/found").unwrap() = json!(false);
        assert!(
            verdict_extension(&no_link).is_err(),
            "a viewer that painted no link overlay must not pass"
        );

        let mut unreachable_link = report.clone();
        *unreachable_link.pointer_mut("/link/hit").unwrap() = json!(false);
        assert!(
            verdict_extension(&unreachable_link).is_err(),
            "a link rectangle under another element must not pass"
        );

        let mut no_prompt = report.clone();
        *no_prompt.pointer_mut("/link/prompt").unwrap() = Json::Null;
        assert!(
            verdict_extension(&no_prompt).is_err(),
            "a link that opened no prompt must not pass"
        );

        let mut foreign = report.clone();
        *foreign.pointer_mut("/extensionId").unwrap() = json!("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
        assert!(
            verdict_extension(&foreign).is_err(),
            "a page under another extension id is not the staged package"
        );

        let mut partial = report.clone();
        *partial.pointer_mut("/stage").unwrap() = json!("press");
        assert!(
            verdict_extension(&partial).is_err(),
            "a probe that stopped before the press must not pass"
        );

        let mut elsewhere = report;
        *elsewhere.pointer_mut("/origin").unwrap() = json!("https://elsewhere.example");
        assert!(
            verdict_extension(&elsewhere).is_err(),
            "the fixture must come from the harness origin, not anywhere"
        );
    }

    /// The falsifiability check: every claim the verdicts make, withdrawn one
    /// at a time, must turn a check red. A verdict that cannot fail is not a
    /// gate, and this repo has been bitten by one of those already.
    ///
    /// Every control keeps `"schema": SCHEMA`, so each one fails for the reason
    /// under test and not merely because the well-formedness check rejected it.
    #[test]
    fn the_verdicts_fail_when_a_claim_is_withdrawn() {
        let unclaimed = json!({"schema": SCHEMA, "scope": "http://127.0.0.1:1/", "controlled": false, "asset": ASSET_BODY, "fatal": Json::Null});
        assert!(
            verdict_service_worker(&unclaimed).is_err(),
            "an unclaimed page must not pass"
        );

        let wrong_body = json!({"schema": SCHEMA, "scope": "http://127.0.0.1:1/", "controlled": true, "asset": "something else", "fatal": Json::Null});
        assert!(
            verdict_service_worker(&wrong_body).is_err(),
            "a body the worker did not precache must not pass"
        );

        let wrong_origin = json!({"schema": SCHEMA, "scope": "https://elsewhere.example/", "controlled": true, "asset": ASSET_BODY, "fatal": Json::Null});
        assert!(
            verdict_service_worker(&wrong_origin).is_err(),
            "a worker off-origin must not pass"
        );

        let not_run =
            json!({"schema": SCHEMA, "sum": 0, "linkError": "LinkError: x", "fatal": Json::Null});
        assert!(
            verdict_async_wasm(&not_run).is_err(),
            "a module that did not run must not pass"
        );

        let no_link_check =
            json!({"schema": SCHEMA, "sum": WASM_PROBE_SUM, "linkError": "", "fatal": Json::Null});
        assert!(
            verdict_async_wasm(&no_link_check).is_err(),
            "no LinkError must not pass"
        );

        // A page that threw has proved nothing, whatever else it set.
        let fatal = json!({"schema": SCHEMA, "controlled": true, "asset": ASSET_BODY, "fatal": "quota exceeded"});
        assert!(verdict_service_worker(&fatal).is_err());

        // And a report in a shape this harness does not speak is refused, not
        // read on a guess.
        let future =
            json!({"schema": 99, "controlled": true, "asset": ASSET_BODY, "fatal": Json::Null});
        assert!(verdict_service_worker(&future).is_err());
        let shapeless = json!({"controlled": true, "asset": ASSET_BODY, "fatal": Json::Null});
        assert!(verdict_service_worker(&shapeless).is_err());
    }

    #[test]
    fn the_committed_asset_is_the_body_the_verdict_expects() {
        // Otherwise the constant and the file drift apart and the check ends up
        // asserting a body the worker never cached.
        let asset = check_root().join("service-worker/asset.txt");
        let text = std::fs::read_to_string(&asset).expect("the committed asset must be readable");
        assert_eq!(text.trim(), ASSET_BODY);
    }

    /// The committed `probe.wasm` is what the browser runs, and it is a binary
    /// blob in the diff — so it is pinned twice over here.
    ///
    /// First it is checked against `probe.wat`, the readable source committed
    /// beside it: both are instantiated with the same import and must compute
    /// the same answer, so editing one without the other fails the build rather
    /// than quietly changing what the browser executes. Then the expected answer
    /// itself is re-derived, so WASM_PROBE_SUM cannot drift from the module
    /// either.
    ///
    /// All of this runs under wasmtime, before any browser is launched. The
    /// browser leg then proves a *different* engine agrees on the same bytes.
    #[test]
    fn the_committed_module_matches_its_readable_source() {
        let dir = check_root().join("async-wasm");
        let wasm = std::fs::read(dir.join("probe.wasm")).expect("the committed module");
        let wat = std::fs::read(dir.join("probe.wat")).expect("the committed source");

        assert_eq!(
            wasm.get(..4),
            Some(&[0x00, 0x61, 0x73, 0x6d][..]),
            "probe.wasm must be a bare WebAssembly module, not a host object file"
        );

        let from_wasm = i64::from(call_probe(&wasm));
        let from_wat = i64::from(call_probe(&wat));
        assert_eq!(
            from_wasm, from_wat,
            "probe.wasm and probe.wat disagree — update both together"
        );
        assert_eq!(from_wasm, WASM_PROBE_SUM);
    }

    #[test]
    fn the_origin_refuses_to_serve_outside_its_root() {
        let root = Path::new("/srv/pages");
        assert!(resolve(root, "/").is_some_and(|p| p.ends_with("index.html")));
        assert!(resolve(root, "/sw.js").is_some_and(|p| p.ends_with("sw.js")));
        // Query strings are not part of the path.
        assert!(resolve(root, "/sw.js?v=2").is_some_and(|p| p.ends_with("sw.js")));
        for target in [
            "/../secrets",
            "/a/../../secrets",
            "/a/..%2f..",
            "/C:/windows",
        ] {
            assert!(resolve(root, target).is_none(), "{target} must not resolve");
        }
    }

    #[test]
    fn content_length_comes_from_the_header() {
        assert_eq!(
            content_length("POST /x HTTP/1.1\r\nContent-Length: 12\r\n\r\n"),
            12
        );
        assert_eq!(
            content_length("POST /x HTTP/1.1\r\ncontent-length:  7 \r\n\r\n"),
            7
        );
        assert_eq!(content_length("GET /x HTTP/1.1\r\n\r\n"), 0);
    }

    #[test]
    fn wasm_is_served_as_wasm_so_the_streaming_api_can_be_used() {
        assert_eq!(content_type(Path::new("probe.wasm")), "application/wasm");
        assert_eq!(
            content_type(Path::new("index.html")),
            "text/html; charset=utf-8"
        );
        assert_eq!(
            content_type(Path::new("sw.js")),
            "text/javascript; charset=utf-8"
        );
    }

    /// The origin itself, with no browser in the loop: it must serve a file and
    /// it must take a verdict. Both halves matter — a page that never loads
    /// and a verdict that is never collected look identical from outside, and
    /// that ambiguity is what a harness like this must not have.
    #[test]
    fn the_origin_serves_files_and_collects_a_verdict() {
        let dir = std::env::temp_dir().join("selis-browser-origin-test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("scratch");
        std::fs::write(dir.join("index.html"), b"<p>hello</p>").expect("write");

        let (server, results) = serve(&dir).expect("bind");
        let port = server.port;

        let mut get = TcpStream::connect(("127.0.0.1", port)).expect("connect");
        // The origin closes after every response, so this read ends at the
        // FIN without any special request on the way in.
        get.write_all(b"GET / HTTP/1.1\r\nHost: x\r\n\r\n")
            .expect("write");
        let mut body = String::new();
        get.read_to_string(&mut body).expect("read");
        assert!(body.starts_with("HTTP/1.1 200 OK"), "got: {body}");
        assert!(body.contains("hello"), "got: {body}");
        assert!(body.contains("Content-Type: text/html"), "got: {body}");
        assert!(body.contains("Connection: close"), "got: {body}");

        let verdict = r#"{"schema":1,"sum":42}"#;
        let mut post = TcpStream::connect(("127.0.0.1", port)).expect("connect");
        let request = format!(
            "POST {RESULT_PATH} HTTP/1.1\r\nHost: x\r\nContent-Length: {}\r\n\r\n{verdict}",
            verdict.len()
        );
        post.write_all(request.as_bytes()).expect("write");
        let mut response = String::new();
        post.read_to_string(&mut response).expect("read");
        assert!(response.starts_with("HTTP/1.1 204"), "got: {response}");
        assert_eq!(
            results
                .recv_timeout(Duration::from_secs(5))
                .expect("a verdict"),
            verdict
        );

        drop(server);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The proof test: drive a **real** engine over the committed checks.
    ///
    /// Skips — loudly, on stdout — when the machine has no engine, so a
    /// developer without Edge still gets a green `cargo test`; `--strict` and
    /// the `browser-check` subcommand exist for the runners that must not skip.
    #[test]
    fn a_real_engine_runs_the_committed_checks() {
        if let Err(probed) = resolve_browser(None) {
            println!("SKIP a_real_engine_runs_the_committed_checks -- {probed}");
            return;
        }
        let dir = std::env::temp_dir().join("selis-browser-harness-selftest");
        let _ = std::fs::remove_dir_all(&dir);
        // The extension check's subject is the packaged `apps/extension/dist`,
        // which `test-fast` deliberately does not build: it has no `wasm-opt`
        // step, and the package gate refuses an unoptimised engine. The
        // `browser-check` job builds it and runs the fifth check with
        // `--strict`; here, a missing package runs the four page checks and
        // says so, rather than failing `cargo test` for a build it does not own.
        let extension_built = extension_dist_dir().exists();
        if !extension_built {
            println!(
                "SKIP extension -- {} does not exist; build the package to measure it",
                extension_dist_dir().display()
            );
        }
        let config = |only: Option<String>, out| Config {
            browser: None,
            page: None,
            only,
            // NOT `DEFAULT_TIMEOUT_MS`, and the difference is deliberate.
            //
            // The default is a product choice for an interactive run: fail fast
            // on a hung browser. This test runs a real Edge, a real WASM engine
            // build and the real origin, on every check, while 171 sibling
            // tests compete for the same cores - so it legitimately takes
            // longer than an interactive run would, and it has twice failed
            // `app-shell` at 30s (`[unmeasured]: the page did not report within
            // 30000ms`) while passing standalone. A timeout that fires under
            // load and not at rest is measuring the machine, not the app.
            //
            // Raised only HERE, deliberately: the interactive default stays
            // tight, so a developer still gets a fast failure on a real hang.
            timeout_ms: 120_000,
            out,
            keep: false,
            strict: true,
        };
        if extension_built {
            let cfg = config(None, dir.join("report.json"));
            if let Err(e) = run(&cfg) {
                panic!("the real-engine harness failed: {e}");
            }
        } else {
            for id in ["service-worker", "async-wasm", "app-shell", "chrome"] {
                let cfg = config(Some(id.to_owned()), dir.join(format!("report-{id}.json")));
                if let Err(e) = run(&cfg) {
                    panic!("the real-engine harness failed for {id}: {e}");
                }
            }
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
