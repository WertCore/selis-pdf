/**
 * SL-4.WEB.01 DoD leg: "a test asserting the app still works with isolation
 * disabled". The engine's two raster paths are output-identical by contract
 * (ADR-P0004, SL-4.WASM.03); this suite proves the *selection* degrades to
 * the single-threaded path whenever COOP/COEP (or SAB) is missing, so the
 * extension and header-stripping embeds keep working.
 */

import { describe, expect, it } from "vitest";
import { canUseThreads, isCrossOriginIsolated, selectThreadMode } from "./isolation.js";

describe("WEB.01 isolation fallback", () => {
	it("reports isolated only on an explicit true", () => {
		expect(isCrossOriginIsolated({ crossOriginIsolated: true })).toBe(true);
		expect(isCrossOriginIsolated({ crossOriginIsolated: false })).toBe(false);
		expect(isCrossOriginIsolated({})).toBe(false);
		expect(isCrossOriginIsolated({ crossOriginIsolated: "true" })).toBe(false);
		expect(isCrossOriginIsolated({ crossOriginIsolated: 1 })).toBe(false);
	});

	it("requires both isolation and SharedArrayBuffer for threads", () => {
		expect(canUseThreads({ crossOriginIsolated: true, sharedArrayBufferAvailable: true })).toBe(
			true,
		);
		// The extension path: no COOP/COEP, SAB present but unusable.
		expect(canUseThreads({ crossOriginIsolated: false, sharedArrayBufferAvailable: true })).toBe(
			false,
		);
		// Enterprise policy: isolated but SAB disabled.
		expect(canUseThreads({ crossOriginIsolated: true, sharedArrayBufferAvailable: false })).toBe(
			false,
		);
		expect(canUseThreads({})).toBe(false);
	});

	it("still works with isolation disabled: selects the single-threaded engine", () => {
		// No headers (extension, file:// preview, header-stripping proxy).
		expect(selectThreadMode({})).toBe("single");
		expect(selectThreadMode({ crossOriginIsolated: false, sharedArrayBufferAvailable: true })).toBe(
			"single",
		);
		// Isolated without SAB still renders — just not threaded.
		expect(selectThreadMode({ crossOriginIsolated: true, sharedArrayBufferAvailable: false })).toBe(
			"single",
		);
	});

	it("selects threads only when the full isolation contract holds", () => {
		expect(selectThreadMode({ crossOriginIsolated: true, sharedArrayBufferAvailable: true })).toBe(
			"threads",
		);
	});
});
