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
- [x] **SL-1.FILT.08 — JPXDecode behind the WASM sandbox** · deps: FILT.01, SL-0.SBX.06 · owner: AI+
  - **Do:** OpenJPEG compiled to WASM, driven through the Tier-2 sandbox with a hard memory cap.
    Never linked natively.
  - **DoD:** A malformed-JPX corpus is contained (no host crash, no unbounded memory); output
    matches Ghostscript within tolerance on the valid set.
  - **Note (done 2026-09-14; oracle-match clause 2026-09-26):**
    JPXDecode runs OpenJPEG 2.5.3 compiled to `wasm32-unknown-unknown` behind the
    SBX.06 Tier-2 host: vendored pinned sources in `third-party/openjpeg`
    (BSD-2, `PROVENANCE.md`, sha256 of the built `selis-jpx.wasm` re-verified
    every decode), zero host imports, linear-memory cap from `Budget.bytes`,
    fuel from `Budget.wall`, cooperative `CancelToken`. Never linked natively
    (ADR-P0018/P0020 spirit: untrusted codec runs behind the sandbox). Engine
    integration: `selis-pdf-engine` `decode_terminal_image_rgba` drives
    `jpx_decode` → `selis_image::dct_to_rgba` (behind the `wasm-host` cargo
    feature — the native `wasmtime` embedding is excluded from the wasm32
    viewer, so the 233 KB module never enters the core bundle; `size-check`
    unaffected).
    - **Containment clause — DEMONSTRATED.** `corpus filter-jpx`
      (`tests/fixtures/filter-jpx/`, README gives the recipe for each file, all
      derived from the golden lossless codestream): `siz-bomb`, `tile-count-bomb`,
      `truncated-tile`, `marker-garbage`, `header-only`. Every entry is decoded
      through the sandbox to a typed sandbox/image error, host-side charge stays
      under 64 KiB (never the claimed canvas), the budget guard is *unpoisoned*
      (SBX.06's "containment, not catastrophe"), and the golden fixture still
      decodes after the barrage (`jpx.rs::filter_jpx_corpus_is_contained`;
      `a_siz_bomb_charges_the_host_for_input_only`). proptest shape/charge
      invariants (`jpx_decode_never_escapes_the_sandbox`) + trailing-byte
      determinism (`trailing_bytes_do_not_change_the_decode`), and the
      `jpx_stream` cargo-fuzz target (nightly matrix + committed seeds) extend
      the hunt. The host copy_out-after-grow bug found with the *real* module
      has a regression test (`selis-sandbox::…_initial_memory_is_not_forged`).
      CI: the `wasm-host` codec now runs in `test-fast` (was only the sandbox
      before) and `xtask`-policy clippy runs both cfg branches (`CI-lesson #1`).
    - **Oracle-match clause — DEMONSTRATED, with an honest caveat.** The valid
      set now carries a **lossy 9/7 fixture** (`jpx_gradient_lossy.j2k`,
      irreversible DWT, Q=40 dB, plus its native ref `jpx_gradient_lossy_ref.raw`)
      in addition to the two 5/3-lossless streams (`jpx_gradient.j2k` MCT/YC and
      `jpx_gradient_nomct.j2k`), so the common in-the-wild lossy JPX path is
      oracle-checked, not only the reversible path. `third-party/refjpx.c` gained
      a `REFJPX_IRREVERSIBLE`/`REFJPX_Q` toggle (native OpenJPEG, host-only
      oracle, never linked into the engine) with a reproducible build script
      `third-party/build-refjpx-native.ps1`. The **Ghostscript differential leg
      is now measured**: `xtask oracle compare-jpx` (behind a new `wasm-host`
      xtask feature, `#[cfg]`-gated so the wasm32 `size-check` build is
      unaffected — verified) wraps each codestream in a minimal PDF image XObject
      and renders GS 9.56.1 `ppmraw` at 72 dpi (1 pt = 1 px), then compares the
      wasm-sandbox decode against GS's own decode. Measured 2026-09-26 over the
      three-fixture valid set: **byte-exact on every stream — max per-channel
      diff 0, mean 0, 100% of pixels within |diff| ≤ 12** (lossless MCT, lossless
      no-MCT, and the 9/7 lossy stream alike). The documented tolerance —
      ≥ 99% of pixels with per-channel |diff| ≤ 12 **and** mean |diff| ≤ 2 — is
      set from that measured data per `21-TESTING-AND-ORACLES.md §5` (a tolerance
      is calibrated to the measurement, never loosened to make a build green);
      the measured values are strictly inside the bound.
      **Honest caveat:** GS 9.56.1's JPXDecode is *not* an independent decoder —
      it statically links OpenJPEG and emits its own `openjpeg warning: unspec
      CS. 3 components. Assuming data RGB.` message, and the byte-exact 9/7
      match (a floating-point DWT, where two independent implementations
      essentially never agree to the bit across 9216 samples) is only plausible
      if both sides run the same library. So the GS leg proves **port fidelity
      plus GS-integration** (our freestanding shim + first-fit allocator produce
      the same samples GS's embedded OpenJPEG does, and our PDF wrapper drives
      GS correctly) but is *weaker than a fully independent spec-conformance
      oracle*. The DoD's literal "matches Ghostscript within tolerance on the
      valid set" clause is satisfied and honest; a genuinely independent JPX
      implementation for true cross-implementation conformance is filed as
      **SL-2.FILT.02 follow-up** (below).
    - **Deferred:** CMYK/`+alpha`/`JP2` colour-management shaping is deferred to
      SL-2.FILT.02 (current 4-comp handling is a documented first-three-channels
      approximation, not a correctness claim). The GS leg runs local-first
      (pinned GS 9.56.1, `SELIS_GS`/`gswin64c`/`gswin32c`/`gs` resolution); it is
      wired to the `21-TESTING-AND-ORACLES` differential harness so the CI oracle
      container can re-run it on a runner that has Ghostscript. Honesty beats
      false green: containment is proven, the oracle-match clause is now measured
      and within a documented tolerance, and the caveat above is stated plainly
      rather than hidden.

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
    * **Child status (2026-09-16):** SL-1.ENC.07 shipped (cert-identity selection + `match_by`),
      SL-1.ENC.09 shipped (`/P∧CMS` enforcement, code 1808) — both pending HUMAN line-by-line
      review, boxes deliberately unchecked; **SL-1.ENC.08 shipped 2026-09-16** (s3/RC4/3DES/RC2
      read-only content + RSAES-OAEP transport + the nightly fuzz build repair, see its
      done-note) — also pending HUMAN review, box deliberately unchecked. All three children are
      now code-complete; ENC.03 therefore stays `[ ]` because **none of the three is reviewed
      yet** (the flip requires all three `[x]`: honesty over false green). Its own DoD clause
      that ENC.08 could *not* close is §8.10's caveat: no real 2003–2009 public-key PDF has been
      recovered from the corpus to open here (corpus is fetch-only; see ENC.08's done-note) — the
      claim remains "s3/RC4/3DES/RC2/OAEP read paths exist and are DoD-tested on deterministic
      DER shapes", not "we open Acrobat's files".
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
  - **Note (done 2026-09-14, pending HUMAN line-by-line review — owner AI+ encryption code; box
    stays unchecked until reviewed):** Shipped scope:
    * `selis-crypto::x509` (new, hand-rolled DER per §8 precedent — no new deps):
      `parse_identity` reads only the four recipient-matching facts (issuer `Name` DER content,
      serial, `subjectKeyIdentifier` extension, EC method-1 SHA-1 of the subject public key);
      signature/validity/subject are skipped structurally, **never verified** (selection is an
      addressing rule, not a trust decision — §4.1 of the design note); `# Budget` /
      `# Malformed Input` rustdoc + its own fuzz target (§6 rules).
    * `pkcs7`: `RecipientIdentifier` (issuerAndSerialNumber / subjectKeyIdentifier, RFC 5652 §6)
      now *parsed* out of both transports (key-agree fanned out per `RecipientEncryptedKey`);
      `PubKeyCredential` = key material + chain (`Vec<der>`, leaf first) + `MatchBy
      {auto|first_valid|certificate}` knob (default `auto`): identity pass selects strictly the
      addressed recipients **before** any unwrap, fall-through to the retained first_valid
      structural scan when the exact ID is absent or a bare key is supplied; `certificate` mode
      = spec-strict, no fall-through; `Matched::Structurally` never appears under `Certificate`
      (asserted in every selection result).
    * `PubKeyAuth`/receipt carry `matched_by` + `recipient_index` for interop observability.
    * No write-path change (ADR-P0019); AES s4/s5 rules unchanged (ENC.08 stays out of scope).
    * Fixtures regenerated by `cargo xtask pubkey-fixtures`: `pubkey-rsa-s4.pdf` (blob addressed
      by Alice's real `issuerAndSerialNumber`), `pubkey-ec-s5.pdf` (method-1 SKI — cert has no
      extension), `pubkey-multi-s5.pdf` (EC/SKI first, RSA/ISN second — *both* open by identity
      at the correct `recipient_index` despite array order). New committed self-signed chains
      `enc03-recipient-rsa2048.x509.der`, `enc03-wrong-rsa2048.x509.der` (= Bob),
      `enc03-recipient-ecp256.x509.der`; the placeholder-signature choice is disclosed in
      design note §4.1 (HUMAN review item).
    * Fuzz: `pkcs7_cms` extended with the X.509 + selection branches over the committed Alice
      key and both chains, asserting (a) every `Ok` selection's identifier re-verifies against
      the *re-parsed* blob ("never select a wrong recipient") and (b) `Certificate` mode
      **never** opens with `Matched::Structurally`; deterministic seeds `cms-alice-isn`,
      `cms-bob-ski`
      (cross-wrong-cert), `cms-carol-ec-isn`, `cms-wrong-serial`, `cms-duplicate-rid`
      (`cargo xtask pubkey-fuzz-seeds`, committed under `fuzz/seeds/pkcs7_cms/`), new
      `x509_identity` target with valid chains + truncated/corrupted/length-lying cert seeds;
      both added to the CI fuzz matrix. `Cargo.lock`/fuzz lock refreshed (path-deps only, no
      new third-party crates → check-vet unaffected; local check-vet/deny green).
    * Tests: `selis-crypto` `pkcs7::tests` (+9: certificate-identity selects the addressed
      recipient incl. decoy-first + Certificate strict/no-chain refusal; bare key →
      `Structurally`; SKI arm via extension *and* method-1; duplicate-identifier twin keep-
      trying; DER-minimality serial equality; damaged chain cert = typed malformed, never
      silently dropped; `permission_block_decoder` strictness) and `x509::tests`; engine
      `tests/pubkey_open.rs`: `pubkey_rsa_s4_selects_recipient_by_issuer_and_serial`,
      `pubkey_ec_s5_opens_with_recipient_key` (identity assertion),
      `pubkey_multi_recipient_matches_each_credential` (identity-first, indices),
      `pubkey_identity_absent_falls_through_by_match_mode` (Bob-cert+Alice-key: `auto`
      structurally opens, `certificate` fails `RECIPIENT_NO_MATCH`, no-chain+strict fails); the
      wrong-key DoD test now asserts the mode matrix.
    * Design note §4 rewritten with the shipped §4.1 (+ §1.1, §6.2, §7 updated). Gates green
      locally (`xtask lint` incl. check-contracts/alloc/layers/purity, `cargo test --workspace`).

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
  - **Note (done 2026-09-16, pending HUMAN line-by-line review — owner AI+ encryption code; box
    stays unchecked until reviewed):** Shipped scope:
    * `selis-crypto::legacy` (new): the CMS *content* ciphers, hand-rolled per §3's DER posture
      (FIPS 46-3 delta-swap DES + TDEA EDE; RFC 2268 RC2 with `rc2EffectiveKeyLength`; RC4 reuses
      the crate's existing `rc4`). `des` 0.5 / `rc2` 0.9 were **evaluated and declined**: both are
      on trait trees incompatible with this workspace's single `cipher` 0.4 (rc2 0.9 = `cipher` 0.5
      + MSRV 1.85; des 0.5 = `block-cipher` 0.3), and each is an unvetted crate for
      *museum-format-only* reads. Constants are the published tables in RustCrypto's layout
      (MIT OR Apache-2.0, licence-clean re-lay) and every entry is pinned against
      `openssl 1.1.1q enc` output — the tables are checked against an independent implementation,
      not self-consistency. **Decrypt-only:** `encrypt_block`/`encrypt` exist for the fixture
      generator + KATs; no `selis-pdf-engine` write path reaches a legacy cipher (ADR-P0019 —
      saves stay `/V 5` AESV3).
    * `pkcs7`: `CekAlgorithm` + RC4 / TDEA-CBC / RC2-CBC (their `AlgorithmIdentifier` parameters:
      absent/NULL for RC4, an 8-byte `OCTET STRING` IV for TDEA,
      `SEQUENCE { INTEGER rc2EffectiveKeyLength, OCTET STRING iv }` for RC2), the `[0]`
      **IMPLICIT** OCTET-STRING content form (what OpenSSL/Adobe actually emit — the legacy
      fixtures use it; the ENC.03 explicit `[0]{OCTET STRING}` stays accepted, both shapes fuzz
      with a *shape contract* per cipher: exactly-24 RC4 / ≥2-block 8-byte CBC / ≥2-block
      16-byte CBC). RSAES-OAEP: `KeyTransport::RsaOaep` with real `RSAES-OAEP-params` parsing
      (`[0]`/`[1]` IMPLICIT digest + `id-MGF1`, RFC 8017 A.2.1 — **not** the task sketch's
      `1.2.840.113549.1.9.16.3.9`, which is CMS's `id-alg-...` *capability* arc; the key
      transport is PKCS#1's `1.2.840.113549.1.1.7`, verified against OpenSSL's own object
      table); SHA-1/SHA-256/SHA-384/SHA-512 label hashes over SHA-1/256/384/512 MGF1, the
      DER-default (absent) parameters, and typed refusals for anything unverifiable
      (non-MGF1 mask, non-SHA digest, **non-empty `id-PSpecified` label**). `unwrap_cek`'s
      OAEP arm maps the parsed (hash, mgf1-hash) pair onto `rsa::Oaep`; unknown pairs end the
      scan as a wrong-key try. PKCS#12 refusal + `ukm`/`unprotectedAttrs` tolerate-ignored **kept
      unchanged as directed**.
    * **Wrong-key posture documented where the DoD asked for it** (`pkcs7::decrypt_payload`
      module/function docs + design note §5/§6.8): the RSAES-PKCS1-v1_5 / RSAES-OAEP transport
      unwrap runs *before* a content key exists, so RC4's missing padding is not an open silence
      channel — a wrong key cannot "slip" into a garbage file key, the scan ends
      `RECIPIENT_NO_MATCH`; the CBC families additionally run PKCS#7 + exact-24-byte-payload
      checks. Fuzz assertions back this up (never `Ok` on a shape the cipher's contract forbids).
    * `selis-pdf-cos`: `authenticate_public_key` no longer refuses `/V ≤ 3`; the RC4-era
      dictionaries route into the same identity/unwrap pass and the *documented* refusal set
      shrank to the actually-broken (§5 table): `aes192`-wrap on ECDH, OAEP variants we cannot
      verify, detached content, PasswordBasedRecipientInfo, PKCS#12. The engine's
      per-object `/V`→revision path (`decrypt_data`) is unchanged — it already knew RC4 for
      `/V ≤ 3`; only the envelope layer used to refuse the documents.
    * **Fixtures (deterministic, in-repo, no new supply-chain surface).** The task said
      "acquire real 2010–2015 RC4-CMS from the corpus only if licensed, else generate
      locally": a `corpus` search (all groups incl. the fetched wild sets' records) has **zero
      `adbe.pkcs7` public-key PDFs** — public-key documents need the recipient's private key, so
      wild corpora do not carry them — so the DoD's "three real-world cases" clause is honestly
      *open* and the shipped material is generated: `xtask pubkey-fixtures` emits
      `pubkey-rc4-s3.pdf` (RC4-128 /V 3), `pubkey-rc4-40-s3.pdf` (/Length 40 export),
      `pubkey-tdea-s3.pdf` (3DES), `pubkey-rc2-s3.pdf` (RC2), `pubkey-oaep-s5.pdf`,
      `pubkey-oaep-256-s5.pdf`; `xtask pubkey-fuzz-seeds` emits the matching `pkcs7_cms` seeds
      incl. the wrong-CEK variants (a 20-byte "TDEA" key, a 9-byte RC4 key, a 20-byte RC4 key)
      that the *RC4 no-padding* concern is about. All bytes reproduce under
      `xtask pubkey-fixtures --check`; the existing ENC.03/07/09 fixtures + seeds are unchanged
      (the 07/09 line-by-line reviews see exactly the code they were handed).
    * Nightly fuzz build repaired (task-companion, SL-1.ROB.06's flagged red): `generic-array`
      0.14.9's crate-level `#[deprecated]` turned every `Block::clone_from_slice` /
      `Output::as_slice` in `selis-crypto` into `-D warnings` errors on nightly ≥ 2026-09-11.
      Re-expressed the AES/KW/CBC paths through `Into`/deref coercions (no behaviour change) and
      added `#![deny(deprecated)]` in `selis-crypto` as the regression lock. Verified locally
      with `cd fuzz && RUSTFLAGS='-D warnings' cargo +nightly check` = clean; every fuzz target
      builds. Design note §9 records it as **R19**.
    * **Plumbing changes to the ENC.07/09 code, kept minimal and recorded (review aid):**
      `KeyTransport` gained the OAEP variant (a new enum arm + one new `unwrap_cek` match arm +
      the recipient parser's key-algorithm switch), `CekAlgorithm` gained the three legacy arms
      (+ an `Option<usize>`-valued `cek_len` and a new `accepts_cek` since RC4/RC2 carry ranges,
      *not* lengths — the call sites changed from `.cek_len()` to `.accepts_cek(cek)` because the
      check is now "the cipher can use this key" not "the key is this long"), and
      `unwrap_cek`/`decrypt_payload`/`parse_encrypted_content_info`/`parse_key_trans_recipient`
      gained the legacy/OAEP branches. Selection order, identifier matching, `MatchBy`
      semantics, `PubKeyAuth`, the permission-block decode, and the cos/engine receipt gates are
      byte-for-byte the ENC.07/09 behaviour (their tests pass unmodified).
    * Tests: `selis-crypto` (+14: DES/TDEA/RC2 vector + round-trip suites, RC4-40/128 &
      TDEA-3key/tdea/RC2 authenticate end-to-end, OAEP SHA-1 & SHA-256 (incl. the
      parameter-less DEFAULT DER), MD5-label & non-MGF1 OAEP = typed refusal, the
      implicit-`[0]` content shape, wrong-key legacy blobs = `RECIPIENT_NO_MATCH` in every
      `MatchBy`, a wrapped key a cipher cannot use = `RECIPIENT_NO_MATCH`, `der`'s signed-INTEGER
      length parse fix (a positive ≥128 CEK length — e.g. 128-bit RC4 keys' `INTEGER` DER `00 80`
      — used to be refused as negative), refuse-list); `selis-pdf-cos` (the s3-era test flips from
      the blanket `ENCRYPT_UNSUPPORTED` to the legacy accept path, still typed on garbage);
      `selis-pdf-engine::pubkey_legacy` (6 fixtures: open with identity + `Matched`, typed
      wrong-key in both modes, tolerant open without a credential).
    * Gates local: `xtask lint` (contracts/alloc/layers/purity/vet/deny/size-check) green;
      `cargo test --workspace` green; fuzz workspace + nightly `-D warnings` compile green.

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
  - **Note (done 2026-09-14, pending HUMAN line-by-line review — owner AI+ encryption code; box
    stays unchecked until reviewed):** Shipped scope (per the ENC.03 sign-off "enforce, do not
    surface"):
    * `selis-crypto::pkcs7`: the 4-byte block is now read through
      `decode_permission_block`/`payload_seed` (strictly 24-byte: any other length is the
      wrong-key detector firing — the selection loop continues / fails typed, **never** a
      default-zero grant); `PubKeyAuth::permissions` unchanged as the bit carrier.
    * `selis-pdf-cos`: public-key `/Encrypt` `/P` is now parsed (was hardcoded `0`; absent ⇒
      all-allow, matching the spec's per-recipient model where the dictionary need not carry
      `/P`).
    * `selis-policy` (the allowed COS→policy edge per `layers.toml`, first real consumer):
      `Permissions::from_cms_block` (little-endian, Table-23 bit positions), `restrict` (the
      `/P ∧ CMS` intersection — a weaker CMS grant never raises the PDF-level grant),
      `ContentOp {Print, CopyText, Annotate, Redact, Edit, FillForm}` (render-to-print,
      copy/extract text, annotate, redact/edit, fill), `check_permissions` — typed
      `PERMISSION_DENIED_BY_CMS` when forbidden; the owner/standard path keeps the SL-1.ENC.04
      `/P`-only semantics (no receipt ⇒ gate never fires). Unit tests incl. the "extract-only
      cannot annotate even though /P allows" case.
    * `selis-error`: new code **1808 `PERMISSION_DENIED_BY_CMS`** (Policy kind — 4300s belong
      to entitlements; this is the enc/permissions 1800–1999 band, never reusing 1804/1807).
    * `selis-pdf-engine`: the receipt is *consumed* — `PublicKeyReceipt`
      (cms/pdf bits + ENC.07 selection) on the session, `Session::open_as_recipient` (the
      DoD's named API; wraps the credential path, contract rustdocs included),
      `recipient_receipt`, `effective_permissions`, `check_permissions(op)` (the single choke
      point Phase-4 viewer UI and Phase-5 edit ops route through), and the first wired content
      egress: `Session::embedded_file_data` copy-gated (inventory `attachments()` stays
      viewable — ADR-P0020 posture). Annotate/redact/edit **engine ops do not exist yet**
      (Phase 5); the same `check_permissions` binds them — disclosed honestly rather than
      pretending six gates are live. CLI surfaces no credentials — the engine API is the
      gate (viewer UI = Phase 4).
    * Fixture `pubkey-perms-s5.pdf` (`cargo xtask pubkey-fixtures`, all recipients same seed →
      one file key, different 4-byte blocks): Alice = `issuerAndSerialNumber` + allow-everything
      `0xFFFF_FE78` (print/modify/copy/annotate/fill/a11y/assemble/hq), Bob =
      `subjectKeyIdentifier` + copy-only `0xFFFF_F020`, Carol = EC + view-only `0xFFFF_F000`;
      all under all-allowing `/P -4` so every denial is CMS-only. Engine
      `tests/pubkey_permissions.rs`: `each_recipient_is_bound_to_its_own_cms_grant`,
      `view_only_recipient_cannot_extract_annotate_edit`,
      `permission_bits_follow_the_opened_recipient_not_the_first_blob` (the DoD's "Bob's bits,
      not Alice's stronger ones" — bare key, structural Auto, still Bob's grant),
      `owner_path_keeps_pdf_only_semantics`, `copy_gate_intersects_cms_for_every_recipient`
      (incl. the wired `embedded_file_data` behaviour); design note gets the /P-vs-CMS
      §6.2 note + §1.1/§6.2/§7 updated. CHANGELOG entries for both features.
    * Fuzz: new `pkcs7_perms` target — the decoder either yields the exact `u32` for a
      24-byte payload, or `None`; never both halves disagree, never a length other than 24
      decodes (the "silent bypass" property); seeded with exact/short/long/empty/zero/max
      payloads (`cargo xtask pubkey-fuzz-seeds`); CI matrix row added.

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
  - **Note (2026-09-14, DoD NOT met — containment stays as shipped):** Re-tested whether the
    stack is now overflow-clean. Pinned versions confirmed in `Cargo.lock`: direct path **skrifa
    0.47.0 / read-fonts 0.44.0 / font-types 0.12.5** — all three are the *newest* crates.io
    releases as of this date; shaping path **swash 0.2.10**, also the newest release, internally
    pulling its own older **skrifa 0.44 / read-fonts 0.41** chain. All seven pinned fixtures pass
    through the containment in both dev and release builds with overflow checks forced on
    (7/7 — via `CARGO_PROFILE_RELEASE_OVERFLOW_CHECKS` for the fuzz-equivalent release profile).
    To attribute what is still *live upstream* rather than masked, the fixtures were then re-run
    with the shipped backstops deliberately removed (temporary pass-through `contain`,
    `resume_unwind`, predicate disabled — reverted, never committed): **read-fonts 0.44
    `ps/type1.rs:1444` still panics** (multiply-with-overflow — containment bypass now would be
    a live crash), and **swash 0.2.10 still panics** at `xmtx.rs:14` (×2), `metrics.rs:169` and
    `internal/cmap.rs:99` with the predicate on/off respectively — matching the known-site
    inventory exactly; no site is covered by a fixed release. Verdict per DoD: keep **all**
    containment (nothing dropped, nothing broadened, no fixture retired; the skrifa-0.47 outline
    path proves clean on the pinned glyf input but shares read-fonts 0.44, which is *not*
    overflow-clean, so the outline/metrics `contain` sites also stay). A confirmation leg was
    still run: `font_ttf`, `font_cff`, `font_type1`, `font_cmap` and `shaper`, each seeded with
    its 27 committed campaign seeds **plus the regression fixtures** (fixture bytes and the
    font-part of each shaper input), 600 s each, libFuzzer: every initial-corpus replay executed,
    all legs exited 0 with **zero crash/OOM/timeout artifacts**, and the printing hook logged
    contained panics only at the *known* sites — `type1.rs:1444` ×3 (font_type1 seed replay),
    swash `cmap.rs:99` ×1 + `parse.rs:39` ×1 (shaper seed replay); font_ttf/font_cff/font_cmap:
    none. Executions: 427.9M / 375.3M / 362.2M / 354.1M / 651.3M (≈0.6–1.1M exec/s). Leg
    honesty: cargo-fuzz does not support windows-msvc (its instrumented builds need the ELF-only
    `__start___sancov_*` section-boundary symbols; lld-link/link provide none, so even `-s none`
    fails to link),     so this leg was the same targets/hook built *uninstrumented* — seeded replay
    then random mutation (`new_units_added: 0`): a **containment confirmation**, not a discovery
    campaign; the 24 h-equivalent instrumented soak stays the Linux fuzz-soak job. Incidental CI
    finding: nightly **and** dispatch fuzz-soak jobs have been red on every target since ≥ 09-11
    — pre-existing, unrelated to fonts: `selis-crypto`'s `deprecated` `GenericArray` uses hit the
    fuzz job's `-D warnings` under newer nightly — fix that before the next instrumented leg is
    possible. Remaining steps for this task: upstream ships overflow-clean swash **and**
    read-fonts releases (or our pins acquire the fixes), then upgrade, drop the Type-1 and
    `contain` backstops, predicate, `catch_unwind` and printing hook, re-run a full instrumented
    Linux campaign leg seeded with the regression fixtures, and confirm zero contained panics —
     fixtures stay as deviation-contract pins regardless.
  - **Note (2026-09-16, nightly fuzz build un-broken):** the "Incidental CI finding" above is
    fixed on the SL-1.ENC.08 branch — `selis-crypto`'s deprecated-`generic-array` call sites
    (`Block::clone_from_slice`, `Digest::Output::as_slice`) were re-expressed as `Into`/deref
    coercions (zero behaviour change) and `#![deny(deprecated)]` now *blocks* reintroducing any,
    so the nightly `fuzz-soak` `-D warnings` gate has to stay green by construction. Verified:
    `cd fuzz && RUSTFLAGS='-D warnings' cargo +nightly check` clean. Design note §9 R19 records
    it; the instrumented Linux campaign leg this was blocking can now run.

---

## 1.OSS — Open-sourcing the parser crates (ADR-P0030)

- [x] **SL-1.OSS.01 — Split `selis-pdf-cos`, `selis-pdf-filter` for publication** · owner: AI+
  - **Do:** Confirm they depend only on L0/L1 and carry no proprietary code; add Apache-2.0
    headers, a public README, and an API-stability statement.
  - **DoD:** `cargo publish --dry-run` clean; `check-layers` proves no proprietary dependency.
- [ ] **SL-1.OSS.02 — Public repo, CI, and issue triage rota** · deps: OSS.01 · owner: HUMAN
- [ ] **SL-1.OSS.03 — Submit to OSS-Fuzz** · deps: OSS.02, SL-0.SEC.03 · owner: HUMAN
