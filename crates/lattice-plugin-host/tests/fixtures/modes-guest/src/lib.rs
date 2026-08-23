//! PH7.11a modes fixture guest.
//!
//! A minimal `wasm32-wasip2` component implementing the `modes-plugin` world:
//!   - `register-modes` (the world export the host calls once) declares modes
//!     via the imported `modes.register-mode`: two well-formed minors
//!     (registered), one with a mis-suffixed id (rejected by the registry's
//!     `-mode` gate), and — OM.2 — a MAJOR claiming a language, plus a minor
//!     whose language claim must be dropped rather than honoured.
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
            target_language: None,
        });
        // A universal minor mode requiring LSP + diagnostics, no keymap.
        modes::register_mode(&ModeDeclaration {
            id: "lsp-lens-mode".to_string(),
            kind: ModeKind::Minor,
            activation_policy: ActivationPolicy::Universal,
            capabilities: ModeCapabilities::LSP | ModeCapabilities::DIAGNOSTICS,
            keymap: vec![],
            target_language: None,
        });
        // A mis-suffixed id — the registry's `-mode` gate rejects it, so it never
        // lands in the registry (the host reports only the accepted ids).
        modes::register_mode(&ModeDeclaration {
            id: "not-suffixed".to_string(),
            kind: ModeKind::Minor,
            activation_policy: ActivationPolicy::Manual,
            capabilities: ModeCapabilities::empty(),
            keymap: vec![],
            target_language: None,
        });
        // OM.2: a MAJOR claiming a language. This is the org shape — a plugin
        // that contributes a language contributes its major too, which is the
        // only route a plugin language has to one. Its keymap binding lands in
        // `MajorMode(fixture-lang-mode)`, gated the same way a minor's is.
        modes::register_mode(&ModeDeclaration {
            id: "fixture-lang-mode".to_string(),
            kind: ModeKind::Major,
            activation_policy: ActivationPolicy::Manual,
            capabilities: ModeCapabilities::empty(),
            keymap: vec![ModeKeymapBinding {
                binding_mode: BindingMode::Normal,
                chord: "<C-y>".to_string(),
                command: "ex:write".to_string(),
            }],
            target_language: Some("fixturelang".to_string()),
        });
        // OM.2: a MINOR claiming a language. It registers, but the claim is
        // dropped — a buffer has exactly one major, and honouring this would
        // install a minor as it.
        modes::register_mode(&ModeDeclaration {
            id: "fixture-greedy-mode".to_string(),
            kind: ModeKind::Minor,
            activation_policy: ActivationPolicy::Manual,
            capabilities: ModeCapabilities::empty(),
            keymap: vec![],
            target_language: Some("fixturelang".to_string()),
        });
    }
}

export!(Component);
