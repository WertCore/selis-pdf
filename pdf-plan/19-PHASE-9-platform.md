# Phase 9 — Platform, SDK, enterprise, compliance (Months 18–30)

**Gate G9 exit criteria:** C ABI GA plus Python/Node/Go/Java wrappers · server API GA · enterprise
admin console and MDM/GPO deployment · PDF/A and PDF/UA conformance products validated by veraPDF ·
at least one OEM letter of intent.

This is where the platform thesis pays off: the engine that shipped seven products becomes a
product itself.

---

## 9.SDK — Developer SDK

- [ ] **SL-9.SDK.01 — C ABI 1.0 with a stability promise** · deps: SL-7.FFI.01 · owner: AI+
  - **Do:** Freeze the ABI, document every function, define the deprecation policy, and publish a
    12-month stability guarantee. Add an ABI-compatibility CI check against the previous release.
- [ ] **SL-9.SDK.02 — Wrapper generation from one IDL** · deps: SDK.01 · owner: AI+
  - **Do:** Python, Node (N-API), Go (cgo), Java (JNI/FFM), and .NET wrappers generated from a
    single description so they cannot drift. Idiomatic error handling per language.
  - **DoD:** The same test suite runs against all five wrappers.
- [ ] **SL-9.SDK.03 — WASM SDK for third-party web apps** · deps: SL-4.WASM.01 · owner: AI+
  - **Do:** `@selis/pdf` as a public npm package with a documented API, TypeScript types, and a
    licence-key mechanism.
  - **Note:** This directly targets the market PSPDFKit/Nutrient and Apryse serve. It is plausibly
    the highest-margin product in the whole plan.
- [ ] **SL-9.SDK.04 — Documentation site, samples, and a live playground** · deps: SDK.02 · owner: AI
- [ ] **SL-9.SDK.05 — SDK licensing and metering** · deps: SL-5.BIZ.01 · owner: AI+
  - **Do:** Per-seat, per-server, and OEM royalty models; offline licence validation; usage
    reporting that does not phone home with customer document data.
- [ ] **SL-9.SDK.06 — Migration guides from PDFium, pdf.js, iText, PDFBox, Apryse** · owner: AI
  - **Note:** An API-shape mapping table is the single most effective piece of SDK marketing.

---

## 9.SERVER — Server product

- [ ] **SL-9.SERVER.01 — `selis-pdf-service` HTTP/gRPC API GA** · deps: SL-8.PROC.01 · owner: AI+
  - **Do:** Render, extract, convert, merge, split, OCR, sign, redact, optimise, and form-fill as
    API operations with per-tenant budgets and strict document isolation.
- [ ] **SL-9.SERVER.02 — Self-hosted deployment: container, Helm chart, air-gapped** · deps: SERVER.01 · owner: AI+
  - **Note:** Air-gapped self-hosting is the natural extension of the local-first promise and the
    thing regulated buyers ask for first.
- [ ] **SL-9.SERVER.03 — Horizontal scaling, queueing, autoscaling** · deps: SERVER.01 · owner: AI+
- [ ] **SL-9.SERVER.04 — Observability: metrics, traces, per-tenant quotas** · deps: SERVER.01 · owner: AI
- [ ] **SL-9.SERVER.05 — Managed hosted tier** · deps: SERVER.03 · owner: AI+

---

## 9.ENT — Enterprise

- [ ] **SL-9.ENT.01 — Admin console: users, licences, policy, usage** · deps: SL-5.BIZ.01 · owner: AI+
- [ ] **SL-9.ENT.02 — SSO: SAML, OIDC, SCIM provisioning** · deps: ENT.01 · owner: AI+
- [ ] **SL-9.ENT.03 — Centralised policy management** · deps: SL-6.DIST.07, SL-8.BOUND.04 · owner: AI+
  - **Do:** Every ADR-P0016/P0017/P0020 toggle centrally enforceable, with a compliance report.
- [ ] **SL-9.ENT.04 — Audit logging and eDiscovery export** · deps: SL-8.TRUST.02 · owner: AI+
- [ ] **SL-9.ENT.05 — DLP and classification integration** · deps: ENT.03 · owner: AI+
  - **Do:** Read and honour Microsoft Purview / Azure Information Protection labels; refuse actions
    a label forbids.
- [ ] **SL-9.ENT.06 — Rights management (encrypt-to-identity, revocable access)** · deps: SL-1.ENC.03 · owner: HUMAN
- [ ] **SL-9.ENT.07 — Volume licensing, procurement, and FedRAMP/StateRAMP assessment** · owner: HUMAN

---

## 9.COMPL — Compliance products

- [ ] **SL-9.COMPL.01 — PDF/A creation and validation** · deps: SL-5.EDIT.05 · owner: AI+
  - **Do:** Convert to PDF/A-1b/2b/2u/3b/4, and validate with our own rule engine cross-checked
    against veraPDF in CI (ADR-P0009).
  - **DoD:** Zero disagreements with veraPDF on the Isartor and veraPDF corpora, or every
    disagreement documented and justified.
- [ ] **SL-9.COMPL.02 — PDF/UA validation and remediation** · deps: ADR-P0031 · owner: AI+
  - **Do:** Full rule engine, plus a *remediation* workflow: reading-order editor, tag tree editor,
    alt-text authoring, table-structure editor, and language marking.
  - **Note:** Accessibility remediation is an underserved, growing, high-priced segment, and our
    Phase-1 structure-tree decision is what makes entering it cheap.
- [ ] **SL-9.COMPL.03 — Accessibility auto-tagging** · deps: COMPL.02, SL-5.OCR.03 · owner: AI+
- [ ] **SL-9.COMPL.04 — PDF/X and prepress** · deps: SL-2.COLOR.07 · owner: AI
  - **Do:** Only if the prepress market is pursued — output intents, overprint preview, ink
    coverage, trim/bleed boxes, preflight profiles.
- [ ] **SL-9.COMPL.05 — PDF 2.0 feature completion** · owner: AI+
  - **Do:** Unencrypted wrapper documents, AES-256 refinements, page-level output intents, black
    point compensation, and the new annotation/association features.

---

## 9.PLAT — Platform completion

- [ ] **SL-9.PLAT.01 — WASM-component plugin model** · owner: AI+
  - **Do:** Revisit `01-ARCHITECTURE.md §13`. If a plugin system happens, it is sandboxed WASM
    components with a capability-based API — never a native ABI.
- [ ] **SL-9.PLAT.02 — Own JPEG 2000 decoder** · deps: ADR-P0018 · owner: AI+
  - **Do:** Retire the OpenJPEG dependency if JPX volume justifies it.
- [ ] **SL-9.PLAT.03 — Integrations: Office add-ins, Google Workspace, Slack, Teams, Zapier** · owner: AI
- [ ] **SL-9.PLAT.04 — Document-intelligence surface** · deps: SL-3.TEXT.07 · owner: AI+
  - **Do:** Structured extraction (tables, key-value pairs, forms) exposed via SDK and API. Keep it
    on-device-capable by default — a local-first document-understanding product is a genuinely
    differentiated position, and the structured-extraction output from Phase 3 is already the
    hard part.
- [ ] **SL-9.PLAT.05 — OEM/embedded licensing programme** · deps: SDK.05 · owner: HUMAN
- [ ] **SL-9.PLAT.06 — Tier A crate graduation review** · deps: SDK.05 · owner: HUMAN
  - **Do:** Assess each Tier A crate (`selis-font`, `selis-shape`, `selis-raster`, `selis-image`,
    `selis-ocr`) against the four ADR-P0034 graduation criteria: 1.0 with no breaking change for
    two consecutive quarters, a production consumer outside the workspace, a named maintainer, and
    CI green without Selis-private fixtures. Extract those that pass via
    `git filter-repo --subdirectory-filter`. Tier B crates stay in the workspace permanently.
  - **DoD:** The crates.io name is unchanged for every extracted crate, so no downstream consumer
    observes a version bump or a re-import.
- [ ] **SL-9.PLAT.07 — Desktop shell profiling and the native-GUI decision** · deps: SL-6.QUAL.02 · owner: HUMAN
  - **Do:** Profile the shipped Tauri shell on a low-end target — 4 GB RAM, 2015-era dual-core,
    integrated graphics. Measure idle RSS, RSS with a 50-page PDF open, cold-start to first page,
    and sustained scroll fps. Separate the *shell* baseline from the *engine* footprint; only the
    former is what a framework change would recover.
  - **Then:** Decide against ADR-P0036. Prototype a Slint or per-platform-native chrome against the
    same numbers only if the shell baseline is a material share of the `03-CONVENTIONS.md §12`
    ≤ 400 MB budget. Re-check Slint's licence against ADR-P0030 and whether Xilem has reached 1.0.
  - **DoD:** A decision recorded in ADR-P0036 with the measurements behind it. "Keep Tauri" is a
    perfectly good outcome and must be equally documented.
  - **Note:** Deliberately last. Before this runs, the cheaper lever for low-end hardware is the
    budget kernel — tile-cache ceilings, lazy page loading, resolution scaling under pressure.
- [ ] **SL-9.PLAT.08 — G9 review** · owner: HUMAN
