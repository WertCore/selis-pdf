import { describe, expect, it } from "vitest";
import {
	type HandoffFile,
	MAX_HANDOFF_BYTES,
	MAX_HANDOFF_FILES,
	filesFromDrop,
	filesFromPaste,
	filesFromPicker,
	isPdfMime,
	isPdfName,
	isSameOrigin,
	parseSrcParam,
	sourceForFile,
} from "./handoff.js";

function file(name: string, size = 1024, type = "application/pdf"): HandoffFile {
	return { name, type, size };
}

describe("handoff file filtering (drop / paste / picker share one filter)", () => {
	it("accepts a PDF file", () => {
		const result = filesFromDrop({ files: [file("report.pdf")] });
		expect(result.accepted).toHaveLength(1);
		expect(result.rejected).toHaveLength(0);
	});

	it("rejects non-PDF filenames", () => {
		const result = filesFromDrop({ files: [file("cat.gif", 100, "image/gif")] });
		expect(result.accepted).toHaveLength(0);
		expect(result.rejected).toHaveLength(1);
	});

	it("rejects empty files", () => {
		const result = filesFromPaste({ files: [file("empty.pdf", 0)] });
		expect(result.accepted).toHaveLength(0);
		expect(result.rejected[0]).toMatch(/empty/);
	});

	it("rejects files over the 64 MiB bound", () => {
		const result = filesFromPicker([file("huge.pdf", MAX_HANDOFF_BYTES + 1)]);
		expect(result.accepted).toHaveLength(0);
		expect(result.rejected[0]).toMatch(/64 MiB/);
	});

	it("caps the file count", () => {
		const many = Array.from({ length: MAX_HANDOFF_FILES + 5 }, (_, i) => file(`d${i}.pdf`));
		const result = filesFromDrop({ files: many });
		expect(result.accepted).toHaveLength(MAX_HANDOFF_FILES);
		expect(result.rejected).toHaveLength(5);
	});

	it("rejects unexpected MIME types but allows empty (OS drags omit it)", () => {
		const bad = filesFromDrop({ files: [file("a.pdf", 100, "image/png")] });
		expect(bad.accepted).toHaveLength(0);
		const emptyType = filesFromDrop({ files: [file("b.pdf", 100, "")] });
		expect(emptyType.accepted).toHaveLength(1);
	});

	it("paste and picker enforce the same filter as drop", () => {
		const bad = [file("x.png", 100, "image/png")];
		expect(filesFromPaste({ files: bad }).accepted).toHaveLength(0);
		expect(filesFromPicker(bad).accepted).toHaveLength(0);
	});
});

describe("isPdfName / isPdfMime", () => {
	it("matches case-insensitively and trims", () => {
		expect(isPdfName("Report.PDF")).toBe(true);
		expect(isPdfName("  a.pdf  ")).toBe(true);
		expect(isPdfName("")).toBe(false);
		expect(isPdfName("pdf")).toBe(false);
	});

	it("allows empty MIME, requires application/pdf otherwise", () => {
		expect(isPdfMime("")).toBe(true);
		expect(isPdfMime("application/pdf")).toBe(true);
		expect(isPdfMime("text/plain")).toBe(false);
	});
});

describe("?src= URL opening (all local)", () => {
	it("returns no-op when ?src= is absent", () => {
		expect(parseSrcParam("https://app.selis/viewer")).toEqual({
			ok: false,
			reason: "no ?src= parameter",
		});
	});

	it("accepts an https URL as a local http-range source", () => {
		const parsed = parseSrcParam("https://app.selis/viewer?src=https://example.org/r.pdf");
		expect(parsed.ok).toBe(true);
		if (parsed.ok) {
			expect(parsed.source).toEqual({
				kind: "http-range",
				url: "https://example.org/r.pdf",
			});
		}
	});

	it("resolves a relative ?src= against the app URL", () => {
		const parsed = parseSrcParam("https://app.selis/viewer?src=/docs/r.pdf");
		expect(parsed.ok).toBe(true);
		if (parsed.ok) {
			expect(parsed.source).toEqual({
				kind: "http-range",
				url: "https://app.selis/docs/r.pdf",
			});
		}
	});

	it("rejects active-content and handle schemes", () => {
		for (const src of [
			"javascript:alert(1)",
			"data:application/pdf;base64,JVBERi0=",
			"file:///etc/passwd",
			"blob:https://app.selis/uuid",
			"opfs:/selis/doc.pdf",
		]) {
			const parsed = parseSrcParam(`https://app.selis/viewer?src=${encodeURIComponent(src)}`);
			expect(parsed.ok, src).toBe(false);
		}
	});

	it("isSameOrigin distinguishes local app URLs from remote ones", () => {
		expect(isSameOrigin("https://app.selis/docs/a.pdf", "https://app.selis/viewer")).toBe(true);
		expect(isSameOrigin("https://example.org/a.pdf", "https://app.selis/viewer")).toBe(false);
		expect(isSameOrigin("not a url", "https://app.selis/viewer")).toBe(false);
	});
});

describe("sourceForFile", () => {
	it("maps a file to a blob source without reading bytes", () => {
		expect(sourceForFile(file("a.pdf", 1234))).toEqual({ kind: "blob", name: "a.pdf", size: 1234 });
	});
});
