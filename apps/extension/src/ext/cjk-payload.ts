/**
 * SL-4.EXT.05 — the CJK font payload as an **optional post-install download**,
 * never a bundled asset.
 *
 * ## Why this file exists and what it deliberately does not do
 *
 * The plan's sentence is "CJK fonts are an optional post-install download into
 * extension storage, not a bundled asset". Half of that is a build rule —
 * `size-budget.ts` fails the build if a font extension appears in the package,
 * so the negative half is checked on every build — and this file is the
 * positive half: a store, in extension storage, that an installed viewer can
 * put a chunk into and take it out of again.
 *
 * **It has no producer yet, and that is reported rather than faked.**
 * `xtask cjk-build` (SL-3.FONT.10) is the thing that produces the payload and
 * its `cjk/manifest.json`; FONT.10 is still open, and its own docs say the
 * numbers are "to be re-set from the first run against real Noto". So this
 * module is written against the *shape* `cjk-build` already emits — the
 * `selis-cjk/1` manifest — and nothing here invents a font, a URL, or a
 * source. {@link CJK_PAYLOAD_SOURCE} is the seam where a producer is
 * attached, and it refuses with a typed code until one exists.
 *
 * ## Why the bytes are not fetched from a URL by this file
 *
 * Two reasons, and the second is the load-bearing one:
 *
 * 1. `host_permissions` is `[]` (EXT.01, and unchanged since). An extension
 *    with no host access cannot fetch `https://…` at all, so a download needs
 *    a permission this extension deliberately does not hold.
 * 2. Even with one, ADR-P0028 is about *code*, and a font payload is data —
 *    but a store reviewer reads "the extension downloads a file and writes it
 *    into its own storage" as remote content until a manifest pins it. ADR-
 *    P0043 §3 already settles the shape of the answer: the shell owns
 *    transport, and cache keys pair the chunk id with the manifest's SHA-256.
 *    That transport is WASM.07's row, not this task's.
 *
 * So the transport is an injected port with no shipped implementation, and the
 * refusal is typed rather than silent. `cjk-payload.test.ts` asserts that the
 * refusal is what a caller gets, which is the only honest thing to assert
 * while FONT.10 is open.
 */

/**
 * Chrome's documented `storage.local` quota, in bytes.
 *
 * `chrome.storage.local` is per-extension, needs only the `storage` permission
 * (not `unlimitedStorage`), and is capped at 10 MB by default. The cap is why
 * {@link CJK_STORAGE_BUDGET_BYTES} sits below it rather than at it, and why
 * exceeding the budget produces a typed code instead of a quota exception: a
 * user who has installed every CJK range should be told the payload does not
 * fit, not handed a `QUOTA_BYTES` error from deep inside a storage call.
 */
export const CHROME_STORAGE_LOCAL_QUOTA_BYTES = 10_485_760;

/**
 * The most CJK payload this extension keeps resident, across every chunk.
 *
 * Deliberately under {@link CHROME_STORAGE_LOCAL_QUOTA_BYTES} so the payload
 * can never be the reason a user's settings fail to save, and far under the
 * ~100 MB a full Noto CJK subsets to — which is the whole reason the payload
 * is per-range and optional rather than bundled (ADR-P0043).
 */
export const CJK_STORAGE_BUDGET_BYTES = 8 * 1024 * 1024;

/**
 * Per-file brotli budgets, mirroring `xtask cjk-build`'s defaults.
 *
 * The producer enforces these too (`--budget-core` / `--budget-chunk`). They
 * are repeated here so a *consumer* can refuse a payload the producer would
 * never have emitted, rather than trusting a number that arrived in the same
 * message as the bytes it describes.
 */
export const CJK_BROTLI_BUDGETS = {
	core: 1_200_000,
	chunk: 1_500_000,
} as const;

/** Key prefix every installed payload record is stored under. */
export const CJK_KEY_PREFIX = "selis.cjk.v1.";

/** The manifest schema this module reads, as `xtask cjk-build` writes it. */
export const CJK_MANIFEST_SCHEMA = "selis-cjk/1";

/** One payload file as the manifest describes it. */
export interface CjkPayloadFile {
	/** Chunk id; the core is `core`. */
	readonly id: string;
	/** Path the producer wrote, e.g. `cjk/hiragana.ttf`. Never fetched. */
	readonly file: string;
	readonly raw_bytes: number;
	readonly brotli_bytes: number;
	/** Lowercase hex SHA-256 of the raw bytes. */
	readonly sha256: string;
}

/** The part of `cjk/manifest.json` this module consumes. */
export interface CjkManifest {
	readonly schema: string;
	readonly core: CjkPayloadFile;
	readonly chunks: readonly CjkPayloadFile[];
}

/** Every way the CJK store can refuse. Registry-shaped, never a bare boolean. */
export type CjkErrorCode =
	/** No producer is attached: SL-3.FONT.10 has not shipped a payload. */
	| "cjk-no-producer"
	/** The manifest is not a `selis-cjk/1` record, or a field is unusable. */
	| "cjk-manifest-invalid"
	/** The bytes do not match the manifest's length or SHA-256. */
	| "cjk-integrity"
	/** The payload, or one file of it, is over its budget. */
	| "cjk-over-budget"
	/** The chunk is not installed. */
	| "cjk-not-installed";

/** A typed refusal, carrying a code so a caller can branch without parsing prose. */
export class CjkPayloadError extends Error {
	constructor(
		readonly code: CjkErrorCode,
		message: string,
	) {
		super(message);
		this.name = "CjkPayloadError";
	}
}

/**
 * Extension storage, as this module needs it.
 *
 * `Uint8Array` rather than strings: a font chunk is binary, and base64ing it
 * through a string port would cost a third of the budget for nothing. The
 * `chrome.storage.local` binding is a thin adapter over this interface;
 * `localStorage` — what `host-env.ts` uses for settings — holds strings and
 * caps out around 5 MB, so it is not used here.
 */
export interface CjkStorage {
	get(key: string): Promise<Uint8Array | null>;
	set(key: string, value: Uint8Array): Promise<void>;
	remove(key: string): Promise<void>;
	/** Every key this store holds, in no particular order. */
	keys(): Promise<readonly string[]>;
}

/** Where an installed chunk's bytes live. Versioned, so a schema change is visible. */
export function cjkStorageKey(id: string): string {
	return `${CJK_KEY_PREFIX}${id}`;
}

/**
 * Validate a parsed manifest, or refuse it.
 *
 * A schema check and a field check, and nothing clever. The failure being
 * guarded against is a *different* build writing a different record, which a
 * structural check catches and a clever one would only mis-handle.
 */
export function parseCjkManifest(value: unknown): CjkManifest {
	const bad = (why: string): never => {
		throw new CjkPayloadError("cjk-manifest-invalid", `CJK manifest rejected: ${why}`);
	};
	if (typeof value !== "object" || value === null) {
		return bad("not an object");
	}
	const record = value as Record<string, unknown>;
	if (record.schema !== CJK_MANIFEST_SCHEMA) {
		return bad(`schema is '${String(record.schema)}', expected '${CJK_MANIFEST_SCHEMA}'`);
	}
	if (!Array.isArray(record.chunks)) {
		return bad("`chunks` is not an array");
	}
	const file = (entry: unknown, id: string): CjkPayloadFile => {
		if (typeof entry !== "object" || entry === null) {
			return bad(`'${id}' is not an object`);
		}
		const row = entry as Record<string, unknown>;
		for (const key of ["file", "raw_bytes", "brotli_bytes", "sha256"]) {
			if (row[key] === undefined || row[key] === null) {
				return bad(`'${id}' has no ${key}`);
			}
		}
		if (typeof row.raw_bytes !== "number" || typeof row.brotli_bytes !== "number") {
			return bad(`'${id}' has non-numeric sizes`);
		}
		if (typeof row.sha256 !== "string" || !/^[0-9a-f]{64}$/.test(row.sha256)) {
			return bad(`'${id}' has no lowercase hex sha256`);
		}
		return {
			id,
			file: String(row.file),
			raw_bytes: row.raw_bytes,
			brotli_bytes: row.brotli_bytes,
			sha256: row.sha256,
		};
	};
	const core = file(record.core, "core");
	const chunks = (record.chunks as unknown[]).map((chunk, index) =>
		file(chunk, String((chunk as { id?: unknown } | null)?.id ?? `chunk-${index}`)),
	);
	return { schema: CJK_MANIFEST_SCHEMA, core, chunks };
}

/**
 * Where a payload comes from, as a port.
 *
 * Two calls, both async, neither of which has a shipped implementation. The
 * transport behind them is WASM.07's row (ADR-P0043 §3: "the shell owns
 * transport, HTTP/Cache-API caching, and quota"); this task's row is the
 * decision that the payload is *not* in the package, and that the store which
 * holds it is.
 */
export interface CjkSource {
	/** The `selis-cjk/1` manifest, as parsed JSON. */
	manifest(): Promise<unknown>;
	/** The bytes of one payload file, addressed by its manifest `file` name. */
	bytes(file: CjkPayloadFile): Promise<Uint8Array>;
}

/**
 * The source this package ships: none.
 *
 * A refusal rather than a placeholder URL. A placeholder would be worse than
 * nothing for two reasons — it would put a remote origin in the source of an
 * ADR-P0028 package, which is the one thing the bundled-only gate exists to
 * prevent, and it would claim a payload exists when SL-3.FONT.10 has not
 * produced one. Callers get a typed refusal they can surface, and a test
 * asserts they get it.
 */
export const CJK_PAYLOAD_SOURCE: CjkSource = {
	manifest: () =>
		Promise.reject(
			new CjkPayloadError(
				"cjk-no-producer",
				"no CJK payload source is configured: SL-3.FONT.10 has not shipped a cjk/manifest.json, and this extension holds no host permission to fetch one",
			),
		),
	bytes: () =>
		Promise.reject(
			new CjkPayloadError("cjk-no-producer", "no CJK payload source is configured (SL-3.FONT.10)"),
		),
};

/** Lowercase hex SHA-256 of `bytes`, via the platform digest. */
async function sha256Hex(bytes: Uint8Array): Promise<string> {
	const subtle = globalThis.crypto?.subtle;
	if (subtle === undefined) {
		throw new CjkPayloadError(
			"cjk-integrity",
			"no WebCrypto digest available: the CJK payload cannot be verified, so it is not installed",
		);
	}
	const digest = await subtle.digest("SHA-256", bytes as unknown as ArrayBuffer);
	return [...new Uint8Array(digest)].map((b) => b.toString(16).padStart(2, "0")).join("");
}

/** The manifest row for `id`, where `core` names the core payload. */
function manifestFileFor(manifest: CjkManifest, id: string): CjkPayloadFile | null {
	return id === "core" ? manifest.core : (manifest.chunks.find((chunk) => chunk.id === id) ?? null);
}

/** What one install did, for a caller that wants to report it. */
export interface CjkInstallResult {
	readonly id: string;
	readonly rawBytes: number;
	readonly brotliBytes: number;
	/** Everything resident after this install, as declared by the manifest. */
	readonly residentBytes: number;
}

/**
 * Put one payload chunk into extension storage.
 *
 * The order of the checks is the point: budget before bytes, bytes before
 * integrity, and the write last. A payload over budget costs a manifest lookup
 * and nothing else; a payload whose bytes do not match never reaches storage
 * at all, so a corrupt or substituted chunk cannot become resident by being
 * written first and checked afterwards.
 */
export async function installCjkChunk(options: {
	storage: CjkStorage;
	source: CjkSource;
	id: string;
}): Promise<CjkInstallResult> {
	const { storage, source, id } = options;
	const manifest = parseCjkManifest(await source.manifest());
	const file = manifestFileFor(manifest, id);
	if (file === null) {
		throw new CjkPayloadError(
			"cjk-not-installed",
			`the CJK manifest has no chunk '${id}' to install`,
		);
	}
	const budget = id === "core" ? CJK_BROTLI_BUDGETS.core : CJK_BROTLI_BUDGETS.chunk;
	if (file.brotli_bytes > budget) {
		throw new CjkPayloadError(
			"cjk-over-budget",
			`CJK ${id} declares ${file.brotli_bytes} brotli bytes; the budget is ${budget} (ADR-P0043)`,
		);
	}

	// Declared sizes, not measured ones: every resident chunk was verified
	// against its manifest row when it was installed, so the manifest is the
	// authority on what is resident and this costs no reads.
	const installed = await installedCjkIds(storage);
	const resident = [manifest.core, ...manifest.chunks]
		.filter((candidate) => candidate.id === id || installed.includes(candidate.id))
		.reduce((sum, candidate) => sum + candidate.raw_bytes, 0);
	if (resident > CJK_STORAGE_BUDGET_BYTES) {
		throw new CjkPayloadError(
			"cjk-over-budget",
			`installing CJK ${id} would leave ${resident} resident bytes, over the ${CJK_STORAGE_BUDGET_BYTES} this extension keeps in extension storage`,
		);
	}

	const bytes = await source.bytes(file);
	if (bytes.length !== file.raw_bytes) {
		throw new CjkPayloadError(
			"cjk-integrity",
			`CJK ${id} is ${bytes.length} bytes; its manifest declares ${file.raw_bytes}`,
		);
	}
	const digest = await sha256Hex(bytes);
	if (digest !== file.sha256) {
		throw new CjkPayloadError(
			"cjk-integrity",
			`CJK ${id} hashes to ${digest.slice(0, 16)}…; its manifest pins ${file.sha256.slice(0, 16)}…`,
		);
	}

	await storage.set(cjkStorageKey(id), bytes);
	return { id, rawBytes: file.raw_bytes, brotliBytes: file.brotli_bytes, residentBytes: resident };
}

/** The chunk ids currently resident in extension storage, sorted. */
export async function installedCjkIds(storage: CjkStorage): Promise<string[]> {
	const keys = await storage.keys();
	return keys
		.filter((key) => key.startsWith(CJK_KEY_PREFIX))
		.map((key) => key.slice(CJK_KEY_PREFIX.length))
		.sort();
}

/** Read one installed chunk's bytes, or refuse with the code that says why. */
export async function readCjkChunk(storage: CjkStorage, id: string): Promise<Uint8Array> {
	const bytes = await storage.get(cjkStorageKey(id));
	if (bytes === null) {
		throw new CjkPayloadError(
			"cjk-not-installed",
			`CJK chunk '${id}' is not installed; CJK text renders .notdef until it is`,
		);
	}
	return bytes;
}

/** Remove one installed chunk. Removing what is not there is the same refusal. */
export async function removeCjkChunk(storage: CjkStorage, id: string): Promise<void> {
	if ((await storage.get(cjkStorageKey(id))) === null) {
		throw new CjkPayloadError("cjk-not-installed", `CJK chunk '${id}' is not installed`);
	}
	await storage.remove(cjkStorageKey(id));
}

/**
 * `chrome.storage.local` as a {@link CjkStorage}.
 *
 * The binding exists so the store is not a description of a store. It is a
 * function rather than a module-level constant because a module that reads
 * `chrome` at import time cannot be imported by the Node test, and
 * `bundle.test.ts` imports the gate's own modules from a plain Node process.
 *
 * `storage.local` is chosen over the `localStorage` that `host-env.ts` uses
 * for settings for two reasons: it stores bytes rather than strings, and its
 * quota is ten times larger. Neither store is shared with the web app and
 * neither pretends to be.
 */
export function createChromeStorageLocal(): CjkStorage {
	const area = (
		globalThis as {
			chrome?: { storage?: { local?: ChromeStorageArea } };
		}
	).chrome?.storage?.local;
	const unavailable = (): never => {
		throw new CjkPayloadError(
			"cjk-no-producer",
			"chrome.storage.local is unavailable in this context, so no CJK payload can be installed",
		);
	};
	if (area === undefined) {
		return { get: unavailable, set: unavailable, remove: unavailable, keys: unavailable };
	}
	return {
		async get(key) {
			const record = await area.get(key);
			const value = record[key];
			return value === undefined ? null : new Uint8Array(value as number[]);
		},
		async set(key, value) {
			// `chrome.storage.local` accepts an Array of numbers, not an
			// ArrayBuffer, and structured-cloning a typed array into it stores
			// an object rather than bytes.
			await area.set({ [key]: [...value] });
		},
		async remove(key) {
			await area.remove(key);
		},
		async keys() {
			// `chrome.storage.local.keys()` already resolves to the key list;
			// wrapping it in `Object.keys` would return array indices, which is
			// the kind of bug that only a double catches.
			return await area.keys();
		},
	};
}

/** The `chrome.storage.StorageArea` calls this module makes. */
interface ChromeStorageArea {
	get(key: string): Promise<Record<string, unknown>>;
	set(items: Record<string, number[]>): Promise<void>;
	remove(key: string): Promise<void>;
	keys(): Promise<string[]>;
}
