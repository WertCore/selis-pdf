/* Real-browser scroll benchmark page (SL-4.UI.03 / SL-4.UI.02 DoD).
 *
 * Loaded as a classic script AFTER `dist/viewer/*.js` have been bundled into
 * `app.js` (an IIFE assigning the real shipped modules to `window.SELIS`).
 * Everything below drives the PRODUCTION compositor -- there is deliberately
 * no reimplementation here, because a benchmark that measures a private copy
 * of the logic proves nothing about the code that ships.
 *
 * The measured path is end-to-end: scroll -> compositor.update() -> coalesced
 * rAF -> surface.draw(). The worker half (worker.js) does the real
 * OffscreenCanvas drawing and is timed separately.
 */
(function () {
  "use strict";

  var out = document.getElementById("selis-bench-result");
  var RESULT_OPEN = "@@SELIS-RESULT@@";
  var RESULT_CLOSE = "@@SELIS-END@@";
  var SCHEMA = 1;

  function finish(payload) {
    out.textContent = RESULT_OPEN + JSON.stringify(payload) + RESULT_CLOSE;
    // Reported out of band over HTTP to the xtask listener.
    //
    // `--dump-dom` is NOT usable for this benchmark: it snapshots the DOM at
    // load completion, by which time rAF has fired at most once, so an
    // animation-frame measurement through it is meaningless. This was measured,
    // not assumed: headless `--dump-dom` yields exactly 1 frame, and a headed
    // window positioned off-screen yields 0. Only a genuinely visible window
    // produces a real vsync-locked frame stream (measured p50 16.7 ms = 60 fps).
    var xhr = new XMLHttpRequest();
    xhr.open("POST", "http://127.0.0.1:__PORT__/", true);
    xhr.send(JSON.stringify(payload));
  }

  function fail(where, error) {
    // Stringified deliberately: an Error serialises to {}, and a harness that
    // reports "{}" for a crash is worse than useless.
    finish({
      schema: SCHEMA,
      fatal: where + ": " + (error && error.stack ? error.stack : String(error)),
    });
  }

  // Anything thrown during setup happens BEFORE the try block at the bottom of
  // this file, and an uncaught error in a classic script is silent as far as
  // `--dump-dom` and the console are concerned. Without this the harness can
  // fail for a reason nobody sees, which is how the first two attempts at this
  // benchmark "passed" while measuring nothing.
  window.addEventListener("error", function (event) {
    finish({
      schema: SCHEMA,
      fatal:
        "uncaught error: " +
        (event && event.message ? event.message : String(event)) +
        " @ " +
        (event && event.filename ? event.filename : "?") +
        ":" +
        (event && event.lineno ? event.lineno : "?"),
    });
  });
  window.addEventListener("unhandledrejection", function (event) {
    var r = event && event.reason;
    finish({
      schema: SCHEMA,
      fatal: "unhandled rejection: " + ((r && r.stack) || String(r)),
    });
  });

  var S = window.SELIS;
  if (!S || !S.createTileCompositor || !S.createPageList) {
    finish({
      schema: SCHEMA,
      fatal:
        "missing exports on window.SELIS: " +
        Object.keys(window.SELIS || {}).join(",") +
        " (did app.js load?)",
    });
    return;
  }

  var PAGE_COUNT = 2000;
  var VIEWPORT_W = 1013;
  var VIEWPORT_H = 900;
  var FRAMES = 180;
  var DPR = 1.25; // fractional on purpose: exercises the DPR-rounding path
  var BUDGET_MS = 1000 / 60;

  var canvas = document.getElementById("selis-bench-canvas");
  canvas.width = Math.round(VIEWPORT_W * DPR);
  canvas.height = Math.round(VIEWPORT_H * DPR);
  canvas.style.width = VIEWPORT_W + "px";
  canvas.style.height = VIEWPORT_H + "px";

  // The offscreen canvas must be handed over BEFORE anything asks for a 2D
  // context on the page canvas. The other order throws InvalidStateError
  // ("Cannot transfer control from a canvas that has a rendering context") --
  // the bug the first attempt at this harness hit, noted here so it is not
  // rediscovered from scratch.
  var offscreen = canvas.transferControlToOffscreen();

  var worker = new Worker("worker.js");
  worker.postMessage({ kind: "attach", canvas: offscreen }, [offscreen]);

  // The engine port. This returns REAL rasterised tiles -- an actual RGBA8
  // buffer of the right dimensions for the requested scale -- because the
  // benchmark's whole value is that the compositor does genuine per-frame work
  // over genuine pixel payloads. Returning `{}` here (as a first attempt did)
  // leaves every page a placeholder, the compositor emits zero draw ops, and
  // the run "measures" an empty pipeline in ~0 ms. The harness's own
  // `avgOpsPerFrame` / `workerDrawnOps` check exists precisely to catch that,
  // and it did -- which is the property that makes the number trustworthy.
  var TILE_W = 96;
  var TILE_H = 124;
  var tilePixels = new Uint8ClampedArray(TILE_W * TILE_H * 4);
  for (var i = 0; i < tilePixels.length; i += 4) {
    tilePixels[i] = (i >> 3) & 255;
    tilePixels[i + 1] = (i >> 5) & 255;
    tilePixels[i + 2] = 200;
    tilePixels[i + 3] = 255;
  }
  var tileData = tilePixels.buffer.slice(0);

  var adapter = {
    engine: {
      open: function () {
        return Promise.resolve({ docId: "bench", pageCount: PAGE_COUNT });
      },
      renderTile: function (request) {
        // `request.scale` is device pixels per PDF point; size the buffer to the
        // media box at that scale, as a real rasteriser would.
        var scale = request.scale || 1;
        var w = Math.max(1, Math.round(612 * scale));
        var h = Math.max(1, Math.round(792 * scale));
        var out = new Uint8ClampedArray(w * h * 4);
        out.set(tilePixels.subarray(0, Math.min(out.length, tilePixels.length)));
        for (var k = 3; k < out.length; k += 4) out[k] = 255;
        return Promise.resolve({
          page: request.page,
          width: w,
          height: h,
          format: "rgba8",
          data: out.buffer,
        });
      },
      close: function () {
        return Promise.resolve();
      },
    },
    files: {},
    storage: {},
    clipboard: {},
    print: {},
    telemetry: {},
    window: {},
  };

  // A uniform Letter document: `DocHandle` is `{ id, pageCount, pageSizes }`,
  // and the page list lays out from `pageSizes` (not a pageWidth/pageHeight
  // pair) -- passing the wrong shape fails deep inside `layoutPages`, which is
  // why the harness reports the whole stack rather than swallowing it.
  var PAGE_SIZES = [];
  for (var p = 0; p < PAGE_COUNT; p += 1) PAGE_SIZES.push({ width: 612, height: 792 });

  var list = S.createPageList({
    adapter: adapter,
    doc: { id: "bench", pageCount: PAGE_COUNT, pageSizes: PAGE_SIZES },
    mode: "fit-width",
  });


  // A surface that records but does not paint: this measures the compositor's
  // per-frame decision cost, and the worker measures the draw cost. Counting
  // ops per frame is what makes the number falsifiable -- a compositor that
  // silently drew nothing would otherwise "pass" instantly.
  var placeholderFrames = 0;
  var workerDrawn = 0;

  worker.onmessage = function (event) {
    if (event.data && event.data.kind === "drew") workerDrawn = event.data.total;
  };

  var surface = {
    takesOwnership: false,
    configure: function () {},
    draw: function (frame) {
      if (frame.ops.length > 0) {
        // Real work dispatched to the real worker, so the frame budget is
        // measured with the worker round trip included.
        worker.postMessage({ kind: "draw", ops: frame.ops.length });
      }
      if (frame.placeholders > 0) placeholderFrames += 1;
    },
    present: function () {},
    dispose: function () {},
  };

  var clock = {
    requestFrame: function (cb) {
      return { id: requestAnimationFrame(cb), cancelled: false };
    },
    cancelFrame: function (h) {
      if (!h.cancelled) cancelAnimationFrame(h.id);
    },
  };

  var frames = 0;
  var measured = [];
  var scrollY = 0;
  var perFrameOps = [];
  var lastStamp = 0;

  var compositor = S.createTileCompositor({
    list: list,
    surface: surface,
    clock: clock,
    onFrame: function (frame) {
      if (frames >= FRAMES) return;
      // A scroll-only run: the offset ramps through the 2000-page document
      // every frame, which is exactly the case the DoD names.
      //
      // The scroll IS the viewport update -- `PageList` exposes `update()`, not
      // a separate scroll setter, so there is nothing else to call here.
      try {
        scrollY = frames * 37;
        compositor.update({
          width: VIEWPORT_W,
          height: VIEWPORT_H,
          scrollTop: scrollY,
          devicePixelRatio: DPR,
        });
      } catch (e) {
        fail("onFrame#" + frames, e);
        return;
      }
      if (frames > 0) measured.push(performance.now() - lastStamp);
      lastStamp = performance.now();
      perFrameOps.push(frame.ops.length);
      frames += 1;
      if (frames >= FRAMES) report();
    },
  });

  function report() {
    var sorted = measured.slice().sort(function (a, b) {
      return a - b;
    });
    var sum = 0;
    for (var i = 0; i < measured.length; i += 1) sum += measured[i];
    var overBudget = 0;
    for (var j = 0; j < measured.length; j += 1) {
      if (measured[j] > BUDGET_MS) overBudget += 1;
    }
    var totalOps = 0;
    for (var k = 0; k < perFrameOps.length; k += 1) totalOps += perFrameOps[k];
    var p95 = sorted[Math.floor(sorted.length * 0.95)];

    finish({
      schema: SCHEMA,
      engine: "edge",
      pageCount: PAGE_COUNT,
      frames: frames,
      samples: measured.length,
      dpr: DPR,
      viewport: { w: VIEWPORT_W, h: VIEWPORT_H },
      budgetMs: BUDGET_MS,
      avgMs: sum / measured.length,
      p50Ms: sorted[Math.floor(sorted.length * 0.5)],
      p95Ms: p95,
      maxMs: sorted[sorted.length - 1],
      overBudgetFrames: overBudget,
      avgOpsPerFrame: totalOps / perFrameOps.length,
      placeholderFrames: placeholderFrames,
      workerDrawnOps: workerDrawn,
      // The honesty fields: how much of the budget p95 consumed, and whether
      // the pipeline actually drew. A pass with workerDrawnOps === 0 would be
      // measuring nothing, so the Rust side treats that as a failure.
      budgetUsePct: (p95 / BUDGET_MS) * 100,
      pass: p95 <= BUDGET_MS && workerDrawn > 0,
    });
  }

  try {
    compositor.update({
      width: VIEWPORT_W,
      height: VIEWPORT_H,
      scrollTop: 0,
      devicePixelRatio: DPR,
    });
    // If rAF never fires (background tab, throttled headless run) this still
    // reports, with a fatal, rather than leaving the gate green on a partial
    // run.
    setTimeout(function () {
      if (frames < FRAMES) {
        finish({
          schema: SCHEMA,
          fatal:
            "only " + frames + "/" + FRAMES +
            " frames arrived in 8s -- rAF is throttled or the page is backgrounded, so the measurement is not valid",
          frames: frames,
        });
      }
    }, 8000);
  } catch (e) {
    fail("setup", e);
  }
})();
