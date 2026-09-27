//! The headless-browser determinism leg (SL-4.WASM.08, `cargo xtask
//! wasm-browser`).
//!
//! # What this proves
//!
//! SL-2.RAST.09 requires byte-identical renders "across x86-64 Linux, arm64
//! macOS, and WASM". The other WASM legs pin the *same* engine twice on the
//! *host*: `xtask perf-wasm` and `xtask wasm-protocol` run the guest under
//! **wasmtime** (Cranelift) and compare it with the native render, and the
//! `wasm-threads` CI job proves the tiled pipeline is identity in-process.
//! None of them execute a line of the module in a browser. This leg does: the
//! exact `wasm32-unknown-unknown` artifact `perf_wasm::build_driver` produces
//! (`RUSTFLAGS="-C target-feature=+simd128"`, the shipped flag) is loaded by a
//! real browser's WebAssembly engine (**V8** -- an independent code generator
//! with its own Liftoff baseline and TurboFan tiers), handed the corpus over
//! the raw render ABI (ADR-P0041: `selis_input_alloc` / `selis_render_page` /
//! `selis_free`), and the resulting RGBA8 pixmaps are hashed and compared with
//! the native render of the same documents. The compared engines are therefore
//! `native` vs `V8`, not `wasmtime` vs `native` again.
//!
//! # The fail-loud rules (the reason this is not a smoke test)
//!
//! A determinism gate that renders nothing passes trivially -- this repo has
//! already been bitten by it (SL-3.CONF.06: two `render-conf` legs measured 0
//! comparable pages and the job stayed green). Therefore:
//!
//! * **Zero comparable pages is a failure**, not a pass
//!   ([`check_comparable_or_fail`]), whatever the browser reported.
//! * **A page the native render produced but the browser did not is a
//!   divergence**, not a skip: [`compare`] fails the leg and names it. A
//!   browser that cannot render what the engine renders natively has lost the
//!   property under test.
//! * **No browser, no harness, no result marker** are hard errors. The
//!   subcommand never degrades to "nothing to compare, all good".
//! * **The JS hasher is self-tested before it is trusted.** The page hashes a
//!   known byte string and compares it with [`perf_wasm::checksum`]'s value
//!   (injected by this harness). A wrong FNV in the page can therefore only
//!   produce a *loud false failure*, never a green one -- and
//!   [`fnv1a64_split32`] (the Rust transliteration of the page's algorithm) is
//!   unit-tested against `checksum`, so the two cannot drift apart silently.//!
//! # The browser is an external tool, not a dependency
//!
//! Nothing is added to `Cargo.toml`, `package.json`, or `pnpm-lock.yaml`: this
//! leg adds **no dependency of any kind**. The browser is discovered at
//! runtime -- `--browser <path>`, `$SELIS_BROWSER`, or the well-known install
//! locations in [`browser_candidates`] -- and if none is found the leg fails
//! with the list of paths it probed. That is the same shape as the repo's
//! other external tools (`qpdf` in the `write05-oracle` CI job, `mutool` /
//! `pdfium` behind `xtask/oracles.toml`), which are installed on the runner
//! and never vendored into the build (ADR-P0009).
//!
//! What that buys and what it costs, stated plainly:
//!
//! * Licence: Chromium is BSD-3-Clause (the engine); the Microsoft Edge
//!   *branded* build adds proprietary media codecs, which this leg never
//!   touches -- it decodes no media, it hashes RGBA8 pixmaps. Edge is a
//!   Chromium fork under the same terms. A distro `chromium` package is the
//!   licence-cleanest choice and is a fallback on the Linux candidate list.
//! * Maintenance: the engine is an external binary by design. The property
//!   under test is *our* module's output being engine-independent, so a
//!   browser upgrade that *did* move a byte is exactly the signal this gate
//!   exists to raise -- it fails loudly, naming the page. Conversely, adding a
//!   browser-download mechanism (a `puppeteer`/`playwright` package, a
//!   vendored Chrome-for-Testing tarball) is a supply-chain addition that has
//!   **not** been proposed or approved; this leg deliberately makes none.
//!
//! # The harness page
//!
//! No bundler, no npm package, no dev server. [`write_harness`] writes three
//! files into a scratch directory:
//!
//! * `wasm_b64.js` -- the guest artifact, base64.
//! * `corpus.js` -- `SELIS_CORPUS` (the documents, base64) and
//!   `SELIS_SELFTEST` (the hasher known-answer vector).
//! * `harness.html` -- loads both with plain `<script src>` and renders.
//!
//! The page is **fully synchronous** (`new WebAssembly.Module` +
//! `new WebAssembly.Instance`, base64 decoded from those two scripts, never
//! `fetch`). That is deliberate: the browser is driven with `--headless
//! --dump-dom`, which snapshots the DOM at load completion, so a single
//! unresolved promise would dump a harness that has rendered nothing and
//! report it as a pass. Synchronous means the `<pre>` is final before the load
//! event fires. (A synchronous `XMLHttpRequest` on `file://` was tried first
//! and is not viable: the response text is decoded through the page charset
//! and mangles every byte above 0x7F.)
//!
//! Two further browser-side details the page handles and the host asserts:
//!
//! * **A detached `ArrayBuffer`.** The guest grows its linear memory during
//!   the render, which detaches every existing view. Every typed array and
//!   `DataView` is therefore re-created *after* `selis_render_page` returns.
//! * **A closed import set.** The import object is built from the module's own
//!   `WebAssembly.Module.imports()` list and rejects anything outside the four
//!   wasm-bindgen shims `perf_wasm::define_shims` allows -- the same discipline
//!   as the wasmtime harness, so the driver cannot grow a host dependency that
//!   the browser leg happens to satisfy and the host leg does not.
//!
//! # Budget
//!
//! This harness consumes no `Budget`: it parses no untrusted bytes. It
//! *renders* committed in-repo corpus documents, and every engine entry point
//! it calls takes the `Viewer`-profile `Budget` internally -- identically on
//! both sides. The page's own resource bound is the scratch directory (bounded
//! by the corpus) and the browser's exit, which the CI job's `timeout-minutes`
//! bounds.

use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::{json, Value as Json};

use crate::perf_wasm;

/// The DOM id the harness writes its JSON verdict into.
const RESULT_ID: &str = "selis-wasm-browser-result";

/// Delimiters wrapped around the JSON inside `RESULT_ID`.
///
/// `--dump-dom` re-serialises the DOM, so the payload is extracted by marker
/// rather than by "the text between `<pre>` and `</pre>`": browser-side entity
/// escaping then cannot turn a real result into a parse failure, or the
/// reverse.
const RESULT_OPEN: &str = "@@SELIS-RESULT@@";
const RESULT_CLOSE: &str = "@@SELIS-END@@";

/// Schema version of the page's verdict document. Bump on any shape change;
/// the host refuses a version it does not know rather than guessing.
const SCHEMA: u32 = 1;

/// The known-answer vector the page hashes before it hashes anything else.
///
/// Deliberately not a PDF: the point is to test *the hasher*, and a render
/// that produced no pixels cannot test a hasher.
const HASHER_SELFTEST: &[u8] = b"selis wasm08 browser hasher self-test\n";
/// The subcommand's inputs.
pub struct Config {
    /// Explicit browser binary (`--browser`).
    pub browser: Option<PathBuf>,
    /// Override the corpus with a single directory (`--corpus`).
    pub corpus: Option<PathBuf>,
    /// Report JSON path (`--out`); also the CI artifact.
    pub out: PathBuf,
    /// Keep the generated harness directory instead of deleting it (`--keep`).
    pub keep: bool,
}

/// One rendered page, as either side reports it.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Page {
    /// Canvas width in pixels.
    pub w: u32,
    /// Canvas height in pixels.
    pub h: u32,
    /// Length of the RGBA8 pixmap in bytes.
    pub len: u32,
    /// FNV-1a 64 of the pixmap, lowercase hex, 16 digits.
    pub hash: String,
}

/// One corpus document's native-side outcome.
#[derive(Clone, Debug)]
enum Native {
    /// The native render produced this page.
    Rendered(Page),
    /// The native engine cannot open/render it -- not comparable, not a
    /// failure.
    Skipped(String),
}

/// One corpus document's browser-side outcome.
#[derive(Clone, Debug)]
enum Browser {
    /// The browser produced this page.
    Rendered(Page),
    /// The guest refused the page in the browser (or never reported it).
    Refused(String),
}

/// One corpus document, both sides' outcomes.
#[derive(Clone, Debug)]
struct Row {
    /// `label/file.pdf` -- unique across corpus directories.
    name: String,
    /// The native outcome.
    native: Native,
    /// The browser-side outcome.
    browser: Browser,
}

/// The leg's result: the rows plus the tally the fail-loud rule reads.
#[derive(Clone, Debug)]
struct Comparison {
    /// Every corpus row, in corpus order.
    rows: Vec<Row>,
    /// Rows where both sides rendered a page.
    comparable: usize,
    /// Rows where the native engine rendered nothing (not comparable).
    native_skipped: usize,
    /// Human-readable divergences; the leg fails when non-empty.
    divergences: Vec<String>,
    /// The browser's own reported fatal error, if it declared one.
    fatal: Option<String>,
}

/// One corpus document as read from disk.
struct Doc {
    /// `label/file.pdf` -- unique across corpus directories.
    name: String,
    /// The committed bytes, read once and handed to both sides.
    bytes: Vec<u8>,
}
/// Run the leg: build the guest, render every corpus page natively, render
/// them again in a real browser, and fail on any divergence -- or on having
/// measured nothing at all.
pub fn run(cfg: &Config) -> Result<(), String> {
    let browser = resolve_browser(cfg.browser.as_deref())?;
    println!("wasm-browser: browser {}", browser.display());

    let wasm = perf_wasm::build_driver()?;
    println!("wasm-browser: guest {}", wasm.display());

    let docs = collect_corpus(cfg.corpus.as_deref())?;
    if docs.is_empty() {
        return Err("the corpus is empty: nothing to compare".to_string());
    }
    println!("wasm-browser: corpus: {} document(s)", docs.len());

    // The native side first: a native-side failure is then reported as such
    // and not mistaken for a browser problem.
    let native: Vec<Native> = docs.iter().map(|doc| native_page(&doc.bytes)).collect();

    let scratch = scratch_dir(&cfg.out);
    if scratch.exists() {
        std::fs::remove_dir_all(&scratch).map_err(|e| format!("{}: {e}", scratch.display()))?;
    }
    std::fs::create_dir_all(&scratch).map_err(|e| format!("{}: {e}", scratch.display()))?;
    let page = write_harness(&scratch, &wasm, &docs)?;
    let dom = run_browser(&browser, &scratch, &page)?;
    let verdict = parse_result(&dom)?;
    if !cfg.keep {
        let _ = std::fs::remove_dir_all(&scratch);
    }

    let comparison = compare(&docs, &native, &verdict)?;
    write_report(&cfg.out, &browser, docs.len(), &comparison)?;

    for row in &comparison.rows {
        let line = match (&row.native, &row.browser) {
            (Native::Rendered(a), Browser::Rendered(b)) => {
                if a == b {
                    format!("match {}", a.hash)
                } else {
                    format!(
                        "MISMATCH native {}x{} len={} {} vs browser {}x{} len={} {}",
                        a.w, a.h, a.len, a.hash, b.w, b.h, b.len, b.hash
                    )
                }
            }
            (Native::Rendered(_), Browser::Refused(why)) => {
                format!("browser refused a natively-rendered page: {why}")
            }
            (Native::Skipped(why), _) => format!("not comparable (native: {why})"),
        };
        println!("wasm-browser: {}: {line}", row.name);
    }

    if let Some(fatal) = &comparison.fatal {
        return Err(format!(
            "the browser harness reported a fatal error: {fatal}"
        ));
    }
    if !comparison.divergences.is_empty() {
        return Err(format!(
            "{} of {} corpus page(s) diverged between the native render and the \
             browser render (SL-4.WASM.08): {}",
            comparison.divergences.len(),
            docs.len(),
            comparison.divergences.join("; ")
        ));
    }
    // SL-3.CONF.06's rule, applied to this leg: a determinism gate that
    // compared nothing is a green gate that measured nothing.
    check_comparable_or_fail(&comparison, docs.len())?;
    println!(
        "wasm-browser: {} comparable page(s) hash-identical (native vs {})",
        comparison.comparable,
        browser_name(&browser)
    );
    Ok(())
}
/// The in-repo corpus: the two directories the engine's own
/// `tiled_equals_untiled_over_the_corpus` test walks, plus the deterministic
/// benchmark set -- the full set the `wasm-threads` CI job reasons about. Each
/// document is labelled with its directory so two fixtures of the same name
/// stay distinguishable in the report.
fn corpus_dirs() -> Vec<(&'static str, PathBuf)> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    vec![
        (
            "engine",
            root.join("..").join("crates/selis-pdf-engine/src/fixtures"),
        ),
        ("corpus", root.join("..").join("corpus/fixtures")),
        ("bench", root.join("..").join("bench/render-set")),
    ]
}

/// Read the corpus (or the single `--corpus` directory) in a stable order.
fn collect_corpus(only: Option<&Path>) -> Result<Vec<Doc>, String> {
    let dirs: Vec<(String, PathBuf)> = match only {
        Some(dir) => vec![("corpus".to_string(), dir.to_path_buf())],
        None => corpus_dirs()
            .into_iter()
            .map(|(label, path)| (label.to_string(), path))
            .collect(),
    };
    let mut docs: Vec<Doc> = Vec::new();
    for (label, dir) in dirs {
        let entries = match std::fs::read_dir(&dir) {
            Ok(entries) => entries,
            // A missing optional corpus directory is not fatal on its own; the
            // zero-comparable guard at the end is what refuses a green run
            // that measured nothing.
            Err(e) if only.is_none() => {
                println!("wasm-browser: skipping {}: {e}", dir.display());
                continue;
            }
            Err(e) => return Err(format!("{}: {e}", dir.display())),
        };
        let mut paths: Vec<PathBuf> = entries.flatten().map(|e| e.path()).collect();
        paths.sort();
        for path in paths {
            if path.extension().and_then(|e| e.to_str()) != Some("pdf") {
                continue;
            }
            let file = path
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| "?".to_string());
            let bytes = std::fs::read(&path).map_err(|e| format!("{}: {e}", path.display()))?;
            docs.push(Doc {
                name: format!("{label}/{file}"),
                bytes,
            });
        }
    }
    Ok(docs)
}

/// Native render of page 0 at 72 DPI, hashed with the *same* FNV-1a 64 the
/// page computes (`perf_wasm::checksum`).
///
/// The clock is the engine's `FixedClock(0)`, not the shell clock, because
/// that is what the guest resolves internally (ADR-P0011: the guest resolves
/// no clock). A native side running on the real clock could exhaust a wall
/// budget the guest never sees, and the resulting "divergence" would be the
/// harness's own doing.
fn native_page(doc: &[u8]) -> Native {
    let budget = selis_sandbox::Budget::profile(selis_sandbox::Surface::Viewer);
    let clock = selis_sandbox::FixedClock(0);
    let session = match selis_pdf_engine::Session::open(doc.to_vec(), &budget, &clock) {
        Ok(session) => session,
        Err(e) => return Native::Skipped(format!("open: {e}")),
    };
    let Some(view) = session.page_view(0, 72.0) else {
        return Native::Skipped("no media box".to_string());
    };
    let Some(mut backend) = selis_pdf_engine::TinySkiaBackend::new(view.width, view.height) else {
        return Native::Skipped("cannot create the canvas".to_string());
    };
    let mut guard = budget.guard_with(&clock, selis_sandbox::CancelToken::new());
    if let Err(e) = session.render_page(0, &mut backend, view.ctm, &budget, &mut guard) {
        return Native::Skipped(format!("render: {e}"));
    }
    let pixels = backend.pixmap().data();
    let len = u32::try_from(pixels.len()).unwrap_or(u32::MAX);
    Native::Rendered(Page {
        w: view.width,
        h: view.height,
        len,
        hash: perf_wasm::checksum(pixels),
    })
}
/// The browser binaries this leg will accept, in probe order.
///
/// A deliberately short, explicit list -- never a bare `PATH` lookup of an
/// unversioned `chrome`, because a determinism gate whose engine it cannot
/// name is not a gate.
///
/// **Microsoft Edge is the default engine.** It is a Chromium fork, so it
/// exercises the same V8 the web app ships on, and it is preinstalled on the
/// Windows and macOS machines this project is developed on, so the gate needs
/// no download step. Chrome and distro `chromium` remain accepted as
/// alternates: this leg's claim is *V8-determinism vs the native engine*, and
/// that holds on any Chromium build, so pinning Edge does not weaken it.
fn browser_candidates() -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = Vec::new();
    if let Some(env) = std::env::var_os("SELIS_BROWSER") {
        out.push(PathBuf::from(env));
    }
    match std::env::consts::OS {
        "windows" => {
            // Edge first: the default engine. Chrome follows as an alternate.
            for (key, tail) in [
                ("ProgramFiles", r"Microsoft\Edge\Application\msedge.exe"),
                (
                    "ProgramFiles(x86)",
                    r"Microsoft\Edge\Application\msedge.exe",
                ),
                ("LocalAppData", r"Microsoft\Edge\Application\msedge.exe"),
                ("ProgramFiles", r"Google\Chrome\Application\chrome.exe"),
                ("ProgramFiles(x86)", r"Google\Chrome\Application\chrome.exe"),
                ("LocalAppData", r"Google\Chrome\Application\chrome.exe"),
            ] {
                if let Some(base) = std::env::var_os(key) {
                    let mut path = PathBuf::from(base);
                    for part in tail.split('\\') {
                        path.push(part);
                    }
                    out.push(path);
                }
            }
        }
        "macos" => {
            for app in [
                "Microsoft Edge.app/Contents/MacOS/Microsoft Edge",
                "Chromium.app/Contents/MacOS/Chromium",
                "Google Chrome.app/Contents/MacOS/Google Chrome",
            ] {
                out.push(PathBuf::from("/Applications").join(app));
            }
        }
        _ => {
            // Linux runners ship Edge too; distro `chromium` stays the
            // licence-cleanest fallback.
            for bin in [
                "/usr/bin/microsoft-edge",
                "/usr/bin/microsoft-edge-stable",
                "/usr/bin/chromium",
                "/usr/bin/chromium-browser",
                "/usr/bin/google-chrome",
                "/usr/bin/google-chrome-stable",
                "/snap/bin/chromium",
            ] {
                out.push(PathBuf::from(bin));
            }
        }
    }
    out
}

/// A short name for the resolved browser, for the report and the log line.
fn browser_name(browser: &Path) -> String {
    browser
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| browser.display().to_string())
}

/// Resolve the browser binary, or fail with every path that was probed.
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
        "no headless browser found -- this leg renders the corpus in a real browser \
         and refuses to report green without one. Install one (a distro `chromium` \
         is the licence-cleanest choice), or pass --browser <path> / set $SELIS_BROWSER. \
         Probed: {}",
        candidates
            .iter()
            .map(|p| p.display().to_string())
            .collect::<Vec<_>>()
            .join(", ")
    ))
}

/// The scratch directory the harness is written into, beside the report.
fn scratch_dir(out: &Path) -> PathBuf {
    let parent = out.parent().unwrap_or_else(|| Path::new("."));
    parent.join("harness")
}

/// Write `bytes` to `path`, reporting the path on failure.
fn write_file(path: &Path, bytes: &[u8]) -> Result<(), String> {
    std::fs::write(path, bytes).map_err(|e| format!("{}: {e}", path.display()))
}
/// Standard base64 (the page's only transport; no `fetch`, no bundler).
fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b0 = u32::from(chunk[0]);
        let b1 = chunk.get(1).copied().map_or(0u32, u32::from);
        let b2 = chunk.get(2).copied().map_or(0u32, u32::from);
        let triple = (b0 << 16) | (b1 << 8) | b2;
        let sextet = |shift: u32| -> char {
            let idx = usize::try_from((triple >> shift) & 0x3f).unwrap_or(0);
            char::from(ALPHABET[idx])
        };
        out.push(sextet(18));
        out.push(sextet(12));
        out.push(if chunk.len() > 1 { sextet(6) } else { '=' });
        out.push(if chunk.len() > 2 { sextet(0) } else { '=' });
    }
    out
}

/// FNV-1a 64 in two 32-bit halves -- the Rust transliteration of the
/// arithmetic the harness page performs in JavaScript.
///
/// The page cannot use `BigInt` per byte (orders of magnitude too slow over a
/// multi-megabyte pixmap) and JS numbers cannot hold 64 bits, so it multiplies
/// a `(hi, lo)` pair by the 64-bit prime `0x100000001b3` with exact
/// intermediate values. This function exists so that algorithm is *tested*
/// rather than trusted: `fnv1a64_split32_matches_the_harness_checksum` asserts
/// it agrees with `perf_wasm::checksum` byte for byte, so a page that drifts
/// from this shape shows up as a loud divergence rather than a silent pass.
fn fnv1a64_split32(bytes: &[u8]) -> String {
    // 0x100000001b3 = PH * 2^32 + PL.
    const PL: u64 = 0x1b3;
    const PH: u64 = 0x100;
    let mut hi: u64 = 0xcbf2_9ce4;
    let mut lo: u64 = 0x8422_2325;
    for &byte in bytes {
        lo ^= u64::from(byte);
        let product = lo * PL;
        let next_lo = product & 0xffff_ffff;
        let carry = product >> 32;
        hi = (hi * PL + lo * PH + carry) & 0xffff_ffff;
        lo = next_lo;
    }
    format!("{hi:08x}{lo:08x}")
}

/// A `file://` URL for `path`, percent-encoding everything outside the
/// unreserved set plus `/` and `:`.
///
/// Written by hand rather than pulled in as a dependency: the only consumer is
/// a locally-generated scratch path, and a wrong URL here would surface as a
/// browser that renders nothing -- which this leg treats as a failure, but a
/// failure that costs a debugging session.
fn file_url(path: &Path) -> String {
    let text = path.to_string_lossy().replace('\\', "/");
    // A Windows drive letter is an *authority*-looking prefix, so a URL of
    // the form `file://C:/...` parses as host "C:" and the browser answers
    // ERR_INVALID_URL. Three slashes are required.
    let absolute = if text.starts_with('/') {
        text
    } else if text.as_bytes().get(1) == Some(&b':') {
        format!("/{text}")
    } else {
        let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/"));
        format!("/{}", cwd.join(path).to_string_lossy().replace('\\', "/"))
    };
    let mut url = String::from("file://");
    for byte in absolute.as_bytes() {
        let c = char::from(*byte);
        if c.is_ascii_alphanumeric() || matches!(c, '-' | '.' | '_' | '~' | '/' | ':') {
            url.push(c);
        } else {
            url.push_str(&format!("%{byte:02X}"));
        }
    }
    url
}
/// The JavaScript the harness page runs.
///
/// Kept as one raw string so the whole contract is reviewable in one place:
/// the FNV-1a 64 (two 32-bit halves -- see [`fnv1a64_split32`]), the closed
/// wasm-bindgen shim set (mirroring `perf_wasm::define_shims`), the
/// alloc -> copy -> render -> read -> free sequence over the raw render ABI,
/// and the views re-created after the memory growth.
const HARNESS_JS: &str = r##"
"use strict";

function b64ToBytes(b64) {
  var bin = atob(b64);
  var out = new Uint8Array(bin.length);
  for (var i = 0; i < bin.length; i++) out[i] = bin.charCodeAt(i) & 0xff;
  return out;
}

// FNV-1a 64 over a (hi, lo) pair. The prime 0x100000001b3 is ph*2^32 + pl and
// every intermediate product is below 2^53, so the doubles stay exact and no
// BigInt (far slower per byte) is needed.
function fnv1a64(bytes) {
  var hi = 0xcbf29ce4, lo = 0x84222325;
  for (var i = 0; i < bytes.length; i++) {
    lo = (lo ^ bytes[i]) >>> 0;
    var product = lo * 0x1b3;
    var nextLo = product >>> 0;
    var carry = Math.floor(product / 4294967296);
    hi = (hi * 0x1b3 + lo * 0x100 + carry) >>> 0;
    lo = nextLo;
  }
  return ("00000000" + hi.toString(16)).slice(-8) + ("00000000" + lo.toString(16)).slice(-8);
}

// The same closed shim set `perf_wasm::define_shims` allows, built from the
// module's own import list: anything outside it is a hard error, so the driver
// cannot grow a host dependency only the browser happens to satisfy.
function importObjectFor(module) {
  var declared = WebAssembly.Module.imports(module);
  var imports = {};
  for (var i = 0; i < declared.length; i++) {
    var mod = declared[i].module, name = declared[i].name;
    if (!imports[mod]) imports[mod] = {};
    if (mod === "__wbindgen_placeholder__") {
      if (name === "__wbindgen_describe") {
        imports[mod][name] = function () {};
      } else if (name.indexOf("__wbg___wbindgen_throw") === 0) {
        imports[mod][name] = function () { throw new Error("guest threw via the wasm-bindgen shim"); };
      } else {
        throw new Error("unexpected driver import " + mod + "::" + name);
      }
    } else if (mod === "__wbindgen_externref_xform__") {
      if (name === "__wbindgen_externref_table_set_null") {
        imports[mod][name] = function () {};
      } else if (name === "__wbindgen_externref_table_grow") {
        imports[mod][name] = function () { return 0; };
      } else {
        throw new Error("unexpected driver import " + mod + "::" + name);
      }
    } else {
      throw new Error("unexpected driver import module " + mod);
    }
  }
  return imports;
}
function renderPage(exports, pdf) {
  var mem = exports.memory;
  var inPtr = exports.selis_input_alloc(pdf.length);
  if (!inPtr) return { status: "refused", detail: "selis_input_alloc returned null" };
  new Uint8Array(mem.buffer, inPtr, pdf.length).set(pdf);
  // Three out-word slots (w, h, len) from the same 4-aligned allocator.
  var words = exports.selis_input_alloc(12);
  if (!words) {
    exports.selis_free(inPtr, pdf.length);
    return { status: "refused", detail: "out-word alloc returned null" };
  }
  var pix = exports.selis_render_page(inPtr, pdf.length, 0, words, words + 4, words + 8);
  if (!pix) {
    exports.selis_free(inPtr, pdf.length);
    exports.selis_free(words, 12);
    return { status: "refused", detail: "selis_render_page returned null" };
  }
  // The render grows linear memory, so every view taken before the call is
  // detached by now: read the geometry and the pixels from fresh views. This
  // must happen BEFORE any free -- freeing hands the blocks straight back to
  // the allocator, and reading the out-words afterwards reads whatever the
  // next allocation wrote there.
  var dv = new DataView(mem.buffer);
  var w = dv.getUint32(words, true);
  var h = dv.getUint32(words + 4, true);
  var len = dv.getUint32(words + 8, true);
  var pixels = new Uint8Array(mem.buffer, pix, len);
  var result = { status: "ok", w: w, h: h, len: len, hash: fnv1a64(pixels) };
  exports.selis_free(pix, len);
  exports.selis_free(words, 12);
  exports.selis_free(inPtr, pdf.length);
  return result;
}

function main() {
  var pages = [];
  var out = document.getElementById(RESULT_ID);
  try {
    var selftest = fnv1a64(b64ToBytes(SELIS_SELFTEST_B64));
    if (selftest !== SELIS_SELFTEST_EXPECT) {
      throw new Error("hasher self-test failed: page says " + selftest + ", host says " + SELIS_SELFTEST_EXPECT);
    }
    var module = new WebAssembly.Module(b64ToBytes(SELIS_WASM_B64));
    var instance = new WebAssembly.Instance(module, importObjectFor(module));
    for (var i = 0; i < SELIS_CORPUS.length; i++) {
      var entry = SELIS_CORPUS[i];
      var page;
      try {
        page = renderPage(instance.exports, b64ToBytes(entry.b64));
      } catch (docError) {
        page = { status: "refused", detail: "" + docError };
      }
      page.name = entry.name;
      pages.push(page);
    }
    out.textContent = RESULT_OPEN + JSON.stringify({ schema: SCHEMA, fatal: null, pages: pages }) + RESULT_CLOSE;
  } catch (error) {
    out.textContent = RESULT_OPEN + JSON.stringify({ schema: SCHEMA, fatal: "" + error, pages: pages }) + RESULT_CLOSE;
  }
}

main();
"##;
/// Write `wasm_b64.js`, `corpus.js` and `harness.html` into `dir`; return the
/// page's path.
fn write_harness(dir: &Path, wasm: &Path, docs: &[Doc]) -> Result<PathBuf, String> {
    let wasm_bytes = std::fs::read(wasm).map_err(|e| format!("{}: {e}", wasm.display()))?;
    let corpus = Json::Array(
        docs.iter()
            .map(|doc| json!({ "name": doc.name, "b64": base64(&doc.bytes) }))
            .collect(),
    );
    let selftest_b64 = base64(HASHER_SELFTEST);
    let selftest_expect = fnv1a64_split32(HASHER_SELFTEST);

    let mut corpus_js = String::new();
    corpus_js.push_str("// Generated by `cargo xtask wasm-browser` (SL-4.WASM.08). Do not edit.\n");
    corpus_js.push_str(&format!("var SELIS_CORPUS = {corpus};\n"));
    corpus_js.push_str(&format!("var SELIS_SELFTEST_B64 = \"{selftest_b64}\";\n"));
    corpus_js.push_str(&format!(
        "var SELIS_SELFTEST_EXPECT = \"{selftest_expect}\";\n"
    ));

    let html = format!(
        "<!doctype html>\n<html><head><meta charset=\"utf-8\">\n\
         <title>selis wasm determinism harness</title></head>\n<body>\n\
         <pre id=\"{RESULT_ID}\">PENDING</pre>\n\
         <script>\n\
         var RESULT_ID = \"{RESULT_ID}\";\n\
         var RESULT_OPEN = \"{RESULT_OPEN}\";\n\
         var RESULT_CLOSE = \"{RESULT_CLOSE}\";\n\
         var SCHEMA = {SCHEMA};\n\
         </script>\n\
         <script src=\"corpus.js\"></script>\n\
         <script src=\"wasm_b64.js\"></script>\n\
         <script>\n{HARNESS_JS}</script>\n</body></html>\n"
    );

    let mut wasm_js = String::new();
    wasm_js.push_str("// Generated by `cargo xtask wasm-browser` (SL-4.WASM.08). Do not edit.\n");
    wasm_js.push_str(&format!(
        "var SELIS_WASM_B64 = \"{}\";\n",
        base64(&wasm_bytes)
    ));

    write_file(&dir.join("corpus.js"), corpus_js.as_bytes())?;
    write_file(&dir.join("wasm_b64.js"), wasm_js.as_bytes())?;
    let page = dir.join("harness.html");
    write_file(&page, html.as_bytes())?;
    Ok(page)
}

/// Drive the browser over the harness page and return the `--dump-dom` DOM.
///
/// `--dump-dom` snapshots at load completion; the page is synchronous, so the
/// `<pre>` is already final. A non-zero exit, an empty DOM, or a DOM without
/// the result marker is an error -- never an empty comparison.
fn run_browser(browser: &Path, dir: &Path, page: &Path) -> Result<String, String> {
    let profile = dir.join("profile");
    let output = Command::new(browser)
        .arg("--headless")
        .arg("--disable-gpu")
        // Headless CI runners have no user-namespace sandbox available, and
        // the page loads only this repository's own committed fixtures.
        .arg("--no-sandbox")
        .arg("--disable-extensions")
        .arg("--no-first-run")
        .arg("--disable-background-networking")
        .arg(format!("--user-data-dir={}", profile.display()))
        .arg("--dump-dom")
        .arg(file_url(page))
        .output()
        .map_err(|e| format!("spawn {}: {e}", browser.display()))?;
    let dom = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr);
    if !output.status.success() {
        return Err(format!(
            "{} --dump-dom exited with {}: {}",
            browser_name(browser),
            output.status,
            stderr.trim()
        ));
    }
    if !dom.contains(RESULT_OPEN) {
        return Err(format!(
            "{} produced no harness result (no `{RESULT_OPEN}` marker in the dumped DOM, \
             {} bytes) -- the page did not finish, so nothing was compared. \
             Browser stderr: {}",
            browser_name(browser),
            dom.len(),
            stderr.trim()
        ));
    }
    Ok(dom)
}
/// Extract the page's JSON verdict from a `--dump-dom` snapshot.
fn parse_result(dom: &str) -> Result<Json, String> {
    let start = dom
        .find(RESULT_OPEN)
        .ok_or_else(|| format!("no `{RESULT_OPEN}` marker in the dumped DOM"))?
        + RESULT_OPEN.len();
    let rest = &dom[start..];
    let end = rest
        .find(RESULT_CLOSE)
        .ok_or_else(|| format!("no `{RESULT_CLOSE}` marker after the result"))?;
    let payload = &rest[..end];
    let value: Json = serde_json::from_str(payload)
        .map_err(|e| format!("harness result is not valid JSON: {e} (payload {payload})"))?;
    if value["schema"] != json!(SCHEMA) {
        return Err(format!(
            "harness result schema is {}, this harness speaks {SCHEMA}",
            value["schema"]
        ));
    }
    Ok(value)
}

/// Line every corpus row up with the browser's per-document outcome and
/// classify it.
///
/// The two rules that matter:
/// * both sides rendered -> comparable; any field difference is a divergence;
/// * native rendered, browser did not -> **also** a divergence, never a skip.
fn compare(docs: &[Doc], native: &[Native], verdict: &Json) -> Result<Comparison, String> {
    if let Some(fatal) = verdict["fatal"].as_str() {
        if !fatal.is_empty() {
            return Ok(Comparison {
                rows: Vec::new(),
                comparable: 0,
                native_skipped: 0,
                divergences: Vec::new(),
                fatal: Some(fatal.to_string()),
            });
        }
    }
    let pages = verdict["pages"]
        .as_array()
        .ok_or("harness result has no `pages` array")?;
    let mut by_name: std::collections::BTreeMap<&str, &Json> = std::collections::BTreeMap::new();
    for page in pages {
        let name = page["name"]
            .as_str()
            .ok_or("harness result page has no `name`")?;
        by_name.insert(name, page);
    }
    let mut rows = Vec::with_capacity(docs.len());
    let mut comparable = 0usize;
    let mut native_skipped = 0usize;
    let mut divergences = Vec::new();
    for (doc, native_side) in docs.iter().zip(native) {
        let browser_side = match by_name.get(doc.name.as_str()) {
            Some(page) if page["status"] == json!("ok") => {
                let field = |key: &str| -> Result<u32, String> {
                    page[key]
                        .as_u64()
                        .and_then(|v| u32::try_from(v).ok())
                        .ok_or_else(|| format!("{}: browser page has no u32 `{key}`", doc.name))
                };
                let hash = page["hash"]
                    .as_str()
                    .ok_or_else(|| format!("{}: browser page has no `hash`", doc.name))?
                    .to_string();
                Browser::Rendered(Page {
                    w: field("w")?,
                    h: field("h")?,
                    len: field("len")?,
                    hash,
                })
            }
            Some(page) => {
                Browser::Refused(page["detail"].as_str().unwrap_or("unspecified").to_string())
            }
            None => Browser::Refused("the browser returned no result for this document".into()),
        };
        match (native_side, &browser_side) {
            (Native::Rendered(a), Browser::Rendered(b)) => {
                comparable += 1;
                if a != b {
                    divergences.push(format!(
                        "{}: native {}x{} len={} {} != browser {}x{} len={} {}",
                        doc.name, a.w, a.h, a.len, a.hash, b.w, b.h, b.len, b.hash
                    ));
                }
            }
            (Native::Rendered(_), Browser::Refused(why)) => {
                divergences.push(format!(
                    "{}: the native render produced this page but the browser did not ({why})",
                    doc.name
                ));
            }
            (Native::Skipped(_), _) => native_skipped += 1,
        }
        rows.push(Row {
            name: doc.name.clone(),
            native: native_side.clone(),
            browser: browser_side,
        });
    }
    Ok(Comparison {
        rows,
        comparable,
        native_skipped,
        divergences,
        fatal: None,
    })
}

/// SL-3.CONF.06's rule, applied to this leg: refuse to report green on a run
/// that compared nothing.
///
/// A browser that silently rendered no page -- a shim that never ran, a corpus
/// path that resolved to nothing, an engine that refused every document --
/// would otherwise turn this into a determinism gate that proves nothing,
/// which is the exact failure this repo has already paid for once.
fn check_comparable_or_fail(cmp: &Comparison, corpus_len: usize) -> Result<(), String> {
    if cmp.comparable == 0 {
        return Err(format!(
            "0 comparable pages out of {corpus_len} corpus document(s) \
             ({} the native engine could not render) -- refusing green on an \
             unmeasured leg (SL-3.CONF.06 / SL-4.WASM.08)",
            cmp.native_skipped
        ));
    }
    Ok(())
}
/// Write the machine-readable report (the CI artifact), whether the leg passed
/// or failed.
fn write_report(
    out: &Path,
    browser: &Path,
    corpus_len: usize,
    cmp: &Comparison,
) -> Result<(), String> {
    let rows: Vec<Json> = cmp
        .rows
        .iter()
        .map(|row| {
            let page_json = |p: &Page| -> Json {
                json!({"status": "ok", "w": p.w, "h": p.h, "len": p.len, "hash": p.hash})
            };
            let native_json = match &row.native {
                Native::Rendered(p) => page_json(p),
                Native::Skipped(why) => json!({"status": "refused", "detail": why}),
            };
            let browser_json = match &row.browser {
                Browser::Rendered(p) => page_json(p),
                Browser::Refused(why) => json!({"status": "refused", "detail": why}),
            };
            json!({"name": row.name, "native": native_json, "browser": browser_json})
        })
        .collect();
    let report = json!({
        "version": 1,
        "task": "SL-4.WASM.08",
        "engine": browser.display().to_string(),
        "engine_name": browser_name(browser),
        "schema": SCHEMA,
        "corpus_documents": corpus_len,
        "comparable": cmp.comparable,
        "native_skipped": cmp.native_skipped,
        "fatal": cmp.fatal,
        "divergences": cmp.divergences,
        "rows": rows,
    });
    if let Some(parent) = out.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
        }
    }
    let text =
        serde_json::to_string_pretty(&report).map_err(|e| format!("serialise report: {e}"))?;
    write_file(out, text.as_bytes())?;
    println!("wasm-browser: wrote {}", out.display());
    Ok(())
}
#[cfg(test)]
mod tests {
    #![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]
    use super::*;

    fn page(hash: &str) -> Page {
        Page {
            w: 595,
            h: 842,
            len: 2_003_960,
            hash: hash.to_string(),
        }
    }

    fn doc(name: &str) -> Doc {
        Doc {
            name: name.to_string(),
            bytes: b"%PDF-1.7".to_vec(),
        }
    }

    /// The page's FNV-1a 64 (two 32-bit halves) must equal the checksum the
    /// native side uses -- otherwise every page would "diverge" for a reason
    /// that has nothing to do with rendering.
    #[test]
    fn fnv1a64_split32_matches_the_harness_checksum() {
        assert_eq!(
            fnv1a64_split32(HASHER_SELFTEST),
            perf_wasm::checksum(HASHER_SELFTEST)
        );
        for len in [0usize, 1, 2, 3, 7, 8, 9, 255, 4096] {
            let bytes: Vec<u8> = (0..len).map(|i| (i % 251) as u8).collect();
            assert_eq!(
                fnv1a64_split32(&bytes),
                perf_wasm::checksum(&bytes),
                "len {len}"
            );
        }
    }

    /// Base64 is the page's only transport; a wrong encoder would corrupt
    /// every document into an unopenable one.
    #[test]
    fn base64_matches_the_standard_alphabet() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"foob"), "Zm9vYg==");
        assert_eq!(base64(b"fooba"), "Zm9vYmE=");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
        // Every length class, so all three padding branches are covered.
        for len in 0..64usize {
            let bytes: Vec<u8> = (0..len).map(|i| (i % 256) as u8).collect();
            let encoded = base64(&bytes);
            assert_eq!(encoded.len(), len.div_ceil(3) * 4, "len {len}");
            assert!(encoded
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'+' || b == b'/' || b == b'='));
        }
    }
    /// The result is extracted by marker, and a DOM that carries no marker
    /// (a page that never ran) is an error rather than an empty comparison.
    #[test]
    fn parse_result_needs_the_markers() {
        let empty = format!("<pre>noise {RESULT_OPEN}{RESULT_CLOSE}</pre>");
        assert!(
            parse_result(&empty).is_err(),
            "an empty payload is not a verdict"
        );
        let payload = json!({"schema": SCHEMA, "fatal": null, "pages": []}).to_string();
        let dom = format!("<pre>x{RESULT_OPEN}{payload}{RESULT_CLOSE}</pre>");
        let parsed = parse_result(&dom).expect("a marked payload parses");
        assert_eq!(parsed["schema"], json!(SCHEMA));
        assert!(parse_result("<html><body>nothing here</body></html>").is_err());
    }

    /// A foreign schema is refused, not interpreted with this harness's shapes.
    #[test]
    fn parse_result_refuses_a_foreign_schema() {
        let payload = json!({"schema": SCHEMA + 1, "pages": []}).to_string();
        let dom = format!("{RESULT_OPEN}{payload}{RESULT_CLOSE}");
        let err = parse_result(&dom).expect_err("a schema mismatch is an error");
        assert!(err.contains("schema"), "{err}");
    }

    /// The DoD itself: identical pages on both sides are comparable and pass.
    #[test]
    fn compare_accepts_identical_pages() {
        let docs = vec![doc("corpus/a.pdf")];
        let native = vec![Native::Rendered(page("aa"))];
        let verdict = json!({
            "schema": SCHEMA,
            "fatal": null,
            "pages": [{
                "name": "corpus/a.pdf", "status": "ok",
                "w": 595, "h": 842, "len": 2_003_960, "hash": "aa",
            }],
        });
        let cmp = compare(&docs, &native, &verdict).expect("compare");
        assert_eq!(cmp.comparable, 1);
        assert!(cmp.divergences.is_empty());
        assert!(check_comparable_or_fail(&cmp, 1).is_ok());
    }

    /// One moved byte is a divergence, and the leg fails.
    #[test]
    fn compare_catches_a_moved_byte() {
        let docs = vec![doc("corpus/a.pdf")];
        let native = vec![Native::Rendered(page("aa"))];
        let verdict = json!({
            "schema": SCHEMA, "fatal": null,
            "pages": [{
                "name": "corpus/a.pdf", "status": "ok",
                "w": 595, "h": 842, "len": 2_003_960, "hash": "ab",
            }],
        });
        let cmp = compare(&docs, &native, &verdict).expect("compare");
        assert_eq!(cmp.comparable, 1);
        assert_eq!(cmp.divergences.len(), 1);
        assert!(cmp.divergences[0].contains("corpus/a.pdf"));
    }

    /// A geometry difference counts too: a canvas that changed shape is as
    /// much a determinism failure as a changed pixel.
    #[test]
    fn compare_catches_a_geometry_change() {
        let docs = vec![doc("corpus/a.pdf")];
        let native = vec![Native::Rendered(page("aa"))];
        let verdict = json!({
            "schema": SCHEMA, "fatal": null,
            "pages": [{
                "name": "corpus/a.pdf", "status": "ok",
                "w": 596, "h": 842, "len": 2_003_960, "hash": "aa",
            }],
        });
        let cmp = compare(&docs, &native, &verdict).expect("compare");
        assert_eq!(cmp.divergences.len(), 1);
    }
    /// The trap this whole leg is built around: a browser that rendered
    /// nothing must not be able to report green.
    #[test]
    fn compare_fails_a_browser_that_refused_a_rendered_page() {
        let docs = vec![doc("corpus/a.pdf")];
        let native = vec![Native::Rendered(page("aa"))];
        let verdict = json!({
            "schema": SCHEMA, "fatal": null,
            "pages": [{
                "name": "corpus/a.pdf", "status": "refused",
                "detail": "selis_render_page returned null",
            }],
        });
        let cmp = compare(&docs, &native, &verdict).expect("compare");
        assert_eq!(cmp.comparable, 0);
        assert_eq!(cmp.divergences.len(), 1);
        assert!(check_comparable_or_fail(&cmp, 1).is_err());
    }

    /// A page the browser never mentions at all is the same failure, and is
    /// reported as such rather than silently not compared.
    #[test]
    fn compare_fails_a_browser_that_omitted_the_document() {
        let docs = vec![doc("corpus/a.pdf")];
        let native = vec![Native::Rendered(page("aa"))];
        let verdict = json!({"schema": SCHEMA, "fatal": null, "pages": []});
        let cmp = compare(&docs, &native, &verdict).expect("compare");
        assert_eq!(cmp.comparable, 0);
        assert_eq!(cmp.divergences.len(), 1);
        assert!(check_comparable_or_fail(&cmp, 1).is_err());
    }

    /// A fatal error in the page is carried through as a failure, with no rows.
    #[test]
    fn compare_surfaces_a_fatal_harness_error() {
        let docs = vec![doc("corpus/a.pdf")];
        let native = vec![Native::Rendered(page("aa"))];
        let verdict = json!({"schema": SCHEMA, "fatal": "hasher self-test failed", "pages": []});
        let cmp = compare(&docs, &native, &verdict).expect("compare");
        assert!(cmp.fatal.is_some());
        assert!(check_comparable_or_fail(&cmp, 1).is_err());
    }

    /// A document the *native* engine cannot render is not comparable and is
    /// not a failure -- but it does not count towards the comparable tally
    /// either, so a corpus of such documents still fails the guard.
    #[test]
    fn native_skips_are_neither_comparable_nor_divergences() {
        let docs = vec![doc("corpus/a.pdf"), doc("corpus/b.pdf")];
        let native = vec![
            Native::Skipped("open: XREF_UNRECOVERABLE".to_string()),
            Native::Rendered(page("bb")),
        ];
        let verdict = json!({
            "schema": SCHEMA, "fatal": null,
            "pages": [{
                "name": "corpus/b.pdf", "status": "ok",
                "w": 595, "h": 842, "len": 2_003_960, "hash": "bb",
            }],
        });
        let cmp = compare(&docs, &native, &verdict).expect("compare");
        assert_eq!(cmp.comparable, 1);
        assert_eq!(cmp.native_skipped, 1);
        assert!(cmp.divergences.is_empty());
        assert!(check_comparable_or_fail(&cmp, 2).is_ok());
    }

    /// Every document unrenderable natively -> zero comparable -> red.
    #[test]
    fn check_comparable_or_fail_fails_an_unmeasured_run() {
        let cmp = Comparison {
            rows: Vec::new(),
            comparable: 0,
            native_skipped: 4,
            divergences: Vec::new(),
            fatal: None,
        };
        let err = check_comparable_or_fail(&cmp, 4).expect_err("0 comparable is a failure");
        assert!(err.contains("0 comparable"), "{err}");
    }
    /// The generated page must be self-contained and synchronous: no `fetch`,
    /// no dynamic `import`, nothing that could leave the `<pre>` unfinished at
    /// load (which `--dump-dom` would snapshot as a green "no results").
    #[test]
    fn harness_page_is_synchronous_and_self_contained() {
        assert!(!HARNESS_JS.contains("fetch("), "the page must not fetch");
        assert!(
            !HARNESS_JS.contains("import("),
            "the page must not dynamic-import"
        );
        assert!(HARNESS_JS.contains("new WebAssembly.Module("));
        assert!(HARNESS_JS.contains("selis_render_page"));
        // The detached-views trap: the DataView must be taken after the call.
        let render = HARNESS_JS
            .split("function renderPage")
            .nth(1)
            .expect("renderPage is defined");
        let call = render.find("selis_render_page(").expect("the render call");
        let view = render.find("new DataView(").expect("a fresh DataView");
        assert!(
            view > call,
            "views must be re-created after the guest grows its memory"
        );
    }

    /// The corpus really is the corpus: the two directories the engine's own
    /// identity test walks plus the benchmark set, with unique labels.
    #[test]
    fn the_in_repo_corpus_is_present() {
        let docs = collect_corpus(None).expect("the corpus directories are readable");
        assert!(
            docs.len() >= 10,
            "corpus too small to be a determinism gate: {} documents",
            docs.len()
        );
        for dir in ["engine", "corpus", "bench"] {
            assert!(
                docs.iter().any(|d| d.name.starts_with(&format!("{dir}/"))),
                "no {dir} documents in the corpus"
            );
        }
        let mut labels: Vec<&str> = docs.iter().map(|d| d.name.as_str()).collect();
        labels.sort_unstable();
        let unique = labels.len();
        labels.dedup();
        assert_eq!(labels.len(), unique, "corpus labels must be unique");
    }

    /// `--dump-dom` takes a URL and the scratch path is machine-generated (a
    /// drive letter, backslashes, possibly spaces), so encoding is not optional.
    #[test]
    fn file_url_percent_encodes() {
        // The drive-letter case: `file://C:/...` parses as a host named "C:".
        assert_eq!(
            file_url(Path::new(r"C:\a b\h.html")),
            "file:///C:/a%20b/h.html"
        );
        assert_eq!(file_url(Path::new("/tmp/h.html")), "file:///tmp/h.html");
    }
}
