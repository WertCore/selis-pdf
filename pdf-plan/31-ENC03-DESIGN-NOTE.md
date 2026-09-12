# 31 — SL-1.ENC.03 design note: the public-key (PKCS#7) security handler, read side

> **Status:** DRAFT — awaiting HUMAN sign-off (the task is `owner: HUMAN`; this draft was
> prepared by the agent and is **not** self-approved).
> **Scope:** read-only decryption of `/Adobe.PPKLite` (and the `/Adobe.PubSec` alias)
> documents per ISO 32000-1 §7.6.6 / ISO 32000-2 §7.6.6.4, ADR-P0019. No encrypt path, no
> certificate management UI, no PKCS#12 keystore handling.

---

## 1. What shipped

| Crate | Change |
|---|---|
| `selis-crypto` | New `der` module — a minimal iterative DER reader for the CMS subset (budget-charged, fuzzed). New `pkcs7` module — `ContentInfo`/`EnvelopedData` parsing, RSA + EC CEK unwrap, AES-CBC payload decrypt, the Algorithm-1 seed-hash file-key derivation, `PubKeyCredential`/`PubKeyAuth` API. |
| `selis-pdf-cos` | `Handler` enum on `EncryptInfo`; `parse_encrypt` dispatches `/Adobe.PPKLite` and `/Adobe.PubSec`; `authenticate_public_key`; `stream_encrypted()`/`string_encrypted()` generalised from the `/StdCF` shortcut to named crypt filters (needed for `/DefaultCryptFilter`). |
| `selis-pdf-engine` | `Session::open_public_key(src, credential, budget, clock)` — the full open path with a typed wrong-key error; `Session::open` opens public-key documents tolerantly (no credential) as before. |
| `selis-error` | New code **1807 `RECIPIENT_NO_MATCH`** (Auth, retryable, NotLoaded). |
| `fuzz` | New target `pkcs7_cms` over `parse_enveloped_data`, 25 committed seeds, CI matrix entry. |
| `xtask/layers.toml` | One sideways edge: `selis-crypto → selis-sandbox` (kernel-to-kernel; the CMS parse charges the budget). |
| Tests | 12 unit tests in `selis-crypto` (DER, KW/KDF vectors, RSA/ECDH round-trips, budget), 5 in `selis-pdf-cos` (dispatch shapes, s3 refusal, handler guard), 5 engine DoD tests over 3 committed fixture PDFs. |

## 2. The algorithm as implemented (Algorithm 1, ISO 32000-2 §7.6.6.4)

1. **Parse** the selected `/Recipients` blob as CMS `ContentInfo { id-envelopedData,
   EnvelopedData }` (RFC 5652 §6).
2. **Match + unwrap**: try each `RecipientInfo` under the supplied credential —
   `KeyTransRecipientInfo` (RSAES-PKCS1-v1_5) or `KeyAgreeRecipientInfo` (ECDH
   static-stdDH + SHA-256 X9.63 KDF + AES-KW, RFC 5753 §2.1.1) — to recover the CEK.
3. **Decrypt** the `EncryptedContentInfo`'s content with the CEK (AES-128/192/256-CBC,
   IV from the algorithm parameters, strict PKCS#7 validation). The payload is exactly
   **24 bytes: a 20-byte seed + the recipient's 4 permission bytes**.
4. **Derive** the file encryption key: `HASH(seed ‖ every recipient blob [‖ FFFFFFFF])`
   truncated to `/Length`/8 — SHA-256 for `/V 5` (AESV3), SHA-1 for `/V 4` (AESV2). The
   hash spans all recipients (so no recipient can shorten another's key); `/EncryptMetadata
   false` appends the 4×`0xFF` marker.
5. Per-object stream/string decryption then follows the standard handler's algorithms
   with `/V` as the revision: V4 → salted AES-128 (Algorithm 1), V5 → direct-key AES-256
   (Algorithm 1a) — reusing `selis_crypto::decrypt_data` unchanged.

**A note on the task wording:** the task brief said "decrypt `/O` with the recipient key per
the spec's algorithm 1 §7.6.1". Public-key `/Encrypt` dictionaries have no `/O`/`/U`; the
recipient key unwraps the CEK, which decrypts the **seed** payload, and the file key is the
seed hash of step 4 (the PDFBox reference implementation matches this construction exactly).
The correct spec citations are ISO 32000-1:2008 §7.6.6 / ISO 32000-2:2020 §7.6.6.4 and
§7.6.3.3 Algorithm 1 for per-object keys.

## 3. Dependency decision: hand-rolled DER reader, `rsa` + `p256` for the crypto

The workspace had no DER parser. Both options allowed by the task were evaluated:

| | `der` crate 0.7 (RustCrypto/formats) | hand-rolled minimal reader (chosen) |
|---|---|---|
| Licence | MIT OR Apache-2.0 ✓ | n/a |
| Maintenance | Actively maintained (the substrate of `pkcs8`/`spki`/`x509-cert`) ✓ | ours, one file |
| no_std / wasm | ✓ | ✓ |
| Supply-chain cost | **Zero marginal** — `rsa` and `p256` already pull `der` transitively for PKCS#8 decoding | zero |
| Ergonomics for CMS | `RecipientIdentifier`/`originator` CHOICEs with untagged SEQUENCE variants and EXPLICIT wrappers need manual `Decode` impls anyway; the derive machinery does not buy much for ~10 constructs | full control over exactly the subset |
| Fuzz assurance | upstream-hardened | ours — but iterative (no recursion), `checked_*` lengths, `get()`-only access, dedicated `pkcs7_cms` target |

**Decision:** hand-roll the reader (`selis-crypto/src/der.rs`, ~200 lines, iterative — no
stack risk) and record it here. The CMS subset accepts exactly what §7.6.6 needs (SEQUENCE/
SET/OID/INTEGER/OCTET STRING/BIT STRING/NULL + context tags 0–1) and rejects everything
else — indefinite lengths, the high-tag-number form, non-minimal long lengths, trailing
bytes — as typed `ENCRYPT_MALFORMED`. RSA/EC **private-key** decoding stays inside
`rsa`/`p256` (their own hardened PKCS#8 decoders); hand-rolling RSA was never on the table.

New dependencies (all RustCrypto, MIT OR Apache-2.0, no_std+alloc, MSRV ≤ 1.82, actively
maintained, wasm32-clean — ADR-P0021 compliant):

| Crate | Version | Why |
|---|---|---|
| `rsa` | 0.9 | The only credible pure-Rust RSA implementation (RFC 8017 PKCS#1 v1.5 CEK transport). |
| `p256` | 0.13 | EC P-256 ECDH key agreement (`ecdh`, `pkcs8` features) for the CMS key-agreement path. |
| `sha1` | 0.10 | Algorithm 1's V4 hash is SHA-1 — format-defined, decrypt-only use. |

## 4. Certificate-selection policy: first-match-with-validation, no X.509 parser

**Policy (v1, read-only):** recipients are tried in `/Recipients` array order; the first
blob whose transport unwraps under the supplied credential **and** whose content decrypts
to a structurally valid 24-byte payload wins. The credential is the private key alone
(`PubKeyCredential::Rsa(PKCS#8 DER)` or `PubKeyCredential::EcP256(PKCS#8 DER)`).

**Why no X.509 certificate matching (the spec's issuer/serial or subjectKeyIdentifier
route) is needed for this policy:** wrong-key decryption is already detected structurally —
RSA PKCS#1 v1.5 padding check (fails with p ≈ 1−2⁻¹⁶), then AES-CBC strict padding plus the
exact 24-byte length (combined false-accept < 2⁻⁶⁰), and AES-KW's integrity register
(2⁻⁶⁴) on the ECDH path. Matching RecipientIdentifiers against a parsed X.509 certificate
would add a certificate parser, a trust-store question, and UI without changing any
outcome for a single-credential local decryption. Trying-with-validation also matches how
PDFBox behaves when the caller supplies only a key.

**Configurability (deferred, listed for sign-off):** an explicit
`match_by(certificate | key_id | first_valid)` policy knob only becomes meaningful when we
add X.509 support (signatures work, `SL-*` SIGN area) or multi-credential UI. The try-order
is deterministic (array order), so behaviour is reproducible and testable.

## 5. Deliberately refused (typed `ENCRYPT_UNSUPPORTED`, never silent)

| Refused | Why |
|---|---|
| `adbe.pkcs7.s3` (`/V ≤ 3`, RC4-era) | RC4 content has **no padding**, so a wrong CEK would silently derive a wrong file key — violating the "wrong key yields a typed error" DoD. RC4-era PKI files are museum pieces; gate on evidence if one ever appears. |
| RC4 / 3DES / RC2 as CMS **content** algorithms | Same wrong-key-silence argument for RC4; 3DES/RC2 are deprecated and have no modern RustCrypto RC2 story. Acrobat's s4/s5 writers use AES-CBC. |
| RSA-OAEP key transport | Not produced by Acrobat for PDF; can be added as one match arm if a file demands it. |
| PKCS#12 (.pfx) keystore parsing | Its own ASN.1+PBES2 project; the credential API takes PKCS#8 DER, which every keystore can export. |
| `unprotectedAttrs` | Tolerated and ignored (rarely present). |
| ECDH `ukm` | Tolerated and ignored (unused by static-stdDH schemes). |

## 6. Known limitations & accepted risks (for the human to weigh)

1. **RUSTSEC-2023-0071 (Marvin-style timing side channel in `rsa` decryption)** —
   unfixed upstream. Accepted here because the operation is a *local* decryption of the
   user's own document with the user's own key (no remote oracle surface); this is the same
   posture as every local PDF tool. A server-batch surface (`Surface::Server`) should
   revisit before shipping public-key decryption in a multi-tenant service.
2. **Permission bits are surfaced, not enforced yet.** The per-recipient 4-byte permission
   block (stronger than `/P`) is returned in `PubKeyAuth::permissions` but not wired into
   `selis-policy` — the same honest-surfacing posture SL-1.ENC.04 chose for `/P`. Wiring
   it up touches the policy layer and deserves its own review.
3. **Unknown security handlers still open tolerantly** (pre-existing behaviour, unchanged
   here): content stays garbage rather than the document being refused. If sign-off wants
   `ENCRYPT_UNSUPPORTED` for unknown `/Filter` names, that is a one-line follow-up.
4. **Committed test key material.** Two throwaway RSA-2048 private keys (PKCS#8 DER) live
   under `crates/selis-pdf-engine/tests/fixtures/` purely to build deterministic fixtures;
   they protect nothing and are flagged here per the "never commit secrets" rule (test
   fixtures are the accepted exception, but the human should see them in the diff).
5. **`aes192`-wrap ECDH KEKs** (24-byte KEK) are skipped structurally (wrapped length
   filter); only 128/256-bit KEKs unwrap. Encounter-driven addition if ever seen.
6. **The seed hash depends on the whole `/Recipients` array** (per spec). Re-saving a
   public-key document with a *different* recipient set therefore changes every existing
   recipient's file key — inherent to the format, noted so future write-path work
   (out of scope per ADR-P0019) does not rediscover it the hard way.

## 7. Test matrix

| Layer | Tests |
|---|---|
| `selis-crypto::der` | short/long lengths, indefinite/truncated/high-tag/non-minimal rejection, trailing bytes, zero-length TLVs, budget exhaustion typed, OID arc decode (incl. the X9.63 scheme OID) |
| `selis-crypto::pkcs7` | RFC 3394 §4.1 KW vector; KW tamper rejection; RSA-512 transport round-trip (independent SHA-1 expectation); wrong RSA key → `RECIPIENT_NO_MATCH`; wrong credential type → `RECIPIENT_NO_MATCH`; ECDH+AES-KW round-trip (independent SHA-256 expectation); 3DES/RC4 content → `ENCRYPT_UNSUPPORTED`; negative version → malformed; trailing bytes; budget exhaustion; hostile empty-TLV fan-out terminates; credential Debug redaction |
| `selis-pdf-cos::encrypt` | s5 dispatch (CF recipients, AES flag, stream/string encrypted); s4 dispatch (dict recipients); `/Adobe.PubSec` alias; s3 → `ENCRYPT_UNSUPPORTED`; standard info → refused; `/CFM /None` → not encrypted; standard `authenticate` refuses pubkey infos |
| `selis-pdf-engine` (DoD) | 3 committed fixtures: `pubkey-rsa-s4.pdf`, `pubkey-ec-s5.pdf`, `pubkey-multi-s5.pdf` (EC first, RSA second). RSA opens + content draws; EC opens + content draws; both credentials open the multi fixture (first-match policy); **wrong key → `RECIPIENT_NO_MATCH`**; credential-less `Session::open` stays tolerant |
| Fuzz | `pkcs7_cms` target over `parse_enveloped_data`, 25 committed seeds (2 valid vectors + deterministic truncations/flips/length attacks), CI `fuzz-soak` matrix |

The fixture generator (`generate_pubkey_fixtures`, `#[ignore]`d, committed) builds the
fixtures from the committed keys **independently of `selis-crypto::pkcs7`** — CMS
envelopes, KW, and the Algorithm-1 seed hash are re-implemented in the generator — so the
DoD tests cross-check the handler rather than trust it.

## 8. What the human sign-off must review

- [ ] **§3 dependency decision** — hand-rolled DER reader vs `der` crate (the reversal is
      a contained module swap); `rsa`/`p256`/`sha1` adoption under ADR-P0021.
- [ ] **§4 certificate-selection policy** — first-match-with-validation without X.509
      matching is v1-acceptable; the deferred configurability list is honest.
- [ ] **§5 refused set** — s3/RC4/3DES/RC2/OAEP refusals (each is a typed error, never a
      silent wrong-key).
- [ ] **§6.1 RSA timing-side-channel acceptance** for local decryption (and the
      server-surface revisit note).
- [ ] **§6.2 permission surfacing** — `PubKeyAuth::permissions` not yet wired into
      policy; confirm deferral.
- [ ] **§6.3 unknown-handler tolerant open** posture (pre-existing; change only on
      sign-off).
- [ ] **§6.4 committed test keys** visible in the diff and acceptable.
- [ ] The `selis-crypto → selis-sandbox` layer edge in `xtask/layers.toml`.
- [ ] The new error code **1807 `RECIPIENT_NO_MATCH`** name/wording (never reused, never
      renumbered).
- [ ] That the plan entry stays **draft-awaiting-sign-off** (no conformance-ladder claim
      beyond "public-key documents open with a supplied recipient key") until this review
      lands.
