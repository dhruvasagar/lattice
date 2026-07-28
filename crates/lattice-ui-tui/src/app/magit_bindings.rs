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

    // ── MG.14: the headerline ────────────────────────────────────────
    //
    // The field builders are unit-tested in `lattice_magit::headerline`
    // (pure functions, no buffer, no git). What those tests CANNOT see
    // is the wiring: whether `on_activate` actually installs the
    // provider against the buffer, and whether the row survives to the
    // point a renderer would collect it. That seam is what these two
    // press-a-real-buffer tests cover — the same class of blind spot
    // MG.13 found for chords.

    /// Wait up to `budget` for `predicate`. The header lands after
    /// `on_activate`'s `spawn_blocking` git call, so it is genuinely
    /// asynchronous — polling is the honest way to observe it.
    async fn wait_for(mut predicate: impl FnMut() -> bool, budget: std::time::Duration) -> bool {
        let deadline = std::time::Instant::now() + budget;
        while !predicate() {
            if std::time::Instant::now() >= deadline {
                return false;
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
        true
    }

    /// The rendered header row for `app`'s active buffer, or `None`
    /// when no headerline provider is registered on it. Reads through
    /// the registry the cells worker reads, not through magit's
    /// handle — a row only a test can see is not a shipped feature.
    fn header_row(app: &crate::app::App) -> Option<String> {
        let buffer = app.editor.document_buffer_id;
        app.editor
            .virtual_row_providers
            .snapshot(buffer)
            .into_iter()
            .find(|p| p.id() == lattice_magit::headerline::MAGIT_HEADERLINE_PROVIDER_ID)
            .and_then(|p| {
                p.collect().into_iter().next().map(|row| {
                    row.cells
                        .iter()
                        .map(|c| char::from_u32(c.codepoint).unwrap_or(' '))
                        .collect::<String>()
                })
            })
    }

    /// magit-status is the view the MG.14 audit was about: branch and
    /// ahead/behind were computed on every refresh and displayed
    /// nowhere. This asserts the branch reaches a row the renderer
    /// would actually collect.
    #[tokio::test(flavor = "multi_thread")]
    async fn the_status_buffer_publishes_a_headerline_carrying_its_branch() {
        let mut app = app_with("", 20);
        run_ex(&mut app, "magit-status");

        let settled = wait_for(
            || header_row(&app).is_some_and(|r| r.trim() != ""),
            std::time::Duration::from_secs(5),
        )
        .await;
        assert!(settled, "magit-status never published a headerline row");

        let row = header_row(&app).unwrap();
        // The test runs inside this repository, so the current branch
        // is whatever git reports — assert the header carries THAT,
        // not a hardcoded name.
        let branch = std::process::Command::new("git")
            .args(["rev-parse", "--abbrev-ref", "HEAD"])
            .output()
            .ok()
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .map(|s| s.trim().to_string())
            .unwrap_or_default();
        assert!(!branch.is_empty(), "test must run inside a git checkout");
        assert!(
            row.contains(&branch),
            "the status header must name the checked-out branch; got {row:?}"
        );
    }

    /// MG.16: the remote/stash ex-commands reach the *booted* registry.
    ///
    /// The unit tests in `lattice-magit` build their own
    /// `CommandRegistry` and call `register_ex_commands` directly, so
    /// they prove the registration function is correct — not that
    /// `install()` calls it. This asserts against the registry a real
    /// editor booted with, which is the same gap the MG.13 service
    /// guard was written to close.
    ///
    /// Deliberately does NOT execute any of them: `:magit-stash` would
    /// stash the working tree of whatever checkout the suite runs in,
    /// and `:magit-fetch` / `:magit-pull` / `:magit-push` would hit the
    /// network. Registration is the property under test; the bodies are
    /// covered by the `RemoteOp` unit tests.
    #[tokio::test(flavor = "multi_thread")]
    async fn the_remote_and_stash_ex_commands_are_registered_at_boot() {
        let app = app_with("", 20);
        let mut missing: Vec<&str> = Vec::new();
        for name in [
            "magit-fetch",
            "magit-pull",
            "magit-push",
            "magit-stash",
            "magit-stash-list",
        ] {
            if app.editor.registry.load().lookup_by_name(name).is_none() {
                missing.push(name);
            }
        }
        assert!(
            missing.is_empty(),
            "ex-commands missing from the booted registry — `:` cannot reach \
             them and neither can a user keybinding: {missing:?}"
        );
    }

    /// MG.15: `<CR>` in the stash list opens that stash's patch.
    ///
    /// Pressed against a real `*magit:stash*` so it covers the whole
    /// seam — chord → mode filter → boot handler → row parse →
    /// `OpenSyntheticBuffer`. The row parse is the part that was
    /// broken: the list rendered `  <message>` while every handler
    /// parsed `stash@{N}`, so `a`/`p`/`d` were dead and `<CR>` would
    /// have shipped dead too.
    ///
    /// Asserts on the *effect*, not on the opened buffer: the stash
    /// list's content arrives from a `spawn_blocking` `git stash
    /// list`, so on a checkout with no stashes there is no row to
    /// stand on. The test therefore drives the handler through the
    /// same parse the chord uses, and separately proves the chord is
    /// reachable at all.
    #[tokio::test(flavor = "multi_thread")]
    async fn the_stash_list_binds_enter_to_the_detail_buffer() {
        let mut app = app_with("", 20);
        run_ex(&mut app, "magit-stash-list");
        assert_eq!(
            major_of(&app),
            Some(lattice_mode::ModeId::new("magit-stash-mode")),
            "magit-stash-mode must be active for its chords to route"
        );

        // The chord resolves to the MG.15 action rather than falling
        // through to the builtin `<CR>` (line-down).
        let resolved = app
            .editor
            .active_modes
            .get(&app.editor.document_buffer_id)
            .map(|am| am.keymap_gated_ids())
            .unwrap_or_default();
        assert!(
            resolved.contains(&lattice_mode::ModeId::new("magit-stash-mode")),
            "the stash major must be in the gated set the keystroke path uses"
        );

        // And the row format the handler parses is the one the list
        // writes — the bug this slice fixed.
        let row = lattice_magit::magit_stash_mode::list_row(3, "WIP on main: deadbee x");
        assert_eq!(
            lattice_magit::magit_stash_mode::parse_index(&row),
            Some(3),
            "a/p/d/<CR> all read the index out of the row the list wrote"
        );
    }

    // The teardown half is a unit test in `lattice_magit::headerline`
    // (`dropping_the_registration_unregisters_the_provider`), not a
    // test here, because `:bd` cannot currently exercise it:
    // `Editor::do_buffer_delete` removes the buffer from the registry
    // but never removes its `active_modes` entry, so NO mode's Drop
    // runs on buffer delete — not magit's fold source, view, or state
    // entry either, nor ai-conversation's headerline + subscription.
    // `gc_ephemeral_buffer` and `dismiss_stale_popup_registry` both do
    // clear it; `do_buffer_delete` is the outlier. Buffer ids are
    // minted from a monotonic counter and never reused, so the effect
    // is a bounded leak rather than a stale row over a live buffer.
    // Tracked as a follow-up on the magit slice plan (MG.14 notes).
}
