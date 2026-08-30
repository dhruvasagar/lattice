//! MV.1b — the **generic provider** behind every plugin-owned multibuffer view.
//!
//! Design: [`plugin-multibuffer-views.md`](../../../../../docs/dev/architecture/plugin-multibuffer-views.md).
//! Slice plan: `slice-plans/plugin-multibuffer-views.md` MV.1.
//!
//! ## What is generic here, and what is the guest's
//!
//! One `ProviderViewOpener` is registered per view a guest declared, so
//! `AppEffect::OpenProviderView { provider, args }` and the view's `gr` both
//! reach it by the guest's own name. This module knows nothing about what the
//! view is *for*: it calls `build`, resolves each excerpt's path to a source
//! document, creates or reuses the named buffer, activates the guest's mode,
//! and writes the guest's summary into the headerline.
//!
//! The guest owns the view's identity (`buffer-name`), its contents and their
//! order, its interactions (`view-mode`), and its status text. Before this,
//! all four were host constants and only the agenda had them — which is why
//! `providers/agenda.rs` is ~1000 lines and org's second view had nowhere to
//! go.
//!
//! ## Why the paths are resolved here rather than crossed as buffer ids
//!
//! `Excerpt` carries a `BufferId` and only the host can mint one. A guest
//! naming a path is the same trade `Effect::WriteToFile` makes: a path is the
//! stable name both sides already share, and the host resolves it.
//!
//! **One source document per path**, however many excerpts point into it —
//! otherwise a file with five rows is opened five times and an edit through one
//! row is invisible through the others. That is `providers/agenda.rs`'s rule
//! and the reason is identical.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use lattice_core::{BufferFlags, BufferId, DocumentBuilder};
use lattice_grammar::{CommandRegistry, CommandRegistryHandle};
use lattice_mode::{ModeActivator, ProviderViewOutcome, ServiceRegistry};
use lattice_runtime::{Document, spawn_document};

use crate::registry::MultibufferRegistryHandle;
use crate::view::create_multibuffer_view;
use crate::{Excerpt, ExcerptHeader, ExcerptHeaderStyle, ExcerptId, HeaderlineStatus};

/// One excerpt a guest asked for, already across the boundary.
///
/// A plain struct rather than the WIT type: `lattice-multibuffer` does not
/// depend on the plugin host (and must not — the host depends on *it*), so the
/// loader converts and hands these over.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginExcerpt {
    pub path: PathBuf,
    pub start_line: u32,
    pub end_line: u32,
    /// Empty renders no header row — the grouping mechanism.
    pub header: String,
    pub match_count: Option<u32>,
}

/// What a guest's `build` produced.
#[derive(Debug, Clone, Default)]
pub struct PluginViewResult {
    pub excerpts: Vec<PluginExcerpt>,
    pub summary: String,
}

/// The identity a guest declared for one of its views.
#[derive(Debug, Clone)]
pub struct PluginViewSpec {
    pub id: String,
    pub doc: String,
    pub buffer_name: String,
    pub view_mode: Option<String>,
    pub reuse: bool,
}

/// Create (or reuse) the view's buffer and hand back its id — the SYNCHRONOUS
/// half.
///
/// `ProviderViewOpener` is a sync closure, and `build` is an async guest call
/// that may read files. Blocking the opener on it would put a plugin's file
/// reads on the dispatch path, which paramount #1 forbids outright. So this
/// half seats an empty view with an in-progress headerline and returns
/// immediately; [`fill_plugin_view`] applies the guest's result when it lands.
/// That is `providers/agenda.rs`'s shape (`open_agenda` + `spawn_agenda_scan`)
/// and it is the same reason.
pub fn open_plugin_view(
    activator: &mut dyn ModeActivator,
    spec: &PluginViewSpec,
    last_view: Option<BufferId>,
) -> ProviderViewOutcome {
    let services = activator.services();

    let Some(registry) = services.get::<CommandRegistryHandle>() else {
        return ProviderViewOutcome::Declined {
            message: format!("{}: command registry unavailable", spec.id),
        };
    };
    let lang_registry = services
        .get::<Arc<lattice_syntax::LangRegistry>>()
        .map(|h| (*h).clone());

    // SECURITY: reuse only a view THIS provider made.
    //
    // Looking a view up by its buffer name would let a guest declare
    // `buffer-name: "*agenda*"` and take over the agenda's buffer —
    // `replace_excerpts` on someone else's view, from a plugin that owns
    // nothing. Buffer names are a flat, unnamespaced space shared with every
    // native provider, so a guest-chosen one cannot be an authority to reuse.
    // The id the provider recorded last time is.
    let existing = spec
        .reuse
        .then(|| last_view.filter(|id| still_a_multibuffer(&services, *id)))
        .flatten();

    let view = match existing {
        Some(view) => view,
        None => create_multibuffer_view(
            activator,
            HashMap::new(),
            Vec::new(),
            Some(spec.buffer_name.clone()),
            BufferFlags::default(),
            (*registry).clone(),
            lang_registry,
        ),
    };

    let services = activator.services();
    let Some(mb_registry) = services.get::<MultibufferRegistryHandle>() else {
        return ProviderViewOutcome::Declined {
            message: format!("{}: multibuffer registry unavailable", spec.id),
        };
    };
    let Some(handle) = mb_registry.handle(view) else {
        return ProviderViewOutcome::Declined {
            message: format!("{}: the view failed to open", spec.id),
        };
    };

    // Replace rather than append: a `gr` refresh re-runs `build` and must not
    // stack a second copy of every row onto the first.
    handle.replace_excerpts(HashMap::new(), Vec::new());
    handle.set_headerline(HeaderlineStatus::InProgress {
        label: format!("Building {}", spec.id),
        count: None,
        emphasis: None,
    });

    // The guest's minor, by name. A name that is not registered warns through
    // the ordinary activation path rather than failing the open — the rows are
    // still worth showing.
    if let Some(mode) = spec.view_mode.as_deref() {
        activator.activate_minor_by_id(view, lattice_mode::ModeId::new(mode));
    }

    ProviderViewOutcome::Opened {
        view,
        message: None,
    }
}

/// Apply a guest's `build` result to an already-seated view — the ASYNCHRONOUS
/// half, called from the task that awaited the guest.
///
/// Publishing [`MultibufferExcerptsReady`] at the end is not optional. An async
/// result that lands without a wake sits until the user happens to press a key,
/// and the symptom reads as a rendering bug rather than a missing wake — the
/// bug class re-introduced repeatedly and designed out by
/// `boot.wake_on_event::<MultibufferExcerptsReady>()`.
pub fn fill_plugin_view(
    mb_registry: &MultibufferRegistryHandle,
    events: Option<&Arc<lattice_runtime::EventBus>>,
    view: BufferId,
    spec: &PluginViewSpec,
    result: PluginViewResult,
) {
    let Some(handle) = mb_registry.handle(view) else {
        return;
    };

    let mut sources: HashMap<PathBuf, BufferId> = HashMap::new();
    let mut excerpts: Vec<Excerpt> = Vec::with_capacity(result.excerpts.len());
    let mut dropped = 0usize;

    for row in &result.excerpts {
        // A path that cannot be read is DROPPED with a log, not a failed view —
        // `error-parser`'s rule, and the same failure class: a stale index must
        // not cost you every other row it got right.
        let source = match sources.get(&row.path) {
            Some(id) => *id,
            None => {
                let Ok(text) = std::fs::read_to_string(&row.path) else {
                    tracing::debug!(
                        view = %spec.id,
                        path = %row.path.display(),
                        "plugin view: excerpt source unreadable; dropping the excerpt"
                    );
                    dropped += 1;
                    continue;
                };
                let id = BufferId::next();
                let document = DocumentBuilder::default()
                    .with_text(&text)
                    .with_path(row.path.clone())
                    .build();
                let doc_registry =
                    Arc::new(arc_swap::ArcSwap::from_pointee(CommandRegistry::new()));
                let doc_handle = spawn_document(id, document, doc_registry);
                handle.add_source(id, Arc::new(doc_handle) as Arc<dyn Document>);
                sources.insert(row.path.clone(), id);
                id
            }
        };
        excerpts.push(Excerpt {
            id: ExcerptId::next(),
            source,
            start_line: row.start_line,
            end_line: row.end_line,
            header: ExcerptHeader {
                title: row.header.clone(),
                style: ExcerptHeaderStyle::default(),
                path: Some(row.path.clone()),
                match_count: row.match_count,
            },
        });
    }

    let count = excerpts.len();
    handle.append_excerpts(excerpts);

    // The guest's own summary. The host has no vocabulary for "42 backlinks",
    // which is exactly why `build` returns one.
    let summary = if dropped == 0 {
        result.summary.clone()
    } else {
        format!("{} ({dropped} unreadable)", result.summary)
    };
    handle.set_headerline(HeaderlineStatus::Complete {
        summary,
        emphasis: None,
    });
    let _ = count;

    if let Some(bus) = events {
        bus.publish_typed(crate::events::MultibufferExcerptsReady { view });
    }
}

/// Report a guest decline on an already-seated view.
///
/// The view stays open with the guest's message in its headerline rather than
/// vanishing: by the time `build` answers, the user is looking at the buffer,
/// and closing it under them is worse than telling them why it is empty.
pub fn decline_plugin_view(
    mb_registry: &MultibufferRegistryHandle,
    events: Option<&Arc<lattice_runtime::EventBus>>,
    view: BufferId,
    message: &str,
) {
    let Some(handle) = mb_registry.handle(view) else {
        return;
    };
    handle.set_headerline(HeaderlineStatus::Complete {
        summary: message.to_string(),
        emphasis: None,
    });
    if let Some(bus) = events {
        bus.publish_typed(crate::events::MultibufferExcerptsReady { view });
    }
}

/// Whether `id` is still a live multibuffer.
///
/// `existing_view`'s rule: a view the user closed is not reusable, and its
/// `BufferId` may since have been handed to something else — appending excerpts
/// to that would be worse than making a fresh view.
fn still_a_multibuffer(services: &ServiceRegistry, id: BufferId) -> bool {
    services
        .get::<MultibufferRegistryHandle>()
        .and_then(|registry| registry.handle(id))
        .is_some()
}

/// Turn a guest decline into the generic outcome.
///
/// Separate function because the message is the GUEST's — the host must not
/// reword it. `build` returning `err` means "there is nothing to show and here
/// is why", which is a first-class outcome and not an error path: opening an
/// empty view and leaving the user to guess is the worse UX.
pub fn declined(view_id: &str, message: String) -> ProviderViewOutcome {
    ProviderViewOutcome::Declined {
        message: format!("{view_id}: {message}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_decline_carries_the_guests_own_words() {
        let outcome = declined("org-roam-backlinks", "nothing links here yet".to_string());
        match outcome {
            ProviderViewOutcome::Declined { message } => {
                assert!(message.contains("nothing links here yet"));
                assert!(message.starts_with("org-roam-backlinks:"));
            }
            other => panic!("expected a decline, got {other:?}"),
        }
    }
}
