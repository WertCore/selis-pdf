import { describe, expect, it } from "vitest";
import { type AdapterFixture, SAMPLE_TEXTS, definePlatformAdapterContract } from "./contract.js";
import { AdapterError, ErrorCode } from "./errors.js";
import { createMockAdapter } from "./mock-adapter.js";

async function createFixture(): Promise<AdapterFixture> {
	const adapter = createMockAdapter();
	const descriptor = adapter.addDocument({
		name: "sample.pdf",
		pageCount: SAMPLE_TEXTS.length,
		textPages: SAMPLE_TEXTS,
	});
	const doc = await adapter.engine.open(descriptor);
	return { adapter, descriptor, doc };
}

// The mock must satisfy the exact suite every future transport runs.
definePlatformAdapterContract("mock adapter", createFixture);

describe("mock adapter specifics", () => {
	it("yields queued pickOpen descriptors exactly once", async () => {
		const adapter = createMockAdapter();
		const descriptor = adapter.addDocument({ name: "picked.pdf", pageCount: 1 });
		adapter.queueOpen(descriptor);
		await expect(adapter.files.pickOpen()).resolves.toEqual([descriptor]);
		await expect(adapter.files.pickOpen()).resolves.toEqual([]);
	});

	it("resolves pickSave to null unless a save was queued", async () => {
		const adapter = createMockAdapter();
		await expect(adapter.files.pickSave("out.pdf")).resolves.toBeNull();
		adapter.queueSave("out.pdf");
		const target = await adapter.files.pickSave("out.pdf");
		expect(target).not.toBeNull();
		const bytes = new Uint8Array([1, 2, 3]);
		await target?.write(bytes);
		expect(adapter.recording.savedFiles.get("out.pdf")).toEqual(bytes);
	});

	it("reports progress during render", async () => {
		const { adapter, doc } = await createFixture();
		const stages: Array<{ fraction: number; stage: string }> = [];
		await adapter.engine.renderTile(
			{ doc, page: 0, scale: 1 },
			{
				onProgress: (p) => stages.push(p),
			},
		);
		expect(stages).toEqual([
			{ fraction: 0.5, stage: "render" },
			{ fraction: 1, stage: "render" },
		]);
	});

	it("drops telemetry recorded before opt-in and keeps it after", async () => {
		const adapter = createMockAdapter();
		adapter.telemetry.record({ name: "before_opt_in" });
		await adapter.telemetry.setEnabled(true);
		adapter.telemetry.record({ name: "after_opt_in", metrics: { opens: 1 } });
		expect(adapter.recording.telemetryEvents).toEqual([
			{ name: "after_opt_in", metrics: { opens: 1 } },
		]);
	});

	it("routes deep links to active handlers and honours unsubscribe", async () => {
		const adapter = createMockAdapter();
		const seen: string[] = [];
		const unsubscribe = adapter.window.onDeepLink((link) => seen.push(link.url));
		adapter.emitDeepLink({ url: "selis://open?src=local" });
		unsubscribe();
		adapter.emitDeepLink({ url: "selis://open?second=1" });
		expect(seen).toEqual(["selis://open?src=local"]);
		expect(adapter.recording.deepLinks).toHaveLength(2);
	});

	it("rejects non-PDF bytes with NOT_A_PDF (registry 1000)", async () => {
		const adapter = createMockAdapter();
		const notPdf = new TextEncoder().encode("GIF89a definitely not a pdf");
		const descriptor = adapter.addDocument({ name: "cat.gif", pageCount: 1, bytes: notPdf });
		await expect(adapter.engine.open(descriptor)).rejects.toMatchObject({
			code: 1000,
			docState: "NotLoaded",
		});
	});

	it("rejects descriptors it did not register with IO_READ_FAILED", async () => {
		const adapter = createMockAdapter();
		const unknown: Parameters<typeof adapter.engine.open>[0] = {
			kind: "file",
			path: "C:/ nowhere.pdf",
		};
		await expect(adapter.engine.open(unknown)).rejects.toMatchObject({
			code: ErrorCode.IoReadFailed,
		});
	});

	it("enforces the pixel budget with BUDGET_PIXELS and docState Loaded", async () => {
		const adapter = createMockAdapter();
		const descriptor = adapter.addDocument({
			name: "huge.pdf",
			pageCount: 1,
			pageSize: { width: 100000, height: 100000 },
		});
		const doc = await adapter.engine.open(descriptor);
		await expect(adapter.engine.renderTile({ doc, page: 0, scale: 1 })).rejects.toMatchObject({
			code: ErrorCode.BudgetPixels,
			docState: "Loaded",
			retryable: true,
		});
	});

	it("supports case-sensitive and whole-word search", async () => {
		const adapter = createMockAdapter();
		const descriptor = adapter.addDocument({
			name: "text.pdf",
			pageCount: 1,
			textPages: ["Alpha alpha Alphabet alpha-soup"],
		});
		const doc = await adapter.engine.open(descriptor);

		async function collect(query: string, opts?: { caseSensitive?: boolean; wholeWord?: boolean }) {
			const all = [];
			for await (const batch of adapter.engine.search(doc, query, opts)) {
				all.push(...batch.matches);
			}
			return all;
		}

		expect((await collect("alpha")).length).toBe(4);
		expect((await collect("alpha", { caseSensitive: true })).length).toBe(2);
		const words = await collect("alpha", { wholeWord: true });
		expect(words.length).toBe(3);
		expect(words.every((m) => m.end - m.start === 5)).toBe(true);
	});

	it("honours fromPage and reports scanned fraction", async () => {
		const { adapter, doc } = await createFixture();
		const batches = [];
		for await (const batch of adapter.engine.search(doc, "delta", { fromPage: 1 })) {
			batches.push(batch);
		}
		expect(batches).toHaveLength(2);
		expect(batches[0]?.progress).toBeCloseTo(0.5);
		expect(batches[1]?.progress).toBe(1);
		expect(batches[1]?.done).toBe(true);
	});

	it("rejects an empty search query as BINDING_BAD_ARGUMENT", async () => {
		const { adapter, doc } = await createFixture();
		const iterator = adapter.engine.search(doc, "")[Symbol.asyncIterator]();
		await expect(iterator.next()).rejects.toBeInstanceOf(AdapterError);
	});
});
