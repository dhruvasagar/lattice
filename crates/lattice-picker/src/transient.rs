//! Transient mode — grouped action menus within the picker.
//!
//! A transient presents groups of action items with single-key
//! selection, toggleable flags, argument inputs, and a live command
//! preview. Magit is the first consumer (dispatch menus, branch menus,
//! stash menus), but which-key key hints, command palette drilldown,
//! and future plugin transients all reuse the same mechanism.
//!
//! See `docs/dev/architecture/magit.md` §8.

use std::collections::HashMap;
use std::sync::Arc;

use lattice_protocol::ids::CommandId;

/// Accumulated state for an open transient: flag values and argument
/// values. Keyed by the `name` field from `TransientItemKind::Flag`
/// and `TransientItemKind::Argument` items.
pub type TransientState = HashMap<String, TransientValue>;

/// A single value in the transient state (flag or argument).
#[derive(Clone, Debug)]
pub enum TransientValue {
    Bool(bool),
    String(String),
}

/// Build initial state from a spec — flags get their defaults,
/// arguments get their defaults (or empty string).
pub fn transient_initial_state(spec: &TransientSpec) -> TransientState {
    let mut state = HashMap::new();
    for group in &spec.groups {
        for item in &group.items {
            match &item.kind {
                TransientItemKind::Flag { name, default } => {
                    state.insert(name.clone(), TransientValue::Bool(*default));
                }
                TransientItemKind::Argument { name, default, .. } => {
                    state.insert(
                        name.clone(),
                        TransientValue::String(default.clone().unwrap_or_default()),
                    );
                }
                _ => {}
            }
        }
    }
    state
}

/// Specification for a transient menu — the complete layout.
/// Stored behind an `Arc` in the `Picker` struct so clone is a ref-count
/// bump; the `preview` boxed closure stays alive across clones.
pub struct TransientSpec {
    pub title: String,
    pub groups: Vec<TransientGroup>,
    /// Optional live command preview. Called every time a flag
    /// toggles or an argument changes; the returned string is
    /// rendered in the picker's preview pane.
    #[allow(clippy::type_complexity)]
    pub preview: Option<Box<dyn Fn(&TransientState) -> String + Send + Sync>>,
    pub footer: Option<String>,
}

impl TransientSpec {
    /// How many items can be selected — every item in every group,
    /// counted in group order.
    ///
    /// This is the number `<C-n>` / `<C-p>` wrap on, and the reason
    /// they can no longer run off the end: it is derived from the spec
    /// alone, so the host clamps on data it owns rather than on a
    /// viewport height only the renderer knows. The previous shape
    /// stored a raw scroll offset and grew it unbounded, leaving the
    /// stored value tens of rows past anything renderable — pressing
    /// `<C-p>` then did nothing visible until the overshoot was walked
    /// back off.
    pub fn selectable_count(&self) -> usize {
        self.groups.iter().map(|g| g.items.len()).sum()
    }

    /// Rows the group+item list occupies — per group one header, one
    /// row per item and one trailing blank separator. EXCLUDES
    /// preview/footer, which only exist in the popup layout.
    ///
    /// Lives here rather than in each renderer because both peers need
    /// it and both had their own copy: undercounting the separator in
    /// both made every multi-group menu's box too short *and* capped
    /// its scroll before the last rows.
    pub fn row_count(&self) -> usize {
        self.selectable_count() + self.groups.len() * 2
    }

    /// Which row the `index`-th selectable item is painted on, in the
    /// same row stream [`Self::row_count`] measures. `None` when
    /// `index` is past the last item.
    ///
    /// The mapping is what lets a renderer window on a *selection*: the
    /// host moves an item index, and each peer turns it into the scroll
    /// offset its own geometry needs.
    pub fn row_of_item(&self, index: usize) -> Option<usize> {
        let mut row = 0;
        let mut seen = 0;
        for group in &self.groups {
            row += 1; // the group header
            if index < seen + group.items.len() {
                return Some(row + (index - seen));
            }
            row += group.items.len() + 1; // items + the separator
            seen += group.items.len();
        }
        None
    }

    /// The `index`-th selectable item, in the same order
    /// [`Self::selectable_count`] counts.
    pub fn item_at(&self, index: usize) -> Option<&TransientItem> {
        self.groups.iter().flat_map(|g| &g.items).nth(index)
    }

    /// The scroll offset that keeps `selected`'s row inside a window
    /// `visible` rows tall, clamped so the list never scrolls past its
    /// own end.
    ///
    /// Derived fresh from the selection every frame rather than stored,
    /// which is what makes the overshoot unrepresentable: there is no
    /// scroll state left to drift out of range.
    pub fn scroll_for(&self, selected: usize, visible: usize) -> usize {
        let row = self.row_of_item(selected).unwrap_or(0);
        let max = self.row_count().saturating_sub(visible.max(1));
        (row + 1).saturating_sub(visible.max(1)).min(max)
    }
}

impl std::fmt::Debug for TransientSpec {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TransientSpec")
            .field("title", &self.title)
            .field("groups", &self.groups)
            .field("preview", &self.preview.as_ref().map(|_| "<fn>"))
            .field("footer", &self.footer)
            .finish()
    }
}

/// A named group of transient items.
#[derive(Clone, Debug)]
pub struct TransientGroup {
    pub label: String,
    pub items: Vec<TransientItem>,
}

/// A single entry in a transient group.
#[derive(Clone, Debug)]
pub struct TransientItem {
    pub key: Vec<String>,
    pub label: String,
    pub description: String,
    pub kind: TransientItemKind,
}

/// The kind of a transient item — determines its interaction.
#[derive(Clone, Debug)]
pub enum TransientItemKind {
    /// Fires an action via the action-handler registry and closes
    /// the transient.
    Action(CommandId),
    /// Opens a nested transient (submenu). The parent transient is
    /// pushed onto a stack; `BS`/`DEL` returns to it.
    Submenu(std::sync::Arc<TransientSpec>),
    /// A boolean flag that toggles in-place. The `name` is the key
    /// in `TransientState`.
    Flag { name: String, default: bool },
    /// An argument that opens a minibuffer prompt for a value. The
    /// `name` is the key in `TransientState`.
    Argument {
        name: String,
        default: Option<String>,
        prompt: String,
    },
    /// Dismisses the transient picker without firing any action.
    /// Used for 'n' / 'q' keys in confirmation dialogs.
    Dismiss,
}

/// Registry of named transient builders, populated at boot by each
/// owning mode crate (magit registers `magit-dispatch` /
/// `magit-file-dispatch`; per the module doc, which-key / command
/// palette / future plugin transients are meant to reuse the same
/// mechanism). `Effect::OpenTransient { source }` carries only the
/// name — resolving it to an actual `TransientSpec` (which can't
/// cross into `lattice-grammar`'s `Effect` enum directly, since
/// `TransientSpec` lives downstream of it) happens here, at the
/// renderer's effect-handling site. Mirrors `PickerRegistry`'s
/// named-source shape, simplified: transients have no candidate
/// generator or arg-schema concept, just a name and a builder.
///
/// Read-only after boot in practice (each owning crate's `install`
/// populates it once), so this stays a plain mutex-guarded map
/// rather than an `ArcSwap` RCU registry like `PickerRegistry` —
/// there's no runtime plugin-load use case for it yet.
/// MG.23h: where a transient was opened from, so a builder can vary
/// its rows.
///
/// A dispatch menu bound globally has to degrade: rows that act on the
/// thing under the cursor are meaningless in a buffer that has no such
/// thing, and a row whose only useful reading depends on which buffer
/// you are in should say the useful thing. Emacs magit answers both
/// with predicates on its prefix definitions — `:if-derived` for "any
/// buffer of this family" and `:if-mode` for "exactly this major" —
/// and this is the same question, asked of the two mode axes.
///
/// **Major and minors are separate fields on purpose.** A flat list of
/// active mode ids can only answer one of those two questions; magit's
/// dispatch asks both of them about the same key (`j` is "jump to
/// section" in magit-status and "display status" everywhere else,
/// while its whole "Applying changes" group is gated on the looser
/// family test).
///
/// **What is deliberately absent:** the buffer id, the cursor, and the
/// selection. A builder produces rows; it does not act. The row's
/// action receives its own `ActionContext`, which already carries the
/// underlying buffer, its cursor and any Visual region — resolved at
/// fire time, when they are current. Duplicating them here would be
/// speculative surface that could also go stale between build and fire.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TransientContext {
    /// The active major mode's id, if the buffer has one. The
    /// `:if-mode` question.
    pub major_mode: Option<String>,
    /// The active minor mode ids. The `:if-derived` question is asked
    /// here: a magit buffer is one where `magit-core-mode` is active,
    /// whatever its major happens to be.
    pub minor_modes: Vec<String>,
}

impl TransientContext {
    /// True iff `id` is the active major — `:if-mode`.
    pub fn is_major(&self, id: &str) -> bool {
        self.major_mode.as_deref() == Some(id)
    }

    /// True iff `id` is an active minor — the family test a mode's
    /// shared minor answers, standing in for `:if-derived`.
    pub fn has_minor(&self, id: &str) -> bool {
        self.minor_modes.iter().any(|m| m == id)
    }
}

#[derive(Default)]
pub struct TransientSourceRegistry {
    #[allow(clippy::type_complexity)]
    sources: std::sync::Mutex<
        HashMap<String, Arc<dyn Fn(&TransientContext) -> TransientSpec + Send + Sync>>,
    >,
}

/// Service handle registered via `SubsystemBoot::register_service` —
/// same `Arc<X>` register-and-lookup convention every other service
/// handle in this codebase uses.
pub type TransientSourceRegistryHandle = Arc<TransientSourceRegistry>;

impl TransientSourceRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a named builder. Re-registering a name overwrites
    /// the previous entry (last writer wins), matching
    /// `PickerRegistry::register`'s semantics.
    pub fn register(
        &self,
        name: impl Into<String>,
        builder: impl Fn(&TransientContext) -> TransientSpec + Send + Sync + 'static,
    ) {
        if let Ok(mut sources) = self.sources.lock() {
            sources.insert(name.into(), Arc::new(builder));
        }
    }

    /// Build the named transient's spec for the place it was opened
    /// from, or `None` if no builder is registered under `name`.
    ///
    /// MG.23h: `ctx` is supplied by the renderer at open time rather
    /// than by whatever emitted `Effect::OpenTransient`. That is what
    /// makes every path uniformly context-aware — the chord, the
    /// ex-command (whose `ExCommandContext` carries no buffer), and any
    /// future plugin-emitted open. Resolving it at emit time instead
    /// would leave all but the chord looking at nothing.
    pub fn build(&self, name: &str, ctx: &TransientContext) -> Option<TransientSpec> {
        let builder = self.sources.lock().ok()?.get(name)?.clone();
        Some(builder(ctx))
    }
}

/// Build a simple y/n confirmation transient spec.
/// `prompt` is the title shown at the top; `yes_command_id`
/// is the action fired when the user presses `y`.
pub fn confirm_transient_spec(prompt: &str, yes_command_id: CommandId) -> TransientSpec {
    TransientSpec {
        title: prompt.to_string(),
        groups: vec![TransientGroup {
            label: String::new(),
            items: vec![
                TransientItem {
                    key: vec!["y".to_string(), "Y".to_string()],
                    label: "Yes".to_string(),
                    description: String::new(),
                    kind: TransientItemKind::Action(yes_command_id),
                },
                TransientItem {
                    key: vec![
                        "n".to_string(),
                        "N".to_string(),
                        "q".to_string(),
                        "Q".to_string(),
                    ],
                    label: "No".to_string(),
                    description: String::new(),
                    kind: TransientItemKind::Dismiss,
                },
            ],
        }],
        preview: None,
        footer: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec_titled(title: &str) -> TransientSpec {
        TransientSpec {
            title: title.to_string(),
            groups: Vec::new(),
            preview: None,
            footer: None,
        }
    }

    #[test]
    fn build_returns_none_for_an_unregistered_name() {
        let registry = TransientSourceRegistry::new();
        assert!(
            registry
                .build("nope", &TransientContext::default())
                .is_none()
        );
    }

    #[test]
    fn register_then_build_round_trips() {
        let registry = TransientSourceRegistry::new();
        registry.register("magit-dispatch", |_| spec_titled("Magit dispatch"));
        let spec = registry
            .build("magit-dispatch", &TransientContext::default())
            .expect("registered name builds");
        assert_eq!(spec.title, "Magit dispatch");
    }

    #[test]
    fn build_calls_the_builder_fresh_every_time() {
        // The registry stores a builder closure, not a cached spec —
        // each `build()` must re-invoke it. Regression guard: an
        // Rc/Cell-backed call counter would only prove this if build()
        // is actually called per-invocation rather than once at
        // registration time.
        let registry = TransientSourceRegistry::new();
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let calls2 = calls.clone();
        registry.register("counted", move |_| {
            calls2.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            spec_titled("counted")
        });
        registry.build("counted", &TransientContext::default());
        registry.build("counted", &TransientContext::default());
        registry.build("counted", &TransientContext::default());
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 3);
    }

    #[test]
    fn re_registering_a_name_overwrites_the_previous_builder() {
        let registry = TransientSourceRegistry::new();
        registry.register("magit-dispatch", |_| spec_titled("first"));
        registry.register("magit-dispatch", |_| spec_titled("second"));
        let spec = registry
            .build("magit-dispatch", &TransientContext::default())
            .expect("still registered");
        assert_eq!(
            spec.title, "second",
            "last writer wins, matching PickerRegistry"
        );
    }

    /// MG.23h: the context reaches the builder, and reaches it on
    /// every build rather than being captured once.
    ///
    /// Without this the signature could be satisfied by a builder that
    /// ignores its argument and the whole mechanism would look wired
    /// while gating nothing.
    #[test]
    fn the_open_context_reaches_the_builder() {
        let registry = TransientSourceRegistry::new();
        registry.register("ctx", |ctx: &TransientContext| {
            spec_titled(ctx.major_mode.as_deref().unwrap_or("none"))
        });
        let in_status = TransientContext {
            major_mode: Some("magit-status-mode".into()),
            minor_modes: vec!["magit-core-mode".into()],
        };
        assert_eq!(
            registry.build("ctx", &in_status).unwrap().title,
            "magit-status-mode"
        );
        assert_eq!(
            registry
                .build("ctx", &TransientContext::default())
                .unwrap()
                .title,
            "none",
            "a second build with a different context must rebuild, not \
             replay the first"
        );
    }

    /// The two questions magit's prefixes ask are different questions,
    /// which is why the two axes are separate fields: a flat list of
    /// active mode ids could answer only one of them.
    #[test]
    fn the_major_and_minor_tests_are_independent() {
        let ctx = TransientContext {
            major_mode: Some("magit-status-mode".into()),
            minor_modes: vec!["magit-core-mode".into()],
        };
        assert!(ctx.is_major("magit-status-mode"));
        assert!(!ctx.is_major("magit-core-mode"), "a minor is not the major");
        assert!(ctx.has_minor("magit-core-mode"));
        assert!(
            !ctx.has_minor("magit-status-mode"),
            "the major is not among the minors"
        );

        // A magit buffer that is not the status buffer: the family
        // test still passes, the exact-major test does not.
        let in_log = TransientContext {
            major_mode: Some("magit-log-mode".into()),
            minor_modes: vec!["magit-core-mode".into()],
        };
        assert!(in_log.has_minor("magit-core-mode"));
        assert!(!in_log.is_major("magit-status-mode"));

        // And no magit at all.
        assert!(!TransientContext::default().has_minor("magit-core-mode"));
    }

    #[test]
    fn distinct_names_stay_independent() {
        let registry = TransientSourceRegistry::new();
        registry.register("magit-dispatch", |_| spec_titled("dispatch"));
        registry.register("magit-file-dispatch", |_| spec_titled("file-dispatch"));
        assert_eq!(
            registry
                .build("magit-dispatch", &TransientContext::default())
                .unwrap()
                .title,
            "dispatch"
        );
        assert_eq!(
            registry
                .build("magit-file-dispatch", &TransientContext::default())
                .unwrap()
                .title,
            "file-dispatch"
        );
    }

    /// A three-group menu whose groups are different sizes, so an
    /// off-by-one in the header/separator accounting cannot hide.
    fn geometry_spec() -> TransientSpec {
        fn item(key: &str) -> TransientItem {
            TransientItem {
                key: vec![key.to_string()],
                label: key.to_string(),
                description: String::new(),
                kind: TransientItemKind::Dismiss,
            }
        }
        TransientSpec {
            title: "t".into(),
            groups: vec![
                TransientGroup {
                    label: "one".into(),
                    items: vec![item("a"), item("b")],
                },
                TransientGroup {
                    label: "two".into(),
                    items: vec![item("c")],
                },
                TransientGroup {
                    label: "three".into(),
                    items: vec![item("d"), item("e"), item("f")],
                },
            ],
            preview: None,
            footer: None,
        }
    }

    #[test]
    fn row_of_item_skips_the_headers_and_separators_between_groups() {
        let spec = geometry_spec();
        assert_eq!(spec.selectable_count(), 6);
        // header a b sep | header c sep | header d e f sep
        // 0      1 2 3   | 4      5 6   | 7      8 9 10 11
        assert_eq!(spec.row_count(), 12);
        assert_eq!(spec.row_of_item(0), Some(1));
        assert_eq!(spec.row_of_item(1), Some(2));
        assert_eq!(spec.row_of_item(2), Some(5));
        assert_eq!(spec.row_of_item(3), Some(8));
        assert_eq!(spec.row_of_item(5), Some(10));
        assert_eq!(spec.row_of_item(6), None, "past the last item");
    }

    /// The property the whole redesign exists for: whatever the
    /// selection and however tall the window, the selected item's row
    /// is inside `[scroll, scroll + visible)`.
    #[test]
    fn scroll_for_always_keeps_the_selection_in_view() {
        let spec = geometry_spec();
        for visible in [1usize, 2, 4, 7, 12, 40] {
            for selected in 0..spec.selectable_count() {
                let scroll = spec.scroll_for(selected, visible);
                let row = spec.row_of_item(selected).expect("a row");
                assert!(
                    row >= scroll && row < scroll + visible.max(1),
                    "selection {selected} (row {row}) fell outside the \
                     window [{scroll}, {}) at visible={visible}",
                    scroll + visible.max(1)
                );
                assert!(
                    scroll <= spec.row_count().saturating_sub(visible.max(1)),
                    "the list must never scroll past its own end"
                );
            }
        }
    }

    /// `<C-n>` past the last item wraps instead of running away.
    ///
    /// The bug this replaces: the host grew a raw scroll offset with
    /// `saturating_add`, so extra presses accumulated far past anything
    /// renderable and `<C-p>` had to walk every phantom step back
    /// before the view moved.
    #[test]
    fn walking_off_either_end_of_a_transient_wraps() {
        use crate::{Picker, PickerAction, PickerSource};

        let mut picker = Picker::new("t", PickerSource::Files, PickerAction::OpenFile);
        picker.transient = Some(std::sync::Arc::new(geometry_spec()));

        for expected in [1, 2, 3, 4, 5, 0, 1] {
            picker.transient_select_next();
            assert_eq!(picker.transient_selected, expected);
        }
        picker.transient_selected = 0;
        picker.transient_select_prev();
        assert_eq!(
            picker.transient_selected, 5,
            "backwards off the top wraps to the last item"
        );
    }

    /// Ten extra `<C-n>`s must leave the selection where one `<C-p>`
    /// undoes them — the user-visible symptom, stated directly.
    #[test]
    fn overshooting_leaves_no_phantom_steps_to_walk_back() {
        use crate::{Picker, PickerAction, PickerSource};

        let mut picker = Picker::new("t", PickerSource::Files, PickerAction::OpenFile);
        picker.transient = Some(std::sync::Arc::new(geometry_spec()));
        for _ in 0..6 {
            picker.transient_select_next();
        }
        assert_eq!(picker.transient_selected, 0, "six items, back to the top");
        picker.transient_select_prev();
        assert_eq!(
            picker.transient_selected, 5,
            "one press back moves one item — not one of N accumulated \
             out-of-range steps"
        );
    }

    /// With no transient open the walkers are inert rather than
    /// scribbling on a field the picker is not using.
    #[test]
    fn the_transient_walkers_are_inert_without_a_transient() {
        use crate::{Picker, PickerAction, PickerSource};

        let mut picker = Picker::new("t", PickerSource::Files, PickerAction::OpenFile);
        picker.transient_select_next();
        picker.transient_select_prev();
        assert_eq!(picker.transient_selected, 0);
        assert!(picker.transient_selected_item().is_none());
    }

    /// `<CR>` fires the item the marker is on — the selection index and
    /// `item_at` must agree with `row_of_item`'s ordering, or the menu
    /// highlights one row and runs another.
    #[test]
    fn the_selected_item_is_the_one_the_index_names() {
        let spec = geometry_spec();
        for (index, key) in ["a", "b", "c", "d", "e", "f"].iter().enumerate() {
            assert_eq!(
                spec.item_at(index).map(|i| i.label.as_str()),
                Some(*key),
                "item {index}"
            );
        }
        assert!(spec.item_at(6).is_none());
    }

    #[test]
    fn confirm_transient_spec_has_yes_action_and_dismiss_items() {
        let cmd_id = CommandId::new(42);
        let spec = confirm_transient_spec("Discard changes?", cmd_id);
        assert_eq!(spec.title, "Discard changes?");
        assert_eq!(spec.groups.len(), 1);
        let items = &spec.groups[0].items;
        assert_eq!(items.len(), 2);
        assert!(matches!(items[0].kind, TransientItemKind::Action(id) if id == cmd_id));
        assert!(matches!(items[1].kind, TransientItemKind::Dismiss));
        assert!(items[1].key.iter().any(|k| k == "q"), "q must dismiss");
        assert!(items[1].key.iter().any(|k| k == "n"), "n must dismiss");
    }
}
