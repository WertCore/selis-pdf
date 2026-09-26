/**
 * Subresource Integrity helpers (SL-4.WEB.01: "SRI on every asset").
 *
 * Contract:
 * - Every `<script src>` and `<link rel="stylesheet">` in the served HTML
 *   carries `integrity="sha384-<base64>"` + `crossorigin="anonymous"`.
 * - No third-party script hosts on the document-handling path (ADR-P0016):
 *   every `src`/`href` is same-origin (`/`, `./`, `../`, or a bare relative
 *   path). Anything `https://`, `http://`, or protocol-relative (`//`) fails
 *   the gate — the one exception is the `https://` inside the CSP keyword
 *   `upgrade-insecure-requests`, which is a directive, not a fetch.
 * - Hash algorithm is fixed to `sha384` (the SRI sweet spot: stronger than
 *   sha256, shorter than sha512). The build fills real digests; this module
 *   computes and validates them, and the tests enforce the HTML shape.
 */

import { createHash } from "node:crypto";

/** The only SRI hash algorithm this deployment uses. */
export const SRI_ALGORITHM = "sha384" as const;

/** `sha384-<base64>` integrity value pattern: 48 digest bytes = exactly 64 base64 chars, no padding. */
export const SRI_VALUE_PATTERN = /^sha384-[A-Za-z0-9+/]{64}$/;

/** Compute the `sha384-<base64>` integrity value for raw bytes. */
export function computeSri(data: Uint8Array): string {
	const digest = createHash("sha384").update(data).digest("base64");
	return `sha384-${digest}`;
}

/** Whether `value` is a well-formed `sha384-…` integrity attribute. */
export function hasValidIntegrityFormat(value: string): boolean {
	return SRI_VALUE_PATTERN.test(value.trim());
}

/** One external asset reference found in HTML. */
export interface AssetReference {
	readonly tag: "script" | "link";
	readonly raw: string;
	readonly src: string;
}

/**
 * Collect `<script src="…">` and `<link rel="stylesheet" href="…">`
 * references from HTML. Inline scripts/styles (no `src`/`href`) are ignored
 * — the CSP (`headers.ts`) already forbids inline execution, so inline
 * presence is a separate, louder failure caught by the CSP test.
 */
export function collectAssetReferences(html: string): AssetReference[] {
	const found: AssetReference[] = [];
	for (const match of html.matchAll(/<script\b[^>]*\bsrc\s*=\s*(['"])(.*?)\1[^>]*>/gi)) {
		const src = match[2] ?? "";
		if (src.length > 0) {
			found.push({ tag: "script", raw: match[0] ?? "", src });
		}
	}
	for (const match of html.matchAll(/<link\b[^>]*>/gi)) {
		const tag = match[0] ?? "";
		if (!/\brel\s*=\s*(['"])stylesheet\1/i.test(tag)) {
			continue;
		}
		const href = /href\s*=\s*(['"])(.*?)\1/i.exec(tag)?.[2] ?? "";
		if (href.length > 0) {
			found.push({ tag: "link", raw: tag, src: href });
		}
	}
	return found;
}

/** Whether `src` is same-origin (relative or root-absolute, never remote). */
export function isSameOriginRef(src: string): boolean {
	const value = src.trim();
	if (value.length === 0) {
		return false;
	}
	if (value.startsWith("//")) {
		return false;
	}
	if (/^[a-zA-Z][a-zA-Z0-9+.-]*:/.test(value)) {
		return value.startsWith("blob:") || value.startsWith("data:");
	}
	return true;
}

/**
 * Script/link `src` values that point off-origin. `blob:`/`data:` tile URLs
 * are created at runtime, never authored into HTML, so an authored remote
 * URL is always a violation here.
 */
export function findThirdPartyRefs(html: string): string[] {
	const offenders: string[] = [];
	for (const ref of collectAssetReferences(html)) {
		const value = ref.src.trim();
		if (value.startsWith("//")) {
			offenders.push(value);
			continue;
		}
		const scheme = /^[a-zA-Z][a-zA-Z0-9+.-]*:/.exec(value)?.[0];
		if (scheme !== undefined && scheme !== "blob:" && scheme !== "data:") {
			offenders.push(value);
		}
	}
	return offenders;
}

/**
 * Asset tags missing `integrity` or `crossorigin="anonymous"`. Returns the
 * raw tags so the failure message shows exactly what to fix.
 */
export function findAssetsWithoutSri(html: string): string[] {
	const missing: string[] = [];
	for (const match of html.matchAll(/<script\b[^>]*\bsrc\s*=\s*(['"]).*?\1[^>]*>/gi)) {
		const tag = match[0] ?? "";
		const integrity = /integrity\s*=\s*(['"])(.*?)\1/i.exec(tag)?.[2] ?? "";
		const crossorigin = /crossorigin\s*=\s*(['"])(.*?)\1/i.exec(tag)?.[2] ?? "";
		if (!hasValidIntegrityFormat(integrity) || crossorigin !== "anonymous") {
			missing.push(tag);
		}
	}
	for (const match of html.matchAll(/<link\b[^>]*>/gi)) {
		const tag = match[0] ?? "";
		if (!/\brel\s*=\s*(['"])stylesheet\1/i.test(tag)) {
			continue;
		}
		if (!/\bhref\s*=/i.test(tag)) {
			continue;
		}
		const integrity = /integrity\s*=\s*(['"])(.*?)\1/i.exec(tag)?.[2] ?? "";
		const crossorigin = /crossorigin\s*=\s*(['"])(.*?)\1/i.exec(tag)?.[2] ?? "";
		if (!hasValidIntegrityFormat(integrity) || crossorigin !== "anonymous") {
			missing.push(tag);
		}
	}
	return missing;
}
