//! OM.A1 — the per-plugin actor bridge for scanned-excerpt-source providers.
//!
//! The agenda analogue of `media_task.rs`, and deliberately its near-twin: a
//! dedicated async task owns the plugin's `Store<PluginState>` for life (the
//! Store is `!Sync`), an [`AgendaCall`] crosses an mpsc channel with a
//! `oneshot` reply, and the `Send + Sync` [`AgendaClient`] serialises calls
//! onto the single-consumer loop.
//!
//! **The serialisation is load-bearing here, not incidental.** `begin` drops
//! per-scan state and every following `scan` reads it, so the two must not
//! interleave with a second scan's calls. One actor, one queue, in order —
//! which is also why the scan walks files sequentially rather than fanning
//! out across the pool.
//!
//! This is the fifth near-copy of the picker / completion / decoration / media
//! actor. The rule-of-three note in `completion_task` is now well past earned;
//! generalising over the bindings type is worth doing the next time one of
//! them changes shape, and this slice deliberately did not take that on
//! mid-seam.

use std::sync::Arc;

use futures::StreamExt;
use futures::channel::{mpsc, oneshot};
use lattice_runtime::EventBus;
use wasmtime::Store;

use crate::agenda_host::bindings::ScannedExcerptSourcePlugin;
use crate::{
    Component, PluginBudget, PluginHost, PluginHostError, PluginId, PluginManifest, PluginState,
    TrustTier, arm_store,
};

/// The WIT `entry`, re-exported so the adapter and the loader name one type.
pub use crate::agenda_host::bindings::lattice::plugin_host::scanned_excerpt_source::Entry;

/// OT.3: parse one scanned file, from text the host has already read.
///
/// `None` means "hand the guest the text instead" — an extension that resolves
/// to no registered language, a grammar that will not load, or a parse that
/// yields no tree. None of those is an error: `scanned-excerpt-source.wit` keeps a
/// source independent of the `language` seam, so a filetype with no grammar
/// must still be scannable.
///
/// `debug!`, never `info!` — this runs once per file of a project-wide walk,
/// and a tree of unparseable files is the ordinary case rather than a problem.
fn parse_for_scan(path: &str, text: &str) -> Option<Arc<lattice_syntax::SyntaxSnapshot>> {
    let lang = lattice_syntax::Lang::detect_from_path(Some(std::path::Path::new(path)));
    let mut syntax = match lattice_syntax::Syntax::for_language(lang) {
        Ok(Some(syntax)) => syntax,
        Ok(None) => {
            tracing::debug!(%path, ?lang, "agenda scan: no grammar; handing the guest text");
            return None;
        }
        Err(error) => {
            tracing::debug!(%path, ?lang, %error, "agenda scan: grammar load failed; handing the guest text");
            return None;
        }
    };
    syntax.parse(text);
    let snapshot = Arc::new(syntax.snapshot_owned());
    if snapshot.tree().is_none() {
        tracing::debug!(%path, ?lang, "agenda scan: parsed to no tree; handing the guest text");
        return None;
    }
    Some(snapshot)
}

type CallResult<T> = Result<T, PluginHostError>;

enum AgendaCall {
    /// `extensions()` — the file extensions this source wants offered.
    Extensions {
        reply: oneshot::Sender<CallResult<Vec<String>>>,
    },
    /// `view-mode()` — the minor this source wants on the agenda view.
    ViewMode {
        reply: oneshot::Sender<CallResult<Option<String>>>,
    },
    /// AF.1: `roots()` — the paths this source wants scanned.
    Roots {
        reply: oneshot::Sender<CallResult<Vec<String>>>,
    },
    /// `begin()` — drop per-scan state, and return the generation key (OT.3b).
    Begin {
        reply: oneshot::Sender<CallResult<u64>>,
    },
    /// `scan(path, text)` — one file's agenda rows.
    Scan {
        path: String,
        text: String,
        reply: oneshot::Sender<CallResult<Result<Vec<Entry>, String>>>,
    },
}

/// The `Send + Sync` handle a caller holds. Cloning is cheap; every clone
/// talks to the same actor / `Store`, so calls serialise on the
/// single-consumer loop the `!Sync` `Store` requires — which is exactly what
/// `begin`-then-`scan` needs.
#[derive(Clone, Debug)]
pub struct AgendaClient {
    tx: mpsc::UnboundedSender<AgendaCall>,
    id: PluginId,
}

impl AgendaClient {
    pub fn id(&self) -> PluginId {
        self.id
    }

    /// Call the guest's `extensions()`. Once, at load.
    pub async fn extensions(&self) -> CallResult<Vec<String>> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .unbounded_send(AgendaCall::Extensions { reply })
            .map_err(|_| PluginHostError::PluginGone { func: "extensions" })?;
        rx.await
            .map_err(|_| PluginHostError::PluginGone { func: "extensions" })?
    }

    /// Call the guest's `view-mode()`. Once, at load.
    pub async fn view_mode(&self) -> CallResult<Option<String>> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .unbounded_send(AgendaCall::ViewMode { reply })
            .map_err(|_| PluginHostError::PluginGone { func: "view-mode" })?;
        rx.await
            .map_err(|_| PluginHostError::PluginGone { func: "view-mode" })?
    }

    /// AF.1: call the guest's `roots()`. Per scan, not once at load — the
    /// answer comes from user configuration and must follow a `:set`.
    pub async fn roots(&self) -> CallResult<Vec<String>> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .unbounded_send(AgendaCall::Roots { reply })
            .map_err(|_| PluginHostError::PluginGone { func: "roots" })?;
        rx.await
            .map_err(|_| PluginHostError::PluginGone { func: "roots" })?
    }

    /// Call the guest's `begin()` — the start of a scan. Returns the guest's
    /// generation key (OT.3b): an opaque `u64` that changes when anything
    /// scan-wide would change its rows, so the host can invalidate cached
    /// results without knowing what those things are.
    pub async fn begin(&self) -> CallResult<u64> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .unbounded_send(AgendaCall::Begin { reply })
            .map_err(|_| PluginHostError::PluginGone { func: "begin" })?;
        rx.await
            .map_err(|_| PluginHostError::PluginGone { func: "begin" })?
    }

    /// Call the guest's `scan(path, text)`.
    ///
    /// The outer result is the host surface (trap / gone / quarantined); the
    /// inner `Result<_, String>` is the guest's own WIT `result`. Either way
    /// the caller skips THIS FILE and continues — one bad file must not fail
    /// the agenda.
    pub async fn scan(&self, path: String, text: String) -> CallResult<Result<Vec<Entry>, String>> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .unbounded_send(AgendaCall::Scan { path, text, reply })
            .map_err(|_| PluginHostError::PluginGone { func: "scan" })?;
        rx.await
            .map_err(|_| PluginHostError::PluginGone { func: "scan" })?
    }
}

/// The per-plugin actor: owns the `Store` + agenda bindings for the plugin's
/// life and serves calls off the channel until every [`AgendaClient`] drops.
pub struct AgendaActor {
    store: Store<PluginState>,
    bindings: ScannedExcerptSourcePlugin,
    budget: PluginBudget,
    rx: mpsc::UnboundedReceiver<AgendaCall>,
    id: PluginId,
    /// Crash-quarantine: the first trap trips this, fires one `PluginCrashed`,
    /// and every later call returns `Quarantined`. A trap mid-scan therefore
    /// leaves the agenda showing what it collected rather than emptying it —
    /// partial-and-honest beats empty-and-silent (`org-mode.md` §8).
    quarantine: crate::Quarantine,
    tracer: Option<crate::trace::PluginTracerHandle>,
}

impl AgendaActor {
    pub fn id(&self) -> PluginId {
        self.id
    }

    pub fn with_tracer(mut self, tracer: Option<crate::trace::PluginTracerHandle>) -> Self {
        self.tracer = tracer;
        self
    }

    pub async fn run(mut self) {
        while let Some(call) = self.rx.next().await {
            match call {
                AgendaCall::Extensions { reply } => {
                    let _ = reply.send(self.call_extensions().await);
                }
                AgendaCall::ViewMode { reply } => {
                    let _ = reply.send(self.call_view_mode().await);
                }
                AgendaCall::Roots { reply } => {
                    let _ = reply.send(self.call_roots().await);
                }
                AgendaCall::Begin { reply } => {
                    let _ = reply.send(self.call_begin().await);
                }
                AgendaCall::Scan { path, text, reply } => {
                    let _ = reply.send(self.call_scan(&path, &text).await);
                }
            }
        }
    }

    async fn call_extensions(&mut self) -> CallResult<Vec<String>> {
        if self.quarantine.is_tripped() {
            return Err(PluginHostError::Quarantined { func: "extensions" });
        }
        arm_store(&mut self.store, self.budget)?;
        let start = std::time::Instant::now();
        let result = self.bindings.call_extensions(&mut self.store).await;
        crate::trip_and_map_traced(
            self.tracer.as_ref(),
            self.id.0,
            crate::PluginSeam::ScannedExcerptSource,
            &mut self.quarantine,
            "extensions",
            start,
            result,
        )
    }

    async fn call_view_mode(&mut self) -> CallResult<Option<String>> {
        if self.quarantine.is_tripped() {
            return Err(PluginHostError::Quarantined { func: "view-mode" });
        }
        arm_store(&mut self.store, self.budget)?;
        let start = std::time::Instant::now();
        let result = self.bindings.call_view_mode(&mut self.store).await;
        crate::trip_and_map_traced(
            self.tracer.as_ref(),
            self.id.0,
            crate::PluginSeam::ScannedExcerptSource,
            &mut self.quarantine,
            "view-mode",
            start,
            result,
        )
    }

    async fn call_roots(&mut self) -> CallResult<Vec<String>> {
        if self.quarantine.is_tripped() {
            return Err(PluginHostError::Quarantined { func: "roots" });
        }
        arm_store(&mut self.store, self.budget)?;
        let start = std::time::Instant::now();
        let result = self.bindings.call_roots(&mut self.store).await;
        crate::trip_and_map_traced(
            self.tracer.as_ref(),
            self.id.0,
            crate::PluginSeam::ScannedExcerptSource,
            &mut self.quarantine,
            "roots",
            start,
            result,
        )
    }

    async fn call_begin(&mut self) -> CallResult<u64> {
        if self.quarantine.is_tripped() {
            return Err(PluginHostError::Quarantined { func: "begin" });
        }
        arm_store(&mut self.store, self.budget)?;
        let start = std::time::Instant::now();
        let result = self.bindings.call_begin(&mut self.store).await;
        crate::trip_and_map_traced(
            self.tracer.as_ref(),
            self.id.0,
            crate::PluginSeam::ScannedExcerptSource,
            &mut self.quarantine,
            "begin",
            start,
            result,
        )
    }

    /// **Fuel is re-armed per call**, and a scan calls this once per file.
    /// Arming once at instantiate — correct for a declare-once seam — would
    /// make the agenda work for the first stretch of a large project and then
    /// silently contribute nothing for the rest, which is precisely the cliff
    /// `WasmErrorParser::rearm` documents. `arm_store` above is that re-arm;
    /// it is not boilerplate.
    async fn call_scan(
        &mut self,
        path: &str,
        text: &str,
    ) -> CallResult<Result<Vec<Entry>, String>> {
        if self.quarantine.is_tripped() {
            return Err(PluginHostError::Quarantined { func: "scan" });
        }
        arm_store(&mut self.store, self.budget)?;
        let start = std::time::Instant::now();
        // OT.3: parse here, from the text the host already read, and lend the
        // guest a borrow ALONGSIDE that text. Not instead of it — the copy is
        // 217 ns (`benches/agenda_scan_input.rs`) while the parse buying the
        // tree is 1-2 ms, and the seam exposes no node TEXT, so a guest without
        // the string would need a crossing per headline to read a TODO keyword.
        // Structure from the tree, characters from the text.
        //
        // NOT `tree-sitter.parse-file`: that is `fs:read` gated, and this is the
        // one seam whose guest holds no capability at all (`scanned-excerpt-source.wit`
        // — "no preopens, no `walk`"). The host reads; the guest is handed the
        // result.
        //
        // `None` when the extension resolves to no language or the parse yields
        // no tree; the guest then scans text alone, as it always did — a source
        // is independent of the `language` seam, so a filetype with no grammar
        // must still scan.
        let snapshot = parse_for_scan(path, text);
        let owned_tree = match &snapshot {
            Some(snap) => Some(
                self.store
                    .data_mut()
                    .table
                    .push(crate::tree_resource::TreeSnapshotResource::new(
                        snap.clone(),
                    ))
                    .map_err(|e| PluginHostError::Linker(e.into()))?,
            ),
            None => None,
        };
        let tree_borrow = owned_tree
            .as_ref()
            .map(|owned| wasmtime::component::Resource::new_borrow(owned.rep()));
        let result = self
            .bindings
            .call_scan(&mut self.store, path, text, tree_borrow)
            .await;
        // Reclaim the lent entry — the host owns it throughout (the
        // `apply-action` pattern), and a scan leaking one per file would grow
        // the resource table for the length of a project walk.
        if let Some(owned) = owned_tree {
            let _ = self.store.data_mut().table.delete(owned);
        }
        crate::trip_and_map_traced(
            self.tracer.as_ref(),
            self.id.0,
            crate::PluginSeam::ScannedExcerptSource,
            &mut self.quarantine,
            "scan",
            start,
            result,
        )
    }
}

impl PluginHost {
    /// Instantiate an `scanned-excerpt-source-plugin` component under its capability
    /// grant and return the bridge. Grant / data-dir / WASI are identical to
    /// every other seam (shared `build_plugin_wasi` + `new_store`), and the
    /// actor is NOT spawned here — the lib owns no runtime.
    pub async fn spawn_agenda_source(
        &self,
        component: &Component,
        manifest: &PluginManifest,
        tier: TrustTier,
        budget: PluginBudget,
        bus: &Arc<EventBus>,
        // AF.3: the editor's option registry, so `config.get-option` answers
        // inside a `roots` / `begin` / `scan` call.
        config: Option<&Arc<lattice_config::ConfigRegistry>>,
    ) -> Result<(AgendaClient, AgendaActor), PluginHostError> {
        let (wasi, outcome, _data_dir) = self.build_plugin_wasi(manifest, tier);
        for denied in &outcome.denied {
            tracing::warn!(
                plugin = %manifest.id,
                capability = ?denied,
                "agenda plugin loaded with a withheld capability (reduced function)"
            );
        }
        let mut store = self.new_store(wasi, outcome.grant, budget, Some(&manifest.id))?;
        let bindings =
            ScannedExcerptSourcePlugin::instantiate_async(&mut store, component, &self.linker)
                .await
                .map_err(|e| PluginHostError::Instantiate(e.into()))?;
        let id = self.alloc_id();
        store.data_mut().log_ctx = self.log_ctx_for(id);
        // AF.3: without this, `config.get-option` answers `none` in every
        // agenda call and the guest silently falls back to its compiled
        // defaults — org's `agenda-files` would read as unset however the user
        // set it, and its TODO keywords would ignore `org.todo-keywords`
        // entirely.
        //
        // `context`, `event`, `transient` and `grammar` all wire this; the
        // agenda store did not, and 73842466 fixed exactly this omission for
        // the events store. It survived here because every agenda test drove
        // `extensions` / `begin` / `scan`, none of which read an option until
        // `roots` did — a seam covered end to end with its config path never
        // called once.
        if let Some(registry) = config {
            store.data_mut().config_registry = Some(Arc::clone(registry));
        }
        let (tx, rx) = mpsc::unbounded();
        let client = AgendaClient { tx, id };
        let actor = AgendaActor {
            store,
            bindings,
            budget,
            rx,
            id,
            quarantine: crate::Quarantine::new(id, Arc::clone(bus)),
            tracer: None,
        };
        Ok((client, actor))
    }
}
