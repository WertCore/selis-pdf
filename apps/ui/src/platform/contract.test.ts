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

	// SL-4.UI.06 — the navigation port's own specifics. The shared contract
	// covers the port's *shape*; these are the mock's guarantees a UI.06 test
	// leans on, and the reason the disabled action classes survive the trip
	// through the transport at all.
	describe("navigation specifics", () => {
		async function navFixture() {
			const adapter = createMockAdapter();
			const descriptor = adapter.addDocument({
				name: "nav.pdf",
				pageCount: 3,
				pageLabels: [
					{ firstPage: 0, style: "r", prefix: "p" },
					{ firstPage: 2, style: "D", firstValue: 7 },
				],
				outline: [
					{
						title: "Chapter one",
						destination: { page: 0, kind: "fit" },
						descendantCount: -1,
						children: [{ title: "Figure 1", destination: { page: 1, kind: "xyz", top: 700 } }],
					},
					{ title: "Missing", namedDestination: "nowhere" },
				],
				destinations: [{ name: "here", destination: { page: 2 } }],
				links: [
					{
						page: 0,
						links: [
							{
								rect: { x: 72, y: 700, width: 100, height: 12 },
								action: { kind: "uri", uri: "https://example.com/a" },
							},
							{
								id: "launch-1",
								rect: { x: 72, y: 680, width: 100, height: 12 },
								action: { kind: "launch", uri: "C:/payload.exe" },
							},
						],
					},
				],
			});
			const doc = await adapter.engine.open(descriptor);
			return { adapter, doc };
		}

		it("serves the declarative outline, labels, destinations and links", async () => {
			const { adapter, doc } = await navFixture();
			const navigation = adapter.navigation;
			if (navigation === undefined) {
				throw new Error("the mock must provide the navigation port");
			}
			const outline = await navigation.outline(doc);
			expect(outline).toHaveLength(2);
			expect(outline[0]?.title).toBe("Chapter one");
			expect(outline[0]?.children?.[0]?.destination?.page).toBe(1);
			// An item naming a destination the name tree does not define comes
			// back as itself, not dropped: the outline is the document's.
			expect(outline[1]?.namedDestination).toBe("nowhere");
			expect(outline[1]?.destination).toBeUndefined();
			expect(await navigation.pageLabels(doc)).toHaveLength(2);
			expect(await navigation.destinations(doc)).toEqual([
				{ name: "here", destination: { page: 2 } },
			]);
			const links = await navigation.pageLinks(doc, 0);
			expect(links.map((link) => link.id)).toEqual(["p0-l0", "launch-1"]);
		});

		it("reports ADR-P0020's disabled action classes verbatim", async () => {
			const { adapter, doc } = await navFixture();
			const links = await adapter.navigation?.pageLinks(doc, 0);
			// The transport's job is to say what the document says. A mock that
			// dropped `/Launch` would make the viewer's refusal untestable, and
			// would let a real transport get away with the same.
			expect(links?.map((link) => link.action.kind)).toEqual(["uri", "launch"]);
			expect(links?.[1]?.action.uri).toBe("C:/payload.exe");
		});

		it("defaults every navigation structure to empty, not to an error", async () => {
			const adapter = createMockAdapter();
			const descriptor = adapter.addDocument({ name: "plain.pdf", pageCount: 2 });
			const doc = await adapter.engine.open(descriptor);
			expect(await adapter.navigation?.outline(doc)).toEqual([]);
			expect(await adapter.navigation?.pageLabels(doc)).toEqual([]);
			expect(await adapter.navigation?.destinations(doc)).toEqual([]);
			expect(await adapter.navigation?.pageLinks(doc, 1)).toEqual([]);
		});

		it("rejects an out-of-range page on pageLinks, like every per-page port", async () => {
			const { adapter, doc } = await navFixture();
			await expect(adapter.navigation?.pageLinks(doc, 3)).rejects.toMatchObject({
				code: ErrorCode.BindingBadArgument,
			});
		});

		it("honours cancellation on every navigation method", async () => {
			const { adapter, doc } = await navFixture();
			const controller = new AbortController();
			controller.abort();
			const options = { signal: controller.signal };
			const navigation = adapter.navigation;
			if (navigation === undefined) {
				throw new Error("the mock must provide the navigation port");
			}
			for (const call of [
				navigation.outline(doc, options),
				navigation.pageLabels(doc, options),
				navigation.destinations(doc, options),
				navigation.pageLinks(doc, 0, options),
			]) {
				await expect(call).rejects.toMatchObject({
					code: ErrorCode.Cancelled,
					docState: "Unchanged",
				});
			}
		});
	});
});
