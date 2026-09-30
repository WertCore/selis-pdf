import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";
import {
	FALLBACK_FACE_SUFFIX,
	FALLBACK_MANIFEST_MIME,
	FALLBACK_MANIFEST_VERSION,
	FallbackManifestError,
	fallbackClaims,
	parseFallbackManifest,
} from "./fallback-manifest.js";
import {
	type ClaimBody,
	FALLBACK_DESCRIPTOR,
	LazyPayloadClient,
	type LazyTransport,
	type PayloadDelivery,
} from "./lazy-payload.js";

/**
 * One valid manifest row, so a test can break exactly the field it names.
 *
 * The `url` is the generator's own value. A test that wants a different layout
 * overrides it, because that is the point: the reader takes the path from here
 * and nowhere else.
 */
function face(name = "LiberationSans-Regular"): Record<string, unknown> {
	return {
		name,
		transfer_size: 78454,
		raw_size: 139512,
		sha256: "784836c044d2f6a515b7e08f2c8d2a0317afb2b83d8471d1b02f6e0bcb7b14c1",
		url: `fallback/${name}.ttf.br`,
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

/**
 * The prose of a refusal, for the cases where the *reason* is the thing under
 * test — a url refused for being absolute is a different bug from one refused
 * for smuggling a separator through a percent-escape, and both carry the same
 * code.
 */
function messageOf(run: () => unknown): string {
	try {
		run();
	} catch (e) {
		if (e instanceof FallbackManifestError) return e.message;
		throw e;
	}
	throw new Error("expected a FallbackManifestError, nothing was thrown");
}

describe("SL-3.FONT.12 — the selis-fallback/2 manifest reader", () => {
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
	 * than partially read — a v3 manifest that happens to keep `faces` would
	 * otherwise be served with this build's meaning.
	 *
	 * **Both directions matter now that the version moved.** Version 2 is this
	 * build's, and version 1 is handled by its own test below because it fails
	 * earlier, on the missing `url`. A reader that had simply bumped the
	 * constant without noticing would start refusing real manifests; one that
	 * kept 1 would start accepting manifests it cannot read.
	 */
	it("refuses a version this build does not know", () => {
		expect(codeOf(() => parseFallbackManifest({ ...manifest(), version: 3 }))).toBe(
			"fallback-manifest-version",
		);
		expect(codeOf(() => parseFallbackManifest({ ...manifest(), version: 99 }))).toBe(
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
	 * **A `selis-fallback/1` document is refused, not reconstructed.**
	 *
	 * Version 1 has no `url`, and the failure lands as a *parse* refusal — which
	 * is exactly where the Rust reader puts it, because serde fails to
	 * deserialise a face that is missing a field before `validate()` ever runs.
	 * The message must therefore name the missing field and the format this
	 * build reads: "has no url" is true and explains nothing about which
	 * documents are loadable, and a reader that quietly filled the field in
	 * would reintroduce the silent-404 hole `url` was added to close.
	 *
	 * Mirrors `an_old_format_manifest_is_refused_not_reconstructed`.
	 */
	it("refuses a v1 manifest with a message naming the field and the format", () => {
		const v1 = {
			version: 1,
			faces: [
				{
					name: "LiberationSans-Regular",
					transfer_size: 78454,
					raw_size: 139512,
					sha256: "784836c044d2f6a515b7e08f2c8d2a0317afb2b83d8471d1b02f6e0bcb7b14c1",
				},
			],
		};
		expect(codeOf(() => parseFallbackManifest(v1))).toBe("fallback-manifest-invalid");
		let message = "";
		try {
			parseFallbackManifest(v1);
		} catch (e) {
			message = (e as Error).message;
		}
		expect(message).toContain("url");
		expect(message).toContain(`selis-fallback/${FALLBACK_MANIFEST_VERSION}`);
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
	 * **The digest rule both readers now share, and why it must not drift.**
	 * The guest's `cjkchunk::parse_digest` matches `0-9a-f` only, so an uppercase
	 * digest yields a claim `FallbackFaceLoader::new` refuses at `fallbackOpen` —
	 * the error surfacing in a browser, far from a build log that said the
	 * manifest was fine. The TypeScript reader has always been strict here; the
	 * Rust validator used `is_ascii_hexdigit`, which also admits A-F, and has been
	 * tightened to match. Both sides now pin the rule, so neither can drift back.
	 */
	it("refuses an uppercase digest, which the guest's digest parser also refuses", () => {
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
		expect(FALLBACK_MANIFEST_VERSION).toBe(2);
		expect(FALLBACK_MANIFEST_MIME).toBe("application/vnd.selis.fallback+json");
	});

	/**
	 * The suffix is what the transport decodes, and both readers require it —
	 * a manifest must not be able to claim a payload the shell would unpack
	 * wrongly. Pinned here because the value is duplicated across the language
	 * boundary rather than shared, and duplication needs a test.
	 */
	it("pins the suffix the Rust generator writes", () => {
		expect(FALLBACK_FACE_SUFFIX).toBe(".ttf.br");
	});

	/**
	 * A `url` is only useful if it is a plain relative path. Every other shape
	 * is refused, and the reason is carried alongside the face so the failure
	 * says which entry was wrong and how.
	 *
	 * The list is exhaustive over the ways a relative path can stop being one:
	 * it can name another origin (absolute, protocol-relative), it can leave the
	 * payload directory (`..`), it can mean two different things to two readers
	 * (backslash, encoded separator), and it can name a file the transport would
	 * decode wrongly (wrong suffix). Mirrors
	 * `a_url_that_is_not_a_plain_relative_path_is_refused` case for case.
	 */
	it("refuses a url that is not a plain relative path", () => {
		const cases: readonly (readonly [string, string])[] = [
			// Absolute, in every spelling a generator or an attacker might use.
			["https://cdn.example.test/fallback/x.ttf.br", "absolute"],
			["http://cdn.example.test/x.ttf.br", "absolute"],
			["file:///etc/passwd.ttf.br", "absolute"],
			["HTTPS://CDN.EXAMPLE.TEST/x.ttf.br", "absolute"],
			["data:font/ttf;base64,AAAA.ttf.br", "absolute"],
			// Inherits the scheme and names a host: the same escape, quieter.
			["//cdn.example.test/fallback/x.ttf.br", "protocol-relative"],
			// Traversal, plainly and percent-encoded. The encoded forms are the
			// ones that matter: a check that decoded first would let `%2e%2e`
			// through a `..` test and then walk up a directory.
			["../x.ttf.br", "`..`"],
			["fallback/../../x.ttf.br", "`..`"],
			["fallback/%2e%2e/x.ttf.br", "`..`"],
			["fallback/%2E%2E/x.ttf.br", "`..`"],
			["fallback/%2e./x.ttf.br", "`..`"],
			["./x.ttf.br", "`..`"],
			// A backslash is a separator to some URL layers and a literal to
			// others, so the path means two different files depending on who
			// fetches it.
			["fallback\\x.ttf.br", "backslash"],
			["..\\..\\x.ttf.br", "backslash"],
			// Empty segments: a leading slash, a doubled one, a trailing one.
			["/fallback/x.ttf.br", "empty path segment"],
			["fallback//x.ttf.br", "empty path segment"],
			["fallback/", "suffix"],
			// A separator smuggled through an escape the segment split cannot
			// see — the same trick as `%2e%2e`, one level up.
			["fallback%2fx.ttf.br", "percent-escape"],
			["fallback%2Fx.ttf.br", "percent-escape"],
			["fallback%5cx.ttf.br", "percent-escape"],
			// The transport decodes brotli-compressed TrueType; anything else is
			// a claim about bytes nobody published that way. Case counts.
			["fallback/x.ttf", "suffix"],
			["fallback/x.TTF.BR", "suffix"],
			["fallback/x.ttf.gz", "suffix"],
			["", "empty"],
		];
		for (const [url, why] of cases) {
			const row = { ...face(), url };
			expect(codeOf(() => parseFallbackManifest(manifest([row])))).toBe("fallback-manifest-url");
			// The reason is asserted too, because a reader that refused every
			// url for the same uninformative reason would pass the code check.
			expect(messageOf(() => parseFallbackManifest(manifest([row])))).toContain(why);
		}
	});

	/**
	 * The list of refused shapes must not quietly become the list of *accepted*
	 * ones: a validator that refuses everything passes the test above. These are
	 * the shapes a generator may legitimately emit — **including a layout that
	 * is not the one the shell used to reconstruct**, which is the whole reason
	 * the manifest carries the path. Mirrors
	 * `a_plain_relative_url_is_accepted_whatever_the_layout`.
	 */
	it("accepts a plain relative url whatever the layout", () => {
		for (const url of [
			"fallback/LiberationSans-Regular.ttf.br",
			"LiberationSans-Regular.ttf.br",
			"v2/fallback/LiberationSans-Regular.ttf.br",
			"custom/place/face.ttf.br",
			// A percent-escape that cannot spell a dot or a separator is left
			// alone rather than refused: this reader is not a second URL parser.
			"fallback/LiberationSans%20Regular.ttf.br",
		]) {
			const row = { ...face(), url };
			expect(parseFallbackManifest(manifest([row])).faces[0]?.url).toBe(url);
		}
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

	/**
	 * **The test that says the reconstruction is gone.**
	 *
	 * Everything above exercises the reader in isolation. This one runs the whole
	 * path a real fetch takes — manifest to claims, claims into a
	 * {@link LazyPayloadClient}, client to the {@link LazyByteSource} that moves
	 * the bytes — with a manifest whose face is published somewhere the old
	 * convention would never have guessed, and asserts the source was asked for
	 * *exactly* that path.
	 *
	 * It is deliberately not a unit test of `joinBase`. The bug being closed was
	 * not a bad join; it was a good join onto a path that had been made up. The
	 * only place that can be observed is the url the byte source is finally
	 * handed, so that is what is asserted.
	 */
	it("fetches from the manifest's own url, never a reconstructed one", async () => {
		const parsed = parseFallbackManifest(
			manifest([{ ...face(), url: "custom/place/face.ttf.br" }]),
		);
		const claims = fallbackClaims(parsed, "https://cdn.example.test/payload");
		const claim = claims[0] as ClaimBody<string>;
		const source = recordingSource();
		const client = new LazyPayloadClient(FALLBACK_DESCRIPTOR, silentGuest(), source);

		await client.open(1, claims);
		await client.serve(1, claim);

		expect(source.urls).toEqual(["https://cdn.example.test/payload/custom/place/face.ttf.br"]);
		// Spelled out as its own assertion because the failure it guards against
		// is silent: a source pointed at the wrong path simply 404s.
		expect(source.urls[0]).not.toContain("fallback/LiberationSans-Regular.ttf.br");
	});

	// --- the committed artifact --------------------------------------------

	/**
	 * The state a fallback payload reports: nothing resident, nothing wanted.
	 *
	 * `request: null` is the guest's own signal that it wants nothing yet, and it
	 * is load-bearing — the client reads an absent key as a malformed response.
	 */
	function guestState(): Record<string, unknown> {
		return {
			revision: 0,
			residentBytes: 0,
			loaded: [],
			needs: [],
			unavailable: [],
			exhausted: [],
			request: null,
		};
	}

	/** A guest that opens, adopts the delivery, and asks for nothing further. */
	function silentGuest(): LazyTransport {
		return {
			async call() {
				return { ...guestState(), adopted: true };
			},
		};
	}

	/**
	 * A byte source that records the url it was handed, verbatim.
	 *
	 * Recording rather than serving is the point: what this test reads is the
	 * address the client *chose*, before any transport could normalise or
	 * redirect it, which is the only place a reconstructed path is still visible.
	 */
	function recordingSource(): {
		load: (request: ClaimBody<string>, key: string) => Promise<PayloadDelivery>;
		urls: string[];
	} {
		const urls: string[] = [];
		return {
			urls,
			async load(request) {
				urls.push(request.url);
				return { status: 200, bytes: new Uint8Array([1, 2, 3]) };
			},
		};
	}

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
		// The committed urls are the *generator's* paths. This is no longer a
		// convention the reader assumes — it is data, and it is the one thing a
		// rename in `fallback_assets.rs` would change without any code change
		// here. Asserting the shape keeps the two ends talking about the same
		// directory, and `cargo xtask fallback-assets --check` is the gate that
		// actually keeps them equal.
		for (const f of parsed.faces) {
			expect(f.url).toBe(`fallback/${f.name}${FALLBACK_FACE_SUFFIX}`);
		}
	});
});
