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
	/** How long a press may take to show all six witnesses. */
	var CHANGE_WAIT_MS = 8000;
	/** How long the first page's link overlay may take to arrive. */
	var LINK_WAIT_MS = 8000;
	/** How long the rail's previews may take to paint before the snapshot. */
	var PREVIEW_WAIT_MS = 5000;
	/** How long the prompt may take to open or close. */
	var PROMPT_WAIT_MS = 3000;
	/** How long one stage key may take to paint the previous page. */
	var KEY_WAIT_MS = 6000;

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
		rail: null,
		link: null,
		press: null,
		key: null,
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

	/** The page number the rail itself marks current, or null when none is. */
	function currentRailPage() {
		var current = document.querySelector('[data-role="thumbnail"][aria-current="page"]');
		return current === null ? null : current.getAttribute("data-page");
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
			railCurrent: currentRailPage(),
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
			rail: document.querySelectorAll('[data-role="thumbnail"]').length,
			links: document.querySelectorAll('[data-role="page-link"]').length,
			prompt: document.querySelector('[data-role="external-prompt"]') !== null,
			canvas:
				canvas === null
					? null
					: { width: canvas.width, height: canvas.height },
		};
	}

	// ── The measured stages ────────────────────────────────────────────────

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
		if (document.querySelector('[data-role="thumbnail-rail"]') === null) {
			return false;
		}
		if (document.querySelectorAll('[data-role="thumbnail"]').length < 1) {
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
				visitLink();
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

	/**
	 * Click the fixture's external link, prove the prompt opened, then cancel it.
	 *
	 * The links live on the page tiles, not in the sidebar, so this is the only
	 * witness that the canvas overlay is real. It runs before the outline press
	 * because page 0 is the page that carries them.
	 */
	function visitLink() {
		report.stage = "link";
		var deadline = Date.now() + LINK_WAIT_MS;
		var poll = function () {
			if (finished) {
				return;
			}
			var links = all('[data-role="page-link"]');
			if (links.length > 0) {
				var chosen = links[0];
				for (var index = 0; index < links.length; index += 1) {
					if (links[index].getAttribute("data-action-kind") === "uri") {
						chosen = links[index];
						break;
					}
				}
				revealLink(chosen, links, 0);
				return;
			}
			if (Date.now() >= deadline) {
				fatal("the page rendered no link overlay after the raster inked", diagnostics());
				return;
			}
			setTimeout(poll, POLL_MS);
		};
		poll();
	}

	/**
	 * Bring the link into the stage before hit-testing it.
	 *
	 * The fixture's link sits near the bottom of a page that is taller than the
	 * stage, so its centre can start below the viewport. `scrollIntoView` on the
	 * control scrolls the page-list scroller (the nearest scrollable ancestor),
	 * which is exactly where a reader would scroll to reach it. Three frames is
	 * enough for the scroll and the page-list repaint to settle; if the control
	 * still cannot be hit, the report says so and the verdict fails.
	 */
	function revealLink(chosen, links, attempt) {
		var box = chosen.getBoundingClientRect();
		var cx = box.left + box.width / 2;
		var cy = box.top + box.height / 2;
		if (
			attempt < 3 &&
			(cx < 0 || cx > globalThis.innerWidth || cy < 0 || cy > globalThis.innerHeight)
		) {
			chosen.scrollIntoView({ block: "center", inline: "center" });
			setTimeout(function () {
				revealLink(chosen, links, attempt + 1);
			}, 80);
			return;
		}
		box = chosen.getBoundingClientRect();
		cx = box.left + box.width / 2;
		cy = box.top + box.height / 2;
		var hit =
			box.width > 0 && box.height > 0 && cy >= 0 && cy <= globalThis.innerHeight
				? document.elementFromPoint(cx, cy)
				: null;
		report.link = {
			found: true,
			count: links.length,
			page: chosen.getAttribute("data-page"),
			actionKind: chosen.getAttribute("data-action-kind"),
			label: chosen.getAttribute("aria-label") || "",
			// The programmatic click below proves the handler; this proves
			// the rectangle is the topmost thing at its own centre, which
			// is what makes a pointer able to reach it at all.
			hit: hit !== null && (hit === chosen || chosen.contains(hit)),
			hitRole: hit === null ? null : hit.getAttribute("data-role"),
			hitTag: hit === null ? null : hit.tagName,
			box: {
				x: box.left,
				y: box.top,
				width: box.width,
				height: box.height,
			},
			viewport: {
				width: globalThis.innerWidth,
				height: globalThis.innerHeight,
			},
			prompt: null,
			closed: false,
		};
		chosen.click();
		waitForPrompt();
	}

	function waitForPrompt() {
		report.stage = "prompt";
		var deadline = Date.now() + PROMPT_WAIT_MS;
		var poll = function () {
			if (finished) {
				return;
			}
			var prompt = document.querySelector('[data-role="external-prompt"]');
			if (prompt !== null) {
				var cancel = prompt.querySelector('[data-role="external-cancel"]');
				report.link.prompt = {
					title: text(prompt.querySelector('[data-role="external-question"]')),
					body: text(prompt.querySelector('[data-role="external-url"]')),
					host: prompt.getAttribute("data-host") || "",
					cancel: text(cancel),
				};
				if (cancel === null) {
					fatal("the external-link prompt carried no cancel button", diagnostics());
					return;
				}
				cancel.click();
				waitForClose();
				return;
			}
			if (Date.now() >= deadline) {
				fatal("clicking the page link opened no external-link prompt", diagnostics());
				return;
			}
			setTimeout(poll, POLL_MS);
		};
		poll();
	}

	function waitForClose() {
		report.stage = "prompt-close";
		var deadline = Date.now() + PROMPT_WAIT_MS;
		var poll = function () {
			if (finished) {
				return;
			}
			if (document.querySelector('[data-role="external-prompt"]') === null) {
				report.link.closed = true;
				waitForPreviews();
				return;
			}
			if (Date.now() >= deadline) {
				fatal("the external-link prompt did not close on cancel", diagnostics());
				return;
			}
			setTimeout(poll, POLL_MS);
		};
		poll();
	}

	/**
	 * Wait for the rail's previews before taking the snapshot.
	 *
	 * The rail has its own scheduler and its own rung plan; the page canvas can
	 * be inked before the first thumbnail reaches a rung. The snapshot is what
	 * the verdict reads, so wait for the two pages' previews rather than racing
	 * a smaller document on a slower machine. The timeout is not a pass: if the
	 * count never arrives, `pressRow` records it and the verdict fails.
	 */
	function waitForPreviews() {
		report.stage = "previews";
		var deadline = Date.now() + PREVIEW_WAIT_MS;
		var poll = function () {
			if (finished) {
				return;
			}
			var thumbnails = document.querySelectorAll('[data-role="thumbnail"]').length;
			var painted = document.querySelectorAll(
				'[data-role="thumbnail"] canvas.selis-thumbnail__preview',
			).length;
			if ((thumbnails > 0 && painted >= thumbnails) || Date.now() >= deadline) {
				pressRow();
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
		var railElement = document.querySelector('[data-role="thumbnail-rail"]');
		report.rail = {
			present: railElement !== null,
			label: text(document.querySelector('[data-role="thumbnail-heading"]')),
			thumbnails: document.querySelectorAll('[data-role="thumbnail"]').length,
			previews: document.querySelectorAll(
				'[data-role="thumbnail"] canvas.selis-thumbnail__preview',
			).length,
			currentPage: currentRailPage(),
			scrollHeight: railElement === null ? 0 : railElement.scrollHeight,
			clientHeight: railElement === null ? 0 : railElement.clientHeight,
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
				railCurrent: after.railCurrent !== before.railCurrent,
				scrollTop: after.scrollTop !== before.scrollTop,
			};
			// The six the verdict treats as the move itself; the others are
			// recorded so a red run says which witness disagreed. The rail's
			// current page is part of the wait because it is a second
			// controller updating from the same page-list publication.
			if (
				(changed.ink && changed.live && changed.currentRow && changed.railCurrent) ||
				Date.now() >= deadline
			) {
				report.press.after = after;
				report.press.changed = changed;
				pressKey();
				return;
			}
			setTimeout(poll, POLL_MS);
		};
		poll();
	}

	/**
	 * The stage's own key map, after the bookmark move: ArrowUp must go back a
	 * page and the shell must claim the key instead of letting the browser
	 * scroll by a line. A synthetic event exercises the same listener a keyboard
	 * does; `defaultPrevented` is the witness that the listener claimed it.
	 */
	function pressKey() {
		report.stage = "key";
		var scroller = document.querySelector('[data-role="page-list-scroll"]');
		if (scroller === null) {
			fatal("the page-list scroller disappeared before the key probe", diagnostics());
			return;
		}
		scroller.focus();
		var before = snapshot();
		var event = new KeyboardEvent("keydown", {
			key: "ArrowUp",
			bubbles: true,
			cancelable: true,
		});
		scroller.dispatchEvent(event);
		report.key = {
			attempted: true,
			key: "ArrowUp",
			prevented: event.defaultPrevented,
			before: before,
			after: null,
			moved: false,
		};
		var deadline = Date.now() + KEY_WAIT_MS;
		var poll = function () {
			if (finished) {
				return;
			}
			var after = snapshot();
			var moved =
				after.currentRow !== before.currentRow ||
				after.railCurrent !== before.railCurrent ||
				after.scrollTop !== before.scrollTop;
			// Wait for the whole move, not the first witness: the row can
			// update on the controller's publication before the canvas has
			// painted the previous page. The final state is the claim.
			var settled =
				after.currentRow === 0 &&
				after.railCurrent === "0" &&
				after.scrollTop < before.scrollTop &&
				before.ink !== null &&
				after.ink !== null &&
				before.ink.hash !== null &&
				after.ink.hash !== null &&
				after.ink.hash !== before.ink.hash;
			if (settled || Date.now() >= deadline) {
				report.key.after = after;
				report.key.moved = moved;
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
