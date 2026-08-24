# 03 — Engineering Conventions

Non-negotiable. `xtask lint` enforces most of this mechanically; the rest is enforced in review.

---

## 1. Workspace configuration (copy verbatim)

```toml
# Cargo.toml — the ONLY place a dependency version appears
[workspace]
resolver = "2"
members  = ["crates/*", "apps/cli", "apps/desktop/src-tauri", "xtask", "fuzz"]

[workspace.package]
edition      = "2021"
rust-version = "1.85"          # MSRV: current stable − 2 (ADR-P0001)
license      = "LicenseRef-Selis-Proprietary"   # except the three OSS crates (ADR-P0030)

[workspace.lints.rust]
unsafe_code                 = "warn"        # deny in all but the allowlist (§2)
missing_docs                = "deny"
unreachable_pub             = "deny"
elided_lifetimes_in_paths   = "deny"

[workspace.lints.clippy]
# The hostile-input lint set — deny everywhere in L0–L3, no exceptions:
unwrap_used              = "deny"
expect_used              = "deny"
panic                    = "deny"
indexing_slicing         = "deny"    # a[i] on document-derived i is a crash primitive
arithmetic_side_effects  = "deny"    # overflow on a parsed length is a heap bug
cast_possible_truncation = "deny"
cast_sign_loss           = "deny"
integer_division         = "deny"
unwrap_in_result         = "deny"
large_stack_arrays       = "deny"
recursive_format_impl    = "deny"
mem_forget               = "deny"
todo                     = "deny"
unimplemented            = "deny"
missing_panics_doc       = "deny"
```

`indexing_slicing` and `arithmetic_side_effects` are the two that matter most and the two people
will try hardest to allow. They are the difference between "malformed length field" and "CVE".
The escape hatch is `get()`/`checked_*`, not `#[allow]`.

---

## 2. `unsafe` policy

- `unsafe` is **denied** by default. The allowlist lives in `xtask/unsafe-allow.toml` and currently
  contains exactly: `selis-bytes` (aligned buffer construction), `selis-pdf-ffi` (the C ABI itself),
  `selis-pdf-jni`, `selis-io` (`mmap` in `FileSource`, native only), and `selis-raster` (proven-identical
  SIMD kernels only).
- Every `unsafe` block carries a `// SAFETY:` comment naming the invariant and who upholds it.
  `xtask check-unsafe` fails the build otherwise.
- `miri` runs over every crate on the allowlist in `test-slow`.
- Adding a crate to the allowlist requires an ADR. Not a review comment — an ADR.

---

## 3. The error model (`selis-error`)

One error type, one numeric registry, generated from `codes.toml`:

```toml
[[code]]
id        = 1204
name      = "XREF_UNRECOVERABLE"
kind      = "Malformed"
meaning   = "The cross-reference data is damaged beyond reconstruction."
user_msg  = "This PDF's index is damaged and could not be rebuilt."
retryable = false
doc_state = "NotLoaded"     # NotLoaded | Loaded | PartiallyLoaded | Unchanged | Modified
```

`doc_state` is the field that matters: for every error, the caller must be able to answer *"what
happened to the user's document?"* without reading the source.

### Code ranges — never reuse a number, never change a meaning

| Range | Domain |
|---|---|
| 1000–1499 | COS / structure / xref |
| 1500–1799 | Filters and codecs |
| 1800–1999 | Encryption and permissions |
| 2000–2299 | Fonts and text |
| 2300–2599 | Content interpretation and rendering |
| 2600–2799 | Annotations and forms |
| 2800–2999 | Edit and save |
| 3000–3199 | Redaction |
| 3200–3399 | Signatures and PKI |
| 3400–3599 | OCR and conversion |
| 3600–3799 | Accessibility and conformance |
| 4000–4299 | Budget / sandbox / cancellation |
| 4300–4499 | Policy / entitlement / consent |
| 5000–5299 | I/O and sources |
| 6000–6299 | Bindings (WASM/FFI/IPC) |
| 7000–7299 | Service tier |

`xtask check-codes` fails on a duplicate id, a changed `meaning`, or a code used in source but
absent from the registry. User-facing strings live in the registry, not in `format!` calls, so
localisation is a data change (ADR-P0034).

---

## 4. Naming and module conventions

- Crates: `selis-<domain>` when format-neutral, `selis-pdf-<domain>` when PDF-specific;
  lib names `selis_<domain>` / `selis_pdf_<domain>`. See ADR-P0034 for which is which.
- Anything that consumes untrusted bytes is `parse_*` / `decode_*` and returns `Result`.
  Anything that produces bytes is `write_*` / `encode_*`. Anything that may be wrong-but-useful
  is `infer_*` and returns a confidence (`selis-pdf-text` reading order, `selis-pdf-a11y` table structure,
  `selis-pdf-edit` paragraph reconstruction).
- `try_*` is reserved for fallible variants of infallible-looking operations. Do not use it for
  ordinary parsing — everything there is fallible and the prefix carries no information.
- PDF spec names appear verbatim where they are spec names: `MediaBox`, `ToUnicode`, `AcroForm`.
  Do not "improve" them into `media_bounds`. A reader with the spec open must be able to grep.
- Every type that mirrors a spec construct cites it: `/// PDF 32000-2:2020 §7.5.8 (xref streams)`.

---

## 5. Documentation requirements

Every public item has a rustdoc comment. Additionally:

- Any function taking a `&dyn DocSource`, `&[u8]` of document origin, or a `Budget` must have
  `# Budget` and `# Malformed Input` sections (ADR-P0013, enforced by `check-contracts`).
- Any function that can produce bytes the user will save must have `# Output Guarantees`.
- Any `infer_*` function must document what the confidence value means and what the caller should
  do below the threshold.

---

## 6. Testing requirements

Full detail in `21-TESTING-AND-ORACLES.md`. Summary:

| Kind | Requirement |
|---|---|
| Unit | Per-crate line coverage floors in `xtask/coverage.toml`. `selis-pdf-cos`, `selis-pdf-filter`, `selis-font`, `selis-pdf-edit`, `selis-pdf-redact`, `selis-pdf-sign`, `selis-sandbox`: **90%**. Other L2: 80%. L3–L4: 70%. |
| Property | `proptest` round-trips: every filter (encode→decode→identity), the COS writer (write→parse→structural equality), the incremental writer (N mutations → parse → semantic equality + original-prefix-intact). |
| Corpus | Every behaviour-changing task adds a corpus entry (ADR-P0032). |
| Differential | Render + extract compared against oracles on every PR for the smoke corpus, nightly for the full corpus. |
| Fuzz | One `cargo-fuzz` target per parser entry point. A new parser without a fuzz target does not merge. |
| Mutation | `cargo-mutants` over `selis-sandbox`, `selis-pdf-edit`, `selis-pdf-redact`, `selis-pdf-sign` in `test-slow`. |
| Determinism | Every render test asserts hash equality across a second run; the cross-platform job asserts it across three targets. |
| Miri | Over the `unsafe` allowlist crates. |

**A bug fix without a regression corpus entry does not merge.** The entry is named for the issue.

---

## 7. Definition of Done

A task is not complete without all of these:

1. Code compiles for `x86_64-linux`, `aarch64-darwin`, `x86_64-windows`, `wasm32-unknown-unknown`,
   `aarch64-ios`, `aarch64-android`.
2. `cargo xtask lint` clean — including `check-layers`, `check-purity`, `check-unsafe`,
   `check-contracts`, `check-codes`, `check-alloc`, `size-check`.
3. The tests named in the task's DoD exist and pass.
4. Corpus entry added if behaviour changed (ADR-P0032).
5. Fuzz target added if a new parser entry point was introduced.
6. Rustdoc complete, including the contract sections.
7. Coverage floor for the crate not regressed.
8. Perf budget for the touched path not regressed (§12).
9. Conformance-ladder level in `20-CONFORMANCE-PROGRAM.md` updated if it moved — including
   *downgrading* it if the corpus revealed the previous claim was wrong.
10. `CHANGELOG.md` entry if user-visible.

---

## 8. PR template (`.github/pull_request_template.md`)

```md
## Task
<!-- SL-x.AREA.nn, or "none" with a reason -->

## What changed

## Blast radius
<!-- Which crates, which conformance areas, which shells. -->

## Document safety
- [ ] No path mutates the source buffer.
- [ ] Save path produces a valid incremental update, or is an explicit rewrite with verification.
- [ ] Every new parse entry point takes a Budget and is fuzzed.
- [ ] No new network egress of document-derived data without a CloudConsent token.

## Tests added
<!-- Named tests + corpus IDs. -->

## Oracle deltas
<!-- Any change in agreement with PDFium/pdf.js/qpdf, with the triage verdict. -->

## Compliance
- [ ] Licence policy respected (no copyleft in shipped crates).
- [ ] Conformance ladder updated.
- [ ] Perf budgets checked.
```

---

## 9. CI pipeline shape

| Stage | Runs on | Gate |
|---|---|---|
| `fmt` + `lint` | every PR | required |
| `build-matrix` (6 targets) | every PR | required |
| `test-fast` (unit + property) | every PR | required |
| `corpus-smoke` (2 000 files, render + extract vs oracles) | every PR | required |
| `size-check` (WASM budgets) | every PR | required |
| `determinism` (render twice, hash) | every PR | required |
| `test-slow` (miri, mutants, 50k-file corpus, veraPDF) | nightly + release | required for release |
| `fuzz-soak` (4 CPU-hours/target) | nightly | non-blocking, files issues |
| `perf` (criterion vs baseline) | nightly + release | blocks release on >5% regression |
| `cross-platform-determinism` | nightly | required for release |
| `oss-fuzz-sync` | weekly | non-blocking |

Merge queue on. PR-required stages must complete in under 12 minutes or the corpus-smoke set gets
smaller, not the gate weaker.

---

## 10. Branching, versioning, release

- Trunk-based. Short-lived branches. Merge queue.
- The engine version is a single workspace version. Shells carry their own marketing versions but
  record the engine version in their about box and every crash report.
- The three OSS crates version independently under semver and may not break API without a major.
- Release trains: web/extension continuous (behind flags), desktop monthly, mobile fortnightly
  (store latency), SDK quarterly with a 12-month API stability promise.
- Every release ships an SBOM and a reproducible-build attestation.

---

## 11. Feature flags

- A flag is `selis-policy::Feature`, evaluated at runtime (ADR-P0014). Cargo features are for
  *platform capability* only (`wasm`, `native-io`, `threads`), never for product tiers.
- Every flag has an owner and an expiry date in `flags.toml`. `xtask check-flags` fails the build
  on a flag that is past expiry, forcing either removal or a deliberate extension.

---

## 12. Performance budgets (regression-gated)

Measured on the reference machine defined in `bench/README.md`, plus a mid-range mobile device.

| Path | Budget |
|---|---|
| Open + first page painted, 5 MB linearised, local | ≤ 250 ms native / ≤ 600 ms WASM |
| Open + first page painted, 5 MB over simulated Fast 3G, linearised | ≤ 1.2 s p75 |
| Render A4 text page @150 DPI | ≤ 18 ms native / ≤ 45 ms WASM |
| Render A4 text page @150 DPI, vs PDFium | ≥ 0.6× throughput at G2, ≥ 0.9× at G5 |
| Text extraction, 300-page report | ≤ 900 ms native |
| Incremental save, 1 annotation on a 200 MB file | ≤ 40 ms (must not rewrite the file) |
| Scroll frame budget, all shells | 60 fps sustained; no frame > 16 ms attributable to the engine |
| Peak RSS, 200 MB PDF, viewer profile | ≤ 400 MB native / ≤ 300 MB WASM |
| Core WASM bundle | ≤ 3 MB brotli (viewer), ≤ 6 MB with the editor chunk |
| Cold app launch → interactive, mobile mid-range | ≤ 800 ms |

The incremental-save budget is the load-bearing one: if saving an annotation on a large file ever
starts rewriting the whole document, ADR-P0007 has been violated somewhere and the number will say
so before a user does.
