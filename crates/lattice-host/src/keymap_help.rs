//! K.3.2 (2026-06-02): help-prefix (`<C-h>` map) bindings.
//!
//! Emacs-style discoverability for the §5.11 self-documenting
//! help facility. From any Normal-mode buffer, `<C-h>` is a
//! prefix that opens the help workflow:
//!
//! | Chord | Command | What it does |
//! |---|---|---|
//! | `<C-h> <C-h>` | `:help-for-help` | Open help index. |
//! | `<C-h> ?` | `:help-for-help` | Alias — easier to type. |
//! | `<C-h> k` | `:describe-key` | Prompt for chord, show binding. |
//! | `<C-h> c` | `:describe-command` | Prompt for command name. |
//! | `<C-h> o` | `:describe-option` | Prompt for option name. |
//! | `<C-h> e` | `:describe-event` | Prompt for typed-event name. |
//! | `<C-h> f` | `:describe-element` | Prompt for theme element / face name. |
//! | `<C-h> m` | `:describe-active-modes` | Active major + minors on this buffer, with their chords. |
//! | `<C-h> M` | `:describe-mode` | Prompt for a mode name; show that mode's metadata. |
//! | `<C-h> b` | `:describe-buffer` | Buffer metadata (kind, flags, modes, …). |
//! | `<C-h> a` | `:apropos` | Cross-cutting search. |
//! | `<C-h> K` | `:describe-bindings` | Chords that can fire on this buffer. `:keymap` remains the full catalog. |
//!
//! ## DAM.4 correction (2026-08-04)
//!
//! K.3.2 bound `<C-h>m` to `:describe-mode`, whose `name` arg is
//! `ArgDefault::Required` — so the no-arg invocation armed the
//! interactive `mode:` prompt and asked which mode to describe. It
//! never showed the active modes, though this table, the
//! `HelpPrefixEntry` doc, and keymap-architecture §12.1 all said it
//! did. `<C-h>m` now routes to `:describe-active-modes`; `<C-h>M`
//! keeps the prompt-for-any-mode path that `<C-h>m` was accidentally
//! providing, so nothing is lost.
//!
//! The `m` / `M` pair reads listing-then-specific while the older
//! `k` / `K` pair reads specific-then-listing. Deliberate: lowercase
//! is the common case in both, and `C-h m` = *active* modes is the
//! emacs muscle memory worth preserving.
//!
//! ## Design choices (per K.3 slice plan)
//!
//! - **No bare `<C-h>` leaf.** K.3.0's trie audit found that
//!   today's matcher returns `Bound` immediately when a node
//!   carries a binding, even if it also has children — and the
//!   dispatcher has no `timeoutlen` machinery to wait for a
//!   follow-on chord. Vim's "leaf + prefix" ambiguity needs
//!   timer-driven dispatch which isn't worth introducing for a
//!   single help affordance. Instead, `<C-h>` is a pure prefix
//!   node (returns `Partial`), and the slice plan's listed
//!   alternative `<C-h><C-h>` (plus the easier-to-type
//!   `<C-h>?`) is the explicit help-for-help entry. One
//!   keystroke of extra friction; zero new infrastructure.
//! - **Normal mode only.** Insert / Visual / OperatorPending /
//!   Cmdline retain their existing `<C-h>` semantics (Insert
//!   keeps backspace; cmdline keeps cmdline-backspace). The
//!   K.1.c per-keystroke filter naturally enforces this because
//!   bindings are registered with `BindingMode::Normal`; other
//!   modes simply don't match.
//! - **`KeymapLayer::Builtin`.** Universal availability across
//!   every Normal-mode buffer — same shape as Normal-mode
//!   motions, operators, and the rest of the vim default
//!   catalog. The mode-architecture §13 convention ("feature-
//!   gated bindings live at MinorMode") doesn't apply here
//!   because the help prefix isn't feature-gated; it's a
//!   universal discoverability affordance.

use lattice_grammar::CommandRegistry;
use lattice_grammar::command::CommandInvocation;
use lattice_grammar::source::SourceLocation;

use crate::chord::{KeyChord, KeyKind, KeyMods};
use crate::keymap::BindingMode;
use crate::keymap_registry::KeymapHandle;
use crate::keymap_trie::{ChordPattern, KeymapLayer};

/// One row of the K.3.2 help-prefix binding table.
///
/// `chord` is a two-element chord sequence (every help-prefix
/// binding is `<C-h>` followed by exactly one key); `command`
/// is the canonical name registered against the
/// `CommandRegistry` (always an `ex:` form for this slice —
/// every help-prefix chord routes to an ex-command, no App-side
/// actions).
struct HelpPrefixEntry {
    chord: &'static [ChordPattern; 2],
    command: &'static str,
    doc: &'static str,
}

/// Register the `<C-h>` help-prefix bindings into the supplied
/// handle's `Builtin` layer at `BindingMode::Normal`.
///
/// Looks up each row's canonical command name in the
/// `CommandRegistry`. If a name doesn't resolve (catalog drift
/// — every help-prefix command should be registered at boot
/// before this pass runs), emits a `tracing::warn!` and skips
/// the row rather than panicking; matches the K.2.4.A.0.3
/// translation-pass convention.
pub fn register_help_prefix_bindings(handle: &KeymapHandle, command_registry: &CommandRegistry) {
    let layer = KeymapLayer::Builtin;
    let mode = BindingMode::Normal;

    for entry in help_prefix_table() {
        let Some(cmd_id) = command_registry.id_by_name(entry.command) else {
            tracing::warn!(
                command = entry.command,
                doc = entry.doc,
                "keymap_help: command not registered in CommandRegistry; skipping <C-h>-prefix binding",
            );
            continue;
        };
        handle.bind(
            layer,
            mode,
            entry.chord,
            CommandInvocation::of(cmd_id),
            SourceLocation::builtin_file(file!(), line!()),
        );
    }
}

/// Compile-time-static help-prefix binding table. Each row
/// pairs a 2-chord sequence with the canonical ex-command name.
fn help_prefix_table() -> &'static [HelpPrefixEntry] {
    const C_H_C_H: &[ChordPattern; 2] = &[
        ChordPattern::Literal(KeyChord {
            key: KeyKind::Char('h'),
            mods: KeyMods::CTRL,
        }),
        ChordPattern::Literal(KeyChord {
            key: KeyKind::Char('h'),
            mods: KeyMods::CTRL,
        }),
    ];
    const C_H_QUESTION: &[ChordPattern; 2] = &[
        ChordPattern::Literal(KeyChord {
            key: KeyKind::Char('h'),
            mods: KeyMods::CTRL,
        }),
        ChordPattern::Literal(KeyChord {
            key: KeyKind::Char('?'),
            mods: KeyMods::NONE,
        }),
    ];
    const C_H_K: &[ChordPattern; 2] = &[
        ChordPattern::Literal(KeyChord {
            key: KeyKind::Char('h'),
            mods: KeyMods::CTRL,
        }),
        ChordPattern::Literal(KeyChord {
            key: KeyKind::Char('k'),
            mods: KeyMods::NONE,
        }),
    ];
    const C_H_C: &[ChordPattern; 2] = &[
        ChordPattern::Literal(KeyChord {
            key: KeyKind::Char('h'),
            mods: KeyMods::CTRL,
        }),
        ChordPattern::Literal(KeyChord {
            key: KeyKind::Char('c'),
            mods: KeyMods::NONE,
        }),
    ];
    const C_H_O: &[ChordPattern; 2] = &[
        ChordPattern::Literal(KeyChord {
            key: KeyKind::Char('h'),
            mods: KeyMods::CTRL,
        }),
        ChordPattern::Literal(KeyChord {
            key: KeyKind::Char('o'),
            mods: KeyMods::NONE,
        }),
    ];
    const C_H_E: &[ChordPattern; 2] = &[
        ChordPattern::Literal(KeyChord {
            key: KeyKind::Char('h'),
            mods: KeyMods::CTRL,
        }),
        ChordPattern::Literal(KeyChord {
            key: KeyKind::Char('e'),
            mods: KeyMods::NONE,
        }),
    ];
    const C_H_F: &[ChordPattern; 2] = &[
        ChordPattern::Literal(KeyChord {
            key: KeyKind::Char('h'),
            mods: KeyMods::CTRL,
        }),
        ChordPattern::Literal(KeyChord {
            key: KeyKind::Char('f'),
            mods: KeyMods::NONE,
        }),
    ];
    const C_H_M: &[ChordPattern; 2] = &[
        ChordPattern::Literal(KeyChord {
            key: KeyKind::Char('h'),
            mods: KeyMods::CTRL,
        }),
        ChordPattern::Literal(KeyChord {
            key: KeyKind::Char('m'),
            mods: KeyMods::NONE,
        }),
    ];
    const C_H_CAP_M: &[ChordPattern; 2] = &[
        ChordPattern::Literal(KeyChord {
            key: KeyKind::Char('h'),
            mods: KeyMods::CTRL,
        }),
        ChordPattern::Literal(KeyChord {
            key: KeyKind::Char('M'),
            mods: KeyMods::NONE,
        }),
    ];
    const C_H_B: &[ChordPattern; 2] = &[
        ChordPattern::Literal(KeyChord {
            key: KeyKind::Char('h'),
            mods: KeyMods::CTRL,
        }),
        ChordPattern::Literal(KeyChord {
            key: KeyKind::Char('b'),
            mods: KeyMods::NONE,
        }),
    ];
    const C_H_A: &[ChordPattern; 2] = &[
        ChordPattern::Literal(KeyChord {
            key: KeyKind::Char('h'),
            mods: KeyMods::CTRL,
        }),
        ChordPattern::Literal(KeyChord {
            key: KeyKind::Char('a'),
            mods: KeyMods::NONE,
        }),
    ];
    const C_H_CAP_K: &[ChordPattern; 2] = &[
        ChordPattern::Literal(KeyChord {
            key: KeyKind::Char('h'),
            mods: KeyMods::CTRL,
        }),
        ChordPattern::Literal(KeyChord {
            key: KeyKind::Char('K'),
            mods: KeyMods::NONE,
        }),
    ];

    &[
        HelpPrefixEntry {
            chord: C_H_C_H,
            command: "ex:help",
            doc: "Open the help-for-help index (alias of `<C-h>?`).",
        },
        HelpPrefixEntry {
            chord: C_H_QUESTION,
            command: "ex:help",
            doc: "Open the help-for-help index.",
        },
        HelpPrefixEntry {
            chord: C_H_K,
            command: "ex:describe-key",
            doc: "Prompt for a chord and show its resolved binding + provenance.",
        },
        HelpPrefixEntry {
            chord: C_H_C,
            command: "ex:describe-command",
            doc: "Prompt for a command name and show its metadata.",
        },
        HelpPrefixEntry {
            chord: C_H_O,
            command: "ex:describe-option",
            doc: "Prompt for an option name and show its metadata + current value.",
        },
        HelpPrefixEntry {
            chord: C_H_E,
            command: "ex:describe-event",
            doc: "Prompt for a typed-event name and show its descriptor.",
        },
        HelpPrefixEntry {
            chord: C_H_F,
            command: "ex:describe-element",
            doc: "Prompt for a theme element / face name and show its metadata + resolved style.",
        },
        HelpPrefixEntry {
            chord: C_H_M,
            command: "ex:describe-active-modes",
            doc: "Show the active major + minor modes on the current buffer, with their chords.",
        },
        HelpPrefixEntry {
            chord: C_H_CAP_M,
            command: "ex:describe-mode",
            doc: "Prompt for a mode name and show that mode's metadata.",
        },
        HelpPrefixEntry {
            chord: C_H_B,
            command: "ex:describe-buffer",
            doc: "Show metadata for the current buffer (kind, flags, mode stack, …).",
        },
        HelpPrefixEntry {
            chord: C_H_A,
            command: "ex:apropos",
            doc: "Cross-cutting search across commands / options / events.",
        },
        HelpPrefixEntry {
            chord: C_H_CAP_K,
            command: "ex:describe-bindings",
            doc: "List the chords that can fire on the current buffer.",
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    use lattice_grammar::CommandRegistry;

    use crate::keymap_registry::KeymapHandle;
    use crate::keymap_trie::LookupResult;

    /// Build a CommandRegistry populated with the builtins +
    /// ex-commands every help-prefix chord references.
    fn registry_with_help_commands() -> CommandRegistry {
        let mut r = CommandRegistry::new();
        let _ = lattice_grammar::builtins::populate(&mut r);
        let _ = lattice_grammar::ex_commands::populate(&mut r);
        r
    }

    fn lookup_normal(handle: &KeymapHandle, chords: &[KeyChord]) -> LookupResult {
        handle.lookup_with_context(BindingMode::Normal, chords, &[])
    }

    #[test]
    fn help_prefix_chord_table_resolves_all_commands() {
        // Every row's `command` name must resolve against the
        // registry populated by `ex_commands::populate`. Catches
        // catalog drift at test time — if a row points at an
        // ex-command name that doesn't exist, this test fails
        // loudly rather than the binding silently warn-and-skip
        // at boot.
        let registry = registry_with_help_commands();
        for entry in help_prefix_table() {
            assert!(
                registry.id_by_name(entry.command).is_some(),
                "help-prefix row `{}` ({}) references unregistered command",
                entry.command,
                entry.doc,
            );
        }
    }

    #[test]
    fn register_help_prefix_binds_double_ctrl_h_to_ex_help() {
        let handle = KeymapHandle::new();
        let registry = registry_with_help_commands();
        register_help_prefix_bindings(&handle, &registry);

        let expected = registry
            .id_by_name("ex:help")
            .expect("ex:help registered by ex_commands::populate");
        match lookup_normal(&handle, &[KeyChord::ctrl('h'), KeyChord::ctrl('h')]) {
            LookupResult::Bound { command, .. } => {
                assert_eq!(command.command.command, expected);
                assert_eq!(command.layer, KeymapLayer::Builtin);
            }
            other => panic!("expected Bound on <C-h><C-h>, got {other:?}"),
        }
    }

    #[test]
    fn register_help_prefix_binds_describe_key_chord() {
        let handle = KeymapHandle::new();
        let registry = registry_with_help_commands();
        register_help_prefix_bindings(&handle, &registry);

        let expected = registry
            .id_by_name("ex:describe-key")
            .expect("ex:describe-key registered");
        match lookup_normal(&handle, &[KeyChord::ctrl('h'), KeyChord::char('k')]) {
            LookupResult::Bound { command, .. } => {
                assert_eq!(command.command.command, expected);
            }
            other => panic!("expected Bound on <C-h>k, got {other:?}"),
        }
    }

    #[test]
    fn ctrl_h_m_shows_active_modes_without_prompting() {
        // DAM.4, the whole point of the arc. `<C-h>m` must route to
        // `:describe-active-modes` — a no-arg command — not to
        // `:describe-mode`, whose required arg armed the `mode:`
        // prompt and made the chord ask a question instead of
        // answering one.
        let handle = KeymapHandle::new();
        let registry = registry_with_help_commands();
        register_help_prefix_bindings(&handle, &registry);

        let expected = registry
            .id_by_name("ex:describe-active-modes")
            .expect("ex:describe-active-modes registered");
        match lookup_normal(&handle, &[KeyChord::ctrl('h'), KeyChord::char('m')]) {
            LookupResult::Bound { command, .. } => {
                assert_eq!(command.command.command, expected);
                assert_eq!(command.layer, KeymapLayer::Builtin);
            }
            other => panic!("expected Bound on <C-h>m, got {other:?}"),
        }

        // And it takes no args, so the interactive prompt cannot arm.
        let spec = registry
            .lookup_by_name("ex:describe-active-modes")
            .expect("registered");
        assert!(
            spec.args_schema.is_empty(),
            "<C-h>m must not be able to prompt",
        );
    }

    #[test]
    fn ctrl_h_capital_m_keeps_the_prompt_for_any_mode_path() {
        // The prompt `<C-h>m` was accidentally providing is preserved
        // on `<C-h>M` rather than removed — `:describe-mode` still
        // has its required arg + `gen:modes` completion.
        let handle = KeymapHandle::new();
        let registry = registry_with_help_commands();
        register_help_prefix_bindings(&handle, &registry);

        let expected = registry
            .id_by_name("ex:describe-mode")
            .expect("ex:describe-mode registered");
        match lookup_normal(&handle, &[KeyChord::ctrl('h'), KeyChord::char('M')]) {
            LookupResult::Bound { command, .. } => {
                assert_eq!(command.command.command, expected);
            }
            other => panic!("expected Bound on <C-h>M, got {other:?}"),
        }

        let spec = registry
            .lookup_by_name("ex:describe-mode")
            .expect("registered");
        assert_eq!(
            spec.args_schema.len(),
            1,
            "<C-h>M must still prompt for a mode name",
        );
    }

    #[test]
    fn ctrl_h_m_and_capital_m_are_distinct_bindings() {
        // Case matters in the trie: a regression that folded `M` into
        // `m` would silently restore the old prompting behaviour on
        // the chord users reach for most.
        let handle = KeymapHandle::new();
        let registry = registry_with_help_commands();
        register_help_prefix_bindings(&handle, &registry);

        let lower = match lookup_normal(&handle, &[KeyChord::ctrl('h'), KeyChord::char('m')]) {
            LookupResult::Bound { command, .. } => command.command.command,
            other => panic!("expected Bound on <C-h>m, got {other:?}"),
        };
        let upper = match lookup_normal(&handle, &[KeyChord::ctrl('h'), KeyChord::char('M')]) {
            LookupResult::Bound { command, .. } => command.command.command,
            other => panic!("expected Bound on <C-h>M, got {other:?}"),
        };
        assert_ne!(lower, upper, "<C-h>m and <C-h>M must not collapse");
    }

    #[test]
    fn capital_m_help_prefix_does_not_fire_outside_normal() {
        // Same mode-scope guarantee the rest of the map has — the new
        // row must not leak into Insert/Visual/etc.
        let handle = help_prefix_handle_for_mode_scope_tests();
        for mode in [
            BindingMode::Insert,
            BindingMode::Visual,
            BindingMode::OperatorPending,
            BindingMode::Command,
            BindingMode::Search,
            BindingMode::Replace,
        ] {
            let result =
                handle.lookup_with_context(mode, &[KeyChord::ctrl('h'), KeyChord::char('M')], &[]);
            assert!(
                matches!(result, LookupResult::Unbound),
                "<C-h>M leaked into {mode:?}",
            );
        }
    }

    #[test]
    fn ctrl_h_capital_k_scopes_bindings_to_the_buffer() {
        // DAM.6: `<C-h>K` answers "what can I press here", so it
        // routes to `:describe-bindings`, not `:keymap`. `:keymap`
        // stays reachable as the exhaustive static catalog — it is
        // just no longer what the chord opens.
        let handle = KeymapHandle::new();
        let registry = registry_with_help_commands();
        register_help_prefix_bindings(&handle, &registry);

        let expected = registry
            .id_by_name("ex:describe-bindings")
            .expect("ex:describe-bindings registered");
        let keymap_id = registry
            .id_by_name("ex:keymap")
            .expect("ex:keymap still registered");
        match lookup_normal(&handle, &[KeyChord::ctrl('h'), KeyChord::char('K')]) {
            LookupResult::Bound { command, .. } => {
                assert_eq!(command.command.command, expected);
                assert_ne!(
                    command.command.command, keymap_id,
                    "<C-h>K must no longer open the full catalog",
                );
            }
            other => panic!("expected Bound on <C-h>K, got {other:?}"),
        }
    }

    #[test]
    fn bare_ctrl_h_is_partial_not_bound() {
        // Option 2 (K.3.0): no bare <C-h> leaf binding. The
        // node is a pure prefix; lookup with the single chord
        // returns `Partial` so the App's AbsorbPartialChord
        // path takes over and waits for the next chord.
        let handle = KeymapHandle::new();
        let registry = registry_with_help_commands();
        register_help_prefix_bindings(&handle, &registry);

        match lookup_normal(&handle, &[KeyChord::ctrl('h')]) {
            LookupResult::Partial => { /* expected */ }
            other => panic!("expected Partial on bare <C-h>, got {other:?}"),
        }
    }

    #[test]
    fn help_prefix_does_not_fire_in_insert_mode() {
        // K.3.3 mode-scope: the bindings register at
        // BindingMode::Normal. Insert-mode lookup must miss —
        // <C-h> in Insert retains its existing backspace
        // semantics.
        let handle = KeymapHandle::new();
        let registry = registry_with_help_commands();
        register_help_prefix_bindings(&handle, &registry);

        let result = handle.lookup_with_context(
            BindingMode::Insert,
            &[KeyChord::ctrl('h'), KeyChord::char('k')],
            &[],
        );
        assert!(matches!(result, LookupResult::Unbound));
    }

    #[test]
    fn register_help_prefix_warns_and_skips_unresolvable_commands() {
        // Defensive: an empty CommandRegistry — none of the
        // `ex:*` commands resolve, every row should warn + skip.
        // No bindings end up in the handle.
        let handle = KeymapHandle::new();
        let empty_registry = CommandRegistry::new();
        register_help_prefix_bindings(&handle, &empty_registry);

        match lookup_normal(&handle, &[KeyChord::ctrl('h'), KeyChord::ctrl('h')]) {
            LookupResult::Unbound => { /* expected */ }
            other => panic!("expected Unbound with empty registry, got {other:?}"),
        }
    }

    // ---- K.3.3: mode-scope enforcement ----
    //
    // K.3.2 covers Normal (positive) and Insert (negative).
    // K.3.3 fills out the remaining binding-modes the slice
    // plan calls out: Visual, OperatorPending, and Cmdline
    // (rich minibuffer's `Command` state). Each must NOT match
    // the help-prefix bindings, so existing per-mode <C-h>
    // semantics (cmdline backspace, etc.) keep working.

    fn help_prefix_handle_for_mode_scope_tests() -> KeymapHandle {
        let handle = KeymapHandle::new();
        let registry = registry_with_help_commands();
        register_help_prefix_bindings(&handle, &registry);
        handle
    }

    #[test]
    fn help_prefix_does_not_fire_in_visual_mode() {
        let handle = help_prefix_handle_for_mode_scope_tests();
        let result = handle.lookup_with_context(
            BindingMode::Visual,
            &[KeyChord::ctrl('h'), KeyChord::char('k')],
            &[],
        );
        assert!(matches!(result, LookupResult::Unbound));
    }

    #[test]
    fn help_prefix_does_not_fire_in_operator_pending_mode() {
        // OperatorPending sees `<C-h>` mid-operator (after `d`,
        // `y`, `c`, `>`, `<`). The help prefix must not absorb
        // those keystrokes — letting `<C-h>k` resolve in
        // OperatorPending would surprise users mid-operator.
        let handle = help_prefix_handle_for_mode_scope_tests();
        let result = handle.lookup_with_context(
            BindingMode::OperatorPending,
            &[KeyChord::ctrl('h'), KeyChord::char('k')],
            &[],
        );
        assert!(matches!(result, LookupResult::Unbound));
    }

    #[test]
    fn help_prefix_does_not_fire_in_cmdline_mode() {
        // BindingMode::Command is the `:`-line / rich
        // minibuffer state. <C-h> there has its own
        // backspace-style semantics for editing the cmdline;
        // the help-prefix bindings must not shadow them.
        let handle = help_prefix_handle_for_mode_scope_tests();
        let result = handle.lookup_with_context(
            BindingMode::Command,
            &[KeyChord::ctrl('h'), KeyChord::char('k')],
            &[],
        );
        assert!(matches!(result, LookupResult::Unbound));
    }

    #[test]
    fn help_prefix_does_not_fire_in_search_mode() {
        // Search (`/` `?`) minibuffer — same shape as Command;
        // <C-h> retains its existing search-line backspace.
        let handle = help_prefix_handle_for_mode_scope_tests();
        let result = handle.lookup_with_context(
            BindingMode::Search,
            &[KeyChord::ctrl('h'), KeyChord::char('k')],
            &[],
        );
        assert!(matches!(result, LookupResult::Unbound));
    }

    #[test]
    fn help_prefix_does_not_fire_in_replace_mode() {
        // Replace mode is character-typing-with-overstrike;
        // <C-h> there is restore-last-overwritten-byte. Help
        // prefix must not interfere.
        let handle = help_prefix_handle_for_mode_scope_tests();
        let result = handle.lookup_with_context(
            BindingMode::Replace,
            &[KeyChord::ctrl('h'), KeyChord::char('k')],
            &[],
        );
        assert!(matches!(result, LookupResult::Unbound));
    }
}
