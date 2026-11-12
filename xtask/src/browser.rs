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

use serde_json::{json, Map, Value as Json};

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
/// Seven now. `service-worker` is the WEB.02 payload: real lifecycle event
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
/// checks the page's no-upload assertion. `editor` drives the real
/// `/edit-pdf/` workspace through a text change, an undo, a redo and a
/// download, and reads the produced bytes back.
/// `extension` (SL-4.UI.06) is the only one that loads an unpacked extension;
/// it needs the extension build and a fixed key so the launcher can name the
/// page it opens.
const CHECKS: [Check; 7] = [
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
    // The editor workspace (pdf-plan/39). It stages the same built app and
    // opens the real `/edit-pdf/` route: a document is opened, a line of its
    // text is changed, the change is undone and redone, and the download is
    // read back to check the bytes are a PDF carrying the change.
    Check {
        id: "editor",
        dir: "editor",
        verdict: verdict_editor,
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
    check_page_count_leg(
        leg,
        label,
        expected_name,
        expected_run_label,
        expected_pages,
        expected_pages,
        needles,
    )
}

/// One tiled-watermark leg: the ordinary stamp proof, plus the repetition the
/// tiling option is supposed to produce. A single centered line would satisfy
/// every other witness, so the occurrence count is the discriminating one.
fn check_tiled_watermark_leg(leg: &Json, label: &str) -> Result<(), String> {
    let marker = "(CONFIDENTIAL) Tj";
    check_stamp_leg(
        leg,
        label,
        "outlined-watermarked.pdf",
        "Add watermark",
        2,
        &[marker],
    )?;
    let outputs = leg
        .get("outputs")
        .and_then(Json::as_array)
        .ok_or_else(|| format!("the {label} run reported no output list"))?;
    let output = outputs
        .first()
        .ok_or_else(|| format!("the {label} run produced no output"))?;
    let counts = output
        .get("foundCounts")
        .and_then(Json::as_array)
        .ok_or_else(|| format!("the {label} output did not report marker counts"))?;
    let count = counts.first().and_then(Json::as_u64).unwrap_or(0);
    if count < 4 {
        return Err(format!(
            "the {label} output stamped {marker} {count} time(s); a tiled watermark must repeat"
        ));
    }
    Ok(())
}

/// The lossy Compress leg: the stamp shape (one PDF in, one out, page count
/// held) plus the two things only the lossy tier produces — the JPEG filter that
/// proves the images were re-encoded, and the `overTarget` flag the target
/// search sets when it cannot reach the requested size.
///
/// The flag is read from the output row's `data-over-target`, not searched for
/// in the bytes: the word "target" appears in no part of a valid PDF, so a
/// byte-needle could never witness it.
fn check_lossy_compress_leg(
    leg: &Json,
    label: &str,
    expect_over_target: bool,
) -> Result<(), String> {
    check_stamp_leg(
        leg,
        label,
        "scan-compressed.pdf",
        "Compress PDF",
        1,
        &["/DCTDecode"],
    )?;
    let output = leg
        .get("outputs")
        .and_then(Json::as_array)
        .and_then(|outputs| outputs.first())
        .ok_or_else(|| format!("the {label} run produced no output"))?;
    if output.get("overTarget").and_then(Json::as_bool) != Some(expect_over_target) {
        return Err(format!(
            "the {label} run reported overTarget={:?}, expected {expect_over_target}",
            output.get("overTarget")
        ));
    }
    Ok(())
}

/// The same proof for a leg that deliberately changes the page count (N-up).
fn check_page_count_leg(
    leg: &Json,
    label: &str,
    expected_name: &str,
    expected_run_label: &str,
    expected_in: u64,
    expected_out: u64,
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
    let expected_pages_row = format!("{expected_in} \u{2192} {expected_out}");
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
    if output.get("pages").and_then(Json::as_u64) != Some(expected_out) {
        return Err(format!(
            "the {label} output reported {:?} pages, expected {expected_out}",
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
            "the {label} PDF is only {bytes} bytes; a {expected_out}-page result should be larger"
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

/// The strings the metadata-strip leg requires to be ABSENT from the real
/// downloaded bytes. Removal is only proven by the negative: a page that
/// rewrote the file but carried `/Info` through would still pass a page-count
/// check.
const STRIP_METADATA_ABSENT: [&str; 4] =
    ["/Info", "Secret Title", "Secret Author", "Secret Producer"];

/// One metadata-removal leg. The shape is the page-preserving stamp shape, plus
/// the negative witness: none of the removed fields may appear in the Blob.
fn check_strip_metadata_leg(leg: &Json, label: &str) -> Result<(), String> {
    check_stamp_leg(
        leg,
        label,
        "metadata-metadata-removed.pdf",
        "Remove metadata",
        3,
        &[],
    )?;
    let outputs = leg
        .get("outputs")
        .and_then(Json::as_array)
        .ok_or_else(|| format!("the {label} run reported no output list"))?;
    let output = outputs
        .first()
        .ok_or_else(|| format!("the {label} run produced no output"))?;
    let absent = output
        .get("absentFound")
        .and_then(Json::as_array)
        .ok_or_else(|| format!("the {label} output did not report its absence checks"))?;
    if absent.len() != STRIP_METADATA_ABSENT.len() {
        return Err(format!(
            "the {label} output reported {} absence checks, expected {}",
            absent.len(),
            STRIP_METADATA_ABSENT.len()
        ));
    }
    if let Some(index) = absent
        .iter()
        .position(|value| value.as_bool() != Some(false))
    {
        return Err(format!(
            "the {label} output still contains {:?}; metadata removal did not reach the bytes",
            STRIP_METADATA_ABSENT[index]
        ));
    }
    Ok(())
}

/// The strings the metadata-report leg requires to be PRESENT in the real
/// downloaded JSON: the status the engine chose, the version and page count it
/// read, one `/Info` field, and the XMP packet's absence. A card that invented
/// its rows from the file name cannot produce any of them.
const METADATA_REPORT_NEEDLES: [&str; 6] = [
    "\"status\": \"ok\"",
    "\"pdf_version\": \"1.4\"",
    "\"pages\": 3",
    "\"key\": \"Title\"",
    "\"value\": \"Secret Title\"",
    "\"present\": false",
];
/// The strings the metadata report must NOT carry: it exports what a document
/// says about itself, never the document.
const METADATA_REPORT_ABSENT: [&str; 1] = ["%PDF-"];

/// One metadata-report leg: one PDF in, one JSON report out, and no PDF at all.
/// The operation is read-only, so the leg fails if the page offers a preview,
/// if the export carries document bytes, or if the rows it shows are not the
/// rows the downloaded file holds.
fn check_metadata_report_leg(leg: &Json, label: &str) -> Result<(), String> {
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
    if string_field(leg, "runLabel")? != "Read metadata" {
        return Err(format!(
            "the {label} button was labelled {:?}, expected \"Read metadata\"",
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
    if result.get("title").and_then(Json::as_str) != Some("notes-metadata.json") {
        return Err(format!(
            "the {label} result was named {:?}, expected \"notes-metadata.json\"",
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
    // The report rows come from the downloaded JSON, so these are the card
    // repeating what the file actually holds.
    for (label_row, wanted) in [
        ("PDF version", "1.4"),
        ("First page", "200 \u{d7} 200 pt"),
        ("Title", "Secret Title"),
        ("XMP packet", "none"),
    ] {
        if row_value(label_row) != wanted {
            return Err(format!(
                "the {label} card reported {label_row:?} as {:?}, expected {wanted:?}",
                row_value(label_row)
            ));
        }
    }
    // Read-only verification: the document is untouched, so the counts pair and
    // the verdict say so rather than claiming a rewrite.
    if row_value("Pages") != "3 \u{2192} 3" {
        return Err(format!(
            "the {label} run reported Pages {:?}, expected \"3 \u{2192} 3\"",
            row_value("Pages")
        ));
    }
    if row_value("Verification") != "read" {
        return Err(format!(
            "the {label} run reported Verification {:?}, expected the read-only verdict \"read\"",
            row_value("Verification")
        ));
    }

    let outputs = leg
        .get("outputs")
        .and_then(Json::as_array)
        .ok_or_else(|| format!("the {label} run reported no output list"))?;
    if outputs.len() != 1 {
        return Err(format!(
            "the {label} run produced {} outputs, expected one report",
            outputs.len()
        ));
    }
    let output = outputs
        .first()
        .ok_or_else(|| format!("the {label} run produced no output"))?;
    if output.get("kind").and_then(Json::as_str) != Some("report") {
        return Err(format!(
            "the {label} output had kind {:?}, expected \"report\"",
            output.get("kind")
        ));
    }
    if output.get("name").and_then(Json::as_str) != Some("notes-metadata.json") {
        return Err(format!(
            "the {label} output was named {:?}, expected \"notes-metadata.json\"",
            output.get("name")
        ));
    }
    if output.get("pages").and_then(Json::as_u64) != Some(0) {
        return Err(format!(
            "the {label} output reported pages, but a report is not a PDF"
        ));
    }
    if output.get("blob").and_then(Json::as_bool) != Some(true) {
        return Err(format!("the {label} output was not a Blob URL"));
    }
    if output.get("previewBlob").and_then(Json::as_bool).is_some() {
        return Err(format!(
            "the {label} output offered a PDF preview; no PDF was made"
        ));
    }
    if output.get("downloadName").and_then(Json::as_str) != Some("notes-metadata.json") {
        return Err(format!(
            "the {label} download was named {:?}",
            output.get("downloadName")
        ));
    }
    if leg.get("output") != Some(&Json::Null) {
        return Err(format!(
            "the {label} leg presented a PDF to preview ({:?}); a metadata report is not a document",
            leg.get("output")
        ));
    }
    let found = output
        .get("found")
        .and_then(Json::as_array)
        .ok_or_else(|| format!("the {label} output did not report its content checks"))?;
    if found.len() != METADATA_REPORT_NEEDLES.len() {
        return Err(format!(
            "the {label} output reported {} content checks, expected {}",
            found.len(),
            METADATA_REPORT_NEEDLES.len()
        ));
    }
    if let Some(index) = found.iter().position(|item| item.as_bool() != Some(true)) {
        return Err(format!(
            "the {label} output is missing {:?}; the report did not carry the metadata it read",
            METADATA_REPORT_NEEDLES[index]
        ));
    }
    let absent = output
        .get("absentFound")
        .and_then(Json::as_array)
        .ok_or_else(|| format!("the {label} output did not report its absence checks"))?;
    if absent.len() != METADATA_REPORT_ABSENT.len() {
        return Err(format!(
            "the {label} output reported {} absence checks, expected {}",
            absent.len(),
            METADATA_REPORT_ABSENT.len()
        ));
    }
    if let Some(index) = absent
        .iter()
        .position(|value| value.as_bool() != Some(false))
    {
        return Err(format!(
            "the {label} output contains {:?}; a metadata report must hold only metadata",
            METADATA_REPORT_ABSENT[index]
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

/// The strings the image→PDF leg requires to be PRESENT in the real downloaded
/// bytes. A page that merely counted its inputs would produce a PDF without
/// these, so the needles prove both containers were decoded and the pixels were
/// embedded as Flate-compressed DeviceRGB image XObjects.
const IMAGE_NEEDLES: [&str; 3] = [
    "/Subtype /Image",
    "/Filter /FlateDecode",
    "/ColorSpace /DeviceRGB",
];
/// The strings the convert leg requires to be PRESENT in the real downloaded
/// bytes: the heading font and the heading text. A page that never rendered
/// Markdown has neither.
const CONVERT_NEEDLES: [&str; 2] = ["/BaseFont /Helvetica-Bold", "Parity Report"];
/// The online LaTeX leg: the source's own section title, rendered bold.
const LATEX_NEEDLES: [&str; 1] = ["Quarto"];
/// The online CSV leg: two cells from the first data row.
const CSV_NEEDLES: [&str; 2] = ["Tea", "Coffee"];
/// The online SVG leg: the text element's own words.
const SVG_NEEDLES: [&str; 1] = ["Ledgers"];
/// The online EPUB leg: the one chapter's heading.
const EPUB_NEEDLES: [&str; 1] = ["Omnibus"];
/// The online pipe-table leg: the caption and a cell, so the table had to
/// survive parsing, laying out and the page gate.
const TABLE_NEEDLES: [&str; 2] = ["Ledger", "Tea"];
const DOCX_NEEDLES: [&str; 3] = ["Quarterly plan", "First item", "Quarter"];
const PPTX_NEEDLES: [&str; 3] = ["Zebra slide", "Quarter", "Amount"];
const XLSX_NEEDLES: [&str; 3] = ["Quarter", "Amount", "cached"];

/// The strings the image-watermark leg requires to be PRESENT in the real
/// downloaded bytes: the embedded RGB XObject and at least one placement that
/// names it. Counting files cannot produce either.
const IMAGE_WATERMARK_NEEDLES: [&str; 3] = [
    "/Subtype /Image",
    "/ColorSpace /DeviceRGB",
    "SelisOverlayImage Do",
];

/// The strings the outline leg requires to be PRESENT in the real downloaded
/// bytes: the outline root and both rewritten titles. A page that never wrote
/// a tree cannot produce them.
const OUTLINE_NEEDLES: [&str; 3] = ["/Type /Outlines", "(Chapter 1)", "(Section)"];

/// One convert leg: a Markdown file becomes a real PDF through the lazy
/// convert chunk, and the downloaded bytes carry the rendered heading.
///
/// This is the only browser proof that the convert chunk is served, links with
/// no imports, and is wired to the card. The chunk is runtime-cached rather
/// than precached, so the offline leg additionally proves the second run comes
/// out of Cache Storage with the origin cut.
/// The Phase 3 text leg: one document in, its own text plus a quality report
/// out, and the text really carries the words the source drew.
///
/// A page separator alone would pass on any PDF, including one that draws no
/// glyphs, so the leg pins the character count and the "pages with text" row as
/// well. The output the browser hands back is the same download the user gets.
fn check_text_leg(leg: &Json, label: &str) -> Result<(), String> {
    if leg.get("fileRows").and_then(Json::as_u64) != Some(1) {
        return Err(format!(
            "the {label} leg did not show one selected file (fileRows = {:?})",
            leg.get("fileRows")
        ));
    }
    if string_field(leg, "noUpload")? != "verified" {
        return Err(format!(
            "the {label} leg never set data-no-upload=verified; the local request check did not pass"
        ));
    }
    if string_field(leg, "runLabel")? != "Extract text" {
        return Err(format!(
            "the {label} button was labelled {:?}, expected \"Extract text\"",
            leg.get("runLabel")
        ));
    }
    let status = string_field(leg, "status")?;
    if !status.contains("on this device") {
        return Err(format!(
            "the {label} status did not say the export was read on the device (status = {status:?})"
        ));
    }

    let result = leg
        .get("result")
        .and_then(Json::as_object)
        .ok_or_else(|| format!("the {label} leg reported no result object"))?;
    let rows = result
        .get("rows")
        .and_then(Json::as_array)
        .ok_or_else(|| format!("the {label} result had no report rows"))?;
    let row_value = |wanted: &str| {
        rows.iter()
            .find(|row| row.get("label").and_then(Json::as_str) == Some(wanted))
            .and_then(|row| row.get("value"))
            .and_then(Json::as_str)
            .unwrap_or("")
    };
    if row_value("Verdict") != "ok" {
        return Err(format!(
            "the {label} run reported Verdict {:?}, expected \"ok\" (the text was empty or the export failed)",
            row_value("Verdict")
        ));
    }
    if row_value("Pages with text") != "1" {
        return Err(format!(
            "the {label} run reported Pages with text {:?}, expected \"1\"",
            row_value("Pages with text")
        ));
    }
    let characters: u64 = row_value("Characters").parse().unwrap_or(0);
    if characters == 0 {
        return Err(format!(
            "the {label} run recovered no characters (Characters = {:?})",
            row_value("Characters")
        ));
    }
    if row_value("Verification") != "read" {
        return Err(format!(
            "the {label} run reported Verification {:?}, expected \"read\"",
            row_value("Verification")
        ));
    }

    let outputs = leg
        .get("outputs")
        .and_then(Json::as_array)
        .ok_or_else(|| format!("the {label} run reported no output list"))?;
    if outputs.len() != 2 {
        return Err(format!(
            "the {label} run produced {} outputs, expected the text and its quality report",
            outputs.len()
        ));
    }
    let text = outputs
        .first()
        .ok_or_else(|| format!("the {label} run produced no output"))?;
    if text.get("kind").and_then(Json::as_str) != Some("text") {
        return Err(format!(
            "the {label} output had kind {:?}, expected \"text\"",
            text.get("kind")
        ));
    }
    let name = text
        .get("name")
        .and_then(Json::as_str)
        .ok_or_else(|| format!("the {label} text output had no name"))?;
    if !name.ends_with(".txt") {
        return Err(format!(
            "the {label} text output was named {name:?}, expected a .txt file"
        ));
    }
    if text.get("blob").and_then(Json::as_bool) != Some(true) {
        return Err(format!(
            "the {label} text output did not use a Blob URL for its download"
        ));
    }
    if text.get("bytes").and_then(Json::as_u64).unwrap_or(0) == 0 {
        return Err(format!(
            "the {label} download was empty (bytes = {:?})",
            text.get("bytes")
        ));
    }
    // The needles are the separator and a word from the source document, both
    // searched for inside the downloaded bytes rather than in the DOM.
    let found = text
        .get("found")
        .and_then(Json::as_array)
        .ok_or_else(|| format!("the {label} download was never searched for the expected words"))?;
    if found.is_empty() || !found.iter().all(|hit| hit.as_bool() == Some(true)) {
        return Err(format!(
            "the {label} downloaded text did not contain the separator and the source's own words (found = {found:?})"
        ));
    }
    let quality = outputs
        .get(1)
        .ok_or_else(|| format!("the {label} run produced no quality report"))?;
    if quality.get("kind").and_then(Json::as_str) != Some("report") {
        return Err(format!(
            "the {label} quality report had kind {:?}, expected \"report\"",
            quality.get("kind")
        ));
    }
    let quality_name = quality
        .get("name")
        .and_then(Json::as_str)
        .ok_or_else(|| format!("the {label} quality report had no name"))?;
    if !quality_name.ends_with("-quality.json") {
        return Err(format!(
            "the {label} quality report was named {quality_name:?}, expected a -quality.json file"
        ));
    }
    Ok(())
}

/// The Phase 3 document legs (Word, PowerPoint, Excel). They take the same
/// shape: one store-method package, handed over as a Blob, with no upload and
/// no network beyond the runtime-cached text chunk. The needles are the
/// package's own parts (which a store-method ZIP keeps in plain sight) and
/// the source's own words, so a leg cannot pass by downloading an empty
/// container.
fn check_ooxml_leg(leg: &Json, label: &str, extension: &str) -> Result<(), String> {
    if leg.get("fileRows").and_then(Json::as_u64) != Some(1) {
        return Err(format!(
            "the {label} leg did not show one selected file (fileRows = {:?})",
            leg.get("fileRows")
        ));
    }
    if string_field(leg, "noUpload")? != "verified" {
        return Err(format!(
            "the {label} leg never set data-no-upload=verified; the local request check did not pass"
        ));
    }
    if string_field(leg, "runLabel")? != "Extract text" {
        return Err(format!(
            "the {label} button was labelled {:?}, expected \"Extract text\"",
            leg.get("runLabel")
        ));
    }
    let status = string_field(leg, "status")?;
    if !status.contains("on this device") {
        return Err(format!(
            "the {label} status did not say the export was read on the device (status = {status:?})"
        ));
    }

    let outputs = leg
        .get("outputs")
        .and_then(Json::as_array)
        .ok_or_else(|| format!("the {label} run reported no output list"))?;
    if outputs.len() != 1 {
        return Err(format!(
            "the {label} run produced {} outputs, expected one package and nothing else",
            outputs.len()
        ));
    }
    let output = outputs
        .first()
        .ok_or_else(|| format!("the {label} run produced no output"))?;
    if output.get("kind").and_then(Json::as_str) != Some("file") {
        return Err(format!(
            "the {label} output had kind {:?}, expected \"file\"",
            output.get("kind")
        ));
    }
    let name = output
        .get("name")
        .and_then(Json::as_str)
        .ok_or_else(|| format!("the {label} output had no name"))?;
    if !name.ends_with(extension) {
        return Err(format!(
            "the {label} output was named {name:?}, expected a {extension} file"
        ));
    }
    if output.get("zip").and_then(Json::as_bool) != Some(true) {
        return Err(format!(
            "the {label} output was not a ZIP container it could be opened with"
        ));
    }
    if output.get("blob").and_then(Json::as_bool) != Some(true) {
        return Err(format!(
            "the {label} output did not use a Blob URL for its download"
        ));
    }
    if output.get("bytes").and_then(Json::as_u64).unwrap_or(0) == 0 {
        return Err(format!(
            "the {label} download was empty (bytes = {:?})",
            output.get("bytes")
        ));
    }
    let found = output
        .get("found")
        .and_then(Json::as_array)
        .ok_or_else(|| format!("the {label} download was never searched for its parts"))?;
    if found.is_empty() || !found.iter().all(|hit| hit.as_bool() == Some(true)) {
        return Err(format!(
            "the {label} package did not contain the parts it was written from (found = {found:?})"
        ));
    }
    let network = leg
        .get("network")
        .ok_or_else(|| format!("the {label} leg reported no network ledger"))?;
    let paths = network
        .get("paths")
        .and_then(Json::as_array)
        .ok_or_else(|| format!("the {label} leg reported no network paths"))?;
    if !paths
        .iter()
        .any(|path| path.as_str() == Some("/wasm/selis_pdf_wasm_text.wasm"))
    {
        return Err(format!(
            "the {label} leg did not load the text chunk (paths = {paths:?})"
        ));
    }
    if let Some(foreign) = network.get("foreign").and_then(Json::as_array) {
        if !foreign.is_empty() {
            return Err(format!(
                "the {label} leg touched a foreign origin (foreign = {foreign:?})"
            ));
        }
    }
    Ok(())
}

/// The shared shape of every text-card leg: one selected file, the run
/// button labelled for what it writes, a result that names this device, and
/// a network ledger that only ever names the runtime-cached chunks.
fn text_card_run(leg: &Json, label: &str) -> Result<(), String> {
    if leg.get("fileRows").and_then(Json::as_u64) != Some(1) {
        return Err(format!(
            "the {label} leg did not show one selected file (fileRows = {:?})",
            leg.get("fileRows")
        ));
    }
    if string_field(leg, "noUpload")? != "verified" {
        return Err(format!(
            "the {label} leg never set data-no-upload=verified; the local request check did not pass"
        ));
    }
    if string_field(leg, "runLabel")? != "Extract text" {
        return Err(format!(
            "the {label} button was labelled {:?}, expected \"Extract text\"",
            leg.get("runLabel")
        ));
    }
    let status = string_field(leg, "status")?;
    if !status.contains("on this device") {
        return Err(format!(
            "the {label} status did not say the export was read on the device (status = {status:?})"
        ));
    }
    let network = leg
        .get("network")
        .ok_or_else(|| format!("the {label} leg reported no network ledger"))?;
    let paths = network
        .get("paths")
        .and_then(Json::as_array)
        .ok_or_else(|| format!("the {label} leg reported no network paths"))?;
    if !paths
        .iter()
        .any(|path| path.as_str() == Some("/wasm/selis_pdf_wasm_text.wasm"))
    {
        return Err(format!(
            "the {label} leg did not load the text chunk (paths = {paths:?})"
        ));
    }
    if let Some(foreign) = network.get("foreign").and_then(Json::as_array) {
        if !foreign.is_empty() {
            return Err(format!(
                "the {label} leg touched a foreign origin (foreign = {foreign:?})"
            ));
        }
    }
    Ok(())
}

/// The outputs of one text-card leg, in order.
fn text_card_outputs(leg: &Json, label: &str) -> Result<Vec<Json>, String> {
    let outputs = leg
        .get("outputs")
        .and_then(Json::as_array)
        .ok_or_else(|| format!("the {label} run reported no output list"))?;
    Ok(outputs.clone())
}

/// The CSVs the table leg must contain, the kind the download must carry,
/// and the report the page must show for the table count.
fn check_tables_leg(leg: &Json, label: &str) -> Result<(), String> {
    text_card_run(leg, label)?;
    let outputs = text_card_outputs(leg, label)?;
    if outputs.len() != 2 {
        return Err(format!(
            "the {label} run produced {} outputs, expected the CSV and its report",
            outputs.len()
        ));
    }
    let csv = &outputs[0];
    if csv.get("kind").and_then(Json::as_str) != Some("tables") {
        return Err(format!(
            "the {label} CSV had kind {:?}, expected \"tables\"",
            csv.get("kind")
        ));
    }
    let name = csv
        .get("name")
        .and_then(Json::as_str)
        .ok_or_else(|| format!("the {label} CSV output had no name"))?;
    if !name.ends_with("-tables.csv") {
        return Err(format!(
            "the {label} CSV output was named {name:?}, expected a -tables.csv file"
        ));
    }
    if csv.get("blob").and_then(Json::as_bool) != Some(true)
        || csv.get("bytes").and_then(Json::as_u64).unwrap_or(0) == 0
    {
        return Err(format!("the {label} CSV download was empty or not a Blob"));
    }
    let found = csv
        .get("found")
        .and_then(Json::as_array)
        .ok_or_else(|| format!("the {label} download was never searched for its cells"))?;
    if found.is_empty() || !found.iter().all(|hit| hit.as_bool() == Some(true)) {
        return Err(format!(
            "the {label} CSV did not contain the cell text from the table (found = {found:?})"
        ));
    }
    let report_out = &outputs[1];
    if report_out.get("kind").and_then(Json::as_str) != Some("report") {
        return Err(format!(
            "the {label} report output had kind {:?}, expected \"report\"",
            report_out.get("kind")
        ));
    }
    let report_name = report_out
        .get("name")
        .and_then(Json::as_str)
        .ok_or_else(|| format!("the {label} report output had no name"))?;
    if !report_name.ends_with("-tables.json") {
        return Err(format!(
            "the {label} report output was named {report_name:?}, expected a -tables.json file"
        ));
    }
    let result = leg
        .get("result")
        .and_then(Json::as_object)
        .ok_or_else(|| format!("the {label} leg reported no result object"))?;
    let rows = result
        .get("rows")
        .and_then(Json::as_array)
        .ok_or_else(|| format!("the {label} result had no report rows"))?;
    let row_value = |wanted: &str| {
        rows.iter()
            .find(|row| row.get("label").and_then(Json::as_str) == Some(wanted))
            .and_then(|row| row.get("value"))
            .and_then(Json::as_str)
            .unwrap_or("")
    };
    if row_value("Tables found") != "1" {
        return Err(format!(
            "the {label} run found {:?} tables, expected \"1\"",
            row_value("Tables found")
        ));
    }
    if row_value("Verification") != "read" {
        return Err(format!(
            "the {label} run did not report a read verification (got {:?})",
            row_value("Verification")
        ));
    }
    Ok(())
}

/// The split bundle: a ZIP of parts, plus the report that names the rule the
/// split used before anything was written.
fn check_text_split_leg(leg: &Json, label: &str) -> Result<(), String> {
    text_card_run(leg, label)?;
    let outputs = text_card_outputs(leg, label)?;
    if outputs.len() != 2 {
        return Err(format!(
            "the {label} run produced {} outputs, expected the ZIP and its report",
            outputs.len()
        ));
    }
    let bundle = &outputs[0];
    if bundle.get("kind").and_then(Json::as_str) != Some("zip") {
        return Err(format!(
            "the {label} bundle had kind {:?}, expected \"zip\"",
            bundle.get("kind")
        ));
    }
    let name = bundle
        .get("name")
        .and_then(Json::as_str)
        .ok_or_else(|| format!("the {label} bundle output had no name"))?;
    if !name.ends_with("-split.zip") {
        return Err(format!(
            "the {label} bundle was named {name:?}, expected a -split.zip file"
        ));
    }
    if bundle.get("zip").and_then(Json::as_bool) != Some(true)
        || bundle.get("bytes").and_then(Json::as_u64).unwrap_or(0) == 0
    {
        return Err(format!(
            "the {label} split bundle was not a real ZIP it could be opened with"
        ));
    }
    let found = bundle
        .get("found")
        .and_then(Json::as_array)
        .ok_or_else(|| format!("the {label} download was never searched for the part names"))?;
    if found.is_empty() || !found.iter().all(|hit| hit.as_bool() == Some(true)) {
        return Err(format!(
            "the {label} bundle did not contain both parts (found = {found:?})"
        ));
    }
    let report_out = &outputs[1];
    if report_out.get("kind").and_then(Json::as_str) != Some("report") {
        return Err(format!(
            "the {label} report output had kind {:?}, expected \"report\"",
            report_out.get("kind")
        ));
    }
    let report_name = report_out
        .get("name")
        .and_then(Json::as_str)
        .ok_or_else(|| format!("the {label} report output had no name"))?;
    if !report_name.ends_with("-split.json") {
        return Err(format!(
            "the {label} report output was named {report_name:?}, expected a -split.json file"
        ));
    }
    let result = leg
        .get("result")
        .and_then(Json::as_object)
        .ok_or_else(|| format!("the {label} leg reported no result object"))?;
    let rows = result
        .get("rows")
        .and_then(Json::as_array)
        .ok_or_else(|| format!("the {label} result had no report rows"))?;
    let row_value = |wanted: &str| {
        rows.iter()
            .find(|row| row.get("label").and_then(Json::as_str) == Some(wanted))
            .and_then(|row| row.get("value"))
            .and_then(Json::as_str)
            .unwrap_or("")
    };
    if row_value("Parts") != "2" {
        return Err(format!(
            "the {label} split made {:?} parts, expected \"2\"",
            row_value("Parts")
        ));
    }
    if row_value("Matched pages") != "2" {
        return Err(format!(
            "the {label} split matched {:?} pages, expected page 2",
            row_value("Matched pages")
        ));
    }
    Ok(())
}

/// The reflow article: one HTML document, carrying the article it was
/// reflowed into and the paragraphs from the source's own words.
fn check_reflow_leg(leg: &Json, label: &str) -> Result<(), String> {
    text_card_run(leg, label)?;
    let outputs = text_card_outputs(leg, label)?;
    if outputs.len() != 1 {
        return Err(format!(
            "the {label} run produced {} outputs, expected one HTML document",
            outputs.len()
        ));
    }
    let output = &outputs[0];
    if output.get("kind").and_then(Json::as_str) != Some("html") {
        return Err(format!(
            "the {label} output had kind {:?}, expected \"html\"",
            output.get("kind")
        ));
    }
    let name = output
        .get("name")
        .and_then(Json::as_str)
        .ok_or_else(|| format!("the {label} output had no name"))?;
    if !name.ends_with(".html") {
        return Err(format!(
            "the {label} output was named {name:?}, expected a .html file"
        ));
    }
    if output.get("blob").and_then(Json::as_bool) != Some(true)
        || output.get("bytes").and_then(Json::as_u64).unwrap_or(0) == 0
    {
        return Err(format!("the {label} HTML download was empty or not a Blob"));
    }
    let found = output
        .get("found")
        .and_then(Json::as_array)
        .ok_or_else(|| format!("the {label} download was never searched for its words"))?;
    if found.is_empty() || !found.iter().all(|hit| hit.as_bool() == Some(true)) {
        return Err(format!(
            "the {label} article did not contain its own structure and the source's words (found = {found:?})"
        ));
    }
    Ok(())
}

fn check_convert_leg(
    leg: &Json,
    label: &str,
    expected_title: &str,
    needles: &[&str],
    expected_pages: u32,
) -> Result<(), String> {
    if leg.get("fileRows").and_then(Json::as_u64) != Some(1) {
        return Err(format!(
            "the {label} leg did not show one selected file (fileRows = {:?})",
            leg.get("fileRows")
        ));
    }
    if string_field(leg, "noUpload")? != "verified" {
        return Err(format!(
            "the {label} leg never set data-no-upload=verified; the local request check did not pass"
        ));
    }
    if string_field(leg, "runLabel")? != "Convert to PDF" {
        return Err(format!(
            "the {label} button was labelled {:?}, expected \"Convert to PDF\"",
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
    if result.get("title").and_then(Json::as_str) != Some(expected_title) {
        return Err(format!(
            "the {label} result was named {:?}, expected {expected_title:?}",
            result.get("title")
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
    // A generated document has no input page count, so the row is the output
    // count alone.
    if row_value("Pages") != expected_pages.to_string() {
        return Err(format!(
            "the {label} run reported Pages {:?}, expected {expected_pages}",
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
    let output = outputs
        .first()
        .ok_or_else(|| format!("the {label} run produced no output"))?;
    if output.get("kind").and_then(Json::as_str) != Some("pdf") {
        return Err(format!(
            "the {label} output had kind {:?}, expected \"pdf\"",
            output.get("kind")
        ));
    }
    if output.get("name").and_then(Json::as_str) != Some(expected_title) {
        return Err(format!(
            "the {label} output was named {:?}, expected {expected_title:?}",
            output.get("name")
        ));
    }
    if output.get("pages").and_then(Json::as_u64) != Some(u64::from(expected_pages)) {
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
    if output.get("downloadName").and_then(Json::as_str) != Some(expected_title) {
        return Err(format!(
            "the {label} download was named {:?}, expected {expected_title:?}",
            output.get("downloadName")
        ));
    }
    let found = output
        .get("found")
        .and_then(Json::as_array)
        .ok_or_else(|| format!("the {label} output did not report its content checks"))?;
    if found.len() != needles.len() {
        return Err(format!(
            "the {label} output reported {} content checks, expected {}",
            found.len(),
            needles.len()
        ));
    }
    if let Some(index) = found.iter().position(|item| item.as_bool() != Some(true)) {
        return Err(format!(
            "the {label} output is missing {:?}; the source was not rendered",
            needles[index]
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
/// The strings the PDF→PNG leg requires to be PRESENT in the real downloaded
/// bytes: the IHDR and IDAT chunk types every non-empty PNG carries. A page
/// that returned an empty Blob or a PDF would have neither.
const PNG_NEEDLES: [&str; 2] = ["IHDR", "IDAT"];
/// The ZIP entry names a three-page PNG bundle must contain, in order. The
/// raster chunk pads to the digit width of the page count, so a document of
/// three pages yields `page-1.png` … and one of twelve yields `page-01.png`.
const PNG_ZIP_NEEDLES: [&str; 3] = ["page-1.png", "page-2.png", "page-3.png"];
/// The strings a PDF→JPG leg requires in the downloaded bytes: the JFIF
/// identifier the writer puts in its APP0 segment. A PDF or an empty Blob
/// carries neither.
const JPG_NEEDLES: [&str; 1] = ["JFIF"];
/// The entry names a three-page JPEG bundle must contain, in order.
const JPG_ZIP_NEEDLES: [&str; 3] = ["page-1.jpg", "page-2.jpg", "page-3.jpg"];
/// The strings a PDF→WebP leg requires in the downloaded bytes: the fourcc of
/// the lossless chunk this writer emits. A PDF, a ZIP or a lossy `VP8 ` file
/// carries neither.
const WEBP_NEEDLES: [&str; 1] = ["VP8L"];
/// The entry names a three-page WebP bundle must contain, in order.
const WEBP_ZIP_NEEDLES: [&str; 3] = ["page-1.webp", "page-2.webp", "page-3.webp"];

/// What one raster leg promises, so the PNG, JPG, TIFF and WebP cards are held
/// to the same rules with their own names. `signature`, `width_key` and
/// `height_key` are the sniff fields the harness fills in from the downloaded
/// bytes; `baseline_key` is set only for JPEG, whose start-of-frame marker says
/// which coding the writer actually used — SOF0 is baseline sequential, SOF2
/// would be a progressive frame, and the chunk promises the former.
/// `profile_key` is the format's equivalent claim: the TIFF's walk of its IFD
/// chain reports whether every page is the RGB Deflate strip this writer makes,
/// and the WebP's read of its VP8L chunk header reports whether the stream is
/// the lossless coding rather than a lossy frame renamed. `profile_claim` names
/// that profile in the failure message, because "the profile" means something
/// different to each of them.
/// `pages` is how many the downloaded
/// file is expected to hold, and `count_key` names the sniff field that says so —
/// only a container reports a count, because the per-page formats put their extra
/// pages in a ZIP whose entry names are already the needles. `preview` records
/// what the card may promise: Chromium displays PNG, JPEG and WebP inline and
/// does not display TIFF, so the TIFF leg fails if a preview link appears.
struct RasterSpec {
    verb: &'static str,
    format: &'static str,
    extension: &'static str,
    signature: &'static str,
    width_key: &'static str,
    height_key: &'static str,
    baseline_key: Option<&'static str>,
    profile_key: Option<&'static str>,
    profile_claim: &'static str,
    pages: u32,
    count_key: Option<&'static str>,
    preview: bool,
    needles: &'static [&'static str],
    zip_needles: &'static [&'static str],
}

const PNG_LEG: RasterSpec = RasterSpec {
    verb: "Render PNGs",
    format: "PNG",
    extension: "png",
    signature: "png",
    width_key: "pngWidth",
    height_key: "pngHeight",
    baseline_key: None,
    profile_key: None,
    profile_claim: "",
    pages: 1,
    count_key: None,
    preview: true,
    needles: &PNG_NEEDLES,
    zip_needles: &PNG_ZIP_NEEDLES,
};

const JPG_LEG: RasterSpec = RasterSpec {
    verb: "Render JPGs",
    format: "JPEG",
    extension: "jpg",
    signature: "jpeg",
    width_key: "jpegWidth",
    height_key: "jpegHeight",
    baseline_key: Some("jpegBaseline"),
    profile_key: None,
    profile_claim: "",
    pages: 1,
    count_key: None,
    preview: true,
    needles: &JPG_NEEDLES,
    zip_needles: &JPG_ZIP_NEEDLES,
};

/// One page inside one TIFF. The needles are empty because a TIFF carries no
/// text marker to search for — `IHDR`, `IDAT` and `JFIF` are format signatures
/// the container does not have — so the leg's proof is the IFD walk instead.
const TIFF_LEG: RasterSpec = RasterSpec {
    verb: "Render TIFF",
    format: "TIFF",
    extension: "tif",
    signature: "tiff",
    width_key: "tiffWidth",
    height_key: "tiffHeight",
    baseline_key: None,
    profile_key: Some("tiffProfile"),
    profile_claim: "RGB Deflate strip",
    pages: 1,
    count_key: Some("tiffPages"),
    preview: false,
    needles: &[],
    zip_needles: &[],
};

/// Three pages of the same document in the same container. This is the leg the
/// ZIP-bundle legs stand in for on the per-page formats.
const TIFF_THREE_LEG: RasterSpec = RasterSpec {
    pages: 3,
    ..TIFF_LEG
};

/// One page as a lossless WebP. The sniff reads the VP8L chunk header — the
/// 0x2F signature, the two 14-bit dimensions and the reserved version bits —
/// so `webpLossless` is the format's own claim that the bytes are this writer's
/// coding. The card previews: unlike a TIFF, a WebP is something Chromium draws.
const WEBP_LEG: RasterSpec = RasterSpec {
    verb: "Render WebPs",
    format: "WebP",
    extension: "webp",
    signature: "webp",
    width_key: "webpWidth",
    height_key: "webpHeight",
    baseline_key: None,
    profile_key: Some("webpLossless"),
    profile_claim: "VP8L lossless",
    pages: 1,
    count_key: None,
    preview: true,
    needles: &WEBP_NEEDLES,
    zip_needles: &WEBP_ZIP_NEEDLES,
};

/// The request-check tail every lazy-chunk leg shares: the page must have
/// asserted no upload and fetched nothing from another origin.
fn check_no_upload_network(leg: &Json, label: &str) -> Result<(), String> {
    if string_field(leg, "noUpload")? != "verified" {
        return Err(format!(
            "the {label} leg never set data-no-upload=verified; the local request check did not pass"
        ));
    }
    let status = string_field(leg, "status")?;
    if !status.contains("verified") {
        return Err(format!(
            "the {label} status did not say the result was verified (status = {status:?})"
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

/// One single-page raster leg: the lazy raster chunk renders the page and the
/// real Blob is the image the card promises, with the sniff reporting the
/// format's own signature and canvas size.
///
/// This is the only browser proof that the raster chunk is served, links with
/// no imports, and is wired to the card. The leg also proves the page hands the
/// file over the way `preview` says it should: a preview link for a format the
/// browser displays, none for one it does not.
fn check_raster_leg(leg: &Json, label: &str, spec: &RasterSpec) -> Result<(), String> {
    if leg.get("fileRows").and_then(Json::as_u64) != Some(1) {
        return Err(format!(
            "the {label} leg did not show one selected file (fileRows = {:?})",
            leg.get("fileRows")
        ));
    }
    if string_field(leg, "runLabel")? != spec.verb {
        return Err(format!(
            "the {label} button was labelled {:?}, expected {:?}",
            leg.get("runLabel"),
            spec.verb
        ));
    }
    check_no_upload_network(leg, label)?;

    let result = leg
        .get("result")
        .and_then(Json::as_object)
        .ok_or_else(|| format!("the {label} leg reported no result object"))?;
    let wanted = format!("report.{}", spec.extension);
    if result.get("title").and_then(Json::as_str) != Some(wanted.as_str()) {
        return Err(format!(
            "the {label} result was named {:?}, expected {wanted:?}",
            result.get("title")
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
    let pages_wanted = spec.pages.to_string();
    if row_value("Pages") != pages_wanted {
        return Err(format!(
            "the {label} run reported Pages {:?}, expected {pages_wanted:?}",
            row_value("Pages")
        ));
    }
    if row_value("Verification") != "ok" {
        return Err(format!(
            "the {label} run did not report a passing verification (got {:?})",
            row_value("Verification")
        ));
    }

    let outputs = leg
        .get("outputs")
        .and_then(Json::as_array)
        .ok_or_else(|| format!("the {label} run reported no output list"))?;
    if outputs.len() != 1 {
        return Err(format!(
            "the {label} run produced {} outputs, expected one {}",
            outputs.len(),
            spec.format
        ));
    }
    let output = outputs
        .first()
        .ok_or_else(|| format!("the {label} run produced no output"))?;
    if output.get("kind").and_then(Json::as_str) != Some("image") {
        return Err(format!(
            "the {label} output had kind {:?}, expected \"image\"",
            output.get("kind")
        ));
    }
    if output.get("name").and_then(Json::as_str) != Some(wanted.as_str())
        || output.get("downloadName").and_then(Json::as_str) != Some(wanted.as_str())
    {
        return Err(format!(
            "the {label} output/download was named {:?}/{:?}, expected {wanted:?}",
            output.get("name"),
            output.get("downloadName")
        ));
    }
    if output.get("pages").and_then(Json::as_u64) != Some(u64::from(spec.pages)) {
        return Err(format!(
            "the {label} output reported {:?} pages, expected {}",
            output.get("pages"),
            spec.pages
        ));
    }
    if output.get("blob").and_then(Json::as_bool) != Some(true) {
        return Err(format!(
            "the {label} output was not handed over as a Blob URL"
        ));
    }
    let previewed = output.get("previewBlob").and_then(Json::as_bool) == Some(true);
    if previewed != spec.preview {
        return Err(if spec.preview {
            format!("the {label} output offered no preview link")
        } else {
            format!(
                "the {label} output offered a preview link, and a {} is not displayed inline",
                spec.format
            )
        });
    }
    if output.get(spec.signature).and_then(Json::as_bool) != Some(true) {
        return Err(format!(
            "the {label} output did not read back as a complete {}",
            spec.format
        ));
    }
    if spec
        .baseline_key
        .is_some_and(|key| output.get(key).and_then(Json::as_bool) != Some(true))
    {
        return Err(format!(
            "the {label} {} is not the baseline sequential frame the chunk writes",
            spec.format
        ));
    }
    if spec
        .profile_key
        .is_some_and(|key| output.get(key).and_then(Json::as_bool) != Some(true))
    {
        return Err(format!(
            "the {label} {} is not the {} profile the chunk writes",
            spec.format, spec.profile_claim
        ));
    }
    if let Some(key) = spec.count_key {
        let counted = output.get(key).and_then(Json::as_u64).unwrap_or(0);
        if counted != u64::from(spec.pages) {
            return Err(format!(
                "the {label} {} holds {counted} page(s) by its own IFD chain, expected {}",
                spec.format, spec.pages
            ));
        }
    }
    let width = output
        .get(spec.width_key)
        .and_then(Json::as_u64)
        .unwrap_or(0);
    let height = output
        .get(spec.height_key)
        .and_then(Json::as_u64)
        .unwrap_or(0);
    if width == 0 || height == 0 {
        return Err(format!(
            "the {label} {} reported a zero canvas ({width}x{height})",
            spec.format
        ));
    }
    // A format with no text marker in its bytes (a TIFF has no chunk names like
    // IHDR or a JFIF tag) declares no needles, and the harness records no
    // content list for it; its proof is the structure walk above.
    if !spec.needles.is_empty() {
        let found = output
            .get("found")
            .and_then(Json::as_array)
            .ok_or_else(|| format!("the {label} output did not report its content checks"))?;
        if found.len() != spec.needles.len()
            || found.iter().any(|item| item.as_bool() != Some(true))
        {
            return Err(format!(
                "the {label} output is missing bytes the {} writer always writes ({found:?})",
                spec.format
            ));
        }
    }
    let absent = output
        .get("absentFound")
        .and_then(Json::as_array)
        .ok_or_else(|| format!("the {label} output did not report its absence checks"))?;
    if absent.iter().any(|item| item.as_bool() == Some(true)) {
        return Err(format!(
            "the {label} {} still carries PDF page syntax; the bytes were not rendered",
            spec.format
        ));
    }
    Ok(())
}

/// One multi-page raster leg: several pages come back as a single store-method
/// ZIP of `page-N.png` / `page-N.jpg` entries, so the browser never has to
/// allow a burst of downloads. The Pages row is the rendered count, and the
/// entry names prove the archive holds one image per page rather than one blob.
fn check_raster_zip_leg(leg: &Json, label: &str, spec: &RasterSpec) -> Result<(), String> {
    if leg.get("fileRows").and_then(Json::as_u64) != Some(1) {
        return Err(format!(
            "the {label} leg did not show one selected file (fileRows = {:?})",
            leg.get("fileRows")
        ));
    }
    if string_field(leg, "runLabel")? != spec.verb {
        return Err(format!(
            "the {label} button was labelled {:?}, expected {:?}",
            leg.get("runLabel"),
            spec.verb
        ));
    }
    check_no_upload_network(leg, label)?;

    let result = leg
        .get("result")
        .and_then(Json::as_object)
        .ok_or_else(|| format!("the {label} leg reported no result object"))?;
    if result.get("title").and_then(Json::as_str) != Some("report-pages.zip") {
        return Err(format!(
            "the {label} result was named {:?}, expected \"report-pages.zip\"",
            result.get("title")
        ));
    }
    let rows = result
        .get("rows")
        .and_then(Json::as_array)
        .ok_or_else(|| format!("the {label} result had no verification rows"))?;
    let pages = rows
        .iter()
        .find(|row| row.get("label").and_then(Json::as_str) == Some("Pages"))
        .and_then(|row| row.get("value"))
        .and_then(Json::as_str)
        .unwrap_or("");
    if pages != "3" {
        return Err(format!(
            "the {label} run reported Pages {pages:?}, expected \"3\""
        ));
    }

    let outputs = leg
        .get("outputs")
        .and_then(Json::as_array)
        .ok_or_else(|| format!("the {label} run reported no output list"))?;
    if outputs.len() != 1 {
        return Err(format!(
            "the {label} run produced {} outputs, expected one ZIP",
            outputs.len()
        ));
    }
    let output = outputs
        .first()
        .ok_or_else(|| format!("the {label} run produced no output"))?;
    if output.get("kind").and_then(Json::as_str) != Some("zip") {
        return Err(format!(
            "the {label} output had kind {:?}, expected \"zip\"",
            output.get("kind")
        ));
    }
    if output.get("name").and_then(Json::as_str) != Some("report-pages.zip")
        || output.get("downloadName").and_then(Json::as_str) != Some("report-pages.zip")
    {
        return Err(format!(
            "the {label} output/download was named {:?}/{:?}, expected \"report-pages.zip\"",
            output.get("name"),
            output.get("downloadName")
        ));
    }
    if output.get("blob").and_then(Json::as_bool) != Some(true) {
        return Err(format!(
            "the {label} output did not use a Blob URL for download"
        ));
    }
    if output.get("zip").and_then(Json::as_bool) != Some(true) {
        return Err(format!(
            "the {label} output did not begin with the ZIP local-file signature"
        ));
    }
    // An archive is not a document. The page may download it, but it must not
    // hand one to the browser viewer as a preview.
    if output.get("previewBlob").map(Json::is_null) != Some(true) {
        return Err(format!(
            "the {label} offered an archive as a preview (preview = {:?})",
            output.get("previewBlob")
        ));
    }
    let found = output
        .get("found")
        .and_then(Json::as_array)
        .ok_or_else(|| format!("the {label} output did not report its content checks"))?;
    if found.len() != spec.zip_needles.len() {
        return Err(format!(
            "the {label} output reported {} content checks, expected {}",
            found.len(),
            spec.zip_needles.len()
        ));
    }
    if let Some(index) = found.iter().position(|item| item.as_bool() != Some(true)) {
        return Err(format!(
            "the {label} bundle is missing {:?}; not every page was rendered",
            spec.zip_needles
                .get(index)
                .copied()
                .unwrap_or("an unnamed entry")
        ));
    }
    Ok(())
}

/// What the browser's own decoder made of a leg's file: the canvas it painted
/// and a digest of the samples it put on it. The harness carries one output
/// object per raster leg inside its `outputs` list, so that is where the paint
/// is read from. `None` when the leg carried no Chromium decoding at all — a
/// TIFF or an archive, which the browser does not draw, or a file Chromium could
/// not open.
fn chromium_painting(leg: &Json) -> Option<(u64, u64, u64)> {
    let output = leg
        .get("outputs")
        .and_then(Json::as_array)
        .and_then(|list| list.first())
        .or_else(|| leg.get("output"))
        .and_then(Json::as_object)?;
    let chromium = output.get("chromium").and_then(Json::as_object)?;
    let width = chromium.get("width").and_then(Json::as_u64)?;
    let height = chromium.get("height").and_then(Json::as_u64)?;
    let digest = chromium.get("digest").and_then(Json::as_u64)?;
    Some((width, height, digest))
}

/// The one raster proof that does not run in the writer's favour: two lossless
/// files of the same render — here a PNG and a WebP of the same page at the same
/// DPI — painted by Chromium's own decoders have to come out the same picture.
/// Neither decoder is ours, so a WebP whose samples drift from the page shows up
/// here as two digests that disagree, not as a reader agreeing with its own
/// writer.
fn check_lossless_pair_agrees(
    first: &Json,
    second: &Json,
    first_format: &str,
    second_format: &str,
    label: &str,
) -> Result<(), String> {
    let first_painting = chromium_painting(first).ok_or_else(|| {
        format!("the {label} {first_format} leg never reported what Chromium made of its file")
    })?;
    let second_painting = chromium_painting(second).ok_or_else(|| {
        format!("the {label} {second_format} leg never reported what Chromium made of its file")
    })?;
    if first_painting != second_painting {
        return Err(format!(
            "the {label} {second_format} is not the picture the {first_format} of the same render \
             is: Chromium painted the {first_format} {first_painting:?} and the {second_format} \
             {second_painting:?}"
        ));
    }
    Ok(())
}

/// One page of the positioned HTML document the card owes the user: its number
/// as the selection named it, the canvas it is drawn on, and how many
/// positioned spans the browser's parser found inside it.
struct HtmlSection {
    number: u64,
    width: u64,
    height: u64,
    spans: u64,
}

/// The HTML card's promise, in the shape the browser check can verify: a whole
/// selection is ONE document, each page is a section on its own canvas, and each
/// recovered word is a positioned span inside it.
struct HtmlSpec {
    verb: &'static str,
    /// The picked file's stem, which the page turns into both the download name
    /// and the document's `<title>`.
    stem: &'static str,
    /// The Pages row the result card must show, as the page writes it.
    pages_row: &'static str,
    sections: &'static [HtmlSection],
    needles: &'static [&'static str],
    /// Byte runs the document must not contain. A positioned text layer has no
    /// business holding an executable script.
    absent: &'static [&'static str],
}

/// Every page of the two-page fixture: page 1 carries one word, page 2 carries
/// none. Both must appear — a document that dropped the empty page would be
/// reporting a selection the user did not make.
const HTML_LEG: HtmlSpec = HtmlSpec {
    verb: "Export HTML",
    stem: "outlined",
    pages_row: "2",
    sections: &[
        HtmlSection {
            number: 1,
            width: 612,
            height: 792,
            spans: 1,
        },
        HtmlSection {
            number: 2,
            width: 612,
            height: 792,
            spans: 0,
        },
    ],
    needles: &["<!doctype html>", "Selis", "data-page=\"2\""],
    absent: &["<script"],
};

/// One page selected: still one document, and the section still says which page
/// it came from.
const HTML_ONE_LEG: HtmlSpec = HtmlSpec {
    pages_row: "1",
    sections: &[HtmlSection {
        number: 1,
        width: 612,
        height: 792,
        spans: 1,
    }],
    needles: &["Selis"],
    ..HTML_LEG
};

/// One HTML leg. The bytes the browser downloaded are re-walked by the browser's
/// own HTML parser — a second implementation of the format, and the only reading
/// that says what a user who opens the file will see — so this is the proof that
/// the document the chunk verified is the document the user receives.
fn check_html_leg(leg: &Json, label: &str, spec: &HtmlSpec) -> Result<(), String> {
    if leg.get("fileRows").and_then(Json::as_u64) != Some(1) {
        return Err(format!(
            "the {label} leg did not show one selected file (fileRows = {:?})",
            leg.get("fileRows")
        ));
    }
    if string_field(leg, "runLabel")? != spec.verb {
        return Err(format!(
            "the {label} button was labelled {:?}, expected {:?}",
            leg.get("runLabel"),
            spec.verb
        ));
    }
    check_no_upload_network(leg, label)?;

    let wanted = format!("{}.html", spec.stem);
    let result = leg
        .get("result")
        .and_then(Json::as_object)
        .ok_or_else(|| format!("the {label} leg reported no result object"))?;
    if result.get("title").and_then(Json::as_str) != Some(wanted.as_str()) {
        return Err(format!(
            "the {label} result was named {:?}, expected {wanted:?}",
            result.get("title")
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
    if row_value("Pages") != spec.pages_row {
        return Err(format!(
            "the {label} run reported Pages {:?}, expected {:?}",
            row_value("Pages"),
            spec.pages_row
        ));
    }
    if row_value("Verification") != "ok" {
        return Err(format!(
            "the {label} run did not report a passing verification (got {:?})",
            row_value("Verification")
        ));
    }

    let outputs = leg
        .get("outputs")
        .and_then(Json::as_array)
        .ok_or_else(|| format!("the {label} run reported no output list"))?;
    if outputs.len() != 1 {
        return Err(format!(
            "the {label} run produced {} outputs; a selection is one HTML document",
            outputs.len()
        ));
    }
    let output = outputs
        .first()
        .ok_or_else(|| format!("the {label} run produced no output"))?;
    if output.get("kind").and_then(Json::as_str) != Some("html") {
        return Err(format!(
            "the {label} output had kind {:?}, expected \"html\"",
            output.get("kind")
        ));
    }
    if output.get("name").and_then(Json::as_str) != Some(wanted.as_str())
        || output.get("downloadName").and_then(Json::as_str) != Some(wanted.as_str())
    {
        return Err(format!(
            "the {label} output/download was named {:?}/{:?}, expected {wanted:?}",
            output.get("name"),
            output.get("downloadName")
        ));
    }
    if output.get("pages").and_then(Json::as_u64) != Some(spec.sections.len() as u64) {
        return Err(format!(
            "the {label} document claimed {:?} pages, expected {}",
            output.get("pages"),
            spec.sections.len()
        ));
    }
    if output.get("blob").and_then(Json::as_bool) != Some(true) {
        return Err(format!(
            "the {label} output did not use a Blob URL for download"
        ));
    }
    // A blob: document runs in this page's own origin, so the card must offer
    // the download and nothing else.
    if output.get("previewBlob").map(Json::is_null) != Some(true) {
        return Err(format!(
            "the {label} offered an HTML document as a same-origin preview (preview = {:?})",
            output.get("previewBlob")
        ));
    }
    if output.get("html").and_then(Json::as_bool) != Some(true) {
        return Err(format!(
            "the {label} download did not open as an HTML document"
        ));
    }
    // One root, whatever the selection. Concatenating a document per page — the
    // shape this card replaced — would still parse, so the counts are taken from
    // the bytes rather than the repaired tree.
    for key in ["htmlDoctypes", "htmlRoots", "htmlClosers", "htmlMains"] {
        let counted = output.get(key).and_then(Json::as_u64).unwrap_or(0);
        if counted != 1 {
            return Err(format!(
                "the {label} document holds {counted} {key} markers, expected exactly one"
            ));
        }
    }
    if output.get("htmlTitle").and_then(Json::as_str) != Some(spec.stem) {
        return Err(format!(
            "the {label} document is titled {:?}, expected {:?}",
            output.get("htmlTitle"),
            spec.stem
        ));
    }

    let pages = output
        .get("htmlPages")
        .and_then(Json::as_array)
        .ok_or_else(|| format!("the {label} document carried no pages for the harness to walk"))?;
    if pages.len() != spec.sections.len() {
        return Err(format!(
            "the {label} document holds {} section(s), expected {}",
            pages.len(),
            spec.sections.len()
        ));
    }
    for (index, expected) in spec.sections.iter().enumerate() {
        let Some(page) = pages.get(index).and_then(Json::as_object) else {
            return Err(format!(
                "the {label} document has no section at position {index}"
            ));
        };
        let number = page.get("number").and_then(Json::as_u64).unwrap_or(0);
        if number != expected.number {
            return Err(format!(
                "the {label} document's section {index} is numbered {number}, expected {}",
                expected.number
            ));
        }
        let width = page.get("width").and_then(Json::as_u64).unwrap_or(0);
        let height = page.get("height").and_then(Json::as_u64).unwrap_or(0);
        if (width, height) != (expected.width, expected.height) {
            return Err(format!(
                "the {label} document's page {number} is a {width}x{height} canvas, expected {}x{}",
                expected.width, expected.height
            ));
        }
        if page.get("style").and_then(Json::as_bool) != Some(true) {
            return Err(format!(
                "the {label} document's page {number} declares a canvas its own style disagrees with"
            ));
        }
        let spans = page.get("spans").and_then(Json::as_u64).unwrap_or(0);
        if spans != expected.spans {
            return Err(format!(
                "the {label} document's page {number} holds {spans} positioned span(s), expected {}",
                expected.spans
            ));
        }
        // A span outside its section is text the user cannot see: the box was
        // projected onto a canvas the page does not have.
        let outside = page.get("outside").and_then(Json::as_u64).unwrap_or(0);
        if outside != 0 {
            return Err(format!(
                "the {label} document puts {outside} span(s) outside the canvas of page {number}"
            ));
        }
        // Nothing in these fixtures drew text the extractor could not recover,
        // so a marker here would be a false alarm on a page that reads.
        if page.get("marker").and_then(Json::as_u64).unwrap_or(0) != 0 {
            return Err(format!(
                "the {label} document flags page {number} as low-confidence without reason"
            ));
        }
    }

    let found = output
        .get("found")
        .and_then(Json::as_array)
        .ok_or_else(|| format!("the {label} output did not report its content checks"))?;
    if found.len() != spec.needles.len() {
        return Err(format!(
            "the {label} output reported {} content checks, expected {}",
            found.len(),
            spec.needles.len()
        ));
    }
    if let Some(index) = found.iter().position(|item| item.as_bool() != Some(true)) {
        return Err(format!(
            "the {label} document is missing {:?}; the extraction did not reach the file",
            spec.needles
                .get(index)
                .copied()
                .unwrap_or("an unnamed marker")
        ));
    }
    let absent = output
        .get("absentFound")
        .and_then(Json::as_array)
        .ok_or_else(|| format!("the {label} output did not report its absence checks"))?;
    if absent.len() != spec.absent.len() {
        return Err(format!(
            "the {label} output reported {} absence checks, expected {}",
            absent.len(),
            spec.absent.len()
        ));
    }
    if let Some(index) = absent.iter().position(|item| item.as_bool() == Some(true)) {
        return Err(format!(
            "the {label} document carries {:?}, which this card never writes",
            spec.absent
                .get(index)
                .copied()
                .unwrap_or("an unnamed marker")
        ));
    }
    Ok(())
}

/// The downloaded byte count of a leg's first output.
fn leg_output_bytes(leg: &Json, label: &str) -> Result<u64, String> {
    leg.get("outputs")
        .and_then(Json::as_array)
        .and_then(|outputs| outputs.first())
        .and_then(|output| output.get("bytes"))
        .and_then(Json::as_u64)
        .ok_or_else(|| format!("the {label} leg reported no downloaded byte count"))
}

/// One leg's file must come back smaller than another's. This is the witness
/// that a knob the page exposes changes the bytes the browser hands over rather
/// than only the encoder's internals.
fn check_smaller_than(
    small: &Json,
    large: &Json,
    small_label: &str,
    large_label: &str,
) -> Result<(), String> {
    let small_bytes = leg_output_bytes(small, small_label)?;
    let large_bytes = leg_output_bytes(large, large_label)?;
    if small_bytes >= large_bytes {
        return Err(format!(
            "{small_label} downloaded {small_bytes} bytes against {large_label}'s {large_bytes}; \
             the quality knob changed nothing"
        ));
    }
    Ok(())
}

/// One form-fill leg: the fixture's seven fields render, the signature and
/// read-only rows are disabled, the signature warning is shown, and the
/// downloaded Blob carries the fills and asks the viewer to rebuild
/// appearances.
///
/// This is the only browser proof that the form-fill card works through the
/// real page: the fields list comes from the tools chunk, the fill is
/// committed through the same engine whose CLI writes the bytes, and the
/// disclosure of signature invalidation is visible before the user touches
/// the run button.
fn check_fill_forms_leg(leg: &Json, label: &str) -> Result<(), String> {
    const NEEDLES: [&str; 2] = ["(Browser Ada Lovelace)", "/NeedAppearances true"];
    if leg.get("fileRows").and_then(Json::as_u64) != Some(1) {
        return Err(format!(
            "the {label} leg did not show one selected file (fileRows = {:?})",
            leg.get("fileRows")
        ));
    }
    if string_field(leg, "runLabel")? != "Fill form" {
        return Err(format!(
            "the {label} button was labelled {:?}, expected \"Fill form\"",
            leg.get("runLabel")
        ));
    }
    check_no_upload_network(leg, label)?;

    if leg.get("fieldRows").and_then(Json::as_u64) != Some(7) {
        return Err(format!(
            "the {label} leg rendered {} form rows, expected 7",
            leg.get("fieldRows").and_then(Json::as_u64).unwrap_or(0)
        ));
    }
    if leg.get("signatureWarn").and_then(Json::as_bool) != Some(true) {
        return Err(format!(
            "the {label} leg never showed the signature warning; an invalidated signature must be disclosed"
        ));
    }
    if leg.get("signatureFieldDisabled").and_then(Json::as_bool) != Some(true) {
        return Err(format!("the {label} leg left the signature field fillable"));
    }
    if leg.get("readOnlyDisabled").and_then(Json::as_bool) != Some(true) {
        return Err(format!("the {label} leg left the read-only field fillable"));
    }

    let result = leg
        .get("result")
        .and_then(Json::as_object)
        .ok_or_else(|| format!("the {label} leg reported no result object"))?;
    if result.get("title").and_then(Json::as_str) != Some("form-filled.pdf") {
        return Err(format!(
            "the {label} result was named {:?}, expected \"form-filled.pdf\"",
            result.get("title")
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
    if row_value("Pages") != "1 \u{2192} 1" {
        return Err(format!(
            "the {label} run reported Pages {:?}, expected \"1 → 1\"",
            row_value("Pages")
        ));
    }
    if row_value("Verification") != "ok" {
        return Err(format!(
            "the {label} run did not report a passing verification (got {:?})",
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
    let output = outputs
        .first()
        .ok_or_else(|| format!("the {label} run produced no output"))?;
    if output.get("kind").and_then(Json::as_str) != Some("pdf") {
        return Err(format!(
            "the {label} output had kind {:?}, expected \"pdf\"",
            output.get("kind")
        ));
    }
    if output.get("name").and_then(Json::as_str) != Some("form-filled.pdf")
        || output.get("downloadName").and_then(Json::as_str) != Some("form-filled.pdf")
    {
        return Err(format!(
            "the {label} output/download was named {:?}/{:?}, expected \"form-filled.pdf\"",
            output.get("name"),
            output.get("downloadName")
        ));
    }
    if output.get("pages").and_then(Json::as_u64) != Some(1) {
        return Err(format!(
            "the {label} output reported {:?} pages, expected 1",
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

    let found = output
        .get("found")
        .and_then(Json::as_array)
        .ok_or_else(|| format!("the {label} output did not report its content checks"))?;
    if found.len() != NEEDLES.len() {
        return Err(format!(
            "the {label} output reported {} content checks, expected {}",
            found.len(),
            NEEDLES.len()
        ));
    }
    if let Some(index) = found.iter().position(|item| item.as_bool() != Some(true)) {
        return Err(format!(
            "the {label} output is missing {:?}; the filled document does not carry the fills",
            NEEDLES[index]
        ));
    }
    Ok(())
}

/// One edit-text leg: a one-page invoice whose text `900` is replaced with
/// `1250`. The edit is written as an incremental update, so the output is a
/// `%PDF-` file named after the input stem that carries the new show-text
/// operator in its new revision, produced with no upload.
fn check_edit_text_leg(leg: &Json, label: &str) -> Result<(), String> {
    const NEEDLES: [&str; 1] = ["(Invoice total: 1250 USD) Tj"];
    if leg.get("fileRows").and_then(Json::as_u64) != Some(1) {
        return Err(format!(
            "the {label} leg did not show one selected file (fileRows = {:?})",
            leg.get("fileRows")
        ));
    }
    if string_field(leg, "runLabel")? != "Replace text" {
        return Err(format!(
            "the {label} button was labelled {:?}, expected \"Replace text\"",
            leg.get("runLabel")
        ));
    }
    check_no_upload_network(leg, label)?;

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
    let output = outputs
        .first()
        .ok_or_else(|| format!("the {label} run produced no output"))?;
    if output.get("kind").and_then(Json::as_str) != Some("pdf") {
        return Err(format!(
            "the {label} output had kind {:?}, expected \"pdf\"",
            output.get("kind")
        ));
    }
    if output.get("name").and_then(Json::as_str) != Some("edit-edited.pdf") {
        return Err(format!(
            "the {label} output was named {:?}, expected \"edit-edited.pdf\"",
            output.get("name")
        ));
    }
    if output.get("header").and_then(Json::as_str) != Some("%PDF-") {
        return Err(format!(
            "the {label} output did not begin with %PDF- (header = {:?})",
            output.get("header")
        ));
    }
    let found = output
        .get("found")
        .and_then(Json::as_array)
        .ok_or_else(|| format!("the {label} output did not report its content checks"))?;
    if found.len() != NEEDLES.len() {
        return Err(format!(
            "the {label} output reported {} content checks, expected {}",
            found.len(),
            NEEDLES.len()
        ));
    }
    if let Some(index) = found.iter().position(|item| item.as_bool() != Some(true)) {
        return Err(format!(
            "the {label} output is missing {:?}; the edit did not take",
            NEEDLES[index]
        ));
    }
    Ok(())
}

/// One image→PDF leg: two image files (PNG + JPEG) become one PDF with one
/// page per image, and the real Blob carries Flate-compressed DeviceRGB image
/// XObjects. The page count row has no input count, because the output is
/// generated rather than rewritten.
fn check_image_leg(leg: &Json, label: &str) -> Result<(), String> {
    if leg.get("fileRows").and_then(Json::as_u64) != Some(2) {
        return Err(format!(
            "the {label} leg did not show two selected images (fileRows = {:?})",
            leg.get("fileRows")
        ));
    }
    if string_field(leg, "noUpload")? != "verified" {
        return Err(format!(
            "the {label} leg never set data-no-upload=verified; the local request check did not pass"
        ));
    }
    if string_field(leg, "runLabel")? != "Create PDF" {
        return Err(format!(
            "the {label} button was labelled {:?}, expected \"Create PDF\"",
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
    if result.get("title").and_then(Json::as_str) != Some("one-images.pdf") {
        return Err(format!(
            "the {label} result was named {:?}, expected \"one-images.pdf\"",
            result.get("title")
        ));
    }
    let rows = result
        .get("rows")
        .and_then(Json::as_array)
        .ok_or_else(|| format!("the {label} leg reported no verification rows"))?;
    let row_value = |wanted: &str| {
        rows.iter()
            .find(|row| row.get("label").and_then(Json::as_str) == Some(wanted))
            .and_then(|row| row.get("value"))
            .and_then(Json::as_str)
            .unwrap_or("")
    };
    if row_value("Pages") != "2" {
        return Err(format!(
            "the {label} run reported Pages {:?}, expected \"2\"",
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
    let output = outputs
        .first()
        .ok_or_else(|| format!("the {label} run produced no output"))?;
    if output.get("kind").and_then(Json::as_str) != Some("pdf") {
        return Err(format!(
            "the {label} output had kind {:?}, expected \"pdf\"",
            output.get("kind")
        ));
    }
    if output.get("name").and_then(Json::as_str) != Some("one-images.pdf") {
        return Err(format!(
            "the {label} output was named {:?}, expected \"one-images.pdf\"",
            output.get("name")
        ));
    }
    if output.get("pages").and_then(Json::as_u64) != Some(2) {
        return Err(format!(
            "the {label} output reported {:?} pages, expected 2",
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
    if output.get("downloadName").and_then(Json::as_str) != Some("one-images.pdf") {
        return Err(format!(
            "the {label} download was named {:?}, expected \"one-images.pdf\"",
            output.get("downloadName")
        ));
    }
    let found = output
        .get("found")
        .and_then(Json::as_array)
        .ok_or_else(|| format!("the {label} output did not report its needle checks"))?;
    if found.len() != IMAGE_NEEDLES.len() {
        return Err(format!(
            "the {label} output reported {} content checks, expected {}",
            found.len(),
            IMAGE_NEEDLES.len()
        ));
    }
    if let Some(index) = found.iter().position(|item| item.as_bool() != Some(true)) {
        return Err(format!(
            "the {label} output is missing {:?}; the images were not embedded as Flate RGB XObjects",
            IMAGE_NEEDLES[index]
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
/// One image-watermark leg: a PDF plus a PNG produce a PDF whose real Blob
/// carries the embedded RGB XObject and a placement that names it. The page
/// count is the input count because the mark rewrites pages in place.
fn check_image_watermark_leg(leg: &Json, label: &str) -> Result<(), String> {
    if leg.get("fileRows").and_then(Json::as_u64) != Some(2) {
        return Err(format!(
            "the {label} leg did not show two selected files (fileRows = {:?})",
            leg.get("fileRows")
        ));
    }
    if string_field(leg, "noUpload")? != "verified" {
        return Err(format!(
            "the {label} leg never set data-no-upload=verified; the local request check did not pass"
        ));
    }
    if string_field(leg, "runLabel")? != "Add image watermark" {
        return Err(format!(
            "the {label} button was labelled {:?}, expected \"Add image watermark\"",
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
    if result.get("title").and_then(Json::as_str) != Some("one-image-watermarked.pdf") {
        return Err(format!(
            "the {label} result was named {:?}, expected \"one-image-watermarked.pdf\"",
            result.get("title")
        ));
    }
    let rows = result
        .get("rows")
        .and_then(Json::as_array)
        .ok_or_else(|| format!("the {label} leg reported no verification rows"))?;
    let row_value = |wanted: &str| {
        rows.iter()
            .find(|row| row.get("label").and_then(Json::as_str) == Some(wanted))
            .and_then(|row| row.get("value"))
            .and_then(Json::as_str)
            .unwrap_or("")
    };
    if row_value("Pages") != "1 \u{2192} 1" {
        return Err(format!(
            "the {label} run reported Pages {:?}, expected \"1 \u{2192} 1\"",
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
    let output = outputs
        .first()
        .ok_or_else(|| format!("the {label} run produced no output"))?;
    if output.get("kind").and_then(Json::as_str) != Some("pdf") {
        return Err(format!(
            "the {label} output had kind {:?}, expected \"pdf\"",
            output.get("kind")
        ));
    }
    if output.get("name").and_then(Json::as_str) != Some("one-image-watermarked.pdf") {
        return Err(format!(
            "the {label} output was named {:?}, expected \"one-image-watermarked.pdf\"",
            output.get("name")
        ));
    }
    if output.get("pages").and_then(Json::as_u64) != Some(1) {
        return Err(format!(
            "the {label} output reported {:?} pages, expected 1",
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
    if output.get("downloadName").and_then(Json::as_str) != Some("one-image-watermarked.pdf") {
        return Err(format!(
            "the {label} download was named {:?}, expected \"one-image-watermarked.pdf\"",
            output.get("downloadName")
        ));
    }
    let found = output
        .get("found")
        .and_then(Json::as_array)
        .ok_or_else(|| format!("the {label} output did not report its needle checks"))?;
    if found.len() != IMAGE_WATERMARK_NEEDLES.len() {
        return Err(format!(
            "the {label} output reported {} content checks, expected {}",
            found.len(),
            IMAGE_WATERMARK_NEEDLES.len()
        ));
    }
    if let Some(index) = found.iter().position(|item| item.as_bool() != Some(true)) {
        return Err(format!(
            "the {label} output is missing {:?}; the image was not embedded and drawn",
            IMAGE_WATERMARK_NEEDLES[index]
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
/// One outline leg: a three-page PDF is rewritten with a two-entry bookmark
/// tree, and the downloaded bytes carry the root and both titles.
fn check_outline_leg(leg: &Json, label: &str) -> Result<(), String> {
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
    if string_field(leg, "runLabel")? != "Save bookmarks" {
        return Err(format!(
            "the {label} button was labelled {:?}, expected \"Save bookmarks\"",
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
    if result.get("title").and_then(Json::as_str) != Some("report-outline.pdf") {
        return Err(format!(
            "the {label} result was named {:?}, expected \"report-outline.pdf\"",
            result.get("title")
        ));
    }
    let rows = result
        .get("rows")
        .and_then(Json::as_array)
        .ok_or_else(|| format!("the {label} leg reported no verification rows"))?;
    let row_value = |wanted: &str| {
        rows.iter()
            .find(|row| row.get("label").and_then(Json::as_str) == Some(wanted))
            .and_then(|row| row.get("value"))
            .and_then(Json::as_str)
            .unwrap_or("")
    };
    if row_value("Pages") != "3 \u{2192} 3" {
        return Err(format!(
            "the {label} run reported Pages {:?}, expected \"3 \u{2192} 3\"",
            row_value("Pages")
        ));
    }
    if row_value("Outline entries") != "0 \u{2192} 2" {
        return Err(format!(
            "the {label} run reported Outline entries {:?}, expected \"0 \u{2192} 2\"",
            row_value("Outline entries")
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
    let output = outputs
        .first()
        .ok_or_else(|| format!("the {label} run produced no output"))?;
    if output.get("kind").and_then(Json::as_str) != Some("pdf") {
        return Err(format!(
            "the {label} output had kind {:?}, expected \"pdf\"",
            output.get("kind")
        ));
    }
    if output.get("name").and_then(Json::as_str) != Some("report-outline.pdf") {
        return Err(format!(
            "the {label} output was named {:?}, expected \"report-outline.pdf\"",
            output.get("name")
        ));
    }
    if output.get("pages").and_then(Json::as_u64) != Some(3) {
        return Err(format!(
            "the {label} output reported {:?} pages, expected 3",
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
    if output.get("downloadName").and_then(Json::as_str) != Some("report-outline.pdf") {
        return Err(format!(
            "the {label} download was named {:?}, expected \"report-outline.pdf\"",
            output.get("downloadName")
        ));
    }
    let found = output
        .get("found")
        .and_then(Json::as_array)
        .ok_or_else(|| format!("the {label} output did not report its needle checks"))?;
    if found.len() != OUTLINE_NEEDLES.len() {
        return Err(format!(
            "the {label} output reported {} content checks, expected {}",
            found.len(),
            OUTLINE_NEEDLES.len()
        ));
    }
    if let Some(index) = found.iter().position(|item| item.as_bool() != Some(true)) {
        return Err(format!(
            "the {label} output is missing {:?}; the outline was not rewritten",
            OUTLINE_NEEDLES[index]
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

/// One attach leg: a PDF plus an opaque payload. The proof is the page count
/// staying put, the embedded-file count rising to one, and the real Blob
/// carrying both the name tree and the payload's name.
fn check_attach_leg(leg: &Json, label: &str) -> Result<(), String> {
    if leg.get("fileRows").and_then(Json::as_u64) != Some(2) {
        return Err(format!(
            "the {label} leg did not show a PDF and one payload file (fileRows = {:?})",
            leg.get("fileRows")
        ));
    }
    if string_field(leg, "noUpload")? != "verified" {
        return Err(format!(
            "the {label} leg never set data-no-upload=verified; the local request check did not pass"
        ));
    }
    if string_field(leg, "runLabel")? != "Embed files" {
        return Err(format!(
            "the {label} button was labelled {:?}, expected \"Embed files\"",
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
    if result.get("title").and_then(Json::as_str) != Some("report-attached.pdf") {
        return Err(format!(
            "the {label} result was named {:?}, expected \"report-attached.pdf\"",
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
    if row_value("Pages") != "3 \u{2192} 3" {
        return Err(format!(
            "the {label} run reported Pages {:?}, expected \"3 \u{2192} 3\"",
            row_value("Pages")
        ));
    }
    if row_value("Embedded files") != "0 \u{2192} 1" {
        return Err(format!(
            "the {label} run reported Embedded files {:?}, expected \"0 \u{2192} 1\"",
            row_value("Embedded files")
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
    let output = outputs
        .first()
        .ok_or_else(|| format!("the {label} run produced no output"))?;
    if output.get("kind").and_then(Json::as_str) != Some("pdf") {
        return Err(format!(
            "the {label} output had kind {:?}, expected \"pdf\"",
            output.get("kind")
        ));
    }
    if output.get("name").and_then(Json::as_str) != Some("report-attached.pdf") {
        return Err(format!(
            "the {label} output was named {:?}, expected \"report-attached.pdf\"",
            output.get("name")
        ));
    }
    if output.get("pages").and_then(Json::as_u64) != Some(3) {
        return Err(format!(
            "the {label} output reported {:?} pages, expected 3",
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
    if output.get("downloadName").and_then(Json::as_str) != Some("report-attached.pdf") {
        return Err(format!(
            "the {label} download was named {:?}, expected \"report-attached.pdf\"",
            output.get("downloadName")
        ));
    }
    if output.get("bytes").and_then(Json::as_u64).unwrap_or(0) <= 500 {
        return Err(format!(
            "the {label} PDF is only {:?} bytes; a three-page result with an embedded file should be larger",
            output.get("bytes")
        ));
    }
    let found = output
        .get("found")
        .and_then(Json::as_array)
        .ok_or_else(|| format!("the {label} output did not report its needle checks"))?;
    if found.len() != 2 || found.iter().any(|item| item.as_bool() != Some(true)) {
        return Err(format!(
            "the {label} output did not carry both /EmbeddedFiles and note.txt (found = {found:?})"
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

/// One detach leg: every embedded file is gone, the page count is unchanged,
/// and the negative witnesses confirm the name tree and payload bytes are
/// absent from the real Blob.
fn check_detach_leg(leg: &Json, label: &str) -> Result<(), String> {
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
    if string_field(leg, "runLabel")? != "Remove attachments" {
        return Err(format!(
            "the {label} button was labelled {:?}, expected \"Remove attachments\"",
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
    if result.get("title").and_then(Json::as_str) != Some("attachments-detached.pdf") {
        return Err(format!(
            "the {label} result was named {:?}, expected \"attachments-detached.pdf\"",
            result.get("title")
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
    if row_value("Pages") != "1 \u{2192} 1" {
        return Err(format!(
            "the {label} run reported Pages {:?}, expected \"1 \u{2192} 1\"",
            row_value("Pages")
        ));
    }
    if row_value("Embedded files") != "2 \u{2192} 0" {
        return Err(format!(
            "the {label} run reported Embedded files {:?}, expected \"2 \u{2192} 0\"",
            row_value("Embedded files")
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
    let output = outputs
        .first()
        .ok_or_else(|| format!("the {label} run produced no output"))?;
    if output.get("kind").and_then(Json::as_str) != Some("pdf") {
        return Err(format!(
            "the {label} output had kind {:?}, expected \"pdf\"",
            output.get("kind")
        ));
    }
    if output.get("name").and_then(Json::as_str) != Some("attachments-detached.pdf") {
        return Err(format!(
            "the {label} output was named {:?}, expected \"attachments-detached.pdf\"",
            output.get("name")
        ));
    }
    if output.get("pages").and_then(Json::as_u64) != Some(1) {
        return Err(format!(
            "the {label} output reported {:?} pages, expected 1",
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
    if output.get("downloadName").and_then(Json::as_str) != Some("attachments-detached.pdf") {
        return Err(format!(
            "the {label} download was named {:?}, expected \"attachments-detached.pdf\"",
            output.get("downloadName")
        ));
    }
    let absent = output
        .get("absentFound")
        .and_then(Json::as_array)
        .ok_or_else(|| format!("the {label} output did not report its absence checks"))?;
    if absent.len() != 5 || absent.iter().any(|item| item.as_bool() != Some(false)) {
        return Err(format!(
            "the {label} output still carries removed attachment bytes (absentFound = {absent:?})"
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

/// One repair leg: a damaged cross-reference table is rebuilt, every page is
/// kept, the recovery strategy is shown, and the damaged marker is gone from
/// the real Blob.
fn check_repair_leg(leg: &Json, label: &str) -> Result<(), String> {
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
    if string_field(leg, "runLabel")? != "Repair PDF" {
        return Err(format!(
            "the {label} button was labelled {:?}, expected \"Repair PDF\"",
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
    if result.get("title").and_then(Json::as_str) != Some("damaged-repaired.pdf") {
        return Err(format!(
            "the {label} result was named {:?}, expected \"damaged-repaired.pdf\"",
            result.get("title")
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
    if row_value("Pages") != "3 \u{2192} 3" {
        return Err(format!(
            "the {label} run reported Pages {:?}, expected \"3 \u{2192} 3\"",
            row_value("Pages")
        ));
    }
    if row_value("Recovery") != "reconstructed index" {
        return Err(format!(
            "the {label} run reported Recovery {:?}, expected \"reconstructed index\"",
            row_value("Recovery")
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
    let output = outputs
        .first()
        .ok_or_else(|| format!("the {label} run produced no output"))?;
    if output.get("kind").and_then(Json::as_str) != Some("pdf") {
        return Err(format!(
            "the {label} output had kind {:?}, expected \"pdf\"",
            output.get("kind")
        ));
    }
    if output.get("name").and_then(Json::as_str) != Some("damaged-repaired.pdf") {
        return Err(format!(
            "the {label} output was named {:?}, expected \"damaged-repaired.pdf\"",
            output.get("name")
        ));
    }
    if output.get("pages").and_then(Json::as_u64) != Some(3) {
        return Err(format!(
            "the {label} output reported {:?} pages, expected 3",
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
    if output.get("downloadName").and_then(Json::as_str) != Some("damaged-repaired.pdf") {
        return Err(format!(
            "the {label} download was named {:?}, expected \"damaged-repaired.pdf\"",
            output.get("downloadName")
        ));
    }
    let absent = output
        .get("absentFound")
        .and_then(Json::as_array)
        .ok_or_else(|| format!("the {label} output did not report its absence checks"))?;
    if absent.len() != 1 || absent.iter().any(|item| item.as_bool() != Some(false)) {
        return Err(format!(
            "the {label} output still carries the damaged index marker (absentFound = {absent:?})"
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

/// One leg of the report, or the sentence that says it never ran: a card the
/// driver did not open has to fail here rather than be silently absent.
fn leg_of<'a>(report: &'a Json, key: &str) -> Result<&'a Json, String> {
    report
        .get(key)
        .ok_or_else(|| format!("the page reported no {key} leg ({key} = None)"))
}

/// The same, for the half of the run that happens after the origin is cut.
fn offline_leg_of<'a>(report: &'a Json, key: &str) -> Result<&'a Json, String> {
    leg_of(report, "offline")?
        .get(key)
        .ok_or_else(|| format!("the offline leg reported no {key} result ({key} = None)"))
}

/// The password cards report as one block, because their legs chain: the unlock
/// of the page's own file reads what the protect leg above it wrote.
fn security_legs<'a>(parent: &'a Json, phase: &str) -> Result<&'a Json, String> {
    parent
        .get("security")
        .ok_or_else(|| format!("the {phase} run reported no password legs (security = None)"))
}

/// One named leg inside the security block.
fn security_leg<'a>(block: &'a Json, phase: &str, name: &str) -> Result<&'a Json, String> {
    block
        .get(name)
        .ok_or_else(|| format!("the {phase} password run reported no {name} leg"))
}

/// The signing cards report as one block, because their legs chain: the report
/// reads what the certificate leg above it wrote.
fn signing_legs<'a>(parent: &'a Json, phase: &str) -> Result<&'a Json, String> {
    parent
        .get("sign")
        .ok_or_else(|| format!("the {phase} run reported no signing legs (sign = None)"))
}

/// One named leg inside the signing block.
fn signing_leg<'a>(block: &'a Json, phase: &str, name: &str) -> Result<&'a Json, String> {
    block
        .get(name)
        .ok_or_else(|| format!("the {phase} signing run reported no {name} leg"))
}

/// One card, one run, one downloaded file. The rotate, delete, edit-metadata
/// and ZIP legs all end this way and differ only in the name of the result, the
/// page count the gate must assert, and which markers the bytes must carry.
///
/// The marker counts are read from the downloaded Blob, not from what the
/// engine claimed: `/Rotate 90` three times is what proves the rotate card
/// turned every page rather than the first one it found, and `/Count 2` with
/// `/Count 3` gone is what proves the delete card rebuilt the page tree instead
/// of only hiding a page.
struct SingleLeg<'a> {
    label: &'a str,
    run_label: &'a str,
    output_name: &'a str,
    file_rows: u64,
    zip: bool,
    pages: u64,
    pages_row: Option<&'a str>,
    found_counts: &'a [u64],
}

fn check_single_output_leg(leg: &Json, want: &SingleLeg) -> Result<(), String> {
    let label = want.label;
    if leg.get("fileRows").and_then(Json::as_u64) != Some(want.file_rows) {
        return Err(format!(
            "the {label} leg did not show {} selected files (fileRows = {:?})",
            want.file_rows,
            leg.get("fileRows")
        ));
    }
    if string_field(leg, "noUpload")? != "verified" {
        return Err(format!(
            "the {label} leg never set data-no-upload=verified; the local request check did not pass"
        ));
    }
    if string_field(leg, "runLabel")? != want.run_label {
        return Err(format!(
            "the {label} button was labelled {:?}, expected {:?}",
            leg.get("runLabel"),
            want.run_label
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
    if result.get("title").and_then(Json::as_str) != Some(want.output_name) {
        return Err(format!(
            "the {label} result was named {:?}, expected {:?}",
            result.get("title"),
            want.output_name
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
    if row_value("Verification") != "ok" {
        return Err(format!(
            "the {label} card reported Verification {:?}, expected \"ok\"",
            row_value("Verification")
        ));
    }
    if let Some(expected) = want.pages_row {
        if row_value("Pages") != expected {
            return Err(format!(
                "the {label} card reported Pages {:?}, expected {expected:?}",
                row_value("Pages")
            ));
        }
    }

    let outputs = leg
        .get("outputs")
        .and_then(Json::as_array)
        .ok_or_else(|| format!("the {label} run reported no output list"))?;
    if outputs.len() != 1 {
        return Err(format!(
            "the {label} run produced {} outputs, expected one file",
            outputs.len()
        ));
    }
    let output = outputs
        .first()
        .ok_or_else(|| format!("the {label} run produced no output"))?;
    let expected_kind = if want.zip { "zip" } else { "pdf" };
    if output.get("kind").and_then(Json::as_str) != Some(expected_kind) {
        return Err(format!(
            "the {label} output had kind {:?}, expected {expected_kind:?}",
            output.get("kind")
        ));
    }
    if output.get("name").and_then(Json::as_str) != Some(want.output_name)
        || output.get("downloadName").and_then(Json::as_str) != Some(want.output_name)
    {
        return Err(format!(
            "the {label} output was named {:?} / downloaded as {:?}, expected {:?}",
            output.get("name"),
            output.get("downloadName"),
            want.output_name
        ));
    }
    if output.get("zip").and_then(Json::as_bool) != Some(want.zip) {
        return Err(format!(
            "the {label} output claimed zip = {:?}, expected {:?}",
            output.get("zip"),
            want.zip
        ));
    }
    if output.get("pages").and_then(Json::as_u64) != Some(want.pages) {
        return Err(format!(
            "the {label} output reported {:?} pages, expected {}",
            output.get("pages"),
            want.pages
        ));
    }
    let expected_header = if want.zip {
        "PK\u{3}\u{4}\u{14}"
    } else {
        "%PDF-"
    };
    if output.get("header").and_then(Json::as_str) != Some(expected_header) {
        return Err(format!(
            "the {label} output began with {:?}, expected {expected_header:?}",
            output.get("header")
        ));
    }
    if output.get("blob").and_then(Json::as_bool) != Some(true) {
        return Err(format!("the {label} output was not offered as a Blob URL"));
    }
    // An archive is not a document: the page must download it and never open it
    // in the PDF viewer.
    let preview = output
        .get("previewBlob")
        .ok_or_else(|| format!("the {label} output said nothing about a preview"))?;
    if want.zip {
        if *preview != Json::Null {
            return Err(format!(
                "the {label} output offered an archive as a PDF preview (preview = {preview:?})"
            ));
        }
    } else if preview.as_bool() != Some(true) {
        return Err(format!(
            "the {label} output did not offer its PDF as a Blob URL preview (preview = {preview:?})"
        ));
    }
    let declared = output.get("declaredBytes").and_then(Json::as_u64);
    if declared.is_none() || declared != output.get("bytes").and_then(Json::as_u64) {
        return Err(format!(
            "the {label} download was {:?} bytes while the row declared {declared:?}",
            output.get("bytes")
        ));
    }
    let found = output
        .get("foundCounts")
        .and_then(Json::as_array)
        .ok_or_else(|| format!("the {label} output reported no marker counts"))?;
    let counts: Vec<u64> = found.iter().filter_map(Json::as_u64).collect();
    if counts != want.found_counts {
        return Err(format!(
            "the {label} bytes carried the markers {counts:?}, expected {:?}",
            want.found_counts
        ));
    }
    if output
        .get("found")
        .and_then(Json::as_array)
        .is_none_or(|items| items.iter().any(|item| item.as_bool() != Some(true)))
    {
        return Err(format!(
            "the {label} output did not carry every marker the leg asked for"
        ));
    }
    if let Some(absent) = output.get("absentFound").and_then(Json::as_array) {
        if absent.iter().any(|item| item.as_bool() != Some(false)) {
            return Err(format!(
                "the {label} output still carries a marker it was meant to leave behind (absentFound = {absent:?})"
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

/// One password card's promise, in the shape the browser check can verify.
///
/// `check_single_output_leg` pins how many times each marker appears, which is
/// right for a stamp or a rotation — the count IS the claim. Here the claim is
/// about protection: an envelope of a given revision, a flag with a given value,
/// and `/Encrypt` gone from an unlocked file. How many objects a crypt filter
/// happens to name is the writer's business, so this asserts presence, not
/// multiplicity, and spends its strictness on the result card's sentences
/// instead.
struct SecuritySpec<'a> {
    run_label: &'a str,
    /// The finished sentence, in full. A card that changed its claim about what
    /// it did is exactly what this gate is for.
    status: &'a str,
    output_name: &'a str,
    pages: u64,
    /// Rows the result card must show, as the page writes them. The values come
    /// out of the flag the writer wrote, so a grant that was not applied cannot
    /// be claimed here.
    rows: &'a [(&'a str, &'a str)],
    /// Byte runs the downloaded document must carry.
    needles: &'a [&'a str],
    /// Byte runs the downloaded document must not carry.
    absent: &'a [&'a str],
}

fn check_security_leg(leg: &Json, label: &str, spec: &SecuritySpec) -> Result<(), String> {
    if leg.get("fileRows").and_then(Json::as_u64) != Some(1) {
        return Err(format!(
            "the {label} leg did not show exactly one selected file (fileRows = {:?})",
            leg.get("fileRows")
        ));
    }
    if string_field(leg, "noUpload")? != "verified" {
        return Err(format!(
            "the {label} leg never set data-no-upload=verified; the local request check did not pass"
        ));
    }
    if string_field(leg, "runLabel")? != spec.run_label {
        return Err(format!(
            "the {label} button was labelled {:?}, expected {:?}",
            leg.get("runLabel"),
            spec.run_label
        ));
    }
    if string_field(leg, "status")? != spec.status {
        return Err(format!(
            "the {label} card said {:?}, expected {:?}",
            leg.get("status"),
            spec.status
        ));
    }

    let result = leg
        .get("result")
        .and_then(Json::as_object)
        .ok_or_else(|| format!("the {label} leg reported no result object"))?;
    if result.get("title").and_then(Json::as_str) != Some(spec.output_name) {
        return Err(format!(
            "the {label} result was named {:?}, expected {:?}",
            result.get("title"),
            spec.output_name
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
    if row_value("Verification") != "ok" {
        return Err(format!(
            "the {label} card reported Verification {:?}, expected \"ok\"",
            row_value("Verification")
        ));
    }
    for &(wanted, expected) in spec.rows {
        let value = row_value(wanted);
        if value != expected {
            return Err(format!(
                "the {label} card reported {wanted:?} {value:?}, expected {expected:?}"
            ));
        }
    }

    let outputs = leg
        .get("outputs")
        .and_then(Json::as_array)
        .ok_or_else(|| format!("the {label} run reported no output list"))?;
    if outputs.len() != 1 {
        return Err(format!(
            "the {label} run produced {} outputs; a password operation is one document in and \
             one out",
            outputs.len()
        ));
    }
    let output = outputs
        .first()
        .ok_or_else(|| format!("the {label} run produced no output"))?;
    for key in ["name", "downloadName"] {
        if output.get(key).and_then(Json::as_str) != Some(spec.output_name) {
            return Err(format!(
                "the {label} output {key} was {:?}, expected {:?}",
                output.get(key),
                spec.output_name
            ));
        }
    }
    if output.get("kind").and_then(Json::as_str) != Some("pdf") {
        return Err(format!(
            "the {label} output had kind {:?}, expected \"pdf\"",
            output.get("kind")
        ));
    }
    if output.get("header").and_then(Json::as_str) != Some("%PDF-") {
        return Err(format!(
            "the {label} output began with {:?}, expected \"%PDF-\"",
            output.get("header")
        ));
    }
    if output.get("pages").and_then(Json::as_u64) != Some(spec.pages) {
        return Err(format!(
            "the {label} output claimed {:?} pages, expected {}",
            output.get("pages"),
            spec.pages
        ));
    }
    if output.get("blob").and_then(Json::as_bool) != Some(true) {
        return Err(format!("the {label} output was not offered as a Blob URL"));
    }
    // A password operation returns a real document, so the native viewer keeps
    // its preview — and the reader can open it without leaving the page.
    if output.get("previewBlob").and_then(Json::as_bool) != Some(true) {
        return Err(format!(
            "the {label} output offered no PDF preview (preview = {:?})",
            output.get("previewBlob")
        ));
    }
    let declared = output.get("declaredBytes").and_then(Json::as_u64);
    if declared.is_none() || declared != output.get("bytes").and_then(Json::as_u64) {
        return Err(format!(
            "the {label} download was {:?} bytes while the row declared {declared:?}",
            output.get("bytes")
        ));
    }

    let found = output
        .get("found")
        .and_then(Json::as_array)
        .ok_or_else(|| format!("the {label} output reported no marker presence"))?;
    if found.len() != spec.needles.len() {
        return Err(format!(
            "the {label} output answered for {} markers, expected {}",
            found.len(),
            spec.needles.len()
        ));
    }
    let counts = output
        .get("foundCounts")
        .and_then(Json::as_array)
        .ok_or_else(|| format!("the {label} output reported no marker counts"))?;
    // The downloaded Blob is what is judged, object by object: a marker the leg
    // asked for that never appears means the card said it wrote an envelope it
    // did not write.
    for (index, seen) in found.iter().enumerate() {
        let count = counts.get(index).and_then(Json::as_u64).unwrap_or(0);
        if seen.as_bool() != Some(true) || count == 0 {
            return Err(format!(
                "the {label} download did not carry {:?} (present = {seen:?}, count = {count})",
                spec.needles.get(index).copied().unwrap_or("")
            ));
        }
    }
    let none: Vec<Json> = Vec::new();
    let absent = output
        .get("absentFound")
        .and_then(Json::as_array)
        .unwrap_or(&none);
    if absent.len() != spec.absent.len() {
        return Err(format!(
            "the {label} output answered for {} removed markers, expected {}",
            absent.len(),
            spec.absent.len()
        ));
    }
    for (index, still_there) in absent.iter().enumerate() {
        if still_there.as_bool() != Some(false) {
            return Err(format!(
                "the {label} download still carries {:?}, which this operation was meant to leave \
                 behind",
                spec.absent.get(index).copied().unwrap_or("")
            ));
        }
    }
    check_no_upload_network(leg, label)
}

/// One refusal: the card was asked to do something it must not do, and the
/// answer is a sentence and nothing else. The absence is the whole point — a
/// wrong password that produced a file would be the most damaging bug this set
/// of cards could ship with.
struct SecurityRefusal<'a> {
    run_label: &'a str,
    /// The code the engine names, so a generic failure cannot pass as a refusal.
    code: &'a str,
    /// The document the reader picked up, which the sentence must name.
    document: &'a str,
    /// What the leg typed. A refusal that repeats it puts the password on the
    /// page's status line — and a screenshot of a status line is a shared thing.
    secret: &'a str,
}

fn check_security_refusal(leg: &Json, label: &str, want: &SecurityRefusal) -> Result<(), String> {
    if leg.get("refused").and_then(Json::as_bool) != Some(true) {
        return Err(format!("the {label} leg reported no refusal"));
    }
    if string_field(leg, "runLabel")? != want.run_label {
        return Err(format!(
            "the {label} button was labelled {:?}, expected {:?}",
            leg.get("runLabel"),
            want.run_label
        ));
    }
    let status = string_field(leg, "status")?;
    if !status.contains(want.code) {
        return Err(format!(
            "the {label} refusal said {status:?}, which does not name {}",
            want.code
        ));
    }
    if !status.contains(want.document) {
        return Err(format!(
            "the {label} refusal said {status:?}, which does not name the document {:?}",
            want.document
        ));
    }
    if status.contains(want.secret) {
        return Err(format!(
            "the {label} refusal repeated the password it was given"
        ));
    }
    if leg.get("resultHidden").and_then(Json::as_bool) != Some(true) {
        return Err(format!(
            "the {label} refusal left a result panel open (resultHidden = {:?})",
            leg.get("resultHidden")
        ));
    }
    if leg.get("outputRows").and_then(Json::as_u64) != Some(0) {
        return Err(format!(
            "the {label} refusal offered {} output row(s) to download",
            leg.get("outputRows").and_then(Json::as_u64).unwrap_or(0)
        ));
    }
    // The shared witness check asks for `data-no-upload=verified`, which the page
    // sets only over a result it produced. A refusal has no result, so the honest
    // claim here is the narrower one: nothing left this origin.
    let network = leg
        .get("network")
        .and_then(Json::as_object)
        .ok_or_else(|| format!("the {label} refusal reported no network object"))?;
    let foreign = network
        .get("foreign")
        .and_then(Json::as_array)
        .ok_or_else(|| format!("the {label} refusal reported no foreign-resource list"))?;
    if !foreign.is_empty() {
        return Err(format!(
            "the {label} refusal fetched {} resource(s) from another origin: {:?}",
            foreign.len(),
            foreign.iter().filter_map(Json::as_str).collect::<Vec<_>>()
        ));
    }
    Ok(())
}

/// Protect: an envelope the reader can check, with the flags the page asked for
/// and the plain text of the page gone.
const PROTECT_LEG: SecuritySpec = SecuritySpec {
    run_label: "Protect PDF",
    status: "Done. Protected with AES-256 and verified on this device. Granted: print, copy.",
    output_name: "report-protected.pdf",
    pages: 3,
    rows: &[
        ("Encryption", "AES-256 (revision 6)"),
        ("Granted", "print, copy"),
        ("Restricted", "modify, annotate"),
        ("Document metadata", "encrypted"),
    ],
    needles: &[
        "/Encrypt",
        "/Filter /Standard",
        "/CFM /AESV3",
        "/V 5",
        "/R 6",
        "/P -44",
    ],
    absent: &["q 1 0 0 rg"],
};

/// Set permissions: the same writer, an owner password only, and a document that
/// still opens without one.
const PERMISSIONS_LEG: SecuritySpec = SecuritySpec {
    run_label: "Set permissions",
    status: "Done. Protected with AES-256 and verified on this device. Granted: print.",
    output_name: "report-protected.pdf",
    pages: 3,
    rows: &[
        ("Encryption", "AES-256 (revision 6)"),
        ("Granted", "print"),
        ("Restricted", "modify, copy, annotate"),
        ("Document metadata", "encrypted"),
    ],
    needles: &["/Encrypt", "/CFM /AESV3", "/P -60"],
    absent: &["q 1 0 0 rg"],
};

/// Unlock of a document qpdf wrote: a foreign envelope, opened on this device,
/// with every trace of the crypt filter gone.
const UNLOCK_LEG: SecuritySpec = SecuritySpec {
    run_label: "Remove password",
    status: "Done. The password is removed and the open document was verified on this device.",
    output_name: "locked-unlocked.pdf",
    pages: 3,
    rows: &[(
        "Password",
        "Removed — the output opens without one and carries no /Encrypt",
    )],
    needles: &["/Type /Pages", "/Count 3"],
    absent: &["/Encrypt", "/Filter /Standard", "/AESV3"],
};

/// Unlock of the file the protect card on this same page just wrote. The loop
/// closes here: the bytes this page encrypted open again with the password the
/// user typed.
const UNLOCK_OWN_LEG: SecuritySpec = SecuritySpec {
    run_label: "Remove password",
    status: "Done. The password is removed and the open document was verified on this device.",
    output_name: "report-protected-unlocked.pdf",
    pages: 3,
    rows: &[(
        "Password",
        "Removed — the output opens without one and carries no /Encrypt",
    )],
    needles: &["/Type /Pages", "/Count 3"],
    absent: &["/Encrypt", "/Filter /Standard", "/AESV3"],
};

/// Clear permissions: the owner password lifts the flags, the content stays
/// encrypted under the unchanged file key, and the old revision is still
/// reachable in the appended one.
const CLEAR_LEG: SecuritySpec = SecuritySpec {
    run_label: "Clear permissions",
    status: "Done. Permissions cleared and verified on this device. Now granted: modify, copy, \
             annotate.",
    output_name: "locked-permissions-cleared.pdf",
    pages: 3,
    rows: &[
        ("Now granted", "modify, copy, annotate"),
        (
            "How",
            "R5/6 incremental: /P and /Perms updated; the content stays encrypted under the \
             unchanged file key",
        ),
    ],
    needles: &["/Encrypt", "/P -1", "/Prev"],
    absent: &[],
};

/// A wrong open password is refused, and the page shows the engine's sentence.
const UNLOCK_REFUSAL: SecurityRefusal = SecurityRefusal {
    run_label: "Remove password",
    code: "WRONG_PASSWORD",
    document: "locked.pdf",
    secret: "not-the-password",
};

/// The user password opens a document; it does not change its permissions.
const CLEAR_REFUSAL: SecurityRefusal = SecurityRefusal {
    run_label: "Clear permissions",
    code: "WRONG_OWNER_PASSWORD",
    document: "locked.pdf",
    secret: "selis-open",
};

/// One signing leg that writes a document: a typed stamp, a certificate
/// signature, or an approval block. The rows are the card's own account of what
/// it appended, and the needles are checked against the bytes the browser
/// actually downloaded, so a card that claimed a signature it did not write
/// fails here.
struct SignDocumentSpec<'a> {
    run_label: &'a str,
    status: &'a str,
    output_name: &'a str,
    pages: u64,
    rows: &'a [(&'a str, &'a str)],
    needles: &'a [&'a str],
    absent: &'a [&'a str],
}

fn check_sign_document(leg: &Json, label: &str, spec: &SignDocumentSpec) -> Result<(), String> {
    if leg.get("fileRows").and_then(Json::as_u64) != Some(1) {
        return Err(format!(
            "the {label} leg did not show exactly one selected file (fileRows = {:?})",
            leg.get("fileRows")
        ));
    }
    if string_field(leg, "runLabel")? != spec.run_label {
        return Err(format!(
            "the {label} button was labelled {:?}, expected {:?}",
            leg.get("runLabel"),
            spec.run_label
        ));
    }
    if string_field(leg, "status")? != spec.status {
        return Err(format!(
            "the {label} card said {:?}, expected {:?}",
            leg.get("status"),
            spec.status
        ));
    }

    let result = leg
        .get("result")
        .and_then(Json::as_object)
        .ok_or_else(|| format!("the {label} leg reported no result object"))?;
    if result.get("title").and_then(Json::as_str) != Some(spec.output_name) {
        return Err(format!(
            "the {label} result was named {:?}, expected {:?}",
            result.get("title"),
            spec.output_name
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
    if row_value("Verification") != "ok" {
        return Err(format!(
            "the {label} card reported Verification {:?}, expected \"ok\"",
            row_value("Verification")
        ));
    }
    for &(wanted, expected) in spec.rows {
        let value = row_value(wanted);
        if value != expected {
            return Err(format!(
                "the {label} card reported {wanted:?} {value:?}, expected {expected:?}"
            ));
        }
    }

    let outputs = leg
        .get("outputs")
        .and_then(Json::as_array)
        .ok_or_else(|| format!("the {label} run reported no output list"))?;
    if outputs.len() != 1 {
        return Err(format!(
            "the {label} run produced {} outputs; signing is one document in and one out",
            outputs.len()
        ));
    }
    let output = outputs
        .first()
        .ok_or_else(|| format!("the {label} run produced no output"))?;
    for key in ["name", "downloadName"] {
        if output.get(key).and_then(Json::as_str) != Some(spec.output_name) {
            return Err(format!(
                "the {label} output {key} was {:?}, expected {:?}",
                output.get(key),
                spec.output_name
            ));
        }
    }
    if output.get("kind").and_then(Json::as_str) != Some("pdf") {
        return Err(format!(
            "the {label} output had kind {:?}, expected \"pdf\"",
            output.get("kind")
        ));
    }
    if output.get("header").and_then(Json::as_str) != Some("%PDF-") {
        return Err(format!(
            "the {label} output began with {:?}, expected \"%PDF-\"",
            output.get("header")
        ));
    }
    if output.get("pages").and_then(Json::as_u64) != Some(spec.pages) {
        return Err(format!(
            "the {label} output claimed {:?} pages, expected {}",
            output.get("pages"),
            spec.pages
        ));
    }
    if output.get("blob").and_then(Json::as_bool) != Some(true) {
        return Err(format!("the {label} output was not offered as a Blob URL"));
    }
    if output.get("previewBlob").and_then(Json::as_bool) != Some(true) {
        return Err(format!(
            "the {label} output offered no PDF preview (preview = {:?})",
            output.get("previewBlob")
        ));
    }
    let declared = output.get("declaredBytes").and_then(Json::as_u64);
    if declared.is_none() || declared != output.get("bytes").and_then(Json::as_u64) {
        return Err(format!(
            "the {label} download was {:?} bytes while the row declared {declared:?}",
            output.get("bytes")
        ));
    }

    let found = output
        .get("found")
        .and_then(Json::as_array)
        .ok_or_else(|| format!("the {label} output reported no marker presence"))?;
    if found.len() != spec.needles.len() {
        return Err(format!(
            "the {label} output answered for {} markers, expected {}",
            found.len(),
            spec.needles.len()
        ));
    }
    for (index, seen) in found.iter().enumerate() {
        if seen.as_bool() != Some(true) {
            return Err(format!(
                "the {label} download did not carry {:?}",
                spec.needles.get(index).copied().unwrap_or("")
            ));
        }
    }
    let none: Vec<Json> = Vec::new();
    let absent = output
        .get("absentFound")
        .and_then(Json::as_array)
        .unwrap_or(&none);
    if absent.len() != spec.absent.len() {
        return Err(format!(
            "the {label} output answered for {} removed markers, expected {}",
            absent.len(),
            spec.absent.len()
        ));
    }
    for (index, still_there) in absent.iter().enumerate() {
        if still_there.as_bool() != Some(false) {
            return Err(format!(
                "the {label} download still carries {:?}, which this operation was meant to leave \
                 behind",
                spec.absent.get(index).copied().unwrap_or("")
            ));
        }
    }
    check_no_upload_network(leg, label)
}

/// One signature report. It downloads nothing: the rows ARE the result, and the
/// claim is that the bytes this page wrote a moment ago verify and that their
/// chain ends at the root the reader supplied.
struct SignatureReportSpec<'a> {
    status: &'a str,
    meta: &'a str,
    rows: &'a [(&'a str, &'a str)],
}

fn check_signature_report(
    leg: &Json,
    label: &str,
    spec: &SignatureReportSpec,
) -> Result<(), String> {
    if leg.get("fileRows").and_then(Json::as_u64) != Some(1) {
        return Err(format!(
            "the {label} leg did not show exactly one selected file (fileRows = {:?})",
            leg.get("fileRows")
        ));
    }
    if string_field(leg, "runLabel")? != "Validate signatures" {
        return Err(format!(
            "the {label} button was labelled {:?}, expected \"Validate signatures\"",
            leg.get("runLabel")
        ));
    }
    if string_field(leg, "status")? != spec.status {
        return Err(format!(
            "the {label} card said {:?}, expected {:?}",
            leg.get("status"),
            spec.status
        ));
    }
    let result = leg
        .get("result")
        .and_then(Json::as_object)
        .ok_or_else(|| format!("the {label} leg reported no report card"))?;
    if result.get("title").and_then(Json::as_str) != Some("Signature report") {
        return Err(format!(
            "the {label} report was titled {:?}, expected \"Signature report\"",
            result.get("title")
        ));
    }
    if result.get("meta").and_then(Json::as_str) != Some(spec.meta) {
        return Err(format!(
            "the {label} report meta was {:?}, expected {:?}",
            result.get("meta"),
            spec.meta
        ));
    }
    let outputs = leg
        .get("outputs")
        .and_then(Json::as_array)
        .ok_or_else(|| format!("the {label} run reported no output list"))?;
    if !outputs.is_empty() {
        return Err(format!(
            "the {label} report offered {} download(s); a report is not a document",
            outputs.len()
        ));
    }
    let rows = result
        .get("rows")
        .and_then(Json::as_array)
        .ok_or_else(|| format!("the {label} report had no rows"))?;
    let row_value = |wanted: &str| {
        rows.iter()
            .find(|row| row.get("label").and_then(Json::as_str) == Some(wanted))
            .and_then(|row| row.get("value"))
            .and_then(Json::as_str)
            .unwrap_or("")
    };
    for &(wanted, expected) in spec.rows {
        let value = row_value(wanted);
        if value != expected {
            return Err(format!(
                "the {label} report reported {wanted:?} {value:?}, expected {expected:?}"
            ));
        }
    }
    check_no_upload_network(leg, label)
}

/// A typed stamp: the page's own text is drawn inside the signature rectangle
/// and no signature dictionary is written at all.
const STAMP_LEG: SignDocumentSpec = SignDocumentSpec {
    run_label: "Sign PDF",
    status: "Done. The stamp was appended and verified on this device.",
    output_name: "report-stamped.pdf",
    pages: 3,
    rows: &[("Page", "2"), ("Stamp", "2 line(s), 0 stroke(s)")],
    needles: &["/SelisSigF", "(Browser Approved) Tj", "(Ada Lovelace"],
    absent: &["/ByteRange"],
};

/// A certificate signature: a real `/Sig` dictionary around a detached CMS blob
/// and the chain the reader selected. The `/M`, `/Name` and `/Reason` values are
/// the ones the harness typed, so an option that was dropped fails here.
const CERTIFICATE_LEG: SignDocumentSpec = SignDocumentSpec {
    run_label: "Sign PDF",
    status: "Done. The signature was written and verified on this device.",
    output_name: "report-signed.pdf",
    pages: 3,
    rows: &[
        ("Page", "1"),
        ("Signature field", "Signature1"),
        ("Sub-filter", "adbe.pkcs7.detached"),
    ],
    needles: &[
        "/Type /Sig",
        "/Filter /Adobe.PPKLite",
        "/SubFilter /adbe.pkcs7.detached",
        "/ByteRange [",
        "/Name (Ada Lovelace)",
        "/Reason (Browser harness approval)",
        "/Location (Local device)",
    ],
    absent: &[],
};

/// The approval card: a decision block appended as a visible stamp, with the
/// sentence the page writes about where the decision is kept.
const WORKFLOW_LEG: SignDocumentSpec = SignDocumentSpec {
    run_label: "Record approval",
    status: "Done. The decision was appended and verified on this device.",
    output_name: "report-stamped.pdf",
    pages: 3,
    rows: &[
        ("Decision", "APPROVED"),
        ("Approver", "Grace Hopper"),
        ("Note", "Looks good"),
        (
            "Workflow state",
            "recorded in the appended block; no server holds a copy",
        ),
        ("Page", "3"),
        ("Stamp", "3 line(s), 0 stroke(s)"),
    ],
    needles: &["/SelisSigF", "(APPROVED) Tj", "(Grace Hopper"],
    absent: &[],
};

/// The report over the file the certificate leg just wrote, checked against the
/// root the chain ends at. Every row is a fact the report had to establish by
/// verifying the bytes, not a label the page could print on its own.
const SIGNATURE_REPORT_LEG: SignatureReportSpec = SignatureReportSpec {
    status: "Done. 1 of 1 signature verified on this device.",
    meta: "1 signature \u{b7} 1 anchor \u{b7} checked locally",
    rows: &[
        ("Signature 1", "Signature1: verified"),
        ("Signer 1", "Ada Lovelace"),
        ("Coverage 1", "the whole document"),
        (
            "Trust 1",
            "trusted \u{2014} the chain ends at a supplied anchor (Selis Test Signer)",
        ),
        (
            "Revocation",
            "not checked \u{2014} this build has no network, so no CRL or OCSP response was fetched",
        ),
    ],
};

/// Every card `/tools.html` renders, sorted — the list the browser check holds
/// itself to. A new card has to be added here and given a leg, not merely
/// rendered.
const TOOLS_PAGE_CARDS: [&str; 47] = [
    "attach",
    "attachments",
    "batch",
    "bates",
    "certificate",
    "clear_permissions",
    "compress",
    "convert",
    "crop",
    "delete",
    "detach",
    "edit_text",
    "fill_forms",
    "fingerprint",
    "flip",
    "header_footer",
    "html",
    "image_to_pdf",
    "image_watermark",
    "invert",
    "jpg",
    "merge",
    "metadata",
    "metadata_report",
    "mix",
    "nup",
    "outline",
    "page_labels",
    "page_numbers",
    "permissions",
    "png",
    "privacy",
    "protect",
    "reorder",
    "repair",
    "resize",
    "rotate",
    "sign",
    "split",
    "strip_metadata",
    "text",
    "tiff",
    "unlock",
    "watermark",
    "webp",
    "workflow",
    "zip",
];

/// The generated pages this check opens by URL (plan 36 §B5). Only these three
/// slugs are hard-coded here; every other page is covered by the manifest check
/// below. A rename in `tool-registry.ts` that this file does not follow fails
/// the run instead of silently testing an alias.
const TOOLS_PAGE_BY_URL: [&str; 3] = ["merge-pdf", "unlock-pdf", "sign-pdf"];

/// The gate the roadmap words as "the browser check drives every card online and
/// after an offline reload". A card that renders and is never opened is an
/// untested card, so the driver names every id in the grid and records the ids
/// it clicked in each phase; both lists must equal the grid exactly.
fn check_card_coverage(report: &Json) -> Result<(), String> {
    let names = |array: &[Json]| -> Vec<String> {
        array
            .iter()
            .filter_map(Json::as_str)
            .map(str::to_string)
            .collect()
    };
    let grid = names(
        report
            .get("cardIds")
            .and_then(Json::as_array)
            .ok_or_else(|| "the page reported no card-id list (cardIds = None)".to_string())?,
    );
    let expected: Vec<String> = TOOLS_PAGE_CARDS
        .iter()
        .map(|card| (*card).to_string())
        .collect();
    if grid != expected {
        return Err(format!(
            "the tools grid held a different set of cards than the check covers: got {grid:?}, expected {expected:?}"
        ));
    }
    let clicked = report
        .get("clicked")
        .and_then(Json::as_object)
        .ok_or_else(|| "the driver reported no card-click record (clicked = None)".to_string())?;
    for phase in ["online", "offline"] {
        let driven = names(
            clicked
                .get(phase)
                .and_then(Json::as_array)
                .ok_or_else(|| format!("the driver reported no {phase} click list"))?,
        );
        let missing: Vec<&str> = grid
            .iter()
            .filter(|card| !driven.iter().any(|driven| driven == *card))
            .map(String::as_str)
            .collect();
        if !missing.is_empty() {
            return Err(format!(
                "the {phase} run never opened {} of {} cards: {:?}",
                missing.len(),
                grid.len(),
                missing
            ));
        }
    }
    Ok(())
}

/// The generated page's own identity: the slug it declares, the path it was
/// served at, the canonical URL, a non-empty heading and the absence of the
/// card grid. A page that carries the grid is `/tools.html` wearing a slug.
fn check_tool_page_identity(page: &Json, slug: &str, label: &str) -> Result<(), String> {
    if string_field(page, "slug")? != slug {
        return Err(format!(
            "the {label} page declared data-tool={:?}, expected {slug:?}",
            page.get("slug")
        ));
    }
    let path = format!("/{slug}/");
    if string_field(page, "path")? != path {
        return Err(format!(
            "the {label} page was served at {:?}, expected {path:?}",
            page.get("path")
        ));
    }
    let canonical = string_field(page, "canonical")?;
    let canonical_path = format!("/{slug}/");
    if !canonical.ends_with(&canonical_path) {
        return Err(format!(
            "the {label} page canonicalised to {canonical:?}, expected a URL ending {canonical_path:?}"
        ));
    }
    if string_field(page, "heading")?.trim().is_empty() {
        return Err(format!(
            "the {label} page had no workspace heading; a generated page must name its tool"
        ));
    }
    if page.get("hasGrid").and_then(Json::as_bool) != Some(false) {
        return Err(format!(
            "the {label} page carried the card grid (hasGrid = {:?})",
            page.get("hasGrid")
        ));
    }
    Ok(())
}

/// `pages.json` against the grid: one generated page per registry entry, and no
/// registry entry without a page. The driver fetches every slug the manifest
/// lists, so a build that dropped one file fails here.
fn check_tool_page_manifest(report: &Json) -> Result<(), String> {
    let pages = report
        .get("pages")
        .ok_or_else(|| "the page reported no generated-page manifest (pages = None)".to_string())?;
    if pages.get("count").and_then(Json::as_u64) != Some(TOOLS_PAGE_CARDS.len() as u64) {
        return Err(format!(
            "pages.json listed {:?} pages, expected {}",
            pages.get("count"),
            TOOLS_PAGE_CARDS.len()
        ));
    }
    let missing: Vec<&str> = pages
        .get("missing")
        .and_then(Json::as_array)
        .ok_or_else(|| "the page reported no missing-page list".to_string())?
        .iter()
        .filter_map(Json::as_str)
        .collect();
    if !missing.is_empty() {
        return Err(format!(
            "{} generated page(s) in pages.json could not be fetched: {missing:?}",
            missing.len()
        ));
    }
    let ids: Vec<String> = pages
        .get("ids")
        .and_then(Json::as_array)
        .ok_or_else(|| "the page reported no manifest id list".to_string())?
        .iter()
        .filter_map(Json::as_str)
        .map(str::to_string)
        .collect();
    let expected: Vec<String> = TOOLS_PAGE_CARDS
        .iter()
        .map(|card| (*card).to_string())
        .collect();
    if ids != expected {
        return Err(format!(
            "pages.json does not describe the rendered cards: got {ids:?}, expected {expected:?}"
        ));
    }
    Ok(())
}

/// One merge leg opened from a generated page: two files in, one `%PDF-` out,
/// with the page's own no-upload assertion still in force.
fn check_by_url_merge(leg: &Json, label: &str) -> Result<(), String> {
    if leg.get("fileRows").and_then(Json::as_u64) != Some(2) {
        return Err(format!(
            "the {label} leg did not show two selected PDFs (fileRows = {:?})",
            leg.get("fileRows")
        ));
    }
    if string_field(leg, "runLabel")? != "Merge PDFs" {
        return Err(format!(
            "the {label} button was labelled {:?}, expected \"Merge PDFs\"",
            leg.get("runLabel")
        ));
    }
    if string_field(leg, "noUpload")? != "verified" {
        return Err(format!(
            "the {label} leg never set data-no-upload=verified; the local request check did not pass"
        ));
    }
    let result = leg
        .get("result")
        .and_then(Json::as_object)
        .ok_or_else(|| format!("the {label} leg reported no result object"))?;
    if result.get("title").and_then(Json::as_str) != Some("merged.pdf") {
        return Err(format!(
            "the {label} result was named {:?}, expected \"merged.pdf\"",
            result.get("title")
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
    if row_value("Pages") != "2 \u{2192} 2" {
        return Err(format!(
            "the {label} merge reported Pages {:?}, expected \"2 \u{2192} 2\"",
            row_value("Pages")
        ));
    }
    if row_value("Verification") != "ok" {
        return Err(format!(
            "the {label} result did not report a passing structural verification (got {:?})",
            row_value("Verification")
        ));
    }
    let output = leg
        .get("output")
        .and_then(Json::as_object)
        .ok_or_else(|| format!("the {label} leg reported no output object"))?;
    if output.get("blob").and_then(Json::as_bool) != Some(true) {
        return Err(format!("the {label} result download was not a Blob URL"));
    }
    if output.get("header").and_then(Json::as_str) != Some("%PDF-") {
        return Err(format!(
            "the {label} downloaded bytes did not begin with %PDF- (header = {:?})",
            output.get("header")
        ));
    }
    if output.get("bytes").and_then(Json::as_u64).unwrap_or(0) <= 500 {
        return Err(format!(
            "the {label} downloaded PDF is only {:?} bytes; two 423-byte inputs should produce more",
            output.get("bytes")
        ));
    }
    Ok(())
}

/// B5's by-URL legs: three generated pages opened without a click online, one
/// of them again with the origin cut, and an unknown path that must not become
/// a cached tool page.
fn check_by_url_legs(report: &Json) -> Result<(), String> {
    let by_url = report
        .get("byUrl")
        .ok_or_else(|| "the driver reported no by-URL legs (byUrl = None)".to_string())?;
    let online = by_url
        .get("online")
        .ok_or_else(|| "the driver reported no online by-URL legs".to_string())?;

    let merge = online
        .get("merge")
        .ok_or_else(|| "the driver never opened merge-pdf by URL".to_string())?;
    check_tool_page_identity(
        merge
            .get("page")
            .ok_or_else(|| "the merge-pdf leg reported no page identity".to_string())?,
        TOOLS_PAGE_BY_URL[0],
        "online merge-pdf",
    )?;
    check_by_url_merge(merge, "online merge-pdf")?;
    // The generated-page accessibility gate: the axe-core scan of the
    // merge-pdf page, judged exactly like app-shell's (an unrun scan is a
    // failure, and "axe-core clean" means zero violations).
    assert_accessibility(merge).map_err(|why| format!("the merge-pdf page scan: {why}"))?;

    let unlock = online
        .get("unlock")
        .ok_or_else(|| "the driver never opened unlock-pdf by URL".to_string())?;
    check_tool_page_identity(
        unlock
            .get("page")
            .ok_or_else(|| "the unlock-pdf leg reported no page identity".to_string())?,
        TOOLS_PAGE_BY_URL[1],
        "online unlock-pdf",
    )?;
    check_security_leg(unlock, "online unlock-pdf", &UNLOCK_LEG)?;

    let sign = online
        .get("sign")
        .ok_or_else(|| "the driver never opened sign-pdf by URL".to_string())?;
    check_tool_page_identity(
        sign.get("page")
            .ok_or_else(|| "the sign-pdf leg reported no page identity".to_string())?,
        TOOLS_PAGE_BY_URL[2],
        "online sign-pdf",
    )?;
    check_sign_document(sign, "online sign-pdf", &STAMP_LEG)?;

    let offline = by_url
        .get("offline")
        .ok_or_else(|| "the driver reported no offline by-URL legs".to_string())?;
    let offline_merge = offline
        .get("merge")
        .ok_or_else(|| "the driver never opened a generated page offline".to_string())?;
    check_tool_page_identity(
        offline_merge
            .get("page")
            .ok_or_else(|| "the offline by-URL leg reported no page identity".to_string())?,
        TOOLS_PAGE_BY_URL[0],
        "offline merge-pdf",
    )?;
    check_by_url_merge(offline_merge, "offline merge-pdf")?;

    let unknown = offline
        .get("unknown")
        .ok_or_else(|| "the driver never tried an unknown path offline".to_string())?;
    if unknown.get("hasGrid").and_then(Json::as_bool) != Some(false) {
        return Err(format!(
            "an unknown offline path was answered with the tools grid (hasGrid = {:?})",
            unknown.get("hasGrid")
        ));
    }
    if unknown.get("slug").is_some_and(|slug| !slug.is_null()) {
        return Err(format!(
            "an unknown offline path was answered with the tool page {:?}",
            unknown.get("slug")
        ));
    }
    // Offline here is the staging server's 503, not a dead socket, so the
    // worker's `offlineFallback()` HTML is only one of the two honest
    // answers: the other's body is the server's own "offline" text, which
    // still proves no cached tool page was served. A blank error page
    // matches neither.
    let heading = string_field(unknown, "heading")?;
    let body = string_field(unknown, "body")?;
    if heading != "Not available offline yet" && body != "offline" {
        return Err(format!(
            "an unknown offline path was answered with heading {:?} and body {:?}, not the worker's offline fallback or the staging server's offline 503",
            unknown.get("heading"),
            unknown.get("body")
        ));
    }
    Ok(())
}

/// The batch queue's promise, measured on the page: three picked files, two
/// verified downloads named after their own documents, one row saying which
/// file was skipped and why, and a ZIP holding both results.
///
/// The page reads its rows from the guest's own per-file report, so the two
/// different page counts below (3 and 1) are the evidence that a queue does
/// not average or borrow a neighbour's structure.
fn check_batch_leg(leg: &Json, label: &str) -> Result<(), String> {
    if leg.get("fileRows").and_then(Json::as_u64) != Some(3) {
        return Err(format!(
            "the {label} leg did not show three selected files (fileRows = {:?})",
            leg.get("fileRows")
        ));
    }
    if string_field(leg, "noUpload")? != "verified" {
        return Err(format!(
            "the {label} leg never set data-no-upload=verified; the local request check did not pass"
        ));
    }
    if string_field(leg, "runLabel")? != "Run the queue" {
        return Err(format!(
            "the {label} button was labelled {:?}, expected \"Run the queue\"",
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
    if result.get("title").and_then(Json::as_str) != Some("2 PDFs and a ZIP") {
        return Err(format!(
            "the {label} result was named {:?}, expected \"2 PDFs and a ZIP\"",
            result.get("title")
        ));
    }
    let meta = result.get("meta").and_then(Json::as_str).unwrap_or("");
    if !meta.contains("3 files") || !meta.contains("verified locally") {
        return Err(format!(
            "the {label} summary did not describe three locally verified files (meta = {meta:?})"
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
    if row_value("Queue") != "compress \u{b7} 2 of 3 files \u{b7} 1 not processed" {
        return Err(format!(
            "the {label} card reported the Queue as {:?}, expected \"compress \u{b7} 2 of 3 files \u{b7} 1 not processed\"",
            row_value("Queue")
        ));
    }
    // One row per picked file, each naming only its own document's result.
    if row_value("alpha.pdf") != "alpha-compressed.pdf \u{b7} 3 pages \u{b7} ok" {
        return Err(format!(
            "the {label} card reported alpha.pdf as {:?}, expected its own 3-page result",
            row_value("alpha.pdf")
        ));
    }
    if row_value("beta.pdf") != "beta-compressed.pdf \u{b7} 1 page \u{b7} ok" {
        return Err(format!(
            "the {label} card reported beta.pdf as {:?}, expected its own 1-page result",
            row_value("beta.pdf")
        ));
    }
    let skipped = row_value("broken.pdf");
    if !skipped.starts_with("not processed \u{2014} ") {
        return Err(format!(
            "the {label} card did not say the failed file was skipped with a reason (row = {skipped:?})"
        ));
    }
    // The queue's own counts: four pages in, four pages out, and a verdict that
    // admits one file never ran.
    if row_value("Pages") != "4 \u{2192} 4" {
        return Err(format!(
            "the {label} run reported Pages {:?}, expected \"4 \u{2192} 4\"",
            row_value("Pages")
        ));
    }
    if row_value("Verification") != "partial" {
        return Err(format!(
            "the {label} run reported Verification {:?}, expected the honest \"partial\" verdict",
            row_value("Verification")
        ));
    }

    let outputs = leg
        .get("outputs")
        .and_then(Json::as_array)
        .ok_or_else(|| format!("the {label} run reported no output list"))?;
    if outputs.len() != 3 {
        return Err(format!(
            "the {label} run produced {} outputs, expected two PDFs and the bundle",
            outputs.len()
        ));
    }
    let pdf_names: Vec<&str> = outputs
        .iter()
        .filter(|output| output.get("kind").and_then(Json::as_str) == Some("pdf"))
        .filter_map(|output| output.get("name").and_then(Json::as_str))
        .collect();
    if pdf_names != ["alpha-compressed.pdf", "beta-compressed.pdf"] {
        return Err(format!(
            "the {label} downloads were named {pdf_names:?}, expected one result per surviving file"
        ));
    }
    for (index, expected_pages) in [(0usize, 3u64), (1, 1)] {
        let output = outputs
            .get(index)
            .ok_or_else(|| format!("the {label} run produced no output at {index}"))?;
        if output.get("pages").and_then(Json::as_u64) != Some(expected_pages) {
            return Err(format!(
                "the {label} output {index} reported {:?} pages, expected {expected_pages}",
                output.get("pages")
            ));
        }
        if output.get("header").and_then(Json::as_str) != Some("%PDF-") {
            return Err(format!(
                "the {label} output {index} did not begin with %PDF- (header = {:?})",
                output.get("header")
            ));
        }
        if output.get("blob").and_then(Json::as_bool) != Some(true) {
            return Err(format!(
                "the {label} output {index} was not offered as a Blob URL"
            ));
        }
    }
    let bundle = outputs
        .get(2)
        .ok_or_else(|| format!("the {label} run produced no bundle"))?;
    if bundle.get("kind").and_then(Json::as_str) != Some("zip")
        || bundle.get("name").and_then(Json::as_str) != Some("alpha-batch.zip")
        || bundle.get("zip").and_then(Json::as_bool) != Some(true)
    {
        return Err(format!(
            "the {label} bundle was {:?}, expected a real ZIP named alpha-batch.zip",
            bundle.get("name")
        ));
    }
    // Both survivors are inside the bundle: the page-tree marker occurs twice
    // in the stored (uncompressed) archive and once in each single-document
    // download, so the count also proves the queue did not write one file twice.
    for (index, expected) in [(0usize, 1u64), (1, 1), (2, 2)] {
        let output = outputs
            .get(index)
            .ok_or_else(|| format!("the {label} run produced no output at {index}"))?;
        let counts = output
            .get("foundCounts")
            .and_then(Json::as_array)
            .ok_or_else(|| format!("the {label} output {index} reported no needle counts"))?;
        if counts.first().and_then(Json::as_u64) != Some(expected) {
            return Err(format!(
                "the {label} output {index} held {counts:?}, expected a page tree {expected} time(s)"
            ));
        }
        let absent = output
            .get("absentFound")
            .and_then(Json::as_array)
            .ok_or_else(|| format!("the {label} output {index} reported no absence checks"))?;
        if absent.len() != 1 || absent.iter().any(|item| item.as_bool() != Some(false)) {
            return Err(format!(
                "the {label} output {index} carries the bytes of the file that failed (absentFound = {absent:?})"
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

/// `tools-page` (SL-4.TOOLS.02 + SL-4.TOOLS.05 + the Phase 1A.1 split and
/// page-organization modes): the real `/tools.html` merges and mixes two PDFs,
/// splits a three-page PDF one page at a time, duplicates a page, and inserts a
/// blank page online, then the harness cuts the origin, reloads the same page,
/// and repeats those runs with every byte coming from the worker's cache.
///
/// Every claim is read from the page the user gets, not from a helper:
///
/// * thirty cards render, and choosing Merge opens the workspace;
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
/// * the crop, flip, resize, and N-up legs rewrite page layout, the page-label
///   leg writes an explicit `/PageLabels` tree whose prefix is visible in the
///   real Blob, the resize leg writes the A4 `/MediaBox` into the bytes, the
///   N-up leg places three pages on two sheets through Form XObjects, and the
///   metadata-strip leg proves `/Info` and its values are absent from the bytes;
/// * the attachment leg downloads both embedded files with their decoded bytes
///   and a ZIP, without offering either file as a PDF preview;
/// * the metadata-report leg exports one JSON file for the same fixture the
///   strip leg rewrites, and the report's rows, its read-only `read` verdict,
///   and its downloaded bytes agree — and no PDF is offered, because none was
///   made;
/// * the batch leg queues three files, one of them a PDF whose index the engine
///   cannot read, and shows the two survivors as their own downloads with their
///   own page counts, the skipped file as its own row with its reason, and a ZIP
///   whose bytes hold both;
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
    if report.get("cards").and_then(Json::as_u64) != Some(TOOLS_PAGE_CARDS.len() as u64) {
        return Err(format!(
            "the tools grid did not render all {} cards (cards = {:?})",
            TOOLS_PAGE_CARDS.len(),
            report.get("cards")
        ));
    }
    check_card_coverage(report)?;
    check_tool_page_manifest(report)?;
    check_by_url_legs(report)?;
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
    let watermark_tiled = report.get("watermarkTiled").ok_or_else(|| {
        "the page reported no tiled-watermark leg (watermarkTiled = None)".to_string()
    })?;
    check_tiled_watermark_leg(watermark_tiled, "online tiled watermark")?;
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
    let compress_lossy = report.get("compressLossy").ok_or_else(|| {
        "the page reported no compressLossy leg (compressLossy = None)".to_string()
    })?;
    check_lossy_compress_leg(compress_lossy, "online compress lossy", false)?;
    let compress_over_target = report.get("compressOverTarget").ok_or_else(|| {
        "the page reported no compressOverTarget leg (compressOverTarget = None)".to_string()
    })?;
    check_lossy_compress_leg(compress_over_target, "online compress over target", true)?;
    let crop = report
        .get("crop")
        .ok_or_else(|| "the page reported no crop leg (crop = None)".to_string())?;
    check_stamp_leg(
        crop,
        "online crop",
        "report-cropped.pdf",
        "Crop PDF",
        3,
        &[],
    )?;
    let flip = report
        .get("flip")
        .ok_or_else(|| "the page reported no flip leg (flip = None)".to_string())?;
    check_stamp_leg(
        flip,
        "online flip",
        "report-flipped.pdf",
        "Flip PDF",
        3,
        &[],
    )?;
    let page_labels = report
        .get("pageLabels")
        .ok_or_else(|| "the page reported no page-label leg (pageLabels = None)".to_string())?;
    check_stamp_leg(
        page_labels,
        "online page labels",
        "report-labeled.pdf",
        "Apply page labels",
        3,
        &["/PageLabels", "(A-)"],
    )?;
    let resize = report
        .get("resize")
        .ok_or_else(|| "the page reported no resize leg (resize = None)".to_string())?;
    check_stamp_leg(
        resize,
        "online resize",
        "report-resized.pdf",
        "Resize pages",
        3,
        &["595.28", "841.89"],
    )?;
    let nup = report
        .get("nup")
        .ok_or_else(|| "the page reported no N-up leg (nup = None)".to_string())?;
    check_page_count_leg(
        nup,
        "online N-up",
        "report-n-up.pdf",
        "Place pages",
        3,
        2,
        &["/Subtype /Form", "/Type /XObject"],
    )?;
    let attachments = report.get("attachments").ok_or_else(|| {
        "the page reported no attachment-extraction leg (attachments = None)".to_string()
    })?;
    check_attachments_leg(
        attachments,
        "online attachments",
        &[("notes.txt", "hello attachment"), ("evil.txt", "safe name")],
    )?;
    let attach = report
        .get("attach")
        .ok_or_else(|| "the page reported no attach leg (attach = None)".to_string())?;
    check_attach_leg(attach, "online attach")?;
    let detach = report
        .get("detach")
        .ok_or_else(|| "the page reported no detach leg (detach = None)".to_string())?;
    check_detach_leg(detach, "online detach")?;
    let repair = report
        .get("repair")
        .ok_or_else(|| "the page reported no repair leg (repair = None)".to_string())?;
    check_repair_leg(repair, "online repair")?;
    check_single_output_leg(
        leg_of(report, "rotate")?,
        &SingleLeg {
            label: "online rotate",
            run_label: "Rotate PDF",
            output_name: "report-rotated.pdf",
            file_rows: 1,
            zip: false,
            pages: 3,
            pages_row: Some("3 \u{2192} 3"),
            found_counts: &[3],
        },
    )?;
    check_single_output_leg(
        leg_of(report, "delete")?,
        &SingleLeg {
            label: "online delete pages",
            run_label: "Delete pages",
            output_name: "report-trimmed.pdf",
            file_rows: 1,
            zip: false,
            pages: 2,
            pages_row: Some("3 \u{2192} 2"),
            found_counts: &[1, 1],
        },
    )?;
    check_single_output_leg(
        leg_of(report, "metadata")?,
        &SingleLeg {
            label: "online edit metadata",
            run_label: "Save metadata",
            output_name: "metadata-metadata.pdf",
            file_rows: 1,
            zip: false,
            pages: 3,
            pages_row: Some("3 \u{2192} 3"),
            found_counts: &[1, 1],
        },
    )?;
    check_single_output_leg(
        leg_of(report, "zip")?,
        &SingleLeg {
            label: "online zip",
            run_label: "Create ZIP",
            output_name: "one.zip",
            file_rows: 2,
            zip: true,
            pages: 0,
            pages_row: None,
            found_counts: &[1, 2, 2],
        },
    )?;
    let batch = report
        .get("batch")
        .ok_or_else(|| "the page reported no batch-queue leg (batch = None)".to_string())?;
    check_batch_leg(batch, "online batch queue")?;
    let strip_metadata = report.get("stripMetadata").ok_or_else(|| {
        "the page reported no metadata-strip leg (stripMetadata = None)".to_string()
    })?;
    check_strip_metadata_leg(strip_metadata, "online strip metadata")?;
    let metadata_report = report.get("metadataReport").ok_or_else(|| {
        "the page reported no metadata-report leg (metadataReport = None)".to_string()
    })?;
    check_metadata_report_leg(metadata_report, "online metadata report")?;
    let image_to_pdf = report
        .get("imageToPdf")
        .ok_or_else(|| "the page reported no image-to-PDF leg (imageToPdf = None)".to_string())?;
    check_image_leg(image_to_pdf, "online image to PDF")?;
    let image_watermark = report.get("imageWatermark").ok_or_else(|| {
        "the page reported no image-watermark leg (imageWatermark = None)".to_string()
    })?;
    check_image_watermark_leg(image_watermark, "online image watermark")?;
    let outline = report
        .get("outline")
        .ok_or_else(|| "the page reported no outline leg (outline = None)".to_string())?;
    check_outline_leg(outline, "online outline")?;
    let convert = report
        .get("convert")
        .ok_or_else(|| "the page reported no convert leg (convert = None)".to_string())?;
    check_convert_leg(
        convert,
        "online convert",
        "notes-converted.pdf",
        &CONVERT_NEEDLES,
        1,
    )?;
    for (key, title, needles, pages) in [
        ("latex", "quarto-converted.pdf", &LATEX_NEEDLES[..], 1),
        ("csv", "ledger-converted.pdf", &CSV_NEEDLES[..], 1),
        ("svg", "mark-converted.pdf", &SVG_NEEDLES[..], 1),
        ("epub", "book-converted.pdf", &EPUB_NEEDLES[..], 1),
        ("table", "table-converted.pdf", &TABLE_NEEDLES[..], 1),
        ("docx", "letter-converted.pdf", &DOCX_NEEDLES[..], 1),
        ("pptx", "deck-converted.pdf", &PPTX_NEEDLES[..], 2),
        ("xlsx", "book2-converted.pdf", &XLSX_NEEDLES[..], 2),
    ] {
        let leg = report
            .get(key)
            .ok_or_else(|| format!("the page reported no {key} leg ({key} = None)"))?;
        check_convert_leg(leg, &format!("online {key}"), title, needles, pages)?;
    }
    let tables = report
        .get("tables")
        .ok_or_else(|| "the page reported no tables leg (tables = None)".to_string())?;
    check_tables_leg(tables, "online tables")?;
    let split = report
        .get("splitText")
        .ok_or_else(|| "the page reported no split leg (splitText = None)".to_string())?;
    check_text_split_leg(split, "online split")?;
    let reflow = report
        .get("reflow")
        .ok_or_else(|| "the page reported no reflow leg (reflow = None)".to_string())?;
    check_reflow_leg(reflow, "online reflow")?;
    let epub_out = report
        .get("epubOut")
        .ok_or_else(|| "the page reported no EPUB leg (epubOut = None)".to_string())?;
    check_ooxml_leg(epub_out, "online EPUB", ".epub")?;
    let form_fill = report
        .get("formFill")
        .ok_or_else(|| "the page reported no form-fill leg (formFill = None)".to_string())?;
    check_fill_forms_leg(form_fill, "online form fill")?;
    let edit_text = report
        .get("editText")
        .ok_or_else(|| "the page reported no edit-text leg (editText = None)".to_string())?;
    check_edit_text_leg(edit_text, "online edit text")?;
    let text = report
        .get("text")
        .ok_or_else(|| "the page reported no text leg (text = None)".to_string())?;
    check_text_leg(text, "online text")?;
    let word = report
        .get("word")
        .ok_or_else(|| "the page reported no Word leg (word = None)".to_string())?;
    check_ooxml_leg(word, "online word", ".docx")?;
    let slides = report
        .get("slides")
        .ok_or_else(|| "the page reported no slides leg (slides = None)".to_string())?;
    check_ooxml_leg(slides, "online slides", ".pptx")?;
    let workbook = report
        .get("workbook")
        .ok_or_else(|| "the page reported no workbook leg (workbook = None)".to_string())?;
    check_ooxml_leg(workbook, "online workbook", ".xlsx")?;
    let png = report
        .get("png")
        .ok_or_else(|| "the page reported no PDF-to-PNG leg (png = None)".to_string())?;
    check_raster_leg(png, "online PNG", &PNG_LEG)?;
    let png_zip = report.get("pngZip").ok_or_else(|| {
        "the page reported no multi-page PNG bundle leg (pngZip = None)".to_string()
    })?;
    check_raster_zip_leg(png_zip, "online PNG bundle", &PNG_LEG)?;
    let jpg = report
        .get("jpg")
        .ok_or_else(|| "the page reported no PDF-to-JPG leg (jpg = None)".to_string())?;
    check_raster_leg(jpg, "online JPG", &JPG_LEG)?;
    let jpg_zip = report.get("jpgZip").ok_or_else(|| {
        "the page reported no multi-page JPG bundle leg (jpgZip = None)".to_string()
    })?;
    check_raster_zip_leg(jpg_zip, "online JPG bundle", &JPG_LEG)?;
    // The quality knob has to move the bytes the browser actually downloads,
    // not only the encoder's internals: the same page at quality 1 must come
    // back smaller than the same page at quality 90.
    let jpg_coarse = report.get("jpgCoarse").ok_or_else(|| {
        "the page reported no coarse-quality JPG leg (jpgCoarse = None)".to_string()
    })?;
    check_raster_leg(jpg_coarse, "online JPG at quality 1", &JPG_LEG)?;
    check_smaller_than(jpg_coarse, jpg, "online JPG at quality 1", "online JPG")?;
    let tiff = report
        .get("tiff")
        .ok_or_else(|| "the page reported no PDF-to-TIFF leg (tiff = None)".to_string())?;
    check_raster_leg(tiff, "online TIFF", &TIFF_LEG)?;
    // The container is the card: three pages selected must be three pages
    // inside the one file the browser downloads, not three files in a ZIP.
    let tiff_three = report
        .get("tiffThree")
        .ok_or_else(|| "the page reported no three-page TIFF leg (tiffThree = None)".to_string())?;
    check_raster_leg(tiff_three, "online TIFF of three pages", &TIFF_THREE_LEG)?;
    let webp = report
        .get("webp")
        .ok_or_else(|| "the page reported no PDF-to-WebP leg (webp = None)".to_string())?;
    check_raster_leg(webp, "online WebP", &WEBP_LEG)?;
    let webp_zip = report.get("webpZip").ok_or_else(|| {
        "the page reported no multi-page WebP bundle leg (webpZip = None)".to_string()
    })?;
    check_raster_zip_leg(webp_zip, "online WebP bundle", &WEBP_LEG)?;
    // Two lossless files of one render, and the browser — not the engine — is
    // the judge: the WebP of this page must paint exactly what the PNG of it
    // paints. This is the leg that would catch a coder that is self-consistent
    // and wrong.
    check_lossless_pair_agrees(png, webp, "PNG", "WebP", "online")?;
    // PDF to HTML runs in its own lazy chunk and its own shape: the whole
    // selection is one document, so the leg is read back by the browser's HTML
    // parser rather than a format sniff the writer could agree with itself.
    let html = report
        .get("html")
        .ok_or_else(|| "the page reported no PDF-to-HTML leg (html = None)".to_string())?;
    check_html_leg(html, "online HTML", &HTML_LEG)?;
    let html_one = report
        .get("htmlOne")
        .ok_or_else(|| "the page reported no single-page HTML leg (htmlOne = None)".to_string())?;
    check_html_leg(html_one, "online HTML of page 1", &HTML_ONE_LEG)?;

    // The four password cards share one lazy chunk of their own, so the first leg
    // below is also the moment the page asks for that module. Each success is
    // judged on the bytes the browser downloaded — the envelope it carries and
    // the sentence the card said over it — and the two refusals are judged on what
    // the page did NOT produce, which is the promise these cards make.
    let security = security_legs(report, "online")?;
    check_security_leg(
        security_leg(security, "online", "protect")?,
        "online protect",
        &PROTECT_LEG,
    )?;
    check_security_leg(
        security_leg(security, "online", "permissions")?,
        "online set permissions",
        &PERMISSIONS_LEG,
    )?;
    check_security_refusal(
        security_leg(security, "online", "unlockRefusal")?,
        "online unlock with a wrong password",
        &UNLOCK_REFUSAL,
    )?;
    check_security_leg(
        security_leg(security, "online", "unlock")?,
        "online unlock",
        &UNLOCK_LEG,
    )?;
    check_security_leg(
        security_leg(security, "online", "unlockOwn")?,
        "online unlock of the file this page wrote",
        &UNLOCK_OWN_LEG,
    )?;
    check_security_refusal(
        security_leg(security, "online", "clearRefusal")?,
        "online clear permissions with the user password",
        &CLEAR_REFUSAL,
    )?;
    check_security_leg(
        security_leg(security, "online", "clear")?,
        "online clear permissions",
        &CLEAR_LEG,
    )?;

    // The signing cards share one lazy chunk, and their legs chain: the
    // certificate leg signs the fixture, then the report leg validates the
    // bytes it just wrote against the root the chain ends at. A report row that
    // says "verified" cannot come from a fixture this page did not sign.
    let signing = signing_legs(report, "online")?;
    check_sign_document(
        signing_leg(signing, "online", "stamp")?,
        "online stamp",
        &STAMP_LEG,
    )?;
    check_sign_document(
        signing_leg(signing, "online", "certificate")?,
        "online certificate signature",
        &CERTIFICATE_LEG,
    )?;
    check_sign_document(
        signing_leg(signing, "online", "workflow")?,
        "online approval workflow",
        &WORKFLOW_LEG,
    )?;
    check_signature_report(
        signing_leg(signing, "online", "report")?,
        "online signature report",
        &SIGNATURE_REPORT_LEG,
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
    if offline.get("cards").and_then(Json::as_u64) != Some(TOOLS_PAGE_CARDS.len() as u64) {
        return Err(format!(
            "the offline tools page did not render all {} cards (cards = {:?})",
            TOOLS_PAGE_CARDS.len(),
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
    let offline_watermark_tiled = offline.get("watermarkTiled").ok_or_else(|| {
        "the offline leg reported no tiled-watermark result (watermarkTiled = None)".to_string()
    })?;
    check_tiled_watermark_leg(offline_watermark_tiled, "offline tiled watermark")?;
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
    let offline_compress_lossy = offline.get("compressLossy").ok_or_else(|| {
        "the offline leg reported no lossy compress result (compressLossy = None)".to_string()
    })?;
    check_lossy_compress_leg(offline_compress_lossy, "offline compress lossy", false)?;
    let offline_compress_over_target = offline.get("compressOverTarget").ok_or_else(|| {
        "the offline leg reported no over-target compress result (compressOverTarget = None)"
            .to_string()
    })?;
    check_lossy_compress_leg(
        offline_compress_over_target,
        "offline compress over target",
        true,
    )?;
    let offline_crop = offline
        .get("crop")
        .ok_or_else(|| "the offline leg reported no crop result".to_string())?;
    check_stamp_leg(
        offline_crop,
        "offline crop",
        "report-cropped.pdf",
        "Crop PDF",
        3,
        &[],
    )?;
    let offline_flip = offline
        .get("flip")
        .ok_or_else(|| "the offline leg reported no flip result".to_string())?;
    check_stamp_leg(
        offline_flip,
        "offline flip",
        "report-flipped.pdf",
        "Flip PDF",
        3,
        &[],
    )?;
    let offline_page_labels = offline
        .get("pageLabels")
        .ok_or_else(|| "the offline leg reported no page-label result".to_string())?;
    check_stamp_leg(
        offline_page_labels,
        "offline page labels",
        "report-labeled.pdf",
        "Apply page labels",
        3,
        &["/PageLabels", "(A-)"],
    )?;
    let offline_resize = offline
        .get("resize")
        .ok_or_else(|| "the offline leg reported no resize result".to_string())?;
    check_stamp_leg(
        offline_resize,
        "offline resize",
        "report-resized.pdf",
        "Resize pages",
        3,
        &["595.28", "841.89"],
    )?;
    let offline_nup = offline
        .get("nup")
        .ok_or_else(|| "the offline leg reported no N-up result".to_string())?;
    check_page_count_leg(
        offline_nup,
        "offline N-up",
        "report-n-up.pdf",
        "Place pages",
        3,
        2,
        &["/Subtype /Form", "/Type /XObject"],
    )?;
    let offline_attachments = offline
        .get("attachments")
        .ok_or_else(|| "the offline leg reported no attachment-extraction result".to_string())?;
    check_attachments_leg(
        offline_attachments,
        "offline attachments",
        &[("notes.txt", "hello attachment"), ("evil.txt", "safe name")],
    )?;
    let offline_attach = offline
        .get("attach")
        .ok_or_else(|| "the offline leg reported no attach result".to_string())?;
    check_attach_leg(offline_attach, "offline attach")?;
    let offline_detach = offline
        .get("detach")
        .ok_or_else(|| "the offline leg reported no detach result".to_string())?;
    check_detach_leg(offline_detach, "offline detach")?;
    let offline_repair = offline
        .get("repair")
        .ok_or_else(|| "the offline leg reported no repair result".to_string())?;
    check_repair_leg(offline_repair, "offline repair")?;
    check_single_output_leg(
        offline_leg_of(report, "rotate")?,
        &SingleLeg {
            label: "offline rotate",
            run_label: "Rotate PDF",
            output_name: "report-rotated.pdf",
            file_rows: 1,
            zip: false,
            pages: 3,
            pages_row: Some("3 \u{2192} 3"),
            found_counts: &[3],
        },
    )?;
    check_single_output_leg(
        offline_leg_of(report, "delete")?,
        &SingleLeg {
            label: "offline delete pages",
            run_label: "Delete pages",
            output_name: "report-trimmed.pdf",
            file_rows: 1,
            zip: false,
            pages: 2,
            pages_row: Some("3 \u{2192} 2"),
            found_counts: &[1, 1],
        },
    )?;
    check_single_output_leg(
        offline_leg_of(report, "metadata")?,
        &SingleLeg {
            label: "offline edit metadata",
            run_label: "Save metadata",
            output_name: "metadata-metadata.pdf",
            file_rows: 1,
            zip: false,
            pages: 3,
            pages_row: Some("3 \u{2192} 3"),
            found_counts: &[1, 1],
        },
    )?;
    check_single_output_leg(
        offline_leg_of(report, "zip")?,
        &SingleLeg {
            label: "offline zip",
            run_label: "Create ZIP",
            output_name: "one.zip",
            file_rows: 2,
            zip: true,
            pages: 0,
            pages_row: None,
            found_counts: &[1, 2, 2],
        },
    )?;
    let offline_batch = offline.get("batch").ok_or_else(|| {
        "the offline leg reported no batch-queue result (offline.batch = None)".to_string()
    })?;
    check_batch_leg(offline_batch, "offline batch queue")?;
    let offline_strip_metadata = offline.get("stripMetadata").ok_or_else(|| {
        "the offline leg reported no metadata-strip result (stripMetadata = None)".to_string()
    })?;
    check_strip_metadata_leg(offline_strip_metadata, "offline strip metadata")?;
    let offline_metadata_report = offline.get("metadataReport").ok_or_else(|| {
        "the offline leg reported no metadata-report result (metadataReport = None)".to_string()
    })?;
    check_metadata_report_leg(offline_metadata_report, "offline metadata report")?;
    let offline_image_to_pdf = offline.get("imageToPdf").ok_or_else(|| {
        "the offline leg reported no image-to-PDF result (imageToPdf = None)".to_string()
    })?;
    check_image_leg(offline_image_to_pdf, "offline image to PDF")?;
    let offline_image_watermark = offline.get("imageWatermark").ok_or_else(|| {
        "the offline leg reported no image-watermark result (imageWatermark = None)".to_string()
    })?;
    check_image_watermark_leg(offline_image_watermark, "offline image watermark")?;
    let offline_outline = offline
        .get("outline")
        .ok_or_else(|| "the offline leg reported no outline result".to_string())?;
    check_outline_leg(offline_outline, "offline outline")?;
    let offline_convert = offline
        .get("convert")
        .ok_or_else(|| "the offline leg reported no convert result".to_string())?;
    check_convert_leg(
        offline_convert,
        "offline convert",
        "notes-converted.pdf",
        &CONVERT_NEEDLES,
        1,
    )?;
    let offline_form_fill = offline
        .get("formFill")
        .ok_or_else(|| "the offline leg reported no form-fill result".to_string())?;
    check_fill_forms_leg(offline_form_fill, "offline form fill")?;
    let offline_edit_text = offline
        .get("editText")
        .ok_or_else(|| "the offline leg reported no edit-text result".to_string())?;
    check_edit_text_leg(offline_edit_text, "offline edit text")?;
    let offline_text = offline
        .get("text")
        .ok_or_else(|| "the offline leg reported no text result".to_string())?;
    check_text_leg(offline_text, "offline text")?;
    let offline_word = offline
        .get("word")
        .ok_or_else(|| "the offline leg reported no Word result".to_string())?;
    check_ooxml_leg(offline_word, "offline word", ".docx")?;
    let offline_slides = offline
        .get("slides")
        .ok_or_else(|| "the offline leg reported no slides result".to_string())?;
    check_ooxml_leg(offline_slides, "offline slides", ".pptx")?;
    let offline_workbook = offline
        .get("workbook")
        .ok_or_else(|| "the offline leg reported no workbook result".to_string())?;
    check_ooxml_leg(offline_workbook, "offline workbook", ".xlsx")?;
    let offline_tables = offline
        .get("tables")
        .ok_or_else(|| "the offline leg reported no tables result".to_string())?;
    check_tables_leg(offline_tables, "offline tables")?;
    let offline_split_text = offline
        .get("splitText")
        .ok_or_else(|| "the offline leg reported no split result".to_string())?;
    check_text_split_leg(offline_split_text, "offline split")?;
    let offline_reflow = offline
        .get("reflow")
        .ok_or_else(|| "the offline leg reported no reflow result".to_string())?;
    check_reflow_leg(offline_reflow, "offline reflow")?;
    let offline_epub_out = offline
        .get("epubOut")
        .ok_or_else(|| "the offline leg reported no EPUB export result".to_string())?;
    check_ooxml_leg(offline_epub_out, "offline EPUB export", ".epub")?;
    for (key, title, needles, pages) in [
        ("latex", "quarto-converted.pdf", &LATEX_NEEDLES[..], 1),
        ("csv", "ledger-converted.pdf", &CSV_NEEDLES[..], 1),
        ("svg", "mark-converted.pdf", &SVG_NEEDLES[..], 1),
        ("epub", "book-converted.pdf", &EPUB_NEEDLES[..], 1),
        ("table", "table-converted.pdf", &TABLE_NEEDLES[..], 1),
        ("docx", "letter-converted.pdf", &DOCX_NEEDLES[..], 1),
        ("pptx", "deck-converted.pdf", &PPTX_NEEDLES[..], 2),
        ("xlsx", "book2-converted.pdf", &XLSX_NEEDLES[..], 2),
    ] {
        let leg = offline
            .get(key)
            .ok_or_else(|| format!("the offline leg reported no {key} result"))?;
        check_convert_leg(leg, &format!("offline {key}"), title, needles, pages)?;
    }
    let offline_png = offline
        .get("png")
        .ok_or_else(|| "the offline leg reported no PDF-to-PNG result".to_string())?;
    check_raster_leg(offline_png, "offline PNG", &PNG_LEG)?;
    let offline_png_zip = offline
        .get("pngZip")
        .ok_or_else(|| "the offline leg reported no multi-page PNG bundle result".to_string())?;
    check_raster_zip_leg(offline_png_zip, "offline PNG bundle", &PNG_LEG)?;
    let offline_jpg = offline
        .get("jpg")
        .ok_or_else(|| "the offline leg reported no PDF-to-JPG result".to_string())?;
    check_raster_leg(offline_jpg, "offline JPG", &JPG_LEG)?;
    let offline_jpg_zip = offline
        .get("jpgZip")
        .ok_or_else(|| "the offline leg reported no multi-page JPG bundle result".to_string())?;
    check_raster_zip_leg(offline_jpg_zip, "offline JPG bundle", &JPG_LEG)?;
    let offline_tiff = offline
        .get("tiff")
        .ok_or_else(|| "the offline leg reported no PDF-to-TIFF result".to_string())?;
    check_raster_leg(offline_tiff, "offline TIFF", &TIFF_LEG)?;
    let offline_tiff_three = offline
        .get("tiffThree")
        .ok_or_else(|| "the offline leg reported no three-page TIFF result".to_string())?;
    check_raster_leg(
        offline_tiff_three,
        "offline TIFF of three pages",
        &TIFF_THREE_LEG,
    )?;
    let offline_webp = offline
        .get("webp")
        .ok_or_else(|| "the offline leg reported no PDF-to-WebP result".to_string())?;
    check_raster_leg(offline_webp, "offline WebP", &WEBP_LEG)?;
    let offline_webp_zip = offline.get("webpZip").ok_or_else(|| {
        "the offline leg reported no multi-page WebP bundle result (offline.webpZip = None)"
            .to_string()
    })?;
    check_raster_zip_leg(offline_webp_zip, "offline WebP bundle", &WEBP_LEG)?;
    check_lossless_pair_agrees(offline_png, offline_webp, "PNG", "WebP", "offline")?;
    let offline_html = offline.get("html").ok_or_else(|| {
        "the offline leg reported no PDF-to-HTML result (offline.html = None)".to_string()
    })?;
    check_html_leg(offline_html, "offline HTML", &HTML_LEG)?;
    let offline_html_one = offline.get("htmlOne").ok_or_else(|| {
        "the offline leg reported no single-page HTML result (offline.htmlOne = None)".to_string()
    })?;
    check_html_leg(offline_html_one, "offline HTML of page 1", &HTML_ONE_LEG)?;

    // The same seven password legs with the origin cut — including the chained
    // one, which reads bytes this page wrote moments ago rather than a fixture.
    let offline_security = offline.get("security").ok_or_else(|| {
        "the offline run reported no password legs (offline.security = None)".to_string()
    })?;
    check_security_leg(
        security_leg(offline_security, "offline", "protect")?,
        "offline protect",
        &PROTECT_LEG,
    )?;
    check_security_leg(
        security_leg(offline_security, "offline", "permissions")?,
        "offline set permissions",
        &PERMISSIONS_LEG,
    )?;
    check_security_refusal(
        security_leg(offline_security, "offline", "unlockRefusal")?,
        "offline unlock with a wrong password",
        &UNLOCK_REFUSAL,
    )?;
    check_security_leg(
        security_leg(offline_security, "offline", "unlock")?,
        "offline unlock",
        &UNLOCK_LEG,
    )?;
    check_security_leg(
        security_leg(offline_security, "offline", "unlockOwn")?,
        "offline unlock of the file this page wrote",
        &UNLOCK_OWN_LEG,
    )?;
    check_security_refusal(
        security_leg(offline_security, "offline", "clearRefusal")?,
        "offline clear permissions with the user password",
        &CLEAR_REFUSAL,
    )?;
    check_security_leg(
        security_leg(offline_security, "offline", "clear")?,
        "offline clear permissions",
        &CLEAR_LEG,
    )?;

    // The same four signing legs with the origin cut. The signing chunk is
    // runtime-cached on first use rather than precached, so these legs prove a
    // device that once signed can sign and validate again with no server.
    let offline_signing = offline.get("sign").ok_or_else(|| {
        "the offline run reported no signing legs (offline.sign = None)".to_string()
    })?;
    check_sign_document(
        signing_leg(offline_signing, "offline", "stamp")?,
        "offline stamp",
        &STAMP_LEG,
    )?;
    check_sign_document(
        signing_leg(offline_signing, "offline", "certificate")?,
        "offline certificate signature",
        &CERTIFICATE_LEG,
    )?;
    check_sign_document(
        signing_leg(offline_signing, "offline", "workflow")?,
        "offline approval workflow",
        &WORKFLOW_LEG,
    )?;
    check_signature_report(
        signing_leg(offline_signing, "offline", "report")?,
        "offline signature report",
        &SIGNATURE_REPORT_LEG,
    )?;

    let status = string_field(report, "status")?;
    if !status.contains("verified") {
        return Err(format!(
            "the success status did not say the result was verified (status = {status:?})"
        ));
    }
    Ok(())
}

/// `editor` (pdf-plan/39): the workspace, with a real document in it.
///
/// The claims, in the order the page made them: the route is the real one,
/// the document was drawn with a clickable text layer, a line of its text
/// changed, the change was undone and the document came back exactly as it
/// was, the redo brought it back, and the download is a PDF carrying the
/// change.
///
/// The undo leg is the one that matters most. An editor whose undo restores
/// something *like* the document it had is not an undo, and only a byte-level
/// comparison of the text layer can tell the two apart — so the line's text
/// before the change and after the undo are compared, not just "the undo
/// button did something".
fn verdict_editor(report: &Json) -> Result<(), String> {
    well_formed(report)?;
    if string_field(report, "check")? != "editor" {
        return Err("the page did not identify itself as the editor check".into());
    }
    if string_field(report, "path")? != "/edit-pdf/" {
        return Err(format!(
            "the workspace ran at {:?}, expected \"/edit-pdf/\"",
            report.get("path")
        ));
    }

    let online = report
        .get("online")
        .and_then(Json::as_object)
        .ok_or_else(|| "the page reported no online leg (online = None)".to_string())?;
    check_editor_leg(online, "online")?;

    // The three ways of changing what is ON the page: a text box, an image,
    // and a redaction.
    let text_box = report
        .get("textBox")
        .and_then(Json::as_object)
        .ok_or_else(|| "the page reported no text-box leg (textBox = None)".to_string())?;
    let boxed = text_box
        .get("text")
        .and_then(Json::as_str)
        .ok_or_else(|| "the text-box leg reported no text".to_string())?;
    if text_box.get("carriesText").and_then(Json::as_bool) != Some(true) {
        return Err(format!(
            "the downloaded document does not carry the text box {boxed:?}"
        ));
    }
    // The formatting row: the size and the colour the reader chose before
    // the text was placed. The colour has to be IN the file - a swatch that
    // only changes the editor's own text is decoration.
    if text_box.get("chosenSize").and_then(Json::as_str) != Some("24") {
        return Err("the text box's size was not chosen from the formatting row".into());
    }
    if text_box.get("chosenColour").and_then(Json::as_str) != Some("Red") {
        return Err("the text box's colour was not chosen from the formatting row".into());
    }
    if text_box.get("carriesColour").and_then(Json::as_bool) != Some(true) {
        return Err("the text box is not red in the produced file".into());
    }

    // The placement tools, tested through the browser's own hit test.
    let placement = report
        .get("placement")
        .and_then(Json::as_object)
        .ok_or_else(|| "the page reported no placement leg (placement = None)".to_string())?;
    if placement.get("onCanvas").and_then(Json::as_bool) != Some(true) {
        return Err(format!(
            "a click on the page's empty space landed on {:?}, not on the canvas; the text \\
             layer is swallowing every click that is not exactly on a word",
            placement.get("hit")
        ));
    }
    if placement.get("opened").and_then(Json::as_bool) != Some(true) {
        return Err("the text box did not open on a click on the page's empty space".into());
    }
    // And the placement has to work over TEXT too: a reader who drags a box
    // across a paragraph is not asking for an exception, and a word that
    // takes the pointer is a word that swallows the drag.
    if placement.get("overText").and_then(Json::as_bool) != Some(true) {
        return Err("the placement was not proved over the page's text".into());
    }
    if placement.get("overTheCanvas").and_then(Json::as_bool) != Some(true) {
        return Err("a click over the page's text did not reach the canvas".into());
    }

    // The command palette: a view over the ribbon.
    let palette = report
        .get("palette")
        .and_then(Json::as_object)
        .ok_or_else(|| "the page reported no palette leg (palette = None)".to_string())?;
    let commands = palette.get("commands").and_then(Json::as_u64).unwrap_or(0);
    if commands < 8 {
        return Err(format!(
            "the palette offered {commands} commands; it should offer the whole ribbon"
        ));
    }
    let narrowed = palette.get("narrowed").and_then(Json::as_u64).unwrap_or(0);
    if narrowed == 0 || narrowed >= commands {
        return Err("the palette did not narrow to what was typed".into());
    }
    if palette.get("ran").and_then(Json::as_bool) != Some(true) {
        return Err("a command run from the palette did not do what it does in the ribbon".into());
    }

    // The panels that read and change the document.
    let panels = report
        .get("panels")
        .and_then(Json::as_object)
        .ok_or_else(|| "the page reported no panels leg (panels = None)".to_string())?;
    let report_text = panels.get("report").and_then(Json::as_str).unwrap_or("");
    if !report_text.contains("tagged") {
        return Err(format!(
            "the accessibility report said nothing about the document's tags: {report_text:?}"
        ));
    }
    let attachments = panels.get("attachments").and_then(Json::as_str).unwrap_or("");
    if !attachments.contains("notes.txt") {
        return Err(format!(
            "the attached file is not in the document's list of attachments: {attachments:?}"
        ));
    }
    let kept = panels.get("kept").and_then(Json::as_str).unwrap_or("");
    if !kept.contains("Kept") {
        return Err(format!("the pages were not kept: {kept:?}"));
    }

    // The measure tool and the signature.
    let tools = report
        .get("tools")
        .and_then(Json::as_object)
        .ok_or_else(|| "the page reported no tools leg (tools = None)".to_string())?;
    let measured = tools.get("measured").and_then(Json::as_str).unwrap_or("");
    if !measured.contains("pt") || !measured.contains("mm") {
        return Err(format!("the measurement said nothing about the page: {measured:?}"));
    }
    if tools.get("overlay").and_then(Json::as_u64).unwrap_or(0) < 1 {
        return Err("the measurement was not drawn on the page".into());
    }
    let signed = tools.get("signed").and_then(Json::as_str).unwrap_or("");
    if !signed.contains("Signed") {
        return Err(format!("the signature was not placed: {signed:?}"));
    }
    if tools.get("carriesTheSignature").and_then(Json::as_bool) != Some(true) {
        return Err("the signature is not in the produced file".into());
    }

    // The markup: the marks a reader leaves on a document.
    let markup = report
        .get("markup")
        .and_then(Json::as_object)
        .ok_or_else(|| "the page reported no markup leg (markup = None)".to_string())?;
    let marked = markup.get("marked").and_then(Json::as_str).unwrap_or("");
    if !marked.contains("mark") {
        return Err(format!("the mark was not placed: {marked:?}"));
    }
    // The annotation must be IN the file, and it must be a standard one with
    // an appearance: a highlight that is only on the screen is not a
    // highlight.
    if markup.get("carriesTheAnnotation").and_then(Json::as_bool) != Some(true) {
        return Err("the highlight is not in the produced file".into());
    }
    if markup.get("carriesTheAppearance").and_then(Json::as_bool) != Some(true) {
        return Err("the highlight has no appearance stream in the produced file".into());
    }
    let comments = markup.get("comments").and_then(Json::as_str).unwrap_or("");
    if !comments.contains("Highlight") {
        return Err(format!("the document's own report does not list the mark: {comments:?}"));
    }
    // A mark can be picked up and put down, and taken off.
    let moved = markup.get("moved").and_then(Json::as_str).unwrap_or("");
    if !moved.contains("Moved") {
        return Err(format!("the mark was not moved: {moved:?}"));
    }
    let removed = markup.get("removed").and_then(Json::as_str).unwrap_or("");
    if !removed.contains("Removed") {
        return Err(format!("the mark was not removed: {removed:?}"));
    }

    // The session: the document survives a reload, and the app bar says
    // what there is to do.
    let session = report
        .get("session")
        .and_then(Json::as_object)
        .ok_or_else(|| "the page reported no session leg (session = None)".to_string())?;
    if session.get("openHidden").and_then(Json::as_bool) != Some(true) {
        return Err("the Open button is still offered while a document is open".into());
    }
    if session.get("closeHidden").and_then(Json::as_bool) != Some(false) {
        return Err("the Close button is missing while a document is open".into());
    }
    if session.get("restoredPages").and_then(Json::as_u64).unwrap_or(0) < 1 {
        return Err("the document did not survive the reload".into());
    }
    let restored = session.get("restoredStatus").and_then(Json::as_str).unwrap_or("");
    if !restored.contains("Restored") {
        return Err(format!("the reload did not restore the document: {restored:?}"));
    }

    // The inline editor must BE the line it is over.
    let inline = report
        .get("inline")
        .and_then(Json::as_object)
        .ok_or_else(|| "the page reported no inline leg (inline = None)".to_string())?;
    let gap = inline.get("gap").and_then(Json::as_f64).unwrap_or(f64::NAN);
    if !gap.is_finite() || gap.abs() > 0.5 {
        return Err(format!(
            "the inline editor's text sits {gap}px from the line's own baseline; it has to \\
             share the baseline to look like editing the document"
        ));
    }
    if inline.get("size").and_then(Json::as_str) != inline.get("lineSize").and_then(Json::as_str) {
        return Err("the inline editor is not the line's own size".into());
    }
    if inline.get("overTheLine").and_then(Json::as_bool) != Some(true) {
        return Err("the inline editor is not over the line it is editing".into());
    }
    if inline.get("committedOnClickAway").and_then(Json::as_bool) != Some(true) {
        return Err("clicking away from the editor threw the edit away instead of keeping it".into());
    }

    let image = report
        .get("image")
        .and_then(Json::as_object)
        .ok_or_else(|| "the page reported no image leg (image = None)".to_string())?;
    if image.get("carriesImage").and_then(Json::as_bool) != Some(true) {
        return Err("the downloaded document does not carry the placed image".into());
    }

    let redact = report
        .get("redact")
        .and_then(Json::as_object)
        .ok_or_else(|| "the page reported no redact leg (redact = None)".to_string())?;
    if redact.get("stillCarriesText").and_then(Json::as_bool) != Some(false) {
        return Err(
            "the redacted text is still in the file; a redaction that only covers the text is \
             not a redaction"
                .into(),
        );
    }
    if redact.get("coversText").and_then(Json::as_bool) != Some(true) {
        return Err("the redacted region is not covered by an opaque box".into());
    }

    // The page's own size, the document's properties, and the form's fields.
    let crop = report
        .get("crop")
        .and_then(Json::as_object)
        .ok_or_else(|| "the page reported no crop leg (crop = None)".to_string())?;
    if crop.get("narrowed").and_then(Json::as_bool) != Some(true) {
        return Err("the cropped page is not narrower than it was".into());
    }

    let resize = report
        .get("resize")
        .and_then(Json::as_object)
        .ok_or_else(|| "the page reported no resize leg (resize = None)".to_string())?;
    if resize.get("resized").and_then(Json::as_bool) != Some(true) {
        return Err("the resized page is the same size as it was".into());
    }

    let bookmarks = report
        .get("bookmarks")
        .and_then(Json::as_object)
        .ok_or_else(|| "the page reported no bookmarks leg (bookmarks = None)".to_string())?;
    if bookmarks.get("carriesBookmark").and_then(Json::as_bool) != Some(true) {
        return Err("the saved document does not carry the bookmark the check added".into());
    }

    let shell = report
        .get("shell")
        .and_then(Json::as_object)
        .ok_or_else(|| "the page reported no shell leg (shell = None)".to_string())?;
    if shell.get("tabs").and_then(Json::as_bool) != Some(true) {
        return Err("the ribbon's tabs did not swap the commands under them".into());
    }
    if shell.get("thumbnails").and_then(Json::as_u64).unwrap_or(0) < 1 {
        return Err("the page rail drew no thumbnail of the document".into());
    }
    if shell.get("matched").and_then(Json::as_u64).unwrap_or(0) < 1 {
        return Err("the find box found nothing in the document".into());
    }
    if shell.get("zoomed").and_then(Json::as_bool) != Some(true) {
        return Err("the zoom did not change the size of the page".into());
    }
    if shell.get("history").and_then(Json::as_u64).unwrap_or(0) < 1 {
        return Err("the history panel lists no changes".into());
    }
    if shell.get("carriesWatermark").and_then(Json::as_bool) != Some(true) {
        return Err("the watermark is not in the produced file".into());
    }

    let theme = report
        .get("theme")
        .and_then(Json::as_object)
        .ok_or_else(|| "the page reported no theme leg (theme = None)".to_string())?;
    if theme.get("switched").and_then(Json::as_bool) != Some(true) {
        return Err("the theme switch did not change the page's theme".into());
    }
    if theme.get("remembered").and_then(Json::as_bool) != Some(true) {
        return Err("the theme switch did not remember the reader's choice".into());
    }

    // The FACE, before any document is open.
    let face = report
        .get("face")
        .and_then(Json::as_object)
        .ok_or_else(|| "the page reported no face leg (face = None)".to_string())?;
    if face.get("emptyVisible").and_then(Json::as_bool) != Some(true) {
        return Err("the invitation to open a file is not on the screen".into());
    }
    if face.get("nothingToScroll").and_then(Json::as_bool) != Some(true) {
        return Err("the workspace has to be scrolled to see that no file is open".into());
    }
    if face.get("buttonThere").and_then(Json::as_bool) != Some(true) {
        return Err("the panel's button disappeared with the panel".into());
    }
    if face.get("pickerAsked").and_then(Json::as_bool) != Some(true) {
        return Err("the invitation's own button did not ask for a file".into());
    }
    let collapsed = face.get("collapsed").and_then(Json::as_u64).unwrap_or(0);
    let restored = face.get("restored").and_then(Json::as_u64).unwrap_or(0);
    if collapsed == 0 || collapsed >= restored {
        return Err("the panel could not be put away and brought back".into());
    }

    // The app bar in its empty state: there is nothing to download and
    // nothing to close.
    let empty_bar = face
        .get("emptyBar")
        .and_then(Json::as_object)
        .ok_or_else(|| "the page reported no app-bar state (emptyBar = None)".to_string())?;
    if empty_bar.get("downloadHidden").and_then(Json::as_bool) != Some(true) {
        return Err("the Download button is offered while there is nothing to download".into());
    }
    if empty_bar.get("openHidden").and_then(Json::as_bool) != Some(false) {
        return Err("the Open button is missing while there is no document".into());
    }
    if empty_bar.get("closeHidden").and_then(Json::as_bool) != Some(true) {
        return Err("the Close button is offered while there is no document".into());
    }


    let export = report
        .get("export")
        .and_then(Json::as_object)
        .ok_or_else(|| "the page reported no export leg (export = None)".to_string())?;
    let formats = export
        .get("formats")
        .and_then(Json::as_array)
        .map(|rows| rows.len())
        .unwrap_or(0);
    if formats < 4 {
        return Err(format!(
            "the editor offered {formats} export formats; it should offer the whole set"
        ));
    }
    // A PNG is a PNG: it starts with the four bytes 89 50 4E 47.
    let png_header = export
        .get("pngHeader")
        .and_then(Json::as_str)
        .unwrap_or("");
    if !png_header.starts_with("\u{89}PNG") && !png_header.contains("PNG") {
        return Err(format!(
            "the PNG export is not a PNG (it starts {png_header:?})"
        ));
    }

    // A Word document and an Excel workbook are ZIP containers, so they start
    // with "PK". A file that does not is not what the button said it was.
    let word_header = export
        .get("wordHeader")
        .and_then(Json::as_str)
        .unwrap_or("");
    if !word_header.starts_with("PK") {
        return Err(format!(
            "the Word export is not a ZIP container (it starts {word_header:?})"
        ));
    }
    let html_header = export
        .get("htmlHeader")
        .and_then(Json::as_str)
        .unwrap_or("");
    if !html_header.starts_with("<!do") && !html_header.starts_with("<htm") {
        return Err(format!(
            "the HTML export does not start with HTML (it starts {html_header:?})"
        ));
    }
    if export.get("htmlCarriesTheText").and_then(Json::as_bool) != Some(true) {
        return Err("the HTML export does not carry the document's text".into());
    }
    if export.get("offeredAgain").and_then(Json::as_bool) != Some(true) {
        return Err("the export did not offer the file again".into());
    }
    // The object URL of the produced file must still resolve after the
    // export. The bug this catches is the quiet one: the URL was revoked in
    // the same turn as the click, so the browser read a URL that no longer
    // resolved and the file was lost with nothing to show for it.
    if export.get("stillThere").and_then(Json::as_bool) != Some(true) {
        return Err("the exported file's URL was revoked before the browser could read it".into());
    }

    let bates = report
        .get("bates")
        .and_then(Json::as_object)
        .ok_or_else(|| "the page reported no Bates leg (bates = None)".to_string())?;
    if bates.get("carriesThePrefix").and_then(Json::as_bool) != Some(true) {
        return Err("the Bates numbers are not in the produced file".into());
    }

    let properties = report
        .get("properties")
        .and_then(Json::as_object)
        .ok_or_else(|| "the page reported no properties leg (properties = None)".to_string())?;
    if properties.get("carriesTitle").and_then(Json::as_bool) != Some(true) {
        return Err("the downloaded document does not carry the title the check set".into());
    }

    let form = report
        .get("form")
        .and_then(Json::as_object)
        .ok_or_else(|| "the page reported no form leg (form = None)".to_string())?;
    if form.get("fields").and_then(Json::as_u64).unwrap_or(0) < 2 {
        return Err(format!(
            "the form leg read {} fields; the fixture has seven",
            form.get("fields").and_then(Json::as_u64).unwrap_or(0)
        ));
    }
    if form.get("readOnlyNotEditable").and_then(Json::as_bool) != Some(true) {
        return Err(
            "the form's read-only field was offered for editing; the engine refuses to \
             override the flag"
                .into(),
        );
    }
    if form.get("carriesValue").and_then(Json::as_bool) != Some(true) {
        return Err("the filled form does not carry the value the check wrote".into());
    }

    // The offline leg is the same edit with the origin cut: the workspace, its
    // module graph, its stylesheet, the engine chunk and the tools chunk all
    // have to come back from the worker's cache, or the editor is a page that
    // only works while the network is up.
    let offline = report
        .get("offline")
        .and_then(Json::as_object)
        .ok_or_else(|| "the page reported no offline leg (offline = None)".to_string())?;
    check_editor_leg(offline, "offline")?;

    if string_field(report, "noUpload")? != "verified" {
        return Err(
            "the page never set data-no-upload=verified; the local request check did not pass"
                .into(),
        );
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
            "the workspace fetched {} resource(s) from another origin: {:?}",
            foreign.len(),
            foreign.iter().filter_map(Json::as_str).collect::<Vec<_>>()
        ));
    }
    let paths = network
        .get("paths")
        .and_then(Json::as_array)
        .ok_or_else(|| "the page reported no resource paths".to_string())?;
    for wanted in [
        "/assets/host/editor/editor-page.js",
        "/assets/host/editor/engine.js",
        "/assets/editor.css",
        "/wasm/selis_pdf_wasm.wasm",
        "/wasm/selis_pdf_wasm_tools.wasm",
    ] {
        if !paths.iter().any(|path| path.as_str() == Some(wanted)) {
            return Err(format!(
                "the workspace never fetched {wanted}; the editor could not have drawn or \
                 changed the document without it"
            ));
        }
    }
    Ok(())
}

/// One leg of the editor check: a document opened, a line changed, the change
/// undone and redone, and a download read back.
///
/// The undo leg is the one that matters most. An editor whose undo restores
/// something *like* the document it had is not an undo, and only a comparison
/// of the text layer can tell the two apart — so the line's text before the
/// change and after the undo are compared, not just "the undo button did
/// something".
fn check_editor_leg(leg: &Map<String, Json>, label: &str) -> Result<(), String> {
    let pages = leg
        .get("pages")
        .and_then(Json::as_u64)
        .ok_or_else(|| format!("the {label} leg reported no page count"))?;
    if pages < 1 {
        return Err(format!(
            "the {label} leg drew {pages} pages; the fixture has one"
        ));
    }

    let lines = |key: &str| -> Vec<String> {
        leg.get(key)
            .and_then(Json::as_array)
            .map(|array| {
                array
                    .iter()
                    .filter_map(Json::as_str)
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default()
    };
    let before = lines("before");
    let after = lines("after");
    let undone = lines("undone");
    let redone = lines("redone");
    if before.is_empty() {
        return Err(format!(
            "the {label} leg recovered no text from the document"
        ));
    }
    let original = before[0].clone();
    if !original.contains("900") {
        return Err(format!(
            "the fixture's first line was {original:?}; the check changes the number in it"
        ));
    }
    if after.is_empty() || after[0] == original {
        return Err(format!(
            "the {label} leg did not change the text of the first line (before = {original:?}, \
             after = {:?})",
            after.first()
        ));
    }
    if !after[0].contains("1200") {
        return Err(format!(
            "the {label} leg changed the line to {:?}, which does not carry the replacement",
            after[0]
        ));
    }
    if undone.first() != Some(&original) {
        return Err(format!(
            "the {label} undo did not restore the line: got {:?}, expected {original:?}",
            undone.first()
        ));
    }
    if redone.first() != after.first() {
        return Err(format!(
            "the {label} redo did not restore the change: got {:?}, expected {:?}",
            redone.first(),
            after.first()
        ));
    }

    let output = leg
        .get("output")
        .and_then(Json::as_object)
        .ok_or_else(|| format!("the {label} leg reported no output object"))?;
    if output.get("blob").and_then(Json::as_bool) != Some(true) {
        return Err(format!("the {label} download was not a Blob URL"));
    }
    if output.get("header").and_then(Json::as_str) != Some("%PDF-") {
        return Err(format!(
            "the {label} download did not begin with %PDF- (header = {:?})",
            output.get("header")
        ));
    }
    let bytes = output.get("bytes").and_then(Json::as_u64).unwrap_or(0);
    if bytes <= 598 {
        return Err(format!(
            "the {label} download is only {bytes} bytes; an incremental update of a 598-byte \
             document is larger than the document"
        ));
    }
    let carried = output
        .get("carriesText")
        .and_then(Json::as_str)
        .unwrap_or("");
    if !carried.contains("1200 USD") {
        return Err(format!(
            "the {label} download does not carry the change the check made"
        ));
    }
    let name = output
        .get("downloadName")
        .and_then(Json::as_str)
        .unwrap_or("");
    if !name.ends_with(".pdf") {
        return Err(format!(
            "the {label} download name {name:?} is not a PDF name"
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
    // `tools-page` and `editor` drive the two product surfaces of the same
    // built app.
    if check.id == "app-shell"
        || check.id == "chrome"
        || check.id == "tools-page"
        || check.id == "editor"
    {
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
        row(id)?
            .get("severity")
            .and_then(Json::as_str)
            .ok_or_else(|| format!("health row {id} carries no severity"))
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
///    (the claim that matters most) NO failure panel appeared. The reader
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
            respond_closing(stream, "200 OK", kind, &bytes)
        }
        Err(_) => {
            // A 404 is a response, not an error: the favicon is a 404 on
            // every run. Its disposition is the same as every other
            // response, so nothing lands behind a dying socket.
            respond_closing(stream, "404 Not Found", "text/plain", b"not found")
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
    // Directory-index resolution is the one host behaviour plan 36's routes
    // are designed around: `/merge-pdf/` and `/merge-pdf` both serve
    // `merge-pdf/index.html`. Doing it here keeps the harness's origin honest
    // instead of making the test depend on a server quirk no host guarantees.
    if out.is_dir() {
        out.push("index.html");
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
        real_tools_count_leg(name, run_label, pages, pages, needles)
    }

    /// The tiled-watermark leg: the stamp shape plus the occurrence counts the
    /// verdict reads to prove the line repeats across the page.
    fn real_tools_tiled_watermark_leg() -> Json {
        let mut leg = real_tools_stamp_leg(
            "outlined-watermarked.pdf",
            "Add watermark",
            2,
            &["(CONFIDENTIAL) Tj"],
        );
        for pointer in ["/outputs/0", "/output"] {
            leg.pointer_mut(pointer)
                .and_then(Json::as_object_mut)
                .expect("stamp output")
                .insert("foundCounts".to_string(), json!([12]));
        }
        leg
    }

    /// The same shape for a leg that changes the page count (N-up).
    fn real_tools_count_leg(
        name: &str,
        run_label: &str,
        pages_in: u64,
        pages_out: u64,
        needles: &[&str],
    ) -> Json {
        let found: Vec<bool> = needles.iter().map(|_| true).collect();
        let output = json!({
            "kind": "pdf",
            "name": name,
            "pages": pages_out,
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
                    { "label": "Pages", "value": format!("{pages_in} \u{2192} {pages_out}") },
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

    /// The attach leg: a PDF plus one opaque payload. The verdict requires the
    /// page count to hold and the embedded-file count to rise.
    fn real_tools_attach_leg() -> Json {
        let output = json!({
            "kind": "pdf",
            "name": "report-attached.pdf",
            "pages": 3,
            "declaredBytes": 900,
            "bytes": 900,
            "blob": true,
            "previewBlob": true,
            "downloadName": "report-attached.pdf",
            "header": "%PDF-",
            "zip": false,
            "found": [true, true],
        });
        json!({
            "fileRows": 2,
            "runLabel": "Embed files",
            "noUpload": "verified",
            "status": "Done. The result was verified on this device.",
            "result": {
                "title": "report-attached.pdf",
                "meta": "1 file \u{00b7} 900 B \u{00b7} verified locally",
                "rows": [
                    { "label": "Pages", "value": "3 \u{2192} 3" },
                    { "label": "Embedded files", "value": "0 \u{2192} 1" },
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

    /// The detach leg: every embedded file is removed. The negative witnesses
    /// are all false, which is the pass.
    fn real_tools_detach_leg() -> Json {
        let output = json!({
            "kind": "pdf",
            "name": "attachments-detached.pdf",
            "pages": 1,
            "declaredBytes": 700,
            "bytes": 700,
            "blob": true,
            "previewBlob": true,
            "downloadName": "attachments-detached.pdf",
            "header": "%PDF-",
            "zip": false,
            "absentFound": [false, false, false, false, false],
        });
        json!({
            "fileRows": 1,
            "runLabel": "Remove attachments",
            "noUpload": "verified",
            "status": "Done. The result was verified on this device.",
            "result": {
                "title": "attachments-detached.pdf",
                "meta": "1 file \u{00b7} 700 B \u{00b7} verified locally",
                "rows": [
                    { "label": "Pages", "value": "1 \u{2192} 1" },
                    { "label": "Embedded files", "value": "2 \u{2192} 0" },
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

    /// The repair leg: a damaged cross-reference table is rebuilt, the page
    /// count holds, and the damaged marker is absent from the rewritten bytes.
    fn real_tools_repair_leg() -> Json {
        let output = json!({
            "kind": "pdf",
            "name": "damaged-repaired.pdf",
            "pages": 3,
            "declaredBytes": 800,
            "bytes": 800,
            "blob": true,
            "previewBlob": true,
            "downloadName": "damaged-repaired.pdf",
            "header": "%PDF-",
            "zip": false,
            "absentFound": [false],
        });
        json!({
            "fileRows": 1,
            "runLabel": "Repair PDF",
            "noUpload": "verified",
            "status": "Done. The result was verified on this device.",
            "result": {
                "title": "damaged-repaired.pdf",
                "meta": "1 file \u{00b7} 800 B \u{00b7} verified locally",
                "rows": [
                    { "label": "Pages", "value": "3 \u{2192} 3" },
                    { "label": "Recovery", "value": "reconstructed index" },
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

    /// One PDF → PNG leg: the lazy raster chunk renders the page, and the real
    /// Blob is a PNG of the canvas the engine asked for — offered as a preview
    /// as well as a download — with no PDF page syntax left inside it.
    fn real_tools_png_leg() -> Json {
        let output = json!({
            "kind": "image",
            "name": "report.png",
            "pages": 1,
            "declaredBytes": 802,
            "bytes": 802,
            "blob": true,
            "previewBlob": true,
            "downloadName": "report.png",
            "header": "\u{fffd}PNG",
            "zip": false,
            "png": true,
            "pngWidth": 200,
            "pngHeight": 200,
            "webp": false,
            "chromium": { "width": 200, "height": 200, "digest": 3_627_224_773_u64, "samples": 160_000 },
            "overTarget": false,
            "foundCounts": [1, 1],
            "found": [true, true],
            "absentFound": [false],
        });
        json!({
            "fileRows": 1,
            "runLabel": "Render PNGs",
            "noUpload": "verified",
            "status": "Done. The result was verified on this device.",
            "result": {
                "title": "report.png",
                "meta": "1 file \u{00b7} 802 B \u{00b7} verified locally",
                "rows": [
                    { "label": "Pages", "value": "1" },
                    { "label": "Size", "value": "979 B \u{2192} 802 B" },
                    { "label": "Verification", "value": "ok" },
                ],
            },
            "outputs": [output.clone()],
            "output": output,
            "network": {
                "origin": "http://127.0.0.1:1234",
                "resources": 1,
                "foreign": [],
                "paths": ["/wasm/selis_pdf_wasm_raster.wasm"],
            },
        })
    }

    /// The multi-page leg: three pages come back as one store-method ZIP with
    /// one `page-N.png` entry per page, and no preview.
    fn real_tools_png_zip_leg() -> Json {
        let output = json!({
            "kind": "zip",
            "name": "report-pages.zip",
            "pages": 0,
            "declaredBytes": 2712,
            "bytes": 2712,
            "blob": true,
            "previewBlob": Json::Null,
            "downloadName": "report-pages.zip",
            "header": "PK\u{3}\u{4}\u{14}",
            "zip": true,
            "png": false,
            "overTarget": false,
            "foundCounts": [2, 2, 2],
            "found": [true, true, true],
            "absentFound": [],
        });
        json!({
            "fileRows": 1,
            "runLabel": "Render PNGs",
            "noUpload": "verified",
            "status": "Done. The result was verified on this device.",
            "result": {
                "title": "report-pages.zip",
                "meta": "1 file \u{00b7} 2.6 KiB \u{00b7} verified locally",
                "rows": [
                    { "label": "Pages", "value": "3" },
                    { "label": "Size", "value": "979 B \u{2192} 2.6 KiB" },
                    { "label": "Verification", "value": "ok" },
                ],
            },
            "outputs": [output.clone()],
            "output": output,
            "network": {
                "origin": "http://127.0.0.1:1234",
                "resources": 1,
                "foreign": [],
                "paths": ["/wasm/selis_pdf_wasm_raster.wasm"],
            },
        })
    }

    /// One PDF → JPG leg: the same lazy chunk renders the page and the real Blob
    /// is a complete baseline JPEG — SOI, a readable frame, EOI — offered as a
    /// preview as well as a download, with no PDF page syntax left inside it.
    fn real_tools_jpg_leg() -> Json {
        let output = json!({
            "kind": "image",
            "name": "report.jpg",
            "pages": 1,
            "declaredBytes": 1200,
            "bytes": 1200,
            "blob": true,
            "previewBlob": true,
            "downloadName": "report.jpg",
            "header": "\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{0}",
            "zip": false,
            "png": false,
            "jpeg": true,
            "webp": false,
            "jpegWidth": 200,
            "jpegHeight": 200,
            "jpegBaseline": true,
            "overTarget": false,
            "foundCounts": [1],
            "found": [true],
            "absentFound": [false],
        });
        json!({
            "fileRows": 1,
            "runLabel": "Render JPGs",
            "noUpload": "verified",
            "status": "Done. The result was verified on this device.",
            "result": {
                "title": "report.jpg",
                "meta": "1 file \u{00b7} 1.2 KiB \u{00b7} verified locally",
                "rows": [
                    { "label": "Pages", "value": "1" },
                    { "label": "Size", "value": "979 B \u{2192} 1.2 KiB" },
                    { "label": "Verification", "value": "ok" },
                ],
            },
            "outputs": [output.clone()],
            "output": output,
            "network": {
                "origin": "http://127.0.0.1:1234",
                "resources": 1,
                "foreign": [],
                "paths": ["/wasm/selis_pdf_wasm_raster.wasm"],
            },
        })
    }

    /// The same page at quality 1: a smaller file, which is the only proof in the
    /// browser that the quality field reaches the encoder.
    fn real_tools_jpg_coarse_leg() -> Json {
        let mut leg = real_tools_jpg_leg();
        let object = leg["output"].as_object_mut().expect("jpg output object");
        object.insert("bytes".to_string(), json!(480));
        object.insert("declaredBytes".to_string(), json!(480));
        let outputs = leg["outputs"].as_array_mut().expect("jpg outputs");
        if let Some(first) = outputs.first_mut() {
            if let Some(map) = first.as_object_mut() {
                map.insert("bytes".to_string(), json!(480));
                map.insert("declaredBytes".to_string(), json!(480));
            }
        }
        let result = leg["result"].as_object_mut().expect("jpg result");
        result.insert(
            "meta".to_string(),
            json!("1 file \u{00b7} 480 B \u{00b7} verified locally"),
        );
        leg
    }

    /// Three pages of JPEG rendered on one card come back as one store-method ZIP
    /// whose entries are named `page-N.jpg`.
    fn real_tools_jpg_zip_leg() -> Json {
        let output = json!({
            "kind": "zip",
            "name": "report-pages.zip",
            "pages": 0,
            "declaredBytes": 3600,
            "bytes": 3600,
            "blob": true,
            "previewBlob": Json::Null,
            "downloadName": "report-pages.zip",
            "header": "PK\u{3}\u{4}\u{14}",
            "zip": true,
            "png": false,
            "jpeg": false,
            "overTarget": false,
            "foundCounts": [2, 2, 2],
            "found": [true, true, true],
            "absentFound": [],
        });
        json!({
            "fileRows": 1,
            "runLabel": "Render JPGs",
            "noUpload": "verified",
            "status": "Done. The result was verified on this device.",
            "result": {
                "title": "report-pages.zip",
                "meta": "1 file \u{00b7} 3.5 KiB \u{00b7} verified locally",
                "rows": [
                    { "label": "Pages", "value": "3" },
                    { "label": "Size", "value": "979 B \u{2192} 3.5 KiB" },
                    { "label": "Verification", "value": "ok" },
                ],
            },
            "outputs": [output.clone()],
            "output": output,
            "network": {
                "origin": "http://127.0.0.1:1234",
                "resources": 1,
                "foreign": [],
                "paths": ["/wasm/selis_pdf_wasm_raster.wasm"],
            },
        })
    }

    /// One PDF → TIFF leg: the same lazy chunk renders `pages` pages into ONE
    /// container, so the downloaded Blob is a `II`-ordered TIFF whose IFD walk
    /// counts exactly those pages and reports the RGB Deflate profile the writer
    /// promises. There is no preview link: Chromium does not display TIFF, and a
    /// card that promised one would be promising something the browser refuses.
    fn real_tools_tiff_leg(pages: u64) -> Json {
        let bytes = if pages == 1 { 3_216 } else { 9_648 };
        let output = json!({
            "kind": "image",
            "name": "report.tif",
            "pages": pages,
            "declaredBytes": bytes,
            "bytes": bytes,
            "blob": true,
            "previewBlob": Json::Null,
            "downloadName": "report.tif",
            "header": "II*\u{0}\u{8}",
            "zip": false,
            "png": false,
            "jpeg": false,
            "webp": false,
            "tiff": true,
            "tiffPages": pages,
            "tiffWidth": 200,
            "tiffHeight": 200,
            "tiffProfile": true,
            "chromium": Json::Null,
            "overTarget": false,
            "absentFound": [false],
        });
        json!({
            "fileRows": 1,
            "runLabel": "Render TIFF",
            "noUpload": "verified",
            "status": "Done. The result was verified on this device.",
            "result": {
                "title": "report.tif",
                "meta": format!("1 file \u{00b7} {bytes} B \u{00b7} verified locally"),
                "rows": [
                    { "label": "Pages", "value": pages.to_string() },
                    { "label": "Size", "value": format!("979 B \u{2192} {bytes} B") },
                    { "label": "Verification", "value": "ok" },
                ],
            },
            "outputs": [output.clone()],
            "output": output,
            "network": {
                "origin": "http://127.0.0.1:1234",
                "resources": 1,
                "foreign": [],
                "paths": ["/wasm/selis_pdf_wasm_raster.wasm"],
            },
        })
    }

    /// One PDF → WebP leg: the same lazy chunk renders the page and the real
    /// Blob is a RIFF container whose one chunk is `VP8L`, so the sniff reports
    /// the lossless coding, the canvas the engine asked for and the alpha plane
    /// the PNG keeps. The card previews — Chromium draws a WebP, unlike a TIFF —
    /// and the picture it paints is the one the PNG leg of the same render
    /// produces: two decoders that are not ours agreeing is the only raster proof
    /// that does not run in the writer's favour.
    fn real_tools_webp_leg() -> Json {
        let output = json!({
            "kind": "image",
            "name": "report.webp",
            "pages": 1,
            "declaredBytes": 152,
            "bytes": 152,
            "blob": true,
            "previewBlob": true,
            "downloadName": "report.webp",
            "header": "RIFF\u{fffd}",
            "zip": false,
            "png": false,
            "jpeg": false,
            "tiff": false,
            "webp": true,
            "webpWidth": 200,
            "webpHeight": 200,
            "webpAlpha": false,
            "webpLossless": true,
            "chromium": { "width": 200, "height": 200, "digest": 3_627_224_773_u64, "samples": 160_000 },
            "overTarget": false,
            "foundCounts": [1],
            "found": [true],
            "absentFound": [false],
        });
        json!({
            "fileRows": 1,
            "runLabel": "Render WebPs",
            "noUpload": "verified",
            "status": "Done. The result was verified on this device.",
            "result": {
                "title": "report.webp",
                "meta": "1 file \u{00b7} 152 B \u{00b7} verified locally",
                "rows": [
                    { "label": "Pages", "value": "1" },
                    { "label": "Size", "value": "979 B \u{2192} 152 B" },
                    { "label": "Verification", "value": "ok" },
                ],
            },
            "outputs": [output.clone()],
            "output": output,
            "network": {
                "origin": "http://127.0.0.1:1234",
                "resources": 1,
                "foreign": [],
                "paths": ["/wasm/selis_pdf_wasm_raster.wasm"],
            },
        })
    }

    /// The WebP bundle: three pages, one `page-N.webp` entry each, in one
    /// store-method ZIP — the same shape the PNG bundle owes the user, because
    /// WebP is a per-page format and has no multi-page container here.
    fn real_tools_webp_zip_leg() -> Json {
        let output = json!({
            "kind": "zip",
            "name": "report-pages.zip",
            "pages": 0,
            "declaredBytes": 790,
            "bytes": 790,
            "blob": true,
            "previewBlob": Json::Null,
            "downloadName": "report-pages.zip",
            "header": "PK\u{3}\u{4}\u{14}",
            "zip": true,
            "png": false,
            "webp": false,
            "chromium": Json::Null,
            "overTarget": false,
            "foundCounts": [2, 2, 2],
            "found": [true, true, true],
            "absentFound": [],
        });
        json!({
            "fileRows": 1,
            "runLabel": "Render WebPs",
            "noUpload": "verified",
            "status": "Done. The result was verified on this device.",
            "result": {
                "title": "report-pages.zip",
                "meta": "1 file \u{00b7} 790 B \u{00b7} verified locally",
                "rows": [
                    { "label": "Pages", "value": "3" },
                    { "label": "Size", "value": "979 B \u{2192} 790 B" },
                    { "label": "Verification", "value": "ok" },
                ],
            },
            "outputs": [output.clone()],
            "output": output,
            "network": {
                "origin": "http://127.0.0.1:1234",
                "resources": 1,
                "foreign": [],
                "paths": ["/wasm/selis_pdf_wasm_raster.wasm"],
            },
        })
    }

    /// A PDF-to-HTML leg, as the harness reports it: one document for the whole
    /// selection, and its own walk of the downloaded bytes. `all` is the
    /// every-pages leg — two sections, one carrying the fixture's only word and
    /// one honestly empty — while the other is the single-page selection.
    fn real_tools_html_leg(all: bool) -> Json {
        let section = |number: u64, spans: u64, words: &str| {
            json!({
                "number": number,
                "width": 612,
                "height": 792,
                "style": true,
                "spans": spans,
                "outside": 0,
                "marker": 0,
                "words": words,
            })
        };
        let pages = if all {
            vec![section(1, 1, "Selis"), section(2, 0, "")]
        } else {
            vec![section(1, 1, "Selis")]
        };
        let pages_row = pages.len().to_string();
        let found: Vec<bool> = if all {
            vec![true, true, true]
        } else {
            vec![true]
        };
        let bytes = if all { 1_318 } else { 1_102 };
        let output = json!({
            "kind": "html",
            "name": "outlined.html",
            "pages": pages.len(),
            "declaredBytes": bytes,
            "bytes": bytes,
            "blob": true,
            "previewBlob": Json::Null,
            "downloadName": "outlined.html",
            "header": "<!doc",
            "zip": false,
            "png": false,
            "jpeg": false,
            "tiff": false,
            "html": true,
            "htmlRoots": 1,
            "htmlClosers": 1,
            "htmlDoctypes": 1,
            "htmlMains": 1,
            "htmlTitle": "outlined",
            "htmlPages": pages,
            "overTarget": false,
            "found": found,
            "foundCounts": if all { vec![1u64, 2, 1] } else { vec![1u64] },
            "absentFound": [false],
        });
        json!({
            "fileRows": 1,
            "runLabel": "Export HTML",
            "noUpload": "verified",
            "status": "Done. The result was verified on this device.",
            "result": {
                "title": "outlined.html",
                "meta": format!("1 file \u{00b7} {bytes} B \u{00b7} verified locally"),
                "rows": [
                    { "label": "Pages", "value": pages_row },
                    { "label": "Size", "value": format!("4\u{2009}034 B \u{2192} {bytes} B") },
                    { "label": "Verification", "value": "ok" },
                ],
            },
            "outputs": [output.clone()],
            "output": output,
            "network": {
                "origin": "http://127.0.0.1:1234",
                "resources": 1,
                "foreign": [],
                "paths": ["/wasm/selis_pdf_wasm_html.wasm"],
            },
        })
    }

    /// A leg that ends with one rewritten PDF. `counts` is what the page found in
    /// the downloaded bytes, in the order the leg asked for them.
    fn real_tools_single_leg(
        run_label: &str,
        output_name: &str,
        file_rows: u64,
        pages: u64,
        pages_row: &str,
        counts: &[u64],
    ) -> Json {
        let output = json!({
            "kind": "pdf",
            "name": output_name,
            "pages": pages,
            "declaredBytes": 1039,
            "bytes": 1039,
            "blob": true,
            "previewBlob": true,
            "downloadName": output_name,
            "header": "%PDF-",
            "zip": false,
            "overTarget": false,
            "foundCounts": counts,
            "found": counts.iter().map(|_| true).collect::<Vec<bool>>(),
            "absentFound": [false],
        });
        json!({
            "fileRows": file_rows,
            "runLabel": run_label,
            "noUpload": "verified",
            "status": "Done. The result was verified on this device.",
            "result": {
                "title": output_name,
                "meta": "1 file \u{00b7} 1.0 KiB \u{00b7} verified locally",
                "rows": [
                    { "label": "Pages", "value": pages_row },
                    { "label": "Size", "value": "979 B \u{2192} 1.0 KiB" },
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

    /// The ZIP card's leg: two files picked, one archive back, and no preview —
    /// an archive is not a document the browser should open in its viewer.
    fn real_tools_zip_leg() -> Json {
        let output = json!({
            "kind": "zip",
            "name": "one.zip",
            "pages": 0,
            "declaredBytes": 1610,
            "bytes": 1610,
            "blob": true,
            "previewBlob": Json::Null,
            "downloadName": "one.zip",
            "header": "PK\u{3}\u{4}\u{14}",
            "zip": true,
            "overTarget": false,
            "foundCounts": [1, 2, 2],
            "found": [true, true, true],
            "absentFound": [],
        });
        json!({
            "fileRows": 2,
            "runLabel": "Create ZIP",
            "noUpload": "verified",
            "status": "Done. The result was verified on this device.",
            "result": {
                "title": "one.zip",
                "meta": "1 file \u{00b7} 1.6 KiB \u{00b7} verified locally",
                "rows": [
                    { "label": "Size", "value": "0 B \u{2192} 1.6 KiB" },
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

    /// The batch queue leg: two documents of different sizes and one PDF whose
    /// index the engine cannot read. Each survivor is its own download, the
    /// skipped file is a row with a reason, and the bundle holds both documents.
    fn real_tools_batch_leg() -> Json {
        let first = json!({
            "kind": "pdf",
            "name": "alpha-compressed.pdf",
            "pages": 3,
            "declaredBytes": 900,
            "bytes": 900,
            "blob": true,
            "previewBlob": true,
            "downloadName": "alpha-compressed.pdf",
            "header": "%PDF-",
            "zip": false,
            "foundCounts": [1],
            "absentFound": [false],
        });
        let second = json!({
            "kind": "pdf",
            "name": "beta-compressed.pdf",
            "pages": 1,
            "declaredBytes": 500,
            "bytes": 500,
            "blob": true,
            "previewBlob": true,
            "downloadName": "beta-compressed.pdf",
            "header": "%PDF-",
            "zip": false,
            "foundCounts": [1],
            "absentFound": [false],
        });
        let bundle = json!({
            "kind": "zip",
            "name": "alpha-batch.zip",
            "pages": 0,
            "declaredBytes": 1450,
            "bytes": 1450,
            "blob": true,
            "previewBlob": null,
            "downloadName": "alpha-batch.zip",
            "header": "PK\u{3}\u{4}",
            "zip": true,
            "foundCounts": [2],
            "absentFound": [false],
        });
        json!({
            "fileRows": 3,
            "runLabel": "Run the queue",
            "noUpload": "verified",
            "status": "Done. The result was verified on this device.",
            "result": {
                "title": "2 PDFs and a ZIP",
                "meta": "3 files \u{00b7} 3.0 KiB \u{00b7} verified locally",
                "rows": [
                    {
                        "label": "Queue",
                        "value": "compress \u{00b7} 2 of 3 files \u{00b7} 1 not processed"
                    },
                    {
                        "label": "alpha.pdf",
                        "value": "alpha-compressed.pdf \u{00b7} 3 pages \u{00b7} ok"
                    },
                    {
                        "label": "broken.pdf",
                        "value": "not processed \u{2014} in/1: [E1101] This PDF's index is damaged. (during xref-int) (at byte 9)"
                    },
                    {
                        "label": "beta.pdf",
                        "value": "beta-compressed.pdf \u{00b7} 1 page \u{00b7} ok"
                    },
                    { "label": "Pages", "value": "4 \u{2192} 4" },
                    { "label": "Verification", "value": "partial" },
                ],
            },
            "outputs": [first.clone(), second, bundle],
            "output": first,
            "network": {
                "origin": "http://127.0.0.1:1234",
                "resources": 1,
                "foreign": [],
                "paths": ["/wasm/selis_pdf_wasm_tools.wasm"],
            },
        })
    }

    /// The convert leg: a Markdown file rendered to a one-page PDF, with the
    /// heading font and text present in the downloaded bytes.
    fn real_tools_convert_leg() -> Json {
        let output = json!({
            "kind": "pdf",
            "name": "notes-converted.pdf",
            "pages": 1,
            "declaredBytes": 1500,
            "bytes": 1500,
            "blob": true,
            "previewBlob": true,
            "downloadName": "notes-converted.pdf",
            "header": "%PDF-",
            "zip": false,
            "found": [true, true],
        });
        json!({
            "fileRows": 1,
            "runLabel": "Convert to PDF",
            "noUpload": "verified",
            "status": "Done. The result was verified on this device.",
            "result": {
                "title": "notes-converted.pdf",
                "meta": "1 file \u{00b7} 1.5 kB \u{00b7} verified locally",
                "rows": [
                    { "label": "Pages", "value": "1" },
                    { "label": "Verification", "value": "ok" },
                ],
            },
            "outputs": [output.clone()],
            "output": output,
            "network": {
                "origin": "http://127.0.0.1:1234",
                "resources": 2,
                "foreign": [],
                "paths": [
                    "/wasm/selis_pdf_wasm_tools.wasm",
                    "/wasm/selis_pdf_wasm_convert.wasm",
                ],
            },
        })
    }
    /// One of the convert card's other formats. All eight share the shape and
    /// the network ledger of the Markdown leg: the same lazy chunk, nothing
    /// else fetched, and the needles are each source's own words.
    fn convert_format_leg(title: &str, declared_bytes: u64, found: Vec<bool>, pages: u32) -> Json {
        let output = json!({
            "kind": "pdf",
            "name": title,
            "pages": pages,
            "declaredBytes": declared_bytes,
            "bytes": declared_bytes,
            "blob": true,
            "previewBlob": true,
            "downloadName": title,
            "header": "%PDF-",
            "zip": false,
            "found": found,
        });
        json!({
            "fileRows": 1,
            "runLabel": "Convert to PDF",
            "noUpload": "verified",
            "status": "Done. The result was verified on this device.",
            "result": {
                "title": title,
                "meta": "1 file \u{00b7} 1.5 kB \u{00b7} verified locally",
                "rows": [
                    { "label": "Pages", "value": pages.to_string() },
                    { "label": "Verification", "value": "ok" },
                ],
            },
            "outputs": [output.clone()],
            "output": output,
            "network": {
                "origin": "http://127.0.0.1:1234",
                "resources": 2,
                "foreign": [],
                "paths": [
                    "/wasm/selis_pdf_wasm_tools.wasm",
                    "/wasm/selis_pdf_wasm_convert.wasm",
                ],
            },
        })
    }

    fn real_tools_form_fill_leg() -> Json {
        let output = json!({
            "kind": "pdf",
            "name": "form-filled.pdf",
            "pages": 1,
            "declaredBytes": 2100,
            "bytes": 2100,
            "blob": true,
            "previewBlob": true,
            "downloadName": "form-filled.pdf",
            "header": "%PDF-",
            "zip": false,
            "found": [true, true],
        });
        json!({
            "fileRows": 1,
            "runLabel": "Fill form",
            "noUpload": "verified",
            "status": "Done. The result was verified on this device.",
            "result": {
                "title": "form-filled.pdf",
                "meta": "1 file · 2.1 kB · verified locally",
                "rows": [
                    { "label": "Pages", "value": "1 \u{2192} 1" },
                    { "label": "Verification", "value": "ok" },
                ],
            },
            "outputs": [output.clone()],
            "output": output,
            "network": {
                "origin": "http://127.0.0.1:1234",
                "resources": 2,
                "foreign": [],
                "paths": [
                    "/wasm/selis_pdf_wasm_tools.wasm",
                ],
            },
            "signatureWarn": true,
            "signatureFieldDisabled": true,
            "readOnlyDisabled": true,
            "fieldRows": 7,
        })
    }
    /// The edit-text leg: one one-page invoice whose `900` became `1250`,
    /// written as an incremental update, with the new show-text operator
    /// proven present in the produced bytes.
    fn real_tools_edit_text_leg() -> Json {
        let output = json!({
            "kind": "pdf",
            "name": "edit-edited.pdf",
            "pages": 1,
            "declaredBytes": 790,
            "bytes": 790,
            "blob": true,
            "previewBlob": true,
            "downloadName": "edit-edited.pdf",
            "header": "%PDF-",
            "zip": false,
            "found": [true],
        });
        json!({
            "fileRows": 1,
            "runLabel": "Replace text",
            "noUpload": "verified",
            "status": "Done. The result was verified on this device.",
            "result": {
                "title": "edit-edited.pdf",
                "meta": "1 file · 790 B · verified locally",
                "rows": [
                    { "label": "Pages", "value": "1 → 1" },
                    { "label": "Verification", "value": "ok" },
                ],
            },
            "outputs": [output.clone()],
            "output": output,
            "network": {
                "origin": "http://127.0.0.1:1234",
                "resources": 2,
                "foreign": [],
                "paths": [
                    "/wasm/selis_pdf_wasm_tools.wasm",
                ],
            },
        })
    }
    /// The Phase 3 text leg: one `.txt` plus its quality report, and a report
    /// that proves the run recovered characters rather than a bare separator.
    fn real_tools_text_leg() -> Json {
        json!({
            "fileRows": 1,
            "runLabel": "Extract text",
            "noUpload": "verified",
            "status": "Done. The export was read from the document on this device.",
            "result": {
                "title": "0 PDFs",
                "meta": "2 files \u{00b7} 378 B \u{00b7} verified locally",
                "rows": [
                    { "label": "Verdict", "value": "ok" },
                    { "label": "Pages", "value": "1" },
                    { "label": "Characters", "value": "57" },
                    { "label": "Lines", "value": "4" },
                    { "label": "Pages with text", "value": "1" },
                    { "label": "Verification", "value": "read" },
                ],
            },
            "outputs": [
                {
                    "kind": "text",
                    "name": "notes-converted.txt",
                    "declaredBytes": 85,
                    "bytes": 85,
                    "blob": true,
                    "previewBlob": null,
                    "downloadName": "notes-converted.txt",
                    "header": "--- p",
                    "found": [true, true],
                },
                {
                    "kind": "report",
                    "name": "notes-converted-quality.json",
                    "declaredBytes": 293,
                    "bytes": 293,
                    "blob": true,
                    "previewBlob": null,
                    "downloadName": "notes-converted-quality.json",
                    "found": [false, false],
                },
            ],
            "output": null,
            "network": {
                "origin": "http://127.0.0.1:1234",
                "resources": 3,
                "foreign": [],
                "paths": [
                    "/assets/host/tools/text-protocol.js",
                    "/wasm/selis_pdf_wasm_text.wasm",
                    "/wasm/selis_pdf_wasm_tools.wasm",
                ],
            },
        })
    }
    /// The Phase 3 document legs. Same shape as the text leg: one package,
    /// one Blob, nothing fetched but the runtime-cached chunk. The needles
    /// are written into the package by the writers themselves, so a fixture
    /// that downloads an empty container fails the leg.
    fn real_tools_word_leg() -> Json {
        json!({
            "fileRows": 1,
            "runLabel": "Extract text",
            "noUpload": "verified",
            "status": "Done. The export was read from the document on this device.",
            "result": {
                "title": "notes-converted.docx",
                "meta": "1 file \u{00b7} 2.1 kB \u{00b7} verified locally",
                "rows": [],
            },
            "outputs": [
                {
                    "kind": "file",
                    "name": "notes-converted.docx",
                    "declaredBytes": 2100,
                    "bytes": 2100,
                    "blob": true,
                    "previewBlob": null,
                    "downloadName": "notes-converted.docx",
                    "zip": true,
                    "found": [true, true],
                },
            ],
            "output": null,
            "network": {
                "origin": "http://127.0.0.1:1234",
                "resources": 3,
                "foreign": [],
                "paths": [
                    "/assets/host/tools/text-protocol.js",
                    "/wasm/selis_pdf_wasm_text.wasm",
                    "/wasm/selis_pdf_wasm_tools.wasm",
                ],
            },
        })
    }

    fn real_tools_slides_leg() -> Json {
        json!({
            "fileRows": 1,
            "runLabel": "Extract text",
            "noUpload": "verified",
            "status": "Done. The export was read from the document on this device.",
            "result": {
                "title": "notes-converted.pptx",
                "meta": "1 file \u{00b7} 2.3 kB \u{00b7} verified locally",
                "rows": [],
            },
            "outputs": [
                {
                    "kind": "file",
                    "name": "notes-converted.pptx",
                    "declaredBytes": 2300,
                    "bytes": 2300,
                    "blob": true,
                    "previewBlob": null,
                    "downloadName": "notes-converted.pptx",
                    "zip": true,
                    "found": [true, true],
                },
            ],
            "output": null,
            "network": {
                "origin": "http://127.0.0.1:1234",
                "resources": 3,
                "foreign": [],
                "paths": [
                    "/assets/host/tools/text-protocol.js",
                    "/wasm/selis_pdf_wasm_text.wasm",
                    "/wasm/selis_pdf_wasm_tools.wasm",
                ],
            },
        })
    }

    fn real_tools_workbook_leg() -> Json {
        json!({
            "fileRows": 1,
            "runLabel": "Extract text",
            "noUpload": "verified",
            "status": "Done. The export was read from the document on this device.",
            "result": {
                "title": "table-converted.xlsx",
                "meta": "1 file \u{00b7} 2.0 kB \u{00b7} verified locally",
                "rows": [],
            },
            "outputs": [
                {
                    "kind": "file",
                    "name": "table-converted.xlsx",
                    "declaredBytes": 2000,
                    "bytes": 2000,
                    "blob": true,
                    "previewBlob": null,
                    "downloadName": "table-converted.xlsx",
                    "zip": true,
                    "found": [true, true, true],
                },
            ],
            "output": null,
            "network": {
                "origin": "http://127.0.0.1:1234",
                "resources": 3,
                "foreign": [],
                "paths": [
                    "/assets/host/tools/text-protocol.js",
                    "/wasm/selis_pdf_wasm_text.wasm",
                    "/wasm/selis_pdf_wasm_tools.wasm",
                ],
            },
        })
    }
    fn real_tools_tables_leg() -> Json {
        let csv = json!({
            "kind": "tables",
            "name": "table-converted-tables.csv",
            "declaredBytes": 200,
            "bytes": 200,
            "blob": true,
            "previewBlob": null,
            "downloadName": "table-converted-tables.csv",
            "found": [true, true],
        });
        let report_json = json!({
            "kind": "report",
            "name": "table-converted-tables.json",
            "declaredBytes": 110,
            "bytes": 110,
            "blob": true,
            "previewBlob": null,
            "downloadName": "table-converted-tables.json",
            "found": [true, false],
        });
        json!({
            "fileRows": 1,
            "runLabel": "Extract text",
            "noUpload": "verified",
            "status": "Done. The export was read from the document on this device.",
            "result": {
                "title": "0 PDFs",
                "meta": "2 files \u{00b7} 310 B \u{00b7} verified locally",
                "rows": [
                    { "label": "Tables found", "value": "1" },
                    { "label": "Rows", "value": "2" },
                    { "label": "Lowest confidence", "value": "0.90" },
                    { "label": "Verification", "value": "read" },
                ],
            },
            "outputs": [csv, report_json],
            "output": null,
            "network": {
                "origin": "http://127.0.0.1:1234",
                "resources": 3,
                "foreign": [],
                "paths": [
                    "/assets/host/tools/text-protocol.js",
                    "/wasm/selis_pdf_wasm_text.wasm",
                    "/wasm/selis_pdf_wasm_tools.wasm",
                ],
            },
        })
    }

    fn real_tools_split_text_leg() -> Json {
        let bundle = json!({
            "kind": "zip",
            "name": "chapter-converted-split.zip",
            "declaredBytes": 3400,
            "bytes": 3400,
            "blob": true,
            "previewBlob": null,
            "downloadName": "chapter-converted-split.zip",
            "zip": true,
            "found": [true, true],
        });
        let report_json = json!({
            "kind": "report",
            "name": "chapter-converted-split.json",
            "declaredBytes": 180,
            "bytes": 180,
            "blob": true,
            "previewBlob": null,
            "downloadName": "chapter-converted-split.json",
            "found": [true, false],
        });
        json!({
            "fileRows": 1,
            "runLabel": "Extract text",
            "noUpload": "verified",
            "status": "Done. The export was read from the document on this device.",
            "result": {
                "title": "0 PDFs and a ZIP",
                "meta": "2 files \u{00b7} 3.6 kB \u{00b7} verified locally",
                "rows": [
                    { "label": "Rule", "value": "a new part starts at each matching page" },
                    { "label": "Matched pages", "value": "2" },
                    { "label": "Parts", "value": "2" },
                ],
            },
            "outputs": [bundle, report_json],
            "output": null,
            "network": {
                "origin": "http://127.0.0.1:1234",
                "resources": 3,
                "foreign": [],
                "paths": [
                    "/assets/host/tools/text-protocol.js",
                    "/wasm/selis_pdf_wasm_text.wasm",
                    "/wasm/selis_pdf_wasm_tools.wasm",
                ],
            },
        })
    }

    fn real_tools_reflow_leg() -> Json {
        json!({
            "fileRows": 1,
            "runLabel": "Extract text",
            "noUpload": "verified",
            "status": "Done. The export was read from the document on this device.",
            "result": {
                "title": "notes-converted.html",
                "meta": "1 file · 2.0 kB · verified locally",
                "rows": [],
            },
            "outputs": [
                {
                    "kind": "html",
                    "name": "notes-converted.html",
                    "declaredBytes": 2000,
                    "bytes": 2000,
                    "blob": true,
                    "previewBlob": null,
                    "downloadName": "notes-converted.html",
                    "found": [true, true],
                },
            ],
            "output": null,
            "network": {
                "origin": "http://127.0.0.1:1234",
                "resources": 3,
                "foreign": [],
                "paths": [
                    "/assets/host/tools/text-protocol.js",
                    "/wasm/selis_pdf_wasm_text.wasm",
                    "/wasm/selis_pdf_wasm_tools.wasm",
                ],
            },
        })
    }

    fn real_tools_epub_out_leg() -> Json {
        json!({
            "fileRows": 1,
            "runLabel": "Extract text",
            "noUpload": "verified",
            "status": "Done. The export was read from the document on this device.",
            "result": {
                "title": "notes-converted.epub",
                "meta": "1 file · 2.1 kB · verified locally",
                "rows": [],
            },
            "outputs": [
                {
                    "kind": "file",
                    "name": "notes-converted.epub",
                    "declaredBytes": 2100,
                    "bytes": 2100,
                    "blob": true,
                    "previewBlob": null,
                    "downloadName": "notes-converted.epub",
                    "zip": true,
                    "found": [true, true],
                },
            ],
            "output": null,
            "network": {
                "origin": "http://127.0.0.1:1234",
                "resources": 3,
                "foreign": [],
                "paths": [
                    "/assets/host/tools/text-protocol.js",
                    "/wasm/selis_pdf_wasm_text.wasm",
                    "/wasm/selis_pdf_wasm_tools.wasm",
                ],
            },
        })
    }
    /// The metadata-strip leg: the page-preserving stamp shape, plus the
    /// negative witness the verdict requires (`absentFound` all false).
    fn real_tools_strip_metadata_leg() -> Json {
        json!({
            "fileRows": 1,
            "runLabel": "Remove metadata",
            "noUpload": "verified",
            "status": "Done. The result was verified on this device.",
            "result": {
                "title": "metadata-metadata-removed.pdf",
                "meta": "1 file \u{00b7} 700 B \u{00b7} verified locally",
                "rows": [
                    { "label": "Pages", "value": "3 \u{2192} 3" },
                    { "label": "Verification", "value": "ok" },
                ],
            },
            "outputs": [{
                "kind": "pdf",
                "name": "metadata-metadata-removed.pdf",
                "pages": 3,
                "declaredBytes": 700,
                "bytes": 700,
                "blob": true,
                "previewBlob": true,
                "downloadName": "metadata-metadata-removed.pdf",
                "header": "%PDF-",
                "zip": false,
                "absentFound": [false, false, false, false],
            }],
            "output": {
                "kind": "pdf",
                "name": "metadata-metadata-removed.pdf",
                "pages": 3,
                "declaredBytes": 700,
                "bytes": 700,
                "blob": true,
                "previewBlob": true,
                "downloadName": "metadata-metadata-removed.pdf",
                "header": "%PDF-",
                "zip": false,
                "absentFound": [false, false, false, false],
            },
            "network": {
                "origin": "http://127.0.0.1:1234",
                "resources": 1,
                "foreign": [],
                "paths": ["/wasm/selis_pdf_wasm_tools.wasm"],
            },
        })
    }

    /// The metadata-report leg: one PDF in, one JSON report out, and no PDF
    /// row. The card's rows repeat what the downloaded bytes carry, and the
    /// verdict is the read-only `read`.
    fn real_tools_metadata_report_leg() -> Json {
        let output = json!({
            "kind": "report",
            "name": "notes-metadata.json",
            "pages": 0,
            "declaredBytes": 620,
            "bytes": 620,
            "blob": true,
            "previewBlob": Json::Null,
            "downloadName": "notes-metadata.json",
            "header": "{\n  \"",
            "zip": false,
            "found": [true, true, true, true, true, true],
            "absentFound": [false],
        });
        json!({
            "fileRows": 1,
            "runLabel": "Read metadata",
            "noUpload": "verified",
            "status": "Done. The result was verified on this device.",
            "result": {
                "title": "notes-metadata.json",
                "meta": "1 file \u{00b7} 620 B \u{00b7} verified locally",
                "rows": [
                    { "label": "PDF version", "value": "1.4" },
                    { "label": "Revisions", "value": "1" },
                    { "label": "First page", "value": "200 \u{d7} 200 pt" },
                    { "label": "Author", "value": "Secret Author" },
                    { "label": "Producer", "value": "Secret Producer" },
                    { "label": "Title", "value": "Secret Title" },
                    { "label": "XMP packet", "value": "none" },
                    { "label": "Pages", "value": "3 \u{2192} 3" },
                    { "label": "Annotations", "value": "0 \u{2192} 0" },
                    { "label": "Form fields", "value": "0 \u{2192} 0" },
                    { "label": "Layers", "value": "0 \u{2192} 0" },
                    { "label": "Outline entries", "value": "0 \u{2192} 0" },
                    { "label": "Embedded files", "value": "0 \u{2192} 0" },
                    { "label": "Size", "value": "726 B \u{2192} 726 B" },
                    { "label": "Verification", "value": "read" },
                ],
            },
            "outputs": [output],
            "output": Json::Null,
            "network": {
                "origin": "http://127.0.0.1:1234",
                "resources": 1,
                "foreign": [],
                "paths": ["/wasm/selis_pdf_wasm_tools.wasm"],
            },
        })
    }

    /// The image→PDF leg: two image files become one PDF with two pages, and the
    /// real Blob carries Flate-compressed DeviceRGB image XObjects.
    fn real_tools_image_leg() -> Json {
        let output = json!({
            "kind": "pdf",
            "name": "one-images.pdf",
            "pages": 2,
            "declaredBytes": 2000,
            "bytes": 2000,
            "blob": true,
            "previewBlob": true,
            "downloadName": "one-images.pdf",
            "header": "%PDF-",
            "zip": false,
            "found": [true, true, true],
        });
        json!({
            "fileRows": 2,
            "runLabel": "Create PDF",
            "noUpload": "verified",
            "status": "Done. The result was verified on this device.",
            "result": {
                "title": "one-images.pdf",
                "meta": "1 file \u{00b7} 2.0 KiB \u{00b7} verified locally",
                "rows": [
                    { "label": "Pages", "value": "2" },
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

    /// The image-watermark leg: a PDF plus a PNG, with the embedded XObject
    /// and the placement name present in the downloaded bytes.
    fn real_tools_image_watermark_leg() -> Json {
        let output = json!({
            "kind": "pdf",
            "name": "one-image-watermarked.pdf",
            "pages": 1,
            "declaredBytes": 2000,
            "bytes": 2000,
            "blob": true,
            "previewBlob": true,
            "downloadName": "one-image-watermarked.pdf",
            "header": "%PDF-",
            "zip": false,
            "found": [true, true, true],
        });
        json!({
            "fileRows": 2,
            "runLabel": "Add image watermark",
            "noUpload": "verified",
            "status": "Done. The result was verified on this device.",
            "result": {
                "title": "one-image-watermarked.pdf",
                "meta": "1 file \u{00b7} 2.0 KiB \u{00b7} verified locally",
                "rows": [
                    { "label": "Pages", "value": "1 \u{2192} 1" },
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

    /// The outline leg: a three-page PDF rewritten with a two-entry bookmark
    /// tree, with the root and both titles present in the downloaded bytes.
    fn real_tools_outline_leg() -> Json {
        let output = json!({
            "kind": "pdf",
            "name": "report-outline.pdf",
            "pages": 3,
            "declaredBytes": 1500,
            "bytes": 1500,
            "blob": true,
            "previewBlob": true,
            "downloadName": "report-outline.pdf",
            "header": "%PDF-",
            "zip": false,
            "found": [true, true, true],
        });
        json!({
            "fileRows": 1,
            "runLabel": "Save bookmarks",
            "noUpload": "verified",
            "status": "Done. The result was verified on this device.",
            "result": {
                "title": "report-outline.pdf",
                "meta": "1 file \u{00b7} 1.5 kB \u{00b7} verified locally",
                "rows": [
                    { "label": "Pages", "value": "3 \u{2192} 3" },
                    { "label": "Outline entries", "value": "0 \u{2192} 2" },
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
    /// A password leg built from the same spec the gate holds the page to, so
    /// the fixture cannot drift into describing a card the check would accept for
    /// the wrong reason.
    fn real_tools_security_leg(spec: &SecuritySpec) -> Json {
        let bytes = 5_118;
        let mut rows: Vec<Json> = spec
            .rows
            .iter()
            .map(|(label, value)| json!({ "label": label, "value": value }))
            .collect();
        rows.push(
            json!({ "label": "Size", "value": format!("4\u{2009}812 B \u{2192} {bytes} B") }),
        );
        rows.push(json!({ "label": "Verification", "value": "ok" }));
        let output = json!({
            "kind": "pdf",
            "name": spec.output_name,
            "downloadName": spec.output_name,
            "pages": spec.pages,
            "declaredBytes": bytes,
            "bytes": bytes,
            "blob": true,
            "previewBlob": true,
            "header": "%PDF-",
            "zip": false,
            "png": false,
            "jpeg": false,
            "tiff": false,
            "html": false,
            "overTarget": false,
            "found": vec![true; spec.needles.len()],
            "foundCounts": vec![1u64; spec.needles.len()],
            "absentFound": vec![false; spec.absent.len()],
        });
        json!({
            "fileRows": 1,
            "runLabel": spec.run_label,
            "noUpload": "verified",
            "status": spec.status,
            "result": {
                "title": spec.output_name,
                "meta": format!("1 file \u{00b7} {bytes} B \u{00b7} verified locally"),
                "rows": rows,
            },
            "outputs": [output.clone()],
            "output": output,
            "network": {
                "origin": "http://127.0.0.1:1234",
                "resources": 1,
                "foreign": [],
                "paths": ["/wasm/selis_pdf_wasm_security.wasm"],
            },
        })
    }

    /// A refusal: the card said the engine's sentence and produced nothing.
    fn real_tools_security_refusal(run_label: &str, status: &str) -> Json {
        json!({
            "runLabel": run_label,
            "status": status,
            "refused": true,
            "resultHidden": true,
            "outputRows": 0,
            "network": {
                "origin": "http://127.0.0.1:1234",
                "resources": 1,
                "foreign": [],
                "paths": ["/wasm/selis_pdf_wasm_security.wasm"],
            },
        })
    }

    /// A signing leg built from the same spec the gate holds the page to.
    fn real_tools_sign_leg(spec: &SignDocumentSpec) -> Json {
        let bytes = 5_300;
        let mut rows: Vec<Json> = spec
            .rows
            .iter()
            .map(|(label, value)| json!({ "label": label, "value": value }))
            .collect();
        rows.push(
            json!({ "label": "Size", "value": format!("4\u{2009}812 B \u{2192} {bytes} B") }),
        );
        rows.push(json!({ "label": "Verification", "value": "ok" }));
        let output = json!({
            "kind": "pdf",
            "name": spec.output_name,
            "downloadName": spec.output_name,
            "pages": spec.pages,
            "declaredBytes": bytes,
            "bytes": bytes,
            "blob": true,
            "previewBlob": true,
            "header": "%PDF-",
            "found": vec![true; spec.needles.len()],
            "foundCounts": vec![1u64; spec.needles.len()],
            "absentFound": vec![false; spec.absent.len()],
        });
        json!({
            "fileRows": 1,
            "runLabel": spec.run_label,
            "noUpload": "verified",
            "status": spec.status,
            "result": {
                "title": spec.output_name,
                "meta": format!("1 file \u{00b7} {bytes} B \u{00b7} verified locally"),
                "rows": rows,
            },
            "outputs": [output.clone()],
            "output": output,
            "network": {
                "origin": "http://127.0.0.1:1234",
                "resources": 1,
                "foreign": [],
                "paths": ["/wasm/selis_pdf_wasm_sign.wasm"],
            },
        })
    }

    /// A signature report: rows, no download, one anchor.
    fn real_tools_signature_report_leg(spec: &SignatureReportSpec) -> Json {
        let rows: Vec<Json> = spec
            .rows
            .iter()
            .map(|(label, value)| json!({ "label": label, "value": value }))
            .collect();
        json!({
            "fileRows": 1,
            "runLabel": "Validate signatures",
            "noUpload": "verified",
            "status": spec.status,
            "result": {
                "title": "Signature report",
                "meta": spec.meta,
                "rows": rows,
            },
            "outputs": [],
            "output": Json::Null,
            "network": {
                "origin": "http://127.0.0.1:1234",
                "resources": 1,
                "foreign": [],
                "paths": ["/wasm/selis_pdf_wasm_sign.wasm"],
            },
        })
    }

    /// One generated page's report identity, matching what `openToolPage()`
    /// records in the harness. The values are representative; the gate is about
    /// the slug, path, canonical and the absence of the grid.
    fn by_url_page(slug: &str) -> Json {
        json!({
            "slug": slug,
            "path": format!("/{slug}/"),
            "title": format!("{slug} - Selis Tools"),
            "heading": "Generated page heading",
            "canonical": format!("https://pdf.selis.com/{slug}/"),
            "hasGrid": false,
            "controlled": true,
        })
    }

    /// A leg with the generated page's identity folded in.
    fn by_url_leg(page: Json, mut leg: Json) -> Json {
        leg.as_object_mut()
            .expect("leg object")
            .insert("page".to_string(), page);
        leg
    }

    /// A merge leg as a generated page reports it: two inputs, one PDF out.
    fn real_tools_merge_by_url_leg() -> Json {
        json!({
            "page": by_url_page("merge-pdf"),
            "a11y": { "available": true, "passes": 22, "violations": [] },
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
            "network": {
                "origin": "http://127.0.0.1:1234",
                "resources": 6,
                "foreign": [],
                "paths": [],
            },
        })
    }

    fn real_tools_page_report() -> Json {
        let mut report = json!({
            "schema": SCHEMA,
            "check": "tools-page",
            "fatal": Json::Null,
            "cards": TOOLS_PAGE_CARDS.len(),
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
            "pages": {
                "count": TOOLS_PAGE_CARDS.len(),
                "missing": [],
                "ids": TOOLS_PAGE_CARDS.as_slice(),
                "origin": "https://pdf.selis.com",
            },
            "byUrl": {
                "online": {
                    "merge": real_tools_merge_by_url_leg(),
                    "unlock": by_url_leg(
                        by_url_page("unlock-pdf"),
                        real_tools_security_leg(&UNLOCK_LEG),
                    ),
                    "sign": by_url_leg(
                        by_url_page("sign-pdf"),
                        real_tools_sign_leg(&STAMP_LEG),
                    ),
                },
                "offline": {
                    "merge": real_tools_merge_by_url_leg(),
                    "unknown": {
                        "title": "Selis PDF Viewer - offline",
                        "heading": "Not available offline yet",
                        "body": "Not available offline yetThis copy of the Selis viewer has not finished installing. Reconnect once, then it will open local files with no network at all.",
                        "hasGrid": false,
                        "slug": Json::Null,
                    },
                },
            },
            "sw": {
                "registration": "registered",
                "script": "http://127.0.0.1:1234/sw.js",
                "controlled": true,
            },
            "offline": {
                "cards": TOOLS_PAGE_CARDS.len(),
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
        report["watermarkTiled"] = real_tools_tiled_watermark_leg();
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
        // The lossy tier re-encodes the scanned image, so its output carries a
        // JPEG stream (/DCTDecode). The target search reports `overTarget` on the
        // output row; the reachable 50 KiB target is false, the 1 KiB one true.
        report["compressLossy"] =
            real_tools_stamp_leg("scan-compressed.pdf", "Compress PDF", 1, &["/DCTDecode"]);
        for pointer in ["/outputs/0/bytes", "/outputs/0/declaredBytes"] {
            (*report["compressLossy"].pointer_mut(pointer).unwrap()) = json!(31532);
        }
        report["compressOverTarget"] =
            real_tools_stamp_leg("scan-compressed.pdf", "Compress PDF", 1, &["/DCTDecode"]);
        for pointer in ["/outputs/0/bytes", "/outputs/0/declaredBytes"] {
            (*report["compressOverTarget"].pointer_mut(pointer).unwrap()) = json!(7685);
        }
        for pointer in ["/outputs/0/overTarget", "/output/overTarget"] {
            (*report["compressOverTarget"].pointer_mut(pointer).unwrap()) = json!(true);
        }
        report["crop"] = real_tools_stamp_leg("report-cropped.pdf", "Crop PDF", 3, &[]);
        report["flip"] = real_tools_stamp_leg("report-flipped.pdf", "Flip PDF", 3, &[]);
        report["pageLabels"] = real_tools_stamp_leg(
            "report-labeled.pdf",
            "Apply page labels",
            3,
            &["/PageLabels", "(A-)"],
        );
        report["resize"] = real_tools_stamp_leg(
            "report-resized.pdf",
            "Resize pages",
            3,
            &["595.28", "841.89"],
        );
        report["nup"] = real_tools_count_leg(
            "report-n-up.pdf",
            "Place pages",
            3,
            2,
            &["/Subtype /Form", "/Type /XObject"],
        );
        report["attachments"] = real_tools_attachments_leg();
        report["attach"] = real_tools_attach_leg();
        report["detach"] = real_tools_detach_leg();
        report["repair"] = real_tools_repair_leg();
        report["batch"] = real_tools_batch_leg();
        report["rotate"] = real_tools_single_leg(
            "Rotate PDF",
            "report-rotated.pdf",
            1,
            3,
            "3 \u{2192} 3",
            &[3],
        );
        report["delete"] = real_tools_single_leg(
            "Delete pages",
            "report-trimmed.pdf",
            1,
            2,
            "3 \u{2192} 2",
            &[1, 1],
        );
        report["metadata"] = real_tools_single_leg(
            "Save metadata",
            "metadata-metadata.pdf",
            1,
            3,
            "3 \u{2192} 3",
            &[1, 1],
        );
        report["zip"] = real_tools_zip_leg();
        report["png"] = real_tools_png_leg();
        report["pngZip"] = real_tools_png_zip_leg();
        report["jpg"] = real_tools_jpg_leg();
        report["jpgCoarse"] = real_tools_jpg_coarse_leg();
        report["jpgZip"] = real_tools_jpg_zip_leg();
        report["tiff"] = real_tools_tiff_leg(1);
        report["tiffThree"] = real_tools_tiff_leg(3);
        report["webp"] = real_tools_webp_leg();
        report["webpZip"] = real_tools_webp_zip_leg();
        report["html"] = real_tools_html_leg(true);
        report["htmlOne"] = real_tools_html_leg(false);
        report["cardIds"] = json!(TOOLS_PAGE_CARDS.as_slice());
        report["clicked"] = json!({
            "online": TOOLS_PAGE_CARDS.as_slice(),
            "offline": TOOLS_PAGE_CARDS.as_slice(),
        });
        report["stripMetadata"] = real_tools_strip_metadata_leg();
        report["metadataReport"] = real_tools_metadata_report_leg();
        report["imageToPdf"] = real_tools_image_leg();
        report["imageWatermark"] = real_tools_image_watermark_leg();
        report["outline"] = real_tools_outline_leg();
        report["convert"] = real_tools_convert_leg();
        // The other eight convert formats share the checker's shape: one PDF,
        // the same lazy chunk (the needle proves the bytes), the new formats'
        // own words, and no fetch beyond it.
        report["latex"] = convert_format_leg("quarto-converted.pdf", 1200, vec![true], 1);
        report["csv"] = convert_format_leg("ledger-converted.pdf", 900, vec![true, true], 1);
        report["svg"] = convert_format_leg("mark-converted.pdf", 1100, vec![true], 1);
        report["epub"] = convert_format_leg("book-converted.pdf", 1300, vec![true], 1);
        report["table"] = convert_format_leg("table-converted.pdf", 1400, vec![true, true], 1);
        report["docx"] =
            convert_format_leg("letter-converted.pdf", 1600, vec![true, true, true], 1);
        report["pptx"] = convert_format_leg("deck-converted.pdf", 1600, vec![true, true, true], 2);
        report["xlsx"] = convert_format_leg("book2-converted.pdf", 1900, vec![true, true, true], 2);
        report["formFill"] = real_tools_form_fill_leg();
        report["editText"] = real_tools_edit_text_leg();
        report["text"] = real_tools_text_leg();
        report["word"] = real_tools_word_leg();
        report["slides"] = real_tools_slides_leg();
        report["workbook"] = real_tools_workbook_leg();
        report["tables"] = real_tools_tables_leg();
        report["splitText"] = real_tools_split_text_leg();
        report["reflow"] = real_tools_reflow_leg();
        report["epubOut"] = real_tools_epub_out_leg();
        report["security"] = json!({
            "protect": real_tools_security_leg(&PROTECT_LEG),
            "permissions": real_tools_security_leg(&PERMISSIONS_LEG),
            "unlockRefusal": real_tools_security_refusal(
                "Remove password",
                "locked.pdf: wrong password (WRONG_PASSWORD)",
            ),
            "unlock": real_tools_security_leg(&UNLOCK_LEG),
            "unlockOwn": real_tools_security_leg(&UNLOCK_OWN_LEG),
            "clearRefusal": real_tools_security_refusal(
                "Clear permissions",
                "locked.pdf: owner password required (WRONG_OWNER_PASSWORD)",
            ),
            "clear": real_tools_security_leg(&CLEAR_LEG),
        });
        report["sign"] = json!({
            "stamp": real_tools_sign_leg(&STAMP_LEG),
            "certificate": real_tools_sign_leg(&CERTIFICATE_LEG),
            "workflow": real_tools_sign_leg(&WORKFLOW_LEG),
            "report": real_tools_signature_report_leg(&SIGNATURE_REPORT_LEG),
        });

        report["offline"]["mix"] = report["mix"].clone();
        report["offline"]["pageNumbers"] = report["pageNumbers"].clone();
        report["offline"]["watermark"] = report["watermark"].clone();
        report["offline"]["watermarkTiled"] = report["watermarkTiled"].clone();
        report["offline"]["headerFooter"] = report["headerFooter"].clone();
        report["offline"]["bates"] = report["bates"].clone();
        report["offline"]["compress"] = report["compress"].clone();
        report["offline"]["compressLossy"] = report["compressLossy"].clone();
        report["offline"]["compressOverTarget"] = report["compressOverTarget"].clone();
        report["offline"]["crop"] = report["crop"].clone();
        report["offline"]["flip"] = report["flip"].clone();
        report["offline"]["pageLabels"] = report["pageLabels"].clone();
        report["offline"]["resize"] = report["resize"].clone();
        report["offline"]["nup"] = report["nup"].clone();
        report["offline"]["attachments"] = report["attachments"].clone();
        report["offline"]["attach"] = report["attach"].clone();
        report["offline"]["detach"] = report["detach"].clone();
        report["offline"]["repair"] = report["repair"].clone();
        report["offline"]["rotate"] = report["rotate"].clone();
        report["offline"]["delete"] = report["delete"].clone();
        report["offline"]["metadata"] = report["metadata"].clone();
        report["offline"]["zip"] = report["zip"].clone();
        report["offline"]["png"] = report["png"].clone();
        report["offline"]["pngZip"] = report["pngZip"].clone();
        report["offline"]["jpg"] = report["jpg"].clone();
        report["offline"]["jpgZip"] = report["jpgZip"].clone();
        report["offline"]["tiff"] = report["tiff"].clone();
        report["offline"]["tiffThree"] = report["tiffThree"].clone();
        report["offline"]["webp"] = report["webp"].clone();
        report["offline"]["webpZip"] = report["webpZip"].clone();
        report["offline"]["html"] = report["html"].clone();
        report["offline"]["htmlOne"] = report["htmlOne"].clone();
        report["offline"]["batch"] = report["batch"].clone();
        report["offline"]["stripMetadata"] = report["stripMetadata"].clone();
        report["offline"]["metadataReport"] = report["metadataReport"].clone();
        report["offline"]["imageToPdf"] = report["imageToPdf"].clone();
        report["offline"]["imageWatermark"] = report["imageWatermark"].clone();
        report["offline"]["outline"] = report["outline"].clone();
        report["offline"]["formFill"] = report["formFill"].clone();
        report["offline"]["editText"] = report["editText"].clone();
        report["offline"]["text"] = report["text"].clone();
        report["offline"]["word"] = report["word"].clone();
        report["offline"]["slides"] = report["slides"].clone();
        report["offline"]["workbook"] = report["workbook"].clone();
        report["offline"]["tables"] = report["tables"].clone();
        report["offline"]["splitText"] = report["splitText"].clone();
        report["offline"]["reflow"] = report["reflow"].clone();
        report["offline"]["epubOut"] = report["epubOut"].clone();
        report["offline"]["latex"] = report["latex"].clone();
        report["offline"]["csv"] = report["csv"].clone();
        report["offline"]["svg"] = report["svg"].clone();
        report["offline"]["epub"] = report["epub"].clone();
        report["offline"]["table"] = report["table"].clone();
        report["offline"]["docx"] = report["docx"].clone();
        report["offline"]["pptx"] = report["pptx"].clone();
        report["offline"]["xlsx"] = report["xlsx"].clone();
        report["offline"]["convert"] = report["convert"].clone();
        report["offline"]["security"] = report["security"].clone();
        report["offline"]["sign"] = report["sign"].clone();
        report
    }

    #[test]
    fn the_verdicts_accept_what_a_real_browser_reported() {
        assert!(verdict_service_worker(&real_service_worker_report()).is_ok());
        assert!(verdict_async_wasm(&real_async_wasm_report()).is_ok());
        assert!(verdict_extension(&real_extension_report()).is_ok());
        // The reason is in the message: a recorded report that no longer
        // satisfies the verdict is a verdict and a fixture that have drifted
        // apart, and "assertion failed" does not say which.
        let tools = verdict_tools_page(&real_tools_page_report());
        assert!(tools.is_ok(), "the recorded tools-page report: {tools:?}");
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
            "cards": TOOLS_PAGE_CARDS.len(),
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
                "watermarkTiled",
                "a run that never tiled a watermark must not pass",
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
            ("crop", "a run that never cropped a page must not pass"),
            ("flip", "a run that never flipped a page must not pass"),
            (
                "pageLabels",
                "a run that never set page labels must not pass",
            ),
            ("resize", "a run that never resized a page must not pass"),
            ("nup", "a run that never placed pages N-up must not pass"),
            (
                "attachments",
                "a run that never extracted attachments must not pass",
            ),
            ("attach", "a run that never embedded a file must not pass"),
            (
                "detach",
                "a run that never removed embedded files must not pass",
            ),
            (
                "stripMetadata",
                "a run that never removed metadata must not pass",
            ),
            (
                "metadataReport",
                "a run that never reported metadata must not pass",
            ),
            (
                "imageToPdf",
                "a run that never converted images must not pass",
            ),
            (
                "imageWatermark",
                "a run that never stamped an image must not pass",
            ),
            ("outline", "a run that never wrote bookmarks must not pass"),
            (
                "batch",
                "a queue that never ran over the picked files must not pass",
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

        let mut word_wrong_kind = report.clone();
        *word_wrong_kind.pointer_mut("/word/outputs/0/kind").unwrap() = json!("zip");
        assert!(
            verdict_tools_page(&word_wrong_kind).is_err(),
            "a Word leg whose package was downloaded as a plain ZIP must not pass"
        );

        let mut slides_missing_needles = report.clone();
        *slides_missing_needles
            .pointer_mut("/slides/outputs/0/found")
            .unwrap() = json!([true, false]);
        assert!(
            verdict_tools_page(&slides_missing_needles).is_err(),
            "a slides leg whose package lost its parts must not pass"
        );

        let mut epub_not_pdf = report.clone();
        *epub_not_pdf.pointer_mut("/epub/outputs/0/header").unwrap() = json!("PK");
        assert!(
            verdict_tools_page(&epub_not_pdf).is_err(),
            "an EPUB convert leg that returned something other than a PDF must not pass"
        );

        let mut table_unrendered = report.clone();
        *table_unrendered
            .pointer_mut("/table/outputs/0/found")
            .unwrap() = json!([true, false]);
        assert!(
            verdict_tools_page(&table_unrendered).is_err(),
            "a pipe-table leg whose table never reached the page must not pass"
        );

        let mut tables_unfound = report.clone();
        *tables_unfound
            .pointer_mut("/tables/outputs/0/found")
            .unwrap() = json!([false, false]);
        assert!(
            verdict_tools_page(&tables_unfound).is_err(),
            "a tables leg whose CSV lost the table's cells must not pass"
        );

        let mut reflow_wrong_kind = report.clone();
        *reflow_wrong_kind
            .pointer_mut("/reflow/outputs/0/kind")
            .unwrap() = json!("file");
        assert!(
            verdict_tools_page(&reflow_wrong_kind).is_err(),
            "a reflow leg whose article came back as a raw file must not pass"
        );

        let mut offline_tables_unfound = report.clone();
        *offline_tables_unfound
            .pointer_mut("/offline/tables/outputs/0/found")
            .unwrap() = json!([false, false]);
        assert!(
            verdict_tools_page(&offline_tables_unfound).is_err(),
            "an offline tables leg whose CSV lost the table's cells must not pass"
        );

        let mut offline_split_one_part = report.clone();
        *offline_split_one_part
            .pointer_mut("/offline/splitText/result/rows/2/value")
            .unwrap() = json!("1");
        assert!(
            verdict_tools_page(&offline_split_one_part).is_err(),
            "an offline split leg that produced one part must not pass"
        );

        let mut offline_latex_not_pdf = report.clone();
        *offline_latex_not_pdf
            .pointer_mut("/offline/latex/outputs/0/header")
            .unwrap() = json!("PK");
        assert!(
            verdict_tools_page(&offline_latex_not_pdf).is_err(),
            "an offline LaTeX leg that returned something other than a PDF must not pass"
        );

        let mut image_wrong_title = report.clone();
        *image_wrong_title
            .pointer_mut("/imageToPdf/result/title")
            .unwrap() = json!("other.pdf");
        assert!(
            verdict_tools_page(&image_wrong_title).is_err(),
            "an image-to-PDF result with the wrong name must not pass"
        );

        let mut image_wrong_pages = report.clone();
        *image_wrong_pages
            .pointer_mut("/imageToPdf/result/rows/0/value")
            .unwrap() = json!("1");
        assert!(
            verdict_tools_page(&image_wrong_pages).is_err(),
            "an image-to-PDF run that dropped a page must not pass"
        );

        let mut image_marker_missing = report.clone();
        *image_marker_missing
            .pointer_mut("/imageToPdf/outputs/0/found/0")
            .unwrap() = json!(false);
        assert!(
            verdict_tools_page(&image_marker_missing).is_err(),
            "an image-to-PDF output without the embedded image marker must not pass"
        );

        let mut no_offline_image = report.clone();
        no_offline_image
            .pointer_mut("/offline")
            .unwrap()
            .as_object_mut()
            .expect("offline object")
            .remove("imageToPdf");
        assert!(
            verdict_tools_page(&no_offline_image).is_err(),
            "an online-only image conversion must not pass the offline gate"
        );

        // Image watermark: a run whose bytes never name the placement, or
        // whose result kept the input's name, must not pass.
        let mut watermark_no_placement = report.clone();
        *watermark_no_placement
            .pointer_mut("/imageWatermark/outputs/0/found/2")
            .unwrap() = json!(false);
        assert!(
            verdict_tools_page(&watermark_no_placement).is_err(),
            "an image watermark without a placement must not pass"
        );

        let mut watermark_wrong_name = report.clone();
        *watermark_wrong_name
            .pointer_mut("/imageWatermark/outputs/0/downloadName")
            .unwrap() = json!("one.pdf");
        assert!(
            verdict_tools_page(&watermark_wrong_name).is_err(),
            "an image watermark that kept the input's name must not pass"
        );

        let mut no_offline_watermark = report.clone();
        no_offline_watermark
            .pointer_mut("/offline")
            .unwrap()
            .as_object_mut()
            .expect("offline object")
            .remove("imageWatermark");
        assert!(
            verdict_tools_page(&no_offline_watermark).is_err(),
            "an online-only image watermark must not pass the offline gate"
        );

        // Outline: a run whose bytes lack a rewritten title, or whose result
        // did not report the new entry count, must not pass.
        let mut outline_missing_title = report.clone();
        *outline_missing_title
            .pointer_mut("/outline/outputs/0/found/1")
            .unwrap() = json!(false);
        assert!(
            verdict_tools_page(&outline_missing_title).is_err(),
            "an outline run whose bytes lack the title must not pass"
        );

        let mut outline_wrong_count = report.clone();
        *outline_wrong_count
            .pointer_mut("/outline/result/rows/1/value")
            .unwrap() = json!("0 \u{2192} 0");
        assert!(
            verdict_tools_page(&outline_wrong_count).is_err(),
            "an outline run that reported no entries must not pass"
        );

        let mut no_offline_outline = report.clone();
        no_offline_outline
            .pointer_mut("/offline")
            .unwrap()
            .as_object_mut()
            .expect("offline object")
            .remove("outline");
        assert!(
            verdict_tools_page(&no_offline_outline).is_err(),
            "an online-only outline edit must not pass the offline gate"
        );

        // Convert: the needles are the rendered heading, so a PDF produced
        // without rendering the Markdown must not pass.
        let mut unrendered = report.clone();
        *unrendered
            .pointer_mut("/convert/outputs/0/found/1")
            .unwrap() = json!(false);
        assert!(
            verdict_tools_page(&unrendered).is_err(),
            "a convert run whose bytes lack the heading text must not pass"
        );

        let mut no_offline_convert = report.clone();
        no_offline_convert
            .pointer_mut("/offline")
            .unwrap()
            .as_object_mut()
            .expect("offline object")
            .remove("convert");
        assert!(
            verdict_tools_page(&no_offline_convert).is_err(),
            "an online-only convert must not pass the offline gate"
        );

        // Form fill: a Blob without the filled text, a hidden signature
        // warning, an enabled signature or read-only field, the wrong field
        // count, or no offline pass must each refuse the run.
        let mut form_fill_missing_value = report.clone();
        *form_fill_missing_value
            .pointer_mut("/formFill/outputs/0/found/0")
            .unwrap() = json!(false);
        assert!(
            verdict_tools_page(&form_fill_missing_value).is_err(),
            "a form fill without the filled text must not pass"
        );

        let mut form_fill_no_warning = report.clone();
        *form_fill_no_warning
            .pointer_mut("/formFill/signatureWarn")
            .unwrap() = json!(false);
        assert!(
            verdict_tools_page(&form_fill_no_warning).is_err(),
            "an undisclosed signature invalidation must not pass"
        );

        let mut form_fill_sign_enabled = report.clone();
        *form_fill_sign_enabled
            .pointer_mut("/formFill/signatureFieldDisabled")
            .unwrap() = json!(false);
        assert!(
            verdict_tools_page(&form_fill_sign_enabled).is_err(),
            "a signature field left fillable must not pass"
        );

        let mut form_fill_readonly_enabled = report.clone();
        *form_fill_readonly_enabled
            .pointer_mut("/formFill/readOnlyDisabled")
            .unwrap() = json!(false);
        assert!(
            verdict_tools_page(&form_fill_readonly_enabled).is_err(),
            "a read-only field left fillable must not pass"
        );

        let mut form_fill_wrong_rows = report.clone();
        *form_fill_wrong_rows
            .pointer_mut("/formFill/fieldRows")
            .unwrap() = json!(6);
        assert!(
            verdict_tools_page(&form_fill_wrong_rows).is_err(),
            "a fill leg that rendered the wrong field count must not pass"
        );

        let mut no_offline_form = report.clone();
        no_offline_form
            .pointer_mut("/offline")
            .unwrap()
            .as_object_mut()
            .expect("offline object")
            .remove("formFill");
        assert!(
            verdict_tools_page(&no_offline_form).is_err(),
            "an online-only form fill must not pass the offline gate"
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

        let mut attach_marker_missing = report.clone();
        *attach_marker_missing
            .pointer_mut("/attach/outputs/0/found/1")
            .unwrap() = json!(false);
        assert!(
            verdict_tools_page(&attach_marker_missing).is_err(),
            "an attach output without the embedded file name must not pass"
        );

        let mut attach_count_unchanged = report.clone();
        *attach_count_unchanged
            .pointer_mut("/attach/result/rows/1/value")
            .unwrap() = json!("0 \u{2192} 0");
        assert!(
            verdict_tools_page(&attach_count_unchanged).is_err(),
            "an attach run that embedded nothing must not pass"
        );

        let mut detach_payload_left = report.clone();
        *detach_payload_left
            .pointer_mut("/detach/outputs/0/absentFound/0")
            .unwrap() = json!(true);
        assert!(
            verdict_tools_page(&detach_payload_left).is_err(),
            "a detach output that still carries /EmbeddedFiles must not pass"
        );

        let mut no_offline_attach = report.clone();
        no_offline_attach
            .pointer_mut("/offline")
            .unwrap()
            .as_object_mut()
            .expect("offline object")
            .remove("attach");
        assert!(
            verdict_tools_page(&no_offline_attach).is_err(),
            "an online-only attach must not pass the offline gate"
        );

        let mut no_offline_detach = report.clone();
        no_offline_detach
            .pointer_mut("/offline")
            .unwrap()
            .as_object_mut()
            .expect("offline object")
            .remove("detach");
        assert!(
            verdict_tools_page(&no_offline_detach).is_err(),
            "an online-only detach must not pass the offline gate"
        );

        let mut repair_marker_left = report.clone();
        *repair_marker_left
            .pointer_mut("/repair/outputs/0/absentFound/0")
            .unwrap() = json!(true);
        assert!(
            verdict_tools_page(&repair_marker_left).is_err(),
            "a repaired output that still carries the damaged index must not pass"
        );

        let mut repair_dropped_a_page = report.clone();
        *repair_dropped_a_page
            .pointer_mut("/repair/result/rows/0/value")
            .unwrap() = json!("3 \u{2192} 2");
        assert!(
            verdict_tools_page(&repair_dropped_a_page).is_err(),
            "a repair that dropped a page must not pass"
        );

        let mut repair_hid_the_strategy = report.clone();
        *repair_hid_the_strategy
            .pointer_mut("/repair/result/rows/1/value")
            .unwrap() = json!("strict");
        assert!(
            verdict_tools_page(&repair_hid_the_strategy).is_err(),
            "a repair that did not report the reconstructed strategy must not pass"
        );

        let mut no_offline_repair = report.clone();
        no_offline_repair
            .pointer_mut("/offline")
            .unwrap()
            .as_object_mut()
            .expect("offline object")
            .remove("repair");
        assert!(
            verdict_tools_page(&no_offline_repair).is_err(),
            "an online-only repair must not pass the offline gate"
        );

        let mut resize_no_target = report.clone();
        *resize_no_target
            .pointer_mut("/resize/outputs/0/found/0")
            .unwrap() = json!(false);
        assert!(
            verdict_tools_page(&resize_no_target).is_err(),
            "a resize output without the A4 MediaBox must not pass"
        );

        let mut no_offline_resize = report.clone();
        no_offline_resize
            .pointer_mut("/offline")
            .unwrap()
            .as_object_mut()
            .expect("offline object")
            .remove("resize");
        assert!(
            verdict_tools_page(&no_offline_resize).is_err(),
            "an online-only resize must not pass the offline gate"
        );

        let mut no_by_url = report.clone();
        no_by_url
            .as_object_mut()
            .expect("report object")
            .remove("byUrl");
        assert!(
            verdict_tools_page(&no_by_url).is_err(),
            "a run that never opened a generated page by URL must not pass"
        );

        let mut missing_page = report.clone();
        *missing_page.pointer_mut("/pages/missing").unwrap() = json!(["unlock-pdf"]);
        assert!(
            verdict_tools_page(&missing_page).is_err(),
            "a registry entry with no generated page must not pass"
        );

        let mut grid_on_generated = report.clone();
        *grid_on_generated
            .pointer_mut("/byUrl/online/sign/page/hasGrid")
            .unwrap() = json!(true);
        assert!(
            verdict_tools_page(&grid_on_generated).is_err(),
            "a generated page that carried the card grid must not pass"
        );

        let mut bad_merge_header = report.clone();
        *bad_merge_header
            .pointer_mut("/byUrl/online/merge/output/header")
            .unwrap() = json!("NOPE");
        assert!(
            verdict_tools_page(&bad_merge_header).is_err(),
            "a by-URL merge whose download is not a PDF must not pass"
        );

        let mut soft_404 = report.clone();
        *soft_404
            .pointer_mut("/byUrl/offline/unknown/heading")
            .unwrap() = json!("Selis Tools");
        assert!(
            verdict_tools_page(&soft_404).is_err(),
            "an unknown offline path answered with a cached tool page must not pass"
        );

        let mut a11y_unrun = report.clone();
        *a11y_unrun
            .pointer_mut("/byUrl/online/merge/a11y/available")
            .unwrap() = json!(false);
        assert!(
            verdict_tools_page(&a11y_unrun).is_err(),
            "a generated page whose axe scan never ran must not pass"
        );

        let mut a11y_dirty = report.clone();
        *a11y_dirty
            .pointer_mut("/byUrl/online/merge/a11y/violations")
            .unwrap() = json!([{ "id": "color-contrast", "impact": "serious", "help": "Fix contrast", "nodes": 1, "firstTarget": "#workspace-title" }]);
        assert!(
            verdict_tools_page(&a11y_dirty).is_err(),
            "a generated page with an axe violation must not pass"
        );

        let mut nup_no_form = report.clone();
        *nup_no_form.pointer_mut("/nup/outputs/0/found/0").unwrap() = json!(false);
        assert!(
            verdict_tools_page(&nup_no_form).is_err(),
            "an N-up output without a Form XObject must not pass"
        );

        let mut no_offline_nup = report.clone();
        no_offline_nup
            .pointer_mut("/offline")
            .unwrap()
            .as_object_mut()
            .expect("offline object")
            .remove("nup");
        assert!(
            verdict_tools_page(&no_offline_nup).is_err(),
            "an online-only N-up run must not pass the offline gate"
        );

        let mut metadata_left_behind = report.clone();
        *metadata_left_behind
            .pointer_mut("/stripMetadata/outputs/0/absentFound/0")
            .unwrap() = json!(true);
        assert!(
            verdict_tools_page(&metadata_left_behind).is_err(),
            "an output that still contains /Info must not pass metadata removal"
        );

        let mut metadata_wrong_count = report.clone();
        metadata_wrong_count
            .pointer_mut("/stripMetadata/outputs/0/absentFound")
            .unwrap()
            .as_array_mut()
            .expect("absence checks")
            .pop();
        assert!(
            verdict_tools_page(&metadata_wrong_count).is_err(),
            "a metadata leg that skipped an absence check must not pass"
        );

        let mut no_offline_metadata = report.clone();
        no_offline_metadata
            .pointer_mut("/offline")
            .unwrap()
            .as_object_mut()
            .expect("offline object")
            .remove("stripMetadata");
        assert!(
            verdict_tools_page(&no_offline_metadata).is_err(),
            "an online-only metadata removal must not pass the offline gate"
        );

        let mut report_held_the_document = report.clone();
        *report_held_the_document
            .pointer_mut("/metadataReport/outputs/0/absentFound/0")
            .unwrap() = json!(true);
        assert!(
            verdict_tools_page(&report_held_the_document).is_err(),
            "a metadata report that carried document bytes must not pass"
        );

        let mut report_lost_a_field = report.clone();
        *report_lost_a_field
            .pointer_mut("/metadataReport/outputs/0/found/4")
            .unwrap() = json!(false);
        assert!(
            verdict_tools_page(&report_lost_a_field).is_err(),
            "a metadata report that never carried the /Info value must not pass"
        );

        let mut report_offered_a_pdf = report.clone();
        *report_offered_a_pdf
            .pointer_mut("/metadataReport/output")
            .unwrap() = json!({ "kind": "pdf", "name": "notes.pdf" });
        assert!(
            verdict_tools_page(&report_offered_a_pdf).is_err(),
            "a read that produced no PDF must not present one to preview"
        );

        let mut report_claimed_a_rewrite = report.clone();
        *report_claimed_a_rewrite
            .pointer_mut("/metadataReport/runLabel")
            .unwrap() = json!("Write metadata");
        assert!(
            verdict_tools_page(&report_claimed_a_rewrite).is_err(),
            "a card that offered to write metadata must not pass a read-only leg"
        );

        let mut no_offline_report = report.clone();
        no_offline_report
            .pointer_mut("/offline")
            .unwrap()
            .as_object_mut()
            .expect("offline object")
            .remove("metadataReport");
        assert!(
            verdict_tools_page(&no_offline_report).is_err(),
            "an online-only metadata report must not pass the offline gate"
        );

        let mut queue_dropped_the_failed_row = report.clone();
        queue_dropped_the_failed_row
            .pointer_mut("/batch/result/rows")
            .unwrap()
            .as_array_mut()
            .expect("batch rows")
            .remove(2);
        assert!(
            verdict_tools_page(&queue_dropped_the_failed_row).is_err(),
            "a queue that hides the file it skipped must not pass"
        );

        let mut queue_claimed_every_file_ran = report.clone();
        *queue_claimed_every_file_ran
            .pointer_mut("/batch/result/rows/0/value")
            .unwrap() = json!("compress \u{00b7} 3 of 3 files");
        assert!(
            verdict_tools_page(&queue_claimed_every_file_ran).is_err(),
            "a queue may not report a file that never ran as processed"
        );

        let mut queue_gave_two_files_one_name = report.clone();
        *queue_gave_two_files_one_name
            .pointer_mut("/batch/outputs/1/name")
            .unwrap() = json!("alpha-compressed.pdf");
        assert!(
            verdict_tools_page(&queue_gave_two_files_one_name).is_err(),
            "two results sharing one name must not pass: the queue would be handing back one document twice"
        );

        let mut queue_borrowed_a_neighbour_count = report.clone();
        *queue_borrowed_a_neighbour_count
            .pointer_mut("/batch/outputs/1/pages")
            .unwrap() = json!(3);
        assert!(
            verdict_tools_page(&queue_borrowed_a_neighbour_count).is_err(),
            "a 1-page document reported with its neighbour's page count must not pass"
        );

        let mut bundle_held_one_document = report.clone();
        *bundle_held_one_document
            .pointer_mut("/batch/outputs/2/foundCounts/0")
            .unwrap() = json!(1);
        assert!(
            verdict_tools_page(&bundle_held_one_document).is_err(),
            "a bundle holding one of the two survivors must not pass"
        );

        let mut survivor_carried_the_broken_bytes = report.clone();
        *survivor_carried_the_broken_bytes
            .pointer_mut("/batch/outputs/0/absentFound/0")
            .unwrap() = json!(true);
        assert!(
            verdict_tools_page(&survivor_carried_the_broken_bytes).is_err(),
            "a result carrying the failed file's bytes must not pass"
        );

        let mut no_offline_queue = report.clone();
        no_offline_queue
            .pointer_mut("/offline")
            .unwrap()
            .as_object_mut()
            .expect("offline object")
            .remove("batch");
        assert!(
            verdict_tools_page(&no_offline_queue).is_err(),
            "an online-only batch queue must not pass the offline gate"
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

        let mut tiled_stamped_once = report.clone();
        *tiled_stamped_once
            .pointer_mut("/watermarkTiled/outputs/0/foundCounts/0")
            .unwrap() = json!(1);
        assert!(
            verdict_tools_page(&tiled_stamped_once).is_err(),
            "a tiled watermark that stamped the line once must not pass"
        );

        let mut tiled_no_counts = report.clone();
        tiled_no_counts
            .pointer_mut("/watermarkTiled/outputs/0")
            .unwrap()
            .as_object_mut()
            .expect("output")
            .remove("foundCounts");
        assert!(
            verdict_tools_page(&tiled_no_counts).is_err(),
            "a tiled watermark without an occurrence count must not pass"
        );

        let mut no_offline_tiled = report.clone();
        no_offline_tiled
            .pointer_mut("/offline")
            .unwrap()
            .as_object_mut()
            .expect("offline object")
            .remove("watermarkTiled");
        assert!(
            verdict_tools_page(&no_offline_tiled).is_err(),
            "an online-only tiled watermark must not pass the offline gate"
        );

        let mut rotated_one_page = report.clone();
        *rotated_one_page
            .pointer_mut("/rotate/outputs/0/foundCounts/0")
            .unwrap() = json!(1);
        assert!(
            verdict_tools_page(&rotated_one_page).is_err(),
            "a rotate that turned only the page it found first must not pass"
        );

        let mut delete_kept_the_page = report.clone();
        *delete_kept_the_page
            .pointer_mut("/delete/result/rows/0/value")
            .unwrap() = json!("3 \u{2192} 3");
        assert!(
            verdict_tools_page(&delete_kept_the_page).is_err(),
            "a delete that left the page in the tree must not pass"
        );

        let mut metadata_left_the_old_title = report.clone();
        *metadata_left_the_old_title
            .pointer_mut("/metadata/outputs/0/absentFound/0")
            .unwrap() = json!(true);
        assert!(
            verdict_tools_page(&metadata_left_the_old_title).is_err(),
            "a metadata save that still carries the title it replaced must not pass"
        );

        let mut archive_previewed = report.clone();
        *archive_previewed
            .pointer_mut("/zip/outputs/0/previewBlob")
            .unwrap() = json!(true);
        assert!(
            verdict_tools_page(&archive_previewed).is_err(),
            "an archive handed to the PDF viewer as a preview must not pass"
        );

        let mut no_offline_zip = report.clone();
        no_offline_zip
            .pointer_mut("/offline")
            .unwrap()
            .as_object_mut()
            .expect("offline object")
            .remove("zip");
        assert!(
            verdict_tools_page(&no_offline_zip).is_err(),
            "an online-only ZIP must not pass the offline gate"
        );

        let mut never_opened_rotate = report.clone();
        *never_opened_rotate.pointer_mut("/clicked/online").unwrap() =
            json!(["metadata", "batch", "zip"]);
        assert!(
            verdict_tools_page(&never_opened_rotate).is_err(),
            "a run that left cards unclicked must not pass, even with every other leg green"
        );

        let mut offline_never_opened_zip = report.clone();
        *offline_never_opened_zip
            .pointer_mut("/clicked/offline")
            .unwrap() = json!(["rotate", "delete", "metadata"]);
        assert!(
            verdict_tools_page(&offline_never_opened_zip).is_err(),
            "an offline pass that clicked three cards must not cover the grid"
        );

        let mut grid_lost_a_card = report.clone();
        *grid_lost_a_card.pointer_mut("/cardIds").unwrap() =
            json!(["merge", "mix", "split", "zip"]);
        assert!(
            verdict_tools_page(&grid_lost_a_card).is_err(),
            "a grid that rendered fewer cards than the check covers must not pass"
        );

        let mut png_has_no_image_chunks = report.clone();
        *png_has_no_image_chunks
            .pointer_mut("/png/outputs/0/found/0")
            .unwrap() = json!(false);
        assert!(
            verdict_tools_page(&png_has_no_image_chunks).is_err(),
            "a PNG leg whose bytes carry no IHDR must not pass"
        );

        let mut png_not_previewed = report.clone();
        *png_not_previewed
            .pointer_mut("/png/outputs/0/previewBlob")
            .unwrap() = json!(false);
        assert!(
            verdict_tools_page(&png_not_previewed).is_err(),
            "a rendered page offered only as a download must not pass"
        );

        let mut png_blank_canvas = report.clone();
        *png_blank_canvas
            .pointer_mut("/png/outputs/0/pngWidth")
            .unwrap() = json!(0);
        assert!(
            verdict_tools_page(&png_blank_canvas).is_err(),
            "a PNG reporting a zero canvas must not pass"
        );

        let mut png_still_carries_pdf = report.clone();
        *png_still_carries_pdf
            .pointer_mut("/png/outputs/0/absentFound/0")
            .unwrap() = json!(true);
        assert!(
            verdict_tools_page(&png_still_carries_pdf).is_err(),
            "an \"image\" that is still the page's PDF syntax must not pass"
        );

        let mut bundle_lost_a_page = report.clone();
        *bundle_lost_a_page
            .pointer_mut("/pngZip/outputs/0/found/1")
            .unwrap() = json!(false);
        assert!(
            verdict_tools_page(&bundle_lost_a_page).is_err(),
            "a bundle holding two of three rendered pages must not pass"
        );

        let mut bundle_previewed = report.clone();
        *bundle_previewed
            .pointer_mut("/pngZip/outputs/0/previewBlob")
            .unwrap() = json!(true);
        assert!(
            verdict_tools_page(&bundle_previewed).is_err(),
            "an image bundle handed to the viewer as a preview must not pass"
        );

        let mut no_offline_png = report.clone();
        no_offline_png
            .pointer_mut("/offline")
            .unwrap()
            .as_object_mut()
            .expect("offline object")
            .remove("png");
        assert!(
            verdict_tools_page(&no_offline_png).is_err(),
            "an online-only PNG must not pass the offline gate"
        );

        let mut no_offline_png_bundle = report.clone();
        no_offline_png_bundle
            .pointer_mut("/offline")
            .unwrap()
            .as_object_mut()
            .expect("offline object")
            .remove("pngZip");
        assert!(
            verdict_tools_page(&no_offline_png_bundle).is_err(),
            "an online-only PNG bundle must not pass the offline gate"
        );

        let mut jpg_truncated = report.clone();
        *jpg_truncated.pointer_mut("/jpg/outputs/0/jpeg").unwrap() = json!(false);
        assert!(
            verdict_tools_page(&jpg_truncated).is_err(),
            "a JPEG leg whose bytes do not read back as a complete JPEG must not pass"
        );

        let mut jpg_progressive = report.clone();
        *jpg_progressive
            .pointer_mut("/jpg/outputs/0/jpegBaseline")
            .unwrap() = json!(false);
        assert!(
            verdict_tools_page(&jpg_progressive).is_err(),
            "a progressive frame is not the baseline sequential JPEG the chunk writes"
        );

        let mut jpg_blank_canvas = report.clone();
        *jpg_blank_canvas
            .pointer_mut("/jpg/outputs/0/jpegWidth")
            .unwrap() = json!(0);
        assert!(
            verdict_tools_page(&jpg_blank_canvas).is_err(),
            "a JPEG reporting a zero canvas must not pass"
        );

        let mut jpg_bundle_lost_a_page = report.clone();
        *jpg_bundle_lost_a_page
            .pointer_mut("/jpgZip/outputs/0/found/2")
            .unwrap() = json!(false);
        assert!(
            verdict_tools_page(&jpg_bundle_lost_a_page).is_err(),
            "a JPEG bundle holding two of three rendered pages must not pass"
        );

        let mut jpg_quality_changed_nothing = report.clone();
        *jpg_quality_changed_nothing
            .pointer_mut("/jpgCoarse/outputs/0/bytes")
            .unwrap() = json!(1200);
        assert!(
            verdict_tools_page(&jpg_quality_changed_nothing).is_err(),
            "a quality knob that produced the same bytes as the default must not pass"
        );

        let mut no_offline_jpg = report.clone();
        no_offline_jpg
            .pointer_mut("/offline")
            .unwrap()
            .as_object_mut()
            .expect("offline object")
            .remove("jpg");
        assert!(
            verdict_tools_page(&no_offline_jpg).is_err(),
            "an online-only JPG must not pass the offline gate"
        );

        let mut no_offline_jpg_bundle = report.clone();
        no_offline_jpg_bundle
            .pointer_mut("/offline")
            .unwrap()
            .as_object_mut()
            .expect("offline object")
            .remove("jpgZip");
        assert!(
            verdict_tools_page(&no_offline_jpg_bundle).is_err(),
            "an online-only JPG bundle must not pass the offline gate"
        );

        let mut webp_not_lossless = report.clone();
        *webp_not_lossless
            .pointer_mut("/webp/outputs/0/webpLossless")
            .unwrap() = json!(false);
        assert!(
            verdict_tools_page(&webp_not_lossless).is_err(),
            "a WebP whose chunk is not the VP8L lossless stream this writer makes must not pass"
        );

        let mut webp_blank_canvas = report.clone();
        *webp_blank_canvas
            .pointer_mut("/webp/outputs/0/webpHeight")
            .unwrap() = json!(0);
        assert!(
            verdict_tools_page(&webp_blank_canvas).is_err(),
            "a WebP reporting a zero canvas must not pass"
        );

        let mut webp_painted_elsewhere = report.clone();
        *webp_painted_elsewhere
            .pointer_mut("/webp/outputs/0/chromium/digest")
            .unwrap() = json!(1_u64);
        assert!(
            verdict_tools_page(&webp_painted_elsewhere).is_err(),
            "a WebP that Chromium paints differently from the PNG of the same render must not pass"
        );

        let mut webp_bundle_lost_a_page = report.clone();
        *webp_bundle_lost_a_page
            .pointer_mut("/webpZip/outputs/0/found/1")
            .unwrap() = json!(false);
        assert!(
            verdict_tools_page(&webp_bundle_lost_a_page).is_err(),
            "a WebP bundle holding two of three rendered pages must not pass"
        );

        let mut no_offline_webp = report.clone();
        no_offline_webp
            .pointer_mut("/offline")
            .unwrap()
            .as_object_mut()
            .expect("offline object")
            .remove("webp");
        assert!(
            verdict_tools_page(&no_offline_webp).is_err(),
            "an online-only WebP must not pass the offline gate"
        );

        let mut no_offline_webp_bundle = report.clone();
        no_offline_webp_bundle
            .pointer_mut("/offline")
            .unwrap()
            .as_object_mut()
            .expect("offline object")
            .remove("webpZip");
        assert!(
            verdict_tools_page(&no_offline_webp_bundle).is_err(),
            "an online-only WebP bundle must not pass the offline gate"
        );

        // ── The four password cards ───────────────────────────────────────────
        // A card that claims an envelope it did not write.
        let mut unprotected = report.clone();
        *unprotected
            .pointer_mut("/security/protect/outputs/0/found/0")
            .unwrap() = json!(false);
        assert!(
            verdict_tools_page(&unprotected).is_err(),
            "a protect run whose download carries no /Encrypt must not pass"
        );

        // A grant the writer did not apply, dressed up as one the reader has.
        let mut overclaimed_grant = report.clone();
        *overclaimed_grant
            .pointer_mut("/security/protect/result/rows/1/value")
            .unwrap() = json!("print, copy, modify");
        assert!(
            verdict_tools_page(&overclaimed_grant).is_err(),
            "a protect card may not claim a permission the flag does not grant"
        );

        // The card's own sentence about what it did, contradicted.
        let mut unprotected_status = report.clone();
        *unprotected_status
            .pointer_mut("/security/permissions/status")
            .unwrap() = json!(
            "Done. Protected with AES-256 and verified on this device. \
                              Granted: print, copy."
        );
        assert!(
            verdict_tools_page(&unprotected_status).is_err(),
            "the set-permissions card must not speak with the protect card's sentence"
        );

        let mut verification_failed = report.clone();
        *verification_failed
            .pointer_mut("/security/clear/result/rows/3/value")
            .unwrap() = json!("mismatch");
        assert!(
            verdict_tools_page(&verification_failed).is_err(),
            "a clear-permissions run that did not verify must not pass"
        );

        // The unlock that leaves the envelope behind is the failure this card is
        // judged on: the reader would still be asked for a password.
        let mut still_encrypted = report.clone();
        *still_encrypted
            .pointer_mut("/security/unlock/outputs/0/absentFound/0")
            .unwrap() = json!(true);
        assert!(
            verdict_tools_page(&still_encrypted).is_err(),
            "an unlocked output that still carries /Encrypt must not pass"
        );

        // A refusal that produced a file anyway — the worst thing a password
        // card can do is hand back a document it could not legitimately open.
        let mut refusal_produced_a_file = report.clone();
        *refusal_produced_a_file
            .pointer_mut("/security/unlockRefusal/outputRows")
            .unwrap() = json!(1);
        assert!(
            verdict_tools_page(&refusal_produced_a_file).is_err(),
            "a wrong password must produce no download"
        );

        // The refusal has to be the engine's, not a generic failure that happens
        // to mention a password.
        let mut vague_refusal = report.clone();
        *vague_refusal
            .pointer_mut("/security/unlockRefusal/status")
            .unwrap() = json!("The tool could not finish.");
        assert!(
            verdict_tools_page(&vague_refusal).is_err(),
            "an unlock failure that does not name WRONG_PASSWORD must not pass"
        );

        let mut leaked_refusal = report.clone();
        *leaked_refusal
            .pointer_mut("/security/clearRefusal/status")
            .unwrap() =
            json!("locked.pdf: owner password required (WRONG_OWNER_PASSWORD); given: selis-open");
        assert!(
            verdict_tools_page(&leaked_refusal).is_err(),
            "a refusal must not repeat the password it was handed"
        );

        let mut no_offline_security = report.clone();
        no_offline_security
            .pointer_mut("/offline")
            .unwrap()
            .as_object_mut()
            .expect("offline object")
            .remove("security");
        assert!(
            verdict_tools_page(&no_offline_security).is_err(),
            "an online-only password run must not pass the offline gate"
        );

        let mut no_offline_clear = report.clone();
        no_offline_clear
            .pointer_mut("/offline/security")
            .unwrap()
            .as_object_mut()
            .expect("security object")
            .remove("clear");
        assert!(
            verdict_tools_page(&no_offline_clear).is_err(),
            "an online-only clear-permissions must not pass the offline gate"
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

    /// A generated tool page is a directory, not a file, and the harness has to
    /// serve it the same way the deployment host will.
    #[test]
    fn a_directory_url_resolves_to_its_index_page() {
        let root = std::env::temp_dir().join(format!("selis-index-test-{}", std::process::id()));
        let page = root.join("merge-pdf");
        std::fs::create_dir_all(&page).unwrap();
        std::fs::write(page.join("index.html"), b"<h1>Merge PDF</h1>").unwrap();

        for target in ["/merge-pdf/", "/merge-pdf", "/merge-pdf/index.html"] {
            let found = resolve(&root, target).expect("a directory URL must resolve");
            assert!(
                found.ends_with("merge-pdf/index.html") || found.ends_with("merge-pdf\\index.html"),
                "{target} resolved to {}",
                found.display()
            );
        }
        // A missing slug has no directory to index, so the served file does
        // not exist and the origin answers a real 404 rather than the tools
        // page (the soft-404 the plan calls out).
        let missing = resolve(&root, "/no-such-tool/").expect("a path is still a path");
        assert!(!missing.exists(), "{} must not exist", missing.display());

        let _ = std::fs::remove_dir_all(&root);
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
