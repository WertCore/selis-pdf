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
