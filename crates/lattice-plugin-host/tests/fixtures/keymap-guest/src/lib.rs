//! PL8.D.1 keymap fixture guest.
//!
//! A minimal `wasm32-wasip2` component implementing the `keymap-plugin` world:
//! from its `register-keymap` export (the host calls it once) it binds user
//! keybindings via the imported `keymap.register-binding` —
//!   - `<C-s>` (Normal) → `ex:write` (a real builtin the host test populates), and
//!   - `gq` (Normal) → `no-such-command` (unregistered — exercises the
//!     graceful-skip path: the host returns `false`, binds nothing).
//!
//! Binding via the raw WIT call (no SDK) is deliberate: it exercises the
//! CANONICAL, language-agnostic keybinding surface any component-model language
//! uses. The first real consumer is the user's `init.rs`.

wit_bindgen::generate!({
    world: "keymap-plugin",
    path: "../../../../../wit",
});

use lattice::plugin_host::keymap;
use lattice::plugin_host::keymap::BindingMode;

struct Component;

impl Guest for Component {
    fn register_keymap() {
        // A well-formed binding to a real command — lands in KeymapLayer::User.
        let _ok = keymap::register_binding(BindingMode::Normal, "<C-s>", "ex:write");
        // An unregistered command — the host binds nothing and returns false
        // (graceful degradation, no trap).
        let _skipped = keymap::register_binding(BindingMode::Normal, "gq", "no-such-command");
    }
}

export!(Component);
