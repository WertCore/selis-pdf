/**
 * The `selis-fallback/1` manifest: the lazily-fetched fallback faces.
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

/** Format version, matching `fallback_manifest::MANIFEST_VERSION`. */
export const FALLBACK_MANIFEST_VERSION = 1;

/** The manifest's media type, as served. */
export const FALLBACK_MANIFEST_MIME = "application/vnd.selis.fallback+json";

/**
 * The compressed file's extension, as `xtask fallback-assets` writes it:
 * `<out>/fallback/<name>.ttf.br`.
 *
 * The manifest deliberately carries no URL — see {@link parseFallbackManifest}
 * for why that is a hole worth reporting rather than a design.
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
	| "fallback-manifest-transfer-larger";

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
 * ## One place this is deliberately stricter than the Rust validator
 *
 * Rust checks the digest with `is_ascii_hexdigit`, which **accepts uppercase**.
 * The guest does not: `cjkchunk::parse_digest` matches `0-9a-f` only, so an
 * uppercase digest produces a claim that `FallbackFaceLoader::new` refuses at
 * `fallbackOpen` — a manifest the Rust reader accepts and the engine rejects.
 * This reader is strict, matching the guest (and `parseCjkManifest`'s existing
 * rule), because the alternative is a parser that hands back claims guaranteed
 * to be thrown away. `fallback-manifest.test.ts` pins the behaviour either way.
 *
 * ## The URL is not in here, and that is a real gap
 *
 * A `fallbackOpen` claim needs `{ name, sha256, rawBytes, url }` and this
 * manifest has no `url` field — the Rust `FallbackFace` does not have one
 * either, although `fallbackchunk::FaceClaim` documents one. The shell has to
 * reconstruct it from the generator's naming convention, which is pinned by
 * {@link fallbackFacePath} rather than read from the manifest. A build that
 * published faces anywhere else would silently fetch nothing.
 */
export function parseFallbackManifest(value: unknown): FallbackManifest {
	const bad = (why: string): never => {
		throw new FallbackManifestError(
			"fallback-manifest-invalid",
			`fallback manifest rejected: ${why}`,
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
	}
}

/**
 * The path the generator published one face's compressed file at, relative to
 * the payload root: `fallback/<name>.ttf.br`.
 *
 * Present because {@link parseFallbackManifest} cannot supply it — the manifest
 * has no URL field. Pinned here so a build that publishes the payload somewhere
 * else has exactly one line to change, and so the convention the Rust
 * `FaceClaim` documents is asserted in a test rather than assumed.
 */
export function fallbackFacePath(name: string): string {
	return `fallback/${name}${FALLBACK_FACE_SUFFIX}`;
}

/**
 * Every face in the manifest as a `fallbackOpen` claim, against a payload base.
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
		url: joinBase(base, fallbackFacePath(f.name)),
	}));
}

/** Join a base and a relative path with exactly one separator between them. */
function joinBase(base: string, path: string): string {
	if (base === "") return path;
	return base.endsWith("/") ? `${base}${path}` : `${base}/${path}`;
}
