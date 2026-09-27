/**
 * `@selis/web-host` — the web deployment shell (SL-4.WEB.03).
 *
 * Local-only document handoff over the WASM.01 Worker protocol:
 * drag-drop, file picker, paste, and `?src=` URL opening, none of which
 * uploads a document. See `README.md` for the guarantee and the CI gate.
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
