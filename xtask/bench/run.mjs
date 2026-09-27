/* Drive the real-browser scroll benchmark in a real, visible browser.
 *
 * Usage:  node xtask/bench/run.mjs <benchDir> [browserPath]
 *
 * Why this is not `--dump-dom`: that flag snapshots the DOM at load
 * completion, when rAF has fired at most once, so it cannot observe a frame
 * stream at all. Both of the following were measured on this machine, not
 * assumed:
 *
 *   headless --dump-dom .................... 1 frame
 *   headed, window-position=-3000,-3000 .... 0 frames  (occluded => throttled)
 *   headed, window-position=0,0 ............ 60 frames, p50 16.7 ms (vsync)
 *
 * So the browser runs headed and genuinely visible, and the page POSTs its
 * result here. That is also the honest reading of the DoD: "60 fps sustained
 * scroll" is a claim about a real compositor, so measuring it needs a real
 * compositor. A headless proxy is not the same claim.
 *
 * What this therefore does and does not establish is documented in
 * `xtask/src/compositor_bench.rs`.
 */
import { spawn, spawnSync } from "node:child_process";
import { createServer } from "node:http";
import { existsSync, readFileSync, writeFileSync } from "node:fs";
import { join, resolve } from "node:path";

const repo = resolve(process.argv[2] ?? ".");
const browser =
  process.argv[3] ??
  "C:\\Program Files (x86)\\Microsoft\\Edge\\Application\\msedge.exe";
const benchDir = join(repo, "xtask", "bench");
const distDir = join(repo, "apps", "ui", "dist", "viewer");

const TIMEOUT_MS = 40_000;

if (!existsSync(browser)) {
  console.error(`browser not found: ${browser}`);
  process.exit(2);
}

// Bundle the SHIPPED modules first. Benchmarking the build output (rather than
// the TypeScript sources) is the point: this measures the code that ships.
const bundle = spawnSync(
  process.execPath,
  [join(benchDir, "bundle.mjs"), distDir, join(benchDir, "app.js")],
  { stdio: ["ignore", "inherit", "inherit"] },
);
if (bundle.status !== 0) {
  console.error("bundling failed — run `pnpm -r build` first");
  process.exit(2);
}

// Serve the page from 127.0.0.1 rather than file://: a Worker created from a
// file:// page is treated as opaque-origin by Chromium, which breaks the
// message channel this benchmark depends on.
const MIME = {
  ".html": "text/html",
  ".js": "text/javascript",
  ".css": "text/css",
};

let resolveResult;
const result = new Promise((r) => {
  resolveResult = r;
});

const server = createServer((req, res) => {
  const url = (req.url ?? "/").split("?")[0];
  if (process.env.SELIS_BENCH_TRACE) {
    console.error(`[bench] ${req.method} ${url}`);
  }
  if (req.method === "POST") {
    let body = "";
    req.on("data", (c) => {
      body += c;
    });
    req.on("end", () => {
      res.writeHead(204).end();
      resolveResult(body);
    });
    return;
  }
  const rel = url === "/" ? "/index.html" : url;
  const file = join(benchDir, rel);
  if (!existsSync(file)) {
    res.writeHead(404).end("not found");
    return;
  }
  const ext = file.slice(file.lastIndexOf("."));
  res.writeHead(200, { "content-type": MIME[ext] ?? "application/octet-stream" });
  res.end(readFileSync(file));
});

server.listen(0, "127.0.0.1", () => {
  const port = server.address().port;
  // The page needs the port baked in; a __PORT__ placeholder is used so the
  // committed file has no machine-specific value in it.
  const page = readFileSync(join(benchDir, "index.html"), "utf8");
  const harness = readFileSync(join(benchDir, "harness.js"), "utf8")
    .replaceAll("__PORT__", String(port));
  writeFileSync(join(benchDir, "harness.live.js"), harness, "utf8");
  const liveIndex = page.replace('src="harness.js"', 'src="harness.live.js"');
  writeFileSync(join(benchDir, "index.live.html"), liveIndex, "utf8");

  const proc = spawn(
    browser,
    [
      "--no-sandbox",
      "--no-first-run",
      "--disable-extensions",
      "--disable-background-networking",
      // Visible and on-screen: the whole point, per the measurements above.
      "--window-position=0,0",
      "--window-size=1200,1000",
      `--user-data-dir=${join(benchDir, "profile")}`,
      `http://127.0.0.1:${port}/index.live.html`,
    ],
    { stdio: ["ignore", "ignore", "pipe"] },
  );
  proc.stderr.on("data", () => {});

  const killer = setTimeout(() => {
    proc.kill();
    resolveResult(JSON.stringify({ fatal: "timed out waiting for the page to report" }));
  }, TIMEOUT_MS);

  result.then((body) => {
    clearTimeout(killer);
    // Give the compositor a moment to settle, then take the browser down.
    setTimeout(() => {
      proc.kill();
      server.close();
      console.log(body);
      process.exit(0);
    }, 150);
  });
});
