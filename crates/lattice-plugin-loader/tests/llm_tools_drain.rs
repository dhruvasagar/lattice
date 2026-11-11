//! AT.5c — the loader's `llm-tools` drain, end to end.
//!
//! A discovered plugin declaring `provides = ["llm-tools"]` has its tools
//! offered to the assistant under its id, with the permission row its grant
//! decides; a model call reaches the guest and back; a proposal goes through
//! lattice's `edit_file`; unload withdraws the tools; and a plugin whose id
//! is a native source's name still loads, without its tools.
//!
//! Skips when the fixture guest wasn't built (no `wasm32-wasip2` target).

#![allow(clippy::unwrap_used, clippy::panic)]

use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};

use lattice_ai::conversation::ToolKind;
use lattice_ai::llm::tools::plugin::{PluginTools, PluginToolsHandle, PluginToolsStatus};
use lattice_ai::llm::tools::{
    BoxFuture, CallContext, Permission, SourceTool, ToolBroker, ToolBrokerHandle, ToolError,
    ToolOutput, ToolSource,
};
use lattice_plugin_host::TrustTier;
use lattice_plugin_loader::{LoaderServices, PluginLoader};
use lattice_runtime::EventBus;
use serde_json::{Value, json};

fn guest_wasm() -> Option<Vec<u8>> {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../lattice-plugin-host/tests/fixtures/llm-tools-guest/target/wasm32-wasip2/release/llm_tools_guest.wasm"
    );
    std::fs::read(path).ok()
}

/// Stands in for lattice's own tools: records each `edit_file` it is asked.
#[derive(Default)]
struct Lattice {
    edits: Mutex<Vec<Value>>,
}

impl ToolSource for Lattice {
    fn list(&self) -> BoxFuture<'_, Result<Vec<SourceTool>, ToolError>> {
        Box::pin(async { Ok(Vec::new()) })
    }
    fn call(
        &self,
        name: &str,
        input: Value,
        _ctx: CallContext,
    ) -> BoxFuture<'_, Result<ToolOutput, ToolError>> {
        assert_eq!(name, "edit_file");
        let path = input["path"].as_str().unwrap().to_string();
        self.edits.lock().unwrap().push(input);
        Box::pin(async move {
            Ok(ToolOutput {
                text: format!("wrote `{path}`"),
            })
        })
    }
}

struct Rig {
    loader: PluginLoader,
    broker: ToolBrokerHandle,
    tools: PluginToolsHandle,
    lattice: Arc<Lattice>,
    plugins: std::path::PathBuf,
    _base: tempfile::TempDir,
}

/// A loader wired with only what this seam needs, and one plugin directory
/// holding the fixture as `id` with `capabilities`.
fn rig(id: &str, capabilities: &str) -> Option<Rig> {
    let wasm = guest_wasm()?;
    let base = tempfile::tempdir().unwrap();
    let plugins = base.path().join("plugins");
    let dir = plugins.join(id);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("plugin.toml"),
        format!("id = \"{id}\"\nprovides = [\"llm-tools\"]\ncapabilities = [{capabilities}]\n"),
    )
    .unwrap();
    std::fs::write(dir.join("component.wasm"), wasm).unwrap();

    let broker: ToolBrokerHandle = Arc::new(ToolBroker::new());
    let lattice = Arc::new(Lattice::default());
    let tools: PluginToolsHandle = Arc::new(PluginTools::new(
        broker.clone(),
        lattice.clone(),
        Arc::new(Vec::new),
    ));
    let host = Arc::new(
        lattice_plugin_host::PluginHost::with_dirs(
            base.path().join("cache"),
            base.path().join("data"),
        )
        .unwrap(),
    );
    let loader = PluginLoader::with_services(
        host,
        LoaderServices {
            runtime: Some(tokio::runtime::Handle::current()),
            bus: Some(Arc::new(EventBus::new())),
            plugin_tools: Some(tools.clone()),
            ..Default::default()
        },
    );
    Some(Rig {
        loader,
        broker,
        tools,
        lattice,
        plugins,
        _base: base,
    })
}

fn select(s: &[&str]) -> BTreeSet<String> {
    s.iter().map(|s| s.to_string()).collect()
}

async fn load(rig: &Rig) {
    let n = rig
        .loader
        .discover_and_load(&rig.plugins, TrustTier::Bundled)
        .await;
    assert_eq!(n, 1, "the plugin loads");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_contained_plugins_tools_are_offered_and_called_through_the_guest() {
    let Some(rig) = rig("notes", "") else {
        eprintln!("skipping: llm-tools-guest wasm not built");
        return;
    };
    load(&rig).await;

    let offer = rig.broker.offer(&select(&["notes.*"])).await;
    let rows: Vec<_> = offer
        .tools()
        .iter()
        .map(|t| (t.wire.as_str(), t.permission, t.kind))
        .collect();
    assert_eq!(
        rows,
        vec![
            ("notes__outline", Permission::Run, ToolKind::Read),
            ("notes__change", Permission::Review, ToolKind::Edit),
        ]
    );

    let read = offer
        .call(
            "notes__outline",
            json!({"q": "work"}),
            CallContext::default(),
        )
        .await
        .unwrap();
    assert_eq!(read.text, r#"outline for {"q":"work"}"#);

    let change = offer
        .call("notes__change", json!({}), CallContext::default())
        .await
        .unwrap();
    assert!(
        change.text.starts_with("marking it done"),
        "{}",
        change.text
    );
    assert!(change.text.contains("wrote `notes.org`"), "{}", change.text);
    let edits = rig.lattice.edits.lock().unwrap().clone();
    assert_eq!(
        edits,
        vec![json!({
            "path": "notes.org",
            "old_string": "* TODO ship",
            "new_string": "* DONE ship",
        })]
    );
}

/// The row comes from the grant: the same plugin able to reach the network
/// asks before every tool, propose included.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_plugin_that_can_reach_the_network_asks() {
    let Some(rig) = rig("notes", "\"net:http:api.example.com\"") else {
        eprintln!("skipping: llm-tools-guest wasm not built");
        return;
    };
    load(&rig).await;

    let offer = rig.broker.offer(&select(&["notes.*"])).await;
    assert_eq!(offer.tools().len(), 2);
    assert!(
        offer
            .tools()
            .iter()
            .all(|t| t.permission == Permission::Ask),
        "{:?}",
        offer.tools()
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unloading_withdraws_the_tools() {
    let Some(rig) = rig("notes", "") else {
        eprintln!("skipping: llm-tools-guest wasm not built");
        return;
    };
    load(&rig).await;
    assert!(!rig.broker.offer(&select(&["notes.*"])).await.is_empty());

    let report = rig.loader.unload("notes").expect("unload succeeds");
    assert_eq!(report.llm_tools, 1);
    assert!(rig.broker.offer(&select(&["notes.*"])).await.is_empty());
    assert!(rig.tools.statuses().is_empty());
}

/// A plugin named like a native source loads (its other seams would work)
/// and its tools are not offered, with the reason listed.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_plugin_named_like_a_native_source_loads_without_its_tools() {
    let Some(rig) = rig("web", "") else {
        eprintln!("skipping: llm-tools-guest wasm not built");
        return;
    };
    load(&rig).await;

    assert!(rig.broker.offer(&select(&["web.outline"])).await.is_empty());
    match rig.tools.statuses().get("web") {
        Some(PluginToolsStatus::Refused(why)) => assert!(why.contains("lattice's own"), "{why}"),
        other => panic!("expected a refusal, got {other:?}"),
    }
    let report = rig.loader.unload("web").expect("unload succeeds");
    assert_eq!(
        report.llm_tools, 0,
        "nothing was offered, so nothing is withdrawn"
    );
}
