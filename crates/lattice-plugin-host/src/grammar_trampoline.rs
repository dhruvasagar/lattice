//! The sync grammar trampoline + registry wiring (plugin-host.md §4.1, PH7.7c).
//!
//! This is the production counterpart to the picker/completion actor adapters —
//! but **synchronous**, per the PH7.7 fork: a plugin motion/operator/text-object
//! must resolve inline on the dispatch thread to compose with its operator (async
//! would break operator∘motion atomicity + dot-repeat/macros). So there is no
//! actor task; the native spec's `apply` closure calls the guest export *directly*
//! under a lock, bounded by a Reflex-class fuel/epoch budget
//! ([`PluginBudget::grammar`]).
//!
//! Flow ([`PluginHost::instantiate_grammar_plugin`]):
//!   1. instantiate the `grammar-plugin` component against the **sync** grammar
//!      linker (`lib.rs`: sync WASI + the `grammar` register import),
//!   2. call the guest's `register-grammar` export (sync) — the guest calls the
//!      imported `register-*` host funcs, which record into
//!      [`GrammarContributions`](crate::grammar_host::GrammarContributions),
//!   3. drain the recorded contributions and build a native `*Spec` for each,
//!      whose `apply` / `parse_args` is a **trampoline** closure over the shared
//!      `Arc<Mutex<GrammarGuest>>` + the guest-chosen callback id,
//!   4. return a [`GrammarContributionSet`] the *caller* registers into its
//!      `CommandRegistry` via `register_plugin_*` (mode-ownership — the host
//!      builds specs, the caller owns the registry; ZERO `Editor::` methods).
//!
//! Graceful degradation (§8): a guest `err`, a fuel/epoch **trap** (the Reflex
//! runaway guard), a boundary-conversion failure, or a poisoned lock all map to
//! [`CommandError::Plugin`] — the dispatcher commits no effect, the contribution
//! is a no-op, the reason is logged. Never a panic, never a keystroke hang.

use std::sync::{Arc, Mutex};

use wasmtime::Store;
use wasmtime::component::Resource;

use lattice_runtime::snapshot::DocumentSnapshot;
use lattice_syntax::SyntaxSnapshot;

use crate::buffer::DocumentResource;
use crate::tree_resource::TreeSnapshotResource;

use lattice_grammar::args::{ArgSpec as NativeArgSpec, Args as NativeArgs};
use lattice_grammar::command::LatencyClass;
use lattice_grammar::effect::Effect as NativeEffect;
use lattice_grammar::error::{CommandError, GrammarResult};
use lattice_grammar::registry::{
    ActionContext, ActionSpec, CommandRegistry, ExCommandContext, ExCommandSpec, MotionContext,
    MotionResult, MotionSpec, OperatorContext, OperatorSpec, SurfaceForm, TextObjectContext,
    TextObjectSpec,
};
use lattice_protocol::position::Range as NativeRange;

use crate::boundary_grammar::{
    project_action_context, project_ex_command_context, project_motion_context,
    project_operator_context, project_text_object_context,
};
use crate::grammar_host::RecordedContribution;
use crate::grammar_host::bindings::GrammarPlugin;
use crate::lattice::plugin_host::types::{
    ActionSpec as WitActionSpec, ArgSpec as WitArgSpec, ExCommandSpec as WitExCommandSpec,
    MotionSpec as WitMotionSpec, OperatorSpec as WitOperatorSpec,
    TextObjectSpec as WitTextObjectSpec,
};
use crate::trace::{
    Direction, HotGate, PluginTraceRecord, PluginTracerHandle, TraceLevel, TraceOutcome,
};
use crate::{
    Component, PluginBudget, PluginHost, PluginHostError, PluginId, PluginManifest, PluginState,
    Quarantine, TrustTier, WitBoundary, arm_store, classify_trap,
};
use lattice_runtime::EventBus;

/// The plugin's `Store` + grammar bindings, shared by every contribution's
/// trampoline closure. Behind an `Arc<Mutex<>>` so the `Send + Sync` grammar
/// `apply` closures can reach the `!Sync` `Store`; the `Mutex` serializes calls
/// onto the single-threaded store (one plugin's motions never run concurrently,
/// which the store requires anyway).
struct GrammarGuest {
    store: Store<PluginState>,
    bindings: GrammarPlugin,
    /// XF.4: the `fs:write` gate for effects this plugin returns.
    ///
    /// Built once at load from `store.data().grant` — a grant never changes
    /// for a plugin's life, so the keystroke path pays only the prefix compare
    /// a `WriteToFile` actually needs, and nothing at all for every other
    /// effect. `Arc` so each trampoline closure captures a pointer rather than
    /// a copy of the prefix list.
    authorizer: Arc<crate::EffectAuthorizer>,
    /// Crash-quarantine (PH7.12) shared by every trampoline closure (they all
    /// lock this one guest). The first `apply-*` trap trips it — one
    /// `PluginCrashed` on the bus — and every later keystroke short-circuits to a
    /// no-op instead of re-trapping the dead `Store` at held-key frequency.
    quarantine: Quarantine,
    /// This plugin's host-issued id — the key on every emitted trace record.
    plugin: u32,
    /// PO.3 hot-path gate: the published per-plugin verbosity atomic. Read once
    /// per guest call with a relaxed load (design §4); at the default `Info` gate
    /// a successful call emits nothing (zero timing / alloc / format).
    gate: HotGate,
    /// The boundary tracer, when wired at load time. `None` in tests / benches
    /// and when the loader has no tracer — the whole trampoline then costs one
    /// `records_calls()` load + a not-taken branch per call.
    tracer: Option<PluginTracerHandle>,
}

/// Emit one grammar boundary-trace record into the wired tracer. Off the hot
/// path: only called after [`HotGate::records_calls`] admitted a success, or on
/// the cold guest-err / trap branches. The tracer re-gates internally, so a
/// racing verbosity drop still drops the record.
#[inline]
fn emit_trace(
    tracer: &PluginTracerHandle,
    plugin: u32,
    func: &'static str,
    level: TraceLevel,
    outcome: TraceOutcome,
) {
    tracer.trace(PluginTraceRecord {
        plugin,
        seam: crate::PluginSeam::Grammar,
        direction: Direction::GuestExport,
        call: std::borrow::Cow::Borrowed(func),
        level,
        outcome,
        detail: None,
    });
}

/// Lock the guest, arm the Reflex-class grammar budget, and run one synchronous
/// callback. Maps every failure mode to [`CommandError::Plugin`] (graceful §8): a
/// wasmtime trap (fuel/epoch runaway guard, or any guest panic) → a logged
/// no-op; a guest-returned WIT `err` → the guest's message; a poisoned lock → a
/// no-op. `T` is the callback's `ok` payload (already the WIT type).
fn run_callback<T>(
    guest: &Arc<Mutex<GrammarGuest>>,
    func: &'static str,
    call: impl FnOnce(&GrammarPlugin, &mut Store<PluginState>) -> wasmtime::Result<Result<T, String>>,
) -> GrammarResult<T> {
    let mut guard = guest
        .lock()
        .map_err(|_| CommandError::Plugin(format!("{func}: plugin lock poisoned")))?;
    // Quarantine short-circuit (PH7.12): a prior `apply-*` trap tainted this
    // instance's `Store`, so every later motion/operator is a clean no-op — the
    // `PluginCrashed` event already fired at trip time. This is what stops a
    // trapping motion from re-failing at held-key (30 Hz) frequency.
    if guard.quarantine.is_tripped() {
        return Err(CommandError::Plugin(format!("{func}: plugin quarantined")));
    }
    let GrammarGuest {
        store,
        bindings,
        quarantine,
        plugin,
        gate,
        tracer,
        // XF.4: read at build time by each trampoline closure, not here.
        authorizer: _,
    } = &mut *guard;
    // Reflex-class budget (audit F1): a runaway plugin motion traps well inside a
    // frame instead of stalling the keystroke.
    arm_store(store, PluginBudget::grammar())
        .map_err(|e| CommandError::Plugin(format!("{func}: arm store: {e}")))?;

    // Design §4 hot-path contract — a single relaxed-atomic gate load + a
    // predicted-not-taken branch. At the default `Info` gate `record_calls` is
    // false, so a *successful* call below does ZERO timing / allocation /
    // formatting: the trampoline's only added cost is this load and branch.
    let record_calls = gate.records_calls();
    let start = record_calls.then(std::time::Instant::now);

    match call(bindings, store) {
        Ok(Ok(value)) => {
            if let (Some(start), Some(tracer)) = (start, tracer.as_ref()) {
                emit_trace(
                    tracer,
                    *plugin,
                    func,
                    TraceLevel::Debug,
                    TraceOutcome::Ok {
                        micros: start.elapsed().as_micros() as u64,
                        fuel_delta: 0,
                    },
                );
            }
            Ok(value)
        }
        Ok(Err(guest_err)) => {
            // A guest-signalled `err` — rare and user-actionable. Recorded at
            // `Warn` so it is KEPT at the default `Info` gate, off the common
            // keystroke path. The sync trampoline sees the guest's inner
            // `Result::Err` directly; the async seams cannot (they observe only
            // the outer `wasmtime::Result`, so a guest err crosses as a nominal
            // `Ok` there) — this seam is deliberately richer, not a mirror.
            if let Some(tracer) = tracer.as_ref() {
                emit_trace(
                    tracer,
                    *plugin,
                    func,
                    TraceLevel::Warn,
                    TraceOutcome::Ok {
                        micros: start.map_or(0, |s| s.elapsed().as_micros() as u64),
                        fuel_delta: 0,
                    },
                );
            }
            Err(CommandError::Plugin(format!("{func}: {guest_err}")))
        }
        Err(trap) => {
            // The trap taints the instance irrecoverably: trip quarantine so the
            // crash fires once and later keystrokes short-circuit above.
            let kind = classify_trap(&trap);
            quarantine.trip(func, kind);
            // Always recorded (Error) — the lifecycle/crash signal the default
            // gate carries per §4. Cold path (a trap trips quarantine once).
            if let Some(tracer) = tracer.as_ref() {
                emit_trace(
                    tracer,
                    *plugin,
                    func,
                    TraceLevel::Error,
                    TraceOutcome::Trap {
                        kind: kind.label().to_string(),
                        func: func.to_string(),
                    },
                );
            }
            Err(CommandError::Plugin(format!(
                "{func} trapped ({kind}): {trap}"
            )))
        }
    }
}

/// Convert a WIT `arg-spec` list into native `ArgSpec`s (reusing the PH7.4a
/// `ArgSpec` mirror). A conversion failure fails the whole registration (the
/// spec is malformed), surfaced to the caller of `instantiate_grammar_plugin`.
fn convert_args_schema(schema: Vec<WitArgSpec>) -> Result<Vec<NativeArgSpec>, PluginHostError> {
    schema
        .into_iter()
        .map(|a| NativeArgSpec::from_wit(a).map_err(PluginHostError::GrammarSpec))
        .collect()
}

/// TS.1 / OT.1: resolve the `tree-snapshot` a grammar callback should see.
///
/// Three seams mint this identically — action (TS.1), motion and text object
/// (OT.1) — so the gate lives in one place rather than being re-derived per
/// seam, where one copy could silently drop a condition. All three are real:
///
/// * **the capability gate** — no `tree-sitter` grant means `none` even on a
///   parsed buffer (design §5, the read-only structural seam is gated);
/// * **the downcast** — `lattice-grammar` type-erases the snapshot as
///   `Arc<dyn Any>` to keep its lean dep set, so the concrete type is recovered
///   here and a foreign payload yields `none` rather than a panic;
/// * **the parse check** — a `SyntaxSnapshot` can exist with no tree behind it
///   (plain text / parse pending), and handing the guest a treeless snapshot
///   would make `root()` answer nothing while `none` says so honestly.
///
/// Takes a borrow so the motion and text-object contexts (which hold
/// `Option<&Arc<…>>` to stay free on the keystroke path) pay the `Arc` bump only
/// on the branch that actually mints a resource.
fn resolve_tree_snapshot(
    tree_sitter_granted: bool,
    syntax: Option<&Arc<dyn std::any::Any + Send + Sync>>,
) -> Option<Arc<SyntaxSnapshot>> {
    tree_sitter_granted
        .then_some(syntax)
        .flatten()
        .and_then(|any| any.clone().downcast::<SyntaxSnapshot>().ok())
        .filter(|snap| snap.tree().is_some())
}

fn build_motion_spec(
    guest: &Arc<Mutex<GrammarGuest>>,
    spec: WitMotionSpec,
    callback: u32,
    // OT.1: same gate the action seam applies, for the same reason.
    tree_sitter_granted: bool,
) -> Result<MotionSpec, PluginHostError> {
    let args_schema = convert_args_schema(spec.args_schema)?;
    let guest = guest.clone();
    Ok(MotionSpec {
        jump: spec.jump,
        exclusive: spec.exclusive,
        args_schema,
        apply: Arc::new(move |ctx: &MotionContext| -> GrammarResult<MotionResult> {
            let wit_ctx = project_motion_context(ctx).map_err(CommandError::Plugin)?;
            // OM.4: mint a point-in-time `document` from the motion's buffer,
            // exactly as `build_action_spec` does (O(1) rope clone). A motion
            // that reads text — org's headline motions are the first — needs
            // the same handle an action gets.
            let snapshot = Arc::new(DocumentSnapshot {
                buffer: ctx.buffer.clone(),
                // OM.6b: so `document.path()` answers on this seam too. A
                // guest that gets `none` here because the mint dropped the
                // field would read it as "unsaved buffer" and be wrong.
                path: ctx.path.map(|p| Arc::new(p.to_path_buf())),
                ..Default::default()
            });
            // OT.1: and the tree, on the action seam's terms. `ctx.syntax` is
            // already a borrow, so an ungranted or unparsed buffer costs a
            // branch and no `Arc` traffic — which matters here and not on the
            // action seam, because motions fire on every `j`.
            let tree_snapshot = resolve_tree_snapshot(tree_sitter_granted, ctx.syntax);
            let wit = run_callback(&guest, "apply-motion", |b, s| {
                // Lend as borrows and reclaim after the call — the host owns
                // the entries throughout (the `apply-action` pattern).
                let owned_doc = s
                    .data_mut()
                    .table
                    .push(DocumentResource::new(snapshot.clone()))?;
                let doc_borrow = Resource::new_borrow(owned_doc.rep());
                let owned_tree = match &tree_snapshot {
                    Some(snap) => Some(
                        s.data_mut()
                            .table
                            .push(TreeSnapshotResource::new(snap.clone()))?,
                    ),
                    None => None,
                };
                let tree_borrow = owned_tree.as_ref().map(|o| Resource::new_borrow(o.rep()));
                let result = b.lattice_plugin_host_grammar_callbacks().call_apply_motion(
                    &mut *s,
                    callback,
                    &wit_ctx,
                    doc_borrow,
                    tree_borrow,
                );
                let _ = s.data_mut().table.delete(owned_doc);
                if let Some(owned_tree) = owned_tree {
                    let _ = s.data_mut().table.delete(owned_tree);
                }
                result
            })?;
            MotionResult::from_wit(wit).map_err(CommandError::Plugin)
        }),
    })
}

/// XF.4: convert a guest-returned effect and AUTHORISE it.
///
/// The one place both halves happen, so a new effect-returning contribution
/// cannot get the conversion and forget the gate — which would be an
/// unchecked cross-file write reachable from any plugin.
fn effect_from_guest(
    authorizer: &Arc<crate::EffectAuthorizer>,
    wit: Vec<crate::lattice::plugin_host::types::Effect>,
) -> GrammarResult<NativeEffect> {
    let native = NativeEffect::from_wit(wit).map_err(CommandError::Plugin)?;
    Ok(authorizer.authorize(native))
}

fn build_operator_spec(
    guest: &Arc<Mutex<GrammarGuest>>,
    spec: WitOperatorSpec,
    callback: u32,
) -> Result<OperatorSpec, PluginHostError> {
    let args_schema = convert_args_schema(spec.args_schema)?;
    let authorizer = Arc::clone(
        &guest
            .lock()
            .expect("grammar guest mutex poisoned")
            .authorizer,
    );
    let guest = guest.clone();
    Ok(OperatorSpec {
        repeatable: spec.repeatable,
        args_schema,
        blockwise_per_row: spec.blockwise_per_row,
        post_motion_char: spec.post_motion_char,
        apply: Arc::new(
            move |ctx: &mut OperatorContext| -> GrammarResult<NativeEffect> {
                let wit_ctx = project_operator_context(ctx).map_err(CommandError::Plugin)?;
                let wit = run_callback(&guest, "apply-operator", |b, s| {
                    b.lattice_plugin_host_grammar_callbacks()
                        .call_apply_operator(s, callback, &wit_ctx)
                })?;
                effect_from_guest(&authorizer, wit)
            },
        ),
    })
}

fn build_text_object_spec(
    guest: &Arc<Mutex<GrammarGuest>>,
    spec: WitTextObjectSpec,
    callback: u32,
    // OT.1: same gate the action and motion seams apply.
    tree_sitter_granted: bool,
) -> Result<TextObjectSpec, PluginHostError> {
    let args_schema = convert_args_schema(spec.args_schema)?;
    let guest = guest.clone();
    Ok(TextObjectSpec {
        args_schema,
        apply: Arc::new(
            move |ctx: &TextObjectContext| -> GrammarResult<NativeRange> {
                let wit_ctx = project_text_object_context(ctx).map_err(CommandError::Plugin)?;
                // OM.4b: mint a point-in-time `document`, as the motion and
                // action paths do. Resolving a subtree's bounds means reading
                // lines, and `text-object-context` always said buffer text
                // rides this handle.
                let snapshot = Arc::new(DocumentSnapshot {
                    buffer: ctx.buffer.clone(),
                    // OM.6b, as on the motion and action seams.
                    path: ctx.path.map(|p| Arc::new(p.to_path_buf())),
                    ..Default::default()
                });
                // OT.1: and the tree — org's `ir` / `ar` resolve a subtree,
                // which is the `(section)` node rather than a star count.
                let tree_snapshot = resolve_tree_snapshot(tree_sitter_granted, ctx.syntax);
                let wit = run_callback(&guest, "apply-text-object", |b, s| {
                    let owned_doc = s
                        .data_mut()
                        .table
                        .push(DocumentResource::new(snapshot.clone()))?;
                    let doc_borrow = Resource::new_borrow(owned_doc.rep());
                    let owned_tree = match &tree_snapshot {
                        Some(snap) => Some(
                            s.data_mut()
                                .table
                                .push(TreeSnapshotResource::new(snap.clone()))?,
                        ),
                        None => None,
                    };
                    let tree_borrow = owned_tree.as_ref().map(|o| Resource::new_borrow(o.rep()));
                    let result = b
                        .lattice_plugin_host_grammar_callbacks()
                        .call_apply_text_object(
                            &mut *s,
                            callback,
                            &wit_ctx,
                            doc_borrow,
                            tree_borrow,
                        );
                    let _ = s.data_mut().table.delete(owned_doc);
                    if let Some(owned_tree) = owned_tree {
                        let _ = s.data_mut().table.delete(owned_tree);
                    }
                    result
                })?;
                NativeRange::from_wit(wit).map_err(CommandError::Plugin)
            },
        ),
    })
}

fn build_action_spec(
    guest: &Arc<Mutex<GrammarGuest>>,
    spec: WitActionSpec,
    callback: u32,
    // TS.1: whether this plugin was granted the `tree-sitter` editor-capability.
    // When false the guest gets `none` for the tree even on a parsed buffer —
    // the read-only structural seam is capability-gated (design §5).
    tree_sitter_granted: bool,
) -> Result<ActionSpec, PluginHostError> {
    let args_schema = convert_args_schema(spec.args_schema)?;
    let authorizer = Arc::clone(
        &guest
            .lock()
            .expect("grammar guest mutex poisoned")
            .authorizer,
    );
    let guest = guest.clone();
    Ok(ActionSpec {
        args_schema,
        apply: Arc::new(move |ctx: &ActionContext| -> GrammarResult<NativeEffect> {
            let wit_ctx = project_action_context(ctx).map_err(CommandError::Plugin)?;
            // AP.0.1: mint a point-in-time `document` from the action's buffer
            // (O(1) rope clone) so the guest can read text around the cursor.
            // OM.6b added `path` to the mint — `document.path()` is what
            // `org-archive-subtree` asks to name `<file>_archive`. The rest of
            // the snapshot's fields are still irrelevant here.
            let snapshot = Arc::new(DocumentSnapshot {
                buffer: ctx.buffer.clone(),
                path: ctx.path.clone(),
                ..Default::default()
            });
            // TS.1: mint a `tree-snapshot` handle only when the grant, the
            // downcast and the parse all hold — see `resolve_tree_snapshot`,
            // shared with the motion and text-object seams since OT.1. The
            // snapshot was acquired the same instant as `buffer` above, so the
            // tree + text handles agree on version (§7).
            let tree_snapshot = resolve_tree_snapshot(tree_sitter_granted, ctx.syntax.as_ref());
            let wit = run_callback(&guest, "apply-action", |b, s| {
                // Lend the resources as borrows: push owned entries, pass
                // non-owning borrow handles to the guest, then reclaim the owned
                // entries after the call (the host owns them throughout). Any
                // `node` the guest derives from the tree borrow is guest-owned and
                // dropped by the guest before it returns.
                let owned_doc = s
                    .data_mut()
                    .table
                    .push(DocumentResource::new(snapshot.clone()))?;
                let doc_borrow = Resource::new_borrow(owned_doc.rep());
                let owned_tree = match &tree_snapshot {
                    Some(snap) => Some(
                        s.data_mut()
                            .table
                            .push(TreeSnapshotResource::new(snap.clone()))?,
                    ),
                    None => None,
                };
                let tree_borrow = owned_tree.as_ref().map(|o| Resource::new_borrow(o.rep()));
                // Reborrow `s` for the call so the owned handles can be reclaimed
                // after (the call takes the store by value via `AsContextMut`).
                let result = b.lattice_plugin_host_grammar_callbacks().call_apply_action(
                    &mut *s,
                    callback,
                    &wit_ctx,
                    doc_borrow,
                    tree_borrow,
                );
                let _ = s.data_mut().table.delete(owned_doc);
                if let Some(owned_tree) = owned_tree {
                    let _ = s.data_mut().table.delete(owned_tree);
                }
                result
            })?;
            effect_from_guest(&authorizer, wit)
        }),
    })
}

fn build_ex_command_spec(
    guest: &Arc<Mutex<GrammarGuest>>,
    spec: WitExCommandSpec,
    parse_callback: u32,
    apply_callback: u32,
) -> Result<ExCommandSpec, PluginHostError> {
    let args_schema = convert_args_schema(spec.args_schema)?;
    let latency_class =
        LatencyClass::from_wit(spec.latency_class).map_err(PluginHostError::GrammarSpec)?;
    let surface_form =
        SurfaceForm::from_wit(spec.surface_form).map_err(PluginHostError::GrammarSpec)?;
    let authorizer = Arc::clone(
        &guest
            .lock()
            .expect("grammar guest mutex poisoned")
            .authorizer,
    );
    let parse_guest = guest.clone();
    let apply_guest = guest.clone();
    Ok(ExCommandSpec {
        latency_class,
        accepts_bang: spec.accepts_bang,
        accepts_range: spec.accepts_range,
        args_schema,
        surface_form,
        parse_args: Arc::new(move |rest: &str, bang: bool| -> GrammarResult<NativeArgs> {
            let rest = rest.to_string();
            let wit = run_callback(&parse_guest, "parse-ex-args", |b, s| {
                b.lattice_plugin_host_grammar_callbacks()
                    .call_parse_ex_args(s, parse_callback, &rest, bang)
            })?;
            NativeArgs::from_wit(wit).map_err(CommandError::Plugin)
        }),
        apply: Arc::new(
            move |ctx: &ExCommandContext| -> GrammarResult<NativeEffect> {
                let wit_ctx = project_ex_command_context(ctx).map_err(CommandError::Plugin)?;
                let wit = run_callback(&apply_guest, "apply-ex-command", |b, s| {
                    b.lattice_plugin_host_grammar_callbacks()
                        .call_apply_ex_command(s, apply_callback, &wit_ctx)
                })?;
                effect_from_guest(&authorizer, wit)
            },
        ),
    })
}

/// The native grammar contributions a plugin declared, each with a trampoline
/// `apply` into the guest, ready for the **caller** to register into its
/// `CommandRegistry`. The host builds the specs (owning the trampoline over the
/// guest store) but never owns the registry — [`register_all`](Self::register_all)
/// takes the caller's `&mut CommandRegistry` and stamps every entry
/// `SourceLayer::Plugin(plugin_id)` via the `register_plugin_*` seam
/// (mode-ownership; ZERO `Editor::` methods).
pub struct GrammarContributionSet {
    plugin_id: PluginId,
    // `(name, doc, spec)` per kind. The specs carry boxed trampoline closures
    // (not `Clone`), so registration consumes the set.
    motions: Vec<(String, String, MotionSpec)>,
    operators: Vec<(String, String, OperatorSpec)>,
    text_objects: Vec<(String, String, TextObjectSpec)>,
    actions: Vec<(String, String, ActionSpec)>,
    ex_commands: Vec<(String, String, ExCommandSpec)>,
}

impl GrammarContributionSet {
    /// The host-issued id of the plugin behind these contributions (the `u32`
    /// stamped into `SourceLayer::Plugin`).
    pub fn plugin_id(&self) -> PluginId {
        self.plugin_id
    }

    /// Total number of contributions across all kinds.
    pub fn len(&self) -> usize {
        self.motions.len()
            + self.operators.len()
            + self.text_objects.len()
            + self.actions.len()
            + self.ex_commands.len()
    }

    /// True when the plugin registered no grammar.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Register every contribution into the caller's `CommandRegistry`, stamped
    /// `SourceLayer::Plugin(plugin_id)`. Consumes the set (the specs own boxed
    /// trampoline closures). The registry, dispatcher, and `:describe-*` views
    /// then treat each entry exactly like a builtin (paramount #3).
    pub fn register_all(self, registry: &mut CommandRegistry) {
        let id = self.plugin_id.0;
        for (name, doc, spec) in self.motions {
            registry.register_plugin_motion(id, &name, &doc, spec);
        }
        for (name, doc, spec) in self.operators {
            registry.register_plugin_operator(id, &name, &doc, spec);
        }
        for (name, doc, spec) in self.text_objects {
            registry.register_plugin_text_object(id, &name, &doc, spec);
        }
        for (name, doc, spec) in self.actions {
            registry.register_plugin_action(id, &name, &doc, spec);
        }
        for (name, doc, spec) in self.ex_commands {
            registry.register_plugin_ex_command(id, &name, &doc, spec);
        }
    }
}

impl PluginHost {
    /// Instantiate a `grammar-plugin` component, drive its `register-grammar`
    /// export, and return the native [`GrammarContributionSet`] (each spec's
    /// `apply` a sync trampoline into the guest). **Synchronous** end to end (the
    /// PH7.7 fork): instantiated against the sync `grammar_linker` (sync WASI +
    /// the `grammar` register import), so there is no async host import a sync
    /// `apply` could reach. The caller registers the result via
    /// [`GrammarContributionSet::register_all`].
    ///
    /// A malformed spec (an `arg-spec` / `latency-class` / `surface-form` that
    /// won't convert) fails registration loudly with [`PluginHostError::GrammarSpec`];
    /// a *runtime* `apply` failure is graceful (a no-op, §8), handled in the
    /// trampoline. Instantiation + `register-grammar` run under the generous
    /// lifecycle budget; per-`apply` calls arm the Reflex budget (audit F1).
    ///
    /// PO.3: pass `Some(tracer)` to instrument the sync grammar seam. Each guest
    /// call then reads the plugin's published [`HotGate`] once (a relaxed atomic
    /// load); at the default `Info` gate that is the trampoline's only added cost
    /// (design §4). `None` (tests / benches / a tracer-less loader) skips even the
    /// gate handoff. The tracer's `hot_gate(id)` is seeded to the plugin's current
    /// effective level and updated live by `:set plugin.trace-level`.
    pub fn instantiate_grammar_plugin(
        &self,
        component: &Component,
        manifest: &PluginManifest,
        tier: TrustTier,
        bus: &Arc<EventBus>,
        tracer: Option<&PluginTracerHandle>,
        // AP.3: the editor's `ConfigRegistry`, so a grammar action can READ an
        // option via `config::get-option` (auto-pair reads `auto-pair.style` to
        // gate manual vs auto behavior). The SAME registry the async config seam
        // writes — one editor registry shared across a plugin's seam instances.
        // `None` leaves `get-option` returning `none` (the pre-AP.3 behavior).
        config_registry: Option<&Arc<lattice_config::ConfigRegistry>>,
    ) -> Result<GrammarContributionSet, PluginHostError> {
        let (wasi, outcome, _data_dir) = self.build_plugin_wasi(manifest, tier);
        // TS.1: the tree-sitter seam is gated on the `tree-sitter` editor
        // capability — a grammar action of a plugin without the grant gets `none`
        // for its tree handle (design §5). Captured once here; every action spec's
        // trampoline reads it.
        let tree_sitter_granted = outcome
            .grant
            .editor
            .contains(lattice_mode::CapabilitySet::TREE_SITTER);
        for denied in &outcome.denied {
            tracing::warn!(
                plugin = %manifest.id,
                capability = ?denied,
                "grammar plugin loaded with a withheld capability (reduced function)"
            );
        }
        let mut store = self.new_store(
            wasi,
            outcome.grant,
            PluginBudget::default(),
            Some(&manifest.id),
        )?;
        // AP.3: wire the shared config registry so a grammar action's
        // `config::get-option` reads the live editor options.
        store.data_mut().config_registry = config_registry.cloned();
        let id = self.alloc_id();
        // OC.1: wire the emit context, for the same reason and at the same point
        // in the sequence `spawn_event_plugin` does — BEFORE `register-grammar`
        // runs, since a guest may `register-event` from there and will
        // `emit-event` later from an `apply-*` callback.
        //
        // `host-services` (and therefore `emit-event`) has been linked into this
        // sync store since OM.11, so the seam has *looked* available all along;
        // what was missing is the context behind it, and a call took the `None`
        // arm of `PluginState::emit_event` — a warn-and-drop. That is the
        // `plugin-gates-hand-guests-throwaway-contexts` shape: wired end to end,
        // answering nothing. It is a defect independent of clocking (any plugin
        // bridging a chord to its own async side hits it); clocking is only what
        // found it.
        //
        // Safe on the keystroke path: `EventBus::publish` snapshots subscribers
        // under a short lock and dispatches into unbounded channels with the lock
        // dropped — bounded, non-blocking work. The `Quarantine` below already
        // publishes on this same bus from this same trampoline.
        store.data_mut().event_emit = Some(crate::EventEmitCtx {
            plugin_id: id,
            bus: Arc::clone(bus),
        });
        // SYNC instantiate against the sync grammar linker — no async import to
        // drive, so a plain `instantiate` is correct (the PH7.7 fork).
        let bindings = GrammarPlugin::instantiate(&mut store, component, &self.grammar_linker)
            .map_err(|e| PluginHostError::Instantiate(e.into()))?;
        // Drive registration: the guest calls the imported `register-*`, which
        // record into the store's `GrammarContributions`.
        arm_store(&mut store, PluginBudget::default())?;
        bindings
            .call_register_grammar(&mut store)
            .map_err(|e| PluginHostError::Instantiate(e.into()))?;
        let recorded = store.data_mut().grammar_contributions.take();

        // PO.3: fetch this plugin's published hot-path gate (seeded to its current
        // effective level), or a permanently-off gate when no tracer is wired.
        let gate = tracer.map_or_else(HotGate::disabled, |t| t.hot_gate(id.0));

        // Move the store + bindings behind the shared lock every trampoline reads.
        // The `Quarantine` rides inside so one `apply-*` trap quarantines every
        // contribution from this plugin (they share the one guest).
        let authorizer = Arc::new(crate::EffectAuthorizer::new(
            &store.data().grant,
            manifest.id.clone(),
        ));
        let guest = Arc::new(Mutex::new(GrammarGuest {
            store,
            bindings,
            authorizer,
            quarantine: Quarantine::new(id, Arc::clone(bus)),
            plugin: id.0,
            gate,
            tracer: tracer.cloned(),
        }));

        let mut set = GrammarContributionSet {
            plugin_id: id,
            motions: Vec::new(),
            operators: Vec::new(),
            text_objects: Vec::new(),
            actions: Vec::new(),
            ex_commands: Vec::new(),
        };
        for contribution in recorded {
            match contribution {
                RecordedContribution::Motion {
                    name,
                    doc,
                    spec,
                    callback,
                } => set.motions.push((
                    name,
                    doc,
                    build_motion_spec(&guest, spec, callback, tree_sitter_granted)?,
                )),
                RecordedContribution::Operator {
                    name,
                    doc,
                    spec,
                    callback,
                } => set
                    .operators
                    .push((name, doc, build_operator_spec(&guest, spec, callback)?)),
                RecordedContribution::TextObject {
                    name,
                    doc,
                    spec,
                    callback,
                } => set.text_objects.push((
                    name,
                    doc,
                    build_text_object_spec(&guest, spec, callback, tree_sitter_granted)?,
                )),
                RecordedContribution::Action {
                    name,
                    doc,
                    spec,
                    callback,
                } => set.actions.push((
                    name,
                    doc,
                    build_action_spec(&guest, spec, callback, tree_sitter_granted)?,
                )),
                RecordedContribution::ExCommand {
                    name,
                    doc,
                    spec,
                    parse_callback,
                    apply_callback,
                } => set.ex_commands.push((
                    name,
                    doc,
                    build_ex_command_spec(&guest, spec, parse_callback, apply_callback)?,
                )),
            }
        }
        Ok(set)
    }
}
