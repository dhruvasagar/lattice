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
    if ctx.terminal_insert_active
        && matches!(ctx.active_buffer, BufferKind::Terminal)
    {
        if chord == KeyChord::ctrl('\\') {
            // The `<C-\>` half of the exit sequence. T2.a
            // approximation: emit `ExitTerminalInsert` directly.
            // T2.c upgrades to "two-key armed" so the next
            // chord (`<C-n>`) confirms the exit and a stray
            // `<C-n>` after some other key still encodes to
            // `\x0e` (shell's next-history).
            return Action::ExitTerminalInsert;
        }
        if let Some(bytes) = crate::keymap_terminal::key_to_ansi(&chord) {
            return Action::TerminalInput(bytes);
        }
        // Unmapped special key (e.g. arrow keys in T2.a) —
        // silent no-op so the user isn't stuck. T2.b removes
        // this branch by fleshing out the encoder.
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

    // Universal escape hatch.
    if chord == KeyChord::ctrl('c') {
        return Action::Quit;
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

    // Terminal-mode T2.a (2026-05-25) — Normal-in-terminal
    // buffer-local bindings. `i` enters `terminal-insert-mode`
    // (so subsequent keystrokes encode to PTY input via the
    // earlier short-circuit). T2.b adds `a` / `I` / `A` here;
    // T2.c adds `<C-w>` window-prefix routing. Other keys fall
    // through to the standard normal-mode grammar (the
    // scrollback-motion path lands in T3).
    if matches!(ctx.active_buffer, BufferKind::Terminal)
        && !ctx.terminal_insert_active
        && matches!(ctx.modal, ModalState::Normal)
        && ctx.partial_chord.is_empty()
    {
        if let KeyKind::Char('i') = chord.key {
            if !chord.mods.ctrl() && !chord.mods.alt() {
                return Action::EnterTerminalInsert;
            }
        }
    }

    match ctx.modal {
        // Slice 8.f: Insert mode dispatches through the layered
        // registry. Base bindings live in
        // `keymap_insert::register_insert_bindings`; the
        // completion-popup and active-snippet overlays ride as
        // `KeymapLayer::MinorMode` layers managed by
        // `App::sync_keymap_overlays`. The drift test in
        // `keymap_insert::tests` is the regression net.
        ModalState::Insert => dispatch_insert(ctx.keymap, &chord, ctx.partial_chord),
        ModalState::Normal => translate_normal(
            chord,
            ctx.builtins,
            ctx.pending_count,
            ctx.op_count,
            ctx.recording_macro,
            ctx.keymap,
            ctx.partial_chord,
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
        ModalState::Visual(kind) => dispatch_visual(ctx.keymap, &chord, kind),
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
) -> Action {
    let _ = builtins;
    // Numeric prefix: `1`-`9` always start (or extend) a count;
    // `0` extends an in-progress count but otherwise is
    // line_start. This is vim's standard count parsing.
    //
    // Slice 8.i.4.f: digit handling must run BEFORE the
    // partial_chord short-circuit. Without this hoist, typing
    // `2` after `d` (partial_chord=['d']) routes to
    // `lookup_normal_with_prefix(['d'], '2')` -- unbound -- which
    // returns `Action::None` and silently aborts the operator.
    // Vim flows like `d2w`, `2d3w`, `5gg` would never see the
    // digit. Safe to hoist because no built-in chord has a digit
    // as second key (verified 8.i.4.f: no `[d, digit]`,
    // `[g, digit]`, `[<C-w>, digit]`, etc.). If a future plugin
    // wants `[X, digit]` chords the rule grows to "digit handler
    // unless `[partial_chord, digit]` is bound" -- the registry
    // already has the data needed.
    if let KeyKind::Char(c) = chord.key
        && let Some(digit) = c.to_digit(10)
        && (digit > 0 || pending_count > 0)
    {
        return Action::PushDigit(digit as u8);
    }

    // Slice 8.i.4: every multi-key Normal-mode chord flows
    // through `App::partial_chord`. When non-empty, the next
    // keystroke routes through the trie with this stack as
    // prefix; the trie's `Bound` resolves the full path,
    // `Partial` absorbs into the stack via
    // `Action::AbsorbPartialChord`, and `Unbound` returns
    // `Action::None` (which `App::apply` turns into a
    // partial_chord clear).
    if !partial_chord.is_empty() {
        return crate::keymap_normal::lookup_normal_with_prefix(keymap, partial_chord, &chord);
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
    crate::keymap_normal::lookup_normal(keymap, &chord).unwrap_or(Action::None)
}
