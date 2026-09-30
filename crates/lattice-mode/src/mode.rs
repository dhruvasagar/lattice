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
    /// Content-type identity (`rust-mode`, `help-mode`). Exactly one per
    /// buffer; activating another replaces it. Chosen by the host's major
    /// resolver ([`Mode::target_buffer_kind`], [`Mode::target_language`],
    /// [`Mode::presents_extensions`]), never by an [`ActivationPolicy`].
    Major,
    /// Additive behaviour layered over the major (`line-numbers-mode`,
    /// `table-mode`). Any number per buffer, kept in activation order;
    /// auto-activated per [`Mode::activation_policy`] or pulled in by
    /// another mode's [`Mode::implies`].
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
///
/// Only *enabled* minors are auto-activated
/// ([`ModeRegistry::is_minor_enabled`](crate::ModeRegistry::is_minor_enabled)):
/// native modes are enabled at registration, plugin modes are not.
///
/// # Examples
///
/// ```
/// use lattice_core::BufferKind;
/// use lattice_mode::{ActivationPolicy, ModeId};
///
/// // A content minor: every real document, never a synthetic buffer.
/// assert!(ActivationPolicy::Global.admits("rust-mode", BufferKind::Document));
/// assert!(!ActivationPolicy::Global.admits("help-mode", BufferKind::Help));
///
/// // A universal leader: everywhere the user can focus.
/// assert!(ActivationPolicy::Universal.admits("help-mode", BufferKind::Help));
///
/// // An allowlist is matched on the major's id, independent of kind.
/// let tables = ActivationPolicy::Majors(vec![
///     ModeId::new("markdown-mode"),
///     ModeId::new("org-mode"),
/// ]);
/// assert!(tables.admits("org-mode", BufferKind::Document));
/// assert!(!tables.admits("rust-mode", BufferKind::Document));
///
/// // The trait default auto-activates nowhere.
/// assert!(!ActivationPolicy::default().admits("rust-mode", BufferKind::Document));
/// ```
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

/// An editable region at the **tail** of an otherwise read-only,
/// owner-written buffer — the comint pattern (AU‑3) (the agent-conversation prompt,
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
///
/// # Examples
///
/// ```
/// use lattice_mode::EditableTail;
///
/// // A one-line `> ` prompt at the bottom of a 5-line transcript.
/// let prompt = EditableTail { trailing_lines: 1, first_line_min_byte: 2, first_editable_line: None };
/// assert!(prompt.permits(4, 2, 5)); // after the marker, on the prompt line
/// assert!(!prompt.permits(4, 0, 5)); // inside the `> ` marker
/// assert!(!prompt.permits(3, 0, 5)); // history above the prompt
/// assert!(prompt.permits(9, 2, 10)); // still the last line after the owner appends
///
/// // The default tail permits nothing.
/// assert!(!EditableTail::default().permits(0, 0, 1));
/// ```
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
///
/// ## Lifecycle, in order
///
/// 1. **Registration** — [`ModeRegistry::register`](crate::ModeRegistry::register)
///    (native, enabled) or `register_available` (plugin, disabled until
///    the user enables it). The registry reads [`id`](Self::id),
///    [`kind`](Self::kind), [`target_buffer_kind`](Self::target_buffer_kind),
///    [`target_language`](Self::target_language) and
///    [`presents_extensions`](Self::presents_extensions) once, to build its
///    indexes. The host separately walks every registered mode once at boot
///    to translate [`keymap`](Self::keymap) into a
///    `KeymapLayer::MinorMode(id)` / `MajorMode(id)` layer and to register
///    [`action_handlers`](Self::action_handlers).
/// 2. **Activation** — per buffer, on the editor actor.
///    `activate_major` / `activate_minor` validates (registered, right
///    kind, [`required_capabilities`](Self::required_capabilities),
///    [`conflicts_with`](Self::conflicts_with), [`implies`](Self::implies)
///    registered), records the mode *and its implied cascade* in
///    [`ActiveModes`](crate::ActiveModes) synchronously, then runs each
///    step's [`on_activate`](Self::on_activate) in DFS order. The cascade
///    future is polled once inline: a hook that never awaits completes
///    before the activate call returns; the first `Pending` moves the rest
///    onto the runtime. Success publishes `MajorEntered` /
///    `MinorActivated`; failure publishes a
///    [`ModeEvent::ModeActivationFailed`](crate::ModeEvent::ModeActivationFailed)
///    and the host rolls the cascade back.
/// 3. **While active** — the host reads the declarative methods
///    ([`options`](Self::options), [`completion_sources`](Self::completion_sources),
///    [`gutter_decorations`](Self::gutter_decorations) per frame,
///    [`editable_tail`](Self::editable_tail) per edit, …). The keymap layer
///    is scoped to buffers where the mode is active by a per-keystroke
///    filter.
/// 4. **Deactivation** — synchronous: the lifecycle event publishes, then
///    the stashed Guard is dropped. Implied minors cascade-deactivate.
///
/// ## What an implementor must not do
///
/// - Keep per-buffer state on `self`. One instance serves every buffer;
///   per-activation state goes in the Guard.
/// - Block in `on_activate`: it may run inline on the editor actor. Do I/O
///   with `.await` or hand it to a spawned task.
/// - Make the declarative methods impure. Most are read once; returning a
///   different answer later is not observed consistently.
/// - Bind feature chords at `KeymapLayer::Builtin` or put handler bodies in
///   the host. The mode owns its chords ([`keymap`](Self::keymap)) *and*
///   the bodies ([`action_handlers`](Self::action_handlers) or handlers
///   registered in `on_activate`).
///
/// # Examples
///
/// A minimal minor mode with an owned Guard, driven through registration,
/// activation and deactivation exactly as the host drives it:
///
/// ```
/// use std::sync::Arc;
/// use std::sync::atomic::{AtomicUsize, Ordering};
///
/// use lattice_config::ConfigRegistry;
/// use lattice_mode::{
///     ActivationPolicy, ActiveModes, GuardStoreHandle, LifecycleFuture, Mode, ModeContext,
///     ModeId, ModeKind, ModeRegistry, ServiceRegistry,
/// };
/// use lattice_protocol::BufferId;
/// use lattice_runtime::EventBus;
///
/// /// Counts live activations; a real Guard would hold a `Subscription`,
/// /// a restored option value, a supervisor handle, …
/// struct CountGuard(Arc<AtomicUsize>);
/// impl Drop for CountGuard {
///     fn drop(&mut self) {
///         self.0.fetch_sub(1, Ordering::SeqCst);
///     }
/// }
///
/// struct TrailingSpaceMode {
///     live: Arc<AtomicUsize>,
/// }
///
/// impl Mode for TrailingSpaceMode {
///     type Guard = CountGuard;
///     fn id(&self) -> ModeId {
///         ModeId::new("trailing-space-mode") // must end in `-mode`
///     }
///     fn kind(&self) -> ModeKind {
///         ModeKind::Minor
///     }
///     fn activation_policy(&self) -> ActivationPolicy {
///         ActivationPolicy::Global // every document buffer
///     }
///     fn on_activate(&self, ctx: ModeContext) -> LifecycleFuture<'_, CountGuard> {
///         let live = self.live.clone();
///         Box::pin(async move {
///             let _ = ctx.buffer_id(); // per-buffer state goes in the Guard
///             live.fetch_add(1, Ordering::SeqCst);
///             Ok(CountGuard(live))
///         })
///     }
/// }
///
/// let live = Arc::new(AtomicUsize::new(0));
/// let mut registry = ModeRegistry::new();
/// let id = registry.register(TrailingSpaceMode { live: live.clone() }).unwrap();
///
/// // What the host holds per editor, and per buffer.
/// let (guards, events) = (GuardStoreHandle::new(), Arc::new(EventBus::new()));
/// let (config, services) = (Arc::new(ConfigRegistry::new()), Arc::new(ServiceRegistry::new()));
/// let buffer = BufferId::new(1);
/// let mut active = ActiveModes::new();
///
/// registry
///     .activate_minor(&mut active, &guards, &config, &events, &services, buffer, id, Default::default())
///     .unwrap();
/// // The hook never awaited, so it completed inline and its Guard is stashed.
/// assert!(active.has_minor(id));
/// assert!(guards.contains(buffer, id));
/// assert_eq!(live.load(Ordering::SeqCst), 1);
///
/// // Deactivation drops the Guard: cleanup is the Guard's `Drop`.
/// registry.deactivate_minor(&mut active, &guards, &events, buffer, id).unwrap();
/// assert!(!active.has_minor(id));
/// assert_eq!(live.load(Ordering::SeqCst), 0);
/// ```
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
    ///
    /// Must end in `-mode`: [`ModeRegistry::register`](crate::ModeRegistry::register)
    /// refuses anything else with
    /// [`RegistrationError::MissingModeSuffix`](crate::RegistrationError::MissingModeSuffix).
    /// It is also the name users and plugins refer to the mode by
    /// (`:describe-mode <id>`, a plugin's `enable-mode(<id>)`, the
    /// `<id>.activation` config key) and the key of the mode's keymap layer, so
    /// changing it is a breaking change. Convention: expose it as an
    /// associated `fn mode_id() -> ModeId` so other code can name the mode
    /// without an instance.
    fn id(&self) -> ModeId;

    /// Major or minor. Read at registration and on every activation
    /// call (`activate_major` on a minor, or vice versa, is
    /// [`ModeActivationError::WrongKind`]).
    fn kind(&self) -> ModeKind;

    /// For major modes, the [`BufferKind`] this mode is the default
    /// major for (H.2, 2026-05-31). `ModeRegistry::register`
    /// indexes this so
    /// [`ModeRegistry::find_major_for_kind`](crate::ModeRegistry::find_major_for_kind) can
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

    /// For major modes, the **language** this mode is the
    /// default major for, by canonical name (`Lang::name()` —
    /// `"rust"`, `"org"`) (OM.1). The peer of
    /// [`target_buffer_kind`](Self::target_buffer_kind) for the
    /// dispatch path [`BufferKind::Document`] takes: language
    /// detection rather than kind dispatch.
    ///
    /// `ModeRegistry::register` indexes this so
    /// [`ModeRegistry::find_major_for_lang`](crate::ModeRegistry::find_major_for_lang) can resolve a
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
    /// (same return value every call); the host merges these
    /// into the buffer's option-resolution layer stack while the mode
    /// is active, at the mode's layer (later-activated minors win ties),
    /// and drops them on deactivation. Build with
    /// `lattice_config::overrides! { lattice_config::Wrap = true, … }`.
    fn options(&self) -> OptionOverrideSet {
        OptionOverrideSet::default()
    }

    /// Keymap chord → command additions / overrides.
    ///
    /// Read once when the mode is registered; the host translates every
    /// binding and entry into the layer `KeymapLayer::MinorMode(id)` (or
    /// `MajorMode(id)`), and a per-keystroke filter makes that layer live
    /// only in buffers where this mode is active. A mode cannot place a
    /// binding in any other layer. Table-form entries name commands by
    /// string and are resolved against the command registry at that point
    /// (an unknown name is logged and skipped).
    ///
    /// # Examples
    ///
    /// ```
    /// use std::sync::LazyLock;
    /// use lattice_mode::{
    ///     keymap_entry, Keymap, KeymapEntry, LifecycleFuture, Mode, ModeContext, ModeId,
    ///     ModeKind,
    /// };
    ///
    /// static PREVIEW_KEYS: LazyLock<Vec<KeymapEntry>> = LazyLock::new(|| {
    ///     vec![keymap_entry! { mode: Normal, chord: "q", doc: "Close the preview", cmd: "preview:close" }]
    /// });
    ///
    /// struct PreviewMode;
    /// impl Mode for PreviewMode {
    ///     type Guard = ();
    ///     fn id(&self) -> ModeId { ModeId::new("preview-mode") }
    ///     fn kind(&self) -> ModeKind { ModeKind::Minor }
    ///     fn keymap(&self) -> Keymap { Keymap::from_entries(PREVIEW_KEYS.as_slice()) }
    ///     fn on_activate(&self, _ctx: ModeContext) -> LifecycleFuture<'_, ()> {
    ///         Box::pin(async { Ok(()) })
    ///     }
    /// }
    ///
    /// let km = PreviewMode.keymap();
    /// assert_eq!(km.entries.len(), 1);
    /// assert_eq!(km.entries[0].doc, "Close the preview");
    /// ```
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

    /// *Global* (buffer-agnostic) action handlers this
    /// mode contributes (SN.3c.0). The host walks every registered mode's
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

    /// Modes this one cannot be active alongside.
    ///
    /// Checked symmetrically when a **minor** is activated (directly or
    /// through an `implies` cascade): activation fails with
    /// [`ModeActivationError::Conflict`] if any mode listed here is active,
    /// or if the active major or any active minor lists this mode. Nothing
    /// is auto-deactivated — the caller decides whether to deactivate the
    /// other mode and retry. `activate_major` does not consult this list.
    fn conflicts_with(&self) -> &[ModeId] {
        &[]
    }

    /// Minor modes activating this mode also activates.
    /// Used by `relative-line-numbers-mode` ⇒ `line-numbers-mode`, and by
    /// read-only majors ⇒ `read-only-mode`.
    ///
    /// Every id must be registered, or activation fails with
    /// [`ModeActivationError::UnregisteredDependency`]. The whole tree is
    /// validated and recorded before any hook runs; hooks then run parent
    /// first, depth-first. Deactivating a minor cascade-deactivates the
    /// minors it implied (deactivating a major does not).
    fn implies(&self) -> &[ModeId] {
        &[]
    }

    /// File extensions (lowercase, no dot) this major PRESENTS rather than
    /// edits — the file is never loaded as text.
    ///
    /// The open path consults this before reading: a match builds a
    /// placeholder buffer with the real path and no content, and this major
    /// renders the file some other way. `image-mode` shows it as a media
    /// block; a future PDF or archive viewer is the same shape.
    ///
    /// Default empty, which is every ordinary major: its buffer IS the file's
    /// text.
    ///
    /// A mode declaring this **must** also be read-only in both of the ways
    /// that matter — `ReadOnly = true` in [`options`](Self::options) AND
    /// `read-only-mode` in [`implies`](Self::implies) — because the buffer's
    /// text is a placeholder and saving it would overwrite the real file with
    /// nothing. The option alone gates typing; the implied mode is what
    /// refuses the operators.
    fn presents_extensions(&self) -> &[&'static str] {
        &[]
    }

    /// Declarative mirror hint for "this mode is the on/off
    /// switch for a typed option of the same observable state".
    /// `Some(canonical_name)` ⇒ a host-driven cascade keeps the
    /// mode's active state and the option's value in sync.
    fn mirrors_option(&self) -> Option<&'static str> {
        None
    }

    /// Invocation-runner discovery (2026-05-26). Modes that own
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

    /// Which of *this mode's own actions* refreshes
    /// its view, or `None` (the default) when the mode backs nothing
    /// refreshable (RV.1, 2026-08-10).
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

    /// Which of this mode's actions `<Tab>` fires, and the declaration that
    /// pulls in
    /// [`foldable-view-mode`](crate::foldable_view_mode::FoldableViewMode) —
    /// the shared minor owning the chord.
    ///
    /// `Some(FOLD_TOGGLE_DEFAULT_ACTION)` for the ordinary "cycle the fold at
    /// the cursor"; `Some(own_action)` for a view with a real specialisation
    /// (magit expands a diff on the first press over a status file line).
    ///
    /// `None` — the default — means this mode's buffers keep `<Tab>` as
    /// jump-list-forward, which is what every ordinary document wants.
    fn fold_toggle_action(&self) -> Option<&'static str> {
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

    /// A *minor* mode's default auto-activation policy
    /// (MA.1; mode-architecture.md §7.4). The host's minor-activation
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

    /// The mode's editable tail on an otherwise read-only buffer, or
    /// `None` (the default) for a fully read-only / fully writable buffer
    /// (AU‑3).
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
    /// ```
    /// # use lattice_mode::{LifecycleFuture, Mode, ModeContext, ModeId, ModeKind};
    /// # struct MarkerMode;
    /// # impl Mode for MarkerMode {
    /// #     fn id(&self) -> ModeId { ModeId::new("marker-mode") }
    /// #     fn kind(&self) -> ModeKind { ModeKind::Minor }
    /// type Guard = ();
    /// fn on_activate(&self, _ctx: ModeContext) -> LifecycleFuture<'_, ()> {
    ///     Box::pin(async { Ok(()) })
    /// }
    /// # }
    /// ```
    ///
    /// **Where it runs.** On the editor actor, polled once inline; if it
    /// returns `Pending` the remainder continues as a runtime task. Awaiting
    /// real I/O is therefore fine; blocking is not. Within one cascade,
    /// steps run strictly in order, so an implied child's hook never
    /// observes its parent's half-built state.
    ///
    /// **Late results.** If the mode is deactivated (or re-activated)
    /// while this future is still pending, the Guard it eventually returns
    /// is dropped immediately instead of stashed — so the Guard's `Drop`
    /// must be correct even for an activation nobody observed.
    ///
    /// **What `ctx` gives you:** the buffer id, typed services
    /// ([`ModeContext::service`]), the config registry and the event bus —
    /// not the buffer's text. A mode that needs to create or fill a
    /// synthetic buffer reaches the host through a service
    /// ([`BufferStoreHandle`](crate::BufferStoreHandle),
    /// [`ModeActivator`](crate::ModeActivator)); asynchronous results must
    /// reach the screen through an inbound channel
    /// ([`inbound`](crate::inbound)), which wakes the editor, not a bare
    /// tick callback.
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
    /// Forwards to [`Mode::id`].
    fn id(&self) -> ModeId;
    /// Forwards to [`Mode::kind`].
    fn kind(&self) -> ModeKind;
    /// Forwards to [`Mode::target_buffer_kind`].
    fn target_buffer_kind(&self) -> Option<BufferKind>;
    /// Forwards to [`Mode::target_language`].
    fn target_language(&self) -> Option<&str>;
    /// Forwards to [`Mode::options`].
    fn options(&self) -> OptionOverrideSet;
    /// Forwards to [`Mode::keymap`].
    fn keymap(&self) -> Keymap;
    /// Forwards to [`Mode::decorations`].
    fn decorations(&self) -> Vec<DecorationProvider>;
    /// Forwards to [`Mode::gutter_decorations`].
    fn gutter_decorations(&self, ctx: &DecorationCtx<'_>) -> Vec<GutterDecoration>;
    /// Forwards to [`Mode::completion_sources`].
    fn completion_sources(&self) -> Vec<lattice_completion::CompletionSourceContribution>;
    /// Forwards to [`Mode::action_handlers`].
    fn action_handlers(&self) -> Vec<ActionHandlerContribution>;
    /// Forwards to [`Mode::required_capabilities`].
    fn required_capabilities(&self) -> CapabilitySet;
    /// Forwards to [`Mode::conflicts_with`].
    fn conflicts_with(&self) -> &[ModeId];
    /// Forwards to [`Mode::implies`].
    fn implies(&self) -> &[ModeId];
    /// Forwards to [`Mode::presents_extensions`].
    fn presents_extensions(&self) -> &[&'static str];
    /// Forwards to [`Mode::mirrors_option`].
    fn mirrors_option(&self) -> Option<&'static str>;
    /// Forwards to [`Mode::invocation_runner`].
    fn invocation_runner(&self) -> Option<ModeId>;
    /// Forwards to [`Mode::refresh_action`].
    fn refresh_action(&self) -> Option<&'static str>;
    /// Forwards to [`Mode::fold_toggle_action`].
    fn fold_toggle_action(&self) -> Option<&'static str>;
    /// Forwards to [`Mode::refresh_on_open`].
    fn refresh_on_open(&self) -> bool;
    /// Forwards to [`Mode::activation_policy`].
    fn activation_policy(&self) -> ActivationPolicy;
    /// Forwards to [`Mode::editable_tail`].
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
    fn presents_extensions(&self) -> &[&'static str] {
        <M as Mode>::presents_extensions(self)
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
    fn fold_toggle_action(&self) -> Option<&'static str> {
        <M as Mode>::fold_toggle_action(self)
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
