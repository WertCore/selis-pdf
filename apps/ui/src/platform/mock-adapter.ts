/**
 * The in-process mock adapter (SL-4.UI.01 DoD): a complete `PlatformAdapter`
 * for tests and story-level development, with no transport, no browser, and
 * no clock. Deterministic by construction — no timers, no randomness; every
 * "async" step is a microtask yield, so cancellation is observable at
 * deterministic await points and tests never sleep.
 *
 * Scope: the mock validates the same contract a real transport must (handle
 * lifetimes, page bounds, cancellation, budget ceilings, telemetry opt-in)
 * and records every host-side interaction for assertions. It does not parse
 * PDFs — documents are described declaratively via {@link MockDocumentSpec}.
 */

import type {
	ClipboardPort,
	FilePort,
	PlatformAdapter,
	PrintPort,
	StoragePort,
	TelemetryPort,
	WindowPort,
} from "./adapter.js";
import { AdapterError, ErrorCode } from "./errors.js";
import type {
	AdapterRequestOptions,
	DeepLink,
	DocHandle,
	DocumentSourceDescriptor,
	PageText,
	PlatformCapabilities,
	PrintOptions,
	ProgressReport,
	RenderedTile,
	SaveTarget,
	SearchBatch,
	SearchMatch,
	SearchOptions,
	Size,
	TelemetryEvent,
} from "./types.js";

/** Declarative description of a mock document. */
export interface MockDocumentSpec {
	/** Document name (used for handles and save defaults). */
	readonly name?: string;
	readonly pageCount: number;
	/** Page size in points; default US Letter 612×792. */
	readonly pageSize?: Size;
	/** Text per page (search/extraction source); missing pages are empty. */
	readonly textPages?: readonly string[];
	/**
	 * Real bytes to validate on open. When provided, open() requires a
	 * `%PDF-` header (registry 1000 NOT_A_PDF otherwise). When omitted, a
	 * minimal synthetic header is fabricated.
	 */
	readonly bytes?: Uint8Array;
	/** RGBA fill for rendered tiles; default slate blue. */
	readonly tileColour?: readonly [number, number, number];
}

/** Observable state of the mock for assertions. */
export interface MockRecording {
	readonly telemetryEvents: TelemetryEvent[];
	readonly clipboardWrites: string[];
	readonly printedDocs: DocHandle[];
	readonly printOptions: PrintOptions[];
	readonly titles: string[];
	readonly externalUrls: string[];
	readonly deepLinks: DeepLink[];
	readonly savedFiles: Map<string, Uint8Array>;
}

interface RegisteredDocument {
	readonly spec: MockDocumentSpec;
	readonly bytes: Uint8Array;
}

const DEFAULT_PAGE: Size = { width: 612, height: 792 };
const DEFAULT_TILE_COLOUR: readonly [number, number, number] = [47, 82, 138];
const PDF_HEADER = [0x25, 0x50, 0x44, 0x46, 0x2d];
/** Stub ceiling that mirrors the engine's pixel-budget behaviour (ADR-P0006). */
const MAX_TILE_PIXELS = 4096 * 4096;

function microtask(): Promise<void> {
	return Promise.resolve();
}

function assertNotAborted(options?: AdapterRequestOptions): void {
	if (options?.signal?.aborted) {
		throw AdapterError.cancelled();
	}
}

function report(options: AdapterRequestOptions | undefined, fraction: number, stage: string): void {
	const progress: ProgressReport = { fraction, stage };
	options?.onProgress?.(progress);
}

function isWordBoundary(haystack: string, start: number, end: number): boolean {
	const before = start === 0 ? " " : haystack.charAt(start - 1);
	const after = end >= haystack.length ? " " : haystack.charAt(end);
	return !isWordChar(before) && !isWordChar(after);
}

function isWordChar(ch: string): boolean {
	return /[a-zA-Z0-9_]/.test(ch);
}

function syntheticPdf(name: string): Uint8Array {
	const header = `%PDF-1.4\n%${name}\n`;
	const bytes = new Uint8Array(header.length);
	for (let i = 0; i < header.length; i += 1) {
		bytes[i] = header.charCodeAt(i) & 0xff;
	}
	return bytes;
}

/**
 * Create the mock adapter. Documents registered via
 * {@link MockAdapter.addDocument} are opened from their descriptor;
 * everything is in-memory and session-scoped.
 */
export function createMockAdapter(
	options: { capabilities?: Partial<PlatformCapabilities> } = {},
): MockAdapter {
	const capabilities: PlatformCapabilities = {
		platform: "mock",
		filePickers: true,
		fileSystemAccess: true,
		opfs: true,
		httpRange: true,
		clipboardRead: true,
		print: true,
		persistedStorage: true,
		threads: true,
		deepLinks: true,
		...options.capabilities,
	};

	const registry = new Map<DocumentSourceDescriptor, RegisteredDocument>();
	const documents = new Map<string, RegisteredDocument>();
	const openDocs = new Set<string>();
	const openQueue: DocumentSourceDescriptor[] = [];
	const saveQueue: string[] = [];
	const storage = new Map<string, string>();
	const deepLinkHandlers = new Set<(link: DeepLink) => void>();
	let clipboard = "";
	let telemetryEnabled = false;
	let documentCounter = 0;

	const recording: MockRecording = {
		telemetryEvents: [],
		clipboardWrites: [],
		printedDocs: [],
		printOptions: [],
		titles: [],
		externalUrls: [],
		deepLinks: [],
		savedFiles: new Map(),
	};

	function lookupDocument(id: string): RegisteredDocument {
		const doc = documents.get(id);
		if (doc === undefined || !openDocs.has(id)) {
			throw AdapterError.badHandle(`unknown or closed document handle "${id}"`);
		}
		return doc;
	}

	function assertPage(spec: MockDocumentSpec, page: number): void {
		if (!Number.isInteger(page) || page < 0 || page >= spec.pageCount) {
			throw AdapterError.badArgument(`page ${page} out of range (0..${spec.pageCount - 1})`);
		}
	}

	const adapter: MockAdapter = {
		capabilities,

		engine: {
			async open(source, requestOptions) {
				assertNotAborted(requestOptions);
				await microtask();
				assertNotAborted(requestOptions);
				const registered = registry.get(source);
				if (registered === undefined) {
					throw AdapterError.sourceUnreadable(
						"descriptor was not registered with the mock adapter",
					);
				}
				const headerOk = registered.bytes
					.subarray(0, 5)
					.every((byte, index) => byte === PDF_HEADER[index]);
				if (registered.bytes.length >= 5 && !headerOk) {
					throw new AdapterError({
						code: 1000,
						message: `registered bytes for "${registered.spec.name ?? "document"}" are not a PDF`,
						docState: "NotLoaded",
						retryable: false,
					});
				}
				documentCounter += 1;
				const id = `mock-doc-${documentCounter}`;
				documents.set(id, registered);
				openDocs.add(id);
				report(requestOptions, 1, "opened");
				return {
					id,
					pageCount: registered.spec.pageCount,
					pageSizes: Array.from(
						{ length: registered.spec.pageCount },
						() => registered.spec.pageSize ?? DEFAULT_PAGE,
					),
				};
			},

			async close(doc) {
				await microtask();
				if (!openDocs.has(doc.id)) {
					throw AdapterError.badHandle(`cannot close unknown document "${doc.id}"`);
				}
				openDocs.delete(doc.id);
			},

			async renderTile(request, requestOptions) {
				assertNotAborted(requestOptions);
				const doc = lookupDocument(request.doc.id);
				assertPage(doc.spec, request.page);
				if (!Number.isFinite(request.scale) || request.scale <= 0) {
					throw AdapterError.badArgument(`scale must be a positive number, got ${request.scale}`);
				}
				await microtask();
				assertNotAborted(requestOptions);
				const base = doc.spec.pageSize ?? DEFAULT_PAGE;
				const rect = request.rect ?? { x: 0, y: 0, width: base.width, height: base.height };
				const width = Math.max(1, Math.round(rect.width * request.scale));
				const height = Math.max(1, Math.round(rect.height * request.scale));
				if (width * height > MAX_TILE_PIXELS) {
					throw new AdapterError({
						code: ErrorCode.BudgetPixels,
						message: `tile ${width}x${height} exceeds the pixel budget`,
						docState: "Loaded",
						retryable: true,
					});
				}
				report(requestOptions, 0.5, "render");
				const [r, g, b] = doc.spec.tileColour ?? DEFAULT_TILE_COLOUR;
				const data = new ArrayBuffer(width * height * 4);
				const pixels = new Uint8ClampedArray(data);
				for (let i = 0; i < pixels.length; i += 4) {
					pixels[i] = r;
					pixels[i + 1] = g;
					pixels[i + 2] = b;
					pixels[i + 3] = 255;
				}
				await microtask();
				assertNotAborted(requestOptions);
				report(requestOptions, 1, "render");
				const tile: RenderedTile = { page: request.page, width, height, format: "rgba8", data };
				return tile;
			},

			async extractText(doc, page, requestOptions) {
				assertNotAborted(requestOptions);
				const mock = lookupDocument(doc.id);
				assertPage(mock.spec, page);
				await microtask();
				const text = mock.spec.textPages?.[page] ?? "";
				const result: PageText = { page, text };
				return result;
			},

			async *search(doc, query, searchOptions, requestOptions) {
				const mock = lookupDocument(doc.id);
				if (query.length === 0) {
					throw AdapterError.badArgument("search query must not be empty");
				}
				const resolvedOptions: SearchOptions = searchOptions ?? {};
				const first = resolvedOptions.fromPage ?? 0;
				if (!Number.isInteger(first) || first < 0 || first >= mock.spec.pageCount) {
					throw AdapterError.badArgument(`fromPage ${first} out of range`);
				}
				const needle = resolvedOptions.caseSensitive ? query : query.toLowerCase();
				for (let page = first; page < mock.spec.pageCount; page += 1) {
					await microtask();
					assertNotAborted(requestOptions);
					const haystack = mock.spec.textPages?.[page] ?? "";
					const searchable = resolvedOptions.caseSensitive ? haystack : haystack.toLowerCase();
					const matches: SearchMatch[] = [];
					let cursor = searchable.indexOf(needle);
					while (cursor !== -1) {
						const end = cursor + needle.length;
						if (!resolvedOptions.wholeWord || isWordBoundary(searchable, cursor, end)) {
							matches.push({ page, start: cursor, end });
						}
						cursor = searchable.indexOf(needle, end);
					}
					const scanned = page - first + 1;
					const total = mock.spec.pageCount - first;
					const progress = scanned / total;
					report(requestOptions, progress, "search");
					const batch: SearchBatch = { matches, progress, done: page === mock.spec.pageCount - 1 };
					yield batch;
				}
			},
		},

		files: {
			async pickOpen() {
				await microtask();
				if (!capabilities.filePickers) {
					throw AdapterError.badArgument("this host does not expose file pickers");
				}
				return openQueue.splice(0);
			},
			async pickSave(suggestedName) {
				await microtask();
				if (!capabilities.filePickers) {
					throw AdapterError.badArgument("this host does not expose file pickers");
				}
				void suggestedName;
				const name = saveQueue.shift();
				if (name === undefined) {
					return null;
				}
				const target: SaveTarget = {
					name,
					async write(bytes) {
						recording.savedFiles.set(name, Uint8Array.from(bytes));
					},
				};
				return target;
			},
		} satisfies FilePort,

		storage: {
			async get(key) {
				await microtask();
				const value = storage.get(key);
				return value === undefined ? null : value;
			},
			async set(key, value) {
				await microtask();
				storage.set(key, value);
			},
			async delete(key) {
				await microtask();
				storage.delete(key);
			},
		} satisfies StoragePort,

		clipboard: {
			async writeText(text) {
				await microtask();
				clipboard = text;
				recording.clipboardWrites.push(text);
			},
			async readText() {
				await microtask();
				if (!capabilities.clipboardRead) {
					throw AdapterError.badArgument("clipboard read is not available on this host");
				}
				return clipboard;
			},
		} satisfies ClipboardPort,

		print: {
			async print(doc, printOptions) {
				await microtask();
				if (!capabilities.print) {
					throw AdapterError.badArgument("this host has no print path");
				}
				lookupDocument(doc.id);
				recording.printedDocs.push(doc);
				recording.printOptions.push(printOptions ?? {});
			},
		} satisfies PrintPort,

		telemetry: {
			isEnabled: () => telemetryEnabled,
			async setEnabled(enabled) {
				await microtask();
				telemetryEnabled = enabled;
			},
			record(event) {
				if (!telemetryEnabled) {
					return;
				}
				recording.telemetryEvents.push(event);
			},
		} satisfies TelemetryPort,

		window: {
			async setTitle(title) {
				await microtask();
				recording.titles.push(title);
			},
			async openExternal(url) {
				await microtask();
				recording.externalUrls.push(url);
			},
			onDeepLink(handler) {
				deepLinkHandlers.add(handler);
				return () => {
					deepLinkHandlers.delete(handler);
				};
			},
		} satisfies WindowPort,

		async dispose() {
			await microtask();
			openDocs.clear();
		},

		addDocument(spec) {
			const bytes = spec.bytes ?? syntheticPdf(spec.name ?? `mock-${documentCounter + 1}`);
			const source: DocumentSourceDescriptor = {
				kind: "bytes",
				bytes: bytes.slice().buffer,
				name: spec.name ?? "mock.pdf",
			};
			registry.set(source, { spec, bytes });
			return source;
		},

		queueOpen(source) {
			openQueue.push(source);
		},

		queueSave(name) {
			saveQueue.push(name);
		},

		emitDeepLink(link) {
			recording.deepLinks.push(link);
			for (const handler of deepLinkHandlers) {
				handler(link);
			}
		},

		recording,
	};

	return adapter;
}

/** The mock adapter plus its test-only observation surface. */
export interface MockAdapter extends PlatformAdapter {
	/** Register a document and get the descriptor that opens it. */
	addDocument(spec: MockDocumentSpec): DocumentSourceDescriptor;
	/** Queue a descriptor for the next `files.pickOpen()`. */
	queueOpen(source: DocumentSourceDescriptor): void;
	/** Queue a save name so the next `files.pickSave()` resolves to a target. */
	queueSave(name: string): void;
	/** Simulate the host routing a deep link into the app. */
	emitDeepLink(link: DeepLink): void;
	/** All recorded host interactions, for assertions. */
	readonly recording: MockRecording;
}
