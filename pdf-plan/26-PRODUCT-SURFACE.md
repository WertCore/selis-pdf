# 26 — Product Surface and SKU Map

One engine, one binary per platform, entitlement sets as products (ADR-P0014).
This document is the boundary: a capability not listed here does not get built.

---

## 1. Competitor surface → our coverage

| Capability | Adobe Acrobat | Foxit | Nitro / PDF-XChange | Smallpdf / iLovePDF | Apryse / Nutrient (SDK) | **Selis phase** |
|---|---|---|---|---|---|---|
| View, navigate, search | ✓ | ✓ | ✓ | ✓ | ✓ | **P4** |
| Browser default viewer | — | ✓ | ✓ | — | — | **P4** |
| Unlock / add password, permissions | ✓ | ✓ | ✓ | ✓ | ✓ | **P1A** |
| Images → PDF | ✓ | ✓ | ✓ | ✓ | ✓ | **P1A** |
| Annotate, comment | ✓ | ✓ | ✓ | partial | ✓ | **P5** |
| Fill forms | ✓ | ✓ | ✓ | partial | ✓ | **P5** |
| Create / edit forms | ✓ | ✓ | ✓ | — | ✓ | **P5** |
| Edit text and images | ✓ | ✓ | ✓ | — | partial | **P5** |
| Page operations, merge, split | ✓ | ✓ | ✓ | ✓ | ✓ | **P1A (mo 4)** |
| OCR | ✓ | ✓ | ✓ | ✓ | ✓ | **P5** |
| Redaction | ✓ | ✓ | ✓ | — | ✓ | **P5** |
| Digital signatures | ✓ | ✓ | ✓ | — | ✓ | **P5** |
| E-signature workflow | ✓ | ✓ | ✓ | ✓ | — | **P8** |
| Compare documents | ✓ | ✓ | partial | — | ✓ | **P5** |
| Convert to/from Office | ✓ | ✓ | ✓ | ✓ | ✓ | **P5** local · **P8** high-fidelity |
| Optimise / compress | ✓ | ✓ | ✓ | ✓ | ✓ | **P1A** lossless · **P2** lossy |
| Batch processing | ✓ | ✓ | ✓ | — | ✓ | **P1A** web · **P6** CLI |
| CLI | — | — | partial | — | ✓ | **P6** |
| Desktop apps | ✓ | ✓ | ✓ | — | — | **P6** |
| Mobile apps | ✓ | ✓ | partial | ✓ | — | **P7** |
| Mobile scan | ✓ | ✓ | — | ✓ | — | **P7** |
| Virtual printer | ✓ | ✓ | ✓ | — | — | **P6** (optional; see `SL-6.OS.06`) |
| Cloud storage / sync | ✓ | ✓ | partial | ✓ | — | **P8** opt-in |
| Real-time collaboration | ✓ | partial | — | — | ✓ | **P8** |
| PDF/A archiving | ✓ | ✓ | ✓ | — | ✓ | **P9** |
| PDF/UA accessibility + remediation | ✓ | partial | partial | — | partial | **P9** |
| Prepress / PDF/X | ✓ | partial | — | — | ✓ | **P9** (optional) |
| Developer SDK | ✓ | ✓ | — | — | ✓ | **P9** |
| Server / API | ✓ | ✓ | — | ✓ | ✓ | **P9** |
| Enterprise admin, SSO, DLP | ✓ | ✓ | partial | partial | — | **P9** |
| Self-hosted / air-gapped | — | partial | — | — | ✓ | **P9** |
| **Fully local processing** | — | — | partial | — | ✓ (SDK) | **P1A** — but `ihatepdf.cv` already ships this. No longer a wedge. |
| **Published conformance data** | — | — | — | — | — | **P4 — still nobody does this. Now the primary wedge.** |
| **Per-day task limits** | n/a | n/a | n/a | ✓ *(the paid ones do)* | n/a | **never — see §2** |
| **Extension w/ viewer takeover** | — | ✓ | ✓ | — | — | **P1A — `ihatepdf.cv` is web-only (verify). Primary differentiator.** |

The last two rows are the strategy. Everything above them is table stakes we must reach; those two
are why a buyer would switch.

---

## 2. SKUs

| SKU | Price shape | Entitlements | Ships |
|---|---|---|---|
| **Selis Tools** | Free, unlimited, no account | Merge, split, page ops, unlock, protect, compress, images→PDF, metadata. Local-only. | **P1A (mo 4)** |
| **Selis Reader** | Free, forever | View, navigate, search, select/copy, print, form *fill* (not save), annotate for personal use, extension | P4 |
| **Selis Editor** | **Free for individuals** (desktop + web); commercial deployment is paid | Everything in Tools + Reader + edit text/images, page ops, OCR, convert, optimise, redact, compare, form authoring, sign | P5 |
| **Selis Sign** | Per-seat or per-envelope | Signature workflows, identity tiers, audit trails, certification | P8 |
| **Selis Scan** | Bundled with Editor; free tier on mobile | Camera capture, dewarp, enhance, OCR, multi-page | P7 |
| **Selis Business** | Per-seat annual | Editor + collaboration, team libraries, SSO, admin console, policy, audit | P8–P9 |
| **Selis Enterprise** | Negotiated | Business + DLP, rights management, on-prem/air-gapped, FedRAMP path, SLA | P9 |
| **Selis Server** | Per-core / per-instance | Headless API, batch, self-hosted or managed | P9 |
| **Selis SDK** | Per-developer + runtime/OEM royalty | C ABI, WASM, five language wrappers, source escrow option | P9 |
| **Selis Accessibility** | Per-seat, add-on | PDF/UA validation, auto-tagging, remediation workspace, compliance reporting | P9 |

### The free/paid boundary

The boundary is **individual vs. commercial**, not view vs. modify. The authoritative statement of
the monetization model is **`27-COST-AND-LICENSING.md §4A`** (proposed ADR-P0046); this section
states how it lands on the SKU grid above.

**For individuals, the whole product is free, forever — the web app *and* the desktop app, with no
account, no watermark, and no daily task limit.** That includes editing, OCR, conversion, redaction,
and signing, not just viewing and the tools surface. Every competitor in the commodity-tools segment
gates on tasks-per-day, and the incumbents (Acrobat, Foxit) charge individuals for the editor; both
do so because the work costs *them* server compute or they price the desktop app as the product.
Ours costs us nothing — the work happens in the user's tab or on their own machine (ADR-P0016) — so
the right move is to give individuals the full product and spend that asymmetry rather than hoard it.
A free, genuinely complete, privacy-respecting Acrobat alternative is both a better product and a
moat a server-side competitor cannot match without changing their unit economics; it is also the
wedge that builds the audience and corpus the paid SDK is sold into.

**The paid relationship begins at commercial / enterprise use**, not at the individual's first edit.
Revenue is the **SDK** (per-developer + runtime/OEM royalty), the **Server/API**, **commercial
deployment** of the apps/CLI across an organisation's fleet, and the **Business / Enterprise /
Accessibility** add-ons (SSO, admin, DLP, air-gap, compliance, SLA, support) — exactly the rows in
the table above whose price shape is per-seat, per-core, per-developer, or negotiated. This is the
Aspose/iText per-developer + per-deployment commercial model with the consumer side inverted: the
incumbents charge individuals and enterprises; Selis charges only enterprises.

The line between individual and commercial is **honour-based, not DRM** — a plain-language licence
plus a CLA on the SDK code, not a technical lock on the local app (`27 §4A.4`). This is a deliberate
consequence of ADR-P0014 (one binary, entitlements at runtime) and ADR-P0015 (a lapsed or absent
licence must never trap a user's work behind it): a hard lock on a *local* app would contradict both
and break the privacy guarantee it is built on. Enforcement sits where it is both enforceable and
material — the SDK and Server are the paid artefacts, sold under a commercial licence with support,
updates, indemnity, and source-escrow that a cracked local binary cannot provide; an enterprise
cannot deploy unlicensed software across a fleet and pass an audit.

> **Historical note (line moved).** Earlier drafts of this section drew the individual paywall at
> "producing a modified document" (free Reader, paid consumer Editor subscription). The product-owner
> decision in `27 §4A.6` moved it: individuals now get the full apps free, and the paid line is
> commercial/enterprise use. The Editor SKU above is reframed accordingly — it is the free
> individual app, with the *commercial deployment* of it as the paid artefact.

---

## 3. Entitlement matrix (`selis-policy::Feature`)

```text
Feature                     Reader  Editor  Business  Enterprise  Server  SDK
─────────────────────────────────────────────────────────────────────────────
View / Search / Print         ✓       ✓        ✓          ✓         ✓      ✓
Annotate (save)               —       ✓        ✓          ✓         ✓      ✓
FormFill (save)               —       ✓        ✓          ✓         ✓      ✓
EditText / EditImage          —       ✓        ✓          ✓         ✓      ✓
PageOps / Merge / Split       —       ✓        ✓          ✓         ✓      ✓
OCR                           —       ✓        ✓          ✓         ✓      ✓
Convert                       —       ✓        ✓          ✓         ✓      ✓
Redact                        —       ✓        ✓          ✓         ✓      ✓
Sign                          —       ✓        ✓          ✓         ✓      ✓
Compare                       —       ✓        ✓          ✓         ✓      ✓
Batch                         —       ✓        ✓          ✓         ✓      ✓
Collaborate                   —       —        ✓          ✓         —      ✓
SSO / SCIM                    —       —        ✓          ✓         —      —
AdminPolicy                   —       —        ✓          ✓         ✓      —
DLP / RightsManagement        —       —        —          ✓         —      —
SelfHosted                    —       —        —          ✓         ✓      ✓
PdfaAuthor / PdfuaAuthor      —       add-on   add-on     ✓         ✓      ✓
```

One enum, evaluated at runtime, in one binary. Adding a SKU is a row in a table, not a build.

---

## 4. Positioning

**Against Adobe:** local-first, faster, no forced cloud, no subscription hostility — and for an
individual, **free**, where Acrobat is a subscription. Adobe's structural weakness is that its
architecture and its economics both assume the cloud and a paying individual; ours assume the device
and monetise the enterprise instead (`27 §4A`).

**Against Foxit / Nitro / PDF-XChange:** better engineering visibility (published conformance),
better privacy, modern web-first distribution, a real SDK. They compete with Adobe on price; we
compete on trust and honesty.

**Against Smallpdf / iLovePDF:** they upload your document to do a page rotation. We do it in the
tab. That comparison still sells itself — but it no longer separates us from every competitor.

**Against `ihatepdf.cv` and other local-first tool sites (the real fight):** they hold the same
privacy position with 46 tools already shipped. See `28-FIRST-RELEASE-SCOPE.md §2`. Do not compete on
tool count. Compete on: the **browser extension** (a channel they appear not to occupy), **provable
correctness** on documents that break glue-code implementations (malformed files, CJK, tagged PDFs,
forms, 500 MB documents), **real redaction** rather than a black rectangle, and the **Privacy Risk
Scanner** — which turns the conformance investment into a visible user-facing feature.

**Against Apryse / Nutrient (SDK):** a genuine alternative in a market with very few, at a lower
price, with a permissively-licensed open parser core (ADR-P0030) that de-risks adoption.

---

## 5. What we will not build

- A general-purpose office suite, or Word/Excel editing.
- A design tool.
- An AI chatbot bolted onto a document viewer. Structured extraction (`SL-9.PLAT.04`) is exposed
  through the SDK and API so *others* can build that, on-device, without our shipping a feature
  whose quality we cannot gate on a corpus.
- Dynamic XFA authoring.
- A native plugin ABI (see `01-ARCHITECTURE.md §13`).
- Anything that requires uploading a document by default.
