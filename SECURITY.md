# Security Policy

Report vulnerabilities to prasadzore129@yahoo.com.

Expect a response within 7 days; no public disclosure for 90 days.

## Dependency supply chain

Rust dependencies are gated: `cargo deny` against `deny.toml` (allow-list, `unmaintained` and
`wildcards` denied, `crates.io` only) plus `cargo vet`, both in CI. `Cargo.lock` is committed
and is the integrity record.

**The npm side is not yet gated the same way, and that is a known gap rather than an
oversight.** The web/extension packages install from npm via pnpm; `pnpm-lock.yaml` is the
integrity record and is committed, but there is no licence allow-list, no `unmaintained` check,
and no equivalent of `cargo vet` for the JS tree.

The first deliberate first-party npm dependency is `@wertkit/ui` (WertCore's design system,
alongside React and `@radix-ui/*`), introduced by SL-4.UI.14. Policy for it — allow-listing,
exact pinning, and the `"license": "UNLICENSED"` gap — is proposed in ADR-P0021 and is **not
yet accepted**. Until that amendment lands, treat the npm tree as ungated.
