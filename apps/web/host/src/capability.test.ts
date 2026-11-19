/**
 * SL-4.EXT.10 — portable capability detection.
 *
 * These tests exercise the typed seam in plain Node. They prove the result is
 * complete and boolean, that nothing is invoked while probing, and that every
 * capability has a fallback. They do NOT claim Firefox or Safari coverage:
 * those browsers cannot be run here (see the module doc).
 */

import { describe, expect, it } from "vitest";
import {
	CAPABILITY_FALLBACKS,
	CAPABILITY_NAMES,
	type BrowserCapabilities,
	type CapabilityScope,
	detectCapabilities,
	missingWithFallbacks,
} from "./capability.js";

function fullScope(): CapabilityScope {
	// Presence-only sentinels: if detection ever calls one of these, the test
	// fails loudly instead of silently "detecting" a working API.
	const sentinel = (name: string) => (): never => {
		throw new Error(`${name} must not be invoked during detection`);
	};
	return {
		navigator: {
			serviceWorker: { register: sentinel("serviceWorker.register") },
			storage: { getDirectory: sentinel("storage.getDirectory") },
			locks: { request: sentinel("locks.request") },
		},
		WebAssembly: { compile: sentinel("WebAssembly.compile") },
		FileSystemSyncAccessHandle: sentinel("FileSystemSyncAccessHandle"),
		showOpenFilePicker: sentinel("showOpenFilePicker"),
		OffscreenCanvas: sentinel("OffscreenCanvas"),
		indexedDB: {},
		URL: { createObjectURL: sentinel("URL.createObjectURL") },
		structuredClone: sentinel("structuredClone"),
		HTMLDialogElement: { prototype: { showModal: sentinel("showModal") } },
		requestAnimationFrame: sentinel("requestAnimationFrame"),
		TextEncoder: sentinel("TextEncoder"),
		ResizeObserver: sentinel("ResizeObserver"),
		IntersectionObserver: sentinel("IntersectionObserver"),
		SharedArrayBuffer: sentinel("SharedArrayBuffer"),
	};
}

describe("SL-4.EXT.10 capability detection", () => {
	it("returns a complete, boolean-typed result", () => {
		const caps = detectCapabilities(fullScope());
		expect(Object.keys(caps).sort()).toEqual([...CAPABILITY_NAMES].sort());
		for (const name of CAPABILITY_NAMES) {
			expect(typeof caps[name], name).toBe("boolean");
		}
	});

	it("detects every API when present without invoking any of them", () => {
		const caps = detectCapabilities(fullScope());
		for (const name of CAPABILITY_NAMES) {
			expect(caps[name], name).toBe(true);
		}
	});

	it("fails closed to all-false in a restricted embedder", () => {
		const caps = detectCapabilities({});
		for (const name of CAPABILITY_NAMES) {
			expect(caps[name], name).toBe(false);
		}
		expect(missingWithFallbacks(caps)).toHaveLength(CAPABILITY_NAMES.length);
	});

	it("marks a partial browser accurately, not all-or-nothing", () => {
		const caps = detectCapabilities({
			WebAssembly: { compile: () => undefined },
			OffscreenCanvas: () => undefined,
			navigator: { storage: { getDirectory: () => undefined } },
		});
		expect(caps.webAssembly).toBe(true);
		expect(caps.offscreenCanvas).toBe(true);
		expect(caps.opfs).toBe(true);
		expect(caps.serviceWorker).toBe(false);
		expect(caps.fileSystemAccess).toBe(false);
		expect(caps.opfsSyncHandle).toBe(false);
	});

	it("does not throw on a getter that throws", () => {
		const hostile: CapabilityScope = {};
		Object.defineProperty(hostile, "navigator", {
			get(): never {
				throw new Error("hostile getter");
			},
		});
		const caps = detectCapabilities(hostile);
		expect(caps.serviceWorker).toBe(false);
		expect(caps.opfs).toBe(false);
	});

	it("every capability has a documented portable fallback", () => {
		for (const name of CAPABILITY_NAMES) {
			expect(CAPABILITY_FALLBACKS[name], name).toBeTruthy();
		}
		expect(CAPABILITY_FALLBACKS.fileSystemAccess).toMatch(/input type=file|Blob/i);
		expect(CAPABILITY_FALLBACKS.offscreenCanvas).toMatch(/main thread/i);
		expect(CAPABILITY_FALLBACKS.sharedArrayBuffer).toMatch(/single-threaded/i);
	});

	it("missingWithFallbacks lists only the absent capabilities", () => {
		const caps = detectCapabilities({
			WebAssembly: { compile: () => undefined },
		}) as BrowserCapabilities;
		const missing = missingWithFallbacks(caps);
		expect(missing.some((entry) => entry.capability === "webAssembly")).toBe(false);
		expect(missing.find((entry) => entry.capability === "serviceWorker")?.fallback).toBe(
			CAPABILITY_FALLBACKS.serviceWorker,
		);
	});

	it("the real global scope is detected without throwing", () => {
		const caps = detectCapabilities();
		for (const name of CAPABILITY_NAMES) {
			expect(typeof caps[name], name).toBe("boolean");
		}
	});
});
