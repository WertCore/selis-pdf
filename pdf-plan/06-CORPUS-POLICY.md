# 06 — Corpus handling policy (wild-corpus)

> **Status:** adopted · **Owner:** SL-0.CORP.05 · **Enforcement:** manual at first; automated
> controls listed in §7 land with the acquisition tooling.
> **Companion:** the acquisition plan lives in `10-PHASE-0-foundation.md` (SL-0.CORP.05).

This policy governs every "wild" corpus file — real-world documents acquired from the web
(Common Crawl-derived data, the SAFEDOCS corpus, govdocs1) as opposed to corpora published as
test suites (pdf.js, veraPDF, PDF Association examples). Wild documents carry unknown third-party
copyright and may contain personal data. The policy's purpose is to make our use of them lawful,
minimal, and reproducible, without polluting the shipped product or the git history.

## 1. Legal basis and licence posture

| Source | Copyright posture | What it allows us | What it forbids |
| --- | --- | --- | --- |
| Common Crawl (raw WARC or index) | Content is third-party; CC grants a limited licence to use the Service and Crawled Content under its [ToU](https://commoncrawl.org/terms-of-use) (March 7, 2024) | Fetching, parsing, analysing, testing, aggregate statistics, scientific research | Redistribution of crawled content as content; PII harvesting for use separately from the corpus; unlawful or privacy-invading use; using it to train/deploy AI systems (indemnity exposure for us) |
| SAFEDOCS / CC-MAIN-2021-31-PDF-UNTRUNCATED (Digital Corpora) | Same third-party content posture; Digital Corpora's *site and metadata* are CC0 | Same as Common Crawl, via an already-extracted, provenance-annotated distribution | Same; plus we do not republish the corpus or its metadata |
| govdocs1 (Digital Corpora) | US Government public-domain works (site terms: CC0 for site material) | Everything, including redistribution of the govdocs1 files themselves | — |
| Seed corpora (pdf.js, veraPDF, pdf20examples) | Apache-2.0 / CC0 / CC-BY-SA-4.0 | Full open-source use per each licence | CC-BY-SA share-alike obligations if we ever vendored those files (we do not — fetch-only) |

Common Crawl's ToU explicitly contemplates research use. Parsing PDFs to measure our engine's
robustness is squarely inside that; the ToU's prohibited-use list (privacy invasion, PII
harvesting "for use separately from the Crawled Content", AI training) is respected by design
under this policy. The SAFEDOCS distribution additionally documents Common Crawl's licence as the
upstream source; it imposes the same posture, so acquiring via SAFEDOCS is legally equivalent to
crawling CC ourselves — with far less engineering and no robot-protocol exposure.

## 2. Redistribution

- **No redistribution of wild corpus files. Not in the repo, not in CI artefacts, not in bug
  reports, not in screenshots, not in marketing material.**
- `corpus/pdfs/` is already gitignored (SL-0.CORP.01); the wild cache lives outside the repo
  entirely (default `%USERPROFILE%\.cache\selis-corpus\wild`, overridable with
  `SELIS_WILD_CORPUS_CACHE`) so nothing wild can be committed even by accident.
- CI never fetches wild corpora. CI runs on the seed corpora only; a pipeline that needed wild
  files would produce redistribution-capable artefacts (compressed test runs) by default.
- Expectation records (`corpus/expect/*.toml`) derived from wild files contain *outcomes and
  hashes only* — never document content — and are committable. If a record must quote document
  bytes to be useful, the file is removed from the corpus instead.
- A PR that fixes a bug triggered by a wild file gets a *reduced, synthetic* reproduction (the
  synthetic generator, SL-0.CORP.04, or a hand-built minimal file). The wild original never
  leaves the local cache.

## 3. Encryption and storage

- Wild files are stored **encrypted at rest** on every machine that holds them:
  - Native store: a BitLocker/LUKS-encrypted volume or `SELIS_WILD_CORPUS_CACHE` on such a
    volume; on Windows the dev disk is BitLocker-encrypted (Device Encryption), which satisfies
    this by default.
  - The acquisition script refuses to write into a non-encrypted location when it can detect it,
    and always warns.
- Access is limited to developer workstations; the corpus is never copied to portable media, a
  server, or a cloud drive.
- Deletion policy: a machine leaves the project → the wild cache is deleted; the acquisition
  script can re-fetch it (that is the point of fetch-only provenance). The wildcard cache is
  considered disposable *by design* — nothing of value is lost by deleting it.

## 4. Human review

- **Default: no human eyes on wild document content.** Triage works on metadata: file size,
  structural parse results, error codes, hashes, page counts. That is enough for robustness
  engineering.
- Exception ("with cause"): a failing case whose *structure* (not content) must be understood —
  e.g. a suspected engine bug that the structural diff cannot localise. Then the minimum
  necessary page(s) are rendered or text-extracted, on a developer workstation, never pasted
  into issues/PRs/any shared medium.
- The exception requires recording in the local triage log: file hash, date, reason. The log is
  local-only.
- Any file discovered to contain obviously sensitive material (a watermarked exam paper, an
  invoice, a medical letter) is flagged locally (`quarantine` marker in the cache) and excluded
  from all runs; a `.quarantine` list of hashes is synced so every workstation honours it.

## 5. Provenance

Every acquisition batch records, in the batch directory (outside the repo,
`<batch>/provenance.json`), never committed:

- the upstream dataset (SAFEDOCS zip name / Common Crawl crawl id + WARC segment),
- fetch timestamp, downloader version, sha256 of every zip fetched,
- the Common Crawl / Digital Corpora terms snapshot URL and date,
- the policy version in force (this document's revision).

The expectation records reference files by content hash, so the corpus can be rebuilt exactly
from upstream (stable SAFEDOCS zip indices) plus the batch plan, indefinitely. This is the
"fetch rather than vendor" rule of SL-0.LEGAL.04 applied to wild data; we deliberately do not
commit even the URL lists of wild documents.

## 6. Opt-out and takedown

- Common Crawl operates an opt-out ledger; the SAFEDOCS corpus predates it but derives from the
  same crawl. If a rights-holder asks us to stop using a document: delete it from the cache,
  add its hash to the local quarantine list, and (only if the file is a recurring triage target)
  replace it with a synthetic lookalike in the synthetic corpus.
- We never challenge a takedown; there is no engineering value in the specific bytes of any one
  wild document.

## 7. Automated controls (status)

1. **[implemented]** `xtask corpus wild fetch --i-have-read-the-policy` (xtask/src/wild.rs):
   refuses without the acknowledgement, refuses a cache inside the repo, refuses a volume
   Windows can prove is BitLocker-off (warns when unverifiable), free-space floor per zip,
   writes `<batch>/provenance.jsonl`, extracts via system tools. `xtask corpus wild status`
   lists batches.
2. **[implemented]** `xtask check-wild-hygiene` (xtask/src/wild_hygiene.rs): fails if any CI
   workflow references a wild source (commoncrawl/digitalcorpora/fetch-wild.ps1/`corpus wild`)
   and fails if any `corpus/expect/*.toml` record exceeds the metadata-only size bound
   (content leak). Wired into `cargo xtask lint`, i.e. the CI lint job; unit-tested.
3. **[pending]** Fuzzing inputs derived from wild files are *seed material only*
   (`fuzz/corpus/` is gitignored); libFuzzer minimises them, and minimised crash inputs are
   still treated as wild (never committed).
4. **[pending]** Bug-report tooling (`xtask oracle triage --export`) strips document bytes;
   exports carry hashes and structure only.

## 8. Scope and precedence

- This policy binds all contributors and all automated agents (including AI agents running in
  this repo). Where it conflicts with convenience, it wins.
- The seed-corpus licence decisions are in `corpus/manifests/*.toml` and supersede nothing here;
  wild corpora are the strict subset of handling rules in this document.
- Changes to this policy require a plan-file edit and a commit that references SL-0.CORP.05.
