//! Translate canonical `KeyChord` values into `Action`s.
//!
//! Renderer-neutral dispatch entry point: reads modal state, the
//! pending-key buffer, and the catalog of built-in command IDs to
//! decide what each chord means. The `crossterm::KeyEvent → KeyChord`
//! adapter (and the analogous future-renderer adapters) live in
//! the renderer crates; this module never sees a raw key event.
//!
//! Shape per DESIGN.md §5.2.3: chord → typed `CommandInvocation`,
//! so swapping in the layered keymap engine later is mechanical.

use lattice_grammar::ModalState;
use lattice_grammar::builtins::Builtins;

use crate::action::Action;
use crate::buffers::BufferKind;
use crate::chord::{KeyChord, KeyKind, SpecialKey};
use crate::keymap_insert::dispatch_insert;
use crate::keymap_registry::KeymapHandle;
use crate::keymap_replace::dispatch_replace;
use crate::keymap_visual::dispatch_visual;

pub struct TranslateContext<'a> {
    pub modal: ModalState,
    pub builtins: &'a Builtins,
    /// In-progress count prefix; `0` means none. Translate uses this to
    /// disambiguate the `0` key (line_start when no count in progress;
    /// digit-zero appended to count otherwise).
    pub pending_count: u32,
    /// Operator-side count latched at `Pending::AfterOperator`
    /// activation (App moves `pending_count` -> `op_count` then
    /// resets `pending_count` so the in-progress digits track
    /// the *motion* side of `<op-count><op><motion-count><motion>`).
    /// Slice 8.g.iv reads this in `keymap_normal::attach_count`
    /// to multiply with the motion count when an
    /// `Action::Invoke` resolves.
    pub op_count: u32,
    /// True when a macro is currently being recorded. Translate uses
    /// this so `q` while recording stops, while `q` otherwise starts a
    /// new recording.
    pub recording_macro: bool,
    /// Which buffer the App's input pipeline currently routes to.
    /// Driven by [`crate::app::App::active_buffer`]; defaults to
    /// [`BufferKind::Document`]. Help buffers route through the
    /// same Normal-mode chord grammar (motions, `<C-o>` / `<C-i>`,
    /// `gg` / `G`, etc.) -- only three buffer-local bindings
    /// differ: `Esc` / `q` dismiss the help overlay, and `<CR>`
    /// follows the link under the cursor.
    pub active_buffer: BufferKind,
    /// True when the command-line completion popup is open
    /// (DESIGN.md §5.11.3). Tab / S-Tab / Enter / Esc are claimed
    /// by the popup before falling through to Command mode.
    pub completion_open: bool,
    /// True when the cmdline cursor sits on an `ArgKind::Chord`
    /// arg slot. In this mode every key event renders to a chord
    /// token and gets appended; the only edits are `<BS>` (delete
    /// last chord token), `<CR>` (submit), `<Esc>` (cancel). Lookup
    /// of multi-stroke sequences (`gg`, `<C-w>j`) is supported by
    /// pressing each chord in turn.
    pub chord_capture: bool,
    /// True when a picker (`Picker` overlay) is open. Picker
    /// claims every key before the modal handlers see it: char
    /// keys append to the query, `<Up>` / `<C-p>` / `<Down>` /
    /// `<C-n>` move selection, `<CR>` accepts, `<Esc>` dismisses.
    pub picker_open: bool,
    /// True when the **Insert-mode completion popup** is open
    /// (Phase 4.2.g.1). Activates the completion-popup minor
    /// mode: `<C-n>` / `<C-p>` navigate, `<C-y>` / `<Tab>` /
    /// `<CR>` accept, `<C-e>` cancels, `<Esc>` cancels and
    /// exits Insert. Bindings inside this layer override the
    /// usual Insert-mode + Normal-mode meanings (notably
    /// `<C-d>` becomes "toggle docs popup" instead of
    /// "shift-left-indent" / "half-page-down"). Closing the
    /// popup deactivates the layer; original bindings restore.
    pub insert_completion_open: bool,
    /// True when an `ActiveSnippet` is in flight (Phase
    /// 4.2.g.4). Activates the active-snippet minor mode:
    /// `<Tab>` jumps to the next placeholder, `<S-Tab>` to
    /// the previous, `<Esc>` exits the snippet (and Insert).
    /// The layer claims `<Tab>` ahead of Insert-mode's
    /// "insert literal tab" so placeholder navigation is
    /// stable; closing the snippet deactivates the layer.
    pub snippet_active: bool,
    /// Terminal-mode T2.a (2026-05-25): true when
    /// `terminal-insert-mode` is active on the active Terminal
    /// buffer. `translate` short-circuits early in this state:
    /// keystrokes encode to ANSI bytes (via
    /// `keymap_terminal::key_to_ansi`) and emit
    /// `Action::TerminalInput` instead of going through the
    /// modal-state dispatchers.
    pub terminal_insert_active: bool,
    /// Terminal-mode T2.b.0 (2026-05-25): resolved value of the
    /// `terminal.esc-exits` typed option for the active pane's
    /// buffer. When `true` and `terminal_insert_active` is also
    /// true, `<Esc>` emits `Action::ExitTerminalInsert` instead
    /// of encoding to `\x1b` for the PTY. Users running nested
    /// vim / htop inside the terminal flip the option off and
    /// use `<C-\><C-n>` (T2.c) to exit.
    pub terminal_esc_exits: bool,
    /// Terminal-mode T2.c (2026-05-25): DECCKM mode bit
    /// (application-cursor-keys) from the active terminal's
    /// alacritty `Term`. Threaded into
    /// `keymap_terminal::key_to_ansi_with_mode` so arrow keys
    /// encode as SS3 (`ESC O A`) vs CSI (`ESC [ A`).
    pub terminal_app_cursor_keys: bool,
    /// Terminal-mode T2.c (2026-05-25): `<C-\>` exit chord is
    /// armed and waiting for the confirm key. When set, the
    /// translate layer routes the next keystroke into the
    /// chord-resolution branch instead of the normal encoder.
    pub terminal_insert_exit_pending: bool,
    /// 2026-05-25: true when the active Terminal buffer holds
    /// an in-flight Visual selection (`t.visual.is_some()`).
    /// Terminal-Visual lives on the buffer (modal stays Normal)
    /// so the `<Esc>` / `v` exit chords don't come through
    /// `keymap_visual`'s `ExitVisual` bindings; this layer
    /// short-circuits them when the flag is set.
    pub terminal_visual_active: bool,
    /// Layered keymap registry (DESIGN.md §5.2.3, audit
    /// slice 8.c -- 8.d). `translate` consults this instead
    /// of the per-mode hand-rolled `match` tables one slice
    /// at a time as the migration progresses; Replace mode
    /// is the first migrated dispatcher. Borrowed for the
    /// duration of the translate call -- a single
    /// `ArcSwap::load` happens inside the dispatcher.
    pub keymap: &'a KeymapHandle,
    /// Slice 8.i.4.a: in-flight partial-chord stack from
    /// `App::partial_chord`. When non-empty, `translate_normal`
    /// runs the keymap lookup with this as the prefix instead
    /// of the legacy `match pending` body; the simple
    /// prefix-only Pending variants (AfterG / AfterZ /
    /// AfterCtrlW / AfterSetMark / AfterJumpMarkLine /
    /// AfterJumpMarkExact / AfterRegister / AfterMacroStart /
    /// AfterMacroPlay) all funnel through here now.
    pub partial_chord: &'a [crate::chord::KeyChord],
    /// D.5.b (2026-05-30): active buffer's minor modes, in
    /// activation order. Threaded into `lookup_with_context` so
    /// chord bindings registered under `MinorMode(ModeId)`
    /// layers only fire on buffers where the corresponding
    /// mode is in `ActiveModes.minors()`. Empty slice means
    /// "no minor modes active" — minor-mode bindings are
    /// invisible to dispatch under that constraint
    /// (K.1.c fast path). Normal-mode dispatch uses this
    /// today (`lookup_normal` / `lookup_normal_with_prefix`);
    /// Visual / Insert / Replace stay on the legacy
    /// all-registered-modes-active `lookup` until D.5
    /// extends their grammar.
    pub active_minor_modes: &'a [lattice_mode::ModeId],
}

pub fn translate(ctx: TranslateContext<'_>, chord: KeyChord) -> Action {
    // Slice 5.4 (slice 5): renderer-neutral dispatch entry point.
    // Takes a canonical `KeyChord` directly; the crossterm-coupled
    // `KeyEvent → KeyChord` adapter lives in
    // `lattice_ui_tui::input::translate`, which is the thin shim
    // every renderer's runtime calls. The future
    // `lattice_ui_gpui::input::translate` ships its own analogous
    // shim and feeds chords into this same function.

    // Picker overlay precedes everything (DESIGN.md §5.9.7): the
    // user is in a focused "type to filter, Enter to act" state;
    // modal handlers never see these keys until the picker is
    // dismissed. `<C-c>` still drops the picker rather than the
    // app so an open picker isn't a foot-gun.
    if ctx.picker_open {
        return translate_picker(chord);
    }

    // Terminal-mode T2.a (2026-05-25): when Terminal-Insert is
    // active, EVERY keystroke encodes to ANSI and goes to the
    // PTY — including `<C-c>` (which becomes shell SIGINT, not
    // "quit the editor"). The one escape is `<C-\><C-n>` which
    // exits Terminal-Insert and falls back to Normal-in-terminal;
    // T2.a handles the `<C-\>` half here and lets `<C-n>` arrive
    // as a separate event with the mode now off. T2.c will add
    // a stateful two-key chord so the escape sequence doesn't
    // leak `\x1c` into the PTY between the two keys.
    if ctx.terminal_insert_active && matches!(ctx.active_buffer, BufferKind::Terminal) {
        // Terminal-mode T2.c (2026-05-25): two-key exit chord.
        // If we're already in "armed" state (previous key was
        // `<C-\>`) the next keystroke resolves the chord:
        //   - `<C-n>` confirms the exit
        //   - anything else: send `\x1c` (the lost `<C-\>`)
        //     plus the chord's normal PTY bytes
        // Either way the arming clears (handled by the
        // `Action::ExitTerminalInsert` / `Action::TerminalInput`
        // dispatch arms).
        if ctx.terminal_insert_exit_pending {
            if chord == KeyChord::ctrl('n') {
                return Action::ExitTerminalInsert;
            }
            let mut bytes = vec![0x1c];
            if let Some(b) =
                crate::keymap_terminal::key_to_ansi_with_mode(&chord, ctx.terminal_app_cursor_keys)
            {
                bytes.extend(b);
            }
            return Action::TerminalInput(bytes);
        }
        if chord == KeyChord::ctrl('\\') {
            // Arm the chord; second key resolves above.
            return Action::TerminalArmExitChord;
        }
        // Terminal-mode T2.b.0 (2026-05-25): `<Esc>` exit gated
        // by `terminal.esc-exits` (default true). When off, Esc
        // falls through to the encoder and reaches the PTY as
        // `\x1b` so nested programs (vim, htop, less) keep their
        // own Esc semantics.
        if ctx.terminal_esc_exits
            && matches!(chord.key, KeyKind::Special(SpecialKey::Esc))
            && chord.mods.is_empty()
        {
            return Action::ExitTerminalInsert;
        }
        // T2.c (2026-05-25): encode with DECCKM awareness so
        // arrow keys flip to SS3 when the program has flipped
        // application-cursor-keys mode.
        if let Some(bytes) =
            crate::keymap_terminal::key_to_ansi_with_mode(&chord, ctx.terminal_app_cursor_keys)
        {
            return Action::TerminalInput(bytes);
        }
        return Action::None;
    }

    // Slice 8.f: the completion-popup and active-snippet minor
    // modes used to short-circuit `translate` from here. They
    // now register as `KeymapLayer::MinorMode` layers via
    // `App::sync_keymap_overlays`, pushed on overlay activation
    // and popped on deactivation. The Insert-mode dispatcher
    // (`dispatch_insert`) consults the merged trie, which
    // already accounts for the layer stack -- so popup / snippet
    // overrides resolve at lookup time without a per-`translate`
    // pre-pass. Push order (snippet first, popup second) makes
    // popup win on overlapping chords (preserving the legacy
    // "popup precedes snippet" gating).

    // Chord-capture overlay precedes the universal `<C-c>` -> Quit
    // hatch, because looking up `<C-c>`'s binding via
    // `:describe-key <C-c>` is a legitimate user need. The overlay
    // reserves Esc as the abort path, so the user is never stuck.
    if matches!(ctx.modal, ModalState::Command) && ctx.chord_capture {
        return translate_command_chord_capture(chord);
    }

    // Universal escape hatch -- with one exception, mirroring the S2
    // digit-precedence rule. When a Normal-mode partial chord is in
    // flight (e.g. emacs-keys' `<C-x>` leader) and `[partial + <C-c>]`
    // is a BOUND continuation, the leader chord wins so emacs
    // `C-x C-c` (= quit-all) works. A bare `<C-c>` (empty partial) or
    // any partial that does NOT bind `<C-c>` still hits the brute
    // quit. The lookup is Normal-specific (`lookup_normal_with_prefix`),
    // so Visual / Insert / Command `<C-c>` are unaffected.
    if chord == KeyChord::ctrl('c') {
        let continues_leader = matches!(ctx.modal, ModalState::Normal)
            && !ctx.partial_chord.is_empty()
            && !matches!(
                crate::keymap_normal::lookup_normal_with_prefix(
                    ctx.keymap,
                    ctx.partial_chord,
                    &chord,
                    ctx.active_minor_modes,
                ),
                Action::None
            );
        if !continues_leader {
            return Action::Quit;
        }
        // else: fall through to `translate_normal`, which resolves the
        // `[partial + <C-c>]` leader chord via the partial-chord path.
    }

    // Buffer-local bindings for read-only buffers (Help / FileTree;
    // DESIGN.md §5.9 buffer-local keymap layer): a small fixed set
    // of bindings unique to those kinds (dismiss + follow-link)
    // intercept first, then everything else flows through
    // `translate_normal` so the chord grammar (`gg`, `<C-d>`,
    // `<C-o>` / `<C-i>`, motions, viewport jumps) works identically
    // to the document path. The cursor that those motions move is
    // decided at apply time by `App::active_buffer`, not here.
    if matches!(ctx.active_buffer, BufferKind::Help | BufferKind::FileTree)
        && matches!(ctx.modal, ModalState::Normal)
        && ctx.partial_chord.is_empty()
    {
        // Esc dismisses; `q` does NOT (it falls through to its
        // normal Normal-mode meaning -- macro-record start). Help
        // and log buffers should behave like other buffers per
        // the user's everything-is-a-buffer expectation; the only
        // explicit close paths are Esc here, `:bd`, and
        // `Action::HelpDismiss` triggered by the State-A auto-
        // dismiss in App::apply.
        match chord.key {
            KeyKind::Special(SpecialKey::Esc) => return Action::HelpDismiss,
            KeyKind::Special(SpecialKey::Enter) => return Action::FollowLink,
            KeyKind::Char('-') => return Action::OilNavigateUp,
            _ => {}
        }
    }

    if matches!(ctx.active_buffer, BufferKind::Oil)
        && matches!(ctx.modal, ModalState::Normal)
        && ctx.partial_chord.is_empty()
    {
        match chord.key {
            KeyKind::Special(SpecialKey::Enter) => return Action::FollowLink,
            KeyKind::Char('-') => return Action::OilNavigateUp,
            _ => {}
        }
    }

    // Terminal-mode T2.a / T2.b (2026-05-25) — Normal-in-terminal
    // buffer-local bindings. Vim's full insert-entry set
    // (`i`/`a`/`I`/`A`) all funnel through one action because
    // the terminal grid has no "before/after column" or "BOL/EOL"
    // semantics — the shell owns the cursor, and the moment we
    // hand control back, every keystroke flows to the PTY.
    // Documenting the four chords keeps muscle memory honest
    // (users coming from vim's `:terminal` instinctively type
    // `a` to mean "insert"), but the resulting action is the same.
    // T2.c adds `<C-w>` window-prefix routing. Other keys fall
    // through to the standard normal-mode grammar (the
    // scrollback-motion path lands in T3).
    // Terminal-mode T3 (2026-05-25, MotionAdapter refactor): the
    // only kind-specific interception in `translate` for Terminal
    // buffers is the Insert-entry chord. Every other motion /
    // action keystroke (j / k / G / gg / <C-d> / <C-u> / <C-f> /
    // <C-b> / <C-e> / <C-y>) flows through the standard
    // Normal-mode keymap → `Editor::run_invocation` →
    // `run_terminal_invocation`. The substrate-aware
    // translation from "the line_down motion" to "scroll the
    // alacritty grid" lives on the runner, not on this layer.
    // (`i` / `a` / `I` / `A` stay here because they switch
    // *minor mode*, which is a renderer-layer concern: the
    // translate layer needs to start encoding subsequent
    // keystrokes to ANSI immediately.)
    if matches!(ctx.active_buffer, BufferKind::Terminal)
        && !ctx.terminal_insert_active
        && matches!(ctx.modal, ModalState::Normal)
        && ctx.partial_chord.is_empty()
        && !chord.mods.ctrl()
        && !chord.mods.alt()
    {
        // 2026-05-25: terminal-Visual exit. Visual lives on the
        // buffer (modal stays Normal), so `keymap_visual`'s
        // `ExitVisual` bindings never fire. Intercept the four
        // vim-standard exits here when terminal-Visual is in
        // flight: `<Esc>`, `v`, `V`, `<C-v>`. The Ctrl-V case
        // is below the !mods.ctrl() guard, so handle it after
        // the gate.
        if ctx.terminal_visual_active {
            match chord.key {
                KeyKind::Special(crate::chord::SpecialKey::Esc) => {
                    return Action::ExitVisual;
                }
                KeyKind::Char('v') | KeyKind::Char('V') => {
                    return Action::ExitVisual;
                }
                _ => {}
            }
        }
        match chord.key {
            KeyKind::Char('i') | KeyKind::Char('a') | KeyKind::Char('I') | KeyKind::Char('A') => {
                return Action::EnterTerminalInsert;
            }
            _ => {}
        }
    }
    // 2026-05-25: `<C-v>` toggle for blockwise terminal-Visual.
    // Sits outside the `!ctrl()` gate above so the Ctrl modifier
    // bit doesn't disqualify it.
    if matches!(ctx.active_buffer, BufferKind::Terminal)
        && !ctx.terminal_insert_active
        && ctx.terminal_visual_active
        && matches!(ctx.modal, ModalState::Normal)
        && ctx.partial_chord.is_empty()
        && chord.mods.ctrl()
        && !chord.mods.alt()
        && matches!(chord.key, KeyKind::Char('v'))
    {
        return Action::ExitVisual;
    }

    match ctx.modal {
        // Slice 8.f: Insert mode dispatches through the layered
        // registry. Base bindings live in
        // `keymap_insert::register_insert_bindings`; the
        // completion-popup and active-snippet overlays ride as
        // `KeymapLayer::MinorMode` layers managed by
        // `App::sync_keymap_overlays`. The drift test in
        // `keymap_insert::tests` is the regression net.
        ModalState::Insert => {
            dispatch_insert(ctx.keymap, &chord, ctx.partial_chord, ctx.active_minor_modes)
        }
        ModalState::Normal => translate_normal(
            chord,
            ctx.builtins,
            ctx.pending_count,
            ctx.op_count,
            ctx.recording_macro,
            ctx.keymap,
            ctx.partial_chord,
            ctx.active_minor_modes,
        ),
        ModalState::Command => translate_command(chord, ctx.completion_open, ctx.chord_capture),
        ModalState::Search(_) => translate_search(chord),
        // Slice 8.e: Visual mode dispatches through the layered
        // registry. The hand-rolled match table moved to
        // `keymap_visual::register_visual_bindings`; the
        // `kind`-specific block-only `I` / `A` overrides stay
        // pre-lookup in `dispatch_visual` until the architecture's
        // minor-mode-on-Visual layer push lands. The drift test
        // in `keymap_visual::tests` is the regression net.
        ModalState::Visual(kind) => {
            dispatch_visual(ctx.keymap, &chord, kind, ctx.partial_chord)
        }
        // SN.3d.1: Select mode — Visual's sibling with inverted typing
        // semantics. Genuinely new dispatch (a bare printable overtypes
        // the selection); see `keymap_select::translate_select`.
        // SN.3d.4: unlike Visual above, Select DOES consult active
        // minor-mode keymaps (it takes `ctx.active_minor_modes`), so a
        // mode that focuses a span — the snippet placeholder default —
        // keeps its `<Tab>` / `<S-Tab>` / `<Esc>` bindings live in
        // Select exactly as in Insert. Visual's minor-mode layer push
        // is still outstanding (the comment above).
        ModalState::Select(kind) => crate::keymap_select::translate_select(
            ctx.keymap,
            &chord,
            kind,
            ctx.partial_chord,
            ctx.active_minor_modes,
        ),
        // Slice 8.d: Replace mode dispatches through the
        // layered registry. `translate_replace`'s legacy match
        // table moved to `keymap_replace::register_replace_bindings`
        // + the `dispatch_replace` adapter; the drift test in
        // `keymap_replace::tests` keeps both honest until 8.i
        // retires the legacy reference.
        ModalState::Replace => dispatch_replace(ctx.keymap, &chord),
        // OperatorPending routes to no-op (it's a transient resolution
        // state inside translate_normal, not a top-level reachable state).
        _ => Action::None,
    }
}

fn translate_search(chord: KeyChord) -> Action {
    match chord.key {
        KeyKind::Special(SpecialKey::Esc) => Action::SearchCancel,
        KeyKind::Special(SpecialKey::Enter) => Action::SearchSubmit,
        KeyKind::Special(SpecialKey::Backspace) => Action::SearchBackspace,
        KeyKind::Char(c) if !chord.mods.ctrl() => Action::SearchAppend(c),
        _ => Action::None,
    }
}

fn translate_command(chord: KeyChord, completion_open: bool, _chord_capture: bool) -> Action {
    // Note: chord-capture is dispatched at the top-level
    // `translate()` (so it precedes the universal Ctrl-C quit).
    // This signature still takes the bit so call sites stay
    // explicit, but if we reach here the overlay is off.

    // The completion popup claims a small set of keys first
    // (Tab / S-Tab / Enter / Esc / C-n / C-p) -- two-stage Esc
    // per DESIGN.md §5.11.3 Q6: first Esc dismisses the popup,
    // second cancels the command line. Other keys fall through;
    // appending text implicitly dismisses the popup (the App
    // handler clears `completion_state` on every typed char).
    //
    // Issue #22 (2026-05-22): `<C-n>` / `<C-p>` symmetrical with
    // picker's `translate_picker` (`Action::PickerSelectNext` /
    // `PickerSelectPrev` for the same chords). User reported
    // these were unbound in cmdline completion; only Tab / S-Tab
    // worked. Now both pairs navigate identically.
    if completion_open {
        match chord.key {
            KeyKind::Special(SpecialKey::Tab) => {
                return if chord.mods.shift() {
                    Action::CommandLineCompletePrev
                } else {
                    Action::CommandLineCompleteOrAdvance
                };
            }
            KeyKind::Char('n') if chord.mods.ctrl() => {
                return Action::CommandLineCompleteOrAdvance;
            }
            KeyKind::Char('p') if chord.mods.ctrl() => {
                return Action::CommandLineCompletePrev;
            }
            KeyKind::Special(SpecialKey::Enter) => return Action::CommandLineAcceptCompletion,
            KeyKind::Special(SpecialKey::Esc) => return Action::CommandLineDismissCompletion,
            _ => {}
        }
    }

    if chord.mods.ctrl() {
        return match chord.key {
            KeyKind::Char('h') => Action::CommandLineDescribeUnderCursor,
            KeyKind::Char('u') => Action::CommandLineClear,
            KeyKind::Char('w') => Action::CommandLineDeleteWordBackward,
            _ => Action::None,
        };
    }

    match chord.key {
        KeyKind::Special(SpecialKey::Esc) => Action::CommandLineCancel,
        KeyKind::Special(SpecialKey::Enter) => Action::CommandLineSubmit,
        KeyKind::Special(SpecialKey::Backspace) => Action::CommandLineBackspace,
        KeyKind::Special(SpecialKey::Tab) if !chord.mods.shift() => {
            Action::CommandLineCompleteOrAdvance
        }
        KeyKind::Special(SpecialKey::Tab) if chord.mods.shift() => Action::CommandLineCompletePrev,
        KeyKind::Special(SpecialKey::Up) => Action::CommandLineHistoryPrev,
        KeyKind::Special(SpecialKey::Down) => Action::CommandLineHistoryNext,
        KeyKind::Char(c) if !chord.mods.ctrl() => Action::CommandLineAppend(c),
        _ => Action::None,
    }
}

/// Cmdline chord-capture overlay. Reserves the three minimal
/// edits (Esc/CR/BS); everything else stringifies through
/// `KeyChord::Display` and becomes one chord token in the cmdline.
fn translate_command_chord_capture(chord: KeyChord) -> Action {
    // Reserved keys -- these never become chord tokens because
    // they're how the user finishes / aborts / corrects. To look
    // up `<Esc>` / `<CR>` themselves, use the missing-arg prompt
    // path (`:describe-key<CR>` with no arg) which captures the
    // very next event.
    match chord.key {
        KeyKind::Special(SpecialKey::Esc) => return Action::CommandLineCancel,
        KeyKind::Special(SpecialKey::Enter) => return Action::CommandLineSubmit,
        KeyKind::Special(SpecialKey::Backspace) => return Action::CommandLineDeleteChord,
        _ => {}
    }
    Action::CommandLineAppendChord(chord.to_string())
}

/// Picker-overlay key router. See [`lattice_picker::Picker`] for
/// the data shape. Reserved keys (Esc / CR / BS / arrows /
/// Ctrl-{n,p,c}) drive the picker's intrinsic actions; printable
/// chars append to the query; everything else is swallowed.
fn translate_picker(chord: KeyChord) -> Action {
    if chord.mods.ctrl() {
        return match chord.key {
            // C-c dismisses the picker (not the app) so the user
            // can always abort.
            KeyKind::Char('c') => Action::PickerDismiss,
            KeyKind::Char('n') => Action::PickerSelectNext,
            KeyKind::Char('p') => Action::PickerSelectPrev,
            // C-u clears the query in one stroke (vim's cmdline
            // shortcut, applied here for consistency).
            KeyKind::Char('u') => Action::PickerBackspace, // approximate; per-char today
            // Issue #32 (2026-05-22): open candidate file in
            // split / vsplit / tab. File-targeting outcomes
            // route through the override; non-file outcomes
            // ignore it (same as `<CR>`).
            KeyKind::Char('s') => Action::PickerAcceptInSplit,
            KeyKind::Char('v') => Action::PickerAcceptInVSplit,
            KeyKind::Char('t') => Action::PickerAcceptInTab,
            _ => Action::None,
        };
    }
    match chord.key {
        KeyKind::Special(SpecialKey::Esc) => Action::PickerDismiss,
        KeyKind::Special(SpecialKey::Enter) => Action::PickerAccept,
        KeyKind::Special(SpecialKey::Backspace) => Action::PickerBackspace,
        KeyKind::Special(SpecialKey::Up) => Action::PickerSelectPrev,
        KeyKind::Special(SpecialKey::Down) => Action::PickerSelectNext,
        KeyKind::Special(SpecialKey::Tab) if !chord.mods.shift() => Action::PickerSelectNext,
        KeyKind::Special(SpecialKey::Tab) if chord.mods.shift() => Action::PickerSelectPrev,
        KeyKind::Char(c) if !chord.mods.ctrl() => Action::PickerAppend(c),
        _ => Action::None,
    }
}

fn translate_normal(
    chord: KeyChord,
    builtins: &Builtins,
    pending_count: u32,
    op_count: u32,
    recording_macro: bool,
    keymap: &KeymapHandle,
    partial_chord: &[KeyChord],
    active_minor_modes: &[lattice_mode::ModeId],
) -> Action {
    // Slice 8.g.iv: every Normal-mode action flows through
    // `attach_count` so motion / operator counts are baked into
    // the resolved `CommandInvocation` before the action leaves
    // translate. App's dispatcher reads `inv.count` directly
    // (no separate `pending_count * op_count` math at the
    // dispatch site any more) -- only fold-aware count
    // *expansion* stays App-side because it depends on the
    // active fold model.
    let action = compute_normal_action(
        chord,
        builtins,
        pending_count,
        recording_macro,
        keymap,
        partial_chord,
        active_minor_modes,
    );
    crate::keymap_normal::attach_count(action, pending_count, op_count)
}

fn compute_normal_action(
    chord: KeyChord,
    builtins: &Builtins,
    pending_count: u32,
    recording_macro: bool,
    keymap: &KeymapHandle,
    partial_chord: &[KeyChord],
    active_minor_modes: &[lattice_mode::ModeId],
) -> Action {
    let _ = builtins;
    // Slice 8.i.4: every multi-key Normal-mode chord flows through
    // `App::partial_chord`. When non-empty, peek the full path through
    // the trie ONCE: `Bound` resolves to the bound action, a deeper
    // `Partial` returns `Action::AbsorbPartialChord`, and `Unbound`
    // returns `Action::None` (which `App::apply` turns into a
    // partial_chord clear). We compute it up front because the digit
    // hoist below needs to know whether this key completes a chord.
    let prefix_action = (!partial_chord.is_empty()).then(|| {
        crate::keymap_normal::lookup_normal_with_prefix(
            keymap,
            partial_chord,
            &chord,
            active_minor_modes,
        )
    });

    // Numeric prefix: `1`-`9` always start (or extend) a count; `0`
    // extends an in-progress count but otherwise is line_start. This is
    // vim's standard count parsing.
    //
    // Slice 8.i.4.f: digit handling must run BEFORE falling into the
    // partial_chord continuation. Without it, typing `2` after `d`
    // (partial_chord=['d']) routes to `lookup_normal_with_prefix(['d'],
    // '2')` -- unbound -- aborting the operator; vim flows like `d2w`,
    // `2d3w`, `5gg` would never see the digit.
    //
    // S2 (emacs-keys) refinement: the one exception the original 8.i.4.f
    // comment anticipated -- when the pending prefix actually BINDS this
    // key as a chord (`Bound`/deeper `Partial`, i.e. `prefix_action` is
    // not `None`), the digit is the chord's literal second key, not a
    // count, so the trie wins. This is mode-agnostic: any layer binding a
    // `[prefix, digit]` chord (emacs-keys `<C-x>2` / `<C-x>3`) benefits.
    let prefix_resolves_chord = matches!(prefix_action, Some(ref a) if !matches!(a, Action::None));
    if !prefix_resolves_chord
        && let KeyKind::Char(c) = chord.key
        && let Some(digit) = c.to_digit(10)
        && (digit > 0 || pending_count > 0)
    {
        return Action::PushDigit(digit as u8);
    }

    if let Some(action) = prefix_action {
        return action;
    }

    // `q` while a macro is recording stops the recording. The
    // trie's `[q]` binding arms `Pending::AfterMacroStart`, but
    // that's the wrong action when the user is mid-recording --
    // the App-side `recording_macro` state determines which
    // path to take, and the trie is stateless. Short-circuit
    // here so `lookup_normal` doesn't see the `q`.
    if recording_macro && matches!(chord.key, KeyKind::Char('q')) {
        return Action::StopMacroRecord;
    }

    // Slice 8.g.vi closes out: every Normal-mode chord -- bare,
    // SHIFT-cased, CTRL-bearing, multi-key prefix, wildcard --
    // now lives in the layered registry under
    // `BindingMode::Normal`. The dispatcher reduces to: pending
    // resolution -> digit prefix -> recording-`q` short-circuit
    // -> trie lookup. `lookup_normal` returns `Some(action)` for
    // any matched chord; on `None` we fall through to
    // `Action::None`.
    crate::keymap_normal::lookup_normal(keymap, &chord, active_minor_modes).unwrap_or(Action::None)
}
