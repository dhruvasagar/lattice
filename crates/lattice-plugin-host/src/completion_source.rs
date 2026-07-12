//! PH7.6 — the `WasmCompletionSource` adapter (the async-produce path).
//!
//! Wraps a completion plugin's [`CompletionClient`] bridge. Unlike the picker
//! adapter, this is NOT an `Arc<dyn CandidateGenerator>` inserted into the
//! synchronous completion pipeline — a WASM `generate` is async + actor-bound,
//! and matching/annotation run *per candidate* on the keystroke path (paramount
//! #1). Instead, following the LSP-completion precedent (`pipeline.rs`
//! `match_and_rank` "pre-supplies rows from async LSP responses"), this adapter
//! **produces candidates asynchronously**; the host then runs the NATIVE
//! `match_and_rank` over them (matching / ranking / annotation stay native).
//! Option A, locked with Dhruva — see `wit/completion-source.wit`.

use lattice_completion::candidate::RawCandidate;

use crate::WitBoundary;
use crate::completion_task::GenerateContext as WitGenerateContext;
use crate::{CompletionClient, PluginHostError};

/// An async completion producer over a plugin's [`CompletionClient`]. Cheap to
/// clone (the client is an mpsc `Sender` clone + cached id/doc); every clone
/// talks to the same actor.
#[derive(Clone)]
pub struct WasmCompletionSource {
    client: CompletionClient,
    /// The source id + doc, converted once at [`connect`](Self::connect) — the
    /// `(name, doc)` `insert_generator` stamps when a host wires this in.
    id: String,
    doc: String,
}

impl WasmCompletionSource {
    /// Fetch the plugin's `spec` through the bridge and build the adapter. Async
    /// because the one-time spec fetch is a guest call; a dead actor is a typed
    /// error, so a bad plugin fails registration loudly.
    pub async fn connect(client: CompletionClient) -> Result<Self, PluginHostError> {
        let spec = client.spec().await?;
        Ok(Self {
            client,
            id: spec.id,
            doc: spec.doc,
        })
    }

    /// The completion source's id (the `insert_generator` name).
    pub fn id(&self) -> &str {
        &self.id
    }

    /// The completion source's doc string.
    pub fn doc(&self) -> &str {
        &self.doc
    }

    /// The host-issued id of the plugin behind this source.
    pub fn plugin_id(&self) -> crate::PluginId {
        self.client.id()
    }

    /// Produce raw candidates for `prefix` — the async generator. The result is
    /// native [`RawCandidate`]s the host feeds through `match_and_rank`
    /// (matching/ranking/annotation stay native). The outer host error (trap /
    /// plugin-gone) and the inner guest WIT `err` both collapse to the `String`
    /// the completion machinery logs; a candidate that fails to cross (malformed
    /// record) fails the whole batch as a typed error, never a silent drop.
    pub async fn generate(
        &self,
        prefix: &str,
        case_sensitive: bool,
    ) -> Result<Vec<RawCandidate>, String> {
        let ctx = WitGenerateContext {
            prefix: prefix.to_string(),
            case_sensitive,
        };
        let wit = match self.client.generate(ctx).await {
            Ok(inner) => inner?,
            Err(host_err) => return Err(format!("completion plugin: {host_err}")),
        };
        wit.into_iter().map(RawCandidate::from_wit).collect()
    }
}
