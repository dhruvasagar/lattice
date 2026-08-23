//! PL8.G modes-as-components fixture guest — the emacs-keys leader tribute
//! shipped as a WASM **component** instead of native Rust.
//!
//! The native `emacs-keys-mode` (`lattice-mode/src/emacs_keys_mode.rs`) is a
//! builtin minor mode whose entire behavior is a keymap layer: it binds the
//! `<C-x>` leader to EXISTING commands. This fixture declares the SAME shape —
//! a minor mode owning a keymap layer of leader→existing-command bindings —
//! through the canonical WIT `modes.register-mode`, proving the design's §5.8.3
//! "major and minor modes ship as components" path end-to-end (PL8.G). Native
//! modes stay native by default; this validates the extension path.
//!
//! ## Why a distinct id + distinct chords
//!
//! The host test loads this into a REAL booted editor where the native
//! `emacs-keys-mode` is already registered (a foundation mode). Two things
//! follow:
//!   - **Distinct id** (`emacs-keys-plugin-mode`, not `emacs-keys-mode`) — the
//!     `ModeRegistry` rejects a duplicate id, so a component cannot re-declare
//!     the still-native mode. A plugin shipping its own leader mode is the
//!     realistic case anyway.
//!   - **Distinct chords** (`<C-x>e` / `<C-x>w`, which the native tribute does
//!     NOT bind) — active mode layers MERGE into one composite trie at lookup,
//!     so a chord the native layer lacks resolves ONLY via this component's
//!     layer. A successful `<C-x>e` dispatch is therefore unambiguously
//!     attributable to the component, not the coincident native `<C-x>` leader.
//!
//! Declaring via the raw WIT calls (no SDK) exercises the CANONICAL,
//! language-agnostic surface — any component-model language calls
//! `register-mode` the same way.

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
    /// The host calls this once; the guest declares its leader mode through the
    /// imported `modes.register-mode` host function.
    fn register_modes() {
        // A universal minor mode (auto-active on every buffer, like the native
        // emacs-keys tribute) contributing a small `<C-x>` leader. Every target
        // is an EXISTING command resolved by name at bind time — the component
        // introduces no new command, exactly like the native mode.
        //
        // The two suffixes are component-EXCLUSIVE (native emacs-keys binds
        // `<C-x>2` for the split and `<C-x><C-s>` for write, never `<C-x>e` /
        // `<C-x>w`), so the host test can attribute a resolved dispatch to this
        // component's layer with certainty.
        modes::register_mode(&ModeDeclaration {
            id: "emacs-keys-plugin-mode".to_string(),
            kind: ModeKind::Minor,
            activation_policy: ActivationPolicy::Universal,
            capabilities: ModeCapabilities::empty(),
            keymap: vec![
                // `<C-x>e` — split the pane (targets the host `action:*` pane
                // command the native `<C-x>2` also uses).
                ModeKeymapBinding {
                    binding_mode: BindingMode::Normal,
                    chord: "<C-x>e".to_string(),
                    command: "action:split-pane-horizontal".to_string(),
                },
                // `<C-x>w` — save the buffer (the built-in `ex:write`, which the
                // native `<C-x><C-s>` also targets).
                ModeKeymapBinding {
                    binding_mode: BindingMode::Normal,
                    chord: "<C-x>w".to_string(),
                    command: "ex:write".to_string(),
                },
            ],
            // OM.2: majors claim a language; this is a minor, so `none`.
            target_language: None,
        });
    }
}

export!(Component);
