<!-- @generated from wit/ by crates/lattice-plugin-api (render.rs).
     Do not edit: run `UPDATE_SITE_REFERENCE=1 cargo test -p lattice-plugin-api`. -->

# `command`

**Direction:** shared types only (not called directly) · **Capability:** none (pure data / dispatch)

Mirrors `CommandRegistry` + `CommandInvocation` + the closed `Effect`
enum (lattice-grammar). Guest→host `invoke`; host→guest `apply`. The
`effect` WIT variant mirrors the ~105-variant enum whole (§4.4) so the
boundary stays typed. Populated in PH7.3 (Effect round-trip) / PH7.7.

## Functions (0)

_(none — a shared type interface)_

