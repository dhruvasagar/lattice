//! `set-option-in-buffer` — a per-buffer option override from a guest.
//!
//! The capability that was missing for mode-scoped user config. WIT's
//! `set-option` is the `:set` path and writes the GLOBAL layer, so a handler
//! wanting "wrap in org buffers" could only wrap everything, with nothing to
//! unwrap on leaving. This writes the buffer-local layer instead, which is the
//! scope the question actually has.
//!
//! Driven by publishing the host-internal request the plugin host's
//! `set-option-in-buffer` emits, rather than through WASM: the seam under test
//! is the Editor's drain and the resolver layering. `lattice-plugin-host`
//! covers the guest half.

#![allow(clippy::unwrap_used, clippy::panic)]

use lattice_config::core_options::AutoWrapOption;
use lattice_core::{AutoWrap, Document as CoreDocument};
use lattice_host::editor::Editor;

fn autowrap_in(editor: &Editor, buffer: lattice_core::BufferId) -> Option<AutoWrap> {
    editor
        .resolved_options
        .get(&buffer)
        .and_then(|r| r.get::<AutoWrapOption>())
        .map(|v| *v)
}

fn request(editor: &Editor, buffer: lattice_core::BufferId, option: &str) {
    editor
        .event_bus
        .publish(lattice_protocol::Event::BufferOptionOverrideRequested {
            buffer: lattice_protocol::ids::BufferId::new(buffer.0 as u64),
            option: option.to_string(),
        });
}

/// The headline: the override lands on the named buffer and nowhere else.
#[test]
fn an_override_applies_to_its_buffer_and_not_to_others() {
    let mut editor = Editor::boot(CoreDocument::from_text("prose\n"));
    let target = editor.document_buffer_id;
    editor.recompute_options_for_buffer(target);

    let before = autowrap_in(&editor, target);
    assert_ne!(
        before,
        Some(AutoWrap::All),
        "precondition: the default is not already the value under test — \
         otherwise this passes without the override doing anything"
    );

    request(&editor, target, "autowrap=all");
    let _ = editor.drain_buffer_option_overrides();

    assert_eq!(
        autowrap_in(&editor, target),
        Some(AutoWrap::All),
        "the buffer the request named resolves to the override"
    );
    assert_eq!(
        editor
            .config
            .get_typed::<AutoWrapOption>()
            .map(|v| *v)
            .unwrap(),
        AutoWrap::Comments,
        "and the GLOBAL layer is untouched — which is the whole difference \
         from `set-option`, and the reason a mode-scoped override needs this \
         call rather than that one"
    );
}

/// An unknown option is refused without touching anything, and without
/// echoing: nothing the user did provoked it, so an error bar over a buffer
/// they just opened would blame them for their config's bug at the least
/// useful moment. The log names it instead.
#[test]
fn an_unknown_option_is_refused_quietly() {
    let mut editor = Editor::boot(CoreDocument::from_text("prose\n"));
    let target = editor.document_buffer_id;
    editor.recompute_options_for_buffer(target);
    let before = autowrap_in(&editor, target);

    request(&editor, target, "no-such-option-at-all=true");
    let _ = editor.drain_buffer_option_overrides();

    assert_eq!(autowrap_in(&editor, target), before, "nothing changed");
    // (The "quietly" half is the `tracing::warn!` in the drain — there is no
    // echo field to assert against, and asserting on a log line would pin the
    // wording rather than the behaviour. What IS assertable is that the
    // refusal changed nothing, which is the part that matters.)
}

/// An invalid VALUE for a known option is refused the same way — the parse is
/// `:setlocal`'s, so a guest can express nothing `:setlocal` could not.
#[test]
fn an_invalid_value_is_refused_the_same_way() {
    let mut editor = Editor::boot(CoreDocument::from_text("prose\n"));
    let target = editor.document_buffer_id;
    editor.recompute_options_for_buffer(target);
    let before = autowrap_in(&editor, target);

    request(&editor, target, "autowrap=not-a-real-mode");
    let _ = editor.drain_buffer_option_overrides();

    assert_eq!(autowrap_in(&editor, target), before);
}

/// A request naming a buffer that closed before the drain ran is dropped.
///
/// Ordinary rather than exceptional: the request crosses a tick, and a buffer
/// can close inside one. The assertion is that it does not panic and does not
/// write the override somewhere else.
#[test]
fn a_request_for_a_closed_buffer_is_dropped() {
    let mut editor = Editor::boot(CoreDocument::from_text("prose\n"));
    let live = editor.document_buffer_id;
    editor.recompute_options_for_buffer(live);
    let before = autowrap_in(&editor, live);

    // An id the registry never minted.
    request(&editor, lattice_core::BufferId(9999), "autowrap=all");
    let _ = editor.drain_buffer_option_overrides();

    assert_eq!(
        autowrap_in(&editor, live),
        before,
        "the live buffer must not inherit an override addressed to a dead one"
    );
}

/// **A user override beats a mode's, which is the ordering that makes
/// mode-scoped user config mean anything.**
///
/// If a mode's contribution won, setting `autowrap` for org buffers from
/// `init.rs` would be silently pointless the moment org declared the same
/// option — and org already declares `foldmethod` and `foldlevel`, so the
/// collision is one line away, not hypothetical.
///
/// It holds because the resolver ranks buffer-local ABOVE mode contributions
/// (`recompute_options_for_buffer`: "Layer 1: modal-state, Layer 2:
/// buffer-local, Layers 3+: modes"), not because of anything this drain does.
/// Pinned here because the drain is what makes the guarantee reachable from a
/// guest, and a layer reorder would break this feature while every resolver
/// test still passed.
#[test]
fn a_user_override_beats_a_modes_contribution() {
    use lattice_config::{OptionOrigin, OptionOverride, OptionOverrideSet, OverridePriority};

    let mode_set: OptionOverrideSet = std::iter::once(OptionOverride {
        option_type_id: std::any::TypeId::of::<AutoWrapOption>(),
        value: std::sync::Arc::new(AutoWrap::Comments),
        priority: OverridePriority::Normal,
    })
    .collect();
    let user_set: OptionOverrideSet = std::iter::once(OptionOverride {
        option_type_id: std::any::TypeId::of::<AutoWrapOption>(),
        value: std::sync::Arc::new(AutoWrap::All),
        priority: OverridePriority::Normal,
    })
    .collect();

    let mut resolved = lattice_config::ResolvedOptions::new();
    // Highest-authority layer first, exactly as the Editor pushes them.
    lattice_config::Resolver.resolve_into_with_origins(
        vec![
            (&user_set, OptionOrigin::BufferLocal),
            (
                &mode_set,
                OptionOrigin::ModeContribution {
                    mode_id: "org-mode".to_string(),
                },
            ),
        ],
        &mut resolved,
    );

    assert_eq!(
        resolved.get::<AutoWrapOption>().map(|v| *v),
        Some(AutoWrap::All),
        "the user's buffer-local value must win over the mode's"
    );
}

/// **…including against a mode that declared `High`, which used to be the one
/// case it lost.**
///
/// `Resolver::candidate_better` makes `OverridePriority::High` win *absolute*
/// — ahead of layer rank, not within it — so a mode declaring it was
/// unoverridable from a user's config. That is fine for the case the rule was
/// written for (`read-only-mode` declaring `writable=false` so no other mode
/// can quietly flip it) and wrong as a general rule, because ANY mode may
/// declare `High` and there is no way for a user to know which did.
///
/// The seam already claimed the behaviour this now has. The mode-option doc
/// says a contribution is "a LAYER, not a write … a `:setlocal` in that buffer
/// still wins over it, which is the right way round — the user gets the last
/// word in their own buffer." It was true only against `Normal`.
///
/// Mode-versus-mode is untouched: `read-only-mode` still beats every other
/// mode regardless of activation order, which is the threat model `High`
/// exists for. What changed is that the person who owns the editor can say
/// otherwise about one buffer.
#[test]
fn a_user_override_beats_even_a_high_priority_mode() {
    use lattice_config::{OptionOrigin, OptionOverride, OptionOverrideSet, OverridePriority};

    let mode_set: OptionOverrideSet = std::iter::once(OptionOverride {
        option_type_id: std::any::TypeId::of::<AutoWrapOption>(),
        value: std::sync::Arc::new(AutoWrap::Comments),
        priority: OverridePriority::High,
    })
    .collect();
    let user_set: OptionOverrideSet = std::iter::once(OptionOverride {
        option_type_id: std::any::TypeId::of::<AutoWrapOption>(),
        value: std::sync::Arc::new(AutoWrap::All),
        priority: OverridePriority::Normal,
    })
    .collect();

    let mut resolved = lattice_config::ResolvedOptions::new();
    lattice_config::Resolver.resolve_into_with_origins(
        vec![
            (&user_set, OptionOrigin::BufferLocal),
            (
                &mode_set,
                OptionOrigin::ModeContribution {
                    mode_id: "some-mode".to_string(),
                },
            ),
        ],
        &mut resolved,
    );

    assert_eq!(
        resolved.get::<AutoWrapOption>().map(|v| *v),
        Some(AutoWrap::All),
        "a user's per-buffer value is the last word — a mode that declared \
         `High` must not be able to make its option unoverridable"
    );
}

/// The other half of that rule, and the reason it keys on `BufferLocal` rather
/// than on "not a mode": GLOBAL config does NOT beat a mode.
///
/// That is the seam working, not a conflict — org setting `foldmethod=syntax`
/// over a global `foldmethod=indent` is exactly what mode options are for. A
/// buffer-local set is a different act: it names one buffer, so there is no
/// reading under which the mode is the more specific answer.
#[test]
fn global_config_still_loses_to_a_mode_contribution() {
    use lattice_config::{OptionOrigin, OptionOverride, OptionOverrideSet, OverridePriority};

    let global_set: OptionOverrideSet = std::iter::once(OptionOverride {
        option_type_id: std::any::TypeId::of::<AutoWrapOption>(),
        value: std::sync::Arc::new(AutoWrap::All),
        priority: OverridePriority::Normal,
    })
    .collect();
    let mode_set: OptionOverrideSet = std::iter::once(OptionOverride {
        option_type_id: std::any::TypeId::of::<AutoWrapOption>(),
        value: std::sync::Arc::new(AutoWrap::Comments),
        priority: OverridePriority::Normal,
    })
    .collect();

    let mut resolved = lattice_config::ResolvedOptions::new();
    // The mode layer is pushed FIRST here — higher authority — exactly as the
    // Editor orders them relative to the global bootstrap.
    lattice_config::Resolver.resolve_into_with_origins(
        vec![
            (
                &mode_set,
                OptionOrigin::ModeContribution {
                    mode_id: "org-mode".to_string(),
                },
            ),
            (&global_set, OptionOrigin::GlobalConfig),
        ],
        &mut resolved,
    );

    assert_eq!(
        resolved.get::<AutoWrapOption>().map(|v| *v),
        Some(AutoWrap::Comments),
        "a mode refines the global baseline for its own buffers — if global \
         config outranked it, `mode-declaration.options` would do nothing for \
         any option the user had ever set"
    );
}
