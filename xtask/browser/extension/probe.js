/**
 * SL-4.UI.06's extension leg: the probe injected into a staged COPY of the
 * packed viewer.
 *
 * The harness cannot drive an extension page over the loopback origin it
 * serves: the page under test runs on `chrome-extension://<id>/`, and the only
 * code that may run there is code inside the package. So `stage_extension`
 * (xtask/src/browser.rs) copies `apps/extension/dist` into scratch, appends one
 * `<script src="./__selis_probe.js">` to the copied `viewer.html`, and writes
 * this file beside it. The injected script is the whole of the check's
 * presence in the package; nothing the extension ships knows it exists.
 *
 * ## Why this shape
 *
 *   - **A classic script with no imports.** A module that fails to fetch or
 *     throws while evaluating never enters its body, so its error handlers are
 *     never installed and the harness sees only silence until the timeout -
 *     the failure that cost a debugging session on the web `chrome` check.
 *     This file has nothing to import, and the first statement that runs
 *     installs the watchdog.
 *   - **The report is a plain POST with `mode: "no-cors"`.** The probe never
 *     reads a response; the origin's 204 is a receipt. A CORS negotiation would
 *     be a way for the report to fail on a check whose subject is the viewer,
 *     not the harness.
 *   - **Every path ends in a POST.** A stall reports what it saw - the page's
 *     status line, the mount's state, the counts - so a red check names the
 *     step the page reached instead of only saying that nothing arrived.
 *
 * ## What it measures, and why each witness exists
 *
 * The check's claim is not "the panels rendered" (the vitest suites already
 * prove the mount composes from published states) but "the PACKAGE, loaded by a
 * real engine as an extension, opens a real document and its navigation moves
 * the reader". So the probe presses the outline row for page 2 and reads four
 * independent witnesses of the move: the scroller's offset, the row carrying
 * `aria-current`, the page-list live region's announcement, and the pixels in
 * the page raster. Each one alone can lie - a scroll without a repaint, a
 * repaint of the same page, an announcement from a stale state - and a check
 * that accepted any single one would pass on a viewer that happened to redraw.
 *
 * The verdict in `verdict_extension` decides; this file only measures. That is
 * the same split every other check in the harness keeps.
 */
(function () {
	"use strict";

	/** The whole run's budget. Inside the harness's 30 s default, so a stall
	 * reports a reason before the engine is killed and the check records
	 * "unmeasured" with nothing to read. */
	var WATCHDOG_MS = 25000;
	var POLL_MS = 100;
	/** How long the raster may stay blank after the mount appears; the
	 * compositor paints on animation frames, so this is frames, not work. */
	var INK_WAIT_MS = 10000;
	/** How long a press may take to show all four witnesses. */
	var CHANGE_WAIT_MS = 8000;

	var startedAt = Date.now();
	var finished = false;
	var watchdog = null;

	var report = {
		schema: 1,
		check: "extension",
		fatal: null,
		stage: "boot",
		elapsedMs: 0,
		href: location.href,
		protocol: location.protocol,
		extensionId: location.hostname || "",
		src: null,
		origin: null,
		composedMs: null,
		status: "",
		mount: null,
		outline: null,
		pageList: null,
		press: null,
	};

	function text(element) {
		return element === null ? "" : (element.textContent || "").trim();
	}

	function all(selector) {
		return Array.prototype.slice.call(document.querySelectorAll(selector));
	}

	function statusElement() {
		return document.getElementById("selis-status");
	}

	function mountElement() {
		return document.getElementById("selis-document");
	}

	// ── Where the verdict goes ───────────────────────────────────────────────

	var src = new URLSearchParams(location.search).get("src");
	report.src = src;
	var origin = null;
	try {
		origin = new URL(src).origin;
	} catch (error) {
		origin = null;
	}
	report.origin = origin;

	function finish() {
		if (finished) {
			return;
		}
		finished = true;
		if (watchdog !== null) {
			clearTimeout(watchdog);
			watchdog = null;
		}
		report.elapsedMs = Date.now() - startedAt;
		report.status = text(statusElement());
		if (origin === null) {
			// No src, nothing named to post to. The harness will record the check
			// unmeasured; the page's own status line is the only witness left.
			return;
		}
		try {
			fetch(origin + "/__selis_result", {
				method: "POST",
				mode: "no-cors",
				cache: "no-store",
				body: JSON.stringify(report),
			}).catch(function () {
				// Delivery is the origin's to prove; the probe cannot read it.
			});
		} catch (error) {
			// A synchronous throw from fetch itself (a malformed URL) leaves the
			// harness to time out; there is no second channel to say so on.
		}
	}

	function fatal(why, diagnostics) {
		if (report.fatal !== null) {
			return;
		}
		report.fatal = why;
		if (diagnostics !== undefined) {
			report.stall = diagnostics;
		}
		finish();
	}

	watchdog = setTimeout(function () {
		if (finished) {
			return;
		}
		fatal(
			"the probe's watchdog fired at " + WATCHDOG_MS + "ms (step: " + report.stage + ")",
			diagnostics(),
		);
	}, WATCHDOG_MS);

	// ── Reading the mount ───────────────────────────────────────────────────

	/** The raster the mount paints into; one canvas holds the whole window. */
	function raster() {
		var mount = mountElement();
		if (mount === null) {
			return null;
		}
		return mount.querySelector("canvas.selis-page-list__surface");
	}

	/**
	 * A content hash of the raster, sampled on a fixed stride.
	 *
	 * Sampled rather than exhaustive because this runs repeatedly while waiting
	 * for a repaint and the backing store is viewport-sized; a fixed stride
	 * makes the before and after read the same pixels, which is the whole
	 * comparison. FNV-1a over the sampled RGBA bytes.
	 */
	function canvasInk() {
		var canvas = raster();
		if (canvas === null) {
			return null;
		}
		var width = canvas.width;
		var height = canvas.height;
		if (width === 0 || height === 0) {
			return { width: width, height: height, hash: null, inkPixels: 0, sampled: 0 };
		}
		var context = canvas.getContext("2d");
		if (context === null) {
			return { width: width, height: height, hash: null, inkPixels: 0, sampled: 0 };
		}
		var data;
		try {
			data = context.getImageData(0, 0, width, height).data;
		} catch (error) {
			return {
				width: width,
				height: height,
				hash: null,
				inkPixels: 0,
				sampled: 0,
				error: String(error),
			};
		}
		var hash = 2166136261 >>> 0;
		var inkPixels = 0;
		var sampled = 0;
		var pixels = width * height;
		// The viewer's canvas is created with `{ alpha: false }`, so alpha is
		// 255 everywhere and says nothing about ink. "Ink" here is a sampled
		// pixel whose RGB differs from the page background, read from the
		// top-left corner - the fixture keeps that corner clear on purpose, and
		// a blank raster is then honestly blank rather than trivially "inked".
		var background = [data[0], data[1], data[2]];
		for (var index = 0; index < pixels; index += 97) {
			var offset = index * 4;
			var delta =
				Math.abs(data[offset] - background[0]) +
				Math.abs(data[offset + 1] - background[1]) +
				Math.abs(data[offset + 2] - background[2]);
			if (delta > 24) {
				inkPixels += 1;
			}
			hash = Math.imul(hash ^ data[offset], 16777619) >>> 0;
			hash = Math.imul(hash ^ data[offset + 1], 16777619) >>> 0;
			hash = Math.imul(hash ^ data[offset + 2], 16777619) >>> 0;
			hash = Math.imul(hash ^ data[offset + 3], 16777619) >>> 0;
			sampled += 1;
		}
		return {
			width: width,
			height: height,
			hash: hash.toString(16),
			inkPixels: inkPixels,
			sampled: sampled,
		};
	}

	function outlineTargets() {
		return all('[data-role="outline-target"]');
	}

	function notes() {
		return all('[data-role="outline-notes"] > *').map(function (note) {
			return {
				role: note.getAttribute("data-role") || "",
				text: text(note),
			};
		});
	}

	function currentRowIndex() {
		var targets = outlineTargets();
		for (var index = 0; index < targets.length; index += 1) {
			if (targets[index].getAttribute("aria-current") === "true") {
				return index;
			}
		}
		return -1;
	}

	/** One instant of everything the press must be shown to have changed. */
	function snapshot() {
		var targets = outlineTargets();
		var current = currentRowIndex();
		var tile = document.querySelector('[data-role="page-tile"][aria-current="page"]');
		var scroller = document.querySelector('[data-role="page-list-scroll"]');
		return {
			live: text(document.querySelector('[data-role="page-list-live"]')),
			navLive: text(document.querySelector('[data-role="navigation-live"]')),
			currentRow: current,
			currentRowPage: current === -1 ? null : targets[current].getAttribute("data-page"),
			currentTile: tile === null ? null : tile.getAttribute("aria-label"),
			scrollTop: scroller === null ? null : scroller.scrollTop,
			ink: canvasInk(),
		};
	}

	function diagnostics() {
		var mount = mountElement();
		var canvas = raster();
		return {
			status: text(statusElement()),
			mountHidden: mount === null ? null : mount.hidden,
			mountChildren: mount === null ? 0 : mount.childElementCount,
			targets: outlineTargets().length,
			notes: notes(),
			pageLive: text(document.querySelector('[data-role="page-list-live"]')),
			navLive: text(document.querySelector('[data-role="navigation-live"]')),
			canvas:
				canvas === null
					? null
					: { width: canvas.width, height: canvas.height },
		};
	}

	// ── The three steps ─────────────────────────────────────────────────────

	/** Whether the mount has composed enough to be read: the frame, at least
	 * one outline row to press, a live region, and a sized raster. */
	function composed() {
		var mount = mountElement();
		if (mount === null || mount.hidden) {
			return false;
		}
		if (document.querySelector('[data-role="page-list-scroll"]') === null) {
			return false;
		}
		if (text(document.querySelector('[data-role="page-list-live"]')) === "") {
			return false;
		}
		if (outlineTargets().length < 2) {
			return false;
		}
		var canvas = raster();
		return canvas !== null && canvas.width > 0 && canvas.height > 0;
	}

	function waitForCompose() {
		report.stage = "compose";
		var poll = function () {
			if (finished) {
				return;
			}
			if (composed()) {
				report.composedMs = Date.now() - startedAt;
				waitForInk();
				return;
			}
			// A failure sentence is a reason, not a stall; report it now rather
			// than after the watchdog has burned twenty seconds.
			var status = text(statusElement()).toLowerCase();
			if (status.indexOf("could not be opened") !== -1) {
				fatal("the viewer reported an open failure", diagnostics());
				return;
			}
			setTimeout(poll, POLL_MS);
		};
		poll();
	}

	function waitForInk() {
		report.stage = "ink";
		var deadline = Date.now() + INK_WAIT_MS;
		var poll = function () {
			if (finished) {
				return;
			}
			var ink = canvasInk();
			if (ink !== null && ink.error !== undefined) {
				fatal("the page raster could not be read: " + ink.error, diagnostics());
				return;
			}
			if (ink !== null && ink.inkPixels > 0) {
				pressRow();
				return;
			}
			if (Date.now() >= deadline) {
				fatal("the page raster stayed blank after the mount composed", diagnostics());
				return;
			}
			setTimeout(poll, POLL_MS);
		};
		poll();
	}

	function pressRow() {
		report.stage = "snapshot";
		var targets = outlineTargets();
		report.mount = {
			hidden: mountElement().hidden,
			children: mountElement().childElementCount,
			width: mountElement().getBoundingClientRect().width,
			height: mountElement().getBoundingClientRect().height,
		};
		report.outline = {
			rows: targets.length,
			pages: targets.map(function (row) {
				return row.getAttribute("data-page");
			}),
			titles: targets.map(function (row) {
				return text(row);
			}),
			labels: targets.map(function (row) {
				return row.getAttribute("aria-label") || "";
			}),
			currentRow: currentRowIndex(),
			notes: notes(),
		};
		report.pageList = {
			tiles: document.querySelectorAll('[data-role="page-tile"]').length,
			live: text(document.querySelector('[data-role="page-list-live"]')),
			scrollTop: document.querySelector('[data-role="page-list-scroll"]').scrollTop,
			scrollHeight: document.querySelector('[data-role="page-list-scroll"]').scrollHeight,
			clientHeight: document.querySelector('[data-role="page-list-scroll"]').clientHeight,
		};

		// The row for the document's LAST page - page index 1 in this fixture.
		// Pressing the page already showing would be indistinguishable from
		// pressing nothing, which is the reason the fixture's two pages differ.
		var pick = -1;
		for (var index = 0; index < targets.length; index += 1) {
			if (targets[index].getAttribute("data-page") === "1") {
				pick = index;
				break;
			}
		}
		if (pick === -1) {
			pick = targets.length - 1;
		}
		var pressed = targets[pick];
		report.press = {
			attempted: true,
			index: pick,
			page: pressed.getAttribute("data-page"),
			title: text(pressed),
			label: pressed.getAttribute("aria-label") || "",
			before: snapshot(),
			after: null,
			changed: null,
		};
		report.stage = "press";
		pressed.click();
		waitForChange(report.press.before);
	}

	function waitForChange(before) {
		var deadline = Date.now() + CHANGE_WAIT_MS;
		var poll = function () {
			if (finished) {
				return;
			}
			var after = snapshot();
			var changed = {
				ink:
					before.ink !== null &&
					after.ink !== null &&
					before.ink.hash !== null &&
					after.ink.hash !== null &&
					before.ink.hash !== after.ink.hash,
				live: after.live !== before.live,
				navLive: after.navLive !== before.navLive,
				currentRow: after.currentRow !== before.currentRow,
				scrollTop: after.scrollTop !== before.scrollTop,
			};
			// The three the verdict treats as the move itself; the others are
			// recorded so a red run says which witness disagreed.
			if ((changed.ink && changed.live && changed.currentRow) || Date.now() >= deadline) {
				report.press.after = after;
				report.press.changed = changed;
				report.stage = "done";
				finish();
				return;
			}
			setTimeout(poll, POLL_MS);
		};
		poll();
	}

	waitForCompose();
})();
