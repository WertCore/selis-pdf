/**
 * SL-4.WEB.01 header gate: the canonical builders in `headers.ts` and the
 * two provider files (`public/_headers`, `vercel.json`) must agree, and the
 * policy must stay strict (wasm-unsafe-eval only, no remotes, A+ set).
 */

import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";
import {
	GRADED_HEADERS,
	ISOLATION_HEADERS,
	buildContentSecurityPolicy,
	buildSecurityHeaders,
} from "./headers.js";

const pkgRoot = join(dirname(fileURLToPath(import.meta.url)), "..");

function readProviderFiles(): { headersFile: string; vercel: string } {
	return {
		headersFile: readFileSync(join(pkgRoot, "public", "_headers"), "utf8"),
		vercel: readFileSync(join(pkgRoot, "vercel.json"), "utf8"),
	};
}

describe("WEB.01 canonical security headers", () => {
	it("emits every header securityheaders.sh grades", () => {
		const headers = buildSecurityHeaders();
		for (const name of GRADED_HEADERS) {
			expect(headers[name], name).toBeDefined();
			expect(headers[name]?.length ?? 0, name).toBeGreaterThan(0);
		}
	});

	it("sends COOP same-origin + COEP require-corp for the threaded path", () => {
		const headers = buildSecurityHeaders();
		expect(headers["Cross-Origin-Opener-Policy"]).toBe(
			ISOLATION_HEADERS["Cross-Origin-Opener-Policy"],
		);
		expect(headers["Cross-Origin-Embedder-Policy"]).toBe(
			ISOLATION_HEADERS["Cross-Origin-Embedder-Policy"],
		);
	});

	it("CSP allows wasm-unsafe-eval and nothing broader", () => {
		const csp = buildContentSecurityPolicy();
		expect(csp).toContain("'wasm-unsafe-eval'");
		expect(csp).not.toContain("'unsafe-inline'");
		// No full eval: the only allowed eval-shaped token is the narrow WASM one.
		expect(csp.replaceAll("'wasm-unsafe-eval'", "")).not.toContain("'unsafe-eval'");
		expect(csp).toContain("object-src 'none'");
		expect(csp).toContain("frame-ancestors 'none'");
		expect(csp).toContain("base-uri 'self'");
		expect(csp).toContain("form-action 'none'");
	});

	it("CSP names no remote hosts (ADR-P0016: no third-party scripts)", () => {
		const csp = buildContentSecurityPolicy();
		// Strip the one directive keyword that legitimately contains `https`.
		const withoutUpgrade = csp.replace("upgrade-insecure-requests", "");
		expect(withoutUpgrade).not.toMatch(/https?:\/\//);
		// `script-src` itself must never allow data:/blob: execution — those
		// schemes appear only in img-src/media-src for locally-rendered tiles.
		const script = csp
			.split(";")
			.map((part) => part.trim())
			.find((part) => part.startsWith("script-src"));
		expect(script).toBe("script-src 'self' 'wasm-unsafe-eval'");
	});

	it("locks connect-src to self (remote ?src= needs a reviewed exception)", () => {
		const csp = buildContentSecurityPolicy();
		const connect = csp
			.split(";")
			.map((part) => part.trim())
			.find((part) => part.startsWith("connect-src"));
		expect(connect).toBe("connect-src 'self'");
	});

	it("provider files agree with the canonical headers (no drift)", () => {
		const canonical = buildSecurityHeaders();
		const { headersFile, vercel } = readProviderFiles();
		for (const [name, value] of Object.entries(canonical)) {
			expect(headersFile, `_headers missing ${name}`).toContain(`${name}: ${value}`);
			expect(vercel, `vercel.json missing ${name}`).toContain(value);
		}
		expect(headersFile).toContain(buildContentSecurityPolicy());
	});
});
