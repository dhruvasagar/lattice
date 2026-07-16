//! PL8.D.1 — the `keymap` seam, driven through a real guest.
//!
//! Instantiates the `keymap-guest` fixture (a `wasm32-wasip2` `keymap-plugin`
//! component) via [`PluginHost::spawn_keymap_plugin`] against a native
//! `KeymapHandle` + `CommandRegistry`, proving the seam end to end:
//!   - a well-formed binding to a real command lands in `KeymapLayer::User`,
//!     resolvable via the native trie (stamped `SourceLayer::Plugin`),
//!   - an unregistered command binds nothing (graceful skip, no trap), and
//!   - the host returns the bound tokens for teardown (PL8.D.2).
//!
//! Skips when the fixture wasn't built (no `wasm32-wasip2` target).

#![allow(clippy::unwrap_used, clippy::panic)]

use std::sync::Arc;

use lattice_grammar::registry::CommandRegistry;
use lattice_keymap::{BindingMode, KeymapHandle, LookupResult};
use lattice_mode::CapabilitySet;
use lattice_plugin_host::{PluginBudget, PluginHost, PluginManifest, TrustTier};
use tempfile::TempDir;

fn guest_wasm() -> Option<&'static str> {
    let path = env!("KEYMAP_GUEST_WASM");
    (!path.is_empty()).then_some(path)
}

/// A command registry with the builtin ex-commands populated, so `ex:write`
/// resolves at bind time.
fn commands() -> Arc<CommandRegistry> {
    let mut r = CommandRegistry::new();
    let _ = lattice_grammar::ex_commands::populate(&mut r);
    Arc::new(r)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn keymap_plugin_binds_a_user_keybinding_and_reports_tokens() {
    let Some(path) = guest_wasm() else {
        eprintln!("SKIP: keymap-guest fixture not built (add the wasm32-wasip2 target)");
        return;
    };

    let dir = TempDir::new().unwrap();
    let host = PluginHost::with_dirs(dir.path().join("cache"), dir.path().join("data"))
        .expect("host builds with tempdirs");
    let component = host.compile(&std::fs::read(path).unwrap()).expect("compile keymap fixture");
    let manifest = PluginManifest::new("keymap-fixture", Vec::new(), CapabilitySet::empty());

    let commands = commands();
    let keymap = KeymapHandle::new();

    let (plugin_id, tokens) = host
        .spawn_keymap_plugin(
            &component,
            &manifest,
            TrustTier::Bundled,
            PluginBudget::default(),
            &keymap,
            &commands,
        )
        .await
        .expect("spawn keymap plugin");

    assert_ne!(plugin_id.0 as i64, -1, "a host id is issued");
    // Exactly one binding landed — `<C-s>`→`ex:write`; the `gq`→unregistered one
    // was gracefully skipped (bound nothing, no trap).
    assert_eq!(tokens.len(), 1, "one binding landed, one skipped: {tokens:?}");
    assert_eq!(tokens[0].mode, BindingMode::Normal);
    assert_eq!(tokens[0].chord, "<C-s>");

    // The binding is live in the native User layer: `<C-s>` in Normal resolves.
    let chord = lattice_protocol::parse_chord_sequence("<C-s>").expect("chord parses");
    assert!(
        matches!(
            keymap.lookup(BindingMode::Normal, &chord),
            LookupResult::Bound { .. }
        ),
        "the plugin's user keybinding resolves through the native trie"
    );
    assert_eq!(keymap.binding_count(), 1, "exactly the one binding is live");
}
