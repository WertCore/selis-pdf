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

/** Register the contract suite for one adapter factory. */
export function definePlatformAdapterContract(
	name: string,
	createFixture: () => Promise<AdapterFixture> | AdapterFixture,
): void {
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

		it("round-trips the clipboard", async () => {
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
	});
}
