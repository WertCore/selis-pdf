/**
 * Registration, scope, and the update handoff (SL-4.WEB.02).
 *
 * The refusals are the interesting half. A service worker is an enhancement over
 * a shell that already opens local files, so every environment where it cannot
 * run - no container, `file://`, an insecure context - has to come back as a
 * value the caller can carry on from, never as a throw and never as a
 * half-registered worker.
 */

import { describe, expect, it } from "vitest";
import { SERVICE_WORKER_PATH, SKIP_WAITING_MESSAGE, SW_SCOPE } from "./sw-policy.js";
import {
	type RegisterEnvironment,
	applyUpdate,
	liveRegisterEnvironment,
	registerServiceWorker,
	registrationBlocker,
	registrationOptions,
	watchForUpdate,
} from "./sw-register.js";

/** A container double that records what it was asked to register. */
function container(behaviour: "ok" | "reject" = "ok") {
	const calls: { url: string; options: RegistrationOptions | undefined }[] = [];
	const registration = { scope: `${SW_SCOPE}` } as unknown as ServiceWorkerRegistration;
	return {
		calls,
		env: {
			container: {
				register: async (url: string, options?: RegistrationOptions) => {
					calls.push({ url, options });
					if (behaviour === "reject") {
						throw new Error("SecurityError: script fetch failed");
					}
					return registration;
				},
			},
			secureContext: true,
			protocol: "https:",
		} satisfies RegisterEnvironment,
	};
}

describe("where the worker is registered", () => {
	it("serves the worker from the root and controls the whole origin", () => {
		// A narrower scope would leave /wasm/… outside the worker, and the
		// offline story would be half-built.
		expect(SERVICE_WORKER_PATH.startsWith("/")).toBe(true);
		expect(SW_SCOPE).toBe("/");
		const options = registrationOptions();
		expect(options.scope).toBe("/");
		expect(options.type).toBe("module");
		// A stale HTTP-cached sw.js would pin an old version forever.
		expect(options.updateViaCache).toBe("none");
	});

	it("passes exactly that to register()", async () => {
		const fake = container();
		const result = await registerServiceWorker(fake.env);
		expect(result.ok).toBe(true);
		expect(fake.calls).toHaveLength(1);
		expect(fake.calls[0]?.url).toBe("/sw.js");
		expect(fake.calls[0]?.options).toEqual(registrationOptions());
	});
});

describe("environments where the worker cannot run", () => {
	it("names each refusal instead of throwing", () => {
		expect(registrationBlocker({ container: null })).toBe("no-container");
		expect(registrationBlocker({})).toBe("no-container");
		expect(registrationBlocker({ container: container().env.container, protocol: "file:" })).toBe(
			"file-protocol",
		);
		expect(
			registrationBlocker({ container: container().env.container, secureContext: false }),
		).toBe("insecure-context");
		expect(
			registrationBlocker({ container: container().env.container, secureContext: true }),
		).toBeNull();
	});

	it("returns the refusal rather than calling register()", async () => {
		const fake = container();
		const result = await registerServiceWorker({
			...fake.env,
			protocol: "file:",
		});
		expect(result).toEqual({ ok: false, reason: "file-protocol" });
		expect(fake.calls).toHaveLength(0);
	});

	it("reports a rejected registration as a value, not an exception", async () => {
		const result = await registerServiceWorker(container("reject").env);
		expect(result.ok).toBe(false);
		if (result.ok) {
			return;
		}
		expect(result.reason).toBe("failed");
		expect(result.detail).toContain("SecurityError");
	});

	it("finds no container under Node, so importing it is inert", () => {
		expect(registrationBlocker(liveRegisterEnvironment())).toBe("no-container");
	});
});

/** A `ServiceWorkerRegistration` double whose events the test fires by hand. */
function fakeRegistration(controller: unknown) {
	const listeners = new Map<string, (() => void)[]>();
	const posted: unknown[] = [];
	const installing = {
		state: "installing",
		addEventListener: (type: string, listener: () => void) => {
			listeners.set(`installing:${type}`, [
				...(listeners.get(`installing:${type}`) ?? []),
				listener,
			]);
		},
	};
	const registration = {
		scope: "/",
		controller,
		installing,
		waiting: { postMessage: (data: unknown) => posted.push(data) },
		addEventListener: (type: string, listener: () => void) => {
			listeners.set(type, [...(listeners.get(type) ?? []), listener]);
		},
	} as unknown as ServiceWorkerRegistration;
	const fire = (key: string) => {
		for (const listener of listeners.get(key) ?? []) {
			listener();
		}
	};
	return { registration, installing, posted, fire };
}

describe("the update handoff", () => {
	it("reports a replacement worker, and the page decides when it takes over", async () => {
		const fake = fakeRegistration({ scriptURL: "/sw.js" });
		const reports: string[] = [];
		watchForUpdate(fake.registration, (report) => reports.push(report.kind));
		fake.fire("updatefound");
		fake.installing.state = "installed";
		fake.fire("installing:statechange");
		expect(reports).toEqual(["update-ready"]);
	});

	it("does not call the first install an update", () => {
		// No controller means nothing is being replaced: announcing that would
		// have the page show an "update" prompt on a first visit.
		const fake = fakeRegistration(null);
		const reports: string[] = [];
		watchForUpdate(fake.registration, (report) => reports.push(report.kind));
		fake.fire("updatefound");
		fake.installing.state = "installed";
		fake.fire("installing:statechange");
		expect(reports).toEqual([]);
	});

	it("ignores a worker that never reaches installed", () => {
		const fake = fakeRegistration({ scriptURL: "/sw.js" });
		const reports: string[] = [];
		watchForUpdate(fake.registration, (report) => reports.push(report.kind));
		fake.fire("updatefound");
		fake.installing.state = "redundant";
		fake.fire("installing:statechange");
		expect(reports).toEqual([]);
	});

	it("asks the waiting worker to take over, and only when one is waiting", async () => {
		const fake = fakeRegistration({ scriptURL: "/sw.js" });
		await applyUpdate(fake.registration);
		expect(fake.posted).toEqual([{ type: SKIP_WAITING_MESSAGE }]);
	});

	it("is a no-op with nothing waiting, so it is safe to call every load", async () => {
		const fake = fakeRegistration({ scriptURL: "/sw.js" });
		const quiet = {
			...fake,
			registration: { ...fake.registration, waiting: null } as unknown as ServiceWorkerRegistration,
		};
		await applyUpdate(quiet.registration);
		expect(fake.posted).toEqual([]);
	});
});
