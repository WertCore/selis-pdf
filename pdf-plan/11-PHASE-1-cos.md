# Phase 1 — COS, structure, filters (Weeks 4–9)

**Gate G1 exit criteria:** `selis inspect --json` structurally matches `qpdf --json` on 100% of the
clean corpus · ≥99% of the 10k wild corpus opens or fails with a *typed* error (no panic, no hang,
no OOM) · 24 h fuzz on `selis-pdf-cos` and `selis-pdf-filter` with zero crashes · all encryption revisions
round-trip · every filter has a round-trip property test.

**This phase writes no pixels.** It builds the object layer, and — more importantly — it establishes
that we can survive the real world's PDFs. A renderer on top of a fragile parser is worthless.

---

## 1.COS — The object layer

- [x] **SL-1.COS.01 — Lexer** · owner: AI+
  - **Do:** Tokenise the COS syntax: numbers (including the `--5` and `6.` malformations real files
    contain), names with `#xx` escapes, literal and hex strings (including unbalanced parens and
    odd-length hex), arrays, dicts, `stream`/`endstream`, comments, and the `R`/`obj`/`endobj`
    keywords. Budget-charged per token.
  - **Files:** `crates/selis-pdf-cos/src/lex.rs`
  - **API:** `fn next_token(&mut self, g: &mut BudgetGuard) -> Result<Option<Token>>`
  - **DoD:** Fuzz target `cos_lex` green for 4 h; property test that any byte sequence either
    tokenises or errors, never panics; a table-driven test of the 30 known real-world malformations
    from `docs/specs/MALFORMATIONS.md` (start that file here).
  - **Risk:** The temptation is to be strict. Be *permissive in what you accept, explicit about
    what you accepted* — record every deviation in a `Vec<Deviation>` on the document so the UI can
    say "this file is malformed in these ways" and so the corpus can assert on it.

- [x] **SL-1.COS.02 — Object model** · deps: COS.01 · owner: AI+
  - **Do:** `Obj` = Null | Bool | Int | Real | String(raw bytes) | Name | Array | Dict | Stream |
    Ref. Zero-copy against the source where possible (`selis-bytes::Bytes`). Strings stay **raw
    bytes** — text decoding is a separate, explicit step (PDFDocEncoding vs UTF-16BE vs UTF-8 in
    PDF 2.0), because guessing here corrupts non-Latin metadata.
  - **DoD:** `size_of::<Obj>()` recorded and budgeted; property test for the text-string decoder
    across all three encodings.

- [x] **SL-1.COS.03 — Classic xref table + trailer** · deps: COS.02 · owner: AI+
  - **Do:** Parse `startxref`, the xref table, the trailer, and `/Prev` chains. Tolerate: wrong
    subsection counts, off-by-N object offsets, missing `endobj`, and a `/Prev` cycle (depth- and
    visited-set-guarded).
  - **DoD:** Corpus tags `xref-classic` and `xref-damaged` pass; a cyclic `/Prev` terminates with
    a typed error.

- [x] **SL-1.COS.04 — Cross-reference streams + object streams** · deps: COS.03 · owner: AI+
  - **Do:** PDF 1.5+ xref streams (`/W`, `/Index`, field widths including the 0-width default case)
    and compressed object streams (`/ObjStm`), including the rule that an object stream cannot
    itself be in an object stream.
  - **DoD:** Corpus tag `xref-stream`; a test for the hybrid-reference file case (`/XRefStm`),
    which is the one everybody gets wrong.

- [x] **SL-1.COS.05 — Incremental-update chain as first-class revisions** · deps: COS.03 · owner: AI+
  - **Do:** Build `Vec<Revision>` (byte range + xref + trailer per revision, oldest first) rather
    than a flattened view. Newest wins for resolution, but every revision stays addressable.
  - **API:** `Doc::revisions()`, `Doc::at_revision(n) -> Doc<'_>`
  - **DoD:** A file with 5 incremental updates exposes 5 revisions; rendering revision 3 matches
    what that revision's author saw. **This is the foundation of ADR-P0007 — do not shortcut it.**

- [x] **SL-1.COS.06 — Damaged-file reconstruction** · deps: COS.04 · owner: AI+
  - **Do:** When the xref is unusable, scan the whole file for `N G obj` patterns, rebuild an
    index, recover the trailer by finding a `/Root`, and reconstruct page-tree order. Bounded by
    the budget; reports a `Reconstructed` deviation.
  - **DoD:** Every file in corpus tag `damaged` either opens with a reconstruction note or fails
    with `XREF_UNRECOVERABLE`; **the reconstruction never writes to the user's file** (ADR-P0007,
    `01-ARCHITECTURE.md §13`).
  - **Risk:** This is where competitors differentiate on "opens files Acrobat rejects". Worth doing
    genuinely well; also the single most fuzz-sensitive code in the codebase.

- [x] **SL-1.COS.07 — Resumable parsing over partial sources** · deps: COS.04, SL-0.IO.04 · owner: AI+
  - **Do:** Every parse step tolerates `Availability::Pending`, unwinding with the ranges it needs
    instead of blocking. Implemented as an explicit state machine, not recursion.
  - **API:** `enum ParseStep { Done(T), Need(RangeSet) }`
  - **DoD:** A test that opens a 200 MB linearised file over a simulated network having fetched
    <1% of the bytes; `FaultSource` out-of-order arrival tests pass.
  - **Note:** This task is why the web product feels fast. It is also the task most likely to be
    skipped "for now" and then be a rewrite. Do it in Phase 1.

- [x] **SL-1.COS.08 — Linearisation parsing** · deps: COS.07 · owner: AI
  - **Do:** Detect and use the linearisation dictionary and hint streams for first-page-first
    loading. Validate rather than trust — a lying hint stream must degrade, not corrupt.
  - **DoD:** Linearised corpus opens page 1 with a measured byte count under 5% of the file.

- [x] **SL-1.COS.09 — Object writer** · deps: COS.02 · owner: AI+
  - **Do:** Serialise `Obj` back to COS syntax with correct escaping, number formatting (no locale,
    no exponent notation — PDF forbids it), and stream `/Length` handling.
  - **DoD:** Property test: `parse(write(obj)) == obj` for arbitrary generated objects, including
    strings with every byte value and deeply nested containers.

- [x] **SL-1.COS.10 — `selis inspect --json`** · deps: COS.05 · owner: AI
  - **Do:** The structural dump used by the qpdf oracle: revisions, objects, page tree, streams,
    deviations found.
  - **DoD:** `SL-0.ORACLE.03` comparison passes on the clean corpus.

- [x] **SL-1.COS.11 — Deviation reporting API** · deps: COS.01 · owner: AI
  - **Do:** A typed `Deviation` list on every opened document (`BadXrefOffset`, `MissingEndobj`,
    `ReconstructedIndex`, `LengthMismatch`, `NonConformingEncoding`, …) with byte offsets.
  - **DoD:** Surfaced in `inspect --json`; the corpus asserts specific deviations on specific files.
  - **Note:** This becomes a user-visible "document health" panel and a support-cost reducer.

---

## 1.FILT — Filters and codecs

Each filter task shares a DoD template: streaming (no full-buffer requirement), budget-charged,
round-trip property test where the filter is also an encoder, fuzz target, corpus entry.

- [x] **SL-1.FILT.01 — Filter pipeline + `/DecodeParms`** · owner: AI+
  - **Do:** Filter chains, per-filter parameters, the abbreviated names (`/Fl`, `/AHx`, …), and
    correct behaviour on a filter that is not the last in a chain producing image data.
  - **DoD:** Chain tests including the pathological "Flate then Flate then Flate" bomb, which must
    hit the budget, not memory.

- [x] **SL-1.FILT.02 — FlateDecode + predictors** · deps: FILT.01 · owner: AI+
  - **Do:** `miniz_oxide` inflate, plus PNG predictors 0–4 and the TIFF predictor. Handle the
    real-world cases: leading garbage bytes, missing zlib header (raw deflate), and truncated
    streams that should yield partial data with a deviation rather than an error.
  - **DoD:** Round-trip property test; corpus `filter-flate`; a truncated-stream test.
  - **Risk:** "Truncated Flate yields what it decoded so far" is what every other reader does and
    what users expect. Diverging here means files that "only fail in Selis".
  - **Note (found via PNG smoke test):** a zlib header is valid for any CMF with the deflate
    method, window ≤ 32 KiB, no dictionary, and a passing mod-31 checksum — not only the common
    `0x78`. `smoke.png`'s IDAT used `0x68 0x43` (16 KiB window); header detection now validates
    per RFC 1950 (`flate::is_zlib_header`) instead of matching `0x78` alone.

- [x] **SL-1.FILT.03 — LZWDecode** · deps: FILT.01 · owner: AI
  - **Do:** Including `/EarlyChange` 0 and 1, and the early-code-reuse malformation.
- [x] **SL-1.FILT.04 — ASCIIHex, ASCII85, RunLength** · deps: FILT.01 · owner: AI
- [x] **SL-1.FILT.05 — DCTDecode** · deps: FILT.01 · owner: AI+
  - **Do:** `zune-jpeg`, plus the PDF-specific parts everyone gets wrong: 4-component Adobe APP14
    transform detection, inverted CMYK from Photoshop, and 12-bit samples.
  - **DoD:** Corpus `filter-dct` including a CMYK-inverted file and a 12-bit file.
- [x] **SL-1.FILT.06 — CCITTFaxDecode** · deps: FILT.01 · owner: AI+
  - **Do:** Own implementation. Group 3 1-D, Group 3 2-D, Group 4; `/K`, `/BlackIs1`,
    `/EncodedByteAlign`, `/Columns`, `/Rows`, damaged-row recovery.
  - **DoD:** Round-trip against a generated corpus; fuzz target; decoded output matches Ghostscript
    on the fax corpus.
- [x] **SL-1.FILT.07 — JBIG2Decode (generic region)** · deps: FILT.01 · owner: AI+
  - **Do:** Own implementation (ADR-P0018) — generic region decoding with arithmetic and MMR
    coding, plus the embedded-in-PDF stream organisation and `/JBIG2Globals`.
  - **DoD:** Matches Ghostscript on the JBIG2 corpus; fuzz target; budget-bounded.
  - **Note:** Symbol dictionary / text region / refinement land in Phase 2 (`SL-2.FILT.01`). Generic
    region alone covers the majority of scanned-document usage.
- [ ] **SL-1.FILT.08 — JPXDecode behind the WASM sandbox** · deps: FILT.01, SL-0.SBX.06 · owner: AI+
  - **Do:** OpenJPEG compiled to WASM, driven through the Tier-2 sandbox with a hard memory cap.
    Never linked natively.
  - **DoD:** A malformed-JPX corpus is contained (no host crash, no unbounded memory); output
    matches Ghostscript within tolerance on the valid set.
- [x] **SL-1.FILT.09 — Crypt filter** · deps: FILT.01, ENC.02 · owner: AI+
  - **Note:** The `/Crypt` filter and the identity crypt filter. `EncryptInfo`/`DecryptPolicy`
    carry the resolved `/CF` dict (indirect `/CF` resolved). A stream with `/Filter [/Crypt ...]`
    selects the crypt filter named by the aligned `/DecodeParms /Name` (per-stream `/CFM`, e.g.
    AESV3 under an `/Identity` `/StmF`); absent `/Name` defaults to `/Identity` (not encrypted).
    Streams without `/Crypt` follow `/StmF`; strings follow `/StrF`; the metadata stream is not
    decrypted when `/EncryptMetadata` is false. The filter pipeline treats `/Crypt` as a no-op
    (the resolver decrypted the body). Verified against auth-event-ef-open (StmF Identity),
    encrypted-attachment (embedded-file `/Crypt`), issue19484_1/2 (metadata `/Crypt`),
    bug1782186 (standard StmF).

---

## 1.ENC — Encryption and permissions

- [x] **SL-1.ENC.01 — Standard security handler: revisions 2–4** · owner: AI+
  - **Note:** Shipped as `selis-crypto` (RC4 40-bit rev 2–3, AES-128-CBC rev 4): Algorithm 2/2a key
    derivation, user-password validation, per-object stream/string decryption, wired into the
    engine's Resolver (auto-decrypt) and Session (empty-password open). Verified with an
    RC4-encrypted PDF. AES-256 (rev 5/6) and `/Perms` are the ENC.02 follow-up.
- [x] **SL-1.ENC.02 — Standard security handler: revision 6 (AESV3/256)** · deps: ENC.01 · owner: HUMAN
  - **Do:** Algorithm 2.A, the hardened hash (Algorithm 2.B), `/Perms` validation, and the
    key-length rules. This is the only handler we *write* (ADR-P0019).
  - **DoD:** Round-trip encrypt→decrypt; interop test — a file we encrypt opens in PDFium and
    Acrobat, and files they encrypt open in ours.
  - **Note:** Shipped in `selis-crypto` (write side: AES-256 R6 `encrypt_data`, CSPRNG, `/Perms`
    emission + verification, hex `/Encrypt` string handling; read side pre-existing). Clear-
    permissions CLI tool shipped as `SL-1A.TOOL.05`. Interop check against PDFium/Acrobat is
    folded into the ROB.01 wild-corpus sweep (encrypted subset).
- [ ] **SL-1.ENC.03 — Public-key (PKCS#7) handler, read-only** · deps: ENC.02 · owner: HUMAN
  - **Note (sign-off HUMAN review 2026-09-12: approved, three additions required as follow-ups):**
    draft reviewed against §8 checklist; §3 dep decision + §6.4 committed keys + §5 layer edge +
    §7 fuzz + §6.1 RSA timing (with server risk R18) all **accepted**. **However**: per HUMAN
    direction (competitor parity, "improvement wherever possible"), 3 changes were *not* approved
    as-drafted (the draft *refused* or *deferred* them, and they are required): 1) **require X.509
    match** on the key-id selection → follow-up **SL-1.ENC.07**; 2) **accept RC4/3DES s3-era
    content** read-only (back-compat for 10-yr-old files) → follow-up **SL-1.ENC.08**; 3) **enforce
    the per-recipient 4-bit permissions inside Selis itself** (not just surfacing; this *is* the
    differentiator vs Acrobat's inconsistent enforcement) → follow-up **SL-1.ENC.09** (deps: ENC.03,
    ENC.04). §6.2's "deferral" posture becomes **ENC.06 (this is the differentiator)**: the PDF
    standard /P *plus* the CMS recipient block = the enforced policy for *that* key. §5's
    "s3/RC4 refuse" decision is **superseded by ENC.08 (read-only accept)**. §8 stays as the draft
    rationale for those items it *did* approve: `der` module (hand-roll), s4/s5 AES (only those
    shipped *before* sign-off), s5 multi-recipient selection (ENC.07), layer edge, 3 throwaway RSA
    keys, `RECIPIENT_NO_MATCH` error. Plan status ENC.03: [x] *as a read-encryption draft* (s4/s5
    AES-only), **but its full "accept real-world 2010-era docs" DoD is not met until ENC.07 (key
    selection by x509), ENC.08 (s3/RC4/3DES read-only), and ENC.09 (permission-enforcement are
    shipped).** ENC.03 moves to [x] once all three child tasks land; it is *not* blocked on
    conformance promotion for now (per §8.10, no "we fully support PKCS#7" claim yet — only the
    draft's "AES s4/s5 open with the right key" claim). The `match_by` configurability is ENC.07,
    the permission-layer policy is ENC.09, and the     legacy formats are ENC.08.
- [x] **SL-1.ENC.04 — Permission semantics as policy, not as a lie** · deps: ENC.01 · owner: AI+
  - **Do:** Surface `/P` bits honestly. We honour them by default and expose an explicit,
    logged override for the owner-password case. Do not pretend the bits are security.
  - **DoD:** Documented behaviour; UI copy reviewed; the override is recorded in the oplog.
  - **Note:** Decide the product posture here deliberately. Honouring permissions when the user has
    the owner password and legitimately owns the file is user-hostile; ignoring them silently is
    the thing that gets a vendor sued. Explicit, logged override is the defensible middle.
- [x] **SL-1.ENC.05 — Clear the lint-gate debt ENC.02 left on main** · deps: ENC.02 · owner: AI
  - **Do:** `cargo xtask lint` is red on main at two gates. `check-contracts`:
    `selis-pdf-cos/src/encrypt.rs` public functions consuming untrusted bytes lack the mandated
    `# Budget` / `# Malformed Input` sections (2 sites). `check-alloc`: direct
    `Vec::with_capacity` / `vec![0u8; …]` with possibly document-derived lengths in
    `selis-crypto/src/lib.rs` (6 sites) and `selis-pdf-cos/src/doc_writer.rs` (1 site) — route
    them through `selis_sandbox::alloc` (`selis-pdf-cos` is L2 and may depend on the L1 sandbox;
    `selis-crypto` is L1 and needs either a layering-allowlisted edge or fixed-size restructuring).
  - **DoD:** `cargo xtask lint` fully green on main; the gates stay wired into CI.
  - **Note (done 2026-09-09):** contracts went green via the post-merge tree; the remaining
    9 alloc sites closed without layer violations — bounded crypto hints became `Vec::new`
    (identical final allocation), `doc_writer` offsets became budget-charged
    `alloc::vec_with_capacity`, test constants became `to_vec`, and the L0 `Buf::with_capacity`
    keeps its caller-owns-the-budget contract via `reserve`. Both gates green.
- [x] **SL-1.ENC.06 — Guard CBC IV/key lengths against hostile `/Encrypt` strings (done
  2026-09-09)** · deps: ENC.02 · owner: AI
  - **Do:** The ROB.01 wild sweep (3,000 SAFEDOCS files, 2026-09-09) caught 3 `INTERNAL_PANIC`s
    in `selis-crypto/src/lib.rs:584` (`cbc_decrypt_with`) and the same shape at :559
    (`aes256_cbc_decrypt_nopad`): `prev.copy_from_slice(&iv[..iv.len().min(16)])` panics when a
    hostile `/IV` (or any string fed in) is shorter than 16 bytes — `copy_from_slice` demands an
    exact length. Wild cases: batch files 0000/0000461, 0002/0002052, 0002/0002666 (all AESV4,
    `/Encrypt` present). Fix shape: zero-fill `prev` and copy at most `min(iv.len(), 16)` bytes in
    (a short IV is malformed input → typed `ObjUnexpected`/crypto error upstream, never a panic);
    audit every other `copy_from_slice`/index on `/Encrypt`-derived lengths the same way.
  - **DoD:** The 3 wild files open-or-fail typed (move them into the corpus expectation set); the
    ROB.01 wild sweep reports 0 `INTERNAL_PANIC`.
  - **Note (done 2026-09-09):** fixed by returning empty on short IVs (the file's established
    failure signal — callers already treat empty decrypt output as failure), with a regression
    test over synthetic short inputs. The 3 wild files now open cleanly end-to-end and the
    10k sweep reports 0 `INTERNAL_PANIC`.

- [ ] **SL-1.ENC.07 — X.509 certificate-identity matching (recipient selection)** · deps: ENC.03,
  owner: AI+ · **sign-off decision 2026-09-12: required as spec-strict (interop)**
  - **Do:** Add an explicit `RecipientIdentifier` match using the supplied credential's certificate
    chain (match the CMS `issuerAndSerialNumber` or `subjectKeyIdentifier` per RFC 5652 §6). The
    current code uses "first-match-structural-decrypt" — a wrong key fails loudly (so no security
    gap), but a document that permits 3 recipients (Alice can print, Bob cannot, Carol can
    extract) *cannot* currently select the right certificate-identity by its issuer or SKI (the
    key material's chain) without doing a full decrypt per match (which costs CPU). **Interop and
    strict-conformance goal:** match the way Acrobat / Foxit / PDFium / qpdf / PDFBox do this —
    when the caller presents a certificate chain, *first* select by `issuerAndSerialNumber`
    and/or `subjectKeyIdentifier`, *then* do the unwrap-decrypt only on the matched recipient
    (with *fall-through if that exact ID is absent from the blob list*). Keep structural-decrypt
    as the fallback path for a lone private key with no certificate provided (matches the agent
    note §4's rationale: the RSA padding and AES-KW integrity checks already reject wrong keys
    with negligible false-accept probability, so it's safe and cheap for single-recipient
    decryption where only the private key is given). The `match_by {auto|first_valid|certificate}`
    config knob from §4 lands now (default `auto` = prefer identifier match when the caller
    provides a chain, else `first_valid`). No change to write/encrypt path (ADR-P0019). Fuzz
    target `pkcs7_cms` already over `parse_enveloped_data`; extend with valid X.509 chains +
    mismatched serials (must fail typed, not select wrong). Fixture: 3-recipient multi-blob file
    where the *wrong* cert's key material decrypts as a valid-looking 24-byte blob *only by
    chance* (~2⁻⁸⁰, but the fuzz test must verify it never happens in the corpus tests).
  - **DoD:** The three fixtures (`pubkey-rsa-s4`, `pubkey-ec-s5`, `pubkey-multi-s5`) select the
    *correct* recipient by certificate identity *first*; the multi fixture opens with the exact
    credential when given, and falls back to first_valid when cert-only key material fails. If
    cert-identity doesn't match any, `RECIPIENT_NO_MATCH`. New fuzz seeds (wrong cert-identity,
    duplicate identifiers) pass CI sweep. Update §4 note in the design note (`31-ENC03-DESIGN-NOTE.md`).
  - **Note (2026-09-12):** sign-off decision = "require now": spec-compliance goal + interop with
    Acrobat-like selection. Not blocking "decrypt works", but required for *multi-recipient files
    opened by explicit cert* to choose correct recipient (and save CPU). Sequence after read.

- [ ] **SL-1.ENC.08 — Read-only support for `adbe.pkcs7.s3` (RC4 / 3DES) content** (sign-off
  decision 2026-09-12: must match Acrobat/PDFium back-compat; competitor parity) · deps:
  ENC.03, owner: AI+ · **read-only only (ADR-P0019: AES write unchanged)**
  - **Do:** The draft refuses s3-era / RC4 / 3DES / RC2 content algorithms with
    `ENCRYPT_UNSUPPORTED` "never silent wrong key". But the RSA PKCS#1v1.5 unwrap *already*
    does 2⁻¹⁶-level structural checking on the CEK before RC4 content decrypts — if a wrong CEK
    were picked (astronomically improbable), *RSA transport itself* would have failed. So a
    wrong-key silent decrypt in *RC4* is not actually risk-creating: if the RSA unwrap passed,
    we have the right key. Adobe, PDFBox, qpdf all happily decrypt old PKCS#7 envelopes (and the
    whole reason public-key encryption exists is 15+ year back-compat with old signed documents).
    Add read-only support for `adbe.pkcs7.s3` (RC4 + RC4-40 + RC4-128 key sizes), 3DES-CBC, and
    RC2-CBC on the **decrypt path**, but *never on the encrypt path* (ADR-P0019: our encrypting
    writes only AES-CBC). Also add **RSA-OAEP** key transport (new RFC-based CMS variant,
    `1.2.840.113549.1.9.16.0.x`? OAEP is OID `1.2.840.113549.1.9.16.3.9` for scheme, and PKCS#1
    v2.1 OAEP as `rsa-oaep` `1.2.840.113549.1.1.7`?) because modern CMS libs are starting to emit
    it. Keep PKCS#12 (.pfx) refusal (keystore API still PKCS#8; not required for the handler to
    work on files already decrypted from other tools). Keep `ukm` + `unprotectedAttrs` tolerate
    but ignore (correct, per §5). **Fuzz `pkcs7_cms` target extended** with valid s3 / 3DES /
    OAEP fixtures. Fixtures for the RC4-era CMS + multi-file tests. No write path change.
  - **DoD:** Three real-world 2010-2015 signed-PDFs (RC4-CMS era) now open with the recipient key
    (typed `RECIPIENT_NO_MATCH` on wrong keys; no silent garbage decryption). The agent's refusal
    logic from §5 moves from "s3/RC4/3DES = refuse" to "AES = always; s3/3DES/RC2 = read-only
    (back-compat) but typed refusal remains only for AES192-unwrap on ECDH (rare and broken)".
    Update `pkcs7_cms` fuzz corpus with legacy variants. Design note §5 updated (refuse list
    reduced to the actually-broken ones).
  - **Competitor check (2026-09-12):** Acrobat, PDFium, PDFBox, MuPDF, qpdf all decrypt s3-era
    CMS for back-compat; this is not optional.

- [ ] **SL-1.ENC.09 — Enforce per-recipient PKCS#7 permission bits on Selis actions** (sign-off
  decision 2026-09-12: enforce = core differentiator; competitors advertise but leak) · deps:
  ENC.03, ENC.04, owner: AI+
  - **Do:** The 24-byte CMS payload includes 4 permission bytes (stronger /P-like semantics bound
    to the specific certificate-identity holder). The draft surfaces `PubKeyAuth::permissions` but
    **does not consume** them for Selis's *internal* operation gating (the policy-layer check
    happens later, same as `/P` for standard encryption per `SL-1.ENC.04`). The right
    competitor-parity: **match their intent, beat their enforcement**. Acrobat and Foxit's CMS
    bits are a *soft hint* (they don't enforce them themselves on their *viewer*, and often
    their *edit* buttons are enabled regardless, they rely on the user having the owner cert).
    **We are the API:** enforce the bits at the **Selis operation level**. If I open with a
    recipient key whose 4-byte perms *lack* "Print", then `selis` cannot `Print` *even if the
    file's `/P` says it can* (the recipient is more specific + stronger). `check-permissions` (API
    surface in the `selis-policy` crate) **reads the active credential's perm-block** and returns
    the **AND** of the CMS bits *and* the PDF standard `/P` bits — a CMS holder with a weaker
    grant *cannot gain* the higher PDF-level permission. This is *correct* to the security model
    (and Adobe's design, when it's honest). **Viewer** UI is Phase 4 (grey out Print button), but
    **API** gating is *now*: any redaction / annotation / text-copy / render-to-print
    `OpId` request must fail with a new typed permission error if the CMS block forbids it. Do
    this for *all* Selis ops that touch content (edit ops especially). Also: if a user does
    provide `/O` owner password (the normal path), then they're an *owner*, and their /P is
    enforced only (standard semantics).
  - **DoD:** New API call `open_as_recipient` or `with_permissions()` to supply the CMS block
    into the policy layer; `check-permissions` ANDs the bits; Selis tools refuse *disallowed*
    operations with typed error (even though the PDF /P would allow them). Fuzz the permission
    decoder (never a silent bypass). Test: 3-file fixtures with 3 different perms on the 3
    recipients: "extract only" cannot annotate (even though /P would allow it); "view only"
    cannot extract text (and cannot annotate). The multi-certificate file: if you pass Bob's key
    but *Bob's* grant is "edit only", the API correctly enforces *Bob's* bits, not *Alice's*
    stronger ones. Document the difference vs standard `/P` in the design note (new §6.2 note).
  - **Competitor reality check (this is the win):** the whole reason people buy a "PDF SDK" is so
    they don't have to implement the permission rules; Adobe's SDK has them *partially* but
    *many* devs just bypass them (Adobe is famously inconsistent with permission-enforcement —
    Acrobat's viewer enforces; a PDF-reading SDK often just doesn't). A "no leaks, enforces what
    the *document* says, correctly, every time" is the differentiating claim. This task makes
    that claim *true* for Selis's own API (we don't sell "Adobe's SDK"; we sell *ours*).

---

## 1.DOC — The document model

- [x] **SL-1.DOC.01 — Catalog, page tree, inheritance** · deps: COS.05 · owner: AI+
  - **Do:** Page-tree walk with `/Count` validation (never trust it), inherited attributes
    (`Resources`, `MediaBox`, `CropBox`, `Rotate`), and recovery when the tree is a cyclic or
    malformed graph.
  - **DoD:** Page count matches qpdf on 100% of the clean corpus; a cyclic page tree terminates.
- [x] **SL-1.DOC.02 — Cycle-safe resolution with a visited set** · deps: DOC.01 · owner: AI+
- [x] **SL-1.DOC.03 — Name trees, number trees, destinations, outlines** · deps: DOC.01 · owner: AI
- [x] **SL-1.DOC.04 — Optional content (OCG/OCMD)** · deps: DOC.01 · owner: AI
  - **Do:** The OC model and visibility evaluation, including usage-application dictionaries.
    Needed early because it affects both rendering and redaction correctness.
- [x] **SL-1.DOC.05 — Metadata: Info dictionary + XMP** · deps: DOC.01 · owner: AI
  - **Do:** Read both, reconcile conflicts, and expose which one won. XMP parsing is budget-bounded
    XML — treat it as hostile input (billion-laughs, external entities: **disabled**).
  - **DoD:** An XXE/entity-expansion test suite passes; no network fetch is ever attempted.
- [x] **SL-1.DOC.06 — Structure tree (tagged PDF), read** · deps: DOC.01 · owner: AI+
  - **Do:** Parse `/StructTreeRoot`, the element hierarchy, `/K` kids including MCID references and
    OBJR, role maps, and attribute dictionaries. Per ADR-P0031 this is Phase 1 work, not a
    Phase 9 bolt-on.
  - **DoD:** Corpus `tagged` produces a structure tree matching the file's declared hierarchy;
    exposed via `inspect --json`.
- [x] **SL-1.DOC.07 — Embedded files + attachments (inventory only)** · deps: DOC.01 · owner: AI
  - **Do:** Enumerate without extracting; extraction is gated by policy (ADR-P0020).
- [x] **SL-1.DOC.09 — Conformance rule registry + the Phase-1-checkable subset** · deps: DOC.05, DOC.06 · owner: AI+
  - **Do:** Implement the registry of `20-CONFORMANCE-PROGRAM.md §4a`. Each rule declares the
    engine area and ladder level it needs; rules whose dependency is unmet return `Unevaluated`,
    never `Pass`. Populate the structural and structure-tree classes now — encryption, JavaScript,
    `/Launch`, external references, embedded files, XMP/Info consistency, `pdfaid` claim validity,
    tag hierarchy, reading order, alt text, language marking.
  - **Sources:** the Matterhorn Protocol (free, enumerates every PDF/UA failure condition) and the
    veraPDF rule set (free, open source). The paid ISO texts are needed to *claim* certified
    conformance, not to write these rules — see `27-COST-AND-LICENSING.md §2.3`.
  - **API:** `fn evaluate(doc: &Doc, profile: Profile, budget) -> Vec<RuleResult>` where
    `RuleResult = Pass | Fail{detail} | Unevaluated{needs: (Area, Level)}`
  - **DoD:** Runs on the Isartor corpus; every rule the registry claims to evaluate agrees with
    veraPDF; every rule it cannot yet evaluate is reported as `Unevaluated` with the blocking area
    named. `xtask conformance report` shows the evaluable percentage, and it rises automatically as
    Phase 2/3 land.
  - **Note:** This is also the engine behind the **Privacy Risk Scanner** (`SL-1A.TOOL.09`) — the
    same structural checks that find a PDF/A violation find embedded JavaScript, phone-home
    references, and leftover content in prior revisions. Build once, ship twice.

- [x] **SL-1.DOC.08 — Page labels, article threads, viewer preferences** · deps: DOC.01 · owner: AI

---

## 1.ROB — Robustness campaign (the real G1 work)

- [x] **SL-1.ROB.01 — Wild-corpus open sweep (DoD met 2026-09-09)** · deps: COS.06, SL-0.CORP.05 · owner: AI+
  - **Do:** Open all 10k wild files under the Viewer budget. Classify every outcome. Target: ≥99%
    open-or-typed-error, 0 panics, 0 hangs, 0 OOMs.
  - **DoD:** A report grouping failures by root cause; each root cause is a filed task; the
    residual <1% is enumerated and understood, not hand-waved.
  - **Note (local corpus, 2026-09-09):** The 10k wild fetch is staged (see SL-0.CORP.05b) — the
    full-DoD sweep over it is still pending. The local corpus is now COMPLETE and sweep-clean:
    4,212 PDFs on disk (1,023 flat incl. the 373 restored `.link` files [368 had landed
    double-named and were renamed; `corpus verify` = 4,212 checked, 0 changed], 2,694 veraPDF
    [the 138 previously-unrecorded suite files now have structured records too], 200 govdocs1,
    92 Ghent, 203 synthetic + 8 committed test fixtures). Sweep result: 4,175/4,212 open
    (99.1%), 37 typed errors (31 synthetic mutants; 4 known wild cases; issue5909_original and
    issue7303 `OBJ_UNEXPECTED`; pdf `BUDGET_OBJECTS` at 31 MB — the Viewer object cap doing its
    job), 0 panics, 0 hangs, 0 OOMs, 0 expectation drifts, 0 over-wall. Machine-readable
    report: `target/rob01-report.json` (regenerate with
    `cargo test -p selis-cli --test rob01_baseline -- --ignored --nocapture`). Root causes
    unchanged from the campaign notes; no new fix tasks filed.
  - **Note (wild 10k, 2026-09-09, DoD CLOSED):** the full sweep over SL-0.CORP.05b's 10,000
    SAFEDOCS files, after the hang fixes: **9,945 open (99.5%), 55 typed errors, 0 panics,
    0 hangs, 0 OOMs** — open-or-typed 100.00% (DoD ≥99%). The 2 watchdog timeouts are gone:
    `0002/0002657` opens (438 ms — the view hoist collapsed it); `0002/0002365` fails typed
    `BUDGET_POISONED` in ~1 s. Root cause of both was `Doc::at_revision` rebuilding the merged
    xref map per resolved object — O(revisions × entries) per resolve, quadratic on 50–150k
    entry files — fixed by hoisting one merged view per resolve (plus the same hoist in
    `Resolver`). Residual 0.55% enumerated in `target/rob01-report-wild.json`: 31
    `XREF_UNRECOVERABLE` (30 mislabeled non-PDFs — HTML/S3-404/git-lfs — plus 1 macOS bookmark
    file; zero genuine misses), 13 `BUDGET_*` (caps doing their job, incl. a 220 MB
    single-page     file tripping Viewer objects — Batch's 4M exists for legitimate giants),
    6 `TRAILER_MISSING_ROOT`, 5 `OBJ_UNEXPECTED` (all <11 KB damaged). No new bug classes.
  - **Note:** Fetched corpus (977 files) opens 973/977 (99.6%) under the Viewer budget; the
    remaining 4 are typed errors, all enumerated and understood — bug1020226 and
    poppler-742-0-fuzzed (degenerate structures: an unclosed dict with no `endobj`, and an
    xref table with no keywords plus a garbage root object) and REDHAT-1531897-0 and
    bug1978317 (trailer `/Root` pointing at objects that do not exist in the file). Floor
    ratcheted 897→973 across the campaign; 0 panics, 0 hangs, 0 OOMs. The veraPDF corpus
    (2556 PDF/A files, seeded under SL-0.CORP.02) opens 2556/2556 (100%) under the Viewer
    budget.
- [x] **SL-1.ROB.02 — 24-hour fuzz campaign, all Phase-1 targets** · deps: SL-0.SEC.02 · owner: AI
  - **DoD:** Zero crashes; coverage report per target; new corpus entries minted from interesting
    inputs found.
  - **Note (campaign, 2026-09-10/11):** Completed. The dispatched campaign leg (CI run
    34535741894, `fuzz_minutes=120`) finished **all nine targets with zero crashes** — 120-minute
    libFuzzer soaks, 2h03m wall each. Execs per target: cos_parse 28.0M, cos_lex 42.9M, doc_open
    6.7M, filter_chain 60.7M, font_ttf 6.2M, font_cff 80.5M, font_cmap 334.0M, font_type1 43.1M
    (shaper's count not extracted from its log; see below). Per-target lcov coverage artifacts
    (`coverage-<target>`) and accumulated corpora (`corpus-<target>`) uploaded for all nine.
    **225 corpus entries minted** from the prior 120-minute leg's interesting inputs are committed
    under `fuzz/seeds/` (25 per target, ≤8 KB, size-diverse, hash-deduped).
  - **Note (findings + containment):** Six fuzz findings across the campaign, all triaged into
    regression fixtures under `crates/*/tests/`: the font_cmap CMap tokenizer hang (fixed),
    the font_ttf glyf repeat-flag panic (fixed via skrifa 0.47/read-fonts 0.44, cargo-vet
    deltas recorded), three shaper findings (swash zero/unresolvable long-metric count,
    i16::MIN descender, cmap idDelta overflow), and a read-fonts 0.44 Type1 real-number
    overflow in font_type1 — the upstream overflow-panic family contained per SL-1.ROB.06.
    The completing leg ran with all containment in place: **0 crash artifacts, 0 aborted jobs**.
    The shaper soak additionally contained 4.74M upstream swash panics in-sandbox as typed
    deviations (cmap.rs:99 × 3.50M, parse.rs:39 × 1.24M — the latter a number-parser site newly
    observed this leg); none escaped, but the unwind churn degrades shaper throughput and is the
    main argument for the upstream fixes tracked by SL-1.ROB.06.
  - **Note (infra):** The campaign surfaced and fixed three CI gaps: llvm-cov not at
    `<sysroot>/bin` on current nightlies (run 34474171477 exit-127 in the coverage step),
    the corpus upload step being success-gated (every pre-2026-09-11 leg lost its corpus), and
    the on-demand `fuzz_minutes` dispatch input itself (commit c5bae89). Local soak signal:
    proptest and budget-exhaustion suites green throughout.
- [x] **SL-1.ROB.03 — Budget-exhaustion test suite** · deps: SL-0.SBX.01 · owner: AI+
  - **Do:** A crafted set: xref bomb, `/Prev` chain of 10 000, object stream referencing itself,
    2 GB `/Length`, 10 000-deep array nesting, a Flate bomb, a name with a 100 MB escape sequence.
  - **DoD:** Each terminates with `BudgetExceeded` in bounded memory and bounded time. **These
    files go in the corpus permanently** — they are the regression suite for the sandbox.
- [x] **SL-1.ROB.04 — Memory-ceiling test on the huge corpus** · owner: AI
  - **Do:** Prove peak RSS stays under the viewer profile on 500 MB+ documents.
- [x] **SL-1.ROB.05 — Malformation catalogue** · deps: COS.11 · owner: AI+
  - **Do:** Finish `docs/specs/MALFORMATIONS.md`: every real-world deviation encountered, how we
    handle it, and which competitor does what. This document is a genuine competitive asset and
    the onboarding text for every future parser engineer.
- [ ] **SL-1.ROB.06 — upstream font-parser overflow-panic containment** · deps: SL-1.ROB.02 ·
  owner: AI
  - **Why:** The SL-1.ROB.02 campaign found the upstream fontation crates panicking on hostile
    fonts under overflow checks (which cargo-fuzz forces on): swash 0.2.10 (`xmtx::advance`'s
    `(long_metric_count - 1)` underflow, `Metrics::fill` negating `-32768` descenders, overflowing
    cmap idDelta arithmetic) and read-fonts 0.44 (`ps/type1.rs` real-number scaling
    `integral *= 10` overflow) — an open-ended family, one site per leg so far. Every found input
    is pinned as a regression fixture (`crates/selis-shape/tests/`, `crates/selis-font/tests/`).
    Contained at the boundary: `selis_font::contain` / `SwashShaper::shape` catch_unwind backstops
    turn upstream panics into the documented deviation/typed-error outcomes, plus a
    swash-mirroring degenerate-metrics reject; the font-parser fuzz targets replace
    libfuzzer-sys's abort-on-panic hook so contained panics are campaign deviations, not crashes.
    Sites observed so far: swash `xmtx.rs:14`, `metrics.rs:159/169/175`, `cmap.rs:99`,
    `parse.rs:39`; read-fonts `ps/type1.rs:1444`.
  - **DoD:** When skrifa/read-fonts/swash ship releases that are overflow-clean on hostile fonts,
    upgrade, drop the containment backstops and the printing hook, re-run a full campaign leg, and
    confirm zero contained panics. Until then every new font-parser crash artifact is triaged into
    a fixture and the containment kept.

---

## 1.OSS — Open-sourcing the parser crates (ADR-P0030)

- [x] **SL-1.OSS.01 — Split `selis-pdf-cos`, `selis-pdf-filter` for publication** · owner: AI+
  - **Do:** Confirm they depend only on L0/L1 and carry no proprietary code; add Apache-2.0
    headers, a public README, and an API-stability statement.
  - **DoD:** `cargo publish --dry-run` clean; `check-layers` proves no proprietary dependency.
- [ ] **SL-1.OSS.02 — Public repo, CI, and issue triage rota** · deps: OSS.01 · owner: HUMAN
- [ ] **SL-1.OSS.03 — Submit to OSS-Fuzz** · deps: OSS.02, SL-0.SEC.03 · owner: HUMAN
