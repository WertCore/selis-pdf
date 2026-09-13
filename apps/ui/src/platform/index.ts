export {
	ErrorCode,
	AdapterError,
	type DocState,
	type ErrorCodeValue,
	isAdapterError,
} from "./errors.js";
export type {
	PlatformAdapter,
	EnginePort,
	FilePort,
	PickOpenOptions,
	StoragePort,
	ClipboardPort,
	PrintPort,
	TelemetryPort,
	WindowPort,
} from "./adapter.js";
export type {
	Size,
	Rect,
	DocumentSourceDescriptor,
	FsaFileHandle,
	BudgetProfile,
	ProgressReport,
	AdapterRequestOptions,
	DocHandle,
	RenderTileRequest,
	RenderedTile,
	PageText,
	SearchOptions,
	SearchMatch,
	SearchBatch,
	SaveTarget,
	PrintOptions,
	TelemetryEvent,
	DeepLink,
	PlatformCapabilities,
} from "./types.js";
export {
	createMockAdapter,
	type MockAdapter,
	type MockDocumentSpec,
	type MockRecording,
} from "./mock-adapter.js";
export { definePlatformAdapterContract, type AdapterFixture, SAMPLE_TEXTS } from "./contract.js";
