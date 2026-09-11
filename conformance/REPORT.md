# Selis conformance ladder

Every area starts at `None` and climbs as the corpus proves it. 
The published claim is never ahead of the measured level.

| Area | Level | Notes |
|---|---|---|
| COS object model | **Parse** | Parse: SL-1.COS.* DoDs; CONF.01 3,836-file parse coverage; CONF.05 zero unexplained refusals; round-trip proptests + cos_lex/cos_parse fuzz |
| Cross-reference tables and streams | **Parse** | Parse: SL-1.COS.* (xref + recovery); CONF.05 typed-refusal adjudication; incremental-save prefix proptests |
| Stream filters | **Parse** | Parse: SL-1.FILT.* + SL-2.FILT.01/02; encode-decode-identity proptests + filter_chain fuzz; CONF.04 JPX/JBIG2 decode suspects tracked |
| Encryption and permissions | **Parse** | Parse: SL-1.ENCRYPT.* DoDs; engine encrypt_write_roundtrip; RC4/AESV2 read over the corpus |
| Document model (catalog, pages, outlines) | **Parse** | Parse: SL-1.DOC.* DoDs; CONF.05 MediaBox fallback renders; /Rotate page geometry open in SL-2.RAST.12 (parse unaffected) |
| Rendering (content, raster) | **Parse** | Parse only: Bar A pass (97.0% <=25% @150, n=3,747 vs MuPDF) but Bar B fails (<=2% 76.2 vs best pair 82.3, 1.1pp past the 5pp allowance; SL-2.RAST.14). Open: size_skew 17 files (RAST.12), blank_selis 25 files (RAST.13), diff>=25 94 files (CONF.04) |
| Text and fonts | **None** | None at G2: extraction is Phase 3 (SL-3.*), unmeasured; render pixels are not a text-model claim |
| Annotations | **None** | None at G2 (target Identify): no detection gate run; /AP appearances suspected in blank_selis 25 files (SL-2.RAST.13) |
| AcroForm | **None** | None at G2 (target Identify): no AcroForm gate run |
| Editing and incremental save | **None** | None: incremental-save machinery exists but the 20-CONFORMANCE-PROGRAM.md §3 Edit gate suite is unmeasured (Phase 5) |
| Redaction | **None** | None (Phase 5; no crate yet) |
| Digital signatures | **None** | None (Phase 5; no crate yet) |
| PDF/A | **None** | None (Phase 9 target Author; structural rules evaluable via the DOC.09 registry) |
| PDF/UA | **None** | None (Phase 9 target Author; structural rules evaluable via the DOC.09 registry) |
