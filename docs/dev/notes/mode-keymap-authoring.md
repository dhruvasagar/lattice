# Mode keymap authoring guide

A reference for crates that implement [`Mode`] and want to ship
keybindings alongside their options, decorations, and lifecycle.
Audience: mode-crate maintainers (lattice-multibuffer,
lattice-lsp, lattice-oil, lattice-snippet, lattice-file-tree,
lattice-help, lattice-terminal, lattice-syntax) and plugin
authors (Phase 7+).

This guide covers the substrate that K.2 landed (K.2.1 chord
primitives, K.2.2 BindingMode, K.2.3 chain form, K.2.4 host
translation pass, K.2.4.A.0 table form). Design rationale lives
in [`../architecture/keymap-architecture.md`](../architecture/keymap-architecture.md)
§11; this doc shows you how to use it.

## Pick a declaration form

Two forms cohabit on the same `Keymap` contribution. Pick one
or compose both.

### Chain form — `bind_chord`

For 1-5 bindings, or any binding whose `CommandInvocation` is
built dynamically at mode-construction time. Reads as a chain
of imperative `bind_chord` calls. `#[track_caller]` captures
the source line per call automatically.

```rust
use lattice_mode::{BindingMode, Keymap, LifecycleFuture, Mode, ModeContext, ModeId, ModeKind};

impl Mode for RefreshMode {
    type Guard = ();

    fn id(&self) -> ModeId { ModeId::new("refresh-mode") }
    fn kind(&self) -> ModeKind { ModeKind::Minor }

    fn keymap(&self) -> Keymap {
        Keymap::new()
            .bind_chord(BindingMode::Normal, "<C-r>", self.refresh_cmd.clone())
            .bind_chord(BindingMode::Normal, "<C-S-r>", self.force_refresh_cmd.clone())
    }

    fn on_activate(&self, _ctx: ModeContext) -> LifecycleFuture<'_, Self::Guard> {
        Box::pin(async { Ok(()) })
    }
}
```

`bind_chord` takes:
- `mode: BindingMode` — Normal / Insert / Visual / Replace /
  `After*` / Snippet / CompletionPopup / Help.
- `chord: &str` — the chord-sequence string. Same notation
  the host's `keymap_entry!` catalog uses (see below).
- `command: CommandInvocation` — typed; usually built once at
  mode construction and stored on `self`.

Source location captured via `std::panic::Location::caller()`
on the binding row. No `SourceLocation::builtin_file(file!(),
line!())` boilerplate per row.

**Malformed chord strings panic at boot.** A typo in
`"<C-Foo>"` surfaces at editor startup with a message naming
the chord and the caller location. The editor refuses to boot
rather than silently dropping the binding — same discipline
the host's static catalog uses.

### Table form — `from_entries`

For 5-20+ bindings against the standard command vocabulary.
Reads as a static catalog the eye can scan top-down. Each row
carries a `doc` string that `:describe-key` surfaces.

```rust
use lattice_mode::{
    keymap_entry, BindingMode, Keymap, KeymapEntry, LifecycleFuture, Mode,
    ModeContext, ModeId, ModeKind,
};
use std::sync::OnceLock;

fn multibuffer_keymap_entries() -> &'static [KeymapEntry] {
    static ENTRIES: OnceLock<Vec<KeymapEntry>> = OnceLock::new();
    ENTRIES.get_or_init(|| {
        vec![
            keymap_entry! { mode: Normal, chord: "]e",
                doc: "Jump to next excerpt",
                cmd: "multibuffer:excerpt-next" },
            keymap_entry! { mode: Normal, chord: "[e",
                doc: "Jump to previous excerpt",
                cmd: "multibuffer:excerpt-prev" },
            keymap_entry! { mode: Normal, chord: "]E",
                doc: "Jump to next file boundary",
                cmd: "multibuffer:file-next" },
            keymap_entry! { mode: Normal, chord: "[E",
                doc: "Jump to previous file boundary",
                cmd: "multibuffer:file-prev" },
        ]
    })
}

impl Mode for MultibufferMode {
    type Guard = ();

    fn id(&self) -> ModeId { ModeId::new("multibuffer-mode") }
    fn kind(&self) -> ModeKind { ModeKind::Major }

    fn keymap(&self) -> Keymap {
        Keymap::from_entries(multibuffer_keymap_entries())
    }

    fn on_activate(&self, _ctx: ModeContext) -> LifecycleFuture<'_, Self::Guard> {
        Box::pin(async { Ok(()) })
    }
}
```

`keymap_entry!` rows carry:
- `mode: <BindingMode variant>` — the binding-mode the chord
  resolves in.
- `chord: <chord notation string>` — the chord sequence. Same
  notation chain form uses.
- `doc: <one-line summary>` — surfaced in `:describe-key` and
  `:keymap`.
- `cmd: <canonical command name>` — `"motion:line-down"`,
  `"multibuffer:excerpt-next"`, … . Resolved against the
  `CommandRegistry` at registration time.

Source location captured per row via the macro's
`file!() + line!()` capture (through a `#[doc(hidden)]
KeymapEntry::__new` constructor; you can't bypass the
capture by hand-rolling a struct literal because the
`source` field is private).

**Resolution at registration time.** The host translation
pass (`crates/lattice-host/src/keymap_mode_contributions.rs`)
walks `Keymap::entries`, looks up each row's `cmd` name in
the `CommandRegistry`, mints a `CommandInvocation`, and
inserts a `KeymapBinding` carrying the row's `doc` + `source`.

Per-row outcomes:

| Row shape | Outcome |
|---|---|
| `cmd = None` | Silent skip. Synthetic catalog row (`PushDigit`, `SetPending`, find-char prefix, mark/register prefix). Informational for `:describe-key` / `:keymap`, not dispatchable. |
| `cmd = Some("known")`, registry miss | `tracing::warn!` and skip. Catalog drift — the binding declares an invocation no command implements. Hits `tracing` instead of panic so a single drifted row doesn't kill the editor. |
| `chord` fails `parse_chord_sequence` | `tracing::warn!` and skip. Defensive — the macro doesn't validate chord strings at expansion. |
| Otherwise | Resolved into a `KeymapBinding` and inserted into the matcher trie at `KeymapLayer::MinorMode(mode.id())`. |

### Compose

The two forms compose freely on the same `Keymap`. Use the
table for the bulk of the bindings (catalog of canonical
commands); chain `bind_chord` for the one or two bindings
whose `CommandInvocation` you construct dynamically:

```rust
fn keymap(&self) -> Keymap {
    Keymap::from_entries(multibuffer_keymap_entries())
        .bind_chord(BindingMode::Normal, "<C-r>", self.refresh_cmd.clone())
}
```

Both fields end up at the same `MinorMode(mode_id)` layer.

## Chord notation

Same notation across both forms.

| Form | Bytes | Parsed |
|---|---|---|
| Bare char | `"j"` | `[char('j')]` |
| Multi-char | `"gd"` | `[char('g'), char('d')]` |
| Modifier-prefixed | `"<C-d>"` | `[ctrl('d')]` |
| Multi-modifier | `"<C-S-x>"` | `[ctrl-shift('x')]` |
| Special key | `"<Esc>"` / `"<Tab>"` / `"<CR>"` / `"<BS>"` | `[special(Esc)]` etc. |
| Function key | `"<F1>"` to `"<F12>"` | `[special(F(N))]` |
| Sequence | `"<C-x>pp"` | `[ctrl('x'), char('p'), char('p')]` |
| Emacs prefix | `"<C-x><C-s>"` | `[ctrl('x'), ctrl('s')]` |
| Vim window prefix | `"<C-w>gd"` | `[ctrl('w'), char('g'), char('d')]` |
| Literal `<` | `"<lt>"` | `[char('<')]` |
| Shift on special | `"<S-Tab>"` | `[shift(Tab)]` |

Shift on a bare letter folds into case: `"A"` is shift+a;
`"<S-a>"` canonicalises to `"A"`.

## What you can't express

The chord-string notation has no wildcard. Bindings that
capture a follow-up char as a value (`'a` mark-target,
`"a` register-target, `fX` find-char-target, `qa`
macro-start, `@a` macro-play) need explicit
`ChordPattern::CharLiteral` in the chord vector and call
`Keymap::bind(KeymapBinding::new(...))` directly. Today
every wildcard-bearing binding lives in the host's built-in
catalog (Normal-mode marks / registers / find-char / macro
prefixes), so mode crates almost never need this lower-level
API.

## Lifecycle

`Mode::keymap()` is declarative — same value returned every
call. The host calls it once at registration time (boot +
dynamic `ModeRegistry::register`) and caches. Don't compute
based on per-buffer or per-activation state.

The host pushes the contribution as a `MinorMode(mode.id())`
layer; pushes are idempotent on `mode.id()` per K.1.b (a
second push for the same mode replaces the layer's bindings
rather than minting a sibling). Bindings stay registered for
the editor's lifetime; per-keystroke gating uses the buffer's
active-modes set (K.1.c precedence: minor-mode chord fires
only when the mode is active on the active buffer).

## Discovery

End users discover your bindings through `:describe-key`,
`:keymap`, and `:describe-mode <your-mode-name>` — see
[`../../user/modes.md`](../../user/modes.md#how-modes-interact-with-keybindings)
for the user-facing flow. Every binding's declaration site
flows through as a clickable `(file:...)` link, so users can
jump from `:describe-key ]e` to the `keymap_entry!` row in
your crate.

## See also

- [`../architecture/keymap-architecture.md`](../architecture/keymap-architecture.md) §11
  — design rationale.
- [`../archive/keymap-substrate.md`](../archive/keymap-substrate.md)
  — K.2 sub-arc sequencing.
- [`../../user/modes.md`](../../user/modes.md#how-modes-interact-with-keybindings)
  — user-facing description of how mode-contributed bindings
  surface.
