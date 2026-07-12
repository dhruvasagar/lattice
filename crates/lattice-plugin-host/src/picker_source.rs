//! PH7.4c.2 — the `WasmPickerSource` host adapter (the create path).
//!
//! Wraps a plugin's picker exports (driven through the [`PickerClient`] bridge,
//! PH7.4c.1b) as an `Arc<dyn PickerSourceGenerator>` so a plugin source is
//! indistinguishable from a first-party one at the `PickerRegistry`
//! (`register_generator`). The boundary conversions are PH7.4a's
//! `WitBoundary` + `project_picker_context`; this adapter only sequences them
//! against the trait's sync/async contract.
//!
//! ## Mapping the trait onto an async, actor-bound guest
//!
//! `PickerSourceGenerator`'s methods are synchronous, but the guest exports are
//! async and bound to the plugin's actor task (`picker_task.rs`). The three
//! methods resolve that mismatch differently:
//!
//! - **`spec`** is fetched once at [`connect`](WasmPickerSource::connect) time
//!   and cached natively, so the sync `spec(&self) -> &PickerSourceSpec` is a
//!   borrow — no per-call guest hop.
//! - **`init`** returns [`PickerInitResult::Future`]: the sync prelude projects
//!   the borrowed context into an owned WIT record (§4.2) and moves it into a
//!   `'static` future that awaits `client.init` off-thread, then converts the
//!   WIT candidate pairs back. This drops straight into the host's existing
//!   `pending_picker_init` drain.
//! - **`accept`** returns `Some` from [`accept_async`](PickerSourceGenerator::accept_async):
//!   the same sync-prelude-then-future shape, awaiting `client.accept`. The
//!   host applies the outcome via the pending-accept drain. The synchronous
//!   [`accept`](PickerSourceGenerator::accept) is a defensive tripwire — the
//!   host always prefers `accept_async` when it returns `Some`, so a plugin
//!   accept never blocks the actor thread (paramount #4).

use std::sync::Arc;

use lattice_completion::candidate::RawCandidate;
use lattice_picker::context::PickerContext;
use lattice_picker::outcome::PickerAcceptOutcome;
use lattice_picker::source::PickerSourceSpec;
use lattice_picker::{
    AcceptFuture, CandidateBatch, PickerInitResult, PickerSourceGenerator, RoutingPayload,
    SourceResult,
};

use crate::WitBoundary;
use crate::boundary_picker::project_picker_context;
use crate::picker_task::{CandidatePair, PickerContext as WitPickerContext};
use crate::{PickerClient, PluginHostError};

/// An `Arc<dyn PickerSourceGenerator>`-ready adapter over a picker plugin's
/// [`PickerClient`]. Cheap to clone (the client is an mpsc `Sender` clone + a
/// cached spec); every clone talks to the same actor.
pub struct WasmPickerSource {
    client: PickerClient,
    /// The native spec, converted once at [`connect`](Self::connect). Held so
    /// the trait's `spec(&self) -> &PickerSourceSpec` is a borrow.
    spec: PickerSourceSpec,
}

impl WasmPickerSource {
    /// Fetch the plugin's `spec` through the bridge, convert it to the native
    /// [`PickerSourceSpec`], and build the adapter. Async because the one-time
    /// spec fetch is a guest call; a malformed spec (or a dead actor) is a
    /// typed error, so a bad plugin fails registration loudly rather than
    /// registering a broken source.
    pub async fn connect(client: PickerClient) -> Result<Self, PluginHostError> {
        let wit_spec = client.spec().await?;
        let spec = PickerSourceSpec::from_wit(wit_spec).map_err(PluginHostError::Boundary)?;
        Ok(Self { client, spec })
    }

    /// The host-issued id of the plugin behind this source.
    pub fn plugin_id(&self) -> crate::PluginId {
        self.client.id()
    }

    /// Project the borrowed native context into its owned WIT mirror (§4.2) —
    /// the synchronous prelude both `init` and `accept_async` run before
    /// handing work to a `'static` future.
    fn project(ctx: &PickerContext<'_>) -> Result<WitPickerContext, String> {
        project_picker_context(ctx)
    }
}

/// Flatten the bridge's nested result into the trait's `SourceResult`: the
/// outer host error (trap / plugin-gone) and the inner guest WIT `err` both
/// collapse to the `String` error the picker echoes.
fn flatten<T>(call: Result<Result<T, String>, PluginHostError>) -> Result<T, String> {
    match call {
        Ok(inner) => inner,
        Err(host_err) => Err(format!("picker plugin: {host_err}")),
    }
}

impl PickerSourceGenerator for WasmPickerSource {
    fn spec(&self) -> &PickerSourceSpec {
        &self.spec
    }

    fn init(&self, ctx: &PickerContext<'_>, args: &[String]) -> SourceResult<PickerInitResult> {
        // Sync prelude: project the borrowed context now, then release the
        // borrow. Everything the future needs is owned + `'static`.
        let wit_ctx = Self::project(ctx)?;
        let args = args.to_vec();
        let client = self.client.clone();
        Ok(PickerInitResult::Future(Box::pin(async move {
            let pairs = flatten(client.init(wit_ctx, args).await)?;
            wit_pairs_to_batch(pairs)
        })))
    }

    fn accept(
        &self,
        _ctx: &PickerContext<'_>,
        _routing: &RoutingPayload,
    ) -> SourceResult<PickerAcceptOutcome> {
        // Defensive tripwire: the host always routes a WASM source's accept
        // through `accept_async` (which returns `Some`), so this is unreachable
        // in the wired path. Surfacing an error rather than a silent `NoOp`
        // makes any future host-wiring regression loud.
        Err("WasmPickerSource::accept must be resolved via accept_async".to_string())
    }

    fn accept_async(
        &self,
        ctx: &PickerContext<'_>,
        routing: &RoutingPayload,
    ) -> Option<AcceptFuture> {
        // Sync prelude: project the context + lower the routing token now. A
        // projection/lowering error is still surfaced *through* the future (as
        // `Some`) so it reaches the pending-accept drain rather than being
        // swallowed by the host's `None => sync accept` fallback.
        let prep = Self::project(ctx).and_then(|wit_ctx| Ok((wit_ctx, routing.to_wit()?)));
        let client = self.client.clone();
        Some(Box::pin(async move {
            let (wit_ctx, wit_routing) = prep?;
            let wit_outcome = flatten(client.accept(wit_ctx, wit_routing).await)?;
            PickerAcceptOutcome::from_wit(wit_outcome)
        }))
    }
}

/// Convert the guest's `list<candidate-pair>` into the native
/// [`CandidateBatch`]. A pair that fails to cross (malformed candidate, non-
/// UTF-8 path) fails the whole batch as a typed error — never a silent drop.
fn wit_pairs_to_batch(pairs: Vec<CandidatePair>) -> SourceResult<CandidateBatch> {
    let mut batch = CandidateBatch::with_capacity(pairs.len());
    for pair in pairs {
        let candidate = RawCandidate::from_wit(pair.candidate)?;
        let routing = RoutingPayload::from_wit(pair.routing)?;
        batch.push((candidate, routing));
    }
    Ok(batch)
}

/// Convenience: connect + wrap as the `Arc<dyn PickerSourceGenerator>` the
/// `PickerRegistry` stores. Registration itself is one call —
/// `registry.register_generator(source)` — with the source keyed by its
/// `spec().id`; provenance (`SourceLayer::Plugin`) is a grammar-contribution
/// concern (PH7.7), not a picker-registry one.
pub async fn connect_picker_source(
    client: PickerClient,
) -> Result<Arc<dyn PickerSourceGenerator>, PluginHostError> {
    Ok(Arc::new(WasmPickerSource::connect(client).await?))
}
