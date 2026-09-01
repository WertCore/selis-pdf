import { describe, expect, it } from "vitest";
import { bootstrap } from "./index.js";

describe("ui", () => {
	it("bootstraps against the ui-kit", () => {
		expect(bootstrap()).toMatch(/^selis ui \d+\.\d+\.\d+$/);
	});
});
