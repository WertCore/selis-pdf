/**
 * SL-4.SHIP.02 — typed, local feature flags.
 *
 * Everything here is in-process: no fetch is stubbed because there is no
 * fetch. The cases pin the two properties that matter for a privacy product —
 * flags are off until explicitly enabled, and the only inputs are typed or
 * explicitly parsed local values.
 */

import { describe, expect, it } from "vitest";
import {
	DEFAULT_FLAGS,
	FEATURE_FLAGS,
	LocalFeatureFlags,
	applyFlagOverrides,
	createFeatureFlags,
	isRolloutEnabled,
	parseFlagOverrides,
	rolloutBucket,
	type FeatureFlag,
} from "./feature-flags.js";

describe("SL-4.SHIP.02 feature flags", () => {
	it("ships every flag default-off", () => {
		const store = new LocalFeatureFlags();
		expect(store.snapshot()).toEqual(DEFAULT_FLAGS);
		expect(store.enabled()).toEqual([]);
		for (const flag of FEATURE_FLAGS) {
			expect(store.isEnabled(flag), flag).toBe(false);
		}
	});

	it("takes an explicit opt-in and opt-out only", () => {
		const store = new LocalFeatureFlags();
		store.set("tools.extensionPanel", true);
		expect(store.isEnabled("tools.extensionPanel")).toBe(true);
		expect(store.enabled()).toEqual(["tools.extensionPanel"]);
		store.set("tools.extensionPanel", false);
		expect(store.isEnabled("tools.extensionPanel")).toBe(false);
		// A different flag is untouched.
		expect(store.isEnabled("ui.noUploadIndicator")).toBe(false);
	});

	it("createFeatureFlags applies only typed true overrides", () => {
		const store = createFeatureFlags({ "ui.noUploadIndicator": true });
		expect(store.isEnabled("ui.noUploadIndicator")).toBe(true);
		expect(store.isEnabled("tools.extensionPanel")).toBe(false);
	});

	it("refuses unknown flag names and non-boolean values", () => {
		const parsed = parseFlagOverrides({
			"tools.extensionPanel": true,
			"ui.unknownFlag": true,
			"viewer.editDeepLink": "yes",
		});
		expect(parsed.flags).toEqual({ "tools.extensionPanel": true });
		expect([...parsed.rejected].sort()).toEqual(["ui.unknownFlag", "viewer.editDeepLink"]);

		const store = new LocalFeatureFlags();
		const rejected = applyFlagOverrides(store, {
			"tools.extensionPanel": true,
			typo: true,
		});
		expect(rejected).toEqual(["typo"]);
		expect(store.isEnabled("tools.extensionPanel")).toBe(true);
		expect(store.enabled()).toEqual(["tools.extensionPanel"]);
	});

	it("rollout bucketing is stable and bounded", () => {
		const bucket = rolloutBucket("viewer.editDeepLink", "install-42");
		expect(bucket).toBeGreaterThanOrEqual(0);
		expect(bucket).toBeLessThan(100);
		expect(rolloutBucket("viewer.editDeepLink", "install-42")).toBe(bucket);
		const subjects = new Set(
			Array.from({ length: 20 }, (_, i) => rolloutBucket("viewer.editDeepLink", `install-${i}`)),
		);
		expect(subjects.size).toBeGreaterThan(1);
	});

	it("staged rollout fails closed at 0, opens at 100, and is deterministic between", () => {
		const store = new LocalFeatureFlags();
		const config = {
			flag: "viewer.editDeepLink" as FeatureFlag,
			subject: "install-42",
			percentage: 0,
		};
		expect(isRolloutEnabled(store, config)).toBe(false);
		expect(isRolloutEnabled(store, { ...config, percentage: 100 })).toBe(true);
		const bucket = rolloutBucket(config.flag, config.subject);
		const decided = isRolloutEnabled(store, { ...config, percentage: 50 });
		expect(decided).toBe(bucket < 50);
		expect(isRolloutEnabled(store, { ...config, percentage: 50 })).toBe(decided);
	});

	it("an explicit override wins over any rollout bucket", () => {
		const store = new LocalFeatureFlags();
		store.set("viewer.editDeepLink", true);
		expect(
			isRolloutEnabled(store, {
				flag: "viewer.editDeepLink",
				subject: "install-42",
				percentage: 0,
			}),
		).toBe(true);
	});

	it("invalid percentages fail closed", () => {
		const store = new LocalFeatureFlags();
		for (const percentage of [Number.NaN, Number.POSITIVE_INFINITY, -1]) {
			expect(
				isRolloutEnabled(store, {
					flag: "tools.extensionPanel",
					subject: "s",
					percentage,
				}),
				String(percentage),
			).toBe(false);
		}
	});
});
