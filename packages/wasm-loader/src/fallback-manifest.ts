/**
 * The `selis-fallback/2` manifest: the lazily-fetched fallback faces.
 *
 * Mirrors `crates/selis-pdf-wasm/src/fallback_manifest.rs` validator for
 * validator, because the two readers guard the same trust boundary from
 * opposite sides: the Rust one is what `cargo xtask fallback-assets` checks its
 * own output against, and this one is what the shell turns a fetched manifest
 * into claims. A manifest that passes one and fails the other is a manifest the
 * build and the runtime disagree about, which is exactly the drift a build
 * artifact is supposed to make impossible.
 *
 * **A font from the network is not a font from the build.** A TTF is a
 * structured binary that a parser reads, so the digests here are a transport
 * check, not a supply-chain review — a compromised upstream Liberation release
 * would be faithfully reproduced by a matching digest. Nothing in this file
 * should ever be read as "and therefore trusted".
 *
 * ## Why this lives beside the client and not in the engine
 *
 * For the same reason the Rust half lives in the wasm crate: the manifest is a
 * delivery artifact, not font logic. It describes bytes in transit, and the
 * thing that moves them owns the trust boundary.
 */

import type { ClaimBody } from "./lazy-payload.js";

/**
 * Format version, matching `fallback_manifest::MANIFEST_VERSION`.
 *
 * **2** added the per-face `url`. A 1 document has no `url`, so it is refused
 * with a message that names the missing field and the format — never read with a
 * reconstructed path, which would reintroduce exactly the silent-404 hole the
 * field was added to close.
 */
export const FALLBACK_MANIFEST_VERSION = 2;

/** The manifest's media type, as served. */
export const FALLBACK_MANIFEST_MIME = "application/vnd.selis.fallback+json";

/**
 * The suffix every `url` must end in, because it says how the bytes are
 * encoded: brotli-compressed TrueType.
 *
 * The generator writes this and this reader requires it, so a manifest cannot
 * claim a payload the transport would decode wrongly. Mirrors
 * `fallback_manifest::FACE_SUFFIX`, which is asserted equal to it by
 * `xtask`'s `the_recorded_url_is_built_from_the_published_layout`.
 *
 * The manifest now **records** where each face is published; this constant is
 * only the check that the recorded value is plausible, not a source of paths.
 */
export const FALLBACK_FACE_SUFFIX = ".ttf.br";

/** One lazily-fetched fallback face, as the manifest describes it. */
export interface FallbackFace {
	/** `family + style`, e.g. `LiberationSerif-Italic`. */
	readonly name: string;
	/** Size of the brotli-compressed payload, for budgeting. */
	readonly transfer_size: number;
	/** Size of the decompressed font, which is what the engine actually holds. */
	readonly raw_size: number;
	/** Lowercase hex SHA-256 of the **decompressed** bytes. */
	readonly sha256: string;
	/**
	 * Where this face's compressed file is published, relative to the base the
	 * manifest itself was served from.
	 *
	 * **Relative, never absolute.** An absolute URL baked into a build artifact
	 * breaks the moment the same artifact is promoted from staging to
	 * production, and it would let a manifest point the fetcher at another
	 * origin. Checked by `urlProblem` before it is used to fetch anything.
	 */
	readonly url: string;
}

/** The whole lazy payload: every face this build does not embed. */
export interface FallbackManifest {
	/** Format version, checked against {@link FALLBACK_MANIFEST_VERSION}. */
	readonly version: number;
	/** The faces on offer, sorted by name by the generator. */
	readonly faces: readonly FallbackFace[];
}

/**
 * Every way the manifest reader can refuse, one variant per `ManifestError` in
 * the Rust validator so the two can be diffed line by line.
 */
export type FallbackManifestErrorCode =
	/** Not an object, not the right shape, or a field of the wrong type. */
	| "fallback-manifest-invalid"
	/** A `version` this build does not know how to read. */
	| "fallback-manifest-version"
	/** The same face name appeared more than once. */
	| "fallback-manifest-duplicate"
	/** A digest was not 64 lowercase hex characters. */
	| "fallback-manifest-digest"
	/** A face declared a zero size. */
	| "fallback-manifest-size"
	/** A face claims to transfer larger than it stores. */
	| "fallback-manifest-transfer-larger"
	/** A face's `url` was not a plain relative path to a compressed face. */
	| "fallback-manifest-url";

/** A typed refusal, carrying a code so a caller can branch without parsing prose. */
export class FallbackManifestError extends Error {
	constructor(
		readonly code: FallbackManifestErrorCode,
		message: string,
	) {
		super(message);
		this.name = "FallbackManifestError";
	}
}

/** Rust's `u32` ceiling; serde rejects anything above it before `validate` runs. */
const U32_MAX = 0xffff_ffff;

/**
 * Validate a parsed manifest, or refuse the whole thing.
 *
 * ## Why refusing whole beats skipping the bad entry
 *
 * The obvious alternative is to drop the one entry with the malformed digest and
 * serve the other nine. That renders a document with nine of its ten fallbacks
 * while looking perfectly healthy: the text that needed the tenth comes out
 * `.notdef`, and nothing anywhere reports an error. A manifest naming one face
 * with a malformed digest is *evidence that something is wrong with the
 * manifest*, not evidence that one face is missing — and every caller of a bad
 * manifest has the same reaction, which is to stop and load nothing. So the
 * reader returns `Err` rather than a partially-valid manifest.
 *
 * ## The order of the checks, and why it is this order
 *
 * Structural checks first, then the version, then the per-face rules — the same
 * order as the Rust validator, because `serde` deserialises the whole document
 * before `validate()` runs, so a malformed *later* entry fails as a parse error
 * even when the version is also wrong. The per-face order (duplicate, digest,
 * size, transfer) is the Rust loop's order too. A reader that reorders these
 * reports a different reason for the same refusal, and two readers reporting
 * different reasons for the same bytes is how a real bug gets argued away as a
 * message mismatch.
 *
 * ## The digest rule, and why both readers are strict about it
 *
 * Both readers admit only `0-9a-f`. This is not an arbitrary tightening: the
 * guest's `cjkchunk::parse_digest` matches `0-9a-f` only, so an uppercase
 * digest produces a claim `FallbackFaceLoader::new` refuses at `fallbackOpen`.
 * A reader that accepted one would hand back claims guaranteed to be thrown
 * away, and a build whose manifest validated and then failed in the browser is
 * the worst of both — the error surfaces far from its cause, with no way to fix
 * it from the build log.
 *
 * (The Rust validator originally used `is_ascii_hexdigit`, which also admits
 * A-F, and the TypeScript half was the strict one. The Rust half has since been
 * tightened to match; `fallback-manifest.test.ts` and
 * `a_malformed_digest_is_refused` pin the rule from both sides so it cannot
 * drift back.)
 *
 * ## Why the `url` is read, and never reconstructed
 *
 * A `fallbackOpen` claim needs `{ name, sha256, rawBytes, url }`. `selis-fallback/1`
 * named the face and pinned its digest but said nothing about *where the bytes
 * are*, so the shell rebuilt the path from a convention duplicated here from the
 * generator — and a build publishing the payload under a CDN prefix, a version
 * segment, or a different extension silently fetched nothing while nothing
 * detected the two sides disagreeing. `selis-fallback/2` records the generator's
 * actual path per face, and this reader uses that value and nothing else.
 *
 * Because the value now arrives from the network, it is *validated* rather than
 * trusted: see {@link urlProblem} for the one rule and the shapes it refuses.
 * Checking it before use is the whole point of moving it out of a literal.
 */
export function parseFallbackManifest(value: unknown): FallbackManifest {
	const bad = (why: string): never => {
		throw new FallbackManifestError(
			"fallback-manifest-invalid",
			// The format hint rides on the *parse* failure because that is where
			// a document from an older format lands: it deserialises into
			// everything except the field that format did not have. "has no
			// url" is technically true and practically useless, so the version
			// this build reads is named alongside it — the same thing the Rust
			// reader does when serde reports a missing `url`.
			`fallback manifest rejected: ${why} (this build reads selis-fallback/${FALLBACK_MANIFEST_VERSION})`,
		);
	};
	if (typeof value !== "object" || value === null || Array.isArray(value)) {
		return bad("not an object");
	}
	const record = value as Record<string, unknown>;
	if (!Array.isArray(record.faces)) {
		return bad("`faces` is not an array");
	}

	// serde deserialises every face before `validate()` runs, so a shape error
	// anywhere in the document outranks the version check below.
	const faces = record.faces.map((entry, index) => readFace(entry, index, bad));

	if (record.version !== FALLBACK_MANIFEST_VERSION) {
		throw new FallbackManifestError(
			"fallback-manifest-version",
			`fallback manifest version ${String(record.version)} is not supported (expected ${FALLBACK_MANIFEST_VERSION})`,
		);
	}

	validate(faces);
	return { version: FALLBACK_MANIFEST_VERSION, faces };
}

/** One manifest row, structurally checked. A shape error is a parse error. */
function readFace(entry: unknown, index: number, bad: (why: string) => never): FallbackFace {
	if (typeof entry !== "object" || entry === null || Array.isArray(entry)) {
		return bad(`faces[${index}] is not an object`);
	}
	const row = entry as Record<string, unknown>;
	if (typeof row.name !== "string" || row.name === "") {
		return bad(`faces[${index}] has no name`);
	}
	if (typeof row.sha256 !== "string") {
		return bad(`'${row.name}' has no sha256`);
	}
	// A v1 document has no `url`, and it is refused here rather than later. This
	// is the branch that turns a stale manifest into a clear message instead of
	// a silently-reconstructed path; the hint on `bad` names the format.
	if (typeof row.url !== "string") {
		return bad(`'${row.name}' has no url`);
	}
	// serde's `u32` refuses a negative, a fractional, and anything above the
	// ceiling — all three are parse failures, not size failures.
	for (const key of ["transfer_size", "raw_size"] as const) {
		const size = row[key];
		if (typeof size !== "number" || !Number.isInteger(size) || size < 0 || size > U32_MAX) {
			return bad(`'${row.name}' has a non-u32 ${key}`);
		}
	}
	return {
		name: row.name,
		transfer_size: row.transfer_size as number,
		raw_size: row.raw_size as number,
		sha256: row.sha256,
		url: row.url,
	};
}

/** The per-face rules, in the Rust validator's order. */
function validate(faces: readonly FallbackFace[]): void {
	const seen = new Set<string>();
	for (const f of faces) {
		if (seen.has(f.name)) {
			// Two entries for one name are two claims about what the font is. The
			// reader would have to pick a winner, so neither is served.
			throw new FallbackManifestError(
				"fallback-manifest-duplicate",
				`fallback manifest lists ${f.name} twice`,
			);
		}
		seen.add(f.name);
		// 64 lowercase hex, because that is what the guest's `parse_digest`
		// accepts. See the note on `parseFallbackManifest`.
		if (!/^[0-9a-f]{64}$/.test(f.sha256)) {
			throw new FallbackManifestError(
				"fallback-manifest-digest",
				`fallback manifest digest for ${f.name} is not 64 lowercase hex chars`,
			);
		}
		// A zero size means the budget for that face is fiction, so the entry is
		// rejected rather than served as a free download.
		if (f.raw_size === 0 || f.transfer_size === 0) {
			throw new FallbackManifestError(
				"fallback-manifest-size",
				`fallback manifest has a zero size for ${f.name}`,
			);
		}
		// Compression made it bigger, which means the face was probably never
		// compressed and the transfer budget does not describe the transfer.
		if (f.transfer_size > f.raw_size) {
			throw new FallbackManifestError(
				"fallback-manifest-transfer-larger",
				`fallback manifest says ${f.name} transfers larger than it stores`,
			);
		}
		// Last, so the cheaper "this face is not what it says it is" rules still
		// report first for a face that is wrong in several ways — the Rust loop's
		// order, for the reason the reader's doc comment gives.
		const why = urlProblem(f.url);
		if (why !== null) {
			throw new FallbackManifestError(
				"fallback-manifest-url",
				`fallback manifest url for ${f.name} is unusable: ${why}`,
			);
		}
	}
}

/**
 * Why a `url` cannot be used, or `null` if it can.
 *
 * The rule is one sentence: a face's `url` is a **plain relative path** to a
 * brotli-compressed font. Everything refused here is a way of being something
 * other than that.
 *
 * ## Why the string is checked raw, not decoded
 *
 * A `..` check that ran on the *decoded* path would be defeated by the same
 * path written `%2e%2e`, which is what an attacker writes when a raw check is in
 * the way — the URL layer decodes it and the fetcher walks up a directory. So
 * the traversal test decodes the one escape that can spell a dot and asks what
 * the segment would *become*, while the empty-segment and encoded-separator
 * tests look at the characters actually present. Checking only one of the two
 * forms is how a validator ends up rejecting `%2e%2e` and admitting `..`, or
 * the reverse.
 *
 * Mirrors `fallback_manifest::url_problem` rule for rule, and returns the
 * *same* reason strings, so a manifest refused here and a manifest refused
 * there fail with one message.
 */
function urlProblem(url: string): string | null {
	if (url === "") {
		return "it is empty";
	}
	// A backslash is a separator to some URL layers and a literal character to
	// others, so a path containing one means two different things depending on
	// who reads it. There is no legitimate face name that needs it.
	if (url.includes("\\")) {
		return "it contains a backslash";
	}
	// `//host/path` is a *network-path reference*: it inherits the scheme and
	// points at another host, which is the one thing a relative field must not
	// be able to do.
	if (url.startsWith("//")) {
		return "it is protocol-relative and would leave the payload origin";
	}
	if (hasScheme(url)) {
		return "it is absolute rather than relative to the manifest base";
	}
	if (!url.endsWith(FALLBACK_FACE_SUFFIX)) {
		return "it does not end in the suffix the transport decodes";
	}
	for (const seg of url.split("/")) {
		if (seg === "") {
			return "it has an empty path segment";
		}
		if (isDotSegment(seg)) {
			return "it has a `.` or `..` path segment";
		}
		if (hasEncodedSeparator(seg)) {
			return "it hides a path separator inside a percent-escape";
		}
	}
	return null;
}

/**
 * Does this path start with something that reads as a URL scheme?
 *
 * `fallback/x.ttf.br` must not be mistaken for one: the first character is a
 * letter, but the run stops at the `/` without reaching a `:`, so it is a
 * relative path. Written as a scan rather than a split so the answer does not
 * depend on a library's URL parser and its idea of what is special.
 */
function hasScheme(url: string): boolean {
	if (!/^[A-Za-z]/.test(url)) {
		return false;
	}
	for (const c of url.slice(1)) {
		if (c === ":") {
			return true;
		}
		if (!/[A-Za-z0-9+\-.]/.test(c)) {
			return false;
		}
	}
	return false;
}

/** Is this segment `.` or `..`, written either plainly or percent-encoded? */
function isDotSegment(seg: string): boolean {
	const decoded = decodeDotEscapes(seg);
	return decoded === "." || decoded === "..";
}

/**
 * Percent-decode **only** the escape that can spell a dot (`%2e`, either case),
 * leaving every other escape exactly as written.
 *
 * Deliberately not a general decoder: decoding `%25` first would let
 * `%252e%252e` become `%2e%2e` and then be mistaken for a real dot, and a
 * decoder that handled the full grammar would be a second URL parser to keep in
 * step with the first. Face names are ASCII identifiers, so the only escapes
 * that can appear are the ones a generator would never write — which is exactly
 * why they have to be understood to be refused.
 */
function decodeDotEscapes(s: string): string {
	let out = "";
	for (let i = 0; i < s.length; i += 1) {
		const c = s[i] as string;
		if (c !== "%") {
			out += c;
			continue;
		}
		const hex = s.slice(i + 1, i + 3);
		if (hex.length === 2 && hex.toLowerCase() === "2e") {
			out += ".";
			i += 2;
		} else {
			out += "%";
		}
	}
	return out;
}

/**
 * Does this segment percent-encode a `/` or a `\`, the two separators?
 *
 * A `%2f` inside a segment is a separator the segment check cannot see, which
 * is the same smuggling trick as `%2e%2e` one level up.
 */
function hasEncodedSeparator(seg: string): boolean {
	const lower = seg.toLowerCase();
	return lower.includes("%2f") || lower.includes("%5c");
}

/**
 * Every face in the manifest as a `fallbackOpen` claim, against a payload base.
 *
 * The claim's `url` is **the manifest's own value**, joined to `base`. Nothing
 * here derives a path from the face name: the generator recorded where it
 * published the file, and that is the only thing that says so. Rebuilding
 * `fallback/<name>.ttf.br` is precisely what made a build publishing the
 * payload elsewhere fetch nothing, in silence.
 *
 * The claim's `rawBytes` and `sha256` are the **decompressed** font's, which is
 * what the guest verifies; the file on the wire is the brotli-compressed one and
 * `LazyByteSource` is responsible for handing back the decompressed bytes.
 * Getting that backwards is a digest mismatch the guest will (correctly) count
 * as a failed attempt.
 *
 * `base` is joined by hand rather than with `new URL`, so a caller that ships
 * from a `chrome-extension://` or `moz-extension://` origin is not forced
 * through a constructor that only speaks `http`.
 */
export function fallbackClaims(
	manifest: FallbackManifest,
	base: string,
): readonly ClaimBody<string>[] {
	return manifest.faces.map((f) => ({
		id: f.name,
		sha256: f.sha256,
		rawBytes: f.raw_size,
		url: joinBase(base, f.url),
	}));
}

/** Join a base and a relative path with exactly one separator between them. */
function joinBase(base: string, path: string): string {
	if (base === "") return path;
	return base.endsWith("/") ? `${base}${path}` : `${base}/${path}`;
}
