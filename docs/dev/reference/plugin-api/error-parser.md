<!-- @generated from wit/ by crates/lattice-plugin-api (render.rs).
     Do not edit: run `UPDATE_SITE_REFERENCE=1 cargo test -p lattice-plugin-api`. -->

# `error-parser`

**Direction:** shared types only (not called directly) · **Capability:** none (pure data / dispatch) · **Worlds:** `error-parser-plugin` (imports)

CM.6: plugin-contributed compilation-output parsers.

A plugin teaches lattice to recognise diagnostics from a build tool the
editor has never heard of. The native set covers cargo/rustc, gnu-style,
and test panics; everything else in the world — a bespoke linter, an
in-house build system, a language whose compiler predates all of them — is
what this is for.

### Line at a time, because the format is

`feed` takes ONE line and returns the entries that line *completed*. A
multi-line format (cargo's `error:` header followed by an `--> file:l:c`
arrow two lines later) keeps its own pending state inside the guest and
emits when the location arrives; a single-line format emits or returns
nothing. It mirrors the native `CompilationParser` trait exactly, because
a plugin parser and a native one are the same job and should not have
different shapes.

`reset` drops that pending state at the start of a run, so a build
interrupted mid-diagnostic cannot leak a half-parsed entry into the next
one.

### Where it runs

Off the UI and actor threads, in the compilation reader (see
`compilation-mode.md` §5). Not the keystroke path — but it IS the critical
path of a fast producer, so a guest that blocks here backs up a build's
output. The host budgets it per call like every other seam.

### What the host does with a bad entry

Validates and drops, never traps. A returned `line`/`col` is guest data
and the host treats it as untrusted: a nonsense path or an entry with an
empty path is logged at debug and skipped, exactly as a native parser's
malformed-but-claimed match is. One bad line must not fail a build.

## Functions (0)

_(none — a shared type interface)_

## Types (2)

### enum `severity`

```wit
enum severity {
    error,
    warning,
    info,
    note,
}
```

Severity of a parsed diagnostic. Mirrors the host's `ErrorSeverity`.

### record `entry`

```wit
record entry {
    path: string,
    line: u32,
    col: u32,
    severity: severity,
    message: string,
}
```

One diagnostic the parser recognised.

**Fields**

- `path`: `string` — Path as the tool printed it. Relative paths resolve against the
  compilation's working directory, host-side — the guest does not
  need to know where the build ran.
- `line`: `u32` — **0-based** line, like the host's `ErrorEntry`. A tool printing
  1-based line numbers (nearly all of them) subtracts one; doing
  that in the guest keeps one convention on this side of the
  boundary instead of two.
- `col`: `u32` — 0-based column.
- `severity`: [`severity`](#enum-severity)
- `message`: `string`

