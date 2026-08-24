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
| **Selis Editor** | Consumer subscription, perpetual-fallback option | Everything in Tools + Reader + edit text/images, page ops, OCR, convert, optimise, redact, compare, form authoring, sign | P5 |
| **Selis Sign** | Per-seat or per-envelope | Signature workflows, identity tiers, audit trails, certification | P8 |
| **Selis Scan** | Bundled with Editor; free tier on mobile | Camera capture, dewarp, enhance, OCR, multi-page | P7 |
| **Selis Business** | Per-seat annual | Editor + collaboration, team libraries, SSO, admin console, policy, audit | P8–P9 |
| **Selis Enterprise** | Negotiated | Business + DLP, rights management, on-prem/air-gapped, FedRAMP path, SLA | P9 |
| **Selis Server** | Per-core / per-instance | Headless API, batch, self-hosted or managed | P9 |
| **Selis SDK** | Per-developer + runtime/OEM royalty | C ABI, WASM, five language wrappers, source escrow option | P9 |
| **Selis Accessibility** | Per-seat, add-on | PDF/UA validation, auto-tagging, remediation workspace, compliance reporting | P9 |

### The free/paid boundary

**Tools and Reader are both free, permanently, with no daily task limit and no account.** Every
competitor in the commodity-tools segment gates on tasks-per-day, because each conversion costs
them server compute. Ours costs us nothing — the work happens in the user's tab (ADR-P0016). That
asymmetry is the single most valuable thing about the local-first architecture commercially, and
the right move is to spend it rather than hoard it: unlimited free tools is both a better product
and a moat a server-side competitor cannot match without changing their unit economics.

Reader is free permanently, and generously so — full-fidelity viewing, search, annotation, and form
filling. It is the funnel, the credibility, and the corpus source. The paywall sits at **producing a
modified document**: saving edits, OCR output, conversion, redaction, and signing.

This boundary is defensible because it maps to real value and is easy to explain. It is also why
ADR-P0015 matters: a lapsed licence must never trap a user's work behind it.

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

**Against Adobe:** local-first, faster, a fraction of the price, no forced cloud, no subscription
hostility. Adobe's structural weakness is that its architecture and its economics both assume the
cloud; ours assume the device.

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
