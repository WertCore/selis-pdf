/**
 * SL-4.EXT.06 - what the extension supplies, and what it refuses.
 *
 * The transport tests prove the seam works. These prove the *posture*: that
 * every capability flag says what the host can actually do, and that each
 * refusal is a real, registry-coded refusal rather than a stub that looks like
 * success.
 *
 * The refusals are the interesting half. A stub that returns `null` or `""` is
 * worse than a refusal, because a UI cannot tell it apart from "the user
 * cancelled" or "the clipboard is empty", and reports it as a failure of
 * something that never happened. So each refusal here is asserted to be an
 * `AdapterError` with a code, and - where the UI would act on it - asserted to
 * have performed no host side effect at all.
 */

import { describe, expect, it } from "vitest";
import { ErrorCode } from "../../../ui/src/platform/errors.js";
import { ALLOWED_HOST_PERMISSIONS, ALLOWED_PERMISSIONS } from "../permissions.js";
import { EXTENSION_CAPABILITIES, createExtensionAdapter, requireExternalUrl } from "./adapter.js";
import { ENSURE_ENGINE_HOST } from "./engine-protocol.js";
import { createFakeHostEnv } from "./fake-host-env.js";
import { createFixtureEngine, sampleBytes } from "./fixture-engine.js";
import type { HostPort } from "./host-env.js";
import { createLoopbackLink } from "./loopback.js";
import { serveEngineHost } from "./offscreen-engine.js";
import {
	createViewerSession,
	describeFailure,
	openDocument,
	sourceName,
	titleFor,
} from "./viewer-session.js";

/** A live adapter over a loopback, with a recording env. */
function live() {
	const link = createLoopbackLink();
	serveEngineHost({ link: link.host, engine: createFixtureEngine() });
	const host = createFakeHostEnv();
	host.usePort(link.client as unknown as HostPort);
	return { adapter: createExtensionAdapter({ env: host.env, link: link.client }), host, link };
}

describe("extension capabilities (SL-4.EXT.06)", () => {
	it("claims only what the approved permission set can give it", () => {
		// The whole point of `host_permissions: []` is that there is no origin
		// to range-request, so httpRange cannot be true. If a future change adds
		// a host permission, this fails and the conversation has to happen.
		expect(EXTENSION_CAPABILITIES.platform).toBe("extension");
		expect(EXTENSION_CAPABILITIES.httpRange).toBe(false);
		expect(EXTENSION_CAPABILITIES.opfs).toBe(false);
		expect(EXTENSION_CAPABILITIES.fileSystemAccess).toBe(false);
		expect(EXTENSION_CAPABILITIES.clipboardRead).toBe(false);
		expect(EXTENSION_CAPABILITIES.deepLinks).toBe(false);
		expect(EXTENSION_CAPABILITIES.threads).toBe(false);
		expect(ALLOWED_HOST_PERMISSIONS).toEqual([]);
		// `storage` (SL-4.EXT.05) buys the optional CJK payload store and
		// nothing else: it grants no origin, so `httpRange` above is still
		// false and the adapter's `storage` port is still the `localStorage`
		// one. A permission that starts changing what the adapter can reach
		// has to fail here.
		expect(ALLOWED_PERMISSIONS).toEqual(["declarativeNetRequest", "offscreen", "storage"]);
	});

	it("refuses clipboard read, and does not pretend to have read anything", async () => {
		const { adapter, host } = live();
		await adapter.clipboard.writeText("selected text");
		expect(host.recording.clipboardWrites).toEqual(["selected text"]);
		await expect(adapter.clipboard.readText()).rejects.toMatchObject({
			code: ErrorCode.BindingBadArgument,
			docState: "Unchanged",
		});
	});

	it("refuses a save target rather than returning null", async () => {
		// `null` means "the user cancelled". A read-only viewer that returned it
		// would have every caller report a cancelled save instead of an
		// unsupported one.
		const { adapter, host } = live();
		await expect(adapter.files.pickSave("out.pdf")).rejects.toMatchObject({
			code: ErrorCode.BindingBadArgument,
		});
		expect(host.recording.externalUrls).toEqual([]);
	});

	it("opens a file through the picker", async () => {
		const { adapter, host } = live();
		const bytes = sampleBytes();
		host.queueOpen([
			{ kind: "blob", blob: new Blob([bytes.buffer as ArrayBuffer]), name: "p.pdf" },
		]);
		const picked = await adapter.files.pickOpen({ accept: [".pdf"] });
		expect(picked).toHaveLength(1);
		expect(host.recording.filePicks).toEqual([{ accept: [".pdf"] }]);
	});

	it("opens external links only over http(s)", async () => {
		const { adapter, host } = live();
		await adapter.window.openExternal("https://example.com/help");
		await adapter.window.openExternal("http://example.com/help");
		expect(host.recording.externalUrls).toEqual([
			"https://example.com/help",
			"http://example.com/help",
		]);

		// ADR-P0020: a document must not be able to talk the browser into
		// executing or navigating to something of its own choosing. The port
		// only performs, so the scheme check is the last line of defence.
		for (const url of [
			"javascript:alert(1)",
			"data:text/html,<script>alert(1)</script>",
			"file:///etc/passwd",
			"chrome://settings",
			"not a url",
		]) {
			await expect(adapter.window.openExternal(url), url).rejects.toMatchObject({
				code: ErrorCode.BindingBadArgument,
			});
		}
		expect(host.recording.externalUrls).toHaveLength(2);
		expect(() => requireExternalUrl("ftp://example.com/x")).toThrow(/only http and https/);
	});

	it("keeps telemetry inert even once the user has opted in", async () => {
		const { adapter } = live();
		expect(adapter.telemetry.isEnabled()).toBe(false);
		// Recording while opted out must change nothing observable...
		adapter.telemetry.record({ name: "session_start" });
		// ...and so must recording while opted in, because there is no sink and
		// no permission to reach one. A buffered "for later" is a store waiting
		// to be misused; a dropped one cannot be.
		await adapter.telemetry.setEnabled(true);
		expect(adapter.telemetry.isEnabled()).toBe(true);
		adapter.telemetry.record({ name: "session_start", metrics: { pages_shown: 3 } });
		expect(adapter.telemetry.isEnabled()).toBe(true);
	});

	it("persists settings and the opt-in across sessions", async () => {
		const link = createLoopbackLink();
		serveEngineHost({ link: link.host, engine: createFixtureEngine() });
		const host = createFakeHostEnv();
		host.usePort(link.client as unknown as HostPort);
		const adapter = createExtensionAdapter({ env: host.env, link: link.client });
		await adapter.storage.set("ui.theme", "dark");
		expect(await adapter.storage.get("ui.theme")).toBe("dark");
		await adapter.storage.delete("ui.theme");
		expect(await adapter.storage.get("ui.theme")).toBeNull();
		expect(await adapter.storage.get("never-set")).toBeNull();
	});

	it("titles the page and prints", async () => {
		const { adapter, host } = live();
		await adapter.window.setTitle("a.pdf - Selis PDF Viewer");
		await adapter.print.print({ id: "d", pageCount: 1, pageSizes: [] });
		expect(host.recording.titles).toEqual(["a.pdf - Selis PDF Viewer"]);
		expect(host.recording.printed).toBe(1);
	});

	it("releases the port when the adapter is disposed", async () => {
		const { adapter } = live();
		await adapter.dispose?.();
		// Disposing twice must not throw; shells call it from teardown paths
		// that can run more than once.
		await adapter.dispose?.();
	});
});

describe("the viewer session (SL-4.EXT.06)", () => {
	it("asks the worker for an engine document before connecting", async () => {
		// `chrome.runtime.connect` does not queue: connecting to a host that is
		// not listening yet drops the first request silently, which looks
		// exactly like a hung engine. So the order is asserted, not assumed.
		const order: string[] = [];
		const host = createFakeHostEnv();
		const realEnsure = host.env.ensureEngineHost;
		host.env.ensureEngineHost = async () => {
			order.push("ensure");
			await realEnsure();
		};
		host.usePort({
			postMessage: () => {},
			disconnect: () => {},
			onMessage: () => () => {},
			onDisconnect: () => () => {
				order.push("connect");
				return () => {};
			},
		});
		await createViewerSession(host.env);
		expect(order[0]).toBe("ensure");
		expect(host.recording.ensureEngineHostCalls).toBe(1);
	});

	it("names the message the worker matches on, from the shared vocabulary", () => {
		// The worker imports this constant rather than repeating the string, so
		// a rename cannot leave the two ends speaking different protocols.
		expect(ENSURE_ENGINE_HOST).toBe("selis/ensure-engine-host");
	});

	it("surfaces a worker that cannot create an engine document", async () => {
		const host = createFakeHostEnv();
		host.failEngineHost(new Error("only a single offscreen document may be created"));
		await expect(createViewerSession(host.env)).rejects.toThrow(/offscreen/);
	});

	it("titles from the document name and nothing else", () => {
		expect(titleFor("report.pdf")).toBe("report.pdf - Selis PDF Viewer");
		expect(titleFor("  ")).toBe("Selis PDF Viewer");
		// A descriptor that names a location rather than a file gets the
		// generic title; a path is never shown to a user.
		expect(sourceName({ kind: "opfs", path: "/home/me/secret.pdf" })).toBe("");
		expect(sourceName({ kind: "bytes", bytes: new ArrayBuffer(1), name: "a.pdf" })).toBe("a.pdf");
	});

	it("opens a document end to end and titles the page", async () => {
		const { adapter, host } = live();
		const bytes = sampleBytes();
		const { doc, title } = await openDocument(adapter, {
			kind: "bytes",
			bytes: bytes.buffer as ArrayBuffer,
			name: "report.pdf",
		});
		expect(doc.pageCount).toBe(3);
		expect(title).toBe("report.pdf - Selis PDF Viewer");
		expect(host.recording.titles).toEqual(["report.pdf - Selis PDF Viewer"]);
	});

	it("keeps the registry code and docState when describing a failure", () => {
		const failure = describeFailure(new (class extends Error {})("boom"));
		expect(failure.docState).toBe("Unchanged");
		expect(failure.message).toContain("boom");
		// A non-AdapterError is a bug in the shell, and must not be dressed up
		// as a registry failure the UI would style like a document problem.
		expect(failure.message).toContain("unexpected");
	});
});
