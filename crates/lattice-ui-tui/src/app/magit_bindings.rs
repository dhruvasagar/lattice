//! MG.13 — chord-level coverage for magit's mode keymaps.
//!
//! This module exists because of a blind spot, not a feature: until
//! MG.13 there was **no test anywhere that proved a magit chord
//! actually fires**. Handler bodies were unit-tested in
//! `lattice-magit` and the keymap tables were reviewed by eye, but the
//! seam between them — press key → translate → dispatch → mode handler
//! → effect — was uncovered. That is the seam the MG.8 dead-transient
//! bug shipped through.
//!
//! Four defects were found by writing the first test here, all
//! invisible to every prior test:
//!
//! 1. **The production defect.** magit's per-buffer modes registered
//!    their action handlers inside `on_activate`, which runs in the
//!    cascade future `ModeRegistry::spawn_cascade` spawns. For a window
//!    after the buffer opened, the chord resolved, the mode read as
//!    active, and no handler existed — the key did nothing. Fixed by
//!    moving registration to `Mode::action_handlers()` (boot-time) with
//!    per-buffer state resolved through a service; see
//!    `lattice_magit::buffer_state`.
//! 2. **The harness defect.** `test_helpers::press` passed
//!    `ActiveModes::minors()` where production passes
//!    `keymap_gated_ids()` — which includes the *major*. The harness was
//!    structurally unable to route any major-mode chord, so no test
//!    could have caught (1), nor a broken major-mode binding in oil,
//!    compilation, or ai-conversation either.
//! 3. **Shared-action collisions.** The handler registry is keyed by
//!    `CommandId` with no buffer dimension, so `gr` (five registrants),
//!    `s`/`u` (status + diff) and `q` (status + core, with *different*
//!    bodies) collapsed to last-writer-wins, and the first deactivation
//!    unregistered for all. `q` in `*magit:status*` was genuinely
//!    nondeterministic. Fixed by the `MagitView` trait — one boot
//!    handler per shared action, per-buffer body.
//! 4. **A missing state service.** `magit-log-mode`'s slot was never
//!    registered, so its handlers would have resolved `None` and
//!    silently no-op'd. Caught by the registration guard below.
//!
//! These tests press in the **same synchronous turn** the buffer opens,
//! which is exactly the window that used to be dead.

#[cfg(test)]
mod tests {
    use crate::app::Action;
    use crate::app::test_helpers::*;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    fn key(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)
    }

    /// Run `:<cmd>` through the real command-line path — the same
    /// `EnterCommandLine` → type → `CommandLineSubmit` sequence the
    /// cmdline tests use.
    fn run_ex(app: &mut crate::app::App, cmd: &str) {
        app.apply(Action::EnterCommandLine);
        press_chars(app, cmd);
        app.apply(Action::CommandLineSubmit);
    }

    fn major_of(app: &crate::app::App) -> Option<lattice_mode::ModeId> {
        app.editor
            .active_modes
            .get(&app.editor.document_buffer_id)
            .and_then(|am| am.major())
    }

    /// The MG.13 regression guard.
    ///
    /// `c` in a magit-branch buffer opens the branch-create wizard's
    /// picker. It is the right chord to assert on because its handler
    /// needs only the mode's *state presence*, not the buffer's
    /// content — content population is genuinely async (a `git branch`
    /// call on `spawn_blocking`), so a chord that reads the branch
    /// under the cursor legitimately finds nothing this early.
    ///
    /// The assertion is on `pending_picker_init`, not on `picker`: an
    /// async picker source is seated only once its candidates land, so
    /// `picker` is still `None` in this turn. `pending_picker_init` is
    /// what the effect sets synchronously — asserting on it keeps the
    /// test measuring "did the handler run", not "how fast does git
    /// answer".
    #[tokio::test(flavor = "multi_thread")]
    async fn branch_chord_fires_in_the_same_turn_the_buffer_opens() {
        let mut app = app_with("", 20);
        run_ex(&mut app, "magit-branch");

        assert_eq!(
            major_of(&app),
            Some(lattice_mode::ModeId::new("magit-branch-mode")),
            "magit-branch-mode must be active synchronously — activate_major \
             sets it in its sync prefix, before the cascade is spawned"
        );

        press(&mut app, key('c'));

        let pending = app
            .editor
            .pending_picker_init
            .as_ref()
            .map(|p| p.source_id.clone());
        assert_eq!(
            pending.as_deref(),
            Some("magit-branch-pick-base"),
            "`c` must reach its handler in the same turn the buffer opened; \
             before MG.13 no handler was registered yet and the keypress was \
             silently swallowed"
        );
    }

    /// The negative half, so the test above cannot pass vacuously: the
    /// same chord in an ordinary buffer must NOT open the picker.
    /// Handlers are registered globally at boot now, so scoping rests
    /// entirely on K.1.c's per-keystroke mode filter plus the handler's
    /// own `state(ctx)?` guard — if either regressed, magit's `c` would
    /// shadow the `c` (change) operator everywhere.
    #[tokio::test(flavor = "multi_thread")]
    async fn the_same_chord_does_nothing_in_an_ordinary_buffer() {
        let mut app = app_with("hello\nworld\n", 20);
        press(&mut app, key('c'));
        assert!(
            app.editor.pending_picker_init.is_none(),
            "magit's `c` must not fire outside a magit-branch buffer — \
             boot-registered handlers are scoped by the mode filter, not by \
             whether they happen to be registered at all"
        );
    }

    /// Guards the harness fix itself. `press` must pass the same
    /// mode-id set production passes (`keymap_gated_ids`, major
    /// included). If it regresses to `minors()`, this fails — and with
    /// it every future major-mode chord test, rather than those tests
    /// silently becoming unable to reach their binding.
    #[tokio::test(flavor = "multi_thread")]
    async fn the_harness_routes_major_mode_chords() {
        let mut app = app_with("", 20);
        run_ex(&mut app, "magit-branch");
        let gated = app
            .editor
            .active_modes
            .get(&app.editor.document_buffer_id)
            .map(|am| am.keymap_gated_ids())
            .unwrap_or_default();
        assert!(
            gated.contains(&lattice_mode::ModeId::new("magit-branch-mode")),
            "the major must be in the gated set the keystroke path uses; \
             passing only minors makes every major-mode chord unreachable"
        );
    }

    /// Every migrated mode must have its `BufferStates` service
    /// registered at boot. A missing registration is silent: the mode's
    /// handlers resolve `state(ctx)` to `None` and no-op, which from
    /// the user's side is indistinguishable from the dead-chord bug
    /// MG.13 removed. This caught a real omission (`magit-log-mode`).
    #[tokio::test(flavor = "multi_thread")]
    async fn every_migrated_mode_has_its_buffer_state_service() {
        let app = app_with("", 20);
        let s = &app.editor.services;
        let mut missing: Vec<&str> = Vec::new();
        macro_rules! require {
            ($ty:ty, $label:literal) => {
                if s.get::<$ty>().is_none() {
                    missing.push($label);
                }
            };
        }
        use lattice_magit::*;
        require!(magit_branch_mode::BranchStatesHandle, "magit-branch-mode");
        require!(magit_stash_mode::StashStatesHandle, "magit-stash-mode");
        require!(
            magit_revision_mode::RevisionStatesHandle,
            "magit-revision-mode"
        );
        require!(magit_blame_mode::BlameStatesHandle, "magit-blame-mode");
        require!(magit_commit_mode::CommitStatesHandle, "magit-commit-mode");
        require!(magit_rebase_mode::RebaseStatesHandle, "magit-rebase-mode");
        require!(magit_log_mode::LogStatesHandle, "magit-log-mode");
        require!(magit_diff_mode::DiffStatesHandle, "magit-diff-mode");
        require!(actions::StatusStatesHandle, "magit-status-mode");
        require!(buffer_state::MagitViewsHandle, "MagitViews (shared `gr`)");
        assert!(
            missing.is_empty(),
            "unregistered per-buffer state services — their chords would \
             silently no-op: {missing:?}"
        );
    }
}
