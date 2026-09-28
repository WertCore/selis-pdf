/**
 * `@selis/web-host` — the web deployment shell (SL-4.WEB.01, WEB.02, WEB.03).
 *
 * Static hosting with cross-origin isolation, an offline service worker whose
 * caching policy can never persist a document, and local-only document handoff
 * over the WASM.01 Worker protocol: drag-drop, file picker, paste, and `?src=`
 * URL opening, none of which uploads a document. See `README.md` for the
 * guarantees and the CI gates.
 */

export {
	MAX_HANDOFF_BYTES,
	MAX_HANDOFF_FILES,
	filesFromDrop,
	filesFromPaste,
	filesFromPicker,
	isPdfMime,
	isPdfName,
	isSameOrigin,
	parseSrcParam,
	sourceForFile,
} from "./handoff.js";
export type {
	HandoffClipboard,
	HandoffDataTransfer,
	HandoffFile,
	HandoffResult,
	HandoffSource,
	SrcOpening,
} from "./handoff.js";
export {
	collectingSink,
	planIntake,
	runIntake,
	DEFAULT_RANGE_CHUNK,
} from "./handoff-flow.js";
export type {
	CollectingSink,
	Intake,
	IntakeDeps,
	IntakeOptions,
	IntakePath,
	OpenedDocument,
	PlanResult,
	PlannedOpen,
	RangeSink,
	RunResult,
} from "./handoff-flow.js";
export {
	assertNoUpload,
	containsBytes,
	createFetchRecorder,
	fingerprintsFor,
	inspectBody,
	installRequestRecorder,
} from "./no-upload.js";
export type {
	DocumentBytes,
	InspectedBody,
	RecordedRequest,
	RequestBody,
	RequestRecorder,
} from "./no-upload.js";
export {
	buildOpenRequest,
	deliveryFor,
	parseOpenResponse,
	toWireDescriptor,
} from "./worker-glue.js";
export type { BudgetSurface, Delivery, GlueSource, WireSourceDescriptor } from "./worker-glue.js";
export {
	CACHE_VERSION,
	ISOLATION_HEADER_NAMES,
	MAX_RUNTIME_ENTRY_BYTES,
	OWNED_CACHES,
	PRECACHE_PATHS,
	RUNTIME_CACHE,
	RUNTIME_CACHE_MAX_ENTRIES,
	RUNTIME_PATH_PREFIXES,
	RUNTIME_STORABLE_CONTENT_TYPES,
	SERVICE_WORKER_PATH,
	SHELL_ALIASES,
	SHELL_CACHE,
	SKIP_WAITING_MESSAGE,
	SW_SCOPE,
	chooseEvictionKey,
	classifyRequest,
	isStaleCacheName,
	mayStoreResponse,
	shellLookupPaths,
} from "./sw-policy.js";
export type {
	Decision,
	RequestFacts,
	RespondDecision,
	ResponseFacts,
	StoreDecision,
} from "./sw-policy.js";
export {
	createWorkerHandlers,
	isServiceWorkerScope,
	isSkipWaitingMessage,
	offlineFallback,
	requestFactsFrom,
	responseFactsFrom,
} from "./sw.js";
export type { WorkerDeps, WorkerHandlers } from "./sw.js";
export {
	applyUpdate,
	liveRegisterEnvironment,
	registerServiceWorker,
	registrationBlocker,
	registrationOptions,
	watchForUpdate,
} from "./sw-register.js";
export type {
	RegisterEnvironment,
	RegisterResult,
	Unavailable,
	UpdateReport,
} from "./sw-register.js";
