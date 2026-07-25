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
}
