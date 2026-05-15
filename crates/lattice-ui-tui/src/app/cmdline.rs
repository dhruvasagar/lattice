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
        if !matches!(self.editor.modal, ModalState::Command) {
            return;
        }
        let line = self.editor.command_line.clone();
        let cursor = line.len();
        let alias_resolver = |short: &str| {
            crate::excommand::aliases()
                .get(short)
                .map(|s| (*s).to_string())
        };
        let slot = lattice_completion::current_slot(&line, cursor, &self.editor.registry, &alias_resolver);

        let word = slot.prefix();
        let canonical = if word.is_empty() {
            None
        } else {
            alias_resolver(word)
                .or_else(|| self.editor.registry.id_by_name(word).and(Some(word.to_string())))
        };

        if let Some(name) = canonical
            && self.editor.registry.id_by_name(&name).is_some()
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
        if !matches!(self.editor.modal, ModalState::Command) {
            return;
        }
        if let Some(state) = self.editor.completion_state.as_mut() {
            if !state.candidates.is_empty() {
                state.selected = (state.selected + 1) % state.candidates.len();
            }
            return;
        }
        self.open_completion_popup();
    }

    pub(super) fn do_command_line_complete_prev(&mut self) {
        if let Some(state) = self.editor.completion_state.as_mut()
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
        let Some(state) = self.editor.completion_state.take() else {
            return;
        };
        if state.candidates.is_empty() {
            return;
        }
        let chosen = &state.candidates[state.selected];
        self.editor.command_line.replace_range(
            state.replace_start..self.editor.command_line.len(),
            &chosen.raw.text,
        );
    }

    /// Walk through `:` command history in Command modal. `back = true`
    /// goes to older entries (Up); `false` goes newer (Down).
    pub(super) fn do_command_history_step(&mut self, back: bool) {
        if !matches!(self.editor.modal, ModalState::Command) {
            return;
        }
        if self.editor.command_history.is_empty() {
            return;
        }
        let new_cursor = match (self.editor.command_history_cursor, back) {
            (None, true) => {
                self.editor.command_history_pending = Some(self.editor.command_line.clone());
                Some(self.editor.command_history.len() - 1)
            }
            (None, false) => return,
            (Some(0), true) => return,
            (Some(i), true) => Some(i - 1),
            (Some(i), false) if i + 1 >= self.editor.command_history.len() => {
                if let Some(pending) = self.editor.command_history_pending.take() {
                    self.editor.command_line = pending;
                }
                self.editor.command_history_cursor = None;
                return;
            }
            (Some(i), false) => Some(i + 1),
        };
        if let Some(idx) = new_cursor {
            self.editor.command_line = self.editor.command_history[idx].clone();
            self.editor.command_history_cursor = Some(idx);
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
        let line = self.editor.command_line.trim();
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
        let canonical = self.editor.registry.id_by_name(cmd).or_else(|| {
            crate::excommand::aliases()
                .get(cmd)
                .copied()
                .and_then(|c| self.editor.registry.id_by_name(c))
        })?;
        let spec = self.editor.registry.ex_command_spec(canonical)?;
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
        if !matches!(self.editor.modal, ModalState::Command) {
            return false;
        }
        let line = &self.editor.command_line;
        let alias_resolver = |short: &str| {
            crate::excommand::aliases()
                .get(short)
                .map(|s| (*s).to_string())
        };
        let slot =
            lattice_completion::current_slot(line, line.len(), &self.editor.registry, &alias_resolver);
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
    pub(super) fn open_completion_popup(&mut self) {
        match self.compute_completion_state() {
            Ok(state) => {
                if self.completion_auto_insert_single() && state.candidates.len() == 1 {
                    let chosen_text = state.candidates[0].raw.text.clone();
                    self.editor.command_line
                        .replace_range(state.replace_start..self.editor.command_line.len(), &chosen_text);
                    // Don't open the popup -- the single candidate
                    // is already applied. `completion_state` stays
                    // `None` so the next `<Tab>` would re-trigger
                    // the pipeline against the new line.
                    return;
                }
                self.editor.completion_state = Some(state);
            }
            Err(err) => {
                let (level, msg) = err.echo();
                self.set_message(level, msg);
            }
        }
    }

    /// Re-run the completion pipeline against the current command line
    /// and update the popup in place. Called from `CommandLineAppend` /
    /// `CommandLineBackspace` / `CommandLineDeleteWordBackward` while
    /// the popup is open -- this is the vertico "filter as you type"
    /// behaviour. No echo: refresh is silent. Empty results keep the
    /// popup alive (so further edits can repopulate it); a slot
    /// transition that has no completion source closes it.
    pub(super) fn refresh_completion_popup(&mut self) {
        if self.editor.completion_state.is_none() {
            return;
        }
        match self.compute_completion_state() {
            Ok(state) => {
                self.editor.completion_state = Some(state);
            }
            Err(CompletionComputeError::NoMatches { .. }) => {
                // Keep the popup open with zero candidates so the user
                // can backspace and re-match without re-tabbing.
                if let Some(state) = self.editor.completion_state.as_mut() {
                    state.candidates.clear();
                    state.selected = 0;
                    state.original_line = self.editor.command_line.clone();
                }
            }
            Err(_) => {
                // Slot moved to a region with no completion source
                // (UnknownCommand, BeyondSchema, arg without
                // `completion`). Drop the popup; the user can re-Tab
                // to re-arm it later.
                self.editor.completion_state = None;
            }
        }
    }

    /// Slot-detect, build the pipeline, run it, and host-rewrite
    /// command candidates to user-facing aliases. Pure -- no
    /// `set_message` side effects, so both the open and the refresh
    /// path can share it. Errors carry enough info for the open path
    /// to surface them via echo.
    pub(super) fn compute_completion_state(
        &self,
    ) -> Result<CompletionState, CompletionComputeError> {
        let line = self.editor.command_line.clone();
        let cursor = line.len();
        let alias_resolver = |short: &str| {
            crate::excommand::aliases()
                .get(short)
                .map(|s| (*s).to_string())
        };
        let slot = lattice_completion::current_slot(&line, cursor, &self.editor.registry, &alias_resolver);
        let (source_name, prefix, replace_start) = match &slot {
            lattice_completion::CommandLineSlot::CommandName {
                prefix,
                replace_start,
            } => ("gen:commands", prefix.clone(), *replace_start),
            lattice_completion::CommandLineSlot::Arg {
                arg_spec,
                prefix,
                replace_start,
                ..
            } => match arg_spec.completion {
                Some(name) => (name, prefix.clone(), *replace_start),
                None => {
                    return Err(CompletionComputeError::NoCompletionForArg(
                        arg_spec.name.to_string(),
                    ));
                }
            },
            lattice_completion::CommandLineSlot::Empty => ("gen:commands", String::new(), 0),
            _ => {
                return Err(CompletionComputeError::NoCompletionAtCursor);
            }
        };

        let Some(generator) = self.editor.completion_registry.generator_by_name(source_name) else {
            return Err(CompletionComputeError::MissingSource(
                source_name.to_string(),
            ));
        };
        let generator_id = generator.id;
        let Some(pipeline) = lattice_completion::CompletionPipeline::for_generator(
            &self.editor.completion_registry,
            generator_id,
        ) else {
            return Err(CompletionComputeError::PipelineUnconfigured);
        };
        let snap = self.document.snapshot();
        let ctx = lattice_completion::GenerateContext {
            prefix: &prefix,
            buffer: &snap.buffer,
            registry: &self.editor.registry,
            case_sensitive: false,
        };
        let mut candidates = pipeline.run(&ctx, &prefix, &self.editor.completion_registry.cache);

        // Host-side post-process: command candidates from
        // `gen:commands` come back as canonical names
        // (`ex:describe-command`). Rewrite to the user-facing alias
        // (`describe-command`) so the popup shows -- and accepts --
        // what the user would actually type. The parser accepts
        // both forms (see excommand::parse_invocation), so this is
        // purely a UX rewrite.
        prefer_aliases_for_command_candidates(&mut candidates, &prefix);

        if candidates.is_empty() {
            return Err(CompletionComputeError::NoMatches { prefix });
        }
        Ok(CompletionState {
            candidates,
            selected: 0,
            replace_start,
            original_line: line,
        })
    }
}

/// Reasons `compute_completion_state` can fail. Kept narrow so the
/// open and refresh paths can pick different recovery strategies --
/// the open path turns these into echoed messages, the refresh path
/// usually closes the popup but keeps it alive on `NoMatches` so
/// vertico-style "type-to-filter, then back-out-to-recover" works.
#[derive(Debug, Clone)]
pub(super) enum CompletionComputeError {
    NoCompletionForArg(String),
    NoCompletionAtCursor,
    MissingSource(String),
    PipelineUnconfigured,
    NoMatches { prefix: String },
}

impl CompletionComputeError {
    pub(super) fn echo(&self) -> (EchoLevel, String) {
        match self {
            Self::NoCompletionForArg(name) => {
                (EchoLevel::Info, format!("no completion for arg `{name}`"))
            }
            Self::NoCompletionAtCursor => (EchoLevel::Info, "no completion at cursor".to_string()),
            Self::MissingSource(name) => (
                EchoLevel::Error,
                format!("completion source `{name}` not registered"),
            ),
            Self::PipelineUnconfigured => (
                EchoLevel::Error,
                "completion pipeline not configured (missing default matcher / ranker)".to_string(),
            ),
            Self::NoMatches { prefix } => {
                (EchoLevel::Info, format!("no completions for `{prefix}`"))
            }
        }
    }
}

/// Rewrite `gen:commands` candidates from canonical
/// (`ex:describe-command`) to their user-facing alias
/// (`describe-command`) so the popup shows -- and accepts --
/// what the user would actually type. The parser accepts
/// both forms; this is purely a UX rewrite.
///
/// We re-derive match ranges instead of clearing them so the
/// popup's match-face highlighting still shows where the query
/// matched.
fn prefer_aliases_for_command_candidates(
    candidates: &mut Vec<lattice_completion::RenderedCandidate>,
    query: &str,
) {
    let needle = query.to_ascii_lowercase();
    candidates.retain_mut(|c| {
        if !matches!(c.raw.kind, lattice_completion::CandidateKind::Command) {
            return true;
        }
        let canonical = c.raw.text.clone();
        let alias = crate::excommand::preferred_alias_for(&canonical);
        let new_text = alias.map(|a| a.to_string()).unwrap_or(canonical);
        c.raw.text = new_text.clone();
        c.raw.display = new_text.clone();
        // Recompute match ranges: subsequence-match the lowercase
        // query against the lowercase text; emit one range per
        // matched byte. Mirrors the fuzzy matcher's range output
        // so the popup highlights work consistently.
        c.match_ranges = subsequence_match_ranges(&needle, &new_text);
        // Keep the candidate even if the rewrite no longer
        // visibly contains the query -- the matcher already
        // accepted it against the canonical form. Filtering here
        // would surprise users (typing `ex:` then accepting an
        // alias-rewritten candidate would unexpectedly drop the
        // candidate). Empty match_ranges just means no
        // highlights.
        true
    });
}

fn subsequence_match_ranges(needle_lower: &str, haystack: &str) -> Vec<std::ops::Range<usize>> {
    if needle_lower.is_empty() {
        return Vec::new();
    }
    let n = needle_lower.as_bytes();
    let h = haystack.as_bytes();
    let mut ranges = Vec::with_capacity(n.len());
    let mut ni = 0;
    for (i, b) in h.iter().enumerate() {
        if ni >= n.len() {
            break;
        }
        if b.eq_ignore_ascii_case(&n[ni]) {
            ranges.push(i..i + 1);
            ni += 1;
        }
    }
    if ni < n.len() {
        // Couldn't match every needle char -- abandon the highlights;
        // the candidate stays (host kept the framework's verdict).
        Vec::new()
    } else {
        ranges
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::test_helpers::{
        app_in_command_mode, app_with, invoke_motion, submit_ex, unique_tempdir,
    };
    use crate::app::*;

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
    fn enter_command_line_clears_buffer_and_sets_modal() {
        let mut a = app_with("abc", 10);
        a.editor.command_line = "stale".into();
        a.editor.last_message = Some(EchoMessage {
            text: "stale".into(),
            level: EchoLevel::Info,
        });
        a.apply(Action::EnterCommandLine);
        assert_eq!(a.editor.modal, ModalState::Command);
        assert_eq!(a.editor.command_line, "");
        assert!(a.editor.last_message.is_none());
    }

    #[test]
    fn command_line_append_pushes_chars() {
        let mut a = app_with("", 10);
        a.apply(Action::EnterCommandLine);
        a.apply(Action::CommandLineAppend('w'));
        a.apply(Action::CommandLineAppend('q'));
        assert_eq!(a.editor.command_line, "wq");
    }

    #[test]
    fn command_line_backspace_pops_chars() {
        let mut a = app_with("", 10);
        a.apply(Action::EnterCommandLine);
        a.apply(Action::CommandLineAppend('w'));
        a.apply(Action::CommandLineAppend('q'));
        a.apply(Action::CommandLineBackspace);
        assert_eq!(a.editor.command_line, "w");
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
        assert_eq!(a.editor.command_line, "");
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
        assert!(a.document.dirty());

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
        assert!(a.document.dirty());

        a.apply(Action::EnterCommandLine);
        for c in format!("w {}", path.display()).chars() {
            a.apply(Action::CommandLineAppend(c));
        }
        a.apply(Action::CommandLineSubmit);

        assert!(!a.document.dirty());
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
        assert_eq!(a.editor.command_line, "set nonumber");
    }

    #[test]
    fn up_then_up_walks_to_older() {
        let mut a = app_with("hello", 10);
        submit_ex(&mut a, "set number");
        submit_ex(&mut a, "set nonumber");
        a.apply(Action::EnterCommandLine);
        a.apply(Action::CommandLineHistoryPrev);
        a.apply(Action::CommandLineHistoryPrev);
        assert_eq!(a.editor.command_line, "set number");
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
        assert_eq!(a.editor.command_line, "set number");
        // Down returns to "se".
        a.apply(Action::CommandLineHistoryNext);
        assert_eq!(a.editor.command_line, "se");
    }

    #[test]
    fn history_navigation_with_no_history_is_no_op() {
        let mut a = app_with("hello", 10);
        a.apply(Action::EnterCommandLine);
        a.apply(Action::CommandLineAppend('w'));
        a.apply(Action::CommandLineHistoryPrev);
        assert_eq!(a.editor.command_line, "w");
    }

    #[test]
    fn history_persists_across_command_sessions() {
        let mut a = app_with("hello", 10);
        submit_ex(&mut a, "set number");
        // Reopen command line; Up should still recall.
        a.apply(Action::EnterCommandLine);
        a.apply(Action::CommandLineHistoryPrev);
        assert_eq!(a.editor.command_line, "set number");
    }

    #[test]
    fn tab_in_command_mode_opens_completion_popup() {
        let mut a = app_in_command_mode("descri");
        a.apply(Action::CommandLineCompleteOrAdvance);
        let state = a.completion_state.as_ref().expect("popup should open");
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
        assert_eq!(a.completion_state.as_ref().unwrap().selected, 1);
    }

    #[test]
    fn chord_capture_active_only_when_in_chord_arg_slot() {
        let mut a = app_with("xx", 10);
        a.editor.modal = ModalState::Command;
        // Empty cmdline -> CommandName slot, not chord-capture.
        a.editor.command_line = String::new();
        assert!(!a.chord_capture_active());
        // Mid command-name slot.
        a.editor.command_line = "describe-key".into();
        assert!(!a.chord_capture_active());
        // Now the cursor is past the space; arg slot is `chord`
        // with kind=Chord -> capture is active.
        a.editor.command_line = "describe-key ".into();
        assert!(a.chord_capture_active());
        // describe-command's first arg is String, NOT Chord ->
        // no capture even though we're in an arg slot.
        a.editor.command_line = "describe-command ".into();
        assert!(!a.chord_capture_active());
        // Outside Command modal, never active.
        a.editor.modal = ModalState::Normal;
        a.editor.command_line = "describe-key ".into();
        assert!(!a.chord_capture_active());
    }

    #[test]
    fn chord_capture_active_for_canonical_command_name() {
        // `:ex:describe-key ` (canonical, not the alias). The slot
        // detector tries `id_by_name` first and only falls back
        // to alias-expand, so both forms switch into chord-capture.
        let mut a = app_with("xx", 10);
        a.editor.modal = ModalState::Command;
        a.editor.command_line = "ex:describe-key ".into();
        assert!(a.chord_capture_active());
    }

    #[test]
    fn empty_submit_of_describe_key_arms_chord_prompt() {
        // User typed `:describe-key<CR>` with no arg. The required
        // Chord arg is missing -- we shouldn't error; we should
        // prefill the cmdline and arm auto-submit.
        let mut a = app_in_command_mode("describe-key");
        a.apply(Action::CommandLineSubmit);
        assert_eq!(a.editor.command_line, "describe-key ");
        assert!(a.editor.auto_submit_after_chord);
        assert!(matches!(a.editor.modal, ModalState::Command));
    }

    #[test]
    fn empty_submit_of_canonical_describe_key_arms_chord_prompt() {
        // Same prompt path through the canonical name, not just
        // the alias.
        let mut a = app_in_command_mode("ex:describe-key");
        a.apply(Action::CommandLineSubmit);
        assert_eq!(a.editor.command_line, "ex:describe-key ");
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
        assert_eq!(a.editor.command_line, "describe-command ");
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
        assert_eq!(a.editor.command_line, "apropos ");
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
        let state = a.completion_state.as_ref().expect("popup should open");
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
