//! AT.5 — the llm-tools seam, driven through a real guest.
//!
//! Instantiates the `llm-tools-guest` fixture via
//! [`PluginHost::spawn_llm_tools`] and drives its exports through the
//! [`LlmToolsClient`] bridge, as the loader's adapter will:
//!
//!   - `tools()` and `profiles()` cross back with every field;
//!   - a call's arguments cross in (the fixture echoes them), and a propose
//!     tool's edits cross back as data;
//!   - a call reads under the plugin's grant and **cannot delete under it**,
//!     though the grant is `fs:write`: the seam's store is read-only, so a
//!     tool changes a file only by proposing it;
//!   - a guest `err` is that call's answer and the plugin stays usable;
//!   - a trap quarantines the plugin, and every later call says so.
//!
//! Skips when the fixture wasn't built (no `wasm32-wasip2` target).

#![allow(clippy::unwrap_used, clippy::panic)]

use std::path::Path;
use std::sync::Arc;

use lattice_mode::CapabilitySet;
use lattice_plugin_host::llm_tools_task::{LlmToolKind, LlmToolResult};
use lattice_plugin_host::{
    Capability, LlmToolsClient, PluginBudget, PluginHost, PluginHostError, PluginManifest,
    TrustTier,
};

fn guest_wasm() -> Option<&'static str> {
    let path = env!("LLM_TOOLS_GUEST_WASM");
    (!path.is_empty()).then_some(path)
}

/// Spawn the fixture with `caps` granted and drive its actor, the way the
/// loader's drain does. The host's dirs live under `dirs`.
async fn client(dirs: &Path, caps: Vec<Capability>) -> LlmToolsClient {
    let host = PluginHost::with_dirs(dirs.join("cache"), dirs.join("data")).unwrap();
    let component = host
        .compile(&std::fs::read(guest_wasm().unwrap()).unwrap())
        .expect("compile llm-tools fixture");
    let manifest = PluginManifest::new("fixture", caps, CapabilitySet::empty());
    let (client, actor) = host
        .spawn_llm_tools(
            &component,
            &manifest,
            TrustTier::Bundled,
            PluginBudget::default(),
            &Arc::new(lattice_runtime::EventBus::new()),
            None,
        )
        .await
        .expect("spawn llm-tools");
    tokio::spawn(actor.run());
    client
}

async fn call(client: &LlmToolsClient, name: &str, args: &str) -> Result<LlmToolResult, String> {
    client
        .call(name.to_string(), args.to_string())
        .await
        .expect("no host-side trap")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tools_and_profiles_cross_with_every_field() {
    let Some(_) = guest_wasm() else {
        return;
    };
    let dirs = tempfile::tempdir().unwrap();
    let client = client(dirs.path(), Vec::new()).await;

    let tools = client.tools().await.unwrap();
    let rows: Vec<_> = tools
        .iter()
        .map(|t| (t.name.as_str(), t.kind, t.description.as_str()))
        .collect();
    assert_eq!(
        rows,
        vec![
            ("outline", LlmToolKind::Read, "The outline of the notes."),
            ("change", LlmToolKind::Propose, "Mark the first TODO done."),
        ]
    );
    // Written by the SDK from the guest's `Outline` struct, doc included.
    let schema: serde_json::Value = serde_json::from_str(&tools[0].input_schema).unwrap();
    assert_eq!(schema["type"], "object");
    assert_eq!(
        schema["properties"]["q"],
        serde_json::json!({"type": "string", "description": "Only headlines matching this."})
    );
    assert_eq!(schema["required"], serde_json::json!([]));

    let profiles = client.profiles().await.unwrap();
    assert_eq!(profiles.len(), 1);
    assert_eq!(profiles[0].name, "fixture-plan");
    assert_eq!(profiles[0].description, "Plan from the fixture's notes.");
    assert_eq!(
        profiles[0].settings,
        vec![
            ("system".to_string(), "Plan the day.".to_string()),
            ("tools".to_string(), "fixture.outline".to_string()),
        ]
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_call_carries_the_input_in_and_its_proposals_out() {
    let Some(_) = guest_wasm() else {
        return;
    };
    let dirs = tempfile::tempdir().unwrap();
    let client = client(dirs.path(), Vec::new()).await;

    let read = call(&client, "outline", r#"{"q":"work"}"#).await.unwrap();
    assert_eq!(read.text, r#"outline for {"q":"work"}"#);
    assert!(read.proposals.is_empty());
    // The SDK's parse in the guest: a wrong kind is the typed `err`, naming
    // the field for the model, and the guest is not quarantined for it.
    let wrong = call(&client, "outline", r#"{"q":3}"#).await.unwrap_err();
    assert_eq!(wrong, "q: expected string, got integer");
    assert!(call(&client, "outline", "").await.is_ok());

    let change = call(&client, "change", "{}").await.unwrap();
    assert_eq!(change.text, "marking it done");
    assert_eq!(change.proposals.len(), 1);
    let edit = &change.proposals[0];
    assert_eq!(
        (
            edit.path.as_str(),
            edit.old_string.as_str(),
            edit.new_string.as_str()
        ),
        ("notes.org", "* TODO ship", "* DONE ship")
    );
}

/// **The test that matters.** The plugin holds `fs:write` over the directory,
/// which its other seams may use. A tool call reads there and cannot delete
/// there, so the only way a tool changes a file is a proposal the host
/// reviews.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_call_reads_under_its_grant_and_cannot_write_under_it() {
    let Some(_) = guest_wasm() else {
        return;
    };
    let dirs = tempfile::tempdir().unwrap();
    let notes = tempfile::tempdir().unwrap();
    let file = notes.path().join("notes.org");
    std::fs::write(&file, "* TODO ship\n").unwrap();
    let client = client(
        dirs.path(),
        vec![Capability::FsWrite(notes.path().to_path_buf())],
    )
    .await;
    let path = file.to_str().unwrap();

    assert_eq!(
        call(&client, "read", path).await.unwrap().text,
        "* TODO ship\n"
    );

    let deleted = call(&client, "delete", path).await.unwrap().text;
    assert!(deleted.starts_with("error: fs delete denied"), "{deleted}");
    assert!(file.exists(), "a tool call deleted a file");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_guest_err_is_the_calls_answer_and_the_plugin_stays_usable() {
    let Some(_) = guest_wasm() else {
        return;
    };
    let dirs = tempfile::tempdir().unwrap();
    let client = client(dirs.path(), Vec::new()).await;

    assert_eq!(
        call(&client, "fail", "{}").await.unwrap_err(),
        "fixture: nothing matched"
    );
    assert!(call(&client, "outline", "{}").await.is_ok());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_trap_quarantines_the_plugin_and_later_calls_say_so() {
    let Some(_) = guest_wasm() else {
        return;
    };
    let dirs = tempfile::tempdir().unwrap();
    let client = client(dirs.path(), Vec::new()).await;

    let trapped = client
        .call("trap".to_string(), "{}".to_string())
        .await
        .unwrap_err();
    assert!(
        matches!(trapped, PluginHostError::Trap { func: "call", .. }),
        "{trapped:?}"
    );
    let later = client
        .call("outline".to_string(), "{}".to_string())
        .await
        .unwrap_err();
    assert!(
        matches!(later, PluginHostError::Quarantined { func: "call" }),
        "{later:?}"
    );
}
