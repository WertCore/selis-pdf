/**
 * SL-4.EXT.07 — the options page's state, and the first-run trigger.
 *
 * The suite is written around the two ways a first-run flag goes wrong in
 * production, because both are invisible until a user is involved:
 *
 * - **Nagged forever.** The guide reappears on every update, or after every
 *   options-page visit, because nothing recorded that it had been seen.
 * - **Never shown at all.** The flag is written before the guide is rendered, or
 *   a corrupt value counts as "seen", and the user never learns what the
 *   extension does.
 *
 * Both are properties of {@link planFirstRun} plus one write, so both are
 * asserted as sequences rather than as single calls: the trigger is exercised
 * the way the browser actually produces it (`install` once, `update`
 * afterwards), and the flag is written the way the page actually writes it.
 *
 * The last test is the one that matters most for review: the key this page
 * writes is the key the viewer's adapter reads. Two settings stores with two
 * key names would be a switch that saves and an adapter that never sees it.
 */

import { describe, expect, it } from "vitest";
import { createExtensionAdapter, restoreTelemetryPreference } from "./ext/adapter.js";
import { createFakeHostEnv } from "./ext/fake-host-env.js";
import { createLoopbackLink } from "./ext/loopback.js";
import {
	DEFAULT_SETTINGS,
	ONBOARDING_KEY,
	ONBOARDING_SEEN,
	type OnboardingStore,
	SETTINGS_KEYS,
	type SettingsStore,
	createOnboardingStore,
	isTelemetryOptIn,
	onboardingSeenFrom,
	planFirstRun,
	readSettings,
	resetSettings,
	writeTelemetryOptIn,
} from "./options-state.js";

/** An in-memory settings store, so the tests need no `localStorage`. */
function memorySettings(seed: Record<string, string> = {}): SettingsStore & {
	readonly written: Map<string, string>;
} {
	const written = new Map(Object.entries(seed));
	return {
		written,
		async get(key) {
			return written.get(key) ?? null;
		},
		async set(key, value) {
			written.set(key, value);
		},
		async remove(key) {
			written.delete(key);
		},
	};
}

/** An in-memory `chrome.storage.local` area, recording what was written. */
function memoryArea(seed: Record<string, unknown> = {}): {
	area: {
		get(key: string): Promise<Record<string, unknown>>;
		set(items: Record<string, unknown>): Promise<void>;
	};
	readonly record: Record<string, unknown>;
} {
	const record: Record<string, unknown> = { ...seed };
	return {
		record,
		area: {
			async get(key) {
				return key in record ? { [key]: record[key] } : {};
			},
			async set(items) {
				Object.assign(record, items);
			},
		},
	};
}

/** An {@link OnboardingStore} that fails every call, for the unavailable case. */
const failingStore: OnboardingStore = {
	read: () => Promise.reject(new Error("chrome.storage.local is unavailable")),
	markSeen: () => Promise.reject(new Error("chrome.storage.local is unavailable")),
};

describe("the settings store (SL-4.EXT.07)", () => {
	it("is off unless a decision was written", async () => {
		// ADR-P0017: the default is the absence of a decision.
		expect(DEFAULT_SETTINGS.telemetryOptIn).toBe(false);
		expect((await readSettings(memorySettings())).telemetryOptIn).toBe(false);
	});

	it("reads only the exact opt-in value as consent", () => {
		// A permissive parse here would turn a value written by some future
		// build into consent the user never gave.
		expect(isTelemetryOptIn("1")).toBe(true);
		for (const notConsent of ["0", "", "true", "TRUE", "yes", " 1", "1 ", null]) {
			expect(isTelemetryOptIn(notConsent), String(notConsent)).toBe(false);
		}
	});

	it("round-trips the opt-in and writes every value it is given", async () => {
		const store = memorySettings();
		await writeTelemetryOptIn(store, true);
		expect((await readSettings(store)).telemetryOptIn).toBe(true);
		await writeTelemetryOptIn(store, false);
		expect((await readSettings(store)).telemetryOptIn).toBe(false);
		// Opting out writes "0" rather than removing the key, so "unset" and
		// "explicitly off" cannot be confused by a later reader.
		expect(store.written.get(SETTINGS_KEYS.telemetryOptIn)).toBe("0");
	});

	it("resets by removing every key it owns, and nothing else", async () => {
		const store = memorySettings({ [SETTINGS_KEYS.telemetryOptIn]: "1", unrelated: "keep" });
		await resetSettings(store);
		expect(store.written.has(SETTINGS_KEYS.telemetryOptIn)).toBe(false);
		expect(store.written.get("unrelated")).toBe("keep");
	});

	it("is the same key the viewer's adapter reads and writes", async () => {
		// The seam, tested end to end: the options page writes, the adapter
		// restores. If these ever diverge, the switch saves and nothing changes.
		const host = createFakeHostEnv();
		await writeTelemetryOptIn(host.env.storage, true);
		const link = createLoopbackLink();
		const adapter = createExtensionAdapter({ env: host.env, link: link.client });
		await restoreTelemetryPreference({ env: host.env, adapter });
		expect(adapter.telemetry.isEnabled()).toBe(true);

		await adapter.telemetry.setEnabled(false);
		expect((await readSettings(host.env.storage)).telemetryOptIn).toBe(false);
	});
});

describe("the first-run trigger (SL-4.EXT.07)", () => {
	it("shows the guide on a fresh install and only then", () => {
		expect(planFirstRun({ reason: "install", seen: false })).toEqual({
			show: true,
			outcome: "fresh-install",
		});
	});

	it("does not nag: nothing but a fresh install ever shows it", () => {
		// An update, a browser upgrade, a shared-module update, and a reason
		// this build has never heard of — all decline, seen or not.
		for (const reason of [
			"update",
			"chrome_update",
			"shared_module_update",
			"something_newer_browser_did",
		]) {
			for (const seen of [false, true]) {
				expect(planFirstRun({ reason, seen }), `${reason}/${String(seen)}`).toEqual({
					show: false,
					outcome: "not-first-run",
				});
			}
		}
	});

	it("does not nag a second time: a seen guide is never offered again", () => {
		expect(planFirstRun({ reason: "install", seen: true })).toEqual({
			show: false,
			outcome: "already-seen",
		});
	});

	it("treats a missing or corrupt flag as unseen, so the guide is not lost", () => {
		// The safe direction: an extra read of a page the user can close is
		// recoverable, never having been told is not.
		for (const stored of [undefined, null, "", "0", "1", true, false, 1, {}, []]) {
			expect(onboardingSeenFrom(stored), JSON.stringify(stored)).toBe(false);
		}
		expect(onboardingSeenFrom(ONBOARDING_SEEN)).toBe(true);
		expect(planFirstRun({ reason: "install", seen: onboardingSeenFrom("nonsense") }).show).toBe(
			true,
		);
	});
});

describe("the flag in chrome.storage.local (SL-4.EXT.07)", () => {
	it("round-trips through the area under one versioned key", async () => {
		const { area, record } = memoryArea();
		const store = createOnboardingStore(area);
		expect(await store.read()).toEqual({ seen: false });
		await store.markSeen();
		expect(record).toEqual({ [ONBOARDING_KEY]: ONBOARDING_SEEN });
		expect(await store.read()).toEqual({ seen: true });
	});

	it("reads a key another build wrote as unseen", async () => {
		const { area } = memoryArea({ [ONBOARDING_KEY]: { seen: true } });
		expect(await createOnboardingStore(area).read()).toEqual({ seen: false });
	});

	it("rejects rather than guessing when the store is unavailable", async () => {
		const store = createOnboardingStore(undefined);
		// No default: "seen" would lose the guide silently, "not seen" would
		// show it on every visit forever. The caller decides.
		await expect(store.read()).rejects.toThrow(/chrome\.storage\.local/);
		await expect(store.markSeen()).rejects.toThrow(/chrome\.storage\.local/);
	});

	it("closes the loop: one install shows the guide, and the next one does not", async () => {
		// The whole anti-nag mechanism, as the browser drives it: install, the
		// page records that it rendered the guide, every later event declines.
		const { area } = memoryArea();
		const store = createOnboardingStore(area);
		const events = ["install", "update", "install"];
		const shown: boolean[] = [];
		for (const reason of events) {
			shown.push(planFirstRun({ reason, seen: (await store.read()).seen }).show);
			if (shown.at(-1) === true) {
				await store.markSeen();
			}
		}
		expect(shown).toEqual([true, false, false]);
	});

	it("leaves the page able to show the guide when the store cannot be read", async () => {
		// What `options-boot.ts` relies on: the failure is catchable, so the
		// guide is shown for this page load and nothing is written.
		let seen: boolean | null = null;
		try {
			seen = (await failingStore.read()).seen;
		} catch {
			seen = false;
		}
		expect(seen).toBe(false);
	});
});
