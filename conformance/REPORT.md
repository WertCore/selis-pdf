# Selis conformance ladder

Every area starts at `None` and climbs as the corpus proves it. 
The published claim is never ahead of the measured level.

| Area | Level | Notes |
|---|---|---|
| COS object model | **Parse** | Parse: SL-1.COS.* DoDs; CONF.01 3,836-file parse coverage; CONF.05 zero unexplained refusals; round-trip proptests + cos_lex/cos_parse fuzz |
| Cross-reference tables and streams | **Parse** | Parse: SL-1.COS.* (xref + recovery); CONF.05 typed-refusal adjudication; incremental-save prefix proptests |
| Stream filters | **Parse** | Parse: SL-1.FILT.* + SL-2.FILT.01/02; encode-decode-identity proptests + filter_chain/jpx_stream fuzz + filter-jpx corpus; CONF.04 JPX/JBIG2 decode suspects tracked. SL-1.FILT.08 JPX decode now runs under the sandboxed wasm host; the Render rung still needs the per-filter oracle tolerance legs (Ghostscript comparison unmeasured) |
| Encryption and permissions | **Parse** | Parse: SL-1.ENCRYPT.* DoDs; engine encrypt_write_roundtrip; RC4/AESV2 read over the corpus |
| Document model (catalog, pages, outlines) | **Parse** | Parse: SL-1.DOC.* DoDs; CONF.05 MediaBox fallback renders; /Rotate page geometry closed by SL-2.RAST.12 |
| Rendering (content, raster) | **Parse** | Parse: Bar A pass (97.2% <=25% @150, n=3,773 vs MuPDF, post-merge re-run) and Bar B now passes on the MuPDF leg (<=2% 80.9 vs best pair 82.3, 1.4pp inside the 5pp allowance; <=5% 86.8, <=10% 92.6) after SL-2.RAST.14 + the SL-3.TEXT.10 BT fix; size_skew closed by SL-2.RAST.12 (17 -> 0), blank_selis 25 -> 2 (typed /Redact-appearance deviations, SL-2.RAST.13), gross diff>=25 106 files tracked by SL-2.CONF.04. Render rung still unmeasured: the gate needs two independent oracles, and the CI legs against PDFium/pdf.js failed on stale digests (re-pinned in this wave) - confirming run pending |
| Text and fonts | **Parse** | Parse: text/font models build and decode; extraction measured across the 3,860-file corpus vs pinned mutool 1.23.0 (969 >=0.99, 978 >=0.98 of 1,745 comparable, median similarity 1.000, mean 0.761). Render withheld: the text-bearing slice itself (>=50 chars, n=635) renders 89.8% <=25% @150 vs MuPDF, below Bar A, and two-independent-oracle evidence awaits the CI re-run (SL-3.CONF.03/05). Extract withheld: 56.05% >=0.98 vs the >=98%-of-corpus G3 wording, which SL-0.ORACLE.04 shows sits under the oracle-vs-oracle ceiling (79.0% pdfium<->pdfjs, ~72.7% mutool pairs) - recalibration owed (SL-3.CONF.03) with the open cohorts: 539 deep-tail (SL-3.CONF.03), 62 silent annotation-AP empties (SL-3.TEXT.12), 13-file MuPDF word-split cohort awaiting OracleBug/OurBug verdicts (SL-3.CONF.04) |
| Annotations | **None** | None at G2 (target Identify): no detection gate run; /AP appearances render since SL-2.RAST.13 (blank_selis 25 -> 2, both typed /Redact deviations); extracting text from those appearances is SL-3.TEXT.12 (filed by SL-3.CONF.02: 62 silent empties whose page streams are genuinely empty) |
| AcroForm | **None** | None at G2 (target Identify): no AcroForm gate run |
| Editing and incremental save | **None** | None: incremental-save machinery exists but the 20-CONFORMANCE-PROGRAM.md §3 Edit gate suite is unmeasured (Phase 5) |
| Redaction | **None** | None (Phase 5; no crate yet) |
| Digital signatures | **None** | None (Phase 5; no crate yet) |
| PDF/A | **None** | None (Phase 9 target Author; structural rules evaluable via the DOC.09 registry) |
| PDF/UA | **None** | None (Phase 9 target Author; structural rules evaluable via the DOC.09 registry) |
