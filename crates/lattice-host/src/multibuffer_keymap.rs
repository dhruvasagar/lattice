//! M.2.b.3 (2026-06-01): keymap layer for `multibuffer-mode`.
//!
//! Also (M.5 2026-06-01) home of `register_multibuffer_ex_commands`
//! which registers `:multibuffer-expand` / `:multibuffer-contract`
//! against the grammar's command registry.
//!
//! Binds the four excerpt-jump motions registered in
//! `lattice_multibuffer::motions` to their canonical chords:
//!
//! - `]e` → `multibuffer.next-excerpt-start`
//! - `[e` → `multibuffer.prev-excerpt-start`
//! - `]E` → `multibuffer.next-file-boundary`
//! - `[E` → `multibuffer.prev-file-boundary`
//!
//! Pushed at boot under `KeymapLayer::MajorMode(multibuffer-mode)`
//! so the bindings are visible only on multibuffer views.
//!
//! Mirrors the shape `crate::diff::mode::diff_mode_layer_bindings`
//! uses for `diff-mode`.

use std::collections::HashMap;
use std::sync::Arc;

use lattice_grammar::CommandInvocation;
use lattice_grammar::source::SourceLocation;
use lattice_multibuffer::MultibufferMotionIds;

use crate::chord::{KeyChord, KeyKind, KeyMods};
use crate::keymap::BindingMode;
use crate::keymap_trie::{BoundCommand, ChordPattern, KeymapLayer, KeymapTrie};

fn lit(c: char) -> ChordPattern {
    ChordPattern::Literal(KeyChord {
        key: KeyKind::Char(c),
        mods: KeyMods::NONE,
    })
}

fn lit_shift(c: char) -> ChordPattern {
    ChordPattern::Literal(KeyChord {
        key: KeyKind::Char(c),
        mods: KeyMods::SHIFT,
    })
}

/// Chord → motion bindings for `multibuffer-mode`. Lives under
/// `KeymapLayer::MajorMode(multibuffer-mode)` so the bindings
/// only fire when `multibuffer-mode` is the active major.
pub fn multibuffer_mode_layer_bindings(
    motion_ids: &MultibufferMotionIds,
) -> HashMap<BindingMode, KeymapTrie> {
    // K.1.b convention: bindings keyed by ModeId go on a
    // `MinorMode(ModeId)` layer regardless of whether the mode is
    // a major or minor — K.1.c's per-keystroke filter checks
    // `ActiveModes` membership, not the major/minor kind. The
    // bindings fire whenever `multibuffer-mode` is the active
    // major.
    let layer = KeymapLayer::MinorMode(lattice_multibuffer::MultibufferMode::mode_id());
    let mut trie = KeymapTrie::new();

    // `]e` → next excerpt start
    trie.insert(
        &[lit(']'), lit('e')],
        Arc::new(BoundCommand::from_invocation(
            CommandInvocation::of(motion_ids.next_excerpt_start.0),
            SourceLocation::builtin_file(file!(), line!()),
            layer,
        )),
    );

    // `[e` → prev excerpt start
    trie.insert(
        &[lit('['), lit('e')],
        Arc::new(BoundCommand::from_invocation(
            CommandInvocation::of(motion_ids.prev_excerpt_start.0),
            SourceLocation::builtin_file(file!(), line!()),
            layer,
        )),
    );

    // `]E` → next file boundary (uppercase E via SHIFT modifier)
    trie.insert(
        &[lit(']'), lit_shift('E')],
        Arc::new(BoundCommand::from_invocation(
            CommandInvocation::of(motion_ids.next_file_boundary.0),
            SourceLocation::builtin_file(file!(), line!()),
            layer,
        )),
    );

    // `[E` → prev file boundary
    trie.insert(
        &[lit('['), lit_shift('E')],
        Arc::new(BoundCommand::from_invocation(
            CommandInvocation::of(motion_ids.prev_file_boundary.0),
            SourceLocation::builtin_file(file!(), line!()),
            layer,
        )),
    );

    let mut modes = HashMap::new();
    modes.insert(BindingMode::Normal, trie);
    modes
}

/// M.5 (2026-06-01): register `:multibuffer-expand [n]` and
/// `:multibuffer-contract [n]` ex-commands. Both take an
/// optional non-negative integer (default 5 — Zed precedent).
/// `apply` produces `Effect::AppAction(AppEffect::MultibufferExpand
/// { delta })` where `delta` is positive for expand, negative
/// for contract. The dispatch handler routes to
/// `Editor::do_multibuffer_expand`, which looks up the active
/// view via `MultibufferRegistry` and calls
/// `expand_excerpt_at` at the active cursor's row.
///
/// No-op when invoked on a non-multibuffer active buffer (no
/// registry entry for the buffer id).
pub fn register_multibuffer_ex_commands(registry: &mut lattice_grammar::CommandRegistry) {
    use lattice_grammar::app_effect::AppEffect;
    use lattice_grammar::args::{ArgSpec, Args};
    use lattice_grammar::command::LatencyClass;
    use lattice_grammar::effect::Effect;
    use lattice_grammar::error::CommandError;
    use lattice_grammar::registry::{ExCommandSpec, SurfaceForm};

    fn parse_optional_count(s: &str, _bang: bool) -> Result<Args, CommandError> {
        let trimmed = s.trim();
        if trimmed.is_empty() {
            return Ok(Args::None);
        }
        match trimmed.parse::<u32>() {
            // Stash as the decimal string so the apply closure
            // can re-parse without re-validating; production code
            // typically passes 1-2 digit counts.
            Ok(_) => Ok(Args::String(trimmed.to_string())),
            Err(_) => Err(CommandError::BadArgs(format!(
                "expected non-negative integer, got `{trimmed}`"
            ))),
        }
    }

    fn count_from_args(args: &Args) -> i32 {
        match args {
            Args::String(s) => s.parse::<i32>().unwrap_or(5),
            _ => 5,
        }
    }

    registry.register_ex_command(
        "multibuffer-expand",
        "Expand context around the excerpt under the cursor by N rows (default 5).",
        ExCommandSpec {
            latency_class: LatencyClass::Reflex,
            accepts_bang: false,
            accepts_range: false,
            parse_args: Box::new(parse_optional_count),
            apply: Box::new(|ctx| {
                let delta = count_from_args(&ctx.args);
                Ok(Effect::AppAction(AppEffect::MultibufferExpand { delta }))
            }),
            args_schema: Vec::<ArgSpec>::new(),
            surface_form: SurfaceForm::Keyword,
        },
    );

    registry.register_ex_command(
        "multibuffer-contract",
        "Contract the excerpt under the cursor by N rows (default 5).",
        ExCommandSpec {
            latency_class: LatencyClass::Reflex,
            accepts_bang: false,
            accepts_range: false,
            parse_args: Box::new(parse_optional_count),
            apply: Box::new(|ctx| {
                let delta = -count_from_args(&ctx.args);
                Ok(Effect::AppAction(AppEffect::MultibufferExpand { delta }))
            }),
            args_schema: Vec::<ArgSpec>::new(),
            surface_form: SurfaceForm::Keyword,
        },
    );
}

/// M.6 (2026-06-01): register the `:search <query>` ex-command.
/// Stashes the query as `Args::String` and routes through
/// `AppEffect::SearchTrigger { query }` → `Action::SearchTrigger
/// { query }` → `Editor::do_search`.
///
/// Empty query is rejected with `BadArgs` — opening an empty
/// search view doesn't make sense.
#[cfg(feature = "search")]
pub fn register_search_ex_command(registry: &mut lattice_grammar::CommandRegistry) {
    use lattice_grammar::app_effect::AppEffect;
    use lattice_grammar::args::{ArgSpec, Args};
    use lattice_grammar::command::LatencyClass;
    use lattice_grammar::effect::Effect;
    use lattice_grammar::error::CommandError;
    use lattice_grammar::registry::{ExCommandSpec, SurfaceForm};

    registry.register_ex_command(
        "search",
        "Project-wide search for the literal query. Opens a multibuffer view that streams results as the scan runs.",
        ExCommandSpec {
            latency_class: LatencyClass::Reflex,
            accepts_bang: false,
            accepts_range: false,
            parse_args: Box::new(|s: &str, _bang: bool| {
                let trimmed = s.trim();
                if trimmed.is_empty() {
                    return Err(CommandError::BadArgs(
                        ":search requires a non-empty query".into(),
                    ));
                }
                Ok(Args::String(trimmed.to_string()))
            }),
            apply: Box::new(|ctx| {
                let query = match &ctx.args {
                    Args::String(s) => s.clone(),
                    _ => String::new(),
                };
                Ok(Effect::AppAction(AppEffect::SearchTrigger { query }))
            }),
            args_schema: Vec::<ArgSpec>::new(),
            surface_form: SurfaceForm::Keyword,
        },
    );
}

/// No-op stub when the `search` feature is disabled; keeps the
/// boot site unconditionally calling it.
#[cfg(not(feature = "search"))]
pub fn register_search_ex_command(_registry: &mut lattice_grammar::CommandRegistry) {}
