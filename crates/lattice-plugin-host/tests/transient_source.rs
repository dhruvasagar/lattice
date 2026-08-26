//! TR.2b — the transient-menu seam, driven through a real guest.
//!
//! Instantiates the `transient-guest` fixture via
//! [`PluginHost::spawn_transient_source`], drives its `id` / `build` exports
//! through the [`TransientClient`] bridge, and converts the result with the
//! same [`spec_from_wit`] the registry builder uses — the whole seam end to
//! end:
//!
//!   - `id()` crosses back as the name the menu registers under;
//!   - the `transient-context` projection crosses IN (the fixture echoes the
//!     major mode and the minor count into the menu's title, so this is
//!     asserted on data only the guest could have produced);
//!   - rows cross back with their keys, labels, resolved `CommandId`s and
//!     **per-row args** — two rows naming the same command with different
//!     arguments, which is the property the seam exists for;
//!   - a row naming an unregistered command is dropped and the rest of the
//!     menu survives;
//!   - a guest `err` surfaces as a typed error and the source is still usable
//!     afterwards (an `err` is a statement, not a quarantine).
//!
//! Skips when the fixture wasn't built (no `wasm32-wasip2` target).

#![allow(clippy::unwrap_used, clippy::panic)]

use std::sync::Arc;

use lattice_mode::CapabilitySet;
use lattice_picker::{TransientContext, TransientItemKind, TransientSpec};
use lattice_plugin_host::{
    PluginBudget, PluginHost, PluginManifest, TransientClient, TrustTier,
    project_transient_context, spec_from_wit,
};

const COMMAND: &str = "fixture-capture-key";

fn guest_wasm() -> Option<&'static str> {
    let path = env!("TRANSIENT_GUEST_WASM");
    (!path.is_empty()).then_some(path)
}

/// A command registry holding the one command the fixture's good rows name.
/// The ghost row's command is deliberately absent.
fn commands() -> lattice_grammar::CommandRegistryHandle {
    let mut reg = lattice_grammar::CommandRegistry::new();
    reg.register_action(
        COMMAND,
        "fixture capture (test)",
        lattice_grammar::registry::ActionSpec {
            args_schema: Vec::new(),
            apply: Arc::new(|_| Ok(lattice_grammar::Effect::None)),
        },
    );
    Arc::new(arc_swap::ArcSwap::from_pointee(reg))
}

/// Spawn the fixture and drive its actor, the way the loader's drain does.
async fn client(host: &PluginHost) -> TransientClient {
    let component = host
        .compile(&std::fs::read(guest_wasm().unwrap()).unwrap())
        .expect("compile transient fixture");
    let manifest = PluginManifest::new("transient-fixture", Vec::new(), CapabilitySet::empty());
    let (client, actor) = host
        .spawn_transient_source(
            &component,
            &manifest,
            TrustTier::Bundled,
            PluginBudget::default(),
            &Arc::new(lattice_runtime::EventBus::new()),
        )
        .await
        .expect("spawn transient source");
    tokio::spawn(actor.run());
    client
}

/// Build the menu through the guest and convert it, exactly as the registry
/// builder does.
///
/// The registry is passed in rather than minted here: `CommandId`s are
/// allocated process-globally and monotonically, so two registries built from
/// the same names hand out DIFFERENT ids — and a test that compared across
/// them would fail for a reason that has nothing to do with the seam.
async fn build(
    client: &TransientClient,
    registry: &lattice_grammar::CommandRegistryHandle,
    ctx: &TransientContext,
) -> Result<TransientSpec, String> {
    let wit = client
        .build(project_transient_context(ctx))
        .await
        .expect("no host-side trap")?;
    Ok(spec_from_wit(wit, registry, "transient-fixture"))
}

fn in_org() -> TransientContext {
    TransientContext {
        major_mode: Some("org-mode".into()),
        minor_modes: vec!["org-global-mode".into(), "auto-pair-mode".into()],
        buffer: Some(lattice_core::BufferId(3)),
    }
}

/// The name the menu registers under — what `Effect::OpenTransient` addresses.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_guest_names_its_menu() {
    let Some(_) = guest_wasm() else {
        eprintln!("SKIP: transient fixture guest not built (add the wasm32-wasip2 target)");
        return;
    };
    let host = PluginHost::new().unwrap();
    let client = client(&host).await;
    assert_eq!(client.menu_id().await.unwrap(), "fixture-capture");
}

/// The context crosses IN. The fixture builds its title out of the projection,
/// so this is asserted on data only the guest could have produced — a
/// signature alone would be satisfied by a guest that ignored its argument.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_open_context_reaches_the_guest() {
    let Some(_) = guest_wasm() else {
        eprintln!("SKIP: transient fixture guest not built");
        return;
    };
    let host = PluginHost::new().unwrap();
    let client = client(&host).await;
    let registry = commands();

    let spec = build(&client, &registry, &in_org())
        .await
        .expect("the menu builds");
    assert_eq!(spec.title, "org-mode (2 minors)");

    // A second build with a different context must REBUILD, not replay — the
    // whole reason `build` is per open rather than cached at registration.
    let spec = build(&client, &registry, &TransientContext::default())
        .await
        .expect("the menu builds");
    assert_eq!(spec.title, "no-major (0 minors)");
}

/// The headline: two rows, one command, different args. Without the per-row
/// slot a menu whose rows differ only in a parameter is inexpressible, and
/// that is exactly the shape a capture menu has.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rows_cross_back_with_their_own_args() {
    let Some(_) = guest_wasm() else {
        eprintln!("SKIP: transient fixture guest not built");
        return;
    };
    let host = PluginHost::new().unwrap();
    let client = client(&host).await;
    let registry = commands();
    let spec = build(&client, &registry, &in_org())
        .await
        .expect("the menu builds");

    let expected_id = registry.load().id_by_name(COMMAND).unwrap();
    let arg_for = |key: &str| {
        spec.groups[0]
            .items
            .iter()
            .find(|i| i.key.iter().any(|k| k == key))
            .and_then(|i| match &i.kind {
                TransientItemKind::Action { command, args } => {
                    assert_eq!(*command, expected_id, "row `{key}` resolved its command");
                    Some(format!("{args:?}"))
                }
                _ => None,
            })
            .unwrap_or_else(|| panic!("no action row keyed `{key}`"))
    };
    assert!(arg_for("t").contains("todo"));
    assert!(arg_for("n").contains("note"));

    assert_eq!(spec.footer.as_deref(), Some("q to dismiss"));
    assert!(
        spec.groups[0]
            .items
            .iter()
            .any(|i| matches!(i.kind, TransientItemKind::Dismiss)),
        "the dismiss row crossed — a menu without `q` is a trap"
    );
}

/// One bad row costs that row, not the menu. The fixture names a command
/// nobody registered; the other three must survive.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_unregistered_command_drops_only_its_row() {
    let Some(_) = guest_wasm() else {
        eprintln!("SKIP: transient fixture guest not built");
        return;
    };
    let host = PluginHost::new().unwrap();
    let client = client(&host).await;
    let registry = commands();
    let spec = build(&client, &registry, &in_org())
        .await
        .expect("the menu builds");

    let keys: Vec<&str> = spec.groups[0]
        .items
        .iter()
        .map(|i| i.key[0].as_str())
        .collect();
    assert_eq!(
        keys,
        vec!["t", "n", "q"],
        "the ghost row is gone and the rest of the menu is intact"
    );
}

/// A guest `err` is a typed error, not a trap — and the source keeps working
/// afterwards, so one refused open does not cost the plugin.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_guest_error_is_typed_and_does_not_quarantine_the_source() {
    let Some(_) = guest_wasm() else {
        eprintln!("SKIP: transient fixture guest not built");
        return;
    };
    let host = PluginHost::new().unwrap();
    let client = client(&host).await;
    let registry = commands();

    let broken = TransientContext {
        major_mode: Some("broken-mode".into()),
        minor_modes: Vec::new(),
        buffer: None,
    };
    let err = build(&client, &registry, &broken)
        .await
        .expect_err("the guest declines");
    assert!(
        err.contains("no templates configured"),
        "the guest's own words reach the host: {err}"
    );

    assert!(
        build(&client, &registry, &in_org()).await.is_ok(),
        "an err is a statement, not a quarantine — the next open must work"
    );
}
