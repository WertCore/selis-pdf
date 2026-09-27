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

The first deliberate first-party npm dependency is `@wertkit/ui` (this organisation's own design
system, alongside React and `@radix-ui/*`), introduced by SL-4.UI.14. It is pre-approved —
first-party, so no licence gate. The gap above is nonetheless **real and still open**: it is a
property of the npm ecosystem, not of any one dependency, and is owned by a pre-release
hardening pass. ADR-P0021 records it.
