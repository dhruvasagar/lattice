//! PH7.11a modes fixture guest.
//!
//! A minimal `wasm32-wasip2` component implementing the `modes-plugin` world:
//!   - `register-modes` (the world export the host calls once) declares minor
//!     modes via the imported `modes.register-mode`: two well-formed (registered)
//!     and one with a mis-suffixed id (rejected by the registry's `-mode` gate).
//!
//! The host test inspects the `ModeRegistry` afterwards. Declaring via the raw
//! WIT calls (no SDK) exercises the CANONICAL, language-agnostic surface.

wit_bindgen::generate!({
    world: "modes-plugin",
    path: "../../../../../wit",
});

use lattice::plugin_host::modes;
use lattice::plugin_host::modes::{
    ActivationPolicy, BindingMode, ModeCapabilities, ModeDeclaration, ModeKeymapBinding, ModeKind,
};

struct Component;

impl Guest for Component {
    /// The host calls this once; the guest declares its modes through the
    /// imported `modes.register-mode` host function.
    fn register_modes() {
        // A manual minor mode requiring buffer-uri, contributing one keymap
        // binding: Normal-mode `<C-s>` → the built-in `ex:write` (PH7.11b). The
        // binding lands in this mode's OWN `MinorMode(git-blame-mode)` layer.
        modes::register_mode(&ModeDeclaration {
            id: "git-blame-mode".to_string(),
            kind: ModeKind::Minor,
            activation_policy: ActivationPolicy::Manual,
            capabilities: ModeCapabilities::BUFFER_URI,
            keymap: vec![ModeKeymapBinding {
                binding_mode: BindingMode::Normal,
                chord: "<C-s>".to_string(),
                command: "ex:write".to_string(),
            }],
        });
        // A universal minor mode requiring LSP + diagnostics, no keymap.
        modes::register_mode(&ModeDeclaration {
            id: "lsp-lens-mode".to_string(),
            kind: ModeKind::Minor,
            activation_policy: ActivationPolicy::Universal,
            capabilities: ModeCapabilities::LSP | ModeCapabilities::DIAGNOSTICS,
            keymap: vec![],
        });
        // A mis-suffixed id — the registry's `-mode` gate rejects it, so it never
        // lands in the registry (the host reports only the two accepted ids).
        modes::register_mode(&ModeDeclaration {
            id: "not-suffixed".to_string(),
            kind: ModeKind::Minor,
            activation_policy: ActivationPolicy::Manual,
            capabilities: ModeCapabilities::empty(),
            keymap: vec![],
        });
    }
}

export!(Component);
