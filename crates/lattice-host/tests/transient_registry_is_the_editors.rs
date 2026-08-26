//! TR.1 — the transient-menu registry belongs to the editor, not to magit.
//!
//! `TransientSourceRegistry` is a `lattice-picker` mechanism. Magit was its
//! first user and, by accident, its owner: `lattice-magit::install`
//! constructed it and registered the service. That made every transient menu
//! in the editor conditional on magit having loaded — fine while magit was the
//! only source, and wrong the moment a plugin contributes one (TR.2), because
//! the dependency is on a crate nothing declares and nothing would name at the
//! point of failure.
//!
//! Design: `docs/dev/architecture/plugin-transients.md` §3.

#![allow(clippy::unwrap_used)]

use lattice_core::Document as CoreDocument;
use lattice_host::editor::Editor;
use lattice_picker::TransientSourceRegistryHandle;

/// The service resolves on a booted editor. Registered by `editor_boot`
/// BEFORE any install that populates it — ordering that matters, since magit
/// installs early and now looks the service up rather than creating it.
#[test]
fn the_registry_resolves_on_a_booted_editor() {
    let editor = Editor::boot(CoreDocument::from_text("hi\n"));
    assert!(
        editor
            .services
            .get::<TransientSourceRegistryHandle>()
            .is_some(),
        "the editor owns the transient registry"
    );
}

/// And magit's own menus are still in it — the move must not cost the sources
/// that motivated the registry in the first place. `magit-dispatch` is
/// magit's root menu, registered by `lattice_magit::install`.
#[test]
fn magits_sources_are_still_registered_into_it() {
    let editor = Editor::boot(CoreDocument::from_text("hi\n"));
    let registry = editor
        .services
        .get::<TransientSourceRegistryHandle>()
        .unwrap();

    let ctx = lattice_picker::TransientContext {
        major_mode: None,
        minor_modes: Vec::new(),
        buffer: None,
        args: Default::default(),
    };
    assert!(
        registry.build("magit-dispatch", &ctx).is_some(),
        "magit contributes into the editor's registry rather than its own"
    );
}

/// A name nobody registered answers `None` rather than panicking — the
/// property TR.2's guest-supplied names will lean on, since a plugin can
/// name anything.
#[test]
fn an_unregistered_name_is_none_not_a_panic() {
    let editor = Editor::boot(CoreDocument::from_text("hi\n"));
    let registry = editor
        .services
        .get::<TransientSourceRegistryHandle>()
        .unwrap();
    let ctx = lattice_picker::TransientContext {
        major_mode: None,
        minor_modes: Vec::new(),
        buffer: None,
        args: Default::default(),
    };
    assert!(registry.build("no-such-menu", &ctx).is_none());
}
