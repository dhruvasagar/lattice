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
#[derive(Default)]
pub struct TransientSourceRegistry {
    sources: std::sync::Mutex<HashMap<String, Arc<dyn Fn() -> TransientSpec + Send + Sync>>>,
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
        builder: impl Fn() -> TransientSpec + Send + Sync + 'static,
    ) {
        if let Ok(mut sources) = self.sources.lock() {
            sources.insert(name.into(), Arc::new(builder));
        }
    }

    /// Build the named transient's spec, or `None` if no builder is
    /// registered under `name`.
    pub fn build(&self, name: &str) -> Option<TransientSpec> {
        let builder = self.sources.lock().ok()?.get(name)?.clone();
        Some(builder())
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
        assert!(registry.build("nope").is_none());
    }

    #[test]
    fn register_then_build_round_trips() {
        let registry = TransientSourceRegistry::new();
        registry.register("magit-dispatch", || spec_titled("Magit dispatch"));
        let spec = registry
            .build("magit-dispatch")
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
        registry.register("counted", move || {
            calls2.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            spec_titled("counted")
        });
        registry.build("counted");
        registry.build("counted");
        registry.build("counted");
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 3);
    }

    #[test]
    fn re_registering_a_name_overwrites_the_previous_builder() {
        let registry = TransientSourceRegistry::new();
        registry.register("magit-dispatch", || spec_titled("first"));
        registry.register("magit-dispatch", || spec_titled("second"));
        let spec = registry.build("magit-dispatch").expect("still registered");
        assert_eq!(
            spec.title, "second",
            "last writer wins, matching PickerRegistry"
        );
    }

    #[test]
    fn distinct_names_stay_independent() {
        let registry = TransientSourceRegistry::new();
        registry.register("magit-dispatch", || spec_titled("dispatch"));
        registry.register("magit-file-dispatch", || spec_titled("file-dispatch"));
        assert_eq!(registry.build("magit-dispatch").unwrap().title, "dispatch");
        assert_eq!(
            registry.build("magit-file-dispatch").unwrap().title,
            "file-dispatch"
        );
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
