//! PV.1 (2026-08-12): the **provider-view seam** — one generic host
//! primitive for "open the multibuffer view a provider owns".
//!
//! Design: `docs/dev/architecture/multibuffer-views.md` §3.7a. First
//! consumer: `lattice-magit`'s project-diff view (PD.3).
//!
//! ## The problem it solves
//!
//! A multibuffer view can only be created through
//! [`ModeActivator`](crate::ModeActivator), which is `&mut`-backed and
//! therefore reachable only from the host. A provider's trigger — an
//! ex-command or a chord-fired action handler — runs against `&self`
//! state and returns an [`Effect`]. So every provider needs *some*
//! effect that carries "open my view" back to a place holding the
//! activator.
//!
//! Before this seam each provider spent its own `AppEffect` variant on
//! that, plus a match arm in the host's dispatcher and a third arm at
//! the plugin boundary: three crates touched for the N+1th provider,
//! which contradicts the acid test a provider crate is supposed to pass
//! (`multibuffer-views.md`: a new provider crate should require zero
//! host additions).
//!
//! This registry replaces the per-provider variant with a single
//! `AppEffect::OpenProviderView { provider, args }`. Provider crates
//! register an opener under a name at boot; the host arm looks the name
//! up, calls the opener with itself as the activator, and applies the
//! generic outcome (activate + echo). Adding a provider now touches
//! exactly one crate — the provider's own.
//!
//! ## What deliberately does NOT go through it
//!
//! `:narrow` / `zn` also produce a multibuffer, and they stay on their
//! typed `AppEffect::{NarrowTrigger,NarrowLines}` variants. They are not
//! the same operation: narrowing resolves a *range against live editor
//! state* — cursor, last-Visual extent, the mark table, and the
//! composed→source one-hop translation — none of which is a provider
//! parameter. Routing it here would mean exporting marks and visual
//! state through [`ModeActivator`], polluting a generic trait with one
//! consumer's surface, which is the rejection `multibuffer-views.md`
//! §3.6 already made against `Document::excerpts()`.

use std::collections::HashMap;
use std::sync::Arc;

use arc_swap::ArcSwap;
use lattice_core::BufferId;
use lattice_grammar::Args;

use crate::activator::ModeActivator;

/// What an opener did, in terms the host can apply generically.
///
/// The host arm knows only these two outcomes — it never learns what
/// the provider computed. Both carry their own message so the *provider*
/// words its own success and refusal (the host has no vocabulary for
/// "no changed files in the working tree").
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProviderViewOutcome {
    /// The view exists. The host activates `view` and echoes `message`
    /// at info level if one is supplied.
    Opened {
        view: BufferId,
        message: Option<String>,
    },
    /// Nothing was opened, for a reason the user should see (not a git
    /// repository, empty result set, a service missing because the
    /// build dropped a feature). Echoed at warn level.
    ///
    /// Declining is a first-class outcome, not an error path: opening
    /// an empty view and leaving the user to guess why is the worse UX.
    Declined { message: String },
}

/// A provider's view-opening closure.
///
/// Receives the host as a [`ModeActivator`] (so it can call
/// `create_multibuffer_view` / `ensure_named_document` and reach every
/// registered service through `activator.services()`) plus the trigger's
/// arguments, verbatim from the ex-command or transient row that fired.
///
/// `Send + Sync` because the registry is shared behind an `Arc`; the
/// closure itself always runs on the editor thread, inside the host's
/// effect application.
pub type ProviderViewOpener =
    Arc<dyn Fn(&mut dyn ModeActivator, &Args) -> ProviderViewOutcome + Send + Sync + 'static>;

/// Typed handle for `ServiceRegistry` lookup.
///
/// Per the `ServiceRegistry` `TypeId` convention: register and look up
/// under THIS alias, never the inner type — registering an
/// `Arc<ProviderViewRegistry>` and asking for `ProviderViewRegistry`
/// silently returns `None`.
pub type ProviderViewRegistryHandle = Arc<ProviderViewRegistry>;

/// OA.15a: "re-open the view I own, with these arguments" — asked for
/// from somewhere that holds no activator and returns no [`Effect`].
///
/// ## Why an effect was not enough
///
/// [`AppEffect::OpenProviderView`](lattice_grammar::app_effect::AppEffect)
/// already says this, and every trigger that can *return* an effect
/// should keep using it. What it cannot serve is a producer that is not
/// running inside a trigger at all: a plugin's `on-event` handler
/// returns `()` by construction (`wit/types.wit`'s event seam is
/// observation-shaped), and a background task holds neither the
/// dispatcher nor the activator.
///
/// The first consumer is the one that made the gap visible. A guest
/// mode's activation is delivered as `minor-activated`, and a guest mode
/// has no lifecycle body of its own to hang behaviour on — `PluginMode`
/// is data with a no-op `on_activate` (`mode_host.rs`). So a plugin
/// whose mode is supposed to CHANGE ITS VIEW could observe the
/// activation and do nothing about it, which makes such a mode a label
/// rather than a switch.
///
/// ## The `enable-mode` precedent, one step further
///
/// `Event::ModeEnablementRequested` is the same shape: the guest cannot
/// reach the activator, so the call is a REQUEST and the Editor applies
/// it. This is that pattern for views, with one deliberate difference —
/// it is a **typed** event, so `BootContext::wake_on_event` covers it
/// and the re-scan reaches the screen with no keystroke. A request that
/// only landed on the next keypress would reproduce, exactly, the
/// "works, but only after I hit something" class this codebase has paid
/// for repeatedly.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderViewRefreshRequested {
    /// The provider name, as registered in [`ProviderViewRegistry`].
    pub provider: String,
    /// The view arguments, verbatim. Routed, never read: they are the
    /// provider's own vocabulary, the same contract `scan_args` carries.
    pub args: Vec<String>,
}

lattice_protocol::register_event!(
    ProviderViewRefreshRequested,
    "provider-view.refresh-requested",
    "A provider asked the Editor to re-open one of its views.",
    "lattice-mode",
);

/// Name → opener, registered at boot and read once per trigger.
///
/// Same wait-free shape as
/// [`ActionHandlerRegistry`](crate::ActionHandlerRegistry) — copy-on-
/// write registration, `Arc` load on lookup — because it is the same
/// kind of thing: a table of provider-contributed closures the host
/// consults without knowing what is in them.
///
/// **Lifetime, amended by MV.1.** Native providers register once during
/// subsystem `install(&mut boot)` and live for the process, which is why
/// there is no RAII token. A PLUGIN's views do not: a plugin unloads and
/// reloads, so [`unregister`](Self::unregister) exists for the teardown
/// path. Without it a reload's `register` would return `false` against
/// the plugin's own stale opener and its views would come back dead.
#[derive(Default)]
pub struct ProviderViewRegistry {
    openers: ArcSwap<HashMap<String, ProviderViewOpener>>,
}

impl ProviderViewRegistry {
    pub fn new() -> Self {
        Self {
            openers: ArcSwap::from_pointee(HashMap::new()),
        }
    }

    /// Register `opener` under `name`.
    ///
    /// Returns `false` when `name` was already taken — and does NOT
    /// replace it. Two providers claiming one name is a boot-wiring bug,
    /// and last-write-wins would make which view `:foo` opens depend on
    /// install order; refusing lets the caller log the collision while
    /// the first registration keeps working.
    pub fn register(&self, name: impl Into<String>, opener: ProviderViewOpener) -> bool {
        let name = name.into();
        let mut inserted = false;
        self.openers.rcu(|map| {
            let mut next = (**map).clone();
            inserted = !next.contains_key(&name);
            if inserted {
                next.insert(name.clone(), opener.clone());
            }
            next
        });
        inserted
    }

    /// Remove `name`'s opener, returning whether one was there.
    ///
    /// MV.1: the plugin teardown path. Native providers never call it —
    /// they outlive every unload — so an unknown name is `false` rather
    /// than a warning.
    pub fn unregister(&self, name: &str) -> bool {
        let mut removed = false;
        self.openers.rcu(|map| {
            let mut next = (**map).clone();
            removed = next.remove(name).is_some();
            next
        });
        removed
    }

    /// Look up an opener by name. `None` for an unregistered provider —
    /// the host then echoes rather than silently doing nothing.
    pub fn lookup(&self, name: &str) -> Option<ProviderViewOpener> {
        self.openers.load().get(name).cloned()
    }

    /// Registered provider names, sorted. For introspection + tests.
    pub fn names(&self) -> Vec<String> {
        let mut names: Vec<String> = self.openers.load().keys().cloned().collect();
        names.sort();
        names
    }
}

impl std::fmt::Debug for ProviderViewRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProviderViewRegistry")
            .field("providers", &self.names())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    /// Minimal `ModeActivator` for tests whose openers never call
    /// through it.
    struct NullActivator;

    impl ModeActivator for NullActivator {
        fn activate_major_for_kind(&mut self, _: BufferId, _: lattice_core::BufferKind) {}
        fn activate_minor_by_id(&mut self, _: BufferId, _: crate::ModeId) {}
        fn ensure_named_document(
            &mut self,
            _: &str,
            _: crate::ModeId,
            _: lattice_core::BufferFlags,
        ) -> BufferId {
            BufferId(0)
        }
        fn services(&self) -> Arc<crate::ServiceRegistry> {
            Arc::new(crate::ServiceRegistry::default())
        }
    }

    fn opener(view: u32) -> ProviderViewOpener {
        Arc::new(
            move |_: &mut dyn ModeActivator, _: &Args| ProviderViewOutcome::Opened {
                view: BufferId(view),
                message: None,
            },
        )
    }

    #[test]
    fn an_unregistered_provider_looks_up_to_nothing() {
        let reg = ProviderViewRegistry::new();
        assert!(reg.lookup("nobody").is_none());
        assert!(reg.names().is_empty());
    }

    #[test]
    fn a_registered_opener_is_found_by_name() {
        let reg = ProviderViewRegistry::new();
        assert!(reg.register("magit-project-diff", opener(7)));
        assert!(reg.lookup("magit-project-diff").is_some());
        assert_eq!(reg.names(), vec!["magit-project-diff".to_string()]);
    }

    /// A name collision is a boot-wiring bug. The FIRST registration
    /// wins so which view a trigger opens does not depend on the order
    /// subsystems happen to install in.
    #[test]
    fn a_duplicate_name_is_refused_and_the_first_registration_survives() {
        let reg = ProviderViewRegistry::new();
        assert!(reg.register("dup", opener(1)));
        assert!(
            !reg.register("dup", opener(2)),
            "the second registration is refused"
        );

        let found = reg.lookup("dup").unwrap();
        assert_eq!(
            found(&mut NullActivator, &Args::None),
            ProviderViewOutcome::Opened {
                view: BufferId(1),
                message: None
            },
            "the surviving opener is the one registered first"
        );
    }

    /// The args reach the opener verbatim — the trigger's parameters are
    /// the provider's business, not the host's.
    #[test]
    fn args_pass_through_to_the_opener_untouched() {
        let reg = ProviderViewRegistry::new();
        reg.register(
            "echo",
            Arc::new(|_: &mut dyn ModeActivator, args: &Args| match args {
                Args::String(s) => ProviderViewOutcome::Declined { message: s.clone() },
                _ => ProviderViewOutcome::Declined {
                    message: "<none>".into(),
                },
            }),
        );
        let opener = reg.lookup("echo").unwrap();
        assert_eq!(
            opener(&mut NullActivator, &Args::String("staged".into())),
            ProviderViewOutcome::Declined {
                message: "staged".into()
            }
        );
    }
}
