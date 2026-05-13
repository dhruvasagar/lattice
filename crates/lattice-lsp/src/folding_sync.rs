//! 4.4.f: the option-side coupling of `lsp-folding-mode`.
//!
//! Activating the mode swaps the buffer's `foldmethod` option
//! to `FoldMethod::Lsp`; deactivating restores the value the
//! buffer had immediately before activation. This is the LSP-
//! folding-specific *behaviour* -- the mode owns it.
//!
//! The mode's hand-written [`crate::modes::LspFoldingMode`]
//! `Mode::on_activate` / `Mode::on_deactivate` impls call the
//! two helpers below. The activation pipeline is identical
//! regardless of *how* the mode gets activated -- direct
//! toggle, `lsp-mode` cascade, programmatic activation from a
//! plugin, future API path -- because everything funnels
//! through the mode trait's lifecycle hooks.
//!
//! The prior `foldmethod` is stashed in [`BufferLocals`] via
//! the typed local [`PriorFoldmethod`]; the mode owns this
//! local (`OWNER_MODE = "lsp-folding-mode"`) per
//! `mode-architecture.md` §9.4.

use lattice_config::{ConfigRegistry, FoldMethodOption};
use lattice_core::FoldMethod;
use lattice_mode::BufferLocal;

/// Buffer-local stash holding the `foldmethod` value that
/// was active immediately before `lsp-folding-mode` activated
/// the buffer. The mode's `on_deactivate` reads this back to
/// restore the user's prior setting; if the local is missing
/// (mode was never activated on this buffer, or was already
/// `Lsp` at activate time so the stash wasn't written),
/// restore is a no-op.
#[derive(Debug, Clone, Copy)]
pub struct PriorFoldmethod(pub FoldMethod);

impl BufferLocal for PriorFoldmethod {
    const NAME: &'static str = "lsp-folding-mode.prior_foldmethod";
    const DOC: &'static str = "foldmethod value at the moment \
                               `lsp-folding-mode` activated. Restored \
                               on deactivate.";
    const OWNER_MODE: &'static str = "lsp-folding-mode";

    fn describe(&self) -> String {
        self.0.label().to_string()
    }
}

/// Swap `foldmethod` to `FoldMethod::Lsp`. Returns the value
/// that was active immediately before the swap so the caller
/// can stash it for later restoration. Returns `None` when no
/// swap was needed (the option was already `Lsp`); the caller
/// should then leave any prior stash alone (idempotent).
pub fn on_activate(config: &ConfigRegistry) -> Option<FoldMethod> {
    let prior = config
        .get_typed::<FoldMethodOption>()
        .map(|v| *v)
        .unwrap_or_default();
    if prior == FoldMethod::Lsp {
        return None;
    }
    let _ = config.set_typed::<FoldMethodOption>(FoldMethod::Lsp);
    Some(prior)
}

/// Restore `foldmethod` to `prior`. No-op when the option is
/// already at the target value (so the caller can call this
/// unconditionally on deactivate without firing a redundant
/// `OptionChanged` event).
pub fn on_deactivate(config: &ConfigRegistry, prior: FoldMethod) {
    let current = config
        .get_typed::<FoldMethodOption>()
        .map(|v| *v)
        .unwrap_or_default();
    if current == prior {
        return;
    }
    let _ = config.set_typed::<FoldMethodOption>(prior);
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn on_activate_swaps_to_lsp_and_returns_prior() {
        let reg = ConfigRegistry::new();
        reg.init_from_linkme();
        // Default `foldmethod` is `Manual`.
        let prior = on_activate(&reg).expect("swap occurred");
        assert_eq!(prior, FoldMethod::Manual);
        assert_eq!(
            *reg.get_typed::<FoldMethodOption>().unwrap(),
            FoldMethod::Lsp,
        );
    }

    #[test]
    fn on_activate_is_idempotent_when_already_lsp() {
        let reg = ConfigRegistry::new();
        reg.init_from_linkme();
        reg.set_typed::<FoldMethodOption>(FoldMethod::Lsp).unwrap();
        assert_eq!(on_activate(&reg), None);
        assert_eq!(
            *reg.get_typed::<FoldMethodOption>().unwrap(),
            FoldMethod::Lsp,
        );
    }

    #[test]
    fn on_deactivate_restores_prior() {
        let reg = ConfigRegistry::new();
        reg.init_from_linkme();
        reg.set_typed::<FoldMethodOption>(FoldMethod::Syntax)
            .unwrap();
        on_activate(&reg);
        on_deactivate(&reg, FoldMethod::Syntax);
        assert_eq!(
            *reg.get_typed::<FoldMethodOption>().unwrap(),
            FoldMethod::Syntax,
        );
    }
}
