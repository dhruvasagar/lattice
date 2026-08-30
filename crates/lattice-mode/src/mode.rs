//! The `Mode` trait, plus `ModeId`, `ModeKind`, and the
//! [`LifecycleFuture`] type alias.

use std::any::Any;
use std::future::Future;
use std::pin::Pin;

pub use lattice_keymap::ModeId;

use crate::action_handler_registry::ActionHandlerContribution;
use crate::capability::CapabilitySet;
use crate::context::ModeContext;
use crate::contributions::{DecorationCtx, DecorationProvider, GutterDecoration, Keymap};
use crate::error::ModeActivationError;
use lattice_config::OptionOverrideSet;
use lattice_core::BufferKind;

/// Major / minor distinction. A buffer has exactly one major and
/// any number of minors active simultaneously
/// (mode-architecture.md §3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModeKind {
    Major,
    Minor,
}

/// A minor mode's *default* auto-activation policy — the allowlist of
/// major modes it activates inside, as the mode itself ships it
/// (mode-architecture.md §7.4). The host's minor-activation resolver
/// subscribes once to [`lattice_protocol::Event::MajorEntered`] and,
/// for each registered minor whose policy [`admits`](Self::admits)
/// the entered major, activates it.
///
/// This is the mode's *declared default*. Config
/// (`<mode>.activation = global | <allowlist> | off`) folds over it;
/// that fold is the host's job (SN.3), not the mode's. The default on
/// the `Mode` trait is [`Manual`](Self::Manual): a mode auto-activates
/// nowhere until it opts in or the user does. Leaving the onus on the
/// user is a legitimate choice — some modes won't ship a sensible
/// default and shouldn't guess.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum ActivationPolicy {
    /// Never auto-activate; only explicit (user / host / `:<mode>`)
    /// activation turns the mode on. The trait default.
    #[default]
    Manual,
    /// Auto-activate on every **document** buffer that enters a major
    /// mode (scoped to [`BufferKind::Document`]). The right policy for
    /// content modes (snippets, LSP, …) that only make sense over
    /// user-edited text, not synthetic UI buffers.
    Global,
    /// Auto-activate on **every** buffer kind that enters a major mode —
    /// documents *and* synthetic UI buffers (`*messages*`, help, file
    /// tree, oil, terminal). For *universal* contributions like the
    /// `emacs-keys` `<C-x>` leader, where navigation chords (switch
    /// buffer, switch pane, quit) should work everywhere the user can
    /// focus — mirroring emacs, whose `C-x` map is live in `*Messages*`
    /// and every other buffer. NOT for content modes (use [`Global`]).
    /// Mode-local keymaps are gated by binding mode, so Terminal-Insert
    /// keystroke passthrough is unaffected by a Normal-only leader.
    ///
    /// [`Global`]: Self::Global
    Universal,
    /// Auto-activate only when the entered major's id is in this
    /// allowlist. An empty list behaves like [`Manual`](Self::Manual)
    /// (matches no major).
    Majors(Vec<ModeId>),
}

impl ActivationPolicy {
    /// Does this policy auto-activate when a buffer of kind
    /// `buffer_kind` enters the major mode named `major`?
    ///
    /// `Global` is scoped to **real document buffers**
    /// ([`BufferKind::Document`]) — every code/text buffer, not the
    /// synthetic UI buffers (file tree, help, `*messages*`, terminal,
    /// …). `Universal` admits every kind (documents *and* synthetic
    /// buffers) for universal-leader modes. A mode that wants a narrow
    /// synthetic opt-in instead names that buffer's major explicitly
    /// via `Majors([..])`, which is kind-independent.
    pub fn admits(&self, major: &str, buffer_kind: BufferKind) -> bool {
        match self {
            Self::Manual => false,
            Self::Global => buffer_kind == BufferKind::Document,
            Self::Universal => true,
            Self::Majors(allow) => allow.iter().any(|m| m.as_str() == major),
        }
    }
}

/// AU‑3: an editable region at the **tail** of an otherwise read-only,
/// owner-written buffer — the comint pattern (the agent-conversation prompt,
/// future `*scratch*` / REPL input lines). A mode declares it via
/// [`Mode::editable_tail`]; the host's read-only edit gate consults it so
/// user keystrokes may edit only the tail while the owner's projection writes
/// (which bypass the gate by going through the runtime document handle
/// directly) keep the rest owner-controlled.
///
/// The region is expressed **structurally, relative to the buffer end**, not
/// as an absolute position — so it stays valid as the owner appends content
/// above the tail without any per-edit bookkeeping:
///
/// - `trailing_lines` — the number of trailing lines that form the region
///   (`1` for a single-line prompt). The first editable line is
///   `line_count - trailing_lines`.
/// - `first_line_min_byte` — the minimum byte column on that first editable
///   line, protecting a prompt marker rendered as buffer text (e.g. the
///   `"> "` prefix ⇒ `2`). Lines strictly after the first are editable from
///   column 0.
///
/// `Default` is the empty tail (`trailing_lines = 0`), i.e. nothing editable.
///
/// ## Bottom-relative vs. anchored
///
/// The bottom-relative `trailing_lines` encoding is correct for a *fixed-height*
/// tail: it stays valid as the owner appends content ABOVE the tail, but breaks
/// the moment the user grows the tail itself (a multi-line prompt), because the
/// added lines push the marker line out of the region. For a prompt whose height
/// changes with user newlines AND whose top drifts as a transcript streams above
/// it, set [`first_editable_line`](Self::first_editable_line) to the ABSOLUTE
/// line where the editable region begins (the transcript-end line); the owning
/// mode updates it as the transcript grows. When set it overrides
/// `trailing_lines`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct EditableTail {
    /// Number of trailing lines forming the editable region. Ignored when
    /// [`first_editable_line`](Self::first_editable_line) is `Some`.
    pub trailing_lines: u32,
    /// Minimum editable byte column on the first editable line (guards a
    /// text-rendered prompt marker). Ignored for lines after the first.
    pub first_line_min_byte: u32,
    /// When `Some(anchor)`, the editable region is `anchor..EOF` (absolute),
    /// overriding the bottom-relative `trailing_lines`. Lets a mode with a
    /// multi-line, growing prompt anchor the region to the transcript end and
    /// keep it correct as the user adds newlines. Clamped to the last line so a
    /// stale-high anchor never freezes the whole buffer.
    pub first_editable_line: Option<u32>,
}

impl EditableTail {
    /// Is a keystroke edit whose earliest affected position is
    /// `(start_line, start_byte)` permitted, given the buffer currently has
    /// `line_count` lines? Pure + unit-testable: the host gate computes the
    /// live `line_count` from the document snapshot and delegates here.
    pub fn permits(&self, start_line: u32, start_byte: u32, line_count: u32) -> bool {
        let first_editable = match self.first_editable_line {
            // Absolute anchor: `anchor..EOF`, clamped so a stale-high anchor
            // still leaves the last line editable rather than freezing the tail.
            Some(anchor) => anchor.min(line_count.saturating_sub(1)),
            None => {
                if self.trailing_lines == 0 {
                    return false;
                }
                line_count.saturating_sub(self.trailing_lines)
            }
        };
        if start_line < first_editable {
            return false;
        }
        if start_line == first_editable && start_byte < self.first_line_min_byte {
            return false;
        }
        true
    }
}

/// Pinned, boxed, send-able future for `Mode::on_activate`.
///
/// The explicit `Pin<Box<dyn Future + Send>>` desugaring (rather
/// than `async fn` in trait) is needed because:
///
/// 1. **Object safety.** [`Mode`] has an associated type
///    ([`Mode::Guard`]) and is not directly object-safe. The
///    dispatcher stores modes as `Arc<dyn DynMode>` via the
///    [`DynMode`](crate::DynMode) adapter; the adapter's
///    `on_activate_dyn` returns a future whose output is
///    type-erased to `Box<dyn Any + Send>`.
/// 2. **`Send` bound.** Lifecycle futures may be scheduled across
///    threads (M-async.2 swaps `poll_now` for runtime-spawned
///    `.await`); the future itself must be `Send` so the executor
///    can move it between worker threads.
/// 3. **Explicit lifetime.** Modes capture their `&self` and the
///    [`ModeContext`] (owned, `Send + 'static`); the future's
///    lifetime is tied to `&self` via `'a`.
///
/// The default type parameter `T = ()` lets marker modes write
/// `LifecycleFuture<'_>` without naming the unit type.
pub type LifecycleFuture<'a, T = ()> =
    Pin<Box<dyn Future<Output = Result<T, ModeActivationError>> + Send + 'a>>;

/// Declarative mode contract.
///
/// Per mode-architecture.md §5.2 + §7.1, this trait splits into
/// three concerns:
///
/// 1. **Declarative methods** (`options`, `keymap`,
///    `subscriptions`, `decorations`, `required_capabilities`,
///    `conflicts_with`, `implies`, `completion_sources`,
///    `mirrors_option`) return read-only data. The registry
///    applies these to the layer stack on activation and removes
///    them on deactivation. The mode can never leak contributions
///    past its lifetime by construction.
/// 2. **Lifecycle hook** ([`Mode::on_activate`]) returns an
///    owned [`Guard`](Mode::Guard) value carrying every resource
///    the mode allocated (subscription IDs, prior option values
///    to restore, supervisor handles, etc.). The dispatcher
///    stashes the Guard in a [`GuardStore`](crate::GuardStore)
///    keyed by `(BufferId, ModeId)`.
/// 3. **Deactivation cleanup.** There is **no `on_deactivate`**.
///    On deactivation the dispatcher drops the stashed Guard;
///    the Guard's `Drop` impl performs every cleanup action.
///    This makes cleanup mandatory (compiler-enforced via
///    Rust ownership), bug-resistant (a forgotten cleanup step
///    becomes a compile-time leak rather than a runtime resource
///    leak), and uniform (marker modes use `()` as Guard).
///
/// Validated against Zed's `Subscription` / `Task<T>` cancel-on-
/// drop pattern and helix's Rust-ownership-based cleanup; see
/// mode-architecture.md §7.1.
///
/// `Send + Sync + 'static` so a single trait object can be shared
/// across threads (the registry runs on whatever task drives
/// activation; subscribers can be on any task).
pub trait Mode: Send + Sync + 'static {
    /// Owned cleanup token returned by [`Self::on_activate`].
    ///
    /// The mode allocates whatever resources it needs (event
    /// subscriptions, supervisor handles, prior option values
    /// to restore) and packages them in a Guard struct with a
    /// `Drop` impl that performs cleanup. Marker modes that
    /// have no cleanup work use `()`.
    ///
    /// `Send + 'static` so the dispatcher can stash the Guard
    /// in a typed-erased `Box<dyn Any + Send>` and move it
    /// across threads if needed.
    type Guard: Send + 'static;

    /// Canonical identity. Same value every call.
    fn id(&self) -> ModeId;

    /// Major / minor.
    fn kind(&self) -> ModeKind;

    /// H.2 (2026-05-31): for major modes, the [`BufferKind`] this
    /// mode is the default major for. `ModeRegistry::register`
    /// indexes this so [`ModeRegistry::find_major_for_kind`] can
    /// dispatch buffer-creation events to the right major without
    /// host-side `match BufferKind { ... }` blocks.
    ///
    /// Returns `None` for:
    /// - All minor modes.
    /// - Major modes that don't bind to a [`BufferKind`] directly
    ///   (e.g. language majors like `rust-mode` / `markdown-mode`
    ///   on plain Documents — they activate via `Lang` detection
    ///   on [`BufferKind::Document`], not via kind dispatch).
    ///
    /// One [`BufferKind`] is owned by at most one major; the
    /// registry treats the first registration as authoritative
    /// and warns on subsequent claims (clobbering is a
    /// developer bug, not an extensibility seam).
    ///
    /// Note: a single major may be referenced by *both* a kind and
    /// a `Lang` (e.g. `markdown-mode` is the major for
    /// [`BufferKind::Help`] and also the language major for
    /// [`BufferKind::Document`] + `Lang::Markdown`). Declaring
    /// `target_buffer_kind = Some(Help)` does not exclude the
    /// `Lang`-detected dispatch path — they cohabit.
    fn target_buffer_kind(&self) -> Option<BufferKind> {
        None
    }

    /// OM.1: for major modes, the **language** this mode is the
    /// default major for, by canonical name (`Lang::name()` —
    /// `"rust"`, `"org"`). The peer of
    /// [`target_buffer_kind`](Self::target_buffer_kind) for the
    /// dispatch path [`BufferKind::Document`] takes: language
    /// detection rather than kind dispatch.
    ///
    /// `ModeRegistry::register` indexes this so
    /// [`ModeRegistry::find_major_for_lang`] can resolve a
    /// document's major without a host-side `match Lang { ... }`,
    /// which is what makes a **plugin-contributed** language's
    /// major possible at all: `Lang::Plugin(_)` has no arm in the
    /// host's hand-written table and never will, because the host
    /// does not know the language exists until a plugin says so.
    ///
    /// Returns `None` for:
    /// - All minor modes. A minor declaring one is ignored at
    ///   register-time rather than indexed — resolving a minor as
    ///   a buffer's major would corrupt activation.
    /// - Major modes not bound to a language (kind-bound majors
    ///   like `file-tree-mode`, or manual-only majors).
    ///
    /// One language is owned by at most one major; first
    /// registration wins and later claims warn, matching
    /// `target_buffer_kind`.
    ///
    /// The built-in language majors (`rust-mode`, `markdown-mode`,
    /// …) do **not** declare this yet — they resolve through
    /// `lattice_syntax::major_mode_id_for_lang`'s table, which is
    /// consulted first. Migrating them onto this index would
    /// collapse that table, and is deliberately left as separate
    /// work: this slice makes plugin languages reachable, it does
    /// not rewrite how the built-ins resolve.
    fn target_language(&self) -> Option<&str> {
        None
    }

    /// Option overrides this mode contributes. Pure declarative
    /// (same return value every call); the registry merges these
    /// into the resolution layer stack on activation.
    fn options(&self) -> OptionOverrideSet {
        OptionOverrideSet::default()
    }

    /// Keymap chord -> command additions / overrides. Layered
    /// into the existing keymap registry at this mode's priority
    /// slot.
    fn keymap(&self) -> Keymap {
        Keymap::default()
    }

    /// Decoration providers (gutter / inline / overlay /
    /// statusline). Stub — reserved for the WIT plugin path (M.10).
    fn decorations(&self) -> Vec<DecorationProvider> {
        Vec::new()
    }

    /// Gutter sign decorations this mode contributes while active.
    /// Called once per pane per frame with a [`DecorationCtx`]
    /// carrying relevant render-state snapshots (diff sign map, LSP
    /// diagnostics arc). Returns per-line `GutterDecoration` values;
    /// the renderer partitions them by variant into the appropriate
    /// gutter column. Default: empty (no contribution).
    fn gutter_decorations(&self, _ctx: &DecorationCtx<'_>) -> Vec<GutterDecoration> {
        Vec::new()
    }

    // ML.3: `status_line_items` retired. Modes contribute modeline
    // content as registered elements pushed over the event bus
    // (`lattice_mode::ModelineElementUpdate`, see modeline.rs §6), not via
    // a render-path trait pull — a Rust trait can't cross the WASM plugin
    // boundary, which is exactly the limitation the element model removes.

    /// Insert-mode completion sources this mode contributes while
    /// active on a buffer. Empty by default; minors that own a
    /// completion source (`lsp-completion-mode`,
    /// `snippet-completion-mode`, `buffer-words-mode`,
    /// `tree-sitter-completion-mode`, `path-completion-mode`,
    /// plugin sources) override.
    fn completion_sources(&self) -> Vec<lattice_completion::CompletionSourceContribution> {
        Vec::new()
    }

    /// SN.3c.0: *global* (buffer-agnostic) action handlers this
    /// mode contributes. The host walks every registered mode's
    /// `action_handlers()` once at boot, resolves each
    /// `action_name` → `CommandId`, registers the handler in the
    /// `ActionHandlerRegistry`, and holds the tokens for the app's
    /// lifetime. Use this for handlers that read the active
    /// buffer / cursor / services from the `ActionContext` at call
    /// time and close over no per-buffer state (e.g. snippet
    /// expand). Per-buffer, session-scoped handlers register in
    /// [`on_activate`](Self::on_activate) instead, so their tokens
    /// drop with the Guard. Default: none. See
    /// `feedback_effect_vocabulary_is_host_boundary`.
    fn action_handlers(&self) -> Vec<ActionHandlerContribution> {
        Vec::new()
    }

    /// Capabilities the mode requires. Validated at activation;
    /// missing capability ⇒
    /// [`ModeActivationError::MissingCapability`], never silent
    /// skip.
    fn required_capabilities(&self) -> CapabilitySet {
        CapabilitySet::empty()
    }

    /// Conflicts. Activating this mode auto-deactivates the
    /// listed minor modes, OR fails if a conflicting major is
    /// active.
    fn conflicts_with(&self) -> &[ModeId] {
        &[]
    }

    /// Implies. Activating this mode auto-activates these.
    /// Used by `relative-line-numbers-mode` ⇒ `line-numbers-mode`.
    fn implies(&self) -> &[ModeId] {
        &[]
    }

    /// Declarative mirror hint for "this mode is the on/off
    /// switch for a typed option of the same observable state".
    /// `Some(canonical_name)` ⇒ a host-driven cascade keeps the
    /// mode's active state and the option's value in sync.
    fn mirrors_option(&self) -> Option<&'static str> {
        None
    }

    /// 2026-05-26: invocation-runner discovery. Modes that own
    /// command-invocation dispatch for their buffer kind
    /// (terminal-mode, oil-mode, file-tree-mode, help-mode, …)
    /// return their canonical [`ModeId`]; the host registers a
    /// runner function under that id at boot, and
    /// `Editor::run_invocation` looks it up by walking the
    /// active modes on the active pane's buffer (minors first,
    /// then major) before falling back to the central grammar
    /// Action gate.
    ///
    /// Returning `None` (the default) means the mode doesn't
    /// claim invocation dispatch — the keymap / decorations /
    /// completion-source contributions still apply.
    ///
    /// Replaces the hardcoded `match BufferKind` block that
    /// previously lived in `Editor::run_invocation`. Plugin-
    /// installed modes for plugin-installed buffer kinds now
    /// extend the dispatcher without touching host code.
    fn invocation_runner(&self) -> Option<ModeId> {
        None
    }

    /// RV.1 (2026-08-10): which of *this mode's own actions* refreshes
    /// its view, or `None` (the default) when the mode backs nothing
    /// refreshable.
    ///
    /// `gr` means "refresh this view" in every synthetic buffer. That
    /// is a property of synthetic views as a class, so the chord lives
    /// once on [`RefreshableViewMode`](crate::RefreshableViewMode) —
    /// **not** re-declared per mode. Before RV.1 it was re-declared per
    /// mode, and the two views that landed most recently (`*problems*`,
    /// narrow) had no `gr` at all: a gap in a copied set does not
    /// announce itself.
    ///
    /// This declares a **target, not a body**. The handler stays exactly
    /// where [`action_handlers`](Self::action_handlers) puts it — a mode
    /// returning `Some("action:magit-refresh")` keeps the closure it
    /// already registered under that name. Declaring a target rather
    /// than doing the work is the same shape
    /// [`invocation_runner`](Self::invocation_runner) and
    /// [`mirrors_option`](Self::mirrors_option) already have; a
    /// `refresh(&self, ctx)` doing the work would give modes two ways to
    /// express one body.
    ///
    /// Returning `Some` also **auto-activates** `refreshable-view-mode`
    /// through the implies cascade, so a mode author writes one line and
    /// gets the chord.
    ///
    /// The host resolves this by walking the buffer's active modes
    /// (minors most-recently-activated first, then major) — see
    /// `Editor::resolve_refresh_action`. When no active mode declares
    /// one, the chord echoes `nothing to refresh here` rather than being
    /// swallowed, so the absence is spoken.
    ///
    /// See `docs/dev/architecture/mode-architecture.md` §5.5.
    fn refresh_action(&self) -> Option<&'static str> {
        None
    }

    /// Should re-opening this view **re-run its refresh**?
    ///
    /// A synthetic buffer is created once and reused: the host's
    /// `ensure_named_synthetic_document` returns the existing buffer by
    /// name, so a mode's `on_activate` — which is what fills the buffer
    /// — runs on the FIRST open only. For a view whose content is a
    /// snapshot of external state, that makes every later open a time
    /// capsule: `C-x g` on an already-open `*magit:status*` showed the
    /// repository as it was when the buffer was first created, with
    /// nothing on screen saying so.
    ///
    /// Returning `true` makes the host dispatch this mode's declared
    /// [`refresh_action`](Self::refresh_action) after an open that
    /// **reused** an existing buffer. First opens are untouched:
    /// `on_activate` has just built the content, and refreshing again
    /// would be a second scan for the same answer.
    ///
    /// **Opt-in, and only for content derived from outside the editor.**
    /// A view whose content is authored in the editor (a help page, a
    /// transcript, `*messages*`) has nothing to re-derive, and refreshing
    /// it would discard scroll position for no gain.
    ///
    /// **The contract on the body:** a refresh reached this way must be
    /// self-contained — spawn its own work and return no `Effect`. This
    /// path has no dispatch outcome to route renderer-coupled effects
    /// through (`OpenBuffer`, `OpenPicker`, …), so a returned effect is
    /// logged as a wiring error rather than half-applied. Magit's
    /// refresh satisfies this: it spawns the git work and returns
    /// `None`.
    ///
    /// Declaring `true` without a `refresh_action` does nothing; the two
    /// are read together.
    fn refresh_on_open(&self) -> bool {
        false
    }

    /// MA.1: a *minor* mode's default auto-activation policy
    /// (mode-architecture.md §7.4). The host's minor-activation
    /// resolver reads this for every registered minor when a buffer
    /// enters a major mode, and activates those whose policy
    /// [`admits`](ActivationPolicy::admits) the entered major. The
    /// default is [`ActivationPolicy::Manual`] — auto-activate
    /// nowhere until the mode or the user opts in. Ignored for major
    /// modes (a buffer's major is chosen by the major resolver, not
    /// this allowlist).
    fn activation_policy(&self) -> ActivationPolicy {
        ActivationPolicy::Manual
    }

    /// AU‑3: the mode's editable tail on an otherwise read-only buffer, or
    /// `None` (the default) for a fully read-only / fully writable buffer.
    ///
    /// A mode backing an owner-written buffer (the agent conversation,
    /// future REPL / scratch buffers) declares a tail so the host's
    /// read-only edit gate lets user keystrokes edit only the trailing
    /// prompt region — the comint pattern. Consulted directly by the gate
    /// (no per-buffer seeding): the tail is expressed relative to the buffer
    /// end (see [`EditableTail`]), so it stays valid as the owner appends
    /// content above it. Returning `None` leaves the read-only gate's
    /// behaviour unchanged (edits rejected iff `ReadOnly` is resolved true).
    fn editable_tail(&self) -> Option<EditableTail> {
        None
    }

    /// Lifecycle. Called once per (buffer, activation) cycle
    /// after the registry has applied the declarative
    /// contributions. Returns an owned [`Guard`](Self::Guard)
    /// carrying every resource the mode allocated. The
    /// dispatcher stashes the Guard until deactivation, at which
    /// point dropping it performs cleanup via the Guard's `Drop`
    /// impl.
    ///
    /// Marker modes whose `Guard = ()` typically write:
    ///
    /// ```ignore
    /// type Guard = ();
    /// fn on_activate(&self, _ctx: ModeContext) -> LifecycleFuture<'_, ()> {
    ///     Box::pin(async { Ok(()) })
    /// }
    /// ```
    ///
    /// Stateful modes return a Guard struct whose `Drop` impl
    /// performs cleanup (unsubscribe, restore prior option,
    /// drop supervisor handle, etc.).
    ///
    /// Errors propagate as [`ModeActivationError`]; do not panic.
    ///
    /// Idempotent setup contract: `on_activate` may run more
    /// than once in a buffer's lifetime (each preceded by a
    /// Guard-drop if previously active). Implementations must
    /// produce a fresh Guard every time.
    fn on_activate(&self, ctx: ModeContext) -> LifecycleFuture<'_, Self::Guard>;
}

/// Object-safe adapter for `Mode`. The registry stores modes
/// as `Arc<dyn DynMode>`; the blanket impl below box-erases
/// each `Mode`'s typed `Guard` into `Box<dyn Any + Send>` so
/// the dispatcher can stash heterogeneous Guards in a single
/// [`GuardStore`](crate::GuardStore) and drop them on
/// deactivation.
///
/// Public (not sealed): the trait is implemented automatically
/// for every `Mode`; consumers never implement `DynMode`
/// directly. Exposed in `pub` form because the registry's
/// public API (`Arc<dyn DynMode>`) leaks it.
pub trait DynMode: Send + Sync + 'static {
    fn id(&self) -> ModeId;
    fn kind(&self) -> ModeKind;
    fn target_buffer_kind(&self) -> Option<BufferKind>;
    fn target_language(&self) -> Option<&str>;
    fn options(&self) -> OptionOverrideSet;
    fn keymap(&self) -> Keymap;
    fn decorations(&self) -> Vec<DecorationProvider>;
    fn gutter_decorations(&self, ctx: &DecorationCtx<'_>) -> Vec<GutterDecoration>;
    fn completion_sources(&self) -> Vec<lattice_completion::CompletionSourceContribution>;
    fn action_handlers(&self) -> Vec<ActionHandlerContribution>;
    fn required_capabilities(&self) -> CapabilitySet;
    fn conflicts_with(&self) -> &[ModeId];
    fn implies(&self) -> &[ModeId];
    fn mirrors_option(&self) -> Option<&'static str>;
    fn invocation_runner(&self) -> Option<ModeId>;
    fn refresh_action(&self) -> Option<&'static str>;
    fn refresh_on_open(&self) -> bool;
    fn activation_policy(&self) -> ActivationPolicy;
    fn editable_tail(&self) -> Option<EditableTail>;

    /// Type-erased lifecycle entry. Returns a future whose
    /// output is the typed Guard erased to `Box<dyn Any + Send>`.
    /// The dispatcher stashes this box keyed by
    /// `(BufferId, ModeId)`; deactivation drops it.
    fn on_activate_dyn<'a>(
        &'a self,
        ctx: ModeContext,
    ) -> Pin<Box<dyn Future<Output = Result<Box<dyn Any + Send>, ModeActivationError>> + Send + 'a>>;
}

impl<M: Mode> DynMode for M {
    fn id(&self) -> ModeId {
        <M as Mode>::id(self)
    }
    fn kind(&self) -> ModeKind {
        <M as Mode>::kind(self)
    }
    fn target_buffer_kind(&self) -> Option<BufferKind> {
        <M as Mode>::target_buffer_kind(self)
    }
    fn target_language(&self) -> Option<&str> {
        <M as Mode>::target_language(self)
    }
    fn options(&self) -> OptionOverrideSet {
        <M as Mode>::options(self)
    }
    fn keymap(&self) -> Keymap {
        <M as Mode>::keymap(self)
    }
    fn decorations(&self) -> Vec<DecorationProvider> {
        <M as Mode>::decorations(self)
    }
    fn gutter_decorations(&self, ctx: &DecorationCtx<'_>) -> Vec<GutterDecoration> {
        <M as Mode>::gutter_decorations(self, ctx)
    }
    fn completion_sources(&self) -> Vec<lattice_completion::CompletionSourceContribution> {
        <M as Mode>::completion_sources(self)
    }
    fn action_handlers(&self) -> Vec<ActionHandlerContribution> {
        <M as Mode>::action_handlers(self)
    }
    fn required_capabilities(&self) -> CapabilitySet {
        <M as Mode>::required_capabilities(self)
    }
    fn conflicts_with(&self) -> &[ModeId] {
        <M as Mode>::conflicts_with(self)
    }
    fn implies(&self) -> &[ModeId] {
        <M as Mode>::implies(self)
    }
    fn mirrors_option(&self) -> Option<&'static str> {
        <M as Mode>::mirrors_option(self)
    }
    fn invocation_runner(&self) -> Option<ModeId> {
        <M as Mode>::invocation_runner(self)
    }
    fn refresh_action(&self) -> Option<&'static str> {
        <M as Mode>::refresh_action(self)
    }
    fn refresh_on_open(&self) -> bool {
        <M as Mode>::refresh_on_open(self)
    }
    fn activation_policy(&self) -> ActivationPolicy {
        <M as Mode>::activation_policy(self)
    }
    fn editable_tail(&self) -> Option<EditableTail> {
        <M as Mode>::editable_tail(self)
    }

    fn on_activate_dyn<'a>(
        &'a self,
        ctx: ModeContext,
    ) -> Pin<Box<dyn Future<Output = Result<Box<dyn Any + Send>, ModeActivationError>> + Send + 'a>>
    {
        let fut = <M as Mode>::on_activate(self, ctx);
        Box::pin(async move {
            let guard = fut.await?;
            Ok(Box::new(guard) as Box<dyn Any + Send>)
        })
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    /// AU‑3: the default `editable_tail()` is `None` (unchanged
    /// read-only semantics for every existing mode).
    #[test]
    fn editable_tail_defaults_to_none() {
        struct BareMode;
        impl Mode for BareMode {
            type Guard = ();
            fn id(&self) -> ModeId {
                ModeId::new("bare-mode")
            }
            fn kind(&self) -> ModeKind {
                ModeKind::Minor
            }
            fn on_activate(&self, _ctx: ModeContext) -> LifecycleFuture<'_, ()> {
                Box::pin(async { Ok(()) })
            }
        }
        assert_eq!(<BareMode as Mode>::editable_tail(&BareMode), None);
    }

    /// AU‑3: a single-line prompt tail (`> ` marker ⇒ min byte 2) permits
    /// edits on the last line at/after column 2 and rejects everything above
    /// it or before the marker — computed against the live line count, so it
    /// tracks the prompt as the transcript grows.
    #[test]
    fn editable_tail_permits_prompt_and_rejects_history() {
        let tail = EditableTail {
            trailing_lines: 1,
            first_line_min_byte: 2,
            first_editable_line: None,
        };
        // 5-line buffer: prompt is line 4 (`line_count - 1`).
        // In the prompt, at/after the marker → allowed.
        assert!(tail.permits(4, 2, 5));
        assert!(tail.permits(4, 7, 5));
        // In the prompt but inside the `> ` marker → rejected.
        assert!(!tail.permits(4, 0, 5));
        assert!(!tail.permits(4, 1, 5));
        // Any history line → rejected.
        assert!(!tail.permits(0, 0, 5));
        assert!(!tail.permits(3, 9, 5));
        // Grow the transcript: prompt is now line 9; the same rule tracks it.
        assert!(tail.permits(9, 2, 10));
        assert!(!tail.permits(4, 2, 10));
    }

    /// AU‑3+ (`<C-j>` multi-line prompt): an absolute anchor makes the region
    /// `anchor..EOF` regardless of the tail's height, so a growing multi-line
    /// prompt stays fully editable while everything above the anchor is frozen.
    #[test]
    fn anchored_editable_tail_covers_a_multiline_prompt() {
        // Transcript ends at line 3; the prompt is lines 3.. (marker on line 3).
        let tail = EditableTail {
            trailing_lines: 1,
            first_line_min_byte: 2,
            first_editable_line: Some(3),
        };
        // Marker line: at/after column 2 allowed, inside the marker rejected.
        assert!(tail.permits(3, 2, 6));
        assert!(!tail.permits(3, 0, 6));
        // Continuation prompt lines (added via `<C-j>`) are fully editable,
        // including column 0 (no marker there) — this is what a 1-line tail
        // could not express.
        assert!(tail.permits(4, 0, 6));
        assert!(tail.permits(5, 0, 6));
        // Transcript lines above the anchor stay frozen.
        assert!(!tail.permits(2, 0, 6));
        assert!(!tail.permits(0, 0, 6));
        // A stale-high anchor clamps to the last line rather than freezing all.
        let stale = EditableTail {
            trailing_lines: 1,
            first_line_min_byte: 2,
            first_editable_line: Some(99),
        };
        assert!(stale.permits(5, 2, 6));
    }

    /// AU‑3: an empty tail (`trailing_lines = 0`, the `Default`) permits
    /// nothing — a fully read-only buffer.
    #[test]
    fn empty_editable_tail_permits_nothing() {
        let tail = EditableTail::default();
        assert!(!tail.permits(0, 0, 3));
        assert!(!tail.permits(2, 5, 3));
    }

    /// A bare `Mode` impl with `Guard = ()` and a trivial
    /// `on_activate`. Confirms `completion_sources()` defaults
    /// to empty.
    #[test]
    fn completion_sources_defaults_to_empty() {
        struct BareMode;
        impl Mode for BareMode {
            type Guard = ();
            fn id(&self) -> ModeId {
                ModeId::new("bare-mode")
            }
            fn kind(&self) -> ModeKind {
                ModeKind::Minor
            }
            fn on_activate(&self, _ctx: ModeContext) -> LifecycleFuture<'_, ()> {
                Box::pin(async { Ok(()) })
            }
        }
        assert!(<BareMode as Mode>::completion_sources(&BareMode).is_empty());
    }

    /// A mode that DOES contribute a source returns it through
    /// the new trait method.
    #[test]
    fn mode_can_contribute_a_completion_source() {
        use lattice_completion::{
            CompletionSourceContribution, CompletionSourceKind, RawCandidate, SyncCompletionSource,
            candidate::CandidateKind,
        };
        use std::sync::Arc;

        #[derive(Debug)]
        struct StubSource;
        impl SyncCompletionSource for StubSource {
            fn produce(&self, _ctx: &lattice_completion::InsertContext<'_>) -> Vec<RawCandidate> {
                vec![RawCandidate::plain("stub", CandidateKind::Plain)]
            }
        }
        struct StubMode;
        impl Mode for StubMode {
            type Guard = ();
            fn id(&self) -> ModeId {
                ModeId::new("stub-mode")
            }
            fn kind(&self) -> ModeKind {
                ModeKind::Minor
            }
            fn completion_sources(&self) -> Vec<CompletionSourceContribution> {
                vec![CompletionSourceContribution {
                    accepts_non_word_query: false,
                    id: lattice_completion::SourceId::new("gen:stub"),
                    default_priority: 100,
                    auto_trigger: true,
                    trigger_chars: Vec::new(),
                    popup_filter_chord: None,
                    kind: CompletionSourceKind::Sync(Arc::new(StubSource)),
                }]
            }
            fn on_activate(&self, _ctx: ModeContext) -> LifecycleFuture<'_, ()> {
                Box::pin(async { Ok(()) })
            }
        }
        let sources = <StubMode as Mode>::completion_sources(&StubMode);
        assert_eq!(sources.len(), 1);
        assert_eq!(sources[0].id.as_str(), "gen:stub");
        assert_eq!(sources[0].kind.kind_label(), "sync");
    }
}
