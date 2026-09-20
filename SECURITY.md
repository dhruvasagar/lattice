# Security Policy

## Supported versions

Lattice is pre-1.0. Only the latest release receives fixes.

## Reporting a vulnerability

Report privately through
[GitHub Security Advisories](https://github.com/dhruvasagar/lattice/security/advisories/new).
Please don't open a public issue for a vulnerability.

Expect an acknowledgement within a week. As a one-maintainer alpha project
there is no formal SLA beyond that.

## Scope worth noting

Lattice runs plugins as WebAssembly components, capability-gated and
fuel-limited, each in its own store. A sandbox escape, a capability that
grants more than it declares, or a plugin reading outside its granted paths
is in scope and interesting. So is anything in the config path: a user's
`init.rs` is compiled to WASM and loaded with boot capabilities.

Binaries are unsigned at 0.9 — that is a known gap, documented in
[known limitations](./docs/user/known-limitations.md), not a vulnerability
report.
