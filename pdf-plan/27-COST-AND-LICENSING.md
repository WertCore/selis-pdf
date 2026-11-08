# 27 — Cost and Licensing Strategy

**Verified: August 2026.** Prices and free-access arrangements in this document drift. Rows marked
**[STABLE]** are licence facts that change rarely; rows marked **[VERIFY]** are prices or access
arrangements that must be re-checked before you rely on them. Sources are listed in §7.

---

## 0. Context for a reader without the rest of the plan

This document is one file of a larger engineering plan for **Selis**, a PDF product suite (browser
extension, web app, desktop, mobile, SDK) built on a from-scratch Rust PDF engine. It answers two
separate questions. The first is a **cost** question — *what money must actually be spent to build
and ship, and when* (§§1–4). The second is a **business-model** question — *who pays, who does not,
and what exactly is sold* (§4A). The two are related but distinct: §§1–4 argue the costs are small;
§4A argues the revenue comes from a specific place and the rest of the product is free on purpose.

Four decisions
from that plan are referenced below and are treated here as given:

- **Own engine.** The PDF engine is written from scratch in Rust. PDFium, pdf.js, qpdf, MuPDF and
  Ghostscript are used only as *test oracles* in CI — run as separate processes, never linked or
  shipped. (Plan reference: ADR-P0009.)
- **Local-first.** All document processing happens on the user's device (native or WebAssembly).
  Nothing is uploaded without explicit per-document consent. There is no server-side processing in
  the default product, therefore no per-document infrastructure cost. (ADR-P0016.)
- **Partial open source.** Three parser crates (`selis-pdf-cos`, `selis-pdf-filter`, `selis-font`) are published
  under Apache-2.0. The renderer, edit model, redaction, signing and all product code stay
  proprietary. (ADR-P0030.)
- **Beachhead is web + browser extension**, shipping before desktop or mobile.

Phase numbering used below: **Phase 0–1** = engine foundations (months 0–2); **Phase 1A** = the PDF
toolkit — merge, split, unlock, compress (month 4, first release); **Phase 4** = viewer (month 7);
**Phase 5** = editor (month 12); **Phase 6–7** = desktop and mobile (months 15–18); **Phase 9** =
SDK, enterprise and compliance products (month 30).

The question this document answers: *"I can't afford costly licences in the initial phase, with
competitors already established. Will open-sourcing the core engine resolve this?"*

> **Short answer: the costs being worried about are mostly not real, and open-sourcing would not
> address the ones that are.**

---

## 0.1 Plain-language summary (read this first)

**There is no PDF licence you have to pay for.** PDF is an open ISO standard and Adobe granted a
royalty-free patent licence for it. You can build any PDF feature and sell it. Nobody charges you,
and nobody can stop you exposing a feature you have built.

**What costs money is the rulebook, not permission.** PDF/A and PDF/UA are sets of rules written
down in documents that ISO sells, roughly CHF 200 (~₹20 000) each. You are buying a book, the way
you would buy a textbook. Once you have read it and written the code, shipping that code is free
forever.

**Therefore "build the feature but hide it because we lack a licence" is not a real strategy:**

- To build PDF/A correctly you must first know the rules — so you would already have bought the
  book. Hiding the result saves nothing.
- Once built, nothing prevents you exposing it. There is no licence gate to hide from.

**What the plan actually does is simpler: build PDF/A and PDF/UA later** — at Phase 9 — because no
user of the first products needs them. Someone merging two PDFs does not care about archival
conformance; a government records office does, and you will sell to them in year three. Buy the
documents then, when the cost is irrelevant.

**And you may never need to buy them at all.** veraPDF and the Matterhorn Protocol are free, are
derived from the standards, and are good enough to build against. You would buy the official ISO
text before *claiming* certified conformance to a procurement officer, not before writing the code.

**The complete list of things that actually cost money:**

| What | Cost | When |
|---|---|---|
| Chrome Web Store account | $5, one time | Now |
| Apple Developer Program | $99/yr | Only when a Mac or iPhone app ships |
| Windows code signing | ~$120/yr | Only when a Windows app ships |
| Google Play account | $25, one time | Only when an Android app ships |
| ISO PDF/A + PDF/UA documents | ~CHF 200 each × 6 | Year 3, optional |

Everything else in this document — every specification, test corpus, and font needed to build the
engine — is free.

---

## 1. Three different things called "licensing"

| | What it is | Does open-sourcing help? |
|---|---|---|
| **A. Procurement** | Specs, memberships, certificates, store accounts, insurance | **No.** Unaffected by how you license your own code. |
| **B. Inbound licences** | What *you* may use — copyleft (GPL/AGPL) vs permissive dependencies | **Marginally** — see §4. |
| **C. Outbound licence** | What *others* may do with your code; the revenue model | **The real question**, and it is a business-model decision, not a cost decision. |

The concern is about A. The proposed remedy addresses B and C. They do not connect.

---

## 2. Procurement: what it costs and when it is due

### 2.1 Free, permanently — acquire in week 1

| Item | Source | Status |
|---|---|---|
| **ISO 32000-2 (PDF 2.0)** — the core spec | PDF Association sponsored access | **[VERIFY]** Free as of Aug 2026. Bundle includes ISO 32000-2:2020, Amd 1, ISO/TS 32001:2022, ISO/TS 32002:2022, with industry errata; last updated June 2026. Sponsored by Adobe, Apryse and Foxit. |
| **PDF Reference 1.7** (Adobe's own publication) | Adobe | **[STABLE]** Free. Technically equivalent in substance to ISO 32000-1 and better-written prose in several areas. **Note:** ISO 32000-1 *as an ISO document* is a paid ISO purchase — you do not need it; use Adobe's edition. |
| **ITU-T T.4, T.6** (CCITT Group 3/4 fax) | itu.int | **[STABLE]** ITU publishes all in-force Recommendations free of charge. |
| **ITU-T T.88** (JBIG2) | itu.int | **[STABLE]** Free. |
| **ITU-T T.800** (JPEG 2000) | itu.int | **[STABLE]** Free. |
| **OpenType specification** | Microsoft | **[STABLE]** Free. |
| **CFF, Type 1, Type 2 charstring specs** | Adobe | **[STABLE]** Free. |
| **Adobe Font Metrics + standard-14 metrics** | Adobe | **[STABLE]** Free. Required for metric-compatible substitution of the 14 standard fonts. |
| **Adobe CMap resources** | GitHub, BSD-3 | **[STABLE]** Free *and* redistributable — you may ship them. |
| **Adobe Glyph List** | Adobe | **[STABLE]** Free. |
| **XMP specification, XFA specification** | Adobe | **[STABLE]** Free. |
| **ICC.1 colour profile spec** | color.org | **[STABLE]** Free. |
| **Matterhorn Protocol** | PDF Association | **[STABLE]** Free. Enumerates every PDF/UA failure condition — **more directly useful for implementation than ISO 14289 itself.** |
| **Isartor test suite** | PDF Association | **[STABLE]** Free. PDF/A-1 negative test corpus. |
| **veraPDF validator + corpus** | GitHub | **[STABLE]** Free, open source. Encodes the PDF/A and PDF/UA rules as executable tests. GPL/MPL — usable as an external CI tool, not as a linked library. |
| **pdf.js test corpus** | GitHub | **[STABLE]** Free. |
| **Ghent Workgroup output suite** | gwg.org | **[STABLE]** Free. |
| **Liberation fonts** (SIL OFL), **TeX Gyre** (GUST FL), **Noto** (SIL OFL) | Google / GUST | **[STABLE]** Free, embeddable, redistributable. Covers metric-compatible Helvetica/Times/Courier substitutes and CJK fallback. No purchase, no legal review needed — read the licences. |

**Every specification required to build the engine through the editor release (Phase 5, month 12)
is free.**

### 2.2 Paid, but deferrable

| Item | ≈ Cost | Due at | Why it waits |
|---|---|---|---|
| ISO 19005 (PDF/A), 4 parts | **[VERIFY]** ISO list prices, roughly CHF 60–220 per part depending on length | **Phase 9** | Not in the PDF Association sponsored bundle as of Aug 2026 — but that bundle has grown over time (it gained ISO/TS 32001 and 32002), so **re-check pdfa.org/sponsored-standards before buying**. Meanwhile veraPDF + Isartor encode the rules as executable tests. |
| ISO 14289 (PDF/UA), 2 parts | **[VERIFY]** same range | **Phase 9** | Same re-check. The free Matterhorn Protocol is more actionable for implementation. |
| PDF Association membership | **[VERIFY]** Not published publicly. Tiered by company size; there is a **"Limited Full Membership"** tier for companies with only a small number of PDF developers. Request a quote. | **Phase 5+** | The corpora, Matterhorn, and the ISO 32000-2 errata stream are all published publicly. Join when spec ambiguities start costing real engineering time and the working groups become worth the fee. |
| Apple Developer Program (Organization) | **[VERIFY]** $99/yr | **Phase 6** (desktop notarisation) / **Phase 7** (iOS) | Not needed for web or the Chrome extension. Requires a D-U-N-S number, which itself takes 5–14 days — start it a month before you need it, not on the day. |
| Windows code signing — **Azure Trusted Signing** (rebranded **Azure Artifact Signing** in 2026) | **[VERIFY]** $9.99/month Basic tier (≈$120/yr), up to 5 000 signatures; $99.99/month Premium | **Phase 6** | Not needed for web or extension. Far cheaper than a traditional OV/EV certificate and issues short-lived auto-renewing certs, which removes key custody from your CI. |
| Google Play developer account | **[VERIFY]** $25 one-time | **Phase 7** | |
| Chrome Web Store developer account | **[VERIFY]** $5 one-time | **Phase 1A** | The only procurement item on the critical path to first release. |
| Cyber / E&O insurance | varies widely | before charging money | Get quotes early; a document editor that corrupts a legal filing is a claim. |
| Trademark filing | **[VERIFY]** jurisdiction-dependent. **US:** the USPTO retired the TEAS Plus / TEAS Standard tiers on 18 Jan 2025; there is now a single **$350 per class** base fee, plus surcharges (+$100 insufficient information, +$200 free-form goods/services description not drawn from the ID Manual, +$200 per extra 1 000 characters). EU (EUIPO), India and others differ substantially — price your own jurisdiction. | before public launch | The *search* is free (USPTO TESS and equivalents). Counsel review is the cost, not the filing. |
| Timestamp Authority / QTSP contracts | varies | **Phase 8** | Only if you ship long-term-validation digital signatures. |

### 2.3 Implementation-grade vs claim-grade — why the paid specs are not a build blocker

A distinction that matters for planning: **nothing in the engine is cancelled because a spec costs
money.** PDF/A and PDF/UA are deferred to Phase 9, and even then the paid ISO documents are not
what unblocks the code.

| | Free, and sufficient to **build** | Paid, and needed to **claim** |
|---|---|---|
| **PDF/A** | veraPDF (open-source validator, encodes the rules as executable tests) + the Isartor negative-test suite | ISO 19005 parts 1–4 |
| **PDF/UA** | The Matterhorn Protocol (enumerates every PDF/UA failure condition, normatively derived) | ISO 14289 parts 1–2 |

You can implement both to a high standard from the free artefacts, because they are derived from
the standards rather than guessed at. Buy the ISO documents before you **market** conformance —
an audit or a government procurement question is answered by the standard, not by a test suite.
That is roughly CHF 1 200 total, at month 30, against a running business.

The engine therefore reaches **`Author` level on PDF/A and PDF/UA at G9** (`20-CONFORMANCE-PROGRAM.md §4`).
Deferred, not excluded.

---

### 2.4 The number that matters

> **Procurement cost to ship the first release (PDF toolkit as a web app + Chrome extension): $5.**
>
> That is the one-time Chrome Web Store developer fee. Every specification is free, every test
> corpus is free, every font is free, no code-signing certificate is required, no other app store
> is on the critical path, and there is no cloud bill because processing is local.

This validates the beachhead choice: it is the one distribution channel with effectively no
procurement gate. Ship there, earn revenue, and fund the Phase 6+ items out of it.

---

## 3. Where money is genuinely required in year 1

Not licences — these:

1. **Engineering time or salaries.** This dwarfs everything in §2 by two orders of magnitude. Any
   analysis that optimises §2 while ignoring this is optimising the wrong variable.
2. **CI compute.** The differential-testing strategy is compute-hungry: nightly renders of ~100 000
   documents against three independent oracle implementations. Budget a self-hosted runner — one
   refurbished workstation pays for itself in weeks versus per-minute cloud CI. Public repositories
   get free GitHub Actions minutes, which is a genuine secondary argument for open-sourcing the
   parser crates.
3. **Storage for the test corpus.** ~100 000 real-world PDFs is a few hundred GB, and must be
   encrypted at rest since it contains third-party documents.
4. **Two or three specific legal hours** — reviewing the wording of the redaction guarantee, and
   a trademark search. This is not a retainer; it is a small number of scoped questions.

---

## 4. Would open-sourcing the core engine help?

### 4.1 What it would not change
Nothing in §2. Specifications, certificates, memberships and store fees are entirely indifferent to
your outbound licence.

### 4.2 What it would change: access to copyleft dependencies
If the engine were AGPL-licensed, AGPL dependencies become usable. In practice the list of things
you would actually want is very short:

- **`jbig2dec` (AGPL, Artifex)** — the mature JBIG2 decoder. Would save roughly two to three weeks
  of implementing JBIG2 yourself.
- **MuPDF / Ghostscript (AGPL, Artifex)** — you would not use these regardless: using them *as* the
  engine contradicts the from-scratch strategy, and Artifex's commercial licence is expensive.
- **veraPDF (GPL/MPL)** — only ever needed as an external CI tool, which is already permitted.

**The entire inbound gain is one image codec.** That does not justify restructuring the business
model.

The thing most often missed here: **PDFium is BSD-3 and pdf.js is Apache-2.0 — both already
permissive.** If time-to-market or cost were the binding constraint, the answer would be to adopt
one of those as a rendering backend, which requires no open-sourcing at all. That option was
considered and declined in favour of the owned engine; open-sourcing does not reopen it.

### 4.3 What it would change: the revenue model
This is the decision worth taking seriously — but as strategy, not as cost reduction.

**Dual licensing (AGPL + commercial)** is proven in exactly this market. **iText** is the direct
precedent: AGPL-3.0 for the community, paid commercial licences for anyone who cannot comply.
Because AGPL's network clause reaches SaaS, any company embedding the engine in a product or
service must either open-source their entire product or buy a licence. For a PDF SDK — where the
buyers are software companies embedding your engine — that is a strong forcing function, and it is
essentially the SDK revenue line with a different enforcement mechanism.

Costs of that choice:

- **Enterprise procurement allergy.** A meaningful share of large organisations ban AGPL outright
  in procurement policy, sometimes even for internal-only use. That collides directly with an
  enterprise SKU.
- **Fork risk.** A funded competitor can take the engine. Your defence becomes the trademark, the
  test corpus, the published conformance evidence, and release cadence — not the code.
- **Copyright hygiene becomes load-bearing.** You can dual-license only what you own. That requires
  a contributor licence agreement with copyright assignment or a broad relicensing grant, in place
  **from the first outside contributor**. Retrofit it later and you cannot relicense without
  tracking down every past contributor.

### 4.4 Recommendation

**Do not AGPL the engine now. Keep the current posture.**

1. **Apache-2.0 the three parser crates** (`selis-pdf-cos`, `selis-pdf-filter`, `selis-font`) and nothing else.
   This already buys the two things worth having: **free continuous fuzzing via OSS-Fuzz** on the
   highest-CVE-risk code in the project, and developer credibility for the later SDK product. It
   costs zero commercial optionality.
2. **Implement JBIG2 yourself.** It is a few weeks and it is already scoped.
3. **Put a CLA with a relicensing grant in place from day one.** This is the cheap insurance that
   keeps dual-licensing available as a future option.
4. **Revisit dual-licensing at Phase 9**, when the SDK actually ships and you know whether your
   buyers are independent developers (where AGPL pressure converts well) or enterprises (where it
   kills deals). Deciding then costs nothing extra; deciding now forecloses a branch.

---

## 4A. Monetization model — who pays, who does not, and what is sold

§§1–4 are about *cost*. This section is about *revenue*, and it is a product-owner decision, recorded
here so the rest of the plan has one place to cite. It supersedes nothing in the engine plan; it
makes explicit a positioning that `26-PRODUCT-SURFACE.md` and `37-ASPOSE-PARITY.md` both assume.

### 4A.1 The one-sentence model

> **Individuals use Selis free, forever — both the desktop app and the web app. Companies that
> embed the engine or deploy it commercially pay. The revenue product is the SDK and the commercial
> deployment licence; the free apps are the wedge that makes the SDK worth buying.**

This is the Aspose/iText commercial model with the consumer side **inverted**: the incumbents charge
individuals (Acrobat's subscription, Smallpdf's task-gated tiers) and charge enterprises; Selis
charges only enterprises and gives individuals the full product at no cost. The asymmetry is
affordable for exactly one reason — **the work runs on the user's device, so an individual user
costs us nothing to serve** (ADR-P0016). A server-side competitor cannot match "free and unlimited
for individuals" without changing their unit economics; we can, because we have no per-document
infrastructure cost to recover.

### 4A.2 The competitive set

Selis is positioned as a **direct competitor to Aspose.PDF, Foxit, and Adobe Acrobat** — not merely
as another consumer web-tools site. Two distinct fronts:

| Front | Incumbents | Selis product | How we differ |
|---|---|---|---|
| **The SDK / library** | Aspose.PDF (per-developer + per-deployment), Apryse/Nutrient, Foxit PDF SDK | **Selis SDK** (C ABI + WASM + language wrappers) | Own Rust engine (memory-safe by construction), permissively-licensed open parser core (ADR-P0030) that de-risks adoption, deterministic output (ADR-P0012), per-operation verification. Priced to undercut the incumbent SDKs. |
| **The editor / app** | Adobe Acrobat (subscription), Foxit PDF Editor | **Selis desktop + web apps** | **Free for individuals, forever** — no account, no watermark, no daily limit. Local-first, no forced cloud. The paid relationship starts only at commercial/enterprise use. |

The SDK front is where the money is. The app front is the wedge: a free, genuinely good,
privacy-respecting Acrobat alternative builds the audience, the credibility, and the corpus that the
SDK is then sold into. This is why **Aspose-surface parity is a primary product goal and not a
nice-to-have** (`37-ASPOSE-PARITY.md`): the SDK is the business, so the engine must reach SDK-grade
completeness. The parity matrix's gaps are the revenue roadmap.

### 4A.3 What is free, and what is sold

**Free, forever, for individuals — no account, no watermark, no daily task limit:**

- The **web app** (the tools surface at `/tools.html` and the viewer) — every operation that runs
  in the tab.
- The **desktop app** (Tauri; Win/mac/Linux) — the same engine, offline.
- Non-commercial use of the CLI.

**Sold — this is the revenue line:**

| What is sold | To whom | Price shape | Ships |
|---|---|---|---|
| **Selis SDK** — C ABI, WASM, language wrappers, source-escrow option | Software companies embedding the engine | Per-developer seat + runtime/OEM royalty (Aspose-style) | P9 |
| **Commercial deployment licence** for the apps/CLI | Companies deploying Selis internally at scale or on managed fleets | Per-seat or per-site, negotiated | P9 (enforced earlier by honour + CLA) |
| **Selis Server / API** — headless, self-hosted or managed | Companies wanting a server/batch surface | Per-core / per-instance | P9 |
| **Enterprise add-ons** — SSO, admin/policy, DLP, air-gapped, FedRAMP path, SLA, support | Large organisations | Negotiated annual | P9 |
| **Compliance SKUs** — PDF/A, PDF/UA validation + remediation, prepress | Government, archival, accessibility-regulated buyers | Per-seat add-on | P9 |

The precise per-SKU entitlement grid is **`26-PRODUCT-SURFACE.md §2–3`**; this section sets only the
*principle* that draws the free/paid line. Where the two appear to disagree, see §4A.6 — the line
has moved and `26` is being brought into agreement in the same change set.

> **No prices are committed here.** The Aspose/Apryse/Foxit public list prices are the reference
> points to undercut, but the actual numbers are a go-to-market decision taken at Phase 9 against
> real pipeline. Any figure that appears in planning before then is an **illustrative placeholder**
> and must be labelled as one.

### 4A.4 How "individual" vs "commercial" is drawn — and enforced

The line is **honour-based, not DRM**, and this is a deliberate consequence of the local-first ADRs,
not a concession:

- **The definition.** "Individual / non-commercial" means a natural person using Selis for their own
  work. "Commercial" means (a) embedding the engine or SDK in a product or service you ship or
  operate, (b) deploying the apps/CLI across an organisation's fleet, or (c) running the Server/API
  for a business. The SDK and Server licences are per the commercial agreement; the free apps carry
  a plain-language licence stating the individual-use grant and the point at which commercial terms
  apply.
- **Why not DRM on the app.** ADR-P0014 ships one binary with entitlements evaluated at runtime, and
  ADR-P0015 requires that a licence failure never destroys or traps a user's work. A hard
  anti-tamper lock on a *local* app contradicts both: it would mean the engine phoning home or
  refusing to open a document, which breaks the privacy guarantee (ADR-P0016) and the
  never-lose-work guarantee (ADR-P0015) that are the product's whole pitch. We do not pay the
  product's soul to stop a determined individual from using paid features locally — an acceptable
  loss already recorded in ADR-P0014/P0015.
- **What actually enforces it.** Commercial revenue is enforced where it is enforceable and where it
  matters: the **SDK and Server are the paid artefacts**, delivered under a signed commercial licence
  with support, updates, indemnity, and (optionally) source escrow — things an enterprise needs and
  cannot get from a cracked local binary. Enterprises buy compliance and support, not bytes. For the
  apps, enforcement is the licence agreement plus org-level procurement (an enterprise cannot deploy
  unlicensed software across a fleet and pass an audit), not a technical lock.
- **The CLA is the load-bearing legal instrument.** Because the paid SDK is built from the same
  crates as the free apps, we must be able to license that code commercially. That requires a
  **contributor licence agreement with a relicensing grant in place from the first outside
  contributor** (§4.3, §4.4). This is the one piece of legal hygiene that is genuinely day-one, and
  it is what makes the open-core model below legally available at all.

### 4A.5 Open-core, reconciled with ADR-P0030 and the no-AGPL stance

The monetization model is an **open-core / commercial-SDK posture**, and it must be read *together*
with the licensing decisions in §4 and ADR-P0030 — they are consistent, and the consistency is the
point:

- **Open core.** The three parser crates (`selis-pdf-cos`, `selis-pdf-filter`, `selis-font`) are
  Apache-2.0 (ADR-P0030). Apache-2.0, not AGPL — see §4.3–4.4: AGPL was considered and declined
  because a meaningful share of enterprises ban it in procurement, which would collide with the
  enterprise SKU that is the whole revenue thesis here. The permissive core is a *sales asset* for
  the SDK (it de-risks adoption and buys OSS-Fuzz capacity), not a copyleft forcing function.
- **Commercial shell.** The renderer, edit model, redaction, signing, the full engine, and all
  product code stay **proprietary**. That proprietary surface is what the SDK and the apps are, and
  it is where the moat lives (the renderer's correctness and the edit model, not the ability to read
  an xref table — §4.4, ADR-P0030).
- **This is "open-core," not "dual-license."** The distinction matters and the plan already draws it.
  A *dual-license* model (iText-style AGPL-or-pay) was evaluated in §4.3 and **not adopted now**;
  the current posture is *open-core*: a permissively-licensed component core plus a proprietary
  commercial product on top. Dual-licensing the *whole engine* remains a Phase-9 option (§4.4), to
  be decided when we know whether SDK buyers are independent developers (where AGPL pressure
  converts) or enterprises (where it kills deals). Deciding then costs nothing; the CLA (§4A.4)
  keeps the branch open.

The net picture — "free permissive parser crates + free apps for individuals + paid proprietary SDK
and enterprise deployment" — is the Aspose-style commercial model with an open, permissive core and
a free consumer tier bolted on where the local-first architecture makes it free to give away.

### 4A.6 Reconciliation note — the free/paid line has moved

`26-PRODUCT-SURFACE.md §2` as previously written drew the individual paywall at **"producing a
modified document"** (free Reader; paid consumer *Editor* subscription). This section records a
**product-owner decision that moves that line**: for **individuals**, the desktop and web apps are
now **free in full** — including editing, OCR, conversion, redaction, and signing — and the paid
relationship begins at **commercial/enterprise use** (SDK, Server, fleet deployment, enterprise
add-ons). The consumer-subscription Editor SKU is therefore reframed, not a paid individual tier.
`26-PRODUCT-SURFACE.md` is updated in the same change set to agree; where any older text still
implies a paid individual Editor, **this section and `26 §2` as revised are authoritative.**

### 4A.7 Proposed ADR

This is a business-model decision of the same weight as ADR-P0030, and it changes how `26` draws its
free/paid line, so it should be recorded as an ADR rather than living only here. **Proposed
ADR-P0046 — "Monetization: free for individuals (desktop + web), paid for commercial/enterprise;
open-core SDK is the revenue product."** It would:

1. State the free-for-individuals / paid-for-commercial split and name the SDK + commercial
   deployment as the revenue line.
2. Record the honour-based (not DRM) enforcement and tie it to ADR-P0014/P0015/P0016.
3. Position the open-core posture as consistent with — and dependent on — ADR-P0030 and the
   no-AGPL-now decision, with whole-engine dual-licensing left as the Phase-9 option.
4. Supersede the `26 §2` "paywall at modify-document" framing for individuals.

ADR-P0046 is the **next free number** (existing ADRs run to ADR-P0045; do not renumber them). It is
recorded here as *proposed*; the actual ADR block belongs in `02-ADRS.md` once the product owner
confirms the wording.

---

## 5. Corrections this document makes to the rest of the plan

| Plan item | Was | Now |
|---|---|---|
| `SL-0.LEAD.05` | "ISO 19005 and 14289 are paid… **Blocks all engine work.**" | **Wrong.** Split into `LEAD.05a` (free specs, week 1, genuinely blocks engine work) and `LEAD.05b` (paid PDF/A + PDF/UA specs, Phase 9, blocks only the compliance SKU). |
| `SL-0.LEAD.06` | PDF Association membership as a day-1 long-lead item | Deferred to Phase 5. The free publications cover Phases 0–4. |
| `SL-0.LEAD.07` | "Get counsel to confirm font embedding posture" | Downgraded. SIL OFL and GUST FL explicitly permit embedding and redistribution. Read the licences; this does not need a lawyer. |
| `ADR-P0030` | unchanged | Reaffirmed, with the dual-licence revisit trigger recorded in §4.4. §4A.5 reads it as the *open core* of an open-core business model. |
| `26-PRODUCT-SURFACE.md §2` | individual paywall at "producing a modified document" (paid consumer Editor) | **Moved.** §4A.6: desktop + web are free in full for individuals; the paid line is commercial/enterprise use. `26 §2` updated to agree. |
| New ADR | — | **Proposed ADR-P0046** (§4A.7) records the monetization model. Next free number; existing ADRs (→P0045) unchanged. |

---

## 6. What is deliberately not claimed here

- That prices are current. They are dated August 2026 and marked **[VERIFY]**.
- That PDF/A or PDF/UA specs are free. They are not, as of August 2026 — but the sponsored bundle
  has expanded before and should be re-checked.
- That this constitutes legal advice. The licence *facts* (which licence applies to which project)
  are checkable and cited; the *consequences* for a specific business are not, and the AGPL
  dual-licensing question in particular is one to run past counsel before committing.
- That open-sourcing is wrong in general — only that it does not solve a procurement-cost problem,
  and that the version which would (AGPL) carries costs that outweigh a single image codec.
- That the §4A monetization numbers are set. No prices are committed; the incumbents' list prices are
  reference points to undercut, and any figure before Phase 9 is an explicit placeholder (§4A.3).
- That honour-based enforcement captures every individual who uses paid features locally. It does
  not, by design (§4A.4, ADR-P0014/P0015) — the revenue is the SDK, Server, and audited enterprise
  deployment, not a lock on the local app.

---

## 7. Sources

- [Announcing no-cost access to ISO 32000-2 (PDF 2.0) — PDF Association](https://pdfa.org/announcing-no-cost-access-to-iso-32000-2-pdf-2-0/)
- [Sponsored ISO standards for PDF technology — PDF Association](https://pdfa.org/sponsored-standards/)
- [ISO 32000-2 resource page — PDF Association](https://pdfa.org/resource/iso-32000-2/)
- [Membership fees — PDF Association](https://pdfa.org/membership-fees/) (tiers listed; amounts not published)
- [Member benefits — PDF Association](https://pdfa.org/member-benefits/)
- [Trusted Signing pricing — Microsoft Azure](https://azure.microsoft.com/en-in/pricing/details/trusted-signing/)
- [Trusted Signing open to individual developers — Microsoft Community Hub](https://techcommunity.microsoft.com/blog/microsoft-security-blog/trusted-signing-is-now-open-for-individual-developers-to-sign-up-in-public-previ/4273554)
- [ISO 19005-1 (PDF/A-1) resource page — PDF Association](https://pdfa.org/resource/iso-19005-1-pdf-a-1/)
- [ISO 14289-1 (PDF/UA) resource page — PDF Association](https://pdfa.org/resource/iso-14289-pdfua/)
- [PDF 2.0, ISO 32000-2 — Library of Congress format description](https://www.loc.gov/preservation/digital/formats/fdd/fdd000474.shtml)
- [Summary of 2025 trademark fee changes — USPTO](https://www.uspto.gov/trademarks/fees-payment-information/summary-2025-trademark-fee-changes)
- [Overview of Key USPTO Trademark Filing Fee Increases Effective January 18, 2025 — JD Supra](https://www.jdsupra.com/legalnews/overview-of-key-uspto-trademark-filing-1143372)
