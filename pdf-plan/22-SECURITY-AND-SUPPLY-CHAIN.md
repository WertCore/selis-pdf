# 22 — Security and Supply Chain

---

## 1. Threat model

**The adversary is the document.** Everything else follows from that.

| # | Threat | Vector | Control |
|---|---|---|---|
| T1 | Memory corruption → RCE | Malformed font, image codec, xref, content stream | Rust (ADR-P0001); `unsafe` allowlist; C codecs only in WASM sandbox (ADR-P0018); fuzzing |
| T2 | Resource exhaustion → DoS / tab kill | Decompression bomb, xref bomb, pixel bomb, deep nesting, pathological pattern | `Budget` on every entry point (ADR-P0006); the exhaustion corpus as a permanent regression suite |
| T3 | Stack overflow → uncatchable abort | Recursive form XObjects, `/Prev` chains, nested object streams | No native recursion in `selis-pdf-cos`/`selis-pdf-content`/`selis-pdf-doc`; explicit worklists + depth budget |
| T4 | SSRF / data exfiltration on open | External stream `/F`, `/GoToR`, remote XMP entity, `/SubmitForm`, embedded JS | Active content off by default (ADR-P0020); **no network access below L4** enforced by `check-purity`; XML entities disabled |
| T5 | Silent document exfiltration | Cloud features, telemetry, crash reports | ADR-P0016 consent chokepoint; the egress test harness (`SL-8.BOUND.02`); crash reports strip document bytes |
| T6 | Signature spoofing | Incremental-update attack, shadow attack, visual-vs-cryptographic confusion | `SL-5.SIGN.01/02`; the published attack corpus in CI; UI that cannot be spoofed by page content |
| T7 | Redaction failure → disclosure | Overlay-only redaction; content surviving in prior revisions, metadata, embedded files | ADR-P0023 removal + verification pass; the adversarial suite at 100% with no override |
| T8 | Phishing via document UI | Annotation appearance that lies about its action; fake "secure document" pages | Show the true destination on every action; never render an annotation appearance as chrome |
| T9 | Supply-chain compromise | A dependency, a build machine, an update channel | §3–§5 below |
| T10 | Malicious extension update / store account takeover | CWS account compromise | Hardware-key 2FA, publisher-account hygiene, signed reproducible builds |
| T11 | Sandbox escape from the WASM codec tier | Bug in the WASM runtime | Memory caps, no WASI, no host imports beyond the buffer protocol; runtime pinned and patched fast |
| T12 | Malicious document targeting the *shell*, not the engine | Path traversal in an attachment filename, `file://` links, OS handler abuse | Filename sanitisation, no automatic extraction, Tier-3 process sandbox for untrusted provenance |

---

## 2. Defence in depth

```text
Layer 1  Language          Rust; unsafe allowlisted, SAFETY-commented, miri-checked
Layer 2  Budget kernel     Every parse bounded in memory, time, depth, objects, pixels
Layer 3  Type system       DocSource cannot write; read-only features cannot name a sink;
                           egress requires a CloudConsent token that only consent can mint
Layer 4  WASM sandbox      C codecs and OCR in capped linear memory, no host access
Layer 5  Process sandbox   Untrusted-provenance documents in a seccomp/AppContainer/App Sandbox child
Layer 6  Browser           Site isolation, CSP, no remote code in the extension
Layer 7  Policy            Active content off; admin-lockable; per-document consent
```

No single layer is trusted. T1 is mitigated by 1, 4, and 5 independently.

---

## 3. Supply chain

- **Dependencies:** `cargo-deny` (licences, advisories, bans, sources), `cargo-vet` (human review
  records for every dependency and every version bump), crates.io only, `wildcards = "deny"`.
- **The pnpm side gets the same treatment**: lockfile committed, `pnpm audit` in CI, no
  `postinstall` scripts allowed without review, and a dependency-count budget for `apps/web/ui`
  because the extension review process punishes bloat and every transitive JS dependency is a
  supply-chain hole.
- **Vendoring policy:** anything vendored (OpenJPEG, Tesseract) is pinned to a commit, patched in
  a tracked patch series, and rebuilt reproducibly. Never a tarball of unknown provenance.
- **A dependency bump is a reviewed change**, not a bot merge. Renovate opens PRs; a human reads
  the diff for anything that touches parsing, crypto, or the build.

> Note for this environment specifically: a proxy that MITMs TLS and serves "fixed" package
> versions is not a trustworthy source for a security bump. Verify off-proxy before acting on any
> auto-filed CVE ticket. (See the local `pmg/SafeDep` note — same principle applies here.)

---

## 4. Build and release integrity

- Reproducible builds for every shipped artefact; a second builder verifies before release.
- SLSA build provenance attestations published per artefact.
- SBOM (CycloneDX) per artefact, published, and diffed release-over-release.
- Signing keys in HSMs; no key material on a developer machine or in CI environment variables.
- Release requires two-person approval; the signing step is a separate, audited pipeline.
- Update channel: TUF with role separation and key rotation (shared per ADR-P0029).
- **An update must never apply while a document has unsaved changes** (`SL-6.DIST.04`).

---

## 5. Vulnerability handling

- `SECURITY.md` with a monitored channel, a 90-day disclosure norm, and a stated SLA:
  triage in 48 h, fix or mitigation for critical parser bugs in 7 days.
- CVE assignment for anything affecting a shipped artefact or the three public crates.
- An out-of-band update path that can ship a parser fix in under 24 h on web/extension and under
  72 h on desktop/mobile (store latency is the constraint — plan a server-side kill switch for a
  specific construct as the stopgap).
- Every security fix gets a permanent corpus entry and a fuzz seed before the fix merges.
- Post-incident: a public write-up for anything user-affecting. In this category, transparency
  after an incident is worth more than the incident costs.

---

## 6. Privacy engineering

Privacy here is a *mechanism*, not a policy document:

- `check-purity` proves no engine crate can reach the network.
- `CloudConsent` makes unconsented egress unrepresentable in the type system.
- `SL-8.BOUND.02` fails the build if any request body contains document bytes without consent.
- Crash reports are tested to contain no document-derived data.
- The web app and extension ship no third-party scripts on the document-handling origin.
- Store privacy declarations ("data not collected") are backed by those tests, so they are
  defensible under audit rather than aspirational.

---

## 7. Security review checkpoints

| Gate | Required |
|---|---|
| G1 | Threat model reviewed; fuzz harness running; budget-exhaustion corpus green |
| G2 | Codec sandbox verified; determinism prevents timing-side-channel surprises in CI |
| G4 | Extension permission review; CSP audit; first external pentest of the web surface |
| G5 | **Full external pentest** focused on redaction, signatures, and the JS sandbox; redaction claim legally reviewed |
| G6 | Desktop sandbox verified per OS; installer and update-channel review; driver posture reviewed if `SL-6.OS.06` shipped |
| G7 | Mobile store privacy declarations audited against the egress harness |
| G8 | SOC 2 Type I; cloud pentest; sub-processor review |
| G9 | SOC 2 Type II; SDK/OEM security documentation; FedRAMP gap assessment if pursued |
