import { describe, expect, it } from "vitest";

import { PLAIN_STYLE, withStyle } from "./text-style.js";

describe("the text styles", () => {
	it("starts plain", () => {
		// Nothing is bold, italic or underlined until the reader says so.
		expect(PLAIN_STYLE).toEqual({ bold: false, italic: false, underline: false });
	});

	it("turns one style on and leaves the rest as they were", () => {
		// The reader picks bold, and the underline they already chose is
		// still there: the styles are independent of each other.
		const bold = withStyle({ ...PLAIN_STYLE, underline: true }, "bold", true);
		expect(bold).toEqual({ bold: true, italic: false, underline: true });
	});

	it("turns one style off again", () => {
		const bold = withStyle({ ...PLAIN_STYLE, bold: true }, "bold", false);
		expect(bold.bold).toBe(false);
	});
});
