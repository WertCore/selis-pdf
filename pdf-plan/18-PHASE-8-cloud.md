# Phase 8 — Opt-in cloud and collaboration (Months 15–21)

**Gate G8 exit criteria:** opt-in sync and real-time annotation collaboration shipping · SOC 2
Type I · DPA and sub-processor list published · the local/cloud boundary enforced by `selis-policy`
and **provable by a test that fails if any document byte leaves the device without consent**.

Read ADR-P0016 before anything in this phase. Every task here is a potential violation of the
product's central promise, so every task here carries a consent obligation.

---

## 8.BOUND — The consent boundary (build this before any service)

- [ ] **SL-8.BOUND.01 — `CloudConsent` token model** · owner: HUMAN
  - **Do:** A consent is per-document, per-feature, time-bounded, revocable, and recorded in the
    oplog. Every egress path takes a `&CloudConsent` and validates it against the document identity
    and the named feature. There is no ambient "user is logged in, so it is fine".
  - **API:** `fn upload(doc: &Doc, feature: CloudFeature, consent: &CloudConsent) -> Result<...>`
  - **DoD:** The type system makes an unconsented upload unrepresentable; a compile-fail test proves it.
- [ ] **SL-8.BOUND.02 — Egress test harness** · deps: BOUND.01 · owner: HUMAN
  - **Do:** A test rig that runs every client surface against a recording proxy and **fails the
    build** if any request body or URL contains document-derived bytes without a matching consent
    record. Run it over the whole corpus, on every shell, nightly.
  - **DoD:** A deliberately-added leak is caught. This test is the product promise; treat it as a
    release gate, not a nice-to-have.
- [ ] **SL-8.BOUND.03 — User-visible data-flow indicator** · deps: BOUND.01 · owner: AI+
  - **Do:** A persistent, honest indicator of whether the current document has ever left the
    device, and a per-document "what was sent, when, to where" log the user can read.
- [ ] **SL-8.BOUND.04 — Admin policy to disable cloud entirely** · deps: BOUND.01, SL-6.DIST.07 · owner: AI+
  - **Do:** An enterprise-lockable setting that removes cloud features from the UI and hard-fails
    any egress. Many buyers will require it; making it a supported configuration is a sales asset.

---

## 8.SYNC — Document sync

- [ ] **SL-8.SYNC.01 — Storage service with client-side encryption** · deps: BOUND.01 · owner: HUMAN
  - **Do:** Documents encrypted client-side with a key the server never sees, by default. Server-side
    features that need plaintext (OCR, convert) require a separate, explicit consent that says so
    in plain language.
  - **DoD:** A test proving the server cannot read a synced document under the default configuration.
- [ ] **SL-8.SYNC.02 — Sync protocol: revisions, conflicts, offline queue** · deps: SYNC.01 · owner: AI+
  - **Do:** Sync the incremental-revision chain (SL-1.COS.05) rather than whole files — a 200 MB
    document with one new annotation should sync a few kilobytes.
  - **DoD:** Conflict cases enumerated and tested; offline edits queue and reconcile.
- [ ] **SL-8.SYNC.03 — Third-party storage connectors** · deps: BOUND.01 · owner: AI+
  - **Do:** Google Drive, OneDrive/SharePoint, Dropbox, Box, and S3-compatible. Documents are
    fetched to the device and processed locally — the connector is I/O, not processing.
  - **Note:** This is the pragmatic version of "cloud" that most users actually want, and it costs
    us no storage or compute.
- [ ] **SL-8.SYNC.04 — Team libraries and shared folders** · deps: SYNC.02 · owner: AI

---

## 8.COLLAB — Real-time collaboration

- [ ] **SL-8.COLLAB.01 — Annotation CRDT** · deps: ADR-P0027 · owner: AI+
  - **Do:** A CRDT over annotation operations (add/modify/delete/reply/resolve) that materialises
    to standard PDF annotations on save (ADR-P0026).
  - **DoD:** Property test: any interleaving of concurrent operation sets converges to the same
    document on all replicas.
- [ ] **SL-8.COLLAB.02 — Relay service, presence, cursors** · deps: COLLAB.01 · owner: AI+
  - **Do:** The relay sees annotation *operations*, not the document. A collaborator who does not
    have the document cannot obtain it from the relay.
- [ ] **SL-8.COLLAB.03 — Comment threads, mentions, notifications** · deps: COLLAB.01 · owner: AI
- [ ] **SL-8.COLLAB.04 — Review workflows: assign, due dates, status roll-up** · deps: COLLAB.03 · owner: AI
- [ ] **SL-8.COLLAB.05 — Shareable review links with granular permissions** · deps: COLLAB.02 · owner: AI+
  - **Do:** View-only, comment, or fill — with expiry, passwords, and an access log. This is the
    lightweight-collaboration use case Smallpdf and Adobe both monetise.
- [ ] **SL-8.COLLAB.06 — Concurrent content-edit conflict UI** · deps: ADR-P0027 · owner: AI+
  - **Do:** Page-granularity last-writer-wins with an explicit conflict resolution UI showing both
    versions. Do not pretend content edits merge.

---

## 8.PROC — Cloud processing (explicitly consented)

- [ ] **SL-8.PROC.01 — Job service on `selis-pdf-service`** · deps: BOUND.01, SL-0.SBX.06 · owner: AI+
  - **Do:** Per-tenant isolation, per-job budgets, ephemeral storage with a stated retention (the
    default should be "deleted on completion"), and a published data-handling statement.
- [ ] **SL-8.PROC.02 — Server OCR at scale** · deps: PROC.01, SL-5.OCR.01 · owner: AI+
  - **Do:** Native Tesseract in an isolated worker for throughput the device cannot match, for
    users who opt in per job.
- [ ] **SL-8.PROC.03 — Office ⇄ PDF conversion service** · deps: PROC.01 · owner: AI+
  - **Do:** The honest answer for high-fidelity Office conversion. Decide the engine deliberately
    (a licensed conversion component, or headless LibreOffice with its licence implications
    reviewed under ADR-P0021 — note that running AGPL software as a service has obligations).
  - **DoD:** Fidelity benchmarked against Adobe's conversion on a labelled corpus and published.
- [ ] **SL-8.PROC.04 — HTML → PDF service (headless browser)** · deps: PROC.01, SL-5.CONV.05 · owner: AI+
- [ ] **SL-8.PROC.05 — Batch/large-job pipeline with resumability** · deps: PROC.01, SL-6.CLI.02 · owner: AI

---

## 8.ESIGN — E-signature workflow

- [ ] **SL-8.ESIGN.01 — Envelope model: documents, recipients, routing order** · deps: SL-5.SIGN.03 · owner: AI+
- [ ] **SL-8.ESIGN.02 — Field placement and signer experience** · deps: ESIGN.01 · owner: AI
- [ ] **SL-8.ESIGN.03 — Identity verification tiers** · deps: ESIGN.01 · owner: HUMAN
  - **Do:** Email/SMS OTP through to eIDAS-qualified via a QTSP partner (SL-0.LEAD.08). Be precise
    in the UI about which legal tier a given signature achieves — this is where the category is
    full of misleading claims.
- [ ] **SL-8.ESIGN.04 — Audit trail and certificate of completion** · deps: ESIGN.01 · owner: HUMAN
  - **Do:** Tamper-evident, timestamped, and embedded in the final PDF (not only in our database),
    so the evidence survives us.
- [ ] **SL-8.ESIGN.05 — Compliance posture: ESIGN Act, eIDAS, UETA** · owner: HUMAN

---

## 8.TRUST — Compliance and trust

- [ ] **SL-8.TRUST.01 — SOC 2 Type I, then Type II** · owner: HUMAN
- [ ] **SL-8.TRUST.02 — GDPR: DPA, sub-processors, DSAR, retention, deletion** · owner: HUMAN
- [ ] **SL-8.TRUST.03 — Regional data residency (EU, US, and one APAC region)** · deps: PROC.01 · owner: AI+
- [ ] **SL-8.TRUST.04 — Third-party penetration test + published summary** · owner: HUMAN
- [ ] **SL-8.TRUST.05 — Bug bounty programme** · deps: SL-0.SEC.04 · owner: HUMAN
- [ ] **SL-8.TRUST.06 — G8 review and go/no-go** · owner: HUMAN
