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
	NavigationPort,
	LocalePort,
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
	PageTextLayer,
	TextLayerChar,
	TextLayerLine,
	SearchOptions,
	SearchMatch,
	SearchBatch,
	PageLabelStyle,
	PageLabelRange,
	DestinationKind,
	PdfDestination,
	OutlineNode,
	NamedDestination,
	LinkActionKind,
	LinkAction,
	LinkAnnotation,
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
	type MockLinkAnnotation,
	type MockRecording,
	type PageLinkSet,
} from "./mock-adapter.js";
// SL-4.UI.01. `contract.js` is deliberately NOT re-exported here.
//
// It is the shared conformance suite (24-BINDINGS-SPEC §1.7) and it imports
// `vitest` at the top level, so re-exporting it put a bare `vitest` specifier
// in the module graph of anything importing `@selis/ui` - including the web
// page, which has no bundler and no node_modules. That is a bare specifier the
// browser cannot resolve, and it only stayed hidden because nothing imported
// `@selis/ui` yet (SL-4.WEB.02's blocker 2: the viewer was never mounted).
//
// A consumer that wants the suite imports it directly:
//
//   import { definePlatformAdapterContract } from "@selis/ui/src/platform/contract.js";
//
// ...which is a TEST-only import - `apps/ui/src/platform/contract.test.ts` and
// the extension's `apps/extension/src/ext/transport.test.ts` both do exactly
// that today. Keeping it out of the public barrel is what keeps the shipped
// graph free of a test framework, and `tools/place-assets.mjs` fails the build
// loudly if a bare specifier ever reappears there.
// `contract.js` is deliberately NOT re-exported from this barrel - see the note
// above. It is test-only (it imports `vitest`), and nothing shipped imports it.
