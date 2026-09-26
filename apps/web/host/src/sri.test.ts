/**
 * SL-4.WEB.01 SRI gate: every served asset carries `integrity` +
 * `crossorigin`, no third-party script hosts on the document path, and the
 * served `index.html` honours both. The build fills real digests via
 * `computeSri`; this suite enforces the shape so a missing attribute fails
 * CI instead of shipping silently unpinned.
 */

import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";
import {
	SRI_ALGORITHM,
	collectAssetReferences,
	computeSri,
	findAssetsWithoutSri,
	findThirdPartyRefs,
	hasValidIntegrityFormat,
	isSameOriginRef,
} from "./sri.js";

const pkgRoot = join(dirname(fileURLToPath(import.meta.url)), "..");

describe("WEB.01 subresource integrity", () => {
	it("pins the algorithm to sha384", () => {
		expect(SRI_ALGORITHM).toBe("sha384");
	});

	it("computes stable, well-formed sha384 digests", () => {
		const a = computeSri(new TextEncoder().encode("selis-viewer-shell"));
		const b = computeSri(new TextEncoder().encode("selis-viewer-shell"));
		const c = computeSri(new TextEncoder().encode("selis-viewer-shell!"));
		expect(a).toBe(b);
		expect(a).not.toBe(c);
		expect(hasValidIntegrityFormat(a)).toBe(true);
		expect(hasValidIntegrityFormat("sha256-abc")).toBe(false);
		expect(hasValidIntegrityFormat("")).toBe(false);
	});

	it("treats only same-origin refs as local (ADR-P0016)", () => {
		expect(isSameOriginRef("/assets/app.js")).toBe(true);
		expect(isSameOriginRef("./worker.js")).toBe(true);
		expect(isSameOriginRef("../assets/style.css")).toBe(true);
		expect(isSameOriginRef("assets/app.js")).toBe(true);
		expect(isSameOriginRef("https://cdn.example/app.js")).toBe(false);
		expect(isSameOriginRef("http://cdn.example/app.js")).toBe(false);
		expect(isSameOriginRef("//cdn.example/app.js")).toBe(false);
	});

	it("flags remote authored URLs and missing integrity attributes", () => {
		const bad = `<script src="https://cdn.example/app.js"></script>
			<script src="/assets/app.js"></script>`;
		expect(findThirdPartyRefs(bad)).toEqual(["https://cdn.example/app.js"]);
		expect(findAssetsWithoutSri(bad)).toHaveLength(2);
		const goodDigest = computeSri(new TextEncoder().encode("x"));
		const good = `<script src="/assets/app.js" integrity="${goodDigest}" crossorigin="anonymous"></script>`;
		expect(findThirdPartyRefs(good)).toEqual([]);
		expect(findAssetsWithoutSri(good)).toEqual([]);
		expect(collectAssetReferences(good)).toHaveLength(1);
	});

	it("served index.html has no third-party scripts and pins every asset", () => {
		const html = readFileSync(join(pkgRoot, "public", "index.html"), "utf8");
		expect(findThirdPartyRefs(html), "remote script/style hosts are forbidden").toEqual([]);
		expect(
			findAssetsWithoutSri(html),
			"every <script src>/<link rel=stylesheet> needs integrity + crossorigin",
		).toEqual([]);
	});
});
