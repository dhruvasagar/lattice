+++
title = "Developing Lattice — a contributor's getting-started guide"
+++


This is the on-ramp for writing code in Lattice. It gets you building and
testing, hands you the mental model you need before touching a hot path, and
walks three real "add your first X" changes end-to-end (a motion, an
ex-command, a minor mode).

It is deliberately **task-oriented and opinionated**. For the *what and why* of
the system, the authoritative sources are the design spec and the status
ledger — see [Navigating the docs](#9-navigating-the-docs) at the end. When this
guide and those disagree, **they win** and this guide is the bug.

> New to the project as a *user*? Read the [README](../../../README) first
> (what Lattice is, the quick start, the editor sanity tour), then come back
> here to start contributing.

---

## 1. Prerequisites and the dev loop

### Toolchain

- **Rust 1.94+ (edition 2024).** The exact compiler is pinned in
  [`rust-toolchain.toml`](../../../rust-toolchain.toml); `rustup` picks it up
  automatically inside the repo, so you test with the same compiler CI does. No
  extra setup — `cargo` just works.
- A POSIX terminal with 256-color + bracketed-paste support for the TUI.
- For the GPU renderer: a working GPUI/`wgpu` graphics stack (macOS or a Linux
  box with GPU drivers).

### Build and run

```sh
# TUI (default renderer) — this is your everyday loop.
cargo run -- some-file.rs

# GPUI (GPU renderer). Note BOTH the feature flag AND the runtime flag.
cargo run --features gui -- --gui some-file.rs
```

`--tui` / `--gui` are mutually exclusive runtime flags; the default is TUI.

> ### ⚠️ The one build trap that will waste your afternoon
>
> **`lattice-ui-gpui` is feature-gated. A plain `cargo build -p lattice-cli`
> does NOT compile it.** The GPUI renderer is an *optional* dependency of
> `lattice-cli` behind the `gui` cargo feature (`default = []`). So:
>
> - Edits to `crates/lattice-ui-gpui/**` are **invisible** to a plain
>   `cargo build/run -p lattice-cli` — cargo reports "Fresh / Finished 0.3s"
>   with no error even when your change wouldn't compile. A silent no-op.
> - To build/run the real GPUI peer: `cargo run --features gui -- --gui`.
> - To *type-check* a GPUI edit fast: `cargo build -p lattice-ui-gpui`
>   (add `--features window` to match the real windowed config).
>
> If you touched a renderer and the build was suspiciously instant, you built
> the wrong thing.

### Test, lint, format — run before every push

```sh
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace          # thousands of tests; runs in seconds
```

The lint policy is workspace-wide **`unsafe_code = "deny"`**. Exactly one module
opts in (`#![allow(unsafe_code)]`) for a single documented `from_utf8_unchecked`
on a streaming window; every other `unsafe` must justify its invariant in prose.

**Iterate on one crate** instead of the whole workspace while developing:

```sh
cargo test -p lattice-grammar          # just the grammar crate
cargo test -p lattice-host dispatch    # filter by test-name substring
cargo build -p lattice-ui-gpui         # type-check a GPUI edit (see trap above)
```

### Benchmarks and the latency ratchet

```sh
cargo bench --workspace
```

Keystroke→glyph latency is a **paramount goal measured in CI and ratcheted**:
the recorded distribution may only move *down*. If you touch the input/render
path, expect to justify the number. Results live in
[`../operations/benchmarks.md`](../operations/benchmarks).

### Logging and debugging a running editor

- Pass `--log-level debug` to opt into diagnostic logging.
- `info!`-level events also surface **inside the editor** in the `*messages*`
  buffer (open it via `:messages` / `:b *messages*`) — a real buffer, so vim
  motions and search work on your logs.
- **Per-keystroke / per-frame / per-tick / probe logs go to `debug!` or
  `trace!`, never `info!`.** `info!` fans out to both stderr *and* `*messages*`;
  a held key at 30 Hz would flood it. Reserve `info!` for one-shot,
  user-actionable events ("LSP server attached", "macro recording stopped").
- **Never `println!` / `eprintln!` in TUI paths.** Raw stdout/stderr writes
  corrupt the alt-screen. If a stray artifact "persists until a force redraw",
  suspect an out-of-band write, not a compose bug.

---

## 2. The mental model (architecture in practice)

You can write a lot of Lattice without internalizing this, but you will write
*wrong* Lattice on the hot paths. Read this once.

### Three layers, one direction of data

```
 UI layer          Core layer                       Plugin layer
 (renderers)       (the editor)                     (WASM)
 ─────────         ──────────────                   ─────────────
 lattice-ui-tui    lattice-runtime + core           lattice-plugin-host
 lattice-ui-gpui   + grammar + host + modes          (wasmtime, one Store
   │                 │                                 per plugin, a tokio
   │  input          │  one DocumentActor per doc       task each)
   │  ───────►       │  (a tokio task)                     ▲
   │  Command        │  owns the writable Document          │  WIT ABI
   │  Invocation     │  bounded mpsc mailbox                │  (typed messages)
   │                 │  publishes immutable snapshots       │
   │  ◄───────       │  via arc-swap ────────────────────── ┘
   │  DisplayMatrix  │
   ▼  (snapshot)     ▼
```

Layers communicate **only** via typed messages and **wait-free snapshot loads**.
The renderer is a pure consumer: it reads a published per-pane snapshot and
paints. It never holds a lock on editor state, never blocks on editor work, and
does **zero** I/O / parsing / shaping / tree-sitter walks per frame. That is
paramount goal #1, enforced architecturally.

### The actor: why `&mut Editor` never escapes

The editor runs on its **own dedicated thread** as a `DocumentActor`
(`lattice-runtime`). You do not get a `&mut Editor` from the UI thread — it is
compile-time impossible (`EditorActorHandle`). Instead:

- **Reads** are wait-free: the actor publishes an immutable `DocumentSnapshot`
  through `arc-swap` after every change; readers `.load()` it. No lock.
- **Writes** go through the mailbox: the UI translates input into a
  `CommandInvocation` and sends it; the actor applies it on its own thread and
  publishes a fresh snapshot.

Concretely, a keystroke's life:

```
key ──► UI translates to CommandInvocation
     ──► actor: lattice_grammar::execute(&registry, &mut document, …)
     ──► returns an Effect (Edits / Yank / ModeChange / AppAction / …)
     ──► actor applies the Effect, publishes a new snapshot
     ──► renderer loads the snapshot next frame and paints only what changed
```

Text is synchronous (the typed char lands immediately); syntax recolour may be
eventual (a frame or two later). The **keystroke UX contract**: only the edited
line may visibly change per keystroke; everything else stays pixel-stable.

### The grammar *is* the command API

There is no separate "command" system bolted onto vim. Operators, motions, text
objects, ex-commands, plugin contributions, and palette entries all share one
`CommandInvocation` and flow through one `execute(...)`. The `:` line is just a
parser front-end that builds a `CommandInvocation`. Every primitive is
reachable by name (`:motion goto-first-line`, `:operator delete word-forward`)
*and* carries help metadata (`:describe-command write`).

This is why **adding a motion or an operator is a first-class, small change**
(see the walkthroughs below) rather than a special case.

### Everything is a buffer

File tree, diagnostics, terminal, search results, `*messages*`, help — all are
`Document`s in one `BufferRegistry`, placed into panes by the user via splits.
`:bn` / `:bp` / `:ls` / `:bd` / `:b <name>` work uniformly across kinds. The
terminal is a real `Document`, so vim motions, search, marks, and text objects
apply to it with **no bespoke code**.

The load-bearing rule that follows from this: **buffers must not carry
kind-specific logic**. Never branch render / motion / scroll / cursor code on
`BufferKind`. When you feel the urge to write
`match buffer_kind { Help => …, _ => … }`, find the *property* that actually
differs (wrap on/off, read-only, line-count) and condition on that as a
per-buffer or global option any kind can have. Fix kind-specific bugs at the
`Document` trait impl, not with a renderer kind-gate.

---

## 3. The crate map — where things live

There are ~35 crates. You do not need all of them in your head; you need the
**layering** and where your change belongs. Bottom = no editor dependencies;
top = renderers.

```
 renderers        lattice-ui-tui   lattice-ui-gpui        (paint snapshots)
 ───────────────────────────────────────────────────────
 host             lattice-host                            (Editor: boot, dispatch,
                                                            buffer registry — a THIN
                                                            substrate; see §4)
 ───────────────────────────────────────────────────────
 feature crates   lattice-lsp  lattice-diff  lattice-ai   (each owns a mode +
                  lattice-terminal  lattice-oil            its keymaps + handlers;
                  lattice-file-tree  lattice-snippet …      installs via install(boot))
 ───────────────────────────────────────────────────────
 subsystems       lattice-mode      lattice-syntax        (mode trait/registry;
                  lattice-completion  lattice-picker        tree-sitter; completion;
                  lattice-config  lattice-help  lattice-lsp  options; help; …)
 ───────────────────────────────────────────────────────
 core             lattice-grammar   lattice-runtime       (vim grammar + registry;
                  lattice-core      lattice-cells          DocumentActor + snapshots;
                                                            Buffer/Document; cell grid)
 ───────────────────────────────────────────────────────
 bottom           lattice-protocol                        (Position, Range, Edit,
                                                            Selection, Event, IDs)
 ───────────────────────────────────────────────────────
 plugins          lattice-plugin-host  lattice-plugin-api  (wasmtime Component-Model
                  lattice-plugin-sdk   wit/                  host + WIT API + guest SDK)
```

The README's [crate map table](../../../README) has a one-line purpose +
status for every crate — keep it open while you find your footing.

**Where do I start reading?** For a grammar change: `lattice-grammar`
(`builtins.rs`, `ex_commands.rs`, `registry.rs`, `dispatcher.rs`). For a
feature/provider: an existing feature crate as a template (`lattice-oil` and
`lattice-diff` are compact and idiomatic). For the boot/dispatch host: start
from `lattice-host/src/editor_boot.rs` and follow the `install(boot)` calls.

---

## 4. The mode-ownership rule (the load-bearing pattern)

This is the single most important convention in the codebase, and the one new
contributors most often violate. Read it before adding any feature.

**A feature is a mode, and the mode owns its full surface.** That means the
mode owns:

- its keymaps (chord → command),
- its **action-handler bodies** (the code that runs when its chords fire),
- its lifecycle subscriptions, decorations, completion sources, option
  overrides, capability requirements,
- the *production* of its buffers.

**The host (`lattice-host`) is a thin substrate.** It exposes generic
primitives (a buffer-store service, a tick-callback registry, the event bus, an
action-handler registry, a generic chord dispatcher) and **nothing
feature-specific**.

### The acid test

> A new provider crate landing should require **zero** new `Editor::` methods
> in `lattice-host`, and **zero** new variants in the host's `Action` enum.

The provider contributes its commands and its `ActionId`s through the registry;
the handler *bodies* live in the provider's own crate; contributions reach the
native registries through the generic `install(boot)` seam.

### The failure mode this prevents: the half-migration

The recurring drift is: you move the *keymap* into the mode but leave the
*handler body* (`Editor::do_my_thing`) in the host's dispatcher. That is a
half-migration — it looks migrated but the host still knows about your feature.
Keep **both** the binding choice and the handler body with the mode.

Feature-specific keymaps live at `KeymapLayer::MinorMode(mode_id)` (or
`MajorMode`), **never** at `KeymapLayer::Builtin` — Builtin is the universal vim
grammar that fires in every buffer.

### Trait method vs. mode helper

When you have mode-relevant data to publish, decide where it lives by **who
consumes it**:

- **A `Document` trait method** — only when *generic host machinery* (the
  renderer's gutter, the generic dispatch loop, the position-history walker)
  reads it *uniformly across all buffer kinds*.
- **A free function / handle method in the owning crate** — when only that
  *specific mode's handler* reads it. Mode-consumed data must **not** widen the
  `Document` trait surface; the mode imports the helper directly.

Rule of thumb: *uniform-host consumer ⇒ trait method; mode consumer ⇒ helper.*

The full rationale, plus the "everything is a buffer" enforcement details, are
in [`CLAUDE.md`](../../../CLAUDE) under "Standing rules" — that file is a
distilled record of real corrections and is worth reading in full before a
non-trivial change.

---

## 5. Worked example: add a built-in motion

Motions are the smallest first-class grammar change. Built-ins are registered
in `crates/lattice-grammar/src/builtins.rs` via `register_motion`.

**Step 1 — write the evaluator.** A motion takes a `&MotionContext` (buffer,
cursor `from`, count, cancellation token, optional tree-sitter resolver) and
returns where the cursor should land:

```rust
// in lattice-grammar/src/builtins.rs (or a submodule)
fn motion_line_end(ctx: &MotionContext) -> GrammarResult<MotionResult> {
    let line = ctx.buffer.line(ctx.from.line);
    Ok(MotionResult {
        target: Position::new(ctx.from.line, line.len_chars_no_newline()),
        linewise: false,
    })
}
```

**Step 2 — register it** inside `populate(registry: &mut CommandRegistry)`,
next to the existing motions. The name is **namespaced** (`motion:...`); the doc
string is what `:describe-command` shows:

```rust
let line_end = registry.register_motion(
    "motion:line-end",
    "Move to the last character of the current line (vim's `$`).",
    MotionSpec {
        jump: false,
        exclusive: false,
        apply: Arc::new(motion_line_end),
        args_schema: vec![],
    },
);
```

The returned `MotionId` is stashed in the `Builtins` struct so the default
keymap can bind a chord to it. To bind it to `$` you add it to the keymap
tables the same way the neighbouring motions are bound.

**Step 3 — test it (write the test first).** Grammar tests live beside the code
in `lattice-grammar`. Assert the behaviour *and* an edge case (empty line, last
line, with a count). Then:

```sh
cargo test -p lattice-grammar line_end
```

Because a motion is just a registration, the dispatcher, `:describe-command`,
completion, and dot-repeat pick it up for free — that is the grammar-is-the-API
payoff.

---

## 6. Worked example: add an ex-command

Ex-commands are registered in `crates/lattice-grammar/src/ex_commands.rs` via
`register_ex_command`. An ex-command **parses** its argument line and **returns
an `Effect`** the host applies — it does not mutate the document directly.

```rust
let reload = registry.register_ex_command(
    "ex:reload",
    "Reload the current buffer from disk, discarding unsaved changes (`:reload`).",
    ExCommandSpec {
        latency_class: LatencyClass::Display,   // touches disk → Display, not Reflex
        accepts_bang: false,
        accepts_range: false,
        parse_args: Arc::new(parse_no_args),    // reuse an existing parser
        apply: Arc::new(apply_reload),          // returns an Effect
        args_schema: vec![],                    // drives prompts / completion / help
        surface_form: SurfaceForm::Keyword,
    },
);
```

**Naming rule (important, and enforced by convention):** subsystem-coupled
commands register exactly **one** alias — the **dashed, namespaced** form
(`lsp-format`, `lsp-rename`, `claude-code-start`). Do **not** add collapsed
forms (`lspformat`), generic names (`format`, `rename`), or invent new 1–2
letter shorts. Vim-canonical shorts (`w`, `q`, `wq`, `bn`, `cn`) stay for
vim-canonical commands only.

`args_schema` is not boilerplate: it drives the command palette, missing-arg
prompts, `<Tab>` completion (via a named `completion` generator like
`gen:files`), validation, and `:describe-command` output. Fill it in.

**Where a command's handler body lives** depends on ownership: generic
grammar commands apply their `Effect` in the grammar/host path, but a
*feature's* commands are declared by the feature and their bodies live in the
feature's mode `action_handlers()` (chords) or the feature's `install(boot)`
`Effect` appliers — never as a new host `Action` variant (the acid test, §4).
`lattice-diff/src/install.rs` is the canonical example to copy.

---

## 7. Worked example: add a minor mode (a whole feature)

This is the shape every feature crate takes. `lattice-oil` and `lattice-diff`
are the templates. A mode is a struct implementing the `Mode` trait
(`lattice-mode/src/mode.rs`); most methods have sensible defaults, so a minimal
mode is small.

**Step 1 — define the mode and its identity:**

```rust
pub struct MyMode;

impl MyMode {
    pub fn mode_id() -> ModeId { ModeId::new("my-mode") }
}
```

**Step 2 — declare its keymap** (chord → command name). Feature keymaps are
mode-scoped, never Builtin:

```rust
fn my_mode_keymap_entries() -> &'static [KeymapEntry] {
    static ENTRIES: OnceLock<Vec<KeymapEntry>> = OnceLock::new();
    ENTRIES.get_or_init(|| {
        vec![keymap_entry!(
            mode: Normal,
            chord: "-",
            doc: "Do the thing this mode owns.",
            cmd: "action:my-thing"     // resolved by name against the registry
        )]
    })
}
```

**Step 3 — implement the trait.** Override only what you need; declare your
keymap; do per-buffer setup in `on_activate` and return a `Guard` whose `Drop`
cleans up (or `()` if there's nothing to tear down):

```rust
impl Mode for MyMode {
    type Guard = ();
    fn id(&self) -> ModeId { Self::mode_id() }
    fn kind(&self) -> ModeKind { ModeKind::Minor }
    fn keymap(&self) -> Keymap { Keymap::from_entries(my_mode_keymap_entries()) }
    fn on_activate(&self, _ctx: ModeContext) -> LifecycleFuture<'_, ()> {
        Box::pin(async { Ok(()) })   // subscribe to events, register providers, …
    }
}
```

**Step 4 — own the handler bodies.** The `"action:my-thing"` command your chord
resolves is *declared* against the registry, but its **body** is a mode
`action_handler` (registered from your crate), not a host method. Follow
`lattice-diff/src/install.rs::register_diff_actions` for the exact shape.

**Step 5 — wire it up with `install(boot)`.** This one function is the *only*
edit outside your crate you should need — one line in the host's boot list:

```rust
pub fn install(boot: &mut impl SubsystemBoot) {
    register_my_modes(boot.modes_mut());      // register the mode(s)
    register_my_commands(boot.commands_mut()); // register action/ex-command specs
    // on_activate + keymap() + action_handlers() are picked up by the host's
    // GENERIC mode-translation + action-handler walks — no host-side special case.
}
```

**The acid test, checked:** if adding your feature required a new `Editor::`
method or a new host `Action` variant, you took a wrong turn — the surface
belongs in your mode/crate, reached through the generic seam. Zero host edits
beyond the single `install` line is the target.

**Step 6 — test both throughput and runtime-responsiveness.** Provider tests
need coverage that the feature *works* AND that it never blocks the actor. The
editor actor runs on a `current_thread` runtime, which means every
`tokio::spawn` lands on the actor thread — default to `spawn_blocking` for
fs/cpu work. If you catch yourself writing `yield_now().await` in a tight loop,
that's a smell: the right shape is almost certainly `spawn_blocking`.

---

## 8. Testing conventions and the four-artefact rule

**A non-trivial change ships four things together, or it is incomplete:**

1. **Documentation** — a design fragment in `docs/dev/architecture/` for the
   *what and why* (contracts, data model, rejected alternatives), separate from
   the slice/sequencing plan.
2. **Benchmark coverage** where performance is relevant, so the impact is
   visible in `benchmarks.md` and CI.
3. **Test coverage** that exercises the new scenarios *and the failure modes* —
   not just the happy path.
4. **Graceful error handling** — log-and-skip on recoverable failures; never
   panic on the hot path; never swallow an error silently.

Beyond that:

- **TDD is the norm.** Write the failing test, watch it fail, implement, watch
  it pass. Tests for new behaviour land in the *same commit* as the behaviour.
- Tests live **beside the code** (`#[cfg(test)] mod tests`) for unit coverage
  and in `crates/<crate>/tests/` for integration/regression pins. Boot-contract
  regressions are pinned in `lattice-host/tests/` — if you change boot, those
  tell you what you broke.
- **Cross-renderer parity is lockstep.** If a change touches `lattice-ui-tui`
  (an `Effect` classifier arm, a gutter/sign variant, a `host_theme.*` field),
  update `lattice-ui-gpui` **in the same patch** — GPUI is a first-class peer,
  not a fallback. End-of-change, `grep` the gpui crate for your new variant; an
  empty grep means you missed it.

---

## 9. Navigating the docs

| Doc | What it is | Authoritative for |
|-----|------------|-------------------|
| [`architecture/design.md`](../architecture/design) | The design spec (v0.4). Terse, principle-led. | **What should exist and why.** |
| [`operations/implementation.md`](../operations/implementation) | Per-feature status ledger, updated per session. | **What currently exists.** Check here before assuming a symbol is implemented. |
| [`operations/slice-plans/`](../operations/) | Per-feature sequencing: slice IDs, dependencies, status icons. | The *when and in what order* of in-flight work. |
| [`operations/benchmarks.md`](../operations/benchmarks) | Latest measured latency/throughput numbers. | The perf bar the CI ratchet enforces. |
| [`../../../CLAUDE.md`](../../../CLAUDE) | Standing rules + conventions (distilled real corrections). | The *how we work* rules — mode-ownership, logging, naming, UX-over-goals. |
| [`architecture/comparison-zed.md`](../architecture/comparison-zed) | The architectural deep-dive vs. the closest peer. | Why the actor/mode/everything-is-a-buffer bets are what they are. |
| [`./plugin-authoring.md`](./plugin-authoring) | Writing WASM plugins against the WIT API. | The extension (plugin) path, as opposed to core contribution. |

**Two rules for reading:**

1. **`design.md` and `implementation.md` are the authoritative pair.** When any
   other doc, memory, or comment disagrees with them, they win.
2. **A doc mentioning a file / type / symbol is *not* evidence it exists.**
   Verify against the source and `implementation.md` before you build on it —
   the design doc describes the target, not necessarily today's code.

**Design fragments and slice plans stay in separate files.** The stable
*what/why* lives in `docs/dev/architecture/<feature>.md`; the churning
*when/how* (slice IDs, status icons) lives in `docs/dev/operations/`. New design
work follows the same split from day one.

---

## 10. Conventions cheat-sheet

- **Commits:** `<type>: <description>` (`feat`, `fix`, `refactor`, `docs`,
  `test`, `chore`, `perf`, `ci`). Each commit is a complete unit — one feature /
  fix / refactor — with its tests.
- **Branch, don't commit to the default branch** for non-trivial work; open an
  issue with the design rationale before a PR for anything significant (the
  design doc is load-bearing).
- **Priority order when things conflict:** UX > the four paramount goals
  (performance > extensibility > vim semantics > asynchronicity) > everything
  else. Architectural purity that produces a visibly worse editor is the wrong
  trade.
- **Immutability by default:** prefer returning new values to mutating in place;
  it keeps the snapshot model and concurrency honest.
- **Many small focused files** over few large ones; organize by feature/domain.
- **No backwards-compat shims** for vim or emacs configs — an explicit non-goal.
- **Diagnostic logs → `debug!`/`trace!`, never `info!`.** No raw stdout/stderr
  in TUI paths.
- **Confirm the plan before non-trivial work** touching architecture, public
  API, or cross-crate boundaries: walk the approach, the trade-offs, and the
  impact surface (which crates / docs / benches / tests) before writing code,
  and slice large changes so each slice lands green.

Welcome aboard.
