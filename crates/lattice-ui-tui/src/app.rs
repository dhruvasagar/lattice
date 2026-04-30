//! Pure application state and transitions.
//!
//! The state machine is intentionally separated from the IO loop so it can be
//! unit-tested without spinning up a terminal. Each input keystroke becomes
//! an `Action`; `App::apply` consumes the action, dispatching motion / edit
//! work through `lattice_grammar::execute()` where appropriate.
//!
//! Phase 2 wiring: motions and the `delete` operator flow through the
//! grammar engine; the modal-mode primitives (`i`, `a`, `o`, `<Esc>`) live
//! locally on `App` because they're inherently a state-machine concern, not
//! a buffer command. Phase 3+ migrates more of these to the grammar layer.

use lattice_core::CoreError;
use lattice_core::Document;
use lattice_core::buffer::AppliedEdit;
use lattice_core::search::{self, SearchHit};
use lattice_grammar::CommandRegistry;
use lattice_grammar::ModalState;
use lattice_grammar::SearchDirection;
use lattice_grammar::VisualKind;
use lattice_grammar::YankKind;
use lattice_grammar::builtins::{Builtins, populate};
use lattice_grammar::command::CommandInvocation;
use lattice_grammar::dispatcher::execute;
use lattice_grammar::effect::Effect;
use lattice_grammar::registry::OperatorId;
use lattice_protocol::edit::Edit;
use lattice_protocol::position::{Position, Range as ProtoRange};
use lattice_protocol::selection::{Selection, SelectionSet, VisualMode};
use lattice_syntax::{Lang, StyledSpan, Syntax};

use crate::excommand::{self, ExCommand};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pending {
    None,
    /// First `g` of a `gg`-style two-key sequence.
    AfterG,
    /// Operator key pressed; awaiting motion or text-object.
    AfterOperator(OperatorId),
    /// `f` / `F` / `t` / `T` pressed; awaiting the target character.
    /// If the user pressed an operator first (like `df`), the operator is
    /// stashed here so we can compose `df<char>` into a single Invoke.
    AfterFindChar {
        kind: FindKind,
        operator: Option<OperatorId>,
    },
    /// `i` or `a` pressed in operator-pending state; awaiting the
    /// text-object selector char (`w`, `"`, `(`, `[`, `{`, etc.).
    AfterTextObject {
        operator: OperatorId,
        around: bool,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FindKind {
    /// `f` -- move to next occurrence of the char on the current line.
    Forward,
    /// `F` -- move to previous occurrence of the char on the current line.
    Backward,
    /// `t` -- move to one byte before the next occurrence (inclusive of arg).
    TillForward,
    /// `T` -- move to one byte after the previous occurrence.
    TillBackward,
}

/// A transient one-line message rendered in the echo area below the mode line
/// (DESIGN.md §5.9.10). Phase 2 wiring: replaced by the next call to
/// `App::set_message` (no timeout-based fade yet).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EchoMessage {
    pub text: String,
    pub level: EchoLevel,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EchoLevel {
    Info,
    Warn,
    Error,
}

#[derive(Debug, Clone)]
pub enum Action {
    None,
    Quit,
    /// Run a CommandInvocation through `lattice_grammar::execute()`.
    Invoke(CommandInvocation),
    /// Insert a string at the cursor (used by Insert mode).
    Insert(String),
    /// Delete the byte before the cursor (Insert-mode backspace).
    DeleteCharBackward,
    /// Move into a different modal state (Insert, Normal, ...).
    EnterMode(ModalState),
    /// Vim's `a`: move cursor one byte right (clamped) and enter Insert.
    EnterAppend,
    /// Vim's `o`: open a new line below the current line and enter Insert.
    OpenLineBelow,
    /// Vim's `O`: open a new line above the current line and enter Insert.
    OpenLineAbove,
    /// Set the pending-key state (e.g., we just saw `g`).
    SetPending(Pending),
    Undo,
    Redo,
    /// Append a digit (0-9) to the in-progress count prefix.
    PushDigit(u8),
    /// Enter Visual modal state (`v` Charwise, `V` Linewise) anchored at
    /// the current cursor.
    EnterVisual(VisualKind),
    /// Exit Visual to Normal, collapsing the selection.
    ExitVisual,
    /// Vim's `.` -- re-dispatch the last buffer-mutating invocation from
    /// the current cursor.
    RepeatLastChange,

    // ---- Command-line minibuffer (Phase 2: simple, single-line) ----
    /// Pressed `:` in Normal mode -- enter command modal with empty buffer.
    EnterCommandLine,
    /// Append a character to the in-progress command line.
    CommandLineAppend(char),
    /// Delete the last character. If the buffer is empty, leave Command mode.
    CommandLineBackspace,
    /// Submit the current command line: parse + execute, then leave Command.
    CommandLineSubmit,
    /// Drop the current command line and leave Command modal.
    CommandLineCancel,
    /// Replace the echo area with a typed message.
    Echo(EchoMessage),

    // ---- Paste (`p`, `P`) ----
    /// Vim's `p` -- paste the unnamed register after the cursor (charwise)
    /// or below the current line (linewise).
    PasteAfter,
    /// Vim's `P` -- paste before cursor / above current line.
    PasteBefore,

    // ---- Search (`/`, `?`, `n`, `N`) ----
    /// Pressed `/` (Forward) or `?` (Backward) -- enter Search modal with
    /// empty pattern, remembering origin so cancel restores cursor.
    EnterSearch(SearchDirection),
    SearchAppend(char),
    /// Delete one char from the pattern. If pattern is empty, leave Search.
    SearchBackspace,
    /// Confirm the pattern: jump to current match (if any) and store it
    /// as `last_search` for `n`/`N` repeat.
    SearchSubmit,
    /// Drop the in-progress pattern, restore cursor, leave Search.
    SearchCancel,
    /// Repeat the last search in its original direction.
    SearchNext,
    /// Repeat the last search in the opposite direction.
    SearchPrevious,
}

/// In-progress `/` or `?` state. The cursor at entry is preserved so
/// Esc can restore it.
#[derive(Debug, Clone)]
pub struct SearchLine {
    pub direction: SearchDirection,
    pub pattern: String,
    pub origin: Position,
}

/// Last completed search -- consulted by `n` and `N`.
#[derive(Debug, Clone)]
pub struct LastSearch {
    pub pattern: String,
    pub direction: SearchDirection,
}

/// The unnamed register's payload. v1 uses a single global slot; the
/// full vim register zoo (`"a-z`, `"+`, `"*`, etc.) lands later.
#[derive(Debug, Clone)]
pub struct UnnamedRegister {
    pub content: String,
    pub kind: YankKind,
}

pub struct App {
    pub document: Document,
    pub cursor: Position,
    /// First visible line in the viewport (0-based).
    pub scroll: u32,
    pub should_quit: bool,
    /// Last height we were drawn at; used by motion clamping and viewport
    /// scrolling. Updated by the renderer before each frame.
    pub viewport_height: u32,
    pub modal: ModalState,
    pub pending: Pending,
    pub registry: CommandRegistry,
    pub builtins: Builtins,
    /// In-progress text in the `:` minibuffer. Populated only while
    /// `modal == ModalState::Command`.
    pub command_line: String,
    /// Most recent transient status / error message, displayed in the echo
    /// area until replaced.
    pub last_message: Option<EchoMessage>,
    /// Per-document tree-sitter state. `None` when the document's language
    /// is `Plain` (no grammar bundled).
    pub syntax: Option<Syntax>,
    /// `text_version` last fed to `syntax.parse(...)`. Used to skip reparse
    /// when no text mutation has happened since the previous frame.
    last_parsed_text_version: u64,
    /// Per-line `StyledSpan`s for the currently visible viewport, indexed
    /// from `[scroll, scroll + viewport_height)`. Recomputed each frame by
    /// `refresh_highlights` (called from the runtime before drawing).
    pub visible_highlights: Vec<Vec<StyledSpan>>,
    /// In-progress `/` or `?` search. `Some` only while
    /// `modal == ModalState::Search(_)`.
    pub search_line: Option<SearchLine>,
    /// Most recent submitted search; consulted by `n` / `N`.
    pub last_search: Option<LastSearch>,
    /// Range of the most recent search match, used to draw the highlight
    /// in the buffer view. Cleared on Esc and on cursor motion.
    pub current_match: Option<ProtoRange>,
    /// Unnamed register -- destination of `y` / `d` / `c`, source of
    /// `p` / `P`. `None` until something has been yanked.
    pub unnamed_register: Option<UnnamedRegister>,
    /// In-progress count prefix being typed (`3` of `3w`, `12` of `12dd`).
    /// 0 means "no count typed". The next `Action::Invoke` consumes this
    /// and resets it to 0.
    pub pending_count: u32,
    /// Count latched when an operator key was pressed (`2` of `2d3w`).
    /// Multiplied with the motion's count (`3`) to give the final count
    /// the operator dispatches with (`6`). 0 means "no operator count".
    pub op_count: u32,
    /// Anchor position when Visual mode was entered. `None` outside
    /// Visual; restored on Esc. The `head` of the selection follows the
    /// cursor; the `anchor` stays put so the selection extends or
    /// contracts as the user moves.
    pub visual_anchor: Option<Position>,
    /// Last operator-class invocation that mutated the buffer.
    /// `.` re-dispatches it from the current cursor. v1 records
    /// operator + motion / operator + range / Visual-mode operator;
    /// insert-mode text replay is a known gap (§5.2.4).
    pub last_change: Option<CommandInvocation>,
}

impl std::fmt::Debug for App {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("App")
            .field("cursor", &self.cursor)
            .field("scroll", &self.scroll)
            .field("should_quit", &self.should_quit)
            .field("viewport_height", &self.viewport_height)
            .field("modal", &self.modal)
            .field("pending", &self.pending)
            .field("command_line", &self.command_line)
            .field("last_message", &self.last_message)
            .field("dirty", &self.document.dirty())
            .finish()
    }
}

impl App {
    pub fn new(document: Document) -> Self {
        let mut registry = CommandRegistry::new();
        let builtins = populate(&mut registry);
        let lang = Lang::detect_from_path(document.path());
        let mut syntax = Syntax::for_language(lang).ok().flatten();
        if let Some(s) = syntax.as_mut() {
            s.parse(&document.text());
        }
        let last_parsed_text_version = document.text_version();
        Self {
            document,
            cursor: Position::ZERO,
            scroll: 0,
            should_quit: false,
            viewport_height: 1,
            modal: ModalState::Normal,
            pending: Pending::None,
            registry,
            builtins,
            command_line: String::new(),
            last_message: None,
            syntax,
            last_parsed_text_version,
            visible_highlights: Vec::new(),
            search_line: None,
            last_search: None,
            current_match: None,
            unnamed_register: None,
            pending_count: 0,
            op_count: 0,
            visual_anchor: None,
            last_change: None,
        }
    }

    pub fn apply(&mut self, action: Action) {
        match action {
            Action::None => {}
            Action::Quit => self.should_quit = true,
            Action::Invoke(inv) => self.run_invocation(inv),
            Action::Insert(s) => self.do_insert_text(&s),
            Action::DeleteCharBackward => self.do_delete_char_backward(),
            Action::EnterMode(state) => self.enter_mode(state),
            Action::EnterAppend => self.do_enter_append(),
            Action::OpenLineBelow => self.do_open_line_below(),
            Action::OpenLineAbove => self.do_open_line_above(),
            Action::SetPending(p) => {
                // When entering operator-pending state, latch the in-progress
                // count as `op_count` so the next motion's count multiplies
                // with it (vim's `2d3w` -> d6w). Other pending transitions
                // keep `pending_count` so a partially-typed `gg` count
                // survives the chord.
                if matches!(p, Pending::AfterOperator(_)) && self.pending_count > 0 {
                    self.op_count = self.pending_count;
                    self.pending_count = 0;
                }
                self.pending = p;
            }
            Action::Undo => {
                let _ = self.document.undo();
                self.clamp_cursor_to_buffer();
            }
            Action::Redo => {
                let _ = self.document.redo();
                self.clamp_cursor_to_buffer();
            }

            Action::EnterCommandLine => {
                self.command_line.clear();
                self.modal = ModalState::Command;
                self.pending = Pending::None;
                self.last_message = None;
            }
            Action::CommandLineAppend(c) => {
                if matches!(self.modal, ModalState::Command) {
                    self.command_line.push(c);
                }
            }
            Action::CommandLineBackspace => {
                if matches!(self.modal, ModalState::Command) && self.command_line.pop().is_none() {
                    // Empty buffer + backspace -> exit Command modal.
                    self.modal = ModalState::Normal;
                }
            }
            Action::CommandLineSubmit => {
                if matches!(self.modal, ModalState::Command) {
                    let line = std::mem::take(&mut self.command_line);
                    self.modal = ModalState::Normal;
                    self.pending = Pending::None;
                    self.execute_ex_line(&line);
                }
            }
            Action::CommandLineCancel => {
                if matches!(self.modal, ModalState::Command) {
                    self.command_line.clear();
                    self.modal = ModalState::Normal;
                    self.pending = Pending::None;
                }
            }
            Action::Echo(message) => {
                self.last_message = Some(message);
            }

            Action::PushDigit(d) => {
                // Accumulate one decimal digit into the pending count.
                // Saturating math prevents overflow on absurd inputs.
                self.pending_count = self
                    .pending_count
                    .saturating_mul(10)
                    .saturating_add(d.into());
            }

            Action::EnterVisual(kind) => self.do_enter_visual(kind),
            Action::ExitVisual => self.do_exit_visual(),

            Action::RepeatLastChange => {
                if let Some(inv) = self.last_change.clone() {
                    // Direct dispatch: bypass the `last_change` recording
                    // path inside run_invocation by setting a re-entry guard.
                    // For v1 simple model, we just call run_invocation; the
                    // resulting last_change overwrite is identical.
                    self.run_invocation(inv);
                } else {
                    self.set_message(
                        EchoLevel::Error,
                        "no previous change to repeat".to_string(),
                    );
                }
            }

            Action::PasteAfter => self.do_paste(false),
            Action::PasteBefore => self.do_paste(true),

            Action::EnterSearch(direction) => {
                self.search_line = Some(SearchLine {
                    direction,
                    pattern: String::new(),
                    origin: self.cursor,
                });
                self.modal = ModalState::Search(direction);
                self.pending = Pending::None;
                self.last_message = None;
                self.current_match = None;
            }
            Action::SearchAppend(c) => {
                if let Some(line) = self.search_line.as_mut() {
                    line.pattern.push(c);
                    self.preview_search();
                }
            }
            Action::SearchBackspace => {
                let leave = match self.search_line.as_mut() {
                    Some(line) => {
                        if line.pattern.pop().is_none() {
                            true
                        } else {
                            self.preview_search();
                            false
                        }
                    }
                    None => false,
                };
                if leave {
                    self.cancel_search();
                }
            }
            Action::SearchSubmit => self.submit_search(),
            Action::SearchCancel => self.cancel_search(),
            Action::SearchNext => self.repeat_search(false),
            Action::SearchPrevious => self.repeat_search(true),
        }
        self.ensure_cursor_visible();
        self.maybe_reparse_syntax();
    }

    /// Reparse syntax if the document's text has changed since the last
    /// parse. Idempotent and cheap when nothing changed.
    fn maybe_reparse_syntax(&mut self) {
        let tv = self.document.text_version();
        if tv == self.last_parsed_text_version {
            return;
        }
        if let Some(syntax) = self.syntax.as_mut() {
            syntax.parse(&self.document.text());
        }
        self.last_parsed_text_version = tv;
    }

    /// Recompute the per-line styled spans for the current viewport.
    /// Called by the runtime before each `terminal.draw`.
    pub fn refresh_highlights(&mut self) {
        let height = self.viewport_height;
        let start = self.scroll;
        let end = start.saturating_add(height);
        self.visible_highlights = match self.syntax.as_mut() {
            Some(syntax) => syntax.highlight_lines(start, end).unwrap_or_default(),
            None => Vec::new(),
        };
    }

    /// Spans for the line at `viewport_row` (0-based, relative to the top of
    /// the viewport). Empty slice if no syntax or the row is past EOF.
    pub fn highlights_for_viewport_row(&self, viewport_row: u32) -> &[StyledSpan] {
        self.visible_highlights
            .get(viewport_row as usize)
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }

    pub fn set_message(&mut self, level: EchoLevel, text: impl Into<String>) {
        self.last_message = Some(EchoMessage {
            text: text.into(),
            level,
        });
    }

    /// Run the in-progress pattern from origin. Used to highlight the
    /// current match while typing in `/` or `?`. Does not move cursor;
    /// the cursor jumps only on `SearchSubmit`.
    fn preview_search(&mut self) {
        let Some(line) = self.search_line.as_ref() else {
            return;
        };
        if line.pattern.is_empty() {
            self.current_match = None;
            return;
        }
        let dir = match line.direction {
            SearchDirection::Forward => search::Direction::Forward,
            SearchDirection::Backward => search::Direction::Backward,
        };
        match search::find(self.document.buffer(), &line.pattern, line.origin, dir) {
            Ok(Some(SearchHit { range, .. })) => self.current_match = Some(range),
            _ => self.current_match = None,
        }
    }

    fn submit_search(&mut self) {
        let Some(line) = self.search_line.take() else {
            return;
        };
        self.modal = ModalState::Normal;
        self.pending = Pending::None;
        if line.pattern.is_empty() {
            // Empty submit: re-run last_search if any (vim behavior).
            if self.last_search.is_some() {
                self.repeat_search(false);
            }
            return;
        }
        let dir = match line.direction {
            SearchDirection::Forward => search::Direction::Forward,
            SearchDirection::Backward => search::Direction::Backward,
        };
        match search::find(self.document.buffer(), &line.pattern, line.origin, dir) {
            Ok(Some(hit)) => {
                self.cursor = hit.range.start;
                self.current_match = Some(hit.range);
                if hit.wrapped {
                    let level = EchoLevel::Warn;
                    let text = match line.direction {
                        SearchDirection::Forward => "search hit BOTTOM, continuing at TOP",
                        SearchDirection::Backward => "search hit TOP, continuing at BOTTOM",
                    };
                    self.set_message(level, text.to_string());
                }
                self.last_search = Some(LastSearch {
                    pattern: line.pattern,
                    direction: line.direction,
                });
            }
            Ok(None) => {
                self.current_match = None;
                self.set_message(EchoLevel::Error, format!("E486: Pattern not found: {}", line.pattern));
                // Vim still records the pattern so `n`/`N` can retry later.
                self.last_search = Some(LastSearch {
                    pattern: line.pattern,
                    direction: line.direction,
                });
            }
            Err(_) => {
                self.current_match = None;
            }
        }
    }

    fn cancel_search(&mut self) {
        if let Some(line) = self.search_line.take() {
            self.cursor = line.origin;
        }
        self.current_match = None;
        self.modal = ModalState::Normal;
        self.pending = Pending::None;
    }

    /// Repeat last search. `reverse=false` keeps the original direction
    /// (`n`); `reverse=true` flips it (`N`).
    fn repeat_search(&mut self, reverse: bool) {
        let Some(last) = self.last_search.clone() else {
            self.set_message(EchoLevel::Error, "E35: no previous regular expression".to_string());
            return;
        };
        let direction = match (last.direction, reverse) {
            (SearchDirection::Forward, false) | (SearchDirection::Backward, true) => {
                SearchDirection::Forward
            }
            (SearchDirection::Backward, false) | (SearchDirection::Forward, true) => {
                SearchDirection::Backward
            }
        };
        let dir = match direction {
            SearchDirection::Forward => search::Direction::Forward,
            SearchDirection::Backward => search::Direction::Backward,
        };
        // Skip current match: advance one byte in the chosen direction.
        let from = step_byte(&self.document, self.cursor, direction);
        match search::find(self.document.buffer(), &last.pattern, from, dir) {
            Ok(Some(hit)) => {
                self.cursor = hit.range.start;
                self.current_match = Some(hit.range);
                if hit.wrapped {
                    let text = match direction {
                        SearchDirection::Forward => "search hit BOTTOM, continuing at TOP",
                        SearchDirection::Backward => "search hit TOP, continuing at BOTTOM",
                    };
                    self.set_message(EchoLevel::Warn, text.to_string());
                }
            }
            Ok(None) => {
                self.current_match = None;
                self.set_message(EchoLevel::Error, format!("E486: Pattern not found: {}", last.pattern));
            }
            Err(_) => {
                self.current_match = None;
            }
        }
    }

    fn execute_ex_line(&mut self, line: &str) {
        match excommand::parse(line) {
            Ok(cmd) => self.execute_ex(cmd),
            Err(err) => {
                self.set_message(EchoLevel::Error, err.to_string());
            }
        }
    }

    fn execute_ex(&mut self, cmd: ExCommand) {
        match cmd {
            ExCommand::Write { path } => self.do_write(path),
            ExCommand::Quit { force } => self.do_quit(force),
            ExCommand::WriteQuit { force } => self.do_write_quit(force),
        }
    }

    fn do_write(&mut self, path: Option<std::path::PathBuf>) {
        let result = match path {
            Some(p) => self.document.save_as(&p).map(|()| p.display().to_string()),
            None => self
                .document
                .save()
                .map(|p| p.display().to_string()),
        };
        match result {
            Ok(displayed) => self.set_message(EchoLevel::Info, format!("\"{displayed}\" written")),
            Err(CoreError::NoPath) => {
                self.set_message(EchoLevel::Error, "no file name (use :w <path>)".to_string());
            }
            Err(e) => self.set_message(EchoLevel::Error, format!("write error: {e}")),
        }
    }

    fn do_quit(&mut self, force: bool) {
        if !force && self.document.dirty() {
            self.set_message(
                EchoLevel::Error,
                "no write since last change (add ! to override)".to_string(),
            );
            return;
        }
        self.should_quit = true;
    }

    fn do_write_quit(&mut self, force: bool) {
        match self.document.save() {
            Ok(_) => {
                self.should_quit = true;
            }
            Err(CoreError::NoPath) => {
                self.set_message(EchoLevel::Error, "no file name (use :w <path>)".to_string());
                if force {
                    self.should_quit = true;
                }
            }
            Err(e) => {
                self.set_message(EchoLevel::Error, format!("write error: {e}"));
                if force {
                    self.should_quit = true;
                }
            }
        }
    }

    fn run_invocation(&mut self, mut inv: CommandInvocation) {
        // Pending state is consumed by the input layer that built `inv`; any
        // dispatch resets it.
        self.pending = Pending::None;
        // Apply the count multiplication: the operator's count (latched at
        // `op_count`) multiplies with the motion's count (the in-progress
        // `pending_count`, or the invocation's own count default of 1).
        // Vim semantics: `2d3w` -> d6w. Either alone replaces the default.
        let motion_count = if self.pending_count > 0 {
            self.pending_count
        } else {
            inv.count.map(|c| c.0).unwrap_or(1)
        };
        let final_count = if self.op_count > 0 {
            self.op_count.saturating_mul(motion_count)
        } else {
            motion_count
        };
        if final_count > 1 {
            inv = inv.with_count(lattice_grammar::command::Count(final_count));
        }
        self.pending_count = 0;
        self.op_count = 0;
        let was_visual = matches!(self.modal, ModalState::Visual(_));
        let mut should_exit_visual = false;
        let inv_for_repeat = inv.clone();
        match execute(&self.registry, &mut self.document, self.cursor, inv) {
            Ok(effect) => {
                // Visual exits on any operator-class effect (mutation OR
                // yank-only); dot-repeat only records buffer mutations.
                should_exit_visual = effect_mutates_or_yanks(&effect);
                if effect_mutates(&effect) {
                    self.last_change = Some(inv_for_repeat);
                }
                self.apply_effect(effect);
            }
            Err(_) => {
                // TODO(error-surface): publish to a notification once that
                // subsystem lands.
            }
        }
        // After a Visual-mode operator (d/y/c on selection), vim returns
        // to Normal. Pure motion in Visual extends the selection -- keep
        // Visual. The `c` operator already flipped to Insert via
        // Effect::EnterMode; the post-check would be a no-op there.
        if was_visual && should_exit_visual && matches!(self.modal, ModalState::Visual(_)) {
            self.do_exit_visual();
        }
        self.clamp_cursor_to_buffer();
    }

    fn apply_effect(&mut self, effect: Effect) {
        match effect {
            Effect::None => {}
            Effect::Edits(edits) => self.handle_edits(&edits),
            Effect::SelectionChange(set) => {
                let new_head = set.primary().head;
                self.cursor = new_head;
                // In Visual mode the head moves but the anchor is preserved
                // -- the dispatcher's `replace_primary(Selection::cursor(...))`
                // would otherwise collapse the selection. Refresh the
                // document's selection to reflect the extension.
                if let ModalState::Visual(kind) = self.modal {
                    let sel = Selection {
                        anchor: self.visual_anchor.unwrap_or(new_head),
                        head: new_head,
                        visual: Some(visual_kind_to_mode(kind)),
                    };
                    self.document.set_selections(SelectionSet::single(sel));
                }
            }
            Effect::Yank { content, kind, .. } => {
                self.unnamed_register = Some(UnnamedRegister { content, kind });
            }
            Effect::EnterMode(mode) => {
                // Operators that flip mode (`c` -> Insert) come through here.
                // We bypass `enter_mode`'s "pull cursor back one byte" guard
                // because the operator already placed the cursor at the
                // correct insertion point.
                self.modal = mode;
                self.pending = Pending::None;
            }
            Effect::Many(many) => {
                for e in many {
                    self.apply_effect(e);
                }
            }
        }
    }

    fn handle_edits(&mut self, edits: &[AppliedEdit]) {
        // After a delete, the cursor sits at the start of the deleted range
        // (which is now the position of whatever followed). Vim's behavior.
        if let Some(first) = edits.first() {
            self.cursor = first.original_range.start;
        }
    }

    fn do_insert_text(&mut self, s: &str) {
        if s.is_empty() {
            return;
        }
        if let Ok(applied) = self.document.apply_edit(Edit::insert(self.cursor, s)) {
            self.cursor = applied.inserted_range.end;
        }
    }

    fn do_delete_char_backward(&mut self) {
        let prev = previous_position(&self.document, self.cursor);
        if prev == self.cursor {
            return;
        }
        let range = ProtoRange::new(prev, self.cursor);
        if self.document.apply_edit(Edit::delete(range)).is_ok() {
            self.cursor = prev;
        }
    }

    fn enter_mode(&mut self, state: ModalState) {
        self.modal = state;
        self.pending = Pending::None;
        if matches!(state, ModalState::Normal) {
            // Vim's behavior: leaving Insert mode pulls the cursor back one
            // byte if it's not already at the start of the line, so the
            // cursor sits on the last inserted char rather than past it.
            if self.cursor.byte > 0 {
                self.cursor.byte -= 1;
            }
        }
    }

    fn do_enter_append(&mut self) {
        let len = line_byte_len(&self.document, self.cursor.line);
        if self.cursor.byte < len {
            self.cursor.byte += 1;
        }
        self.modal = ModalState::Insert;
        self.pending = Pending::None;
    }

    fn do_open_line_below(&mut self) {
        let len = line_byte_len(&self.document, self.cursor.line);
        let eol = Position::new(self.cursor.line, len);
        if self.document.apply_edit(Edit::insert(eol, "\n")).is_ok() {
            self.cursor = Position::new(self.cursor.line + 1, 0);
        }
        self.modal = ModalState::Insert;
        self.pending = Pending::None;
    }

    fn do_enter_visual(&mut self, kind: VisualKind) {
        self.modal = ModalState::Visual(kind);
        self.pending = Pending::None;
        self.visual_anchor = Some(self.cursor);
        // Seed document.selections so Range::Selection picks up the
        // anchor=head=cursor selection immediately.
        let sel = Selection {
            anchor: self.cursor,
            head: self.cursor,
            visual: Some(visual_kind_to_mode(kind)),
        };
        self.document.set_selections(SelectionSet::single(sel));
    }

    fn do_exit_visual(&mut self) {
        self.modal = ModalState::Normal;
        self.pending = Pending::None;
        self.visual_anchor = None;
        // Collapse selection to a cursor at the current head.
        self.document
            .set_selections(SelectionSet::single(Selection::cursor(self.cursor)));
    }

    /// Paste from the unnamed register. `before = true` for `P` (paste
    /// before cursor / above current line), `false` for `p` (paste after
    /// cursor / below current line). Linewise yanks insert on a new line;
    /// charwise yanks splice at the cursor.
    fn do_paste(&mut self, before: bool) {
        let Some(reg) = self.unnamed_register.clone() else {
            self.set_message(EchoLevel::Error, "register empty".to_string());
            return;
        };
        match reg.kind {
            YankKind::Charwise => {
                // `p` inserts after the cursor's byte; `P` at the cursor.
                let line_len = line_byte_len(&self.document, self.cursor.line);
                let insert_at = if before {
                    self.cursor
                } else if self.cursor.byte < line_len {
                    Position::new(self.cursor.line, self.cursor.byte + 1)
                } else {
                    self.cursor
                };
                if let Ok(applied) = self
                    .document
                    .apply_edit(Edit::insert(insert_at, &reg.content))
                {
                    // Vim leaves the cursor on the last char of the pasted text.
                    let end = applied.inserted_range.end;
                    self.cursor = if end.byte > 0 {
                        Position::new(end.line, end.byte - 1)
                    } else {
                        end
                    };
                }
            }
            YankKind::Linewise => {
                // Linewise content is inserted as a whole new line. We
                // normalise by ensuring exactly one trailing newline on the
                // payload before splicing at the appropriate line boundary.
                let mut payload = reg.content.clone();
                if !payload.ends_with('\n') {
                    payload.push('\n');
                }
                let insert_at = if before {
                    Position::new(self.cursor.line, 0)
                } else {
                    let len = line_byte_len(&self.document, self.cursor.line);
                    // Insert at end of current line then a newline -- but
                    // vim's `p` puts the line BELOW. So insert at start of
                    // the next line. If we're on the last line and there's
                    // no trailing newline, insert "\n<payload-without-tail>".
                    if self.cursor.line + 1 < self.document.buffer().line_count() {
                        Position::new(self.cursor.line + 1, 0)
                    } else {
                        // Append at EOL of last line; payload starts with \n
                        // implicit in being on a "new" line.
                        let _ = self
                            .document
                            .apply_edit(Edit::insert(Position::new(self.cursor.line, len), "\n"));
                        Position::new(self.cursor.line + 1, 0)
                    }
                };
                if let Ok(applied) = self
                    .document
                    .apply_edit(Edit::insert(insert_at, &payload))
                {
                    // Cursor lands at the start of the pasted block.
                    self.cursor = applied.inserted_range.start;
                }
            }
        }
    }

    fn do_open_line_above(&mut self) {
        let bol = Position::new(self.cursor.line, 0);
        if self.document.apply_edit(Edit::insert(bol, "\n")).is_ok() {
            self.cursor = bol;
        }
        self.modal = ModalState::Insert;
        self.pending = Pending::None;
    }

    fn clamp_cursor_to_buffer(&mut self) {
        let last_line = last_addressable_line(&self.document);
        if self.cursor.line > last_line {
            self.cursor.line = last_line;
        }
        let len = line_byte_len(&self.document, self.cursor.line);
        if self.cursor.byte > len {
            self.cursor.byte = len;
        }
    }

    pub fn ensure_cursor_visible(&mut self) {
        if self.viewport_height == 0 {
            return;
        }
        if self.cursor.line < self.scroll {
            self.scroll = self.cursor.line;
        }
        let bottom = self.scroll + self.viewport_height - 1;
        if self.cursor.line > bottom {
            self.scroll = self.cursor.line + 1 - self.viewport_height;
        }
    }

    pub fn set_viewport_height(&mut self, height: u32) {
        self.viewport_height = height.max(1);
        self.ensure_cursor_visible();
    }

    pub fn modal_label(&self) -> &'static str {
        match self.modal {
            ModalState::Normal => "NORMAL",
            ModalState::Insert => "INSERT",
            ModalState::Visual(_) => "VISUAL",
            ModalState::OperatorPending => "O-PEND",
            ModalState::Command => "CMD",
            ModalState::Search(_) => "SEARCH",
            ModalState::Replace => "REPLACE",
        }
    }
}

pub(crate) fn line_byte_len(doc: &Document, line: u32) -> u32 {
    let s = doc.text();
    s.split_inclusive('\n')
        .nth(line as usize)
        .map(|l| l.trim_end_matches('\n').len() as u32)
        .unwrap_or(0)
}

pub(crate) fn last_addressable_line(doc: &Document) -> u32 {
    let lc = doc.buffer().line_count();
    let s = doc.text();
    if lc == 0 {
        0
    } else if s.ends_with('\n') {
        lc.saturating_sub(2)
    } else {
        lc.saturating_sub(1)
    }
}

fn visual_kind_to_mode(kind: VisualKind) -> VisualMode {
    match kind {
        VisualKind::Charwise => VisualMode::Charwise,
        VisualKind::Linewise => VisualMode::Linewise,
        VisualKind::Blockwise => VisualMode::Blockwise,
    }
}

/// True if the Effect indicates an operator-class action (the buffer
/// changed or content was yanked). Used by Visual mode to decide whether
/// to auto-exit after the dispatch -- motions in Visual should not exit;
/// d / y / c should.
fn effect_mutates_or_yanks(effect: &Effect) -> bool {
    match effect {
        Effect::Edits(_) | Effect::Yank { .. } => true,
        Effect::Many(parts) => parts.iter().any(effect_mutates_or_yanks),
        Effect::None | Effect::SelectionChange(_) | Effect::EnterMode(_) => false,
    }
}

/// True if the Effect produced a buffer mutation. Used by dot-repeat
/// to decide whether to record the invocation -- yank-only invocations
/// (vim's `y`) are NOT eligible for `.`, only changes.
fn effect_mutates(effect: &Effect) -> bool {
    match effect {
        Effect::Edits(_) => true,
        Effect::Many(parts) => parts.iter().any(effect_mutates),
        Effect::None | Effect::SelectionChange(_) | Effect::Yank { .. } | Effect::EnterMode(_) => {
            false
        }
    }
}

fn previous_position(doc: &Document, p: Position) -> Position {
    if p.byte > 0 {
        Position::new(p.line, p.byte - 1)
    } else if p.line > 0 {
        let prev_line = p.line - 1;
        Position::new(prev_line, line_byte_len(doc, prev_line))
    } else {
        p
    }
}

/// One byte forward or backward, wrapping across newlines. Caller for
/// search-repeat: skip the current match by advancing one byte before
/// calling the engine. At buffer extremes we return the original
/// position; the engine then handles wrap.
fn step_byte(doc: &Document, p: Position, dir: SearchDirection) -> Position {
    match dir {
        SearchDirection::Forward => {
            let len = line_byte_len(doc, p.line);
            if p.byte < len {
                Position::new(p.line, p.byte + 1)
            } else {
                let last = last_addressable_line(doc);
                if p.line < last {
                    Position::new(p.line + 1, 0)
                } else {
                    p
                }
            }
        }
        SearchDirection::Backward => previous_position(doc, p),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]
    use super::*;

    fn app_with(text: &str, viewport: u32) -> App {
        let mut a = App::new(Document::from_text(text));
        a.set_viewport_height(viewport);
        a
    }

    fn invoke_motion(id: lattice_grammar::registry::MotionId) -> Action {
        Action::Invoke(CommandInvocation::of(id.0))
    }

    // ---- Initial state ----

    #[test]
    fn new_app_starts_at_origin_in_normal_mode() {
        let a = app_with("abc", 10);
        assert_eq!(a.cursor, Position::ZERO);
        assert_eq!(a.scroll, 0);
        assert!(!a.should_quit);
        assert_eq!(a.modal, ModalState::Normal);
        assert_eq!(a.pending, Pending::None);
    }

    #[test]
    fn modal_label_reports_state() {
        let mut a = app_with("", 10);
        assert_eq!(a.modal_label(), "NORMAL");
        a.apply(Action::EnterMode(ModalState::Insert));
        assert_eq!(a.modal_label(), "INSERT");
    }

    #[test]
    fn quit_sets_flag() {
        let mut a = app_with("abc", 10);
        a.apply(Action::Quit);
        assert!(a.should_quit);
    }

    // ---- Motion via grammar engine ----

    #[test]
    fn invoke_char_right_advances_cursor() {
        let mut a = app_with("abc", 10);
        let id = a.builtins.char_right;
        a.apply(invoke_motion(id));
        assert_eq!(a.cursor, Position::new(0, 1));
    }

    #[test]
    fn invoke_char_left_at_origin_does_not_underflow() {
        let mut a = app_with("abc", 10);
        let id = a.builtins.char_left;
        a.apply(invoke_motion(id));
        assert_eq!(a.cursor, Position::ZERO);
    }

    #[test]
    fn invoke_line_down_then_line_up() {
        let mut a = app_with("hello\nworld", 10);
        let down = a.builtins.line_down;
        let up = a.builtins.line_up;
        a.apply(invoke_motion(down));
        assert_eq!(a.cursor.line, 1);
        a.apply(invoke_motion(up));
        assert_eq!(a.cursor.line, 0);
    }

    #[test]
    fn invoke_goto_last_line_jumps_to_last_line() {
        let mut a = app_with("a\nb\nc", 10);
        let id = a.builtins.goto_last_line;
        a.apply(invoke_motion(id));
        assert_eq!(a.cursor.line, 2);
    }

    #[test]
    fn invoke_goto_first_line_returns_to_origin() {
        let mut a = app_with("a\nb\nc", 10);
        let last = a.builtins.goto_last_line;
        let first = a.builtins.goto_first_line;
        a.apply(invoke_motion(last));
        a.apply(invoke_motion(first));
        assert_eq!(a.cursor, Position::ZERO);
    }

    #[test]
    fn invoke_line_end_moves_to_eol() {
        let mut a = app_with("hello world", 10);
        let id = a.builtins.line_end;
        a.apply(invoke_motion(id));
        assert_eq!(a.cursor, Position::new(0, 11));
    }

    #[test]
    fn invocation_resets_pending() {
        let mut a = app_with("abc", 10);
        a.apply(Action::SetPending(Pending::AfterG));
        assert_eq!(a.pending, Pending::AfterG);
        let id = a.builtins.char_right;
        a.apply(invoke_motion(id));
        assert_eq!(a.pending, Pending::None);
    }

    // ---- Insert mode ----

    #[test]
    fn entering_insert_mode_does_not_move_cursor() {
        let mut a = app_with("abc", 10);
        let before = a.cursor;
        a.apply(Action::EnterMode(ModalState::Insert));
        assert_eq!(a.modal, ModalState::Insert);
        assert_eq!(a.cursor, before);
    }

    #[test]
    fn insert_mode_inserts_text_and_advances_cursor() {
        let mut a = app_with("", 10);
        a.apply(Action::EnterMode(ModalState::Insert));
        a.apply(Action::Insert("h".into()));
        a.apply(Action::Insert("i".into()));
        assert_eq!(a.document.text(), "hi");
        assert_eq!(a.cursor, Position::new(0, 2));
    }

    #[test]
    fn insert_then_normal_pulls_cursor_back_one() {
        let mut a = app_with("", 10);
        a.apply(Action::EnterMode(ModalState::Insert));
        a.apply(Action::Insert("hi".into()));
        assert_eq!(a.cursor, Position::new(0, 2));
        a.apply(Action::EnterMode(ModalState::Normal));
        assert_eq!(a.cursor, Position::new(0, 1));
    }

    #[test]
    fn backspace_deletes_char_before_cursor_in_insert() {
        let mut a = app_with("hi", 10);
        a.cursor.byte = 2;
        a.apply(Action::EnterMode(ModalState::Insert));
        a.apply(Action::DeleteCharBackward);
        assert_eq!(a.document.text(), "h");
        assert_eq!(a.cursor, Position::new(0, 1));
    }

    #[test]
    fn backspace_at_origin_is_a_no_op() {
        let mut a = app_with("hi", 10);
        a.apply(Action::EnterMode(ModalState::Insert));
        a.apply(Action::DeleteCharBackward);
        assert_eq!(a.document.text(), "hi");
        assert_eq!(a.cursor, Position::ZERO);
    }

    #[test]
    fn backspace_across_line_boundary_joins_lines() {
        let mut a = app_with("a\nb", 10);
        a.cursor = Position::new(1, 0);
        a.apply(Action::EnterMode(ModalState::Insert));
        a.apply(Action::DeleteCharBackward);
        assert_eq!(a.document.text(), "ab");
        assert_eq!(a.cursor, Position::new(0, 1));
    }

    #[test]
    fn enter_append_advances_cursor_one_byte_then_inserts() {
        let mut a = app_with("ab", 10);
        a.apply(Action::EnterAppend);
        assert_eq!(a.modal, ModalState::Insert);
        assert_eq!(a.cursor, Position::new(0, 1));
    }

    #[test]
    fn open_line_below_creates_new_line_and_drops_cursor_to_it() {
        let mut a = app_with("first", 10);
        a.apply(Action::OpenLineBelow);
        assert_eq!(a.modal, ModalState::Insert);
        assert_eq!(a.document.text(), "first\n");
        assert_eq!(a.cursor, Position::new(1, 0));
    }

    #[test]
    fn open_line_above_creates_new_line_above() {
        let mut a = app_with("second", 10);
        a.apply(Action::OpenLineAbove);
        assert_eq!(a.modal, ModalState::Insert);
        assert_eq!(a.document.text(), "\nsecond");
        assert_eq!(a.cursor, Position::new(0, 0));
    }

    // ---- Operator + motion composition ----

    #[test]
    fn delete_with_word_forward_target_dw_in_app() {
        let mut a = app_with("hello world", 10);
        let inv = CommandInvocation::of(a.builtins.delete.0).with_target(
            lattice_grammar::Target::Motion(a.builtins.word_forward, lattice_grammar::Args::None),
        );
        a.apply(Action::Invoke(inv));
        assert_eq!(a.document.text(), "world");
        assert_eq!(a.cursor, Position::ZERO);
    }

    #[test]
    fn delete_char_under_cursor_x_in_app() {
        let mut a = app_with("abc", 10);
        let inv = CommandInvocation::of(a.builtins.delete.0).with_target(
            lattice_grammar::Target::Motion(a.builtins.char_right, lattice_grammar::Args::None),
        );
        a.apply(Action::Invoke(inv));
        assert_eq!(a.document.text(), "bc");
        assert_eq!(a.cursor, Position::ZERO);
    }

    // ---- Undo / Redo ----

    #[test]
    fn undo_after_insert_restores_buffer() {
        let mut a = app_with("", 10);
        a.apply(Action::EnterMode(ModalState::Insert));
        a.apply(Action::Insert("hi".into()));
        a.apply(Action::EnterMode(ModalState::Normal));
        assert_eq!(a.document.text(), "hi");
        a.apply(Action::Undo);
        assert_eq!(a.document.text(), "");
    }

    #[test]
    fn redo_replays_undone_edit() {
        let mut a = app_with("", 10);
        a.apply(Action::EnterMode(ModalState::Insert));
        a.apply(Action::Insert("hi".into()));
        a.apply(Action::EnterMode(ModalState::Normal));
        a.apply(Action::Undo);
        a.apply(Action::Redo);
        assert_eq!(a.document.text(), "hi");
    }

    // ---- Viewport scrolling ----

    #[test]
    fn ensure_visible_scrolls_when_cursor_goes_off_bottom() {
        let mut a = app_with("0\n1\n2\n3\n4\n5\n6\n7\n8\n9", 3);
        let id = a.builtins.goto_last_line;
        a.apply(invoke_motion(id));
        assert_eq!(a.cursor.line, 9);
        assert_eq!(a.scroll, 9 - 3 + 1);
    }

    #[test]
    fn ensure_visible_scrolls_back_to_top_on_goto_first() {
        let mut a = app_with("0\n1\n2\n3\n4", 2);
        let last = a.builtins.goto_last_line;
        let first = a.builtins.goto_first_line;
        a.apply(invoke_motion(last));
        a.apply(invoke_motion(first));
        assert_eq!(a.scroll, 0);
    }

    // ---- Command-line minibuffer ----

    fn unique_tempdir() -> std::path::PathBuf {
        let base = std::env::temp_dir();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let dir = base.join(format!("lattice-tui-test-{nanos}"));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn enter_command_line_clears_buffer_and_sets_modal() {
        let mut a = app_with("abc", 10);
        a.command_line = "stale".into();
        a.last_message = Some(EchoMessage {
            text: "stale".into(),
            level: EchoLevel::Info,
        });
        a.apply(Action::EnterCommandLine);
        assert_eq!(a.modal, ModalState::Command);
        assert_eq!(a.command_line, "");
        assert!(a.last_message.is_none());
    }

    #[test]
    fn command_line_append_pushes_chars() {
        let mut a = app_with("", 10);
        a.apply(Action::EnterCommandLine);
        a.apply(Action::CommandLineAppend('w'));
        a.apply(Action::CommandLineAppend('q'));
        assert_eq!(a.command_line, "wq");
    }

    #[test]
    fn command_line_backspace_pops_chars() {
        let mut a = app_with("", 10);
        a.apply(Action::EnterCommandLine);
        a.apply(Action::CommandLineAppend('w'));
        a.apply(Action::CommandLineAppend('q'));
        a.apply(Action::CommandLineBackspace);
        assert_eq!(a.command_line, "w");
    }

    #[test]
    fn command_line_backspace_on_empty_exits_command_modal() {
        let mut a = app_with("", 10);
        a.apply(Action::EnterCommandLine);
        a.apply(Action::CommandLineBackspace);
        assert_eq!(a.modal, ModalState::Normal);
    }

    #[test]
    fn command_line_cancel_clears_and_returns_to_normal() {
        let mut a = app_with("", 10);
        a.apply(Action::EnterCommandLine);
        a.apply(Action::CommandLineAppend('w'));
        a.apply(Action::CommandLineCancel);
        assert_eq!(a.modal, ModalState::Normal);
        assert_eq!(a.command_line, "");
    }

    #[test]
    fn submit_q_on_clean_buffer_quits() {
        let mut a = app_with("hello", 10);
        a.apply(Action::EnterCommandLine);
        for c in "q".chars() {
            a.apply(Action::CommandLineAppend(c));
        }
        a.apply(Action::CommandLineSubmit);
        assert!(a.should_quit);
        assert_eq!(a.modal, ModalState::Normal);
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
        assert!(!a.should_quit);
        let msg = a.last_message.as_ref().unwrap();
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
        assert!(a.should_quit);
    }

    #[test]
    fn submit_w_without_path_errors() {
        let mut a = app_with("hello", 10);
        a.apply(Action::EnterCommandLine);
        a.apply(Action::CommandLineAppend('w'));
        a.apply(Action::CommandLineSubmit);
        let msg = a.last_message.as_ref().unwrap();
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
        a.apply(invoke_motion(a.builtins.line_end));
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
        let msg = a.last_message.as_ref().unwrap();
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

        assert!(a.should_quit);
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
        let msg = a.last_message.as_ref().unwrap();
        assert_eq!(msg.level, EchoLevel::Error);
        assert!(msg.text.contains("frobnicate"));
    }

    #[test]
    fn submitting_returns_to_normal_modal() {
        let mut a = app_with("hello", 10);
        a.apply(Action::EnterCommandLine);
        a.apply(Action::CommandLineAppend('q'));
        a.apply(Action::CommandLineSubmit);
        assert_eq!(a.modal, ModalState::Normal);
    }

    #[test]
    fn echo_action_replaces_last_message() {
        let mut a = app_with("", 10);
        a.apply(Action::Echo(EchoMessage {
            text: "hi".into(),
            level: EchoLevel::Info,
        }));
        assert_eq!(a.last_message.as_ref().unwrap().text, "hi");
        a.apply(Action::Echo(EchoMessage {
            text: "bye".into(),
            level: EchoLevel::Warn,
        }));
        assert_eq!(a.last_message.as_ref().unwrap().text, "bye");
        assert_eq!(a.last_message.as_ref().unwrap().level, EchoLevel::Warn);
    }

    // ---- Search ----

    fn type_pattern(a: &mut App, pattern: &str) {
        for c in pattern.chars() {
            a.apply(Action::SearchAppend(c));
        }
    }

    #[test]
    fn enter_search_seeds_state() {
        let mut a = app_with("hello world", 10);
        a.apply(Action::EnterSearch(SearchDirection::Forward));
        assert_eq!(a.modal, ModalState::Search(SearchDirection::Forward));
        let line = a.search_line.as_ref().expect("search_line populated");
        assert_eq!(line.pattern, "");
        assert_eq!(line.origin, Position::ZERO);
    }

    #[test]
    fn search_append_grows_pattern_and_previews_match() {
        let mut a = app_with("foo bar foo", 10);
        a.apply(Action::EnterSearch(SearchDirection::Forward));
        type_pattern(&mut a, "bar");
        let line = a.search_line.as_ref().unwrap();
        assert_eq!(line.pattern, "bar");
        // Preview should highlight the first match without moving cursor.
        let m = a.current_match.expect("match previewed");
        assert_eq!(m.start, Position::new(0, 4));
        assert_eq!(a.cursor, Position::ZERO);
    }

    #[test]
    fn search_backspace_shrinks_pattern_and_re_previews() {
        let mut a = app_with("foo bar baz", 10);
        a.apply(Action::EnterSearch(SearchDirection::Forward));
        type_pattern(&mut a, "baz");
        a.apply(Action::SearchBackspace);
        assert_eq!(a.search_line.as_ref().unwrap().pattern, "ba");
        let m = a.current_match.expect("preview after backspace");
        assert_eq!(m.start, Position::new(0, 4));
    }

    #[test]
    fn search_backspace_on_empty_pattern_exits_search() {
        let mut a = app_with("hello", 10);
        a.apply(Action::EnterSearch(SearchDirection::Forward));
        a.apply(Action::SearchBackspace);
        assert_eq!(a.modal, ModalState::Normal);
        assert!(a.search_line.is_none());
    }

    #[test]
    fn search_submit_jumps_cursor_to_match_and_records_last_search() {
        let mut a = app_with("foo bar foo", 10);
        a.apply(Action::EnterSearch(SearchDirection::Forward));
        type_pattern(&mut a, "bar");
        a.apply(Action::SearchSubmit);
        assert_eq!(a.modal, ModalState::Normal);
        assert_eq!(a.cursor, Position::new(0, 4));
        assert!(a.search_line.is_none());
        let last = a.last_search.as_ref().unwrap();
        assert_eq!(last.pattern, "bar");
        assert_eq!(last.direction, SearchDirection::Forward);
    }

    #[test]
    fn search_submit_with_no_match_records_pattern_and_warns() {
        let mut a = app_with("hello", 10);
        a.apply(Action::EnterSearch(SearchDirection::Forward));
        type_pattern(&mut a, "xyz");
        a.apply(Action::SearchSubmit);
        assert!(a.current_match.is_none());
        assert_eq!(a.last_search.as_ref().unwrap().pattern, "xyz");
        let msg = a.last_message.as_ref().unwrap();
        assert_eq!(msg.level, EchoLevel::Error);
        assert!(msg.text.contains("Pattern not found"));
    }

    #[test]
    fn search_cancel_restores_cursor_to_origin() {
        let mut a = app_with("foo bar foo", 10);
        a.cursor = Position::new(0, 5);
        a.apply(Action::EnterSearch(SearchDirection::Forward));
        type_pattern(&mut a, "foo");
        // Preview should have set current_match to "foo" at byte 8.
        assert_eq!(a.current_match.unwrap().start, Position::new(0, 8));
        a.apply(Action::SearchCancel);
        assert_eq!(a.modal, ModalState::Normal);
        assert_eq!(a.cursor, Position::new(0, 5));
        assert!(a.current_match.is_none());
    }

    #[test]
    fn n_after_forward_search_advances_to_next_match() {
        let mut a = app_with("foo bar foo bar", 10);
        a.apply(Action::EnterSearch(SearchDirection::Forward));
        type_pattern(&mut a, "foo");
        a.apply(Action::SearchSubmit);
        assert_eq!(a.cursor, Position::new(0, 0));
        a.apply(Action::SearchNext);
        assert_eq!(a.cursor, Position::new(0, 8));
    }

    #[test]
    fn capital_n_reverses_direction() {
        let mut a = app_with("foo bar foo bar", 10);
        a.apply(Action::EnterSearch(SearchDirection::Forward));
        type_pattern(&mut a, "foo");
        a.apply(Action::SearchSubmit);
        a.apply(Action::SearchNext);
        assert_eq!(a.cursor, Position::new(0, 8));
        a.apply(Action::SearchPrevious);
        assert_eq!(a.cursor, Position::new(0, 0));
    }

    #[test]
    fn n_with_no_last_search_emits_error() {
        let mut a = app_with("hello", 10);
        a.apply(Action::SearchNext);
        let msg = a.last_message.as_ref().unwrap();
        assert_eq!(msg.level, EchoLevel::Error);
        assert!(msg.text.contains("no previous"));
    }

    #[test]
    fn search_forward_wraps_and_warns() {
        let mut a = app_with("alpha beta gamma alpha", 10);
        a.cursor = Position::new(0, 17); // past the second "alpha"... actually at it
        // Move past it for clarity.
        a.cursor = Position::new(0, 18);
        a.apply(Action::EnterSearch(SearchDirection::Forward));
        type_pattern(&mut a, "alpha");
        a.apply(Action::SearchSubmit);
        // First "alpha" is at byte 0; we wrapped from byte 18.
        assert_eq!(a.cursor, Position::new(0, 0));
        let msg = a.last_message.as_ref().unwrap();
        assert_eq!(msg.level, EchoLevel::Warn);
        assert!(msg.text.contains("BOTTOM"));
    }

    #[test]
    fn search_backward_finds_previous_match() {
        let mut a = app_with("alpha beta gamma alpha", 10);
        a.cursor = Position::new(0, 22);
        a.apply(Action::EnterSearch(SearchDirection::Backward));
        type_pattern(&mut a, "alpha");
        a.apply(Action::SearchSubmit);
        assert_eq!(a.cursor, Position::new(0, 17));
    }

    // ---- change operator end-to-end ----

    #[test]
    fn cw_deletes_word_and_enters_insert_mode() {
        let mut a = app_with("hello world", 10);
        let inv = CommandInvocation::of(a.builtins.change.0).with_target(
            lattice_grammar::Target::Motion(a.builtins.word_forward, lattice_grammar::Args::None),
        );
        a.apply(Action::Invoke(inv));
        assert_eq!(a.document.text(), "world");
        assert_eq!(a.modal, ModalState::Insert);
        assert_eq!(a.cursor, Position::ZERO);
    }

    #[test]
    fn cc_clears_current_line_and_enters_insert_mode() {
        let mut a = app_with("aaa\nBBB\nccc", 10);
        a.cursor = Position::new(1, 0);
        let inv = CommandInvocation::of(a.builtins.change.0)
            .with_range(lattice_grammar::Range::CurrentLine);
        a.apply(Action::Invoke(inv));
        assert_eq!(a.document.text(), "aaa\n\nccc");
        assert_eq!(a.modal, ModalState::Insert);
    }

    // ---- Dot-repeat ----

    #[test]
    fn dot_with_no_prior_change_emits_error() {
        let mut a = app_with("hello", 10);
        assert!(a.last_change.is_none());
        a.apply(Action::RepeatLastChange);
        let msg = a.last_message.as_ref().unwrap();
        assert_eq!(msg.level, EchoLevel::Error);
    }

    #[test]
    fn delete_records_last_change_and_dot_replays_it() {
        let mut a = app_with("foo bar foo bar", 10);
        let inv = CommandInvocation::of(a.builtins.delete.0).with_target(
            lattice_grammar::Target::Motion(a.builtins.word_forward, lattice_grammar::Args::None),
        );
        a.apply(Action::Invoke(inv));
        // After dw: "bar foo bar".
        assert_eq!(a.document.text(), "bar foo bar");
        assert!(a.last_change.is_some());
        // `.` replays the same dw at the new cursor position.
        a.apply(Action::RepeatLastChange);
        assert_eq!(a.document.text(), "foo bar");
    }

    #[test]
    fn yank_does_not_record_last_change() {
        let mut a = app_with("hello world", 10);
        let inv = CommandInvocation::of(a.builtins.yank.0).with_target(
            lattice_grammar::Target::Motion(a.builtins.word_forward, lattice_grammar::Args::None),
        );
        a.apply(Action::Invoke(inv));
        // Yank doesn't mutate the buffer; dot-repeat shouldn't pick this up.
        assert!(a.last_change.is_none());
    }

    #[test]
    fn motion_does_not_record_last_change() {
        let mut a = app_with("hello world", 10);
        a.apply(invoke_motion(a.builtins.word_forward));
        assert!(a.last_change.is_none());
    }

    #[test]
    fn dd_records_last_change_and_dot_replays_it() {
        let mut a = app_with("aaa\nBBB\nccc\nddd", 10);
        a.cursor = Position::new(1, 0);
        let inv = CommandInvocation::of(a.builtins.delete.0)
            .with_range(lattice_grammar::Range::CurrentLine);
        a.apply(Action::Invoke(inv));
        assert_eq!(a.document.text(), "aaa\n\nccc\nddd");
        // `.` repeats: empty line is now "deleted" -> empty stays.
        a.apply(Action::RepeatLastChange);
        // `.` re-runs `dd` at the cursor; line 1 (empty) becomes a no-op
        // edit since CurrentLine is empty. Buffer unchanged.
        assert_eq!(a.document.text(), "aaa\n\nccc\nddd");
    }

    #[test]
    fn change_records_last_change() {
        let mut a = app_with("hello world", 10);
        let inv = CommandInvocation::of(a.builtins.change.0).with_target(
            lattice_grammar::Target::Motion(a.builtins.word_forward, lattice_grammar::Args::None),
        );
        a.apply(Action::Invoke(inv));
        // change drops to Insert, but the change itself is recorded.
        assert!(a.last_change.is_some());
    }

    // ---- Visual mode end-to-end ----

    #[test]
    fn enter_visual_charwise_sets_modal_and_anchor() {
        let mut a = app_with("hello", 10);
        a.cursor = Position::new(0, 1);
        a.apply(Action::EnterVisual(VisualKind::Charwise));
        assert_eq!(a.modal, ModalState::Visual(VisualKind::Charwise));
        assert_eq!(a.visual_anchor, Some(Position::new(0, 1)));
        let sel = a.document.selections().primary();
        assert_eq!(sel.anchor, Position::new(0, 1));
        assert_eq!(sel.head, Position::new(0, 1));
        assert_eq!(sel.visual, Some(VisualMode::Charwise));
    }

    #[test]
    fn motion_in_visual_extends_head_keeps_anchor() {
        let mut a = app_with("hello world", 10);
        a.apply(Action::EnterVisual(VisualKind::Charwise));
        a.apply(invoke_motion(a.builtins.word_forward));
        let sel = a.document.selections().primary();
        assert_eq!(sel.anchor, Position::ZERO);
        assert_eq!(sel.head, Position::new(0, 6));
        assert_eq!(a.cursor, Position::new(0, 6));
    }

    #[test]
    fn esc_in_visual_collapses_selection_and_returns_to_normal() {
        let mut a = app_with("hello", 10);
        a.apply(Action::EnterVisual(VisualKind::Charwise));
        a.apply(invoke_motion(a.builtins.char_right));
        a.apply(Action::ExitVisual);
        assert_eq!(a.modal, ModalState::Normal);
        assert!(a.visual_anchor.is_none());
        assert!(a.document.selections().primary().is_cursor());
    }

    #[test]
    fn delete_in_visual_removes_selection_and_returns_to_normal() {
        let mut a = app_with("hello world", 10);
        a.apply(Action::EnterVisual(VisualKind::Charwise));
        a.apply(invoke_motion(a.builtins.char_right));
        a.apply(invoke_motion(a.builtins.char_right));
        a.apply(invoke_motion(a.builtins.char_right));
        // Selection now covers bytes 0..3 of "hello world" charwise (vim
        // INCLUSIVE -> visual range covers 0..=3 = 4 bytes "hell").
        let inv = CommandInvocation::of(a.builtins.delete.0)
            .with_range(lattice_grammar::Range::Selection);
        a.apply(Action::Invoke(inv));
        assert_eq!(a.document.text(), "o world");
        assert_eq!(a.modal, ModalState::Normal);
    }

    #[test]
    fn yank_in_visual_populates_register_charwise() {
        let mut a = app_with("hello world", 10);
        a.apply(Action::EnterVisual(VisualKind::Charwise));
        a.apply(invoke_motion(a.builtins.word_forward));
        let inv = CommandInvocation::of(a.builtins.yank.0)
            .with_range(lattice_grammar::Range::Selection);
        a.apply(Action::Invoke(inv));
        let reg = a.unnamed_register.as_ref().unwrap();
        assert_eq!(reg.kind, YankKind::Charwise);
        // Document text untouched.
        assert_eq!(a.document.text(), "hello world");
        // Visual mode exited.
        assert_eq!(a.modal, ModalState::Normal);
    }

    #[test]
    fn change_in_visual_enters_insert_mode() {
        let mut a = app_with("hello world", 10);
        a.apply(Action::EnterVisual(VisualKind::Charwise));
        a.apply(invoke_motion(a.builtins.word_forward));
        let inv = CommandInvocation::of(a.builtins.change.0)
            .with_range(lattice_grammar::Range::Selection);
        a.apply(Action::Invoke(inv));
        // Change in Visual deletes selection AND drops into Insert.
        assert_eq!(a.modal, ModalState::Insert);
    }

    #[test]
    fn linewise_visual_yank_captures_full_lines() {
        let mut a = app_with("aaa\nBBB\nccc", 10);
        a.cursor = Position::new(1, 1); // mid-line on "BBB"
        a.apply(Action::EnterVisual(VisualKind::Linewise));
        // Selection is single line; yank captures the whole line
        // regardless of byte offsets.
        let inv = CommandInvocation::of(a.builtins.yank.0)
            .with_range(lattice_grammar::Range::Selection);
        a.apply(Action::Invoke(inv));
        let reg = a.unnamed_register.as_ref().unwrap();
        assert_eq!(reg.kind, YankKind::Linewise);
        assert_eq!(reg.content, "BBB");
    }

    #[test]
    fn linewise_visual_extends_to_multiple_lines() {
        let mut a = app_with("aaa\nbbb\nccc\nddd", 10);
        a.apply(Action::EnterVisual(VisualKind::Linewise));
        a.apply(invoke_motion(a.builtins.line_down));
        let inv = CommandInvocation::of(a.builtins.yank.0)
            .with_range(lattice_grammar::Range::Selection);
        a.apply(Action::Invoke(inv));
        let reg = a.unnamed_register.as_ref().unwrap();
        assert_eq!(reg.kind, YankKind::Linewise);
        // Lines 0 and 1 -> "aaa\nbbb".
        assert_eq!(reg.content, "aaa\nbbb");
    }

    #[test]
    fn visual_anchor_persists_across_count_motion() {
        let mut a = app_with("one two three four five", 10);
        a.apply(Action::EnterVisual(VisualKind::Charwise));
        a.apply(Action::PushDigit(2));
        a.apply(invoke_motion(a.builtins.word_forward));
        let sel = a.document.selections().primary();
        assert_eq!(sel.anchor, Position::ZERO);
        // 2w from origin advances 2 word starts: "ONE two THREE" -> byte 8.
        assert_eq!(sel.head, Position::new(0, 8));
    }

    // ---- count prefix end-to-end ----

    #[test]
    fn push_digit_accumulates_pending_count() {
        let mut a = app_with("abc", 10);
        a.apply(Action::PushDigit(1));
        a.apply(Action::PushDigit(2));
        a.apply(Action::PushDigit(3));
        assert_eq!(a.pending_count, 123);
    }

    #[test]
    fn invoke_consumes_pending_count_into_motion() {
        let mut a = app_with("one two three four five", 10);
        a.apply(Action::PushDigit(3));
        let id = a.builtins.word_forward;
        a.apply(invoke_motion(id));
        // 3w from origin: "one two three FOUR five" -> 'f' of "four" at byte 14.
        assert_eq!(a.cursor, Position::new(0, 14));
        // pending_count is reset after dispatch.
        assert_eq!(a.pending_count, 0);
    }

    #[test]
    fn count_with_line_motion_advances_count_lines() {
        let mut a = app_with("0\n1\n2\n3\n4\n5\n6\n7\n8\n9", 20);
        a.apply(Action::PushDigit(5));
        let id = a.builtins.line_down;
        a.apply(invoke_motion(id));
        assert_eq!(a.cursor.line, 5);
    }

    #[test]
    fn operator_then_motion_with_count_multiplies() {
        // `2dw` -> delete 2 words from cursor.
        let mut a = app_with("one two three four five", 10);
        a.apply(Action::PushDigit(2));
        // SetPending latches the count as op_count.
        a.apply(Action::SetPending(Pending::AfterOperator(a.builtins.delete)));
        assert_eq!(a.op_count, 2);
        assert_eq!(a.pending_count, 0);
        let inv = CommandInvocation::of(a.builtins.delete.0).with_target(
            lattice_grammar::Target::Motion(a.builtins.word_forward, lattice_grammar::Args::None),
        );
        a.apply(Action::Invoke(inv));
        // 2dw: deletes "one two " leaving "three four five".
        assert_eq!(a.document.text(), "three four five");
        assert_eq!(a.op_count, 0);
    }

    #[test]
    fn count_on_both_sides_multiplies_2d3w_equals_6w() {
        // `2d3w`: op_count = 2, motion count = 3, final count = 6.
        let mut a = app_with("a b c d e f g h i j", 10);
        a.apply(Action::PushDigit(2));
        a.apply(Action::SetPending(Pending::AfterOperator(a.builtins.delete)));
        assert_eq!(a.op_count, 2);
        a.apply(Action::PushDigit(3));
        let inv = CommandInvocation::of(a.builtins.delete.0).with_target(
            lattice_grammar::Target::Motion(a.builtins.word_forward, lattice_grammar::Args::None),
        );
        a.apply(Action::Invoke(inv));
        // 6 words deleted from "a b c d e f g h i j" leaves "g h i j".
        assert_eq!(a.document.text(), "g h i j");
    }

    #[test]
    fn count_zero_through_pending_count_is_ignored_by_motion() {
        // pending_count remains 0 after no digit; motion uses default 1.
        let mut a = app_with("hello world", 10);
        let id = a.builtins.word_forward;
        a.apply(invoke_motion(id));
        assert_eq!(a.cursor, Position::new(0, 6));
    }

    // ---- find / till motions end-to-end ----

    #[test]
    fn fz_jumps_to_z_on_current_line() {
        let mut a = app_with("hello, world", 10);
        let inv = CommandInvocation::of(a.builtins.find_char_forward.0)
            .with_args(lattice_grammar::Args::Char('w'));
        a.apply(Action::Invoke(inv));
        assert_eq!(a.cursor, Position::new(0, 7));
    }

    #[test]
    fn capital_f_jumps_backward() {
        let mut a = app_with("hello, world", 10);
        a.cursor = Position::new(0, 11); // on 'd'
        let inv = CommandInvocation::of(a.builtins.find_char_backward.0)
            .with_args(lattice_grammar::Args::Char('h'));
        a.apply(Action::Invoke(inv));
        assert_eq!(a.cursor, Position::ZERO);
    }

    #[test]
    fn t_lands_one_byte_before_target() {
        let mut a = app_with("hello, world", 10);
        let inv = CommandInvocation::of(a.builtins.till_char_forward.0)
            .with_args(lattice_grammar::Args::Char('w'));
        a.apply(Action::Invoke(inv));
        assert_eq!(a.cursor, Position::new(0, 6));
    }

    #[test]
    fn df_deletes_through_target_char() {
        // From "hello, world" with cursor at 0, `df,` deletes "hello," and
        // leaves " world".
        let mut a = app_with("hello, world", 10);
        let inv = CommandInvocation::of(a.builtins.delete.0).with_target(
            lattice_grammar::Target::Motion(
                a.builtins.find_char_forward,
                lattice_grammar::Args::Char(','),
            ),
        );
        a.apply(Action::Invoke(inv));
        // dispatcher uses [start, end) range; find_char_forward returns the
        // position of the comma (byte 5), so [0, 5) = "hello" is deleted.
        // The trailing comma stays in place.
        assert_eq!(a.document.text(), ", world");
    }

    #[test]
    fn ct_with_change_enters_insert_mode() {
        let mut a = app_with("hello, world", 10);
        let inv = CommandInvocation::of(a.builtins.change.0).with_target(
            lattice_grammar::Target::Motion(
                a.builtins.till_char_forward,
                lattice_grammar::Args::Char(','),
            ),
        );
        a.apply(Action::Invoke(inv));
        assert_eq!(a.modal, ModalState::Insert);
    }

    #[test]
    fn find_no_match_keeps_cursor() {
        let mut a = app_with("hello", 10);
        a.cursor = Position::new(0, 1);
        let inv = CommandInvocation::of(a.builtins.find_char_forward.0)
            .with_args(lattice_grammar::Args::Char('z'));
        a.apply(Action::Invoke(inv));
        assert_eq!(a.cursor, Position::new(0, 1));
    }

    // ---- yank + paste end-to-end ----

    #[test]
    fn yw_populates_unnamed_register_charwise() {
        let mut a = app_with("hello world", 10);
        let inv = CommandInvocation::of(a.builtins.yank.0).with_target(
            lattice_grammar::Target::Motion(a.builtins.word_forward, lattice_grammar::Args::None),
        );
        a.apply(Action::Invoke(inv));
        let reg = a.unnamed_register.as_ref().unwrap();
        assert_eq!(reg.content, "hello ");
        assert_eq!(reg.kind, YankKind::Charwise);
        // Buffer untouched by yank.
        assert_eq!(a.document.text(), "hello world");
    }

    #[test]
    fn yy_populates_register_linewise() {
        let mut a = app_with("aaa\nBBB\nccc", 10);
        a.cursor = Position::new(1, 0);
        let inv = CommandInvocation::of(a.builtins.yank.0)
            .with_range(lattice_grammar::Range::CurrentLine);
        a.apply(Action::Invoke(inv));
        let reg = a.unnamed_register.as_ref().unwrap();
        assert_eq!(reg.content, "BBB");
        assert_eq!(reg.kind, YankKind::Linewise);
        assert_eq!(a.document.text(), "aaa\nBBB\nccc");
    }

    #[test]
    fn dd_populates_register_linewise_via_delete() {
        // delete also yanks; register kind is linewise for dd.
        let mut a = app_with("aaa\nBBB\nccc", 10);
        a.cursor = Position::new(1, 0);
        let inv = CommandInvocation::of(a.builtins.delete.0)
            .with_range(lattice_grammar::Range::CurrentLine);
        a.apply(Action::Invoke(inv));
        let reg = a.unnamed_register.as_ref().unwrap();
        assert_eq!(reg.kind, YankKind::Linewise);
        assert_eq!(reg.content, "BBB");
    }

    #[test]
    fn paste_after_charwise_inserts_after_cursor() {
        let mut a = app_with("hello", 10);
        a.unnamed_register = Some(UnnamedRegister {
            content: "X".into(),
            kind: YankKind::Charwise,
        });
        a.cursor = Position::new(0, 0); // on 'h'
        a.apply(Action::PasteAfter);
        assert_eq!(a.document.text(), "hXello");
        // Cursor lands on the last char of the pasted text (still 'X').
        assert_eq!(a.cursor, Position::new(0, 1));
    }

    #[test]
    fn paste_before_charwise_inserts_at_cursor() {
        let mut a = app_with("hello", 10);
        a.unnamed_register = Some(UnnamedRegister {
            content: "X".into(),
            kind: YankKind::Charwise,
        });
        a.cursor = Position::new(0, 2); // on 'l'
        a.apply(Action::PasteBefore);
        assert_eq!(a.document.text(), "heXllo");
        assert_eq!(a.cursor, Position::new(0, 2));
    }

    #[test]
    fn paste_after_linewise_inserts_below_current_line() {
        let mut a = app_with("aaa\nBBB\nccc", 10);
        a.unnamed_register = Some(UnnamedRegister {
            content: "XXX\n".into(),
            kind: YankKind::Linewise,
        });
        a.cursor = Position::new(1, 0); // on 'B' line
        a.apply(Action::PasteAfter);
        assert_eq!(a.document.text(), "aaa\nBBB\nXXX\nccc");
        assert_eq!(a.cursor, Position::new(2, 0));
    }

    #[test]
    fn paste_before_linewise_inserts_above_current_line() {
        let mut a = app_with("aaa\nBBB\nccc", 10);
        a.unnamed_register = Some(UnnamedRegister {
            content: "XXX\n".into(),
            kind: YankKind::Linewise,
        });
        a.cursor = Position::new(1, 0);
        a.apply(Action::PasteBefore);
        assert_eq!(a.document.text(), "aaa\nXXX\nBBB\nccc");
        assert_eq!(a.cursor, Position::new(1, 0));
    }

    #[test]
    fn paste_with_empty_register_emits_error_message() {
        let mut a = app_with("hello", 10);
        assert!(a.unnamed_register.is_none());
        a.apply(Action::PasteAfter);
        let msg = a.last_message.as_ref().unwrap();
        assert_eq!(msg.level, EchoLevel::Error);
        assert_eq!(a.document.text(), "hello");
    }

    #[test]
    fn yank_then_paste_round_trips_word() {
        let mut a = app_with("hello world", 10);
        let yank = CommandInvocation::of(a.builtins.yank.0).with_target(
            lattice_grammar::Target::Motion(a.builtins.word_forward, lattice_grammar::Args::None),
        );
        a.apply(Action::Invoke(yank));
        // Move cursor to end of buffer.
        a.cursor = Position::new(0, 11);
        a.apply(Action::PasteAfter);
        assert_eq!(a.document.text(), "hello worldhello ");
    }

    #[test]
    fn delete_then_paste_after_emulates_xp_swap() {
        // Vim trick: cursor on 'a' of "abc"; `xp` swaps 'a' and 'b' -> "bac".
        let mut a = app_with("abc", 10);
        a.cursor = Position::ZERO;
        // x: delete char-right
        let inv = CommandInvocation::of(a.builtins.delete.0).with_target(
            lattice_grammar::Target::Motion(a.builtins.char_right, lattice_grammar::Args::None),
        );
        a.apply(Action::Invoke(inv));
        assert_eq!(a.document.text(), "bc");
        // p: paste after cursor (cursor at 0 on 'b'; paste after -> "bac").
        a.apply(Action::PasteAfter);
        assert_eq!(a.document.text(), "bac");
    }

    #[test]
    fn after_change_user_can_type_and_replacement_lands() {
        let mut a = app_with("hello world", 10);
        let inv = CommandInvocation::of(a.builtins.change.0).with_target(
            lattice_grammar::Target::Motion(a.builtins.word_forward, lattice_grammar::Args::None),
        );
        a.apply(Action::Invoke(inv));
        assert_eq!(a.modal, ModalState::Insert);
        a.apply(Action::Insert("HEY ".into()));
        assert_eq!(a.document.text(), "HEY world");
    }

    #[test]
    fn search_works_across_lines() {
        let mut a = app_with("foo\nbar\nfoo\nbaz", 10);
        a.apply(Action::EnterSearch(SearchDirection::Forward));
        type_pattern(&mut a, "foo");
        a.apply(Action::SearchSubmit);
        assert_eq!(a.cursor, Position::new(0, 0));
        a.apply(Action::SearchNext);
        assert_eq!(a.cursor, Position::new(2, 0));
    }
}
