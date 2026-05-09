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

use super::{App, EchoLevel};

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
        if !matches!(self.modal, ModalState::Command) {
            return;
        }
        let line = self.command_line.clone();
        let cursor = line.len();
        let alias_resolver = |short: &str| {
            crate::excommand::aliases()
                .get(short)
                .map(|s| (*s).to_string())
        };
        let slot = lattice_completion::current_slot(&line, cursor, &self.registry, &alias_resolver);

        let word = slot.prefix();
        let canonical = if word.is_empty() {
            None
        } else {
            alias_resolver(word)
                .or_else(|| self.registry.id_by_name(word).and(Some(word.to_string())))
        };

        if let Some(name) = canonical
            && self.registry.id_by_name(&name).is_some()
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
        if !matches!(self.modal, ModalState::Command) {
            return;
        }
        if let Some(state) = self.completion_state.as_mut() {
            if !state.candidates.is_empty() {
                state.selected = (state.selected + 1) % state.candidates.len();
            }
            return;
        }
        self.open_completion_popup();
    }

    pub(super) fn do_command_line_complete_prev(&mut self) {
        if let Some(state) = self.completion_state.as_mut()
            && !state.candidates.is_empty()
        {
            if state.selected == 0 {
                state.selected = state.candidates.len() - 1;
            } else {
                state.selected -= 1;
            }
        }
    }

    pub(super) fn do_command_line_accept_completion(&mut self) {
        let Some(state) = self.completion_state.take() else {
            return;
        };
        if state.candidates.is_empty() {
            return;
        }
        let chosen = &state.candidates[state.selected];
        self.command_line.replace_range(
            state.replace_start..self.command_line.len(),
            &chosen.raw.text,
        );
    }

    /// Walk through `:` command history in Command modal. `back = true`
    /// goes to older entries (Up); `false` goes newer (Down).
    pub(super) fn do_command_history_step(&mut self, back: bool) {
        if !matches!(self.modal, ModalState::Command) {
            return;
        }
        if self.command_history.is_empty() {
            return;
        }
        let new_cursor = match (self.command_history_cursor, back) {
            (None, true) => {
                self.command_history_pending = Some(self.command_line.clone());
                Some(self.command_history.len() - 1)
            }
            (None, false) => return,
            (Some(0), true) => return,
            (Some(i), true) => Some(i - 1),
            (Some(i), false) if i + 1 >= self.command_history.len() => {
                if let Some(pending) = self.command_history_pending.take() {
                    self.command_line = pending;
                }
                self.command_history_cursor = None;
                return;
            }
            (Some(i), false) => Some(i + 1),
        };
        if let Some(idx) = new_cursor {
            self.command_line = self.command_history[idx].clone();
            self.command_history_cursor = Some(idx);
        }
    }

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
        let line = self.command_line.trim();
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
        let canonical = self.registry.id_by_name(cmd).or_else(|| {
            crate::excommand::aliases()
                .get(cmd)
                .copied()
                .and_then(|c| self.registry.id_by_name(c))
        })?;
        let spec = self.registry.ex_command_spec(canonical)?;
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
        if !matches!(self.modal, ModalState::Command) {
            return false;
        }
        let line = &self.command_line;
        let alias_resolver = |short: &str| {
            crate::excommand::aliases()
                .get(short)
                .map(|s| (*s).to_string())
        };
        let slot =
            lattice_completion::current_slot(line, line.len(), &self.registry, &alias_resolver);
        matches!(
            &slot,
            lattice_completion::CommandLineSlot::Arg { arg_spec, .. }
                if arg_spec.kind == lattice_grammar::ArgKind::Chord
        )
    }
}
