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

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use lattice_completion::candidate::RawCandidate;
use lattice_completion::source::{AsyncCompletionSource, CandidateSink, InsertContextSnapshot};
use lattice_protocol::CancellationToken;

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
    accepts_non_word_query: bool,
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
            accepts_non_word_query: spec.accepts_non_word_query,
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

    /// OR.7: whether this source keeps matching once the query picks up a
    /// non-word character (a phrase source — org-roam's node titles).
    pub fn accepts_non_word_query(&self) -> bool {
        self.accepts_non_word_query
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
    pub async fn generate(&self, ctx: &InsertContextSnapshot) -> Result<Vec<RawCandidate>, String> {
        let ctx = WitGenerateContext {
            prefix: ctx.query.clone(),
            case_sensitive: ctx.case_sensitive,
            // OR.7: the two fields that let a source decide whether it
            // applies at all. Without them every plugin source fires in
            // every buffer on every prefix.
            line_before_cursor: ctx.line_before_cursor.clone(),
            language: ctx.language.clone(),
        };
        let wit = match self.client.generate(ctx).await {
            Ok(inner) => inner?,
            Err(host_err) => return Err(format!("completion plugin: {host_err}")),
        };
        wit.into_iter().map(RawCandidate::from_wit).collect()
    }
}

impl std::fmt::Debug for WasmCompletionSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WasmCompletionSource")
            .field("id", &self.id)
            .finish_non_exhaustive()
    }
}

/// PH7.6 → PL8.B: the adapter that lets a WASM completion source ride a mode's
/// `completion_sources()` like any native async source (LSP's precedent). The
/// aggregator drives `produce_async` at popup-open / `isIncomplete` refresh; the
/// async `generate` runs on the source's actor (spawned by the loader on the
/// multi-thread runtime), **never** the keystroke path — matching / ranking /
/// annotation stay native (the host runs `match_and_rank` over the pushed
/// candidates), so paramount #1 holds.
impl AsyncCompletionSource for WasmCompletionSource {
    fn produce_async(
        &self,
        ctx: InsertContextSnapshot,
        sink: Arc<dyn CandidateSink>,
        token: CancellationToken,
    ) -> Pin<Box<dyn Future<Output = ()> + Send>> {
        // Cheap clone (mpsc `Sender` + cached id/doc) — the future outlives the
        // aggregator's stack frame as it crosses the spawn boundary.
        let source = self.clone();
        Box::pin(async move {
            if token.is_cancelled() {
                return;
            }
            // A host trap / plugin-gone (outer) or a guest WIT `err` (inner) both
            // collapse to a logged zero-candidate result — never a panic, never a
            // poisoned popup (§8 graceful degradation).
            match source.generate(&ctx).await {
                Ok(candidates) => {
                    for candidate in candidates {
                        if token.is_cancelled() {
                            return;
                        }
                        sink.push(candidate);
                    }
                }
                Err(err) => tracing::debug!(
                    source = %source.id,
                    error = %err,
                    "wasm completion source produced no candidates"
                ),
            }
        })
    }
}
