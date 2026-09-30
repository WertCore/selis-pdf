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
use std::net::{Shutdown, TcpListener, TcpStream};
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
/// Two, deliberately. `service-worker` is the WEB.02 payload: real lifecycle
/// event delivery, `clients.claim()`, and browser-enforced `respondWith`, over
/// a real origin. `async-wasm` is the cheap proof that this harness itself
/// works end to end — a page whose verdict is only written after an awaited
/// `WebAssembly.instantiate` of a module committed beside it, run by a
/// different code generator than the native engine. It costs a second or two,
/// so unlike SL-4.WASM.08's 25-minute leg it can run on every commit.
const CHECKS: [Check; 3] = [
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
    // `app-shell` drives the app's OWN worker, so the built app has to sit on
    // the same origin as the page.
    if check.id == "app-shell" {
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
    }
    let profile = unique_profile(scratch, check.id);
    println!("browser-check: {} ...", check.id);
    match measure(browser, &served, &profile, cfg.timeout_ms) {
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
    match measure(browser, &root, &profile, cfg.timeout_ms) {
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
    // The optimised engine is ~2.8 MB, so the order of magnitude is itself the
    // assertion: a stub or a truncated body is far smaller.
    let engine_bytes = report
        .get("offlineEngineBytes")
        .and_then(Json::as_u64)
        .unwrap_or(0);
    if engine_bytes < 1_000_000 {
        return Err(format!(
            "the worker served only {engine_bytes} bytes of engine from cache, expected the \
             ~2.8 MB optimised module"
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

    // The DoD's fourth verb: print. The control is reached, and the stylesheet
    // that decides the printed output is the real one.
    let calls = report
        .get("printCalls")
        .and_then(Json::as_u64)
        .ok_or_else(|| "the app never reported a print (printCalls = None)".to_string())?;
    if calls != 1 {
        return Err(format!(
            "pressing Print called window.print {calls} time(s), expected 1 -- the button is not \
             wired to the browser's print pipeline"
        ));
    }

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

    Ok(())
}

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
fn measure(browser: &Path, root: &Path, profile: &Path, timeout_ms: u64) -> Result<Json, String> {
    let (server, results) = serve(root)?;
    let mut child = spawn(browser, profile, &server.origin())?;

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
            break Err(format!(
                "the page did not report within {timeout_ms}ms -- the engine launched and the \
                 origin served, so this is the page or the browser, not the harness"
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

/// Launch the engine on the harness origin.
///
/// `--headless=new` rather than the old `--headless`: the new headless is the
/// same code path as a real window, which is the point of driving a browser at
/// all. The profile is a fresh directory per check so no previous run's
/// service worker, cache or first-run state can answer for this one. Output is
/// discarded — a browser's stderr is noise here, and the verdict is the
/// structured report the page POSTs.
fn spawn(browser: &Path, profile: &Path, url: &str) -> Result<Child, String> {
    std::fs::create_dir_all(profile).map_err(|e| format!("{}: {e}", profile.display()))?;
    Command::new(browser)
        .arg("--headless=new")
        .arg("--disable-gpu")
        // CI runners have no user-namespace sandbox available, and the page
        // loads only this repository's own committed files. Same rationale as
        // SL-4.WASM.08's headless leg.
        .arg("--no-sandbox")
        .arg("--no-first-run")
        .arg("--no-default-browser-check")
        .arg("--disable-extensions")
        .arg("--disable-background-networking")
        .arg("--disable-component-update")
        .arg("--disable-sync")
        .arg("--disable-default-apps")
        .arg("--metrics-recording-only")
        .arg(format!("--user-data-dir={}", profile.display()))
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
    let (tx, rx) = mpsc::channel::<String>();
    let root = Arc::new(root.to_path_buf());
    let flag = Arc::clone(&stop);
    let handle = std::thread::spawn(move || {
        let reported = Arc::new(AtomicBool::new(false));
        while !flag.load(Ordering::SeqCst) {
            match listener.accept() {
                Ok((stream, _)) => {
                    // One thread per connection, deliberately. A browser opens
                    // speculative and preconnect sockets that it may never write
                    // to, and a single-threaded loop that blocks reading one of
                    // them serves nothing else — which looks exactly like a
                    // page that never loads.
                    let root = Arc::clone(&root);
                    let tx = tx.clone();
                    let reported = Arc::clone(&reported);
                    let _ = std::thread::Builder::new()
                        .name("selis-browser-origin".to_string())
                        .spawn(move || {
                            if let Err(e) = handle_connection(&stream, &root, &tx, &reported) {
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

/// Serve one connection: either the verdict POST, or a file out of `root`.
///
/// One request per connection, and the response says so with
/// `Connection: close`. Keeping the connection open was tried here and
/// **reverted**, and the reason is worth keeping: it did not fix the app-shell
/// flake (the app importing ~35 modules at once did — see `boot.js`), and on
/// Windows it broke `the_origin_serves_files_and_collects_a_verdict` with
/// `ConnectionAborted` on a connection the server had answered and left open.
/// Spending risk on a change that is not load-bearing is how a gate stops being
/// trustworthy, so the simpler behaviour stayed.
fn handle_connection(
    stream: &TcpStream,
    root: &Path,
    tx: &mpsc::Sender<String>,
    reported: &AtomicBool,
) -> std::io::Result<()> {
    // A socket the browser opened and then abandoned must not hold a thread
    // for the length of the run.
    //
    // The window is short on purpose. Chrome opens speculative and preconnect
    // sockets that it may never write to, and the browser's own per-host
    // connection cap means a thread parked on one of those for the full run
    // competes with the requests that actually carry modules. A long timeout
    // therefore shows up as modules the browser ABORTED — `responseStatus: 0`,
    // a zero-byte body, ~2 ms — which `import()` reports as an opaque
    // `TypeError: Failed to fetch dynamically imported module`.
    //
    // 1.5 s is far longer than a local loopback request needs and far shorter
    // than the run, so an idle socket is released before it can matter.
    let _ = stream.set_read_timeout(Some(Duration::from_millis(1500)));
    let mut reader = stream.try_clone()?;

    {
        let mut head = Vec::new();
        let mut byte = [0u8; 1];
        // Read through the blank line that ends the request head. One byte at a
        // time is not a performance concern for a harness that serves a handful of
        // files to one local browser, and it needs no buffering assumptions about
        // what the browser packs into the first packet.
        while !head.ends_with(b"\r\n\r\n") {
            match reader.read(&mut byte) {
                Ok(0) => return Ok(()),
                Ok(_) => head.push(byte[0]),
                // A socket the browser opened, never wrote to, and then abandoned
                // is the normal case, not an error worth printing: a speculative
                // or preconnect socket always ends this way.
                Err(e) if is_idle(&e) => return Ok(()),
                Err(e) => return Err(e),
            }
            if head.len() > MAX_REQUEST_HEAD {
                return respond(
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

        // The verdict POST is the last thing a run does, so the connection is
        // finished here rather than left open for a request that never comes.
        if method == "POST" && target == RESULT_PATH {
            let mut body = vec![0u8; content_length(&head)];
            reader.read_exact(&mut body)?;
            respond(stream, "204 No Content", "text/plain", b"")?;
            let _ = stream.shutdown(Shutdown::Write);
            if !reported.swap(true, Ordering::SeqCst) {
                let _ = tx.send(String::from_utf8_lossy(&body).to_string());
            }
            return Ok(());
        }

        let Some(path) = resolve(root, &target) else {
            return respond(stream, "403 Forbidden", "text/plain", b"forbidden");
        };
        match std::fs::read(&path) {
            Ok(bytes) => {
                let kind = content_type(&path);
                respond(stream, "200 OK", kind, &bytes)?;
                let _ = stream.shutdown(Shutdown::Write);
                Ok(())
            }
            Err(_) => respond(stream, "404 Not Found", "text/plain", b"not found"),
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

/// Write one complete response and close it.
///
/// `Connection: close` because the engine is about to be killed and a
/// half-open keep-alive socket would only delay that. Keeping the connection
/// open was tried and reverted; `handle_connection` says why.
/// `Cache-Control: no-store` because the whole point of the worker check is that
/// the *worker's* cache answers, and a served HTTP cache answering first would
/// prove nothing.
fn respond(stream: &TcpStream, status: &str, kind: &str, body: &[u8]) -> std::io::Result<()> {
    let head = format!(
        "HTTP/1.1 {status}\r\nContent-Type: {kind}\r\nContent-Length: {}\r\n\
         Cache-Control: no-store\r\nConnection: close\r\n\r\n",
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

    #[test]
    fn the_verdicts_accept_what_a_real_browser_reported() {
        assert!(verdict_service_worker(&real_service_worker_report()).is_ok());
        assert!(verdict_async_wasm(&real_async_wasm_report()).is_ok());
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
        get.write_all(b"GET / HTTP/1.1\r\nHost: x\r\n\r\n")
            .expect("write");
        let mut body = String::new();
        get.read_to_string(&mut body).expect("read");
        assert!(body.starts_with("HTTP/1.1 200 OK"), "got: {body}");
        assert!(body.contains("hello"), "got: {body}");
        assert!(body.contains("Content-Type: text/html"), "got: {body}");

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
        let cfg = Config {
            browser: None,
            page: None,
            only: None,
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
            out: dir.join("report.json"),
            keep: false,
            strict: true,
        };
        if let Err(e) = run(&cfg) {
            panic!("the real-engine harness failed: {e}");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
