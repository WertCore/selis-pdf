/**
 * The platform-adapter conformance suite (24-BINDINGS-SPEC §1.7 applied to the
 * UI seam): one behavioural suite that *every* `PlatformAdapter` — the mock
 * today, the WASM.01 worker transport, the extension offscreen transport, and
 * the desktop Tauri transport later — must pass. A behavioural difference
 * between hosts is a build failure, not a support ticket.
 *
 * Usage for a new transport:
 * ```ts
 * definePlatformAdapterContract("wasm worker", () => createWorkerFixture());
 * ```
 * The fixture opens a three-page sample document with the texts
 * "alpha beta" / "gamma delta alpha" / "" and probes search for "alpha".
 */

import { describe, expect, it } from "vitest";
import type { PlatformAdapter } from "./adapter.js";
import { AdapterError, ErrorCode } from "./errors.js";
import type { DocHandle, DocumentSourceDescriptor } from "./types.js";

/** Fixture a transport provides to run the contract suite. */
export interface AdapterFixture {
	readonly adapter: PlatformAdapter;
	/** Descriptor that opens `doc`. */
	readonly descriptor: DocumentSourceDescriptor;
	/** An already-opened three-page sample document. */
	readonly doc: DocHandle;
}

export const SAMPLE_TEXTS: readonly string[] = ["alpha beta", "gamma delta alpha", ""];

/**
 * A probe a host may be *provably unable* to run.
 *
 * The suite's rule is "a behavioural difference between hosts is a build
 * failure", and that rule is right for everything the UI can observe. It is
 * wrong for a capability the host cannot have at all, where "the host refuses"
 * is the correct behaviour and the alternative is requesting a permission the
 * product has decided against. Clipboard read is the case that forced this:
 * the MV3 extension cannot have it without the `clipboardRead` permission, and
 * the extension's approved permission set (SL-4.EXT.01, `host_permissions: []`)
 * deliberately excludes it.
 *
 * Naming the exemption rather than editing the probe keeps the difference
 * visible: a host that stops needing it has to delete the entry, which is a
 * reviewable act, and the call site has to say why in words.
 */
export type ContractExemption = "clipboard-read";

/** Per-host exemptions, each carrying the reason it was claimed. */
export interface ContractOptions {
	/**
	 * `undefined` for an exemption the host claims. The value is the reason, and
	 * it is required: a skip with no stated cause is indistinguishable from a
	 * probe quietly deleted because it was inconvenient.
	 */
	readonly exempt?: Partial<Record<ContractExemption, string>>;
}

/** Register the contract suite for one adapter factory. */
export function definePlatformAdapterContract(
	name: string,
	createFixture: () => Promise<AdapterFixture> | AdapterFixture,
	options: ContractOptions = {},
): void {
	const exempt = options.exempt ?? {};
	describe(`${name} — platform-adapter contract`, () => {
		it("advertises complete capability flags", async () => {
			const { adapter } = await createFixture();
			expect(adapter.capabilities.platform.length).toBeGreaterThan(0);
			for (const flag of [
				"filePickers",
				"fileSystemAccess",
				"opfs",
				"httpRange",
				"clipboardRead",
				"print",
				"persistedStorage",
				"threads",
				"deepLinks",
			] as const) {
				expect(typeof adapter.capabilities[flag], flag).toBe("boolean");
			}
		});

		it("opens the sample document with page metadata", async () => {
			const { adapter, descriptor } = await createFixture();
			const doc = await adapter.engine.open(descriptor);
			expect(doc.pageCount).toBe(SAMPLE_TEXTS.length);
			expect(doc.pageSizes).toHaveLength(doc.pageCount);
			for (const size of doc.pageSizes) {
				expect(size.width).toBeGreaterThan(0);
				expect(size.height).toBeGreaterThan(0);
			}
		});

		it("rejects unregistered descriptors with a registry error", async () => {
			const { adapter } = await createFixture();
			const unknown: DocumentSourceDescriptor = {
				kind: "file",
				path: "/definitely/not/registered.pdf",
			};
			await expect(adapter.engine.open(unknown)).rejects.toBeInstanceOf(AdapterError);
		});

		it("renders a whole-page tile with tightly packed RGBA8 pixels", async () => {
			const { adapter, doc } = await createFixture();
			const tile = await adapter.engine.renderTile({ doc, page: 0, scale: 0.5 });
			expect(tile.page).toBe(0);
			expect(tile.format).toBe("rgba8");
			expect(tile.width).toBeGreaterThan(0);
			expect(tile.height).toBeGreaterThan(0);
			expect(tile.data.byteLength).toBe(tile.width * tile.height * 4);
			const pixels = new Uint8ClampedArray(tile.data);
			expect(pixels[3]).toBe(255);
		});

		it("honours tile rects", async () => {
			const { adapter, doc } = await createFixture();
			const whole = await adapter.engine.renderTile({ doc, page: 0, scale: 1 });
			const half = await adapter.engine.renderTile({
				doc,
				page: 0,
				scale: 1,
				rect: { x: 0, y: 0, width: doc.pageSizes[0]?.width ?? 0, height: 100 },
			});
			expect(half.height).toBeLessThanOrEqual(whole.height);
		});

		it("fails closed handles with BINDING_BAD_HANDLE and docState Unchanged", async () => {
			const { adapter, descriptor } = await createFixture();
			const doc = await adapter.engine.open(descriptor);
			await adapter.engine.close(doc);
			await expect(adapter.engine.renderTile({ doc, page: 0, scale: 1 })).rejects.toMatchObject({
				code: ErrorCode.BindingBadHandle,
				docState: "Unchanged",
			});
		});

		it("rejects out-of-range pages with BINDING_BAD_ARGUMENT", async () => {
			const { adapter, doc } = await createFixture();
			await expect(
				adapter.engine.renderTile({ doc, page: doc.pageCount, scale: 1 }),
			).rejects.toMatchObject({
				code: ErrorCode.BindingBadArgument,
			});
		});

		it("surfaces pre-aborted cancellation as CANCELLED / Unchanged", async () => {
			const { adapter, doc } = await createFixture();
			const controller = new AbortController();
			controller.abort();
			await expect(
				adapter.engine.renderTile({ doc, page: 0, scale: 1 }, { signal: controller.signal }),
			).rejects.toMatchObject({ code: ErrorCode.Cancelled, docState: "Unchanged" });
		});

		it("stops a running search when the signal fires mid-scan", async () => {
			const { adapter, doc } = await createFixture();
			const controller = new AbortController();
			const iterator = adapter.engine
				.search(doc, "a", undefined, { signal: controller.signal })
				[Symbol.asyncIterator]();
			const first = await iterator.next();
			expect(first.done).toBe(false);
			controller.abort();
			await expect(iterator.next()).rejects.toMatchObject({ code: ErrorCode.Cancelled });
		});

		it("extracts text in reading order", async () => {
			const { adapter, doc } = await createFixture();
			const page = await adapter.engine.extractText(doc, 0);
			expect(page.page).toBe(0);
			expect(page.text).toBe(SAMPLE_TEXTS[0]);
		});

		it("streams search progressively and terminates with done", async () => {
			const { adapter, doc } = await createFixture();
			let matches = 0;
			let sawDone = false;
			for await (const batch of adapter.engine.search(doc, "alpha")) {
				matches += batch.matches.length;
				expect(batch.progress).toBeGreaterThan(0);
				expect(batch.progress).toBeLessThanOrEqual(1);
				if (batch.done) {
					sawDone = true;
					expect(batch.progress).toBe(1);
				}
			}
			expect(sawDone).toBe(true);
			expect(matches).toBe(2);
		});

		it("round-trips storage settings", async () => {
			const { adapter } = await createFixture();
			await expect(adapter.storage.get("missing")).resolves.toBeNull();
			await adapter.storage.set("ui.theme", "dark");
			await expect(adapter.storage.get("ui.theme")).resolves.toBe("dark");
			await adapter.storage.delete("ui.theme");
			await expect(adapter.storage.get("ui.theme")).resolves.toBeNull();
		});

		it.skipIf(exempt["clipboard-read"] !== undefined)("round-trips the clipboard", async () => {
			const { adapter } = await createFixture();
			await adapter.clipboard.writeText("selected text");
			await expect(adapter.clipboard.readText()).resolves.toBe("selected text");
		});

		it("keeps telemetry off by default", async () => {
			const { adapter } = await createFixture();
			expect(adapter.telemetry.isEnabled()).toBe(false);
			adapter.telemetry.record({ name: "session_start" });
			await adapter.telemetry.setEnabled(true);
			expect(adapter.telemetry.isEnabled()).toBe(true);
			adapter.telemetry.record({ name: "session_start", metrics: { pages_shown: 3 } });
		});

		// SL-4.UI.11. `locale` is optional for the same reason `navigation` is:
		// a host that cannot say what language the reader wants should report
		// nothing rather than guess, and the viewer treats absence as "ship the
		// source locale". The probes are therefore conditional on the port's
		// presence, and the *shape* of the absence is asserted too, because
		// "no port" and "a port that throws" are different states and the shell
		// branches on the first.
		describe("locale port", () => {
			it("is present or absent, never half-present", async () => {
				const { adapter } = await createFixture();
				const port = adapter.locale;
				if (port === undefined) {
					return;
				}
				expect(typeof port.current).toBe("function");
				expect(typeof port.onChange).toBe("function");
			});

			it("reports a non-empty tag that a shell can resolve", async () => {
				const { adapter } = await createFixture();
				// The tag itself is the host's business — `negotiateLocale` maps it
				// onto what the build ships — but "empty" is not a tag, and a
				// shell that got one would have nothing to negotiate from.
				expect(adapter.locale?.current().trim() ?? "").not.toBe("");
			});

			it("hands back an unsubscribe, and unsubscribing twice is harmless", async () => {
				const { adapter } = await createFixture();
				if (adapter.locale === undefined) {
					return;
				}
				const stop = adapter.locale.onChange(() => undefined);
				expect(typeof stop).toBe("function");
				stop();
				// A shell that tears down twice (a hot reload, a closed tab) must
				// not have to know whether the host already dropped the handler.
				expect(() => stop()).not.toThrow();
			});

			// There is deliberately **no** "delivers a change" probe. The port is
			// read-only by design (a chosen locale is a *setting*, and settings go
			// through `storage`), so a host with a fixed build-time language
			// legitimately never fires `onChange`. A probe demanding a change
			// would force every transport to invent a writable language setting,
			// which is the half of the design this port refuses. The mock, which
			// does have one, is held to the delivery behaviour in
			// `i18n/runtime.test.ts` instead of here.
		});

		// SL-4.UI.06. `navigation` is optional by design (a host whose engine has
		// no outline walk reports `undefined`), so the probes are conditional on
		// its presence rather than skipped per host: a host that *does* declare
		// the port is held to it, and a host that does not is not asked to
		// pretend. The suite still asserts the shape of the absence, because
		// "no navigation" and "navigation that throws" are different states and
		// the viewer branches on the first.
		describe("navigation port", () => {
			it("is present or absent, never half-present", async () => {
				const { adapter } = await createFixture();
				const port = adapter.navigation;
				if (port === undefined) {
					return;
				}
				for (const method of ["outline", "pageLabels", "destinations", "pageLinks"] as const) {
					expect(typeof port[method], method).toBe("function");
				}
			});

			it("returns navigation data for a live handle", async () => {
				const { adapter, doc } = await createFixture();
				if (adapter.navigation === undefined) {
					return;
				}
				const [outline, labels, destinations, links] = await Promise.all([
					adapter.navigation.outline(doc),
					adapter.navigation.pageLabels(doc),
					adapter.navigation.destinations(doc),
					adapter.navigation.pageLinks(doc, 0),
				]);
				// Shape, not content: the contract suite's fixture document has
				// no outline, and a host must not invent one.
				expect(Array.isArray(outline)).toBe(true);
				expect(Array.isArray(labels)).toBe(true);
				expect(Array.isArray(destinations)).toBe(true);
				expect(Array.isArray(links)).toBe(true);
			});

			it("rejects out-of-range pages and closed handles on pageLinks", async () => {
				const { adapter, descriptor, doc } = await createFixture();
				if (adapter.navigation === undefined) {
					return;
				}
				await expect(adapter.navigation.pageLinks(doc, doc.pageCount)).rejects.toMatchObject({
					code: ErrorCode.BindingBadArgument,
				});
				await adapter.engine.close(doc);
				await expect(adapter.navigation.outline(doc)).rejects.toMatchObject({
					code: ErrorCode.BindingBadHandle,
					docState: "Unchanged",
				});
				const reopened = await adapter.engine.open(descriptor);
				expect(Array.isArray(await adapter.navigation.pageLabels(reopened))).toBe(true);
			});

			it("surfaces pre-aborted cancellation as CANCELLED / Unchanged", async () => {
				const { adapter, doc } = await createFixture();
				if (adapter.navigation === undefined) {
					return;
				}
				const controller = new AbortController();
				controller.abort();
				await expect(
					adapter.navigation.pageLinks(doc, 0, { signal: controller.signal }),
				).rejects.toMatchObject({ code: ErrorCode.Cancelled, docState: "Unchanged" });
			});
		});
	});
}
