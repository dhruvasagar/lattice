//! CM.2: wiring a plugin-registered operator's chord into the grammar.
//!
//! An operator is only half a contribution. `register-operator` puts the spec
//! and its `apply` in the command registry; what makes it *reachable* is the
//! operator-pending composition the host builds around it — motion targets,
//! the doubled linewise form, `i_` / `a_` text-object pendings, and the
//! `f` / `F` / `t` / `T` find-char pendings.
//!
//! That composition needs host-resolved builtin ids, so it lives in
//! `lattice-host` (`keymap_normal::register_operator_bindings`, `pub` since
//! N.1.3 for precisely this split — narrow's `zn` is a native provider doing
//! the same thing). The plugin loader cannot call it: `lattice-plugin-loader`
//! does not depend on `lattice-host`, and should not.
//!
//! So the host publishes this trait as a service and the loader calls it, the
//! same shape every other cross-crate seam here uses. **An absent handle is a
//! `NotWired` load failure, not a silent skip** — a plugin whose operator
//! registered correctly and has no keys is indistinguishable from one that
//! never loaded, which is the failure mode this crate keeps growing rules
//! about.

use std::sync::Arc;

/// Wires a plugin operator's chord into the universal operator-pending layer.
pub trait OperatorChordWirer: Send + Sync {
    /// Bind `chord` to `op`, with the full operator-pending surface.
    ///
    /// `doubled` is the TRAILING key of the linewise form (`c` for `gcc`),
    /// not the whole chord; `None` binds no doubled form.
    ///
    /// `mode` scopes the bindings to that minor mode's keymap layer rather
    /// than `Builtin`. A plugin's chord must not outlive the plugin: bound at
    /// `Builtin`, `gc` would survive `:set comment.enabled=false` pointing at
    /// a handler that is gone.
    fn wire(
        &self,
        op: lattice_grammar::registry::OperatorId,
        chord: &str,
        doubled: Option<char>,
        mode: crate::ModeId,
        post_motion_char: bool,
    ) -> Result<(), String>;
}

/// The shared handle. Registered by the host under THIS alias, and looked up
/// under it — see the `ServiceRegistry` `TypeId` rule.
pub type OperatorChordWirerHandle = Arc<dyn OperatorChordWirer>;
