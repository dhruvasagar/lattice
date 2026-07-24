//! Ex command line (`:`) state machine -- the App-side
//! cmdline-completion + history navigation. Driven by
//! `Action::CommandLine{Complete,CompletePrev,
//! AcceptCompletion,DescribeUnderCursor,HistoryStep}`.
//!
//! Methods that live here:
//! - `do_command_line_describe_under_cursor` (`<C-h>`
//!   resolves the word under cursor to its
//!   `:describe-command` view).
//! - `do_command_line_complete_or_advance` (`<Tab>`),
//!   `do_command_line_complete_prev` (`<S-Tab>`),
//!   `do_command_line_accept_completion`.
//! - `do_command_history_step` (`<Up>` / `<Down>` walks
//!   `:` history with snapshot + restore semantics).
//! - `try_resolve_missing_arg_prompt` (the cmdline-submit
//!   missing-required-arg detector) plus its
//!   `MissingArgPrompt` return shape.
//! - `chord_capture_active` (the input-layer gate that
//!   tells `translate_command_chord_capture` whether the
//!   current cmdline cursor is on an `ArgKind::Chord` slot).
//! - `compute_completion_state` (slot-detect + run the
//!   completion pipeline + alias-rewrite command
//!   candidates) and `refresh_completion_popup` (re-run
//!   on `CommandLine{Append,Backspace,DeleteWordBackward}`
//!   while the popup is open). Plus the `CompletionComputeError`
//!   error enum and the `prefer_aliases_for_command_candidates`
//!   / `subsequence_match_ranges` helpers used only here.
//!
//! What does NOT live here yet: the ex-command bodies
//! themselves (`do_edit`, `do_write`, `do_quit`,
//! `do_global`, etc.). Those each belong to their own
//! feature group (`do_edit` -> lifecycle, `do_write` ->
//! lifecycle / oil, `do_global` -> search, ...) and migrate
//! with their respective slices.
//!
//! Also stays in `app.rs` for now: `execute_ex_line` (the
//! top-level submit dispatcher). It moves with a later
//! cmdline.dispatch slice.

use lattice_grammar::ModalState;

use super::{App, CompletionState, EchoLevel};

/// Result of resolving a missing-arg prompt (DESIGN.md §B.1).
/// Returned by [`App::try_resolve_missing_arg_prompt`] when the
/// user submits a bare command with a required first arg empty.
pub(super) struct MissingArgPrompt {
    /// New value for `command_line`. Already contains the command
    /// word + bang + a trailing space; the cursor lands at end-of-
    /// line, in the first arg slot.
    pub(super) prefill: String,
    /// Kind of the first arg. Drives whether the App arms the
    /// chord-capture overlay (kind == Chord) or just leaves the
    /// cmdline open for typed input.
    pub(super) kind: lattice_grammar::ArgKind,
    /// Prompt text for the echo area, taken from the schema's
    /// `prompt` field (or `"<name>:"` when empty).
    pub(super) prompt: String,
}

impl App {
    /// Hybrid `<C-h>` resolution (DESIGN.md §5.11.3 Q11). Walk the
    /// `:` line up to the cursor (v1: cursor is at end), find the
    /// "word" the user is hovering on, and:
    ///
    /// 1. If the word resolves to a registered command (via alias
    ///    expansion), describe THAT -- the user is asking about the
    ///    command they're typing.
    /// 2. Else, if we can identify the slot as an arg of a known
    ///    command, describe the parent command scrolled to
    ///    `arg:<name>`.
    /// 3. Else, no-op + status message.
    pub(super) fn do_command_line_describe_under_cursor(&mut self) {
        if !matches!(self.ad().modal, ModalState::Command) {
            return;
        }
        let line = self.command_line();
        let cursor = line.len();
        let alias_resolver = |short: &str| {
            crate::excommand::aliases()
                .get(short)
                .map(|s| (*s).to_string())
        };
        // Slice 3c.final.E.5e: registry accessed through the
        // Arc-cloning helper; locally-bound so the `&reg` borrow
        // stays valid for the parser + lookup calls below.
        let reg = self.registry();
        let slot = lattice_completion::current_slot(&line, cursor, &reg, &alias_resolver);

        let word = slot.prefix();
        let canonical = if word.is_empty() {
            None
        } else {
            alias_resolver(word).or_else(|| reg.id_by_name(word).and(Some(word.to_string())))
        };

        if let Some(name) = canonical
            && reg.id_by_name(&name).is_some()
        {
            self.do_describe_command(&name, None);
            return;
        }

        match &slot {
            lattice_completion::CommandLineSlot::Arg {
                command_name,
                arg_spec,
                ..
            } => {
                let anchor = format!("arg:{}", arg_spec.name);
                self.do_describe_command(command_name, Some(&anchor));
            }
            lattice_completion::CommandLineSlot::CommandName { prefix, .. } => {
                if prefix.is_empty() {
                    self.set_message(
                        EchoLevel::Info,
                        "type a command name then C-h for its help".to_string(),
                    );
                } else {
                    self.set_message(EchoLevel::Error, format!("no command named `{prefix}`"));
                }
            }
            _ => {
                self.set_message(
                    EchoLevel::Info,
                    "no command-line context for `C-h`".to_string(),
                );
            }
        }
    }

    /// `<Tab>` opens the completion popup or advances within an
    /// open one. Slot detection drives generator selection; the
    /// pipeline runs through the registered matcher / ranker /
    /// annotators.
    pub(super) fn do_command_line_complete_or_advance(&mut self) {
        if !matches!(self.ad().modal, ModalState::Command) {
            return;
        }
        // Slice 3c.final.E.5i: advance the open popup's selection
        // through `mutate_editor_with`; return `true` from the
        // closure when the popup was open (outer body short-
        // circuits) or `false` when there's no popup yet (the
        // open-completion-popup path runs).
        let advanced = self.mutate_editor_with(|e| {
            if let Some(state) = e.completion_state.as_mut() {
                if !state.candidates.is_empty() {
                    state.selected = (state.selected + 1) % state.candidates.len();
                }
                true
            } else {
                false
            }
        });
        if advanced {
            return;
        }
        self.open_completion_popup();
    }

    // 5.5.G.15: `do_command_line_complete_prev` /
    // `do_command_line_accept_completion` migrated to
    // [`lattice_host::dispatch::Editor`].

    // 5.5.G.13: body migrated to
    // [`lattice_host::dispatch::Editor::do_command_history_step`].

    /// On `Action::CommandLineSubmit`, decide whether the line is
    /// an empty-arg invocation of a command whose first required
    /// arg is `Chord`. If so, return the prefill string for the
    /// cmdline (`<command-word> ` -- with trailing space) so the
    /// caller can transition into a chord-capture prompt.
    /// `None` means submit normally.
    /// Generalized missing-arg detection (DESIGN.md §B.1).
    ///
    /// When the user submits a bare command with a required first
    /// arg empty -- e.g. `:write<CR>` (path required), `:edit<CR>`
    /// (path required), `:describe-command<CR>` (name required) --
    /// resolve the spec, look up the schema's first required arg,
    /// and return enough info for the App to prefill the cmdline
    /// + show a prompt.
    ///
    /// Returns `None` when:
    /// - The cmdline is empty.
    /// - The user already supplied an arg (parser handles it).
    /// - The command is unknown (parser errors anyway).
    /// - There's no first arg or it's not Required.
    /// - The command's args use the delimiter form (`:s/.../.../`).
    pub(super) fn try_resolve_missing_arg_prompt(&self) -> Option<MissingArgPrompt> {
        let line_owned = self.command_line();
        let line = line_owned.trim();
        if line.is_empty() {
            return None;
        }
        // Split off the command word + bang the same way
        // `excommand::parse_invocation` does. We don't go through
        // the full parser because we explicitly want the
        // `args == empty` case here (the parser would error).
        let (raw_cmd, rest) = match line.find(char::is_whitespace) {
            Some(i) => (&line[..i], line[i..].trim()),
            None => (line, ""),
        };
        if !rest.is_empty() {
            // User supplied an arg -- normal submit handles it.
            return None;
        }
        let cmd = raw_cmd.strip_suffix('!').unwrap_or(raw_cmd);
        // Slice 3c.final.E.5e: registry through the Arc helper;
        // bound locally so the `&reg`-equivalent lookups below stay
        // alive across the lifetime of the closure-passed callable.
        let reg = self.registry();
        let canonical = reg.id_by_name(cmd).or_else(|| {
            crate::excommand::aliases()
                .get(cmd)
                .copied()
                .and_then(|c| reg.id_by_name(c))
        })?;
        let spec = reg.ex_command_spec(canonical)?;
        // Delimiter-form commands (`:s`, `:g`, `:v`) don't go
        // through the keyword arg-prompt path -- their syntax is
        // its own UX.
        if matches!(
            spec.surface_form,
            lattice_grammar::SurfaceForm::Delimiter { .. }
        ) {
            return None;
        }
        let first = spec.args_schema.first()?;
        if !matches!(first.default, lattice_grammar::ArgDefault::Required) {
            // Non-required arg has a fallback; let the parser take
            // the default path.
            return None;
        }
        let prompt = if first.prompt.is_empty() {
            format!("{}:", first.name)
        } else {
            first.prompt.to_string()
        };
        Some(MissingArgPrompt {
            // Preserve the user's spelling (alias vs canonical) plus
            // any bang they typed; append a trailing space so the
            // cursor lands in the arg slot.
            prefill: format!("{raw_cmd} "),
            kind: first.kind,
            prompt,
        })
    }

    /// True when the cmdline cursor is on an `ArgKind::Chord` arg
    /// slot. Drives the input layer's chord-capture overlay
    /// (`translate_command_chord_capture`). v1: `:describe-key`'s
    /// `chord` arg is the only `Chord`-kinded arg in the registry;
    /// when `:map` / `:nnoremap` land they reuse this gate.
    pub fn chord_capture_active(&self) -> bool {
        // Slice 3c.final.E.5i: modal read now routes through the
        // published `ad().modal` mirror. Tests that mutate
        // `editor.modal` directly are updated to call
        // `publish_render_state()` after the mutation so the
        // mirror reflects the change.
        if !matches!(self.ad().modal, ModalState::Command) {
            return false;
        }
        let line = self.command_line();
        let line = line.as_str();
        let alias_resolver = |short: &str| {
            crate::excommand::aliases()
                .get(short)
                .map(|s| (*s).to_string())
        };
        // Slice 3c.final.E.5e: registry through the Arc helper.
        let reg = self.registry();
        let slot = lattice_completion::current_slot(line, line.len(), &reg, &alias_resolver);
        matches!(
            &slot,
            lattice_completion::CommandLineSlot::Arg { arg_spec, .. }
                if arg_spec.kind == lattice_grammar::ArgKind::Chord
        )
    }

    /// Build the pipeline for the current slot and run it. Caches
    /// results into `completion_state`.
    ///
    /// When `completion.auto_insert_single` is on (the default) and
    /// the pipeline returns exactly one candidate, the popup is
    /// skipped and that candidate is applied to the command line
    /// directly -- same effect as `<Tab><CR>` but without the
    /// confirm keystroke for an unambiguous match. The popup-open
    /// boundary is the only fire point; narrowing an already-open
    /// popup to one candidate while typing does not auto-insert.
    /// 5.5.G.23.cmdline: body migrated to
    /// [`lattice_host::dispatch::Editor::open_completion_popup`].
    /// Retained as a 1-line delegate while App callers (cmdline arms
    /// + complete-or-advance helper) still reach for it directly.
    pub(super) fn open_completion_popup(&mut self) {
        // Slice 3c.final.E.3: route through `mutate_editor`.
        self.mutate_editor(|e| e.open_completion_popup());
    }

    /// 5.5.G.23.cmdline: body migrated to
    /// [`lattice_host::dispatch::Editor::refresh_completion_popup`].
    pub(super) fn refresh_completion_popup(&mut self) {
        // Slice 3c.final.E.3: route through `mutate_editor`.
        self.mutate_editor(|e| e.refresh_completion_popup());
    }

    /// Slot-detect, build the pipeline, run it, and host-rewrite
    /// command candidates to user-facing aliases. Pure -- no
    /// `set_message` side effects, so both the open and the refresh
    /// path can share it. Errors carry enough info for the open path
    /// to surface them via echo.
    /// 5.5.G.23.cmdline: body migrated to
    /// [`lattice_host::dispatch::Editor::compute_completion_state`].
    /// Kept as a delegate for callers that compose this with their
    /// own dispatch (today: `open_completion_popup` /
    /// `refresh_completion_popup` — both also host-resident).
    pub(super) fn compute_completion_state(
        &self,
    ) -> Result<CompletionState, CompletionComputeError> {
        self.read_editor(move |e| e.compute_completion_state())
    }
}

// 5.5.G.23.cmdline: `CompletionComputeError` enum + `echo()` impl
// migrated to [`lattice_host::dispatch::CompletionComputeError`].
// Re-exported here so App callers + in-file tests continue working
// unchanged.
pub(super) use lattice_host::dispatch::CompletionComputeError;

// 5.5.G.23.cmdline: `prefer_aliases_for_command_candidates` migrated
// to `lattice_host::dispatch`. Test-only re-export so the in-file
// test module (`prefer_aliases_*` tests) keeps calling it unchanged
// without leaking the symbol into the non-test build.
#[cfg(test)]
use lattice_host::dispatch::prefer_aliases_for_command_candidates;

// 5.5.G.23.cmdline: `subsequence_match_ranges` retired (zero
// remaining App callers; lives as a private host fn alongside
// `prefer_aliases_for_command_candidates`).

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::test_helpers::{
        app_in_command_mode, app_with, invoke_motion, press, press_chars, submit_ex,
        unique_tempdir,
    };
    use crate::app::*;

    // ── MB.1 (rich minibuffer, tier 1): the `:` line is a real
    //    buffer edited through the universal Insert dispatcher ──

    /// The `:` line is buffer-backed: `:` focuses the synthetic
    /// `*command-line*` buffer, typing flows through the universal Insert
    /// dispatcher, and — the whole point of MB.1 — the cursor moves so
    /// edits land mid-line, not only appended at the end.
    #[test]
    fn mb1_command_line_supports_midline_editing() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let mut a = app_with("hello\n", 10);
        a.apply(Action::EnterCommandLine);
        assert!(
            a.editor.command_line_active(),
            "`:` must focus the *command-line* buffer"
        );
        press_chars(&mut a, "eabc");
        assert_eq!(a.editor.command_line(), "eabc");
        // Walk the cursor back two with the `<Left>` arrow and insert —
        // impossible with the old `String` + append path.
        press(&mut a, KeyEvent::new(KeyCode::Left, KeyModifiers::NONE));
        press(&mut a, KeyEvent::new(KeyCode::Left, KeyModifiers::NONE));
        press_chars(&mut a, "X");
        assert_eq!(
            a.editor.command_line(),
            "eaXbc",
            "insert must land at the cursor, not the end of the line"
        );
        // `<C-b>` (readline) moves the caret the same way the arrow does:
        // from after `X` to before it, so `Y` lands between `a` and `X`.
        press(&mut a, KeyEvent::new(KeyCode::Char('b'), KeyModifiers::CONTROL));
        press_chars(&mut a, "Y");
        assert_eq!(a.editor.command_line(), "eaYXbc");
    }

    /// `<Esc>` / `CommandLineCancel` closes the `:` line and restores the
    /// prior editing buffer (focus-swap unwind), with no command run.
    #[test]
    fn mb1_command_line_cancel_restores_prior_buffer() {
        let mut a = app_with("hello\n", 10);
        let before = a.editor.document_buffer_id;
        a.apply(Action::EnterCommandLine);
        press_chars(&mut a, "e foo");
        assert!(a.editor.command_line_active());
        a.apply(Action::CommandLineCancel);
        assert!(
            !a.editor.command_line_active(),
            "cancel must close the `:` line"
        );
        assert_eq!(
            a.editor.document_buffer_id, before,
            "cancel must restore the prior editing buffer"
        );
    }

    /// History walk seeds the `:` line from `command_history` and the
    /// seeded text is itself buffer-backed (editable + submittable).
    #[test]
    fn mb1_command_line_history_walk_seeds_the_buffer() {
        let mut a = app_with("hello\n", 10);
        a.editor.command_history.push("edit foo".to_string());
        a.apply(Action::EnterCommandLine);
        a.apply(Action::CommandLineHistoryPrev);
        assert_eq!(
            a.editor.command_line(),
            "edit foo",
            "`<C-p>` seeds the `:` buffer from history"
        );
    }

    // ── MB.2 (rich minibuffer, tier 2): `<C-x><C-e>` in-place expand ──

    /// `<C-x><C-e>` toggles the `:` line between the one-row readline line
    /// (`ModalState::Command`) and the expanded full-modal band
    /// (`ModalState::Insert`, full grammar). Collapse returns to the
    /// one-row line for review; the edited text survives both ways.
    #[test]
    fn mb2_toggle_expand_flips_state_and_modal_preserving_text() {
        let mut a = app_with("hello\n", 10);
        a.apply(Action::EnterCommandLine);
        press_chars(&mut a, "e foo");
        assert!(!a.editor.command_line_expanded());
        assert!(matches!(a.editor.modal, ModalState::Command));

        a.apply(Action::CommandLineToggleExpand);
        assert!(a.editor.command_line_expanded(), "expand sets tier-2");
        assert!(
            matches!(a.editor.modal, ModalState::Insert),
            "expand drops into full-modal Insert on the band"
        );
        assert_eq!(a.editor.command_line(), "e foo", "text survives expand");

        a.apply(Action::CommandLineToggleExpand);
        assert!(!a.editor.command_line_expanded(), "collapse clears tier-2");
        assert!(
            matches!(a.editor.modal, ModalState::Command),
            "collapse returns to the readline line for review"
        );
        assert_eq!(a.editor.command_line(), "e foo", "text survives collapse");
    }

    /// `:` is a no-op while the command line is already open — it must not
    /// open a nested command line (the guard the expanded tier-2 band's
    /// Normal mode relies on).
    #[test]
    fn mb2_colon_is_noop_while_command_line_open() {
        let mut a = app_with("hello\n", 10);
        a.apply(Action::EnterCommandLine);
        press_chars(&mut a, "abc");
        let buf = a.editor.document_buffer_id;
        a.apply(Action::EnterCommandLine);
        assert!(a.editor.command_line_active());
        assert_eq!(a.editor.command_line(), "abc", "text preserved, not reset");
        assert_eq!(
            a.editor.document_buffer_id, buf,
            "same `*command-line*` buffer, not a nested one"
        );
    }

    /// In the expanded band, `<CR>` inserts a newline (multi-line editing)
    /// and does NOT submit — submit happens only from the collapsed line.
    #[test]
    fn mb2_expanded_cr_inserts_newline_not_submit() {
        let mut a = app_with("hello\n", 10);
        a.apply(Action::EnterCommandLine);
        press_chars(&mut a, "e foo");
        a.apply(Action::CommandLineToggleExpand); // expand → Insert
        a.apply(Action::CommandLineSubmit); // `<CR>` in the band
        press_chars(&mut a, "bar");
        assert!(
            a.editor.command_line_active(),
            "expanded `<CR>` must not submit / close the command line"
        );
        assert!(
            a.editor.document.text().starts_with("e foo\nbar"),
            "expanded `<CR>` inserts a newline (multi-line): {:?}",
            a.editor.document.text()
        );
        assert_eq!(
            a.editor.command_line(),
            "e foo",
            "`command_line()` reads the first line"
        );
    }

    /// In the expanded band, `<Esc>` from Insert drops to Normal (full
    /// modal) rather than cancelling the command line.
    #[test]
    fn mb2_expanded_esc_from_insert_goes_to_normal() {
        let mut a = app_with("hello\n", 10);
        a.apply(Action::EnterCommandLine);
        a.apply(Action::CommandLineToggleExpand);
        assert!(matches!(a.editor.modal, ModalState::Insert));
        a.apply(Action::CommandLineCancel); // `<Esc>` in the band
        assert!(
            a.editor.command_line_active(),
            "`<Esc>` in the band must not cancel the command line"
        );
        assert!(
            matches!(a.editor.modal, ModalState::Normal),
            "`<Esc>` in the expanded band drops to Normal for full modal"
        );
    }

    /// The renderer's band data path: `command_line_full_text()` carries
    /// every line (what the band draws) while `command_line()` stays the
    /// first-line one-row view.
    #[test]
    fn mb2_expanded_full_text_exposes_multiline_for_the_band() {
        let mut a = app_with("hello\n", 10);
        a.apply(Action::EnterCommandLine);
        press_chars(&mut a, "e foo");
        a.apply(Action::CommandLineToggleExpand);
        a.apply(Action::CommandLineSubmit); // newline in the band
        press_chars(&mut a, "bar");
        assert!(a.editor.command_line_expanded());
        assert_eq!(a.editor.command_line(), "e foo");
        assert!(
            a.editor
                .command_line_full_text()
                .starts_with("e foo\nbar"),
            "band reads the full multi-line text: {:?}",
            a.editor.command_line_full_text()
        );
    }

    /// MB.2e: the `command-line.expand-height` option drives the
    /// expanded band's height. The renderer reads it via
    /// `command_line_expand_height()` and resolves it against the live
    /// frame height (`ExpandHeight::rows`).
    #[test]
    fn mb2e_expand_height_option_drives_band_rows() {
        use lattice_config::{CommandLineExpandHeight, ExpandHeight};
        let a = app_with("hello\n", 40);
        // Default is `half`: half of a 40-row frame, clamped.
        assert_eq!(a.command_line_expand_height(), ExpandHeight::Half);
        assert_eq!(a.command_line_expand_height().rows(40), 20);

        // `full` grows as tall as the frame allows (one pane row kept).
        let _ = a
            .editor
            .config
            .set_typed::<CommandLineExpandHeight>(ExpandHeight::Full);
        assert_eq!(a.command_line_expand_height(), ExpandHeight::Full);
        assert_eq!(a.command_line_expand_height().rows(40), 38);

        // A fixed row count pins the band, clamped to the frame.
        let _ = a
            .editor
            .config
            .set_typed::<CommandLineExpandHeight>(ExpandHeight::Fixed(7));
        assert_eq!(a.command_line_expand_height().rows(40), 7);
    }

    // ── MB.3 (rich minibuffer, phase 3): the `q:` / `:history`
    //    fuzzy history picker. Accept LOADS the picked command into
    //    the editable `:` line — it does NOT execute. ──

    /// `q:` (Normal chord) opens the history picker; accepting a row
    /// loads that command into the `:` line WITHOUT running it, and a
    /// subsequent `<CR>` executes it. The whole MB.3 contract in one
    /// flow.
    #[test]
    fn mb3_q_colon_picker_loads_editable_line_then_cr_runs() {
        let mut a = app_with("hello\n", 10);
        // Build history through the real submit path.
        submit_ex(&mut a, "set number");
        submit_ex(&mut a, "set wrap");
        assert!(!a.editor.command_line_active());

        // `q:` in Normal opens the history picker (chord routes through
        // the trie: `q` arms partial, `:` resolves the exact path).
        press_chars(&mut a, "q:");
        let p = a
            .editor
            .picker
            .as_ref()
            .expect("`q:` must open the history picker");
        assert_eq!(p.source_id.as_deref(), Some("history"));
        // Newest-first: `set wrap` is the top row.
        assert!(p.candidates[0].raw.display.contains("set wrap"));

        // Narrow to `set number` and accept.
        for c in "number".chars() {
            a.apply(Action::PickerAppend(c));
        }
        a.apply(Action::PickerAccept);

        // Picker closed; the `:` line is open, seeded, NOT executed.
        assert!(a.editor.picker.is_none());
        assert!(
            a.editor.command_line_active(),
            "accept must open the `:` line"
        );
        assert_eq!(a.editor.command_line(), "set number");
        // Nothing ran yet: history is unchanged (no dup pushed).
        assert_eq!(
            a.editor.command_history,
            vec!["set number".to_string(), "set wrap".to_string()]
        );

        // `<CR>` now runs the loaded command.
        a.apply(Action::CommandLineSubmit);
        assert!(!a.editor.command_line_active());
    }

    /// `:history` ex-command opens the same picker as `q:`.
    #[test]
    fn mb3_history_ex_command_opens_picker() {
        let mut a = app_with("hello\n", 10);
        submit_ex(&mut a, "write");
        submit_ex(&mut a, "history");
        assert_eq!(a.editor.picker.as_ref().unwrap().source_id.as_deref(), Some("history"));
    }

    /// `q:` with no history is graceful: no picker opens, no panic —
    /// the source returns an error the host echoes.
    #[test]
    fn mb3_q_colon_empty_history_is_graceful() {
        let mut a = app_with("hello\n", 10);
        assert!(a.editor.command_history.is_empty());
        press_chars(&mut a, "q:");
        assert!(
            a.editor.picker.is_none(),
            "empty history must not open a picker"
        );
        assert!(!a.editor.command_line_active());
    }

    /// `q:` from the expanded tier-2 band's Normal mode seeds the
    /// picker filter with the in-progress `:` text (vim's
    /// command-window muscle memory).
    #[test]
    fn mb3_q_colon_from_expanded_band_seeds_filter() {
        let mut a = app_with("hello\n", 10);
        submit_ex(&mut a, "set number");
        submit_ex(&mut a, "write");
        // Open the `:` line, expand to tier-2, drop to Normal in the band.
        a.apply(Action::EnterCommandLine);
        press_chars(&mut a, "set");
        a.apply(Action::CommandLineToggleExpand);
        a.apply(Action::CommandLineCancel); // `<Esc>` in the band → Normal
        assert!(a.editor.command_line_expanded());
        assert!(matches!(a.editor.modal, ModalState::Normal));

        // `q:` in the band's Normal mode opens the picker pre-filtered
        // to `set`, so only `set number` survives.
        press_chars(&mut a, "q:");
        let p = a
            .editor
            .picker
            .as_ref()
            .expect("`q:` in the band must open the history picker");
        assert_eq!(p.query, "set", "filter seeded from the in-progress `:` text");
        assert_eq!(p.candidates.len(), 1);
        assert!(p.candidates[0].raw.display.contains("set number"));
    }

    // ── MB.4 (rich minibuffer, phase 4): live `:` line decorations
    //    — syntax highlighting, error indicator, parameter hint. ──

    /// Typing a known command populates the published decorations:
    /// the command word is a keyword span and a param hint appears;
    /// no error. (Not submitted — `:write` would touch the fs; the
    /// submit-clears path is exercised via cancel below and the
    /// host-side submit handler.)
    #[test]
    fn mb4_typing_known_command_populates_decorations() {
        use lattice_cells::style::Style;
        let mut a = app_with("hello\n", 10);
        a.apply(Action::EnterCommandLine);
        press_chars(&mut a, "write foo");
        let d = a
            .editor
            .command_line_decorations
            .as_ref()
            .expect("decorations produced on edit");
        assert_eq!(d.spans[0].style, Style::Keyword, "command word is a keyword");
        assert!(d.error.is_none(), "known command has no error");
        assert!(d.param_hint.is_some(), "`:write <file>` offers a hint");
    }

    /// An unknown command, once committed with a trailing space, sets
    /// the live error indicator as the user types.
    #[test]
    fn mb4_unknown_command_sets_live_error() {
        let mut a = app_with("hello\n", 10);
        a.apply(Action::EnterCommandLine);
        press_chars(&mut a, "frobnicate ");
        let d = a
            .editor
            .command_line_decorations
            .as_ref()
            .expect("decorations");
        assert_eq!(
            d.error.as_deref(),
            Some("unknown command: frobnicate"),
            "committed unknown command flags an error"
        );
    }

    /// Cancelling the `:` line clears the decorations (no stale
    /// highlight lingers after close).
    #[test]
    fn mb4_cancel_clears_decorations() {
        let mut a = app_with("hello\n", 10);
        a.apply(Action::EnterCommandLine);
        press_chars(&mut a, "write");
        assert!(a.editor.command_line_decorations.is_some());
        a.apply(Action::CommandLineCancel);
        assert!(a.editor.command_line_decorations.is_none());
    }

    #[test]
    fn prefer_aliases_rewrites_canonical_to_alias() {
        use lattice_completion::{
            CandidateData, CandidateKind, MatchScore, RawCandidate, RenderedCandidate,
        };
        use lattice_grammar::source::SourceLocation;
        let mut candidates = vec![RenderedCandidate {
            raw: RawCandidate {
                text: "ex:describe-command".into(),
                display: "ex:describe-command".into(),
                kind: CandidateKind::Command,
                data: CandidateData::Command {
                    name: "ex:describe-command".into(),
                    doc: "doc".into(),
                    kind_label: "ex-command".into(),
                    source: SourceLocation::synthetic("test"),
                },
                source: None,
                accept_action: None,
                annotations: Vec::new(),
                display_spans: Vec::new(),
            },
            score: MatchScore::PERFECT,
            match_ranges: vec![],
            annotations: vec![],
        }];
        prefer_aliases_for_command_candidates(&mut candidates, "descri");
        assert_eq!(candidates[0].raw.text, "describe-command");
        assert_eq!(candidates[0].raw.display, "describe-command");
        // Match ranges recomputed against the new text.
        assert!(!candidates[0].match_ranges.is_empty());
    }

    #[test]
    #[allow(clippy::single_range_in_vec_init)]
    fn prefer_aliases_leaves_non_command_candidates_alone() {
        use lattice_completion::{
            CandidateData, CandidateKind, MatchScore, RawCandidate, RenderedCandidate,
        };
        let mut candidates = vec![RenderedCandidate {
            raw: RawCandidate {
                text: "/tmp/foo.rs".into(),
                display: "foo.rs".into(),
                kind: CandidateKind::File,
                data: CandidateData::File {
                    path: "/tmp/foo.rs".into(),
                    is_dir: false,
                    size: None,
                },
                source: None,
                accept_action: None,
                annotations: Vec::new(),
                display_spans: Vec::new(),
            },
            score: MatchScore::PERFECT,
            match_ranges: vec![0..3],
            annotations: vec![],
        }];
        prefer_aliases_for_command_candidates(&mut candidates, "tmp");
        // File candidate untouched.
        assert_eq!(candidates[0].raw.text, "/tmp/foo.rs");
    }

    #[test]
    fn prefer_aliases_keeps_canonical_when_alias_does_not_match_query() {
        use lattice_completion::{
            CandidateData, CandidateKind, MatchScore, RawCandidate, RenderedCandidate,
        };
        use lattice_grammar::source::SourceLocation;
        // "next-error" → alias "cnext". Query "next-" matches the
        // canonical name but NOT the alias (no '-' in "cnext").
        // The canonical name must be preserved.
        let mut candidates = vec![RenderedCandidate {
            raw: RawCandidate {
                text: "next-error".into(),
                display: "next-error".into(),
                kind: CandidateKind::Command,
                data: CandidateData::Command {
                    name: "next-error".into(),
                    doc: "jump to next error".into(),
                    kind_label: "ex-command".into(),
                    source: SourceLocation::synthetic("test"),
                },
                source: None,
                accept_action: None,
                annotations: Vec::new(),
                display_spans: Vec::new(),
            },
            score: MatchScore::PREFIX,
            match_ranges: vec![0..5],
            annotations: vec![],
        }];
        prefer_aliases_for_command_candidates(&mut candidates, "next-");
        // Must keep the canonical name since alias "cnext" doesn't
        // contain '-' and thus doesn't match the query.
        assert_eq!(candidates[0].raw.text, "next-error");
        assert_eq!(candidates[0].raw.display, "next-error");
        // Original pipeline match ranges preserved.
        assert_eq!(candidates[0].match_ranges, vec![0..5]);
    }

    #[test]
    fn enter_command_line_clears_buffer_and_sets_modal() {
        let mut a = app_with("abc", 10);
        // A stale echo message must be cleared when the `:` line opens.
        // (The buffer starts empty on a fresh open; MB.2's `:`-no-op guard
        // means we can't seed a "stale" buffer without opening the line,
        // which is itself the no-op path — covered separately.)
        a.editor.last_message = Some(EchoMessage {
            text: "stale".into(),
            level: EchoLevel::Info,
        });
        a.apply(Action::EnterCommandLine);
        assert_eq!(a.editor.modal, ModalState::Command);
        assert_eq!(a.editor.command_line(), "");
        assert!(a.editor.last_message.is_none());
    }

    #[test]
    fn command_line_append_pushes_chars() {
        let mut a = app_with("", 10);
        a.apply(Action::EnterCommandLine);
        a.apply(Action::CommandLineAppend('w'));
        a.apply(Action::CommandLineAppend('q'));
        assert_eq!(a.editor.command_line(), "wq");
    }

    #[test]
    fn command_line_backspace_pops_chars() {
        let mut a = app_with("", 10);
        a.apply(Action::EnterCommandLine);
        a.apply(Action::CommandLineAppend('w'));
        a.apply(Action::CommandLineAppend('q'));
        a.apply(Action::CommandLineBackspace);
        assert_eq!(a.editor.command_line(), "w");
    }

    #[test]
    fn command_line_backspace_on_empty_exits_command_modal() {
        let mut a = app_with("", 10);
        a.apply(Action::EnterCommandLine);
        a.apply(Action::CommandLineBackspace);
        assert_eq!(a.editor.modal, ModalState::Normal);
    }

    #[test]
    fn command_line_cancel_clears_and_returns_to_normal() {
        let mut a = app_with("", 10);
        a.apply(Action::EnterCommandLine);
        a.apply(Action::CommandLineAppend('w'));
        a.apply(Action::CommandLineCancel);
        assert_eq!(a.editor.modal, ModalState::Normal);
        assert_eq!(a.editor.command_line(), "");
    }

    #[test]
    fn submit_q_on_clean_buffer_quits() {
        let mut a = app_with("hello", 10);
        a.apply(Action::EnterCommandLine);
        for c in "q".chars() {
            a.apply(Action::CommandLineAppend(c));
        }
        a.apply(Action::CommandLineSubmit);
        assert!(a.editor.should_quit);
        assert_eq!(a.editor.modal, ModalState::Normal);
    }

    #[test]
    fn submit_q_on_dirty_buffer_refuses() {
        let mut a = app_with("hello", 10);
        a.apply(Action::EnterMode(ModalState::Insert));
        a.apply(Action::Insert("X".into()));
        a.apply(Action::EnterMode(ModalState::Normal));
        assert!(a.editor.document.dirty());

        a.apply(Action::EnterCommandLine);
        a.apply(Action::CommandLineAppend('q'));
        a.apply(Action::CommandLineSubmit);
        assert!(!a.editor.should_quit);
        let msg = a.editor.last_message.as_ref().unwrap();
        assert_eq!(msg.level, EchoLevel::Error);
        assert!(msg.text.contains("no write since last change"));
    }

    #[test]
    fn submit_q_bang_quits_even_when_dirty() {
        let mut a = app_with("hello", 10);
        a.apply(Action::EnterMode(ModalState::Insert));
        a.apply(Action::Insert("X".into()));
        a.apply(Action::EnterMode(ModalState::Normal));
        a.apply(Action::EnterCommandLine);
        for c in "q!".chars() {
            a.apply(Action::CommandLineAppend(c));
        }
        a.apply(Action::CommandLineSubmit);
        assert!(a.editor.should_quit);
    }

    #[test]
    fn submit_w_without_path_errors() {
        let mut a = app_with("hello", 10);
        a.apply(Action::EnterCommandLine);
        a.apply(Action::CommandLineAppend('w'));
        a.apply(Action::CommandLineSubmit);
        let msg = a.editor.last_message.as_ref().unwrap();
        assert_eq!(msg.level, EchoLevel::Error);
        assert!(msg.text.contains("no file name"));
    }

    #[test]
    fn submit_w_with_path_writes_and_clears_dirty() {
        let dir = unique_tempdir();
        let path = dir.join("out.txt");
        let mut a = App::new(Document::from_text("hello"));
        a.set_viewport_height(10);
        // Move to end of line, then enter insert and append "!".
        a.apply(invoke_motion(a.editor.builtins.line_end));
        a.apply(Action::EnterMode(ModalState::Insert));
        a.apply(Action::Insert("!".into()));
        a.apply(Action::EnterMode(ModalState::Normal));
        assert!(a.editor.document.dirty());

        a.apply(Action::EnterCommandLine);
        for c in format!("w {}", path.display()).chars() {
            a.apply(Action::CommandLineAppend(c));
        }
        a.apply(Action::CommandLineSubmit);

        assert!(!a.editor.document.dirty());
        let msg = a.editor.last_message.as_ref().unwrap();
        assert_eq!(msg.level, EchoLevel::Info);
        assert!(msg.text.contains("written"));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "hello!");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn submit_wq_writes_then_quits() {
        let dir = unique_tempdir();
        let path = dir.join("out.txt");
        std::fs::write(&path, "first").unwrap();

        let mut a = App::new(Document::open(&path).unwrap());
        a.set_viewport_height(10);
        a.apply(Action::EnterMode(ModalState::Insert));
        a.apply(Action::Insert("X".into()));
        a.apply(Action::EnterMode(ModalState::Normal));

        a.apply(Action::EnterCommandLine);
        for c in "wq".chars() {
            a.apply(Action::CommandLineAppend(c));
        }
        a.apply(Action::CommandLineSubmit);

        assert!(a.editor.should_quit);
        let on_disk = std::fs::read_to_string(&path).unwrap();
        assert!(on_disk.starts_with("X"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn submit_unknown_command_surfaces_error() {
        let mut a = app_with("hello", 10);
        a.apply(Action::EnterCommandLine);
        for c in "frobnicate".chars() {
            a.apply(Action::CommandLineAppend(c));
        }
        a.apply(Action::CommandLineSubmit);
        let msg = a.editor.last_message.as_ref().unwrap();
        assert_eq!(msg.level, EchoLevel::Error);
        assert!(msg.text.contains("frobnicate"));
    }

    #[test]
    fn submit_pushes_command_into_history() {
        let mut a = app_with("hello", 10);
        submit_ex(&mut a, "set number");
        assert_eq!(a.editor.command_history, vec!["set number".to_string()]);
    }

    #[test]
    fn submit_dedupes_consecutive_identical_history() {
        let mut a = app_with("hello", 10);
        submit_ex(&mut a, "set number");
        submit_ex(&mut a, "set number");
        assert_eq!(a.editor.command_history.len(), 1);
    }

    #[test]
    fn empty_submit_does_not_push_history() {
        let mut a = app_with("hello", 10);
        a.apply(Action::EnterCommandLine);
        a.apply(Action::CommandLineSubmit);
        assert!(a.editor.command_history.is_empty());
    }

    #[test]
    fn up_in_command_walks_to_most_recent_history() {
        let mut a = app_with("hello", 10);
        submit_ex(&mut a, "set number");
        submit_ex(&mut a, "set nonumber");
        a.apply(Action::EnterCommandLine);
        a.apply(Action::CommandLineHistoryPrev);
        assert_eq!(a.editor.command_line(), "set nonumber");
    }

    #[test]
    fn up_then_up_walks_to_older() {
        let mut a = app_with("hello", 10);
        submit_ex(&mut a, "set number");
        submit_ex(&mut a, "set nonumber");
        a.apply(Action::EnterCommandLine);
        a.apply(Action::CommandLineHistoryPrev);
        a.apply(Action::CommandLineHistoryPrev);
        assert_eq!(a.editor.command_line(), "set number");
    }

    #[test]
    fn down_returns_to_in_progress_typed_text() {
        let mut a = app_with("hello", 10);
        submit_ex(&mut a, "set number");
        a.apply(Action::EnterCommandLine);
        for c in "se".chars() {
            a.apply(Action::CommandLineAppend(c));
        }
        // User starts typing "se", presses Up -> walks to "set number".
        a.apply(Action::CommandLineHistoryPrev);
        assert_eq!(a.editor.command_line(), "set number");
        // Down returns to "se".
        a.apply(Action::CommandLineHistoryNext);
        assert_eq!(a.editor.command_line(), "se");
    }

    #[test]
    fn history_navigation_with_no_history_is_no_op() {
        let mut a = app_with("hello", 10);
        a.apply(Action::EnterCommandLine);
        a.apply(Action::CommandLineAppend('w'));
        a.apply(Action::CommandLineHistoryPrev);
        assert_eq!(a.editor.command_line(), "w");
    }

    #[test]
    fn history_persists_across_command_sessions() {
        let mut a = app_with("hello", 10);
        submit_ex(&mut a, "set number");
        // Reopen command line; Up should still recall.
        a.apply(Action::EnterCommandLine);
        a.apply(Action::CommandLineHistoryPrev);
        assert_eq!(a.editor.command_line(), "set number");
    }

    /// Regression scaffold for the user-reported "completion is
    /// broken" bug: simulates the full keystroke pipeline (`:`,
    /// then each char, then `<Tab>`) instead of pre-staging modal
    /// + command_line. If `tab_in_command_mode_opens_completion_popup`
    /// passes but this fails, the bug lives in the
    /// keystroke/translate/dispatch path, not in
    /// `do_command_line_complete_or_advance` itself.
    #[test]
    fn typing_desc_then_tab_opens_completion_popup() {
        let mut a = app_with("xx", 10);
        // Enter cmdline mode via `:` keystroke.
        a.apply(Action::EnterCommandLine);
        assert_eq!(a.editor.modal, ModalState::Command);
        // Append `desc`.
        for c in "desc".chars() {
            a.apply(Action::CommandLineAppend(c));
        }
        assert_eq!(a.editor.command_line(), "desc");
        // Tab — should open the completion popup with command-name
        // candidates like `describe-command`, `describe-key`.
        a.apply(Action::CommandLineCompleteOrAdvance);
        let state = a
            .editor
            .completion_state
            .as_ref()
            .expect("popup should open after :desc<Tab>");
        assert!(
            state
                .candidates
                .iter()
                .any(|c| c.raw.text == "describe-command"),
            "candidates: {:?}",
            state
                .candidates
                .iter()
                .map(|c| &c.raw.text)
                .collect::<Vec<_>>(),
        );
    }

    #[test]
    fn tab_in_command_mode_opens_completion_popup() {
        let mut a = app_in_command_mode("descri");
        a.apply(Action::CommandLineCompleteOrAdvance);
        let state = a
            .editor
            .completion_state
            .as_ref()
            .expect("popup should open");
        // Candidates use the user-facing alias form, not the
        // canonical `ex:*` registry name. Both `:describe-command`
        // and `:ex:describe-command` parse correctly via the
        // dispatcher's two-stage resolution; the popup shows the
        // form a user actually types.
        assert!(
            state
                .candidates
                .iter()
                .any(|c| c.raw.text == "describe-command")
        );
        assert!(
            state
                .candidates
                .iter()
                .any(|c| c.raw.text == "describe-buffer")
        );
        assert_eq!(state.selected, 0);
    }

    #[test]
    fn shift_tab_walks_back_through_candidates() {
        let mut a = app_in_command_mode("descri");
        a.apply(Action::CommandLineCompleteOrAdvance);
        a.apply(Action::CommandLineCompleteOrAdvance);
        a.apply(Action::CommandLineCompleteOrAdvance);
        a.apply(Action::CommandLineCompletePrev);
        assert_eq!(a.editor.completion_state.as_ref().unwrap().selected, 1);
    }

    #[test]
    fn chord_capture_active_only_when_in_chord_arg_slot() {
        // Slice 3c.final.E.5i: `chord_capture_active` reads modal
        // through `ad()` (RS-backed mirror), so direct field
        // mutations need an explicit `publish_render_state()` to
        // refresh the mirror.
        let mut a = app_with("xx", 10);
        a.editor.modal = ModalState::Command;
        a.editor.publish_render_state();
        // Empty cmdline -> CommandName slot, not chord-capture.
        a.editor.set_command_line_text("");
        a.editor.publish_render_state();
        assert!(!a.chord_capture_active());
        // Mid command-name slot.
        a.editor.set_command_line_text("describe-key");
        a.editor.publish_render_state();
        assert!(!a.chord_capture_active());
        // Now the cursor is past the space; arg slot is `chord`
        // with kind=Chord -> capture is active.
        a.editor.set_command_line_text("describe-key ");
        a.editor.publish_render_state();
        assert!(a.chord_capture_active());
        // describe-command's first arg is String, NOT Chord ->
        // no capture even though we're in an arg slot.
        a.editor.set_command_line_text("describe-command ");
        a.editor.publish_render_state();
        assert!(!a.chord_capture_active());
        // Outside Command modal, never active.
        a.editor.modal = ModalState::Normal;
        a.editor.publish_render_state();
        a.editor.set_command_line_text("describe-key ");
        a.editor.publish_render_state();
        assert!(!a.chord_capture_active());
    }

    #[test]
    fn chord_capture_active_for_canonical_command_name() {
        // `:ex:describe-key ` (canonical, not the alias). The slot
        // detector tries `id_by_name` first and only falls back
        // to alias-expand, so both forms switch into chord-capture.
        let mut a = app_with("xx", 10);
        a.editor.modal = ModalState::Command;
        a.editor.publish_render_state();
        a.editor.set_command_line_text("ex:describe-key ");
        a.editor.publish_render_state();
        assert!(a.chord_capture_active());
    }

    #[test]
    fn empty_submit_of_describe_key_arms_chord_prompt() {
        // User typed `:describe-key<CR>` with no arg. The required
        // Chord arg is missing -- we shouldn't error; we should
        // prefill the cmdline and arm auto-submit.
        let mut a = app_in_command_mode("describe-key");
        a.apply(Action::CommandLineSubmit);
        assert_eq!(a.editor.command_line(), "describe-key ");
        assert!(a.editor.auto_submit_after_chord);
        assert!(matches!(a.editor.modal, ModalState::Command));
    }

    #[test]
    fn empty_submit_of_canonical_describe_key_arms_chord_prompt() {
        // Same prompt path through the canonical name, not just
        // the alias.
        let mut a = app_in_command_mode("ex:describe-key");
        a.apply(Action::CommandLineSubmit);
        assert_eq!(a.editor.command_line(), "ex:describe-key ");
        assert!(a.editor.auto_submit_after_chord);
    }

    #[test]
    fn empty_submit_of_describe_command_arms_prompt_without_chord_capture() {
        // describe-command's first arg is String (Required) -- the
        // generalized missing-arg path arms a prompt, prefills the
        // cmdline, and leaves the user in Command mode to type the
        // arg. Auto-submit is OFF (only Chord-kind args auto-submit
        // on the next keystroke).
        let mut a = app_in_command_mode("describe-command");
        a.apply(Action::CommandLineSubmit);
        assert!(matches!(a.editor.modal, ModalState::Command));
        assert!(!a.editor.auto_submit_after_chord);
        // Prefilled with the command word + space; cursor in arg slot.
        assert_eq!(a.editor.command_line(), "describe-command ");
        // Echo area carries the arg's prompt.
        assert!(a.editor.last_message.is_some());
    }

    #[test]
    fn empty_submit_of_optional_arg_command_does_not_arm_prompt() {
        // `:write` (alias for `ex:write`) has an OPTIONAL path arg
        // (default = `None` -- absent means "use current path").
        // Submitting bare runs the command normally; no prompt arm.
        let mut a = app_in_command_mode("w");
        a.apply(Action::CommandLineSubmit);
        // Cmdline closed -- the missing-arg prompt path skipped this
        // command because its schema's first arg is Optional.
        assert!(matches!(a.editor.modal, ModalState::Normal));
        assert!(!a.editor.auto_submit_after_chord);
    }

    #[test]
    fn missing_arg_prompt_preserves_user_alias() {
        // User typed the alias `apropos`; prefill must preserve the
        // alias rather than normalising to the canonical
        // `ex:apropos`. (Apropos's `pattern` arg is Required.)
        let mut a = app_in_command_mode("apropos");
        a.apply(Action::CommandLineSubmit);
        assert_eq!(a.editor.command_line(), "apropos ");
        assert!(matches!(a.editor.modal, ModalState::Command));
    }

    #[test]
    fn submit_with_arg_supplied_takes_normal_path() {
        // `describe-key j` with explicit arg should NOT enter
        // prompt mode -- it should just dispatch.
        let mut a = app_in_command_mode("describe-key j");
        a.apply(Action::CommandLineSubmit);
        assert!(!a.editor.auto_submit_after_chord);
        assert!(matches!(a.editor.modal, ModalState::Normal));
        assert!(a.editor.popup_buffer.is_some());
    }

    #[test]
    fn ctrl_h_on_known_command_describes_it_directly() {
        // `:describe-command` on the cmdline; <C-h> describes that
        // command itself (smart-resolve).
        let mut a = app_in_command_mode("describe-command");
        a.apply(Action::CommandLineDescribeUnderCursor);
        let h = a.popup_help().expect("help should open");
        assert!(h.title.contains("ex:describe-command"));
    }

    #[test]
    fn ctrl_h_on_arg_describes_parent_command_at_arg_anchor() {
        // `:describe-command moti` -- the cursor's word `moti`
        // doesn't resolve to a command; fall back to describing
        // the parent (`ex:describe-command`) scrolled to the
        // `arg:name` anchor.
        let mut a = app_in_command_mode("describe-command moti");
        a.apply(Action::CommandLineDescribeUnderCursor);
        let h = a.popup_help().expect("help should open");
        assert!(h.title.contains("ex:describe-command"));
        let scroll = h.scroll;
        // scroll should be set to the arg:name anchor's line.
        let anchors = a.popup_help_anchors().expect("help anchors seeded");
        let arg_anchor = anchors.iter().find(|a| a.name == "arg:name").unwrap();
        assert_eq!(scroll, arg_anchor.line as usize);
    }

    #[test]
    fn ctrl_h_on_arg_value_that_is_a_known_command_describes_it() {
        // `:describe-command motion:line-down` -- the arg VALUE
        // resolves to a known command. Hybrid: describe THAT.
        let mut a = app_in_command_mode("describe-command motion:line-down");
        a.apply(Action::CommandLineDescribeUnderCursor);
        let h = a.popup_help().expect("help should open");
        assert!(h.title.contains("motion:line-down"));
    }

    #[test]
    fn ctrl_h_on_unknown_word_emits_error_message() {
        let mut a = app_in_command_mode("no-such-command");
        a.apply(Action::CommandLineDescribeUnderCursor);
        assert!(a.editor.popup_buffer.is_none());
        let msg = a.editor.last_message.as_ref().unwrap();
        assert_eq!(msg.level, EchoLevel::Error);
    }

    #[test]
    fn cmdline_completion_includes_lsp_subcommand_aliases() {
        // Diagnostic: typing `:lsp-` and tabbing should surface
        // `lsp-trace`, `lsp-restart`, `lsp-status`, etc. -- the
        // user-facing aliases for `ex:lsp-trace` etc. The
        // CommandsGenerator returns canonical names (`ex:lsp-trace`);
        // `prefer_aliases_for_command_candidates` rewrites them
        // to the longest alias (`lsp-trace`). User reported these
        // not appearing; pin the wiring with a regression test.
        let mut a = app_in_command_mode("lsp-");
        a.apply(Action::CommandLineCompleteOrAdvance);
        let state = a
            .editor
            .completion_state
            .as_ref()
            .expect("popup should open");
        let texts: Vec<&str> = state
            .candidates
            .iter()
            .map(|c| c.raw.text.as_str())
            .collect();
        for needle in [
            "lsp-trace",
            "lsp-status",
            "lsp-restart",
            "lsp-log",
            "lsp-log-level",
            "lsp-log-clear",
        ] {
            assert!(
                texts.contains(&needle),
                "completion should include `{needle}` -- got {:?}",
                texts
            );
        }
    }
}
