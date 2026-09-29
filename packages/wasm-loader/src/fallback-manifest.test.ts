import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";
import {
	FALLBACK_FACE_SUFFIX,
	FALLBACK_MANIFEST_MIME,
	FALLBACK_MANIFEST_VERSION,
	FallbackManifestError,
	fallbackClaims,
	fallbackFacePath,
	parseFallbackManifest,
} from "./fallback-manifest.js";

/** One valid manifest row, so a test can break exactly the field it names. */
function face(name = "LiberationSans-Regular"): Record<string, unknown> {
	return {
		name,
		transfer_size: 78454,
		raw_size: 139512,
		sha256: "784836c044d2f6a515b7e08f2c8d2a0317afb2b83d8471d1b02f6e0bcb7b14c1",
	};
}

/** A valid manifest, so a test can break exactly the thing it is about. */
function manifest(faces: Record<string, unknown>[] = [face()]): Record<string, unknown> {
	return { version: FALLBACK_MANIFEST_VERSION, faces };
}

/** The code a refusal carries, so a test asserts the reason and not the prose. */
function codeOf(run: () => unknown): string {
	try {
		run();
	} catch (e) {
		if (e instanceof FallbackManifestError) return e.code;
		throw e;
	}
	throw new Error("expected a FallbackManifestError, nothing was thrown");
}

describe("SL-3.FONT.12 — the selis-fallback/1 manifest reader", () => {
	/**
	 * A reader that accepts the format the generator writes. Everything else in
	 * this file is a refusal, so a reader that refused everything would pass
	 * most of them — this is the test that says it is a reader and not a wall.
	 */
	it("accepts a well-formed manifest and reports version and faces", () => {
		const parsed = parseFallbackManifest(manifest());
		expect(parsed.version).toBe(FALLBACK_MANIFEST_VERSION);
		expect(parsed.faces).toHaveLength(1);
		expect(parsed.faces[0]?.name).toBe("LiberationSans-Regular");
		expect(parsed.faces[0]?.raw_size).toBe(139512);
	});

	/**
	 * An empty face list is a *valid* manifest — this build may embed every face
	 * it has. The Rust test only requires the committed one to be non-empty; the
	 * format itself permits zero, and refusing it would make a fully-embedded
	 * build unopenable.
	 */
	it("accepts a manifest with no faces at all", () => {
		expect(parseFallbackManifest(manifest([])).faces).toEqual([]);
	});

	/**
	 * The version is the schema gate, and an unknown version is refused rather
	 * than partially read — a v2 manifest that happens to keep `faces` would
	 * otherwise be served with this build's meaning.
	 */
	it("refuses a version this build does not know", () => {
		expect(codeOf(() => parseFallbackManifest({ ...manifest(), version: 2 }))).toBe(
			"fallback-manifest-version",
		);
		expect(codeOf(() => parseFallbackManifest({ ...manifest(), version: 0 }))).toBe(
			"fallback-manifest-version",
		);
		// A missing version is a version refusal, not a shape one: it is the
		// value that is wrong.
		expect(codeOf(() => parseFallbackManifest({ faces: [face()] }))).toBe(
			"fallback-manifest-version",
		);
	});

	/**
	 * Two entries for one name are two claims about what the font is, and the
	 * reader would have to pick a winner — so neither is served. Mirrors
	 * `a_duplicate_face_is_refused`.
	 */
	it("refuses a manifest listing the same face twice", () => {
		expect(codeOf(() => parseFallbackManifest(manifest([face(), face()])))).toBe(
			"fallback-manifest-duplicate",
		);
	});

	/**
	 * A digest that is not 64 lowercase hex cannot be checked, and a claim the
	 * loader cannot verify is one it refuses — which is the whole point of
	 * checking. Mirrors `a_malformed_digest_is_refused`, including the
	 * `z`.repeat(64) case the Rust test spells out.
	 */
	it("refuses a digest that is not 64 hex characters", () => {
		for (const bad of ["", "abc", "z".repeat(64), "a".repeat(63), "a".repeat(65)]) {
			const row = { ...face(), sha256: bad };
			expect(codeOf(() => parseFallbackManifest(manifest([row])))).toBe("fallback-manifest-digest");
		}
	});

	/**
	 * **The one place this reader is deliberately stricter than the Rust
	 * validator, and the reason matters.** `fallback_manifest.rs` checks the
	 * digest with `is_ascii_hexdigit`, which accepts uppercase; the guest does
	 * not — `cjkchunk::parse_digest` matches `0-9a-f` only, so an uppercase
	 * digest yields a claim `FallbackFaceLoader::new` refuses at `fallbackOpen`.
	 * The Rust reader therefore accepts a manifest the engine rejects. This
	 * reader refuses it too, because the alternative is a parser that hands
	 * back claims guaranteed to be thrown away.
	 */
	it("refuses an uppercase digest, which the Rust validator would accept and the guest would not", () => {
		const upper = String(face().sha256).toUpperCase();
		expect(codeOf(() => parseFallbackManifest(manifest([{ ...face(), sha256: upper }])))).toBe(
			"fallback-manifest-digest",
		);
		// The lowercase form of the very same digest is fine, so the test is
		// about case and not about that particular value.
		const lower = parseFallbackManifest(manifest([{ ...face(), sha256: upper.toLowerCase() }]));
		expect(lower.faces).toHaveLength(1);
	});

	/**
	 * A zero size means the budget for that face is fiction, so the entry is
	 * rejected rather than served as a free download. Both columns, matching
	 * `if f.raw_size == 0 || f.transfer_size == 0`.
	 */
	it("refuses a zero size on either column", () => {
		expect(codeOf(() => parseFallbackManifest(manifest([{ ...face(), raw_size: 0 }])))).toBe(
			"fallback-manifest-size",
		);
		expect(codeOf(() => parseFallbackManifest(manifest([{ ...face(), transfer_size: 0 }])))).toBe(
			"fallback-manifest-size",
		);
	});

	/**
	 * Transfer larger than raw means the face was probably never compressed, so
	 * the transfer budget does not describe the transfer. Mirrors
	 * `a_transfer_larger_than_its_source_is_refused`; equality is allowed,
	 * because the Rust check is strictly `>`.
	 */
	it("refuses a face that transfers larger than it stores, but allows equality", () => {
		const bigger = manifest([{ ...face(), raw_size: 200, transfer_size: 201 }]);
		expect(codeOf(() => parseFallbackManifest(bigger))).toBe("fallback-manifest-transfer-larger");
		const equal = manifest([{ ...face(), raw_size: 200, transfer_size: 200 }]);
		expect(parseFallbackManifest(equal).faces).toHaveLength(1);
	});

	/**
	 * Garbage in, a typed error out — never a thrown `TypeError` from reading a
	 * property of `null`. This parses bytes from the network, and an unhandled
	 * crash there is a denial of service. Mirrors
	 * `malformed_input_is_an_error_not_a_panic`.
	 */
	it("refuses malformed input with a typed reason, never a crash", () => {
		for (const bad of [null, "not json", 42, [], { faces: "not an array" }]) {
			expect(codeOf(() => parseFallbackManifest(bad))).toBe("fallback-manifest-invalid");
		}
	});

	/**
	 * A row missing a required column is a *parse* error, not a per-face rule
	 * failure, and it is reported before the version is even read. That is
	 * serde's order — the whole document is deserialised before `validate()`
	 * runs — and reordering it would make two readers of one bad manifest give
	 * different reasons for the same refusal, which is how a real bug gets
	 * argued away as a message mismatch.
	 */
	it("reports a malformed row as invalid before it reports a bad version", () => {
		expect(codeOf(() => parseFallbackManifest({ version: 99, faces: [{ name: "X" }] }))).toBe(
			"fallback-manifest-invalid",
		);
	});

	/**
	 * A size outside serde's `u32` is a parse failure, not a size failure: a
	 * negative, a fractional and a value past the ceiling are all refused
	 * before the per-face rules run, so the reason names the real problem.
	 */
	it("refuses a size that is not a u32", () => {
		for (const bad of [-1, 1.5, 0x1_0000_0000]) {
			expect(codeOf(() => parseFallbackManifest(manifest([{ ...face(), raw_size: bad }])))).toBe(
				"fallback-manifest-invalid",
			);
		}
	});

	/**
	 * An empty face name cannot be a key the engine looks up, and admitting one
	 * would produce a claim whose identity no `substitute` could ever produce.
	 */
	it("refuses a row with an empty name", () => {
		expect(codeOf(() => parseFallbackManifest(manifest([{ ...face(), name: "" }])))).toBe(
			"fallback-manifest-invalid",
		);
	});

	/**
	 * The refusal is an `Error` carrying a code, so a caller can branch on it
	 * rather than parse prose — the same error style as `parseCjkManifest`'s
	 * `CjkPayloadError`.
	 */
	it("throws a typed error carrying a code, not a bare string", () => {
		try {
			parseFallbackManifest(manifest([{ ...face(), raw_size: 0 }]));
			throw new Error("expected a refusal");
		} catch (e) {
			expect(e).toBeInstanceOf(FallbackManifestError);
			expect(e).toBeInstanceOf(Error);
			expect((e as FallbackManifestError).code).toBe("fallback-manifest-size");
		}
	});

	/**
	 * **Why refusing whole beats skipping the bad entry**, asserted rather than
	 * only argued in a comment: the alternative — serve the two good faces and
	 * drop the third — renders a document with two of its three fallbacks while
	 * looking perfectly healthy, and the text that needed the third comes out
	 * `.notdef` with nothing reporting an error. A manifest naming one face
	 * with a malformed digest is evidence the *manifest* is wrong, and every
	 * caller of a bad manifest has the same reaction, which is to load nothing.
	 */
	it("refuses the whole manifest rather than skipping the one bad entry", () => {
		const rows = [
			face("LiberationSans-Regular"),
			face("LiberationMono-Bold"),
			{ ...face("LiberationSerif-Italic"), sha256: "nope" },
		];
		expect(codeOf(() => parseFallbackManifest(manifest(rows)))).toBe("fallback-manifest-digest");
		// Not "the other two, with a warning": the two valid faces are not
		// reachable either, which is what makes the silent `.notdef` page
		// impossible.
		expect(() => parseFallbackManifest(manifest(rows))).toThrow(FallbackManifestError);
	});

	// --- the constants the wire and the build agree on ----------------------

	/**
	 * The version and media type are constants both readers must agree on; a
	 * drift here is a manifest served as `application/json` and read by a
	 * parser expecting a different version.
	 */
	it("pins the version and media type the Rust module pins", () => {
		expect(FALLBACK_MANIFEST_VERSION).toBe(1);
		expect(FALLBACK_MANIFEST_MIME).toBe("application/vnd.selis.fallback+json");
	});

	/**
	 * The manifest carries no URL, so the path comes from the generator's naming
	 * convention — pinned here rather than assumed, because a build publishing
	 * the payload elsewhere would otherwise silently fetch nothing.
	 */
	it("derives the face path from the generator's convention", () => {
		expect(FALLBACK_FACE_SUFFIX).toBe(".ttf.br");
		expect(fallbackFacePath("LiberationSerif-Regular")).toBe(
			"fallback/LiberationSerif-Regular.ttf.br",
		);
	});

	/**
	 * The claim's `rawBytes` is the **decompressed** size, because that is the
	 * length the guest compares the delivered bytes against. Getting it backwards
	 * is a mismatch the guest will (correctly) count as a failed attempt — so
	 * `raw_size`, never `transfer_size`, is what goes on the wire.
	 */
	it("builds claims from the decompressed size and digest, not the transfer size", () => {
		const parsed = parseFallbackManifest(manifest());
		const [claim] = fallbackClaims(parsed, "https://cdn.example.test/payload");
		expect(claim?.id).toBe("LiberationSans-Regular");
		expect(claim?.rawBytes).toBe(139512);
		expect(claim?.sha256).toBe("784836c044d2f6a515b7e08f2c8d2a0317afb2b83d8471d1b02f6e0bcb7b14c1");
		expect(claim?.url).toBe(
			"https://cdn.example.test/payload/fallback/LiberationSans-Regular.ttf.br",
		);
	});

	/**
	 * The base is joined by hand rather than with `new URL`, so a caller shipping
	 * from a `chrome-extension://` or `moz-extension://` origin is not forced
	 * through a constructor that only speaks `http`. Pinned on both a trailing
	 * and a missing slash, because a doubled separator is a 404 and a missing
	 * one is a 404 too.
	 */
	it("joins a base and a face path with exactly one separator", () => {
		const parsed = parseFallbackManifest(manifest());
		const withSlash = fallbackClaims(parsed, "https://x.test/p/")[0]?.url;
		const withoutSlash = fallbackClaims(parsed, "https://x.test/p")[0]?.url;
		expect(withSlash).toBe("https://x.test/p/fallback/LiberationSans-Regular.ttf.br");
		expect(withoutSlash).toBe("https://x.test/p/fallback/LiberationSans-Regular.ttf.br");
	});

	// --- the committed artifact --------------------------------------------

	/**
	 * The committed manifest must satisfy the reader the runtime trusts it with.
	 * This is the TypeScript half of the Rust test
	 * `the_committed_manifest_is_valid`, and it is the test that catches a
	 * hand-edited or stale committed manifest — which no amount of runtime
	 * validation would notice, because the runtime is what trusts it.
	 */
	it("the committed manifest is valid, non-empty, and turns into claims", () => {
		const raw = readFileSync(
			new URL("../../../assets/payload/fallback/manifest.json", import.meta.url),
			"utf8",
		);
		const parsed = parseFallbackManifest(JSON.parse(raw));
		expect(parsed.faces.length).toBeGreaterThan(0);
		// And every face turns into a claim, so a path convention that has
		// drifted from the generator's would surface here too.
		expect(fallbackClaims(parsed, "")).toHaveLength(parsed.faces.length);
	});
});
