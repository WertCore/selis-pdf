/* The worker half of the real-browser benchmark (SL-4.UI.03).
 *
 * Owns the transferred OffscreenCanvas, exactly as the shipped
 * `createOffscreenCompositor` does, and answers a `ping` with the number of
 * frames it actually drew. That count is what makes the benchmark falsifiable:
 * if the worker never presented anything, the main-thread number is measuring
 * an empty pipeline and must not be reported as a pass.
 */
let ctx = null;
let attached = false;
let drawn = 0;

self.onmessage = function (event) {
  const msg = event.data;
  try {
    if (msg.kind === "attach") {
      if (attached) {
        self.postMessage({ kind: "fault", fault: "already-attached" });
        return;
      }
      ctx = msg.canvas.getContext("2d");
      if (!ctx) {
        self.postMessage({ kind: "fault", fault: "no-2d-context" });
        return;
      }
      attached = true;
      self.postMessage({ kind: "ready" });
      return;
    }
    if (msg.kind === "draw") {
      // Real drawing work at a realistic tile size, so the number includes a
      // genuine composite rather than a no-op call.
      for (let i = 0; i < msg.ops; i += 1) {
        ctx.fillStyle = "rgb(" + ((i * 37) % 255) + ",0,0)";
        ctx.fillRect((i * 8) % 900, (i * 13) % 800, 120, 160);
      }
      drawn += msg.ops;
      // transferToImageBitmap is the real present path: it hands the backing
      // store away and the worker keeps drawing into a fresh one.
      const bmp = ctx.canvas.transferToImageBitmap();
      bmp.close();
      self.postMessage({ kind: "drew", total: drawn });
      return;
    }
    if (msg.kind === "stats") {
      self.postMessage({ kind: "stats", drawn: drawn });
      return;
    }
  } catch (error) {
    self.postMessage({
      kind: "fault",
      fault: String((error && error.message) || error),
    });
  }
};
