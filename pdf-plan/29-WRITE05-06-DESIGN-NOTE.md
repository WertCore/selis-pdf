# 29 — WRITE.05 / WRITE.06 design note (pre-sign-off draft)

> **Status: DRAFT — awaiting HUMAN sign-off.** Both tasks are `owner: HUMAN`
> (`11A-PHASE-1A-tools.md`); this note, the implementation, and the tests are
> the reviewable draft. Nothing here is final until the human owner signs the
> checkboxes in `11A`. Authored by the AI implementation session (WRITE.05/06,
> worktree `write05-06`, base `main @ 1f6fcc2`).

---

## 1. SL-1A.WRITE.05 — structural verification

### 1.1 What the obligation is

`23-EDIT-MODEL-SPEC.md §7` requires `save_rewritten()` to verify its own
output: parse what was written, re-render every page, and compare against the
pre-save render. The render half needs the G2 renderer. Until then the
weaker, *structural* standard applies (the task's own framing): the output
must reparse through its cross-reference chain, every object reachable from
the roots must resolve, the page tree must be internally consistent, and the
observable structure must match the input's where the operation promises
preservation.

### 1.2 What was built

**`crates/selis-pdf-cos/src/verify.rs`** — `verify_structural(doc_bytes,
expected, budget, g) -> Result<Verdict>`, L2, read-only, budgeted:

1. **Parse**: `xref::find_startxref` + `parse_revisions` — no scan-based
   recovery. A writer output that cannot be parsed from its own xref is a
   fault, not something to repair. When the newest revision fails to parse,
   the I5 rollback (`parse_revisions_resilient`) is consulted so the verdict
   reports *everything* wrong (the torn tail is one fault), but `ok` stays
   false — a writer output is complete or it is wrong.
2. **Reference closure**: every indirect reference reachable from the
   trailer `/Root` and `/Info` must resolve. `seen`-set de-duplication means
   diamond graphs and cyclic outline graphs (`Prev`/`Next`/`Parent`) walk
   once and terminate; each unresolvable reference is one `Fault::UnresolvedRef`
   attributed to its holder.
3. **Page tree**: leaf `/Type /Page` walk, declared `/Count` compared against
   the walked leaf count.
4. **Counts**: annotations (Σ `/Annots` over leaves), form fields
   (`/AcroForm`/`/Fields` recursive through `/Kids`), OCGs
   (`/OCProperties`/`/OCGs`). Expectations are captured from the *input*
   document via `verify::survey` and asserted on the output — the
   count definitions are shared by construction.
5. **Verdict**: `{ ok, observed, faults }` with a typed `Fault` enum whose
   `Display` names the fault in user-readable form; `Fault::kind()` gives a
   stable short label for oplog lines.

**Why in `selis-pdf-cos`**: it needs only `parse_revisions` + `resolve_ref`
(both already there), so layering is untouched (`check-layers` green); and
the writers' own tests can use it without crossing into the engine.

**Explicit scope fence** (module doc + task note): this is the **pre-G2
weaker standard** and must **NOT** be reused for content-rewriting
operations (text editing, content redaction, content re-encoding). It proves
structure, not rendering. The full render+text verification is filed as
`SL-1A.WRITE.08` (see §4).

### 1.3 Wiring — every tool output is verified before it exists

`apps/cli/src/write_gate.rs` is the single choke point:

* `write_verified(bytes, output, expected, …)` — verify, then commit
  atomically (`selis_io::FileSink`: temp file → fsync → rename). A failed
  verification produces **no file at all**; the typed error names the first
  fault (`VERIFY_FAILED` per `codes.toml` 2802).
* `write_generated` — same, with no count expectations, for outputs with no
  input document (`topdf`, `img2pdf`).
* `atomic_write` — no verification, for verbatim copies of the user's own
  bytes (`unlock` of an unencrypted document re-litigating its input would
  be wrong).

All Phase 1A tools now route through the gate: `split`, `delete` (counts
restricted to surviving pages), `rotate`, `reorder`, `set_metadata`,
`redact`, `merge` (page expectation = Σ inputs), `compress`, `unlock`,
`topdf`, `img2pdf`. `clear_permissions` keeps its domain-specific `/Root`+
`Session::open` check and gains the atomic commit.

### 1.4 External oracle — qpdf in CI

* `cargo xtask oracle check-output <file>` / `check-output-dir <dir>`:
  `qpdf --check` local-first (exit 0 = OK, 2 = warnings only → pass, ≥3 =
  errors → fail), pinned-container fallback via the existing
  `docker/oracles/qpdf` image when the binary is absent.
* `cargo xtask fixtures --outdir …`: generates a formed document set, runs
  the whole Phase 1A tool suite over it (including the incremental-append
  path), leaves 8 outputs for the oracle gate.
* CI job `write05-oracle` (`.github/workflows/ci.yml`): installs qpdf via
  apt on the runner, builds the CLI, runs the fixture suite, then
  `check-output-dir`. Local qpdf is pinned by the runner image; the
  container fallback carries the sha256 pin. **CI slot note**: the job is
  wired but a live runner run is pending — the first green run on
  `ubuntu-24.04` should be verified by the human owner (qpdf 3.x on the
  runner is newer than the container's 11.9.0; `--check` exit codes are
  stable across both, but the first run should confirm).

### 1.5 DoD — the negative test

`verify.rs::tests::corrupted_writer_output_is_caught` corrupts a genuinely
written document six ways — tail truncation (no `startxref`), mid-body
truncation (xref offsets dangle), a reference to a never-written object,
wrong page expectation, wrong annotation expectation, wrong field/OCG
expectations — and each corruption is caught with the right fault variant.
`page_tree_count_mismatch_is_caught` covers the `/Count` check;
`incremental_output_verifies` covers the two-revision case (I2 holds through
verification).

---

## 2. SL-1A.WRITE.06 — crash atomicity

### 2.1 The guarantee, precisely

For every save path, killing the process at **any** point must leave the
on-disk result in one of exactly three states:

1. **absent** — a fresh destination that was never committed;
2. **the untouched original** — byte-identical, or (append path) the
   original as a byte-identical prefix with an unreachable partial tail;
3. **a complete valid document** — the full expected output (rewrite path)
   or original+complete-new-revision (append path).

Never a torn file — where "torn" means "the next open sees a document state
that is neither the original nor the complete new one".

### 2.2 How each path gets it

**Full rewrite (split / rotate-rewrite / compress / merge / …):** the bytes
are built in memory, verified, written to a temp file in the destination
directory, fsynced, and renamed over the destination (`FileSink`). The
rename is the only visible transition; a kill before it leaves the original,
after it the complete output. No torn window exists *by construction* —
this is SL-0.IO.05 unchanged, now actually used by every tool.

**Incremental append (rotate-incremental, the WRITE.02 writer +
`AppendFileSink`):** the appended objects + xref + trailer are written
first; the `startxref … %%EOF` commit block is written **last**, and
`finish()` fsyncs. A kill mid-append leaves the original bytes intact as a
prefix, with a partial tail the newest `startxref` cannot reach. The reader
side (`parse_revisions_resilient`, added in `revision.rs`) walks back over
`startxref` occurrences and resolves the newest *complete* revision — the
partial tail is **detectable and discarded on next open** (I5). The strict
parse still refuses the torn tail; that asymmetry is exactly what
`verify_structural` uses to reject incomplete writer output.

**Fixes made where the guarantee did not hold:**

* The tools did not use `FileSink` at all (`std::fs::write` = truncate-then-
  write = torn window). Fixed: all writes route through the gate (§1.3).
* The reader had no I5 rollback: `parse_revisions` failed hard on a torn
  newest revision. Fixed: `parse_revisions_resilient` (bounded to 16
  `startxref` occurrences, budget-charged, typed errors preserved).
* `AppendFileSink` itself is new (in-place append, fsync commit, debug-only
  crash hooks); no writer-side redesign was needed beyond wiring — the
  ordering discipline was already right in `write_incremental_update`.

### 2.3 The kill-test harness

`apps/cli/tests/write06_kill_test.rs`:

* **Fault injection** — debug-only (`#[cfg(debug_assertions)]`), in the
  sinks: `SELIS_DEBUG_CRASH_AFTER_BYTES=<n>` aborts the process once `n`
  bytes have passed through the sink (≤256-byte granularity), and
  `SELIS_DEBUG_CRASH_PHASE=before-fsync|after-fsync` (append) /
  `before-fsync|before-rename|after-rename` (temp+rename) aborts inside
  `finish()`. `std::process::abort()` runs no Drop handlers — exactly a
  process kill, minus the need for an external supervisor. Release builds
  compile none of it (no env access, no abort).
* **Child-per-iteration** — each kill point spawns a real `selis
  __crash-save <op> …` child (hidden debug-only subcommand in
  `apps/cli/src/crash_save.rs`), so the kill is a true process death on the
  real write path, not a simulated one inside the test process.
* **Seeded reproducibility** — xorshift64* with fixed per-operation seeds;
  the same seed always yields the same 500 points. `SELIS_WRITE06_ITERS`
  scales the run (default 500 per op per the DoD).
* **Point mix** — ~97% byte thresholds spread across `[0, output_len+512]`
  (hits every byte region of the write, including mid-trailer), plus the
  durability phases so the commit windows are hit.
* **The assertions** — per §2.1: rewrite paths accept only
  `{original, expected-complete}` byte-identical on disk; the append path
  additionally accepts original-prefix+partial-tail **only if** the next
  open (strict parse fails → resilient parse) resolves to exactly the
  original single revision. Any 3-page-state violation, prefix mutation, or
  half-valid document fails the test.
* **Results** — all 500 points × {split, rotate-incremental, compress} pass
  on this host (1500 child kills; `--test-threads 4`, ~16 min). The three
  tests are `#[ignore]`-tagged so the default suite stays fast; CI job
  `write06-kill` runs them with `--ignored`. A 12-point smoke variant runs
  in the default suite (36 kills, ~30 s) so ordinary PRs still exercise the
  property.

### 2.4 Known limits (for the human owner)

* **Power-loss vs process-kill**: the harness proves process-kill atomicity.
  Durability across power loss holds at `fsync` (both sinks fsync in
  `finish()`), but the appended-revision ordering guarantee ("original bytes
  were already durable before the append began") is inherited from the
  filesystem, not tested here. A true power-loss harness (VM snapshot
  kill) is out of scope for this task and not proposed.
* **Windows rename**: `std::fs::rename` over an existing file is atomic on
  NTFS in practice but not formally guaranteed by the platform contract.
  The human owner may want a `MoveFileEx(MOVEFILE_REPLACE_EXISTING)`-
  documented position, or Linux-first semantics, recorded in an ADR. Not
  decided here — flagged.
* The `__crash-save` `rotate-incremental` op seeds the destination with the
  original before appending (the append sink refuses to create files, and
  the harness keeps a pristine source elsewhere). This matches the real
  editor flow (append to the opened document) but means the harness's
  "absent destination" outcome only applies to the rewrite paths.

---

## 3. Rules compliance

* **No new dependencies.** Everything uses existing workspace crates; the
  kill-test PRNG is a 15-line xorshift, not a crate. (ADR-P0021: nothing to
  justify.)
* **Original buffer immutable (I1)**: `verify_structural` takes `&[u8]` and
  only reads; the incremental path appends through `OpenOptions::append`
  and never touches the prefix; the harness asserts prefix byte-identity
  1500 times.
* **Every parse under a Budget**: `verify_structural` charges the caller's
  guard (objects, depth, ticks) and propagates typed `BUDGET_*` errors;
  `parse_revisions_resilient` charges each rollback attempt.
* **`cargo xtask check-layers`**: green (verify stays in-crate; the CLI
  gate sits at L5 where IO is allowed).
* **Rustdoc on every public item + `# Budget` / `# Malformed Input`**
  sections on document-consuming fns: done; `check-contracts` green.
* **`cargo xtask lint` + `cargo test --workspace`**: green at every commit
  point.

## 4. Filed follow-up

`SL-1A.WRITE.08 — Full render+text output verification (G2)` added to
`11A-PHASE-1A-tools.md` (render+extract every page of the output and
compare against the pre-save render, replacing the structural standard for
content-rewriting operations). Checkbox unchecked — G2-blocked.

## 5. What turned out to be wrong or under-specified in the task text

1. WRITE.02's incremental writer already existed (`write_incremental_update`)
   but nothing appended it to a *file* — the tools only had
   truncate-and-write. The task said "exercises AppendSink + atomic commit
   end-to-end"; in reality the sink path was unused by tools, so the work
   was (a) build the file-backed append sink, (b) route every tool through
   atomic commit, (c) add the reader-side rollback. None of that was a
   redesign — the ordering discipline was already correct — but the task's
   "if the guarantee does not hold, fix the sink path" clause did trigger.
2. `verify_structural`'s "page count matches expectation" needed a decision
   the task didn't specify: expectations come from surveying the *input*
   (shared count definitions), with per-tool overrides for operations that
   legitimately change counts (split/delete keep only surviving pages'
   annotations; merge sums inputs). This convention is now normative in the
   module docs.
3. The task's "kill at ~500 random points" needs a definition of random:
   seeded xorshift64*, fixed seeds per operation, documented so any run is
   exactly reproducible.

## 6. Sign-off checklist for the human owner

- [ ] Review `verify_structural`'s fault model (is `Unparsable` on a torn
      newest revision the right *fault* for a writer output, given the I5
      rollback makes it readable?).
- [ ] Confirm the count-definition convention (§5.2) or amend it.
- [ ] Confirm the Windows rename atomicity position (§2.4) or open the ADR.
- [ ] Verify the first CI run of `write05-oracle` + `write06-kill` on a
      real runner (jobs wired; runner-green pending).
- [ ] Sign the `11A` checkboxes for WRITE.05 / WRITE.06.
