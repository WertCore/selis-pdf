/**
 * SL-4.EXT.05 — the CJK payload store, and the producer it does not have.
 *
 * The suite is written around the split this task decides: the *store* is
 * implemented and tested here, and the *producer* is deliberately absent, so
 * the tests pin both halves. A store that quietly accepted bytes it could not
 * verify, or a budget that could be exceeded by installing one more chunk, would
 * be worse than no store at all — the whole argument for a post-install
 * download is that it is bounded, and a bound that is only documented is not
 * one.
 */

import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";
import {
	CHROME_STORAGE_LOCAL_QUOTA_BYTES,
	CJK_BROTLI_BUDGETS,
	CJK_KEY_PREFIX,
	CJK_MANIFEST_SCHEMA,
	CJK_PAYLOAD_RESIDENT_BYTES,
	CJK_PAYLOAD_SOURCE,
	CJK_STORAGE_BUDGET_BYTES,
	type CjkManifest,
	CjkPayloadError,
	type CjkSource,
	type CjkStorage,
	cjkAvailability,
	createChromeStorageLocal,
	installCjkChunk,
	installedCjkIds,
	parseCjkManifest,
	readCjkChunk,
	removeCjkChunk,
} from "./cjk-payload.js";

/** The repository root, for the payload manifest FONT.10 produced. */
const repoRoot = join(dirname(fileURLToPath(import.meta.url)), "..", "..", "..", "..");

/** An in-memory {@link CjkStorage}, so the store is testable with no `chrome`. */
function memoryStorage(seed: Record<string, Uint8Array> = {}): CjkStorage & {
	readonly written: Map<string, Uint8Array>;
} {
	const written = new Map<string, Uint8Array>(Object.entries(seed));
	return {
		written,
		async get(key) {
			return written.get(key) ?? null;
		},
		async set(key, value) {
			written.set(key, value);
		},
		async remove(key) {
			written.delete(key);
		},
		async keys() {
			return [...written.keys()];
		},
	};
}

/** Lowercase hex SHA-256, computed the same way the store computes it. */
async function sha256(bytes: Uint8Array): Promise<string> {
	const digest = await crypto.subtle.digest("SHA-256", bytes as unknown as ArrayBuffer);
	return [...new Uint8Array(digest)].map((b) => b.toString(16).padStart(2, "0")).join("");
}

/**
 * Bytes that do NOT compress, plus the manifest row that describes them.
 *
 * The default {@link payload} fills with a single repeated byte, which deflate
 * reduces to almost nothing. That is fine for tests about verification, but it
 * is not a thing the store can be asked to hold, so a test about the storage
 * budget built on it would be measuring a rounding error. Font data is
 * high-entropy; these bytes stand in for that, and a chunk built from them
 * occupies roughly the space its manifest claims.
 */
async function incompressiblePayload(id: string, rawBytes: number, brotliBytes: number) {
	const bytes = new Uint8Array(rawBytes);
	// xorshift32: deterministic, so a failure is reproducible, and high-entropy
	// enough that deflate has nothing to find.
	let state = 0x9e3779b9 ^ (id.length * 0x85ebca6b);
	for (let i = 0; i < rawBytes; i += 1) {
		state ^= state << 13;
		state ^= state >>> 17;
		state ^= state << 5;
		bytes[i] = state & 0xff;
	}
	return {
		bytes,
		row: {
			id,
			file: `cjk/${id}.ttf`,
			raw_bytes: rawBytes,
			brotli_bytes: brotliBytes,
			sha256: await sha256(bytes),
		},
	};
}

/** Payload bytes of a given length, plus the manifest row that describes them. */
async function payload(id: string, rawBytes: number, brotliBytes: number) {
	const bytes = new Uint8Array(rawBytes).fill(0x41);
	return {
		bytes,
		row: {
			id,
			file: `cjk/${id}.ttf`,
			raw_bytes: rawBytes,
			brotli_bytes: brotliBytes,
			sha256: await sha256(bytes),
		},
	};
}

/** A manifest over the given rows, shaped as `xtask cjk-build` writes it. */
async function manifestOf(
	rows: readonly { id: string; raw_bytes: number; brotli_bytes: number; sha256: string }[],
): Promise<CjkManifest> {
	const core = rows.find((row) => row.id === "core") ?? rows[0];
	if (core === undefined) {
		throw new Error("a manifest needs at least the core");
	}
	const value = {
		schema: CJK_MANIFEST_SCHEMA,
		core,
		chunks: rows.filter((row) => row.id !== "core"),
	};
	// Round-tripped through the parser so a test fixture and a real install
	// see the same record shape.
	return parseCjkManifest(JSON.parse(JSON.stringify(value)));
}

/** A source serving exactly the rows it was built from. */
function sourceOf(manifest: CjkManifest, files: ReadonlyMap<string, Uint8Array>): CjkSource {
	return {
		manifest: async () => JSON.parse(JSON.stringify(manifest)) as unknown,
		bytes: async (file) => {
			const found = files.get(file.file);
			if (found === undefined) {
				throw new Error(`no bytes for ${file.file}`);
			}
			return found;
		},
	};
}

/** The code a rejection carries, or a thrown assertion if it was not typed. */
async function codeOf(run: () => Promise<unknown>): Promise<string> {
	try {
		await run();
	} catch (error) {
		expect(error).toBeInstanceOf(CjkPayloadError);
		return (error as CjkPayloadError).code;
	}
	throw new Error("expected a CjkPayloadError, and nothing was thrown");
}

describe("EXT.05 the CJK payload is a post-install download, not a bundled asset", () => {
	it("has no producer, and says so with a code rather than a placeholder", async () => {
		// SL-3.FONT.10 has not shipped a `cjk/manifest.json`. The honest
		// outcome is a typed refusal; a placeholder URL would be remote content
		// in an ADR-P0028 package and a claim that a payload exists.
		expect(await codeOf(() => CJK_PAYLOAD_SOURCE.manifest())).toBe("cjk-no-producer");
		expect(
			await codeOf(async () =>
				CJK_PAYLOAD_SOURCE.bytes({
					id: "core",
					file: "cjk/core.ttf",
					raw_bytes: 1,
					brotli_bytes: 1,
					sha256: "0".repeat(64),
				}),
			),
		).toBe("cjk-no-producer");
	});

	it("refuses to install anything while the shipped source refuses", async () => {
		const storage = memoryStorage();
		expect(
			await codeOf(() => installCjkChunk({ storage, source: CJK_PAYLOAD_SOURCE, id: "core" })),
		).toBe("cjk-no-producer");
		expect(await installedCjkIds(storage)).toEqual([]);
	});

	it("keeps its storage budget under the quota it is budgeted against", () => {
		// The escalation PERMISSIONS.md names — `unlimitedStorage` — is only
		// reachable if this number ever exceeds the default quota, so the two
		// are asserted against each other rather than left to a reader.
		expect(CJK_STORAGE_BUDGET_BYTES).toBeLessThan(CHROME_STORAGE_LOCAL_QUOTA_BYTES);
	});
});

describe("EXT.05 the manifest is validated before anything is fetched", () => {
	it("rejects a record from another schema", () => {
		expect(() => parseCjkManifest({ schema: "something/2", core: {}, chunks: [] })).toThrow(
			/selis-cjk\/1/,
		);
	});

	it("rejects a manifest with no core, or no chunk list", () => {
		expect(() => parseCjkManifest({ schema: CJK_MANIFEST_SCHEMA, chunks: [] })).toThrow(/core/);
		expect(() => parseCjkManifest({ schema: CJK_MANIFEST_SCHEMA, core: {} })).toThrow(/chunks/);
	});

	it("rejects a chunk whose integrity pin is not a sha256", () => {
		const row = {
			id: "core",
			file: "cjk/core.ttf",
			raw_bytes: 10,
			brotli_bytes: 5,
			sha256: "nope",
		};
		expect(() => parseCjkManifest({ schema: CJK_MANIFEST_SCHEMA, core: row, chunks: [] })).toThrow(
			/sha256/,
		);
	});

	it("rejects non-numeric sizes, which would make every budget a comparison against NaN", () => {
		const row = {
			id: "core",
			file: "cjk/core.ttf",
			raw_bytes: "10",
			brotli_bytes: 5,
			sha256: "a".repeat(64),
		};
		expect(() => parseCjkManifest({ schema: CJK_MANIFEST_SCHEMA, core: row, chunks: [] })).toThrow(
			/non-numeric/,
		);
	});
});

describe("EXT.05 the store installs, verifies, bounds and removes", () => {
	it("installs a chunk whose bytes match the manifest, and reads it back", async () => {
		const core = await payload("core", 4_096, 2_000);
		const kana = await payload("kana", 2_048, 900);
		const manifest = await manifestOf([core.row, kana.row]);
		const storage = memoryStorage();
		const source = sourceOf(
			manifest,
			new Map([
				[core.row.file, core.bytes],
				[kana.row.file, kana.bytes],
			]),
		);

		const result = await installCjkChunk({ storage, source, id: "kana" });
		expect(result).toEqual({ id: "kana", rawBytes: 2_048, brotliBytes: 900, residentBytes: 2_048 });
		expect(await installedCjkIds(storage)).toEqual(["kana"]);
		expect([...(await readCjkChunk(storage, "kana"))]).toEqual([...kana.bytes]);
		// The key is versioned, so a schema change is visible in the store
		// rather than silently reusing a record.
		expect([...storage.written.keys()]).toEqual([`${CJK_KEY_PREFIX}kana`]);
	});

	it("refuses a chunk whose bytes are not the ones the manifest pins", async () => {
		const core = await payload("core", 4_096, 2_000);
		const manifest = await manifestOf([core.row]);
		const storage = memoryStorage();
		// Same length, different content: the case a size check alone waves
		// through, and the reason the store hashes.
		const tampered = new Uint8Array(core.bytes).fill(0x42);
		const source = sourceOf(manifest, new Map([[core.row.file, tampered]]));
		expect(await codeOf(() => installCjkChunk({ storage, source, id: "core" }))).toBe(
			"cjk-integrity",
		);
		expect(storage.written.size).toBe(0);
	});

	it("refuses a chunk of the wrong length before hashing it", async () => {
		const core = await payload("core", 4_096, 2_000);
		const manifest = await manifestOf([core.row]);
		const storage = memoryStorage();
		const short = new Uint8Array(16);
		const source = sourceOf(manifest, new Map([[core.row.file, short]]));
		expect(await codeOf(() => installCjkChunk({ storage, source, id: "core" }))).toBe(
			"cjk-integrity",
		);
		expect(storage.written.size).toBe(0);
	});

	it("refuses a chunk the manifest's own brotli budget forbids", async () => {
		// The producer (`xtask cjk-build`) would never emit this, but a
		// consumer that trusted the manifest would install it.
		const fat = await payload("core", 9_000_000, CJK_BROTLI_BUDGETS.core + 1);
		const manifest = await manifestOf([fat.row]);
		const storage = memoryStorage();
		const source = sourceOf(manifest, new Map([[fat.row.file, fat.bytes]]));
		expect(await codeOf(() => installCjkChunk({ storage, source, id: "core" }))).toBe(
			"cjk-over-budget",
		);
		expect(storage.written.size).toBe(0);
	});

	it("refuses the install that would push the store past its budget", async () => {
		// Two chunks that each fit, together over the resident budget. The
		// second is refused and the first stays installed, so a user near the
		// limit keeps what they had.
		const half = Math.floor(CJK_STORAGE_BUDGET_BYTES / 2) + 1;
		const first = await incompressiblePayload("core", half, 1_000);
		const second = await incompressiblePayload("kana", half, 1_000);
		const manifest = await manifestOf([first.row, second.row]);
		const storage = memoryStorage();
		const source = sourceOf(
			manifest,
			new Map([
				[first.row.file, first.bytes],
				[second.row.file, second.bytes],
			]),
		);
		await installCjkChunk({ storage, source, id: "core" });
		expect(await codeOf(() => installCjkChunk({ storage, source, id: "kana" }))).toBe(
			"cjk-over-budget",
		);
		expect(await installedCjkIds(storage)).toEqual(["core"]);
	});

	/**
	 * FONT.10-F1, pinned as a test rather than as prose.
	 *
	 * The shipped payload's *raw* total is 11 162 268 B, over both the 8 MiB
	 * resident budget and Chrome's 10 MiB `storage.local` quota, so counting
	 * storage against `raw_bytes` refused a payload that does not actually need
	 * the room. These two chunks are the same shape as that refusal: together
	 * they declare more raw bytes than the budget, and both must install, because
	 * the store is charged what it actually wrote.
	 */
	it("installs a payload whose RAW total is over the budget, because storage is not raw", async () => {
		const half = Math.floor(CJK_STORAGE_BUDGET_BYTES / 2) + 1;
		// Compressible, like the real payload: the point is that a chunk which
		// declares `half` bytes may occupy far fewer.
		const first = await payload("core", half, 1_000);
		const second = await payload("kana", half, 1_000);
		expect(first.row.raw_bytes + second.row.raw_bytes).toBeGreaterThan(CJK_STORAGE_BUDGET_BYTES);
		const manifest = await manifestOf([first.row, second.row]);
		const storage = memoryStorage();
		const source = sourceOf(
			manifest,
			new Map([
				[first.row.file, first.bytes],
				[second.row.file, second.bytes],
			]),
		);
		await installCjkChunk({ storage, source, id: "core" });
		await installCjkChunk({ storage, source, id: "kana" });
		expect(await installedCjkIds(storage)).toEqual(["core", "kana"]);
		// And the store really is smaller than the raw bytes it was handed,
		// which is the whole of the claim.
		expect(await readCjkChunk(storage, "core")).toHaveLength(first.row.raw_bytes);
	});

	it("reads back exactly the bytes the manifest pinned, not a truncated stream", async () => {
		const core = await payload("core", 9_999, 2_000);
		const manifest = await manifestOf([core.row]);
		const storage = memoryStorage();
		const source = sourceOf(manifest, new Map([[core.row.file, core.bytes]]));
		await installCjkChunk({ storage, source, id: "core" });
		expect(await readCjkChunk(storage, "core")).toEqual(core.bytes);
	});

	it("refuses an id the manifest does not carry", async () => {
		const core = await payload("core", 4_096, 2_000);
		const manifest = await manifestOf([core.row]);
		const storage = memoryStorage();
		const source = sourceOf(manifest, new Map([[core.row.file, core.bytes]]));
		expect(await codeOf(() => installCjkChunk({ storage, source, id: "hangul" }))).toBe(
			"cjk-not-installed",
		);
	});

	it("removes an installed chunk, and refuses to remove one that is not there", async () => {
		const core = await payload("core", 4_096, 2_000);
		const manifest = await manifestOf([core.row]);
		const storage = memoryStorage();
		const source = sourceOf(manifest, new Map([[core.row.file, core.bytes]]));
		await installCjkChunk({ storage, source, id: "core" });
		await removeCjkChunk(storage, "core");
		expect(await installedCjkIds(storage)).toEqual([]);
		expect(await codeOf(() => readCjkChunk(storage, "core"))).toBe("cjk-not-installed");
		expect(await codeOf(() => removeCjkChunk(storage, "core"))).toBe("cjk-not-installed");
	});

	it("leaves keys it does not own alone when listing what is installed", async () => {
		// The payload store shares `storage.local` with nothing else today, and
		// a prefix filter is what keeps it that way if that changes.
		const storage = memoryStorage({ "selis.telemetry.enabled": new Uint8Array([1]) });
		expect(await installedCjkIds(storage)).toEqual([]);
	});
});

describe("EXT.05 the chrome.storage.local binding", () => {
	it("refuses, with a code, in a context that has no chrome.storage", async () => {
		// Node, and any extension context without the API. A binding that
		// threw a bare TypeError would be indistinguishable from a bug.
		const storage = createChromeStorageLocal();
		expect(await codeOf(() => storage.get(`${CJK_KEY_PREFIX}core`))).toBe("cjk-no-producer");
		expect(await codeOf(() => storage.keys())).toBe("cjk-no-producer");
	});

	it("round-trips bytes through a chrome.storage.local double", async () => {
		const area = new Map<string, number[]>();
		const globals = globalThis as { chrome?: unknown };
		const previous = globals.chrome;
		globals.chrome = {
			storage: {
				local: {
					async get(key: string) {
						const value = area.get(key);
						return value === undefined ? {} : { [key]: value };
					},
					async set(items: Record<string, number[]>) {
						for (const [key, value] of Object.entries(items)) {
							area.set(key, value);
						}
					},
					async remove(key: string) {
						area.delete(key);
					},
					async keys() {
						return [...area.keys()];
					},
				},
			},
		};
		try {
			const storage = createChromeStorageLocal();
			// Every byte value, because a binding that stored the typed array
			// itself would round-trip as an object in the real API.
			const key = `${CJK_KEY_PREFIX}core`;
			const bytes = new Uint8Array([0, 1, 2, 253, 254, 255]);
			await storage.set(key, bytes);
			expect(Array.from((await storage.get(key)) ?? [])).toEqual(Array.from(bytes));
			expect(await storage.keys()).toEqual([key]);
			await storage.remove(key);
			expect(await storage.get(key)).toBeNull();
		} finally {
			// Assigned rather than deleted: `delete` is a lint error here, and
			// leaving an explicit `undefined` behind is equivalent for every
			// reader of `globalThis.chrome` (it is an optional chain).
			globals.chrome = previous;
		}
	});
});

describe("CJK availability (SL-4.EXT.07)", () => {
	it("reports the measured payload as not fitting, with the overage", () => {
		// FONT.10-F1. The options page shows this arithmetic rather than a
		// "coming soon", so it has to be true and it has to be computed.
		const availability = cjkAvailability();
		expect(availability.budgetBytes).toBe(CJK_STORAGE_BUDGET_BYTES);
		expect(availability.fitsBudget).toBe(false);
		expect(availability.overBudgetBy).toBe(CJK_PAYLOAD_RESIDENT_BYTES - CJK_STORAGE_BUDGET_BYTES);
		expect(availability.overBudgetBy).toBeGreaterThan(0);
	});

	it("still fits if a future payload is small enough", () => {
		// The parameter is the point: a payload that shrinks under the budget
		// reports `fitsBudget` without a sentence in the UI being edited first.
		const fits = cjkAvailability(CJK_STORAGE_BUDGET_BYTES);
		expect(fits.fitsBudget).toBe(true);
		expect(fits.overBudgetBy).toBe(0);
	});

	it("agrees with the payload manifest FONT.10 produced", () => {
		// The one number the page shows, checked against the artefact it claims
		// to describe. When `xtask cjk-build` re-measures, this fails and the
		// constant, this test and the page's copy have to move together.
		const manifestPath = join(repoRoot, "assets", "cjk", "manifest.json");
		const manifest = JSON.parse(readFileSync(manifestPath, "utf8")) as {
			core: { raw_bytes: number };
			chunks: { raw_bytes: number }[];
		};
		const resident = manifest.chunks.reduce((sum, chunk) => sum + chunk.raw_bytes, 0);
		expect(resident + manifest.core.raw_bytes).toBe(CJK_PAYLOAD_RESIDENT_BYTES);
	});
});
