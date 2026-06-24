//! Server-initiated `workspace/configuration` plumbing
//! (Phase 4.1 follow-up; BC.8b reshape).
//!
//! When a language server sends `workspace/configuration` (most
//! commonly during its post-`initialize` startup to pull
//! per-server settings) the client must reply with one value
//! per requested item:
//!
//! ```json
//! {"items": [
//!   {"section": "rust-analyzer.cargo.features", "scopeUri": null},
//!   {"section": "rust-analyzer.checkOnSave",    "scopeUri": null}
//! ]}
//! ```
//!
//! Pre-this-module the actor responded with `[null, null]` for
//! every request -- spec-compliant but functionally useless.
//! With the §5.12 typed-options registry shipped + the TOML
//! loader from 4.2.g.5 (3a/3) caching the merged user + project
//! tree, the App can now look up each `section` against the
//! cached tree and surface the user's actual config.
//!
//! **BC.8b (2026-06-24): reshaped onto the generic inbound primitive.**
//! The bespoke `ConfigurationBus` (an mpsc sender with no wake, drained by a
//! host `Editor::drain_inbound_configuration_requests` method) is gone. The
//! supervisor now holds an `InboundBus<InboundConfigurationRequest>`
//! ([`lattice_mode::inbound`]) whose `send` **wakes the editor** so a
//! server-initiated request is answered off-keystroke, and whose per-tick drain
//! runs the **mode-owned** [`make_handler`] below. The handler is a *pure read*:
//! it walks each requested section in the shared `lsp.*` config tree and
//! resolves the request's oneshot — it emits no [`Effect`]. This is the cleanest
//! of the four LSP inbound buses (no `&mut Editor` work), so it sets the BC.8
//! reshape pattern for show-document / apply-edit / show-message-request.

use std::sync::Arc;

use arc_swap::ArcSwap;
use lattice_grammar::effect::Effect;
use serde_json::Value;
use tokio::sync::oneshot;

/// The bus the supervisor fans out to each actor — the generic inbound
/// primitive specialised to the configuration payload. `send` wakes the editor;
/// the per-tick drain runs [`make_handler`]. (Was the bespoke `ConfigurationBus`
/// before BC.8b.)
pub type ConfigurationBus = lattice_mode::inbound::InboundBus<InboundConfigurationRequest>;

/// One server-initiated `workspace/configuration` request,
/// ferried from the LSP actor to the editor's per-tick drain.
/// Carries the requested section paths verbatim; the handler
/// walks each in the shared TOML tree and writes the per-section
/// values back via the embedded oneshot in the same order.
#[derive(Debug)]
pub struct InboundConfigurationRequest {
    /// Server that sent the request -- recorded for the App's
    /// log entry. Cheap to clone (`Arc<str>`).
    pub server_id: Arc<str>,
    /// Workspace root the originating actor was spawned against
    /// (B'.2). Pairs with `server_id` to form the canonical
    /// `(server_id, workspace)` instance key.
    pub workspace: Arc<std::path::Path>,
    /// One section path per requested item. Spec lets `section`
    /// be `null`/missing (server wants all config); we coerce
    /// those to an empty string upstream so the app always sees
    /// a string.
    pub sections: Vec<String>,
    /// Oneshot the handler fills with one `serde_json::Value` per
    /// section (in input order). Missing sections come back as
    /// `Value::Null`.
    pub response: oneshot::Sender<Vec<Value>>,
}

/// BC.8b: the mode-owned handler for server-initiated
/// `workspace/configuration`, registered via `boot.inbound::<…>()`.
///
/// For each request it reads every requested `section` from the shared `lsp.*`
/// config tree (the same merged user+project tree the host edits on reload) and
/// resolves the request's oneshot with one `serde_json::Value` per section, in
/// input order. A dropped receiver (server gone) is fine — log-and-skip. It is a
/// pure read, so it emits **no** [`Effect`]: the `Vec<Effect>` it returns is
/// always empty.
///
/// `config_tree` is shared (`Arc<ArcSwap<…>>`) so the handler always reads the
/// *current* config — the host re-`store`s it on `:set` / config reload.
pub fn make_handler(
    config_tree: Arc<ArcSwap<toml::Table>>,
) -> impl FnMut(InboundConfigurationRequest) -> Vec<Effect> + Send + 'static {
    move |req| {
        let tree = config_tree.load();
        let values: Vec<Value> =
            req.sections.iter().map(|section| lookup_section(&tree, section)).collect();
        // A dropped response receiver (server gone) is fine — log-and-skip.
        let _ = req.response.send(values);
        Vec::new()
    }
}

/// Resolve one `workspace/configuration` section against the merged `lsp.*`
/// tree. An empty section means "all of `lsp`"; otherwise `lsp.<section>`.
/// Missing → `Value::Null` (the spec-compliant fallback).
fn lookup_section(tree: &toml::Table, section: &str) -> Value {
    let path = if section.is_empty() {
        "lsp".to_string()
    } else {
        format!("lsp.{section}")
    };
    match lattice_config::lookup_dotted_path(tree, &path) {
        // toml::Value -> serde_json::Value via the serde round-trip.
        Some(v) => serde_json::to_value(v).unwrap_or(Value::Null),
        None => Value::Null,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The handler resolves the request's oneshot with one value per requested
    /// section, in input order, reading the shared tree.
    #[tokio::test]
    async fn handler_resolves_sections_in_order() {
        let toml_src = "[lsp.rust-analyzer.cargo]\nfeatures = \"all\"\n";
        let tree: toml::Table = toml::from_str(toml_src).expect("valid toml");
        let shared = Arc::new(ArcSwap::from_pointee(tree));
        let mut handler = make_handler(shared);

        let (tx, resp_rx) = oneshot::channel();
        let effects = handler(InboundConfigurationRequest {
            server_id: Arc::from("test"),
            workspace: Arc::<std::path::Path>::from(std::path::Path::new("/tmp")),
            sections: vec!["rust-analyzer.cargo".into(), "does.not.exist".into()],
            response: tx,
        });
        assert!(effects.is_empty(), "configuration is a pure read — no effects");
        let values = resp_rx.await.expect("handler resolved the oneshot");
        assert_eq!(values.len(), 2);
        assert_eq!(values[0]["features"], serde_json::json!("all"));
        assert_eq!(values[1], serde_json::Value::Null);
    }

    /// An empty section means "all of `lsp`" — the whole sub-tree comes back as
    /// a JSON object (the convention the retired TUI test pinned).
    #[tokio::test]
    async fn handler_empty_section_returns_whole_lsp_subtree() {
        let tree: toml::Table =
            toml::from_str("[lsp.rust-analyzer]\nchecker = \"clippy\"\n").expect("valid toml");
        let shared = Arc::new(ArcSwap::from_pointee(tree));
        let mut handler = make_handler(shared);
        let (tx, resp_rx) = oneshot::channel();
        handler(InboundConfigurationRequest {
            server_id: Arc::from("rust-analyzer"),
            workspace: Arc::<std::path::Path>::from(std::path::Path::new("/tmp")),
            sections: vec![String::new()],
            response: tx,
        });
        let values = resp_rx.await.expect("handler resolved");
        let obj = values[0].as_object().expect("whole lsp subtree is a JSON object");
        assert!(obj.contains_key("rust-analyzer"));
    }

    /// A dropped response receiver (server gone) does not panic the handler.
    #[tokio::test]
    async fn handler_tolerates_dropped_receiver() {
        let shared = Arc::new(ArcSwap::from_pointee(toml::Table::new()));
        let mut handler = make_handler(shared);
        let (tx, resp_rx) = oneshot::channel::<Vec<Value>>();
        drop(resp_rx);
        let effects = handler(InboundConfigurationRequest {
            server_id: Arc::from("test"),
            workspace: Arc::<std::path::Path>::from(std::path::Path::new("/tmp")),
            sections: Vec::new(),
            response: tx,
        });
        assert!(effects.is_empty());
    }
}
