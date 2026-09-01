import { describe, expect, it } from "vitest";
import { VERSION } from "./index.js";

describe("ui-kit", () => {
	it("exports a semver version", () => {
		expect(VERSION).toMatch(/^\d+\.\d+\.\d+$/);
	});
});
