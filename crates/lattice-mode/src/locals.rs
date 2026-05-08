//! Buffer-local mode-internal state — Shape A from
//! `mode-architecture.md` §9.4 (M.3.2.a).
//!
//! A typed analogue of emacs's `buffer-local-variables`. Each
//! piece of mode-internal data declares an [`OptionDecl`]-style
//! type identity via the [`BufferLocal`] trait; the typed-map
//! [`BufferLocals`] stores them keyed by `TypeId` for O(1)
//! type-keyed reads. Each entry carries metadata (display name,
//! doc, owner mode) so `:describe-buffer` can enumerate every
//! local a buffer carries grouped by its owning mode.
//!
//! ## Distinction from options
//!
//! Buffer-locals are *runtime data the mode owns*, not user-
//! configurable values. They store opaque Rust structs
//! (`SyntaxHandle`, `Vec<FileTreeEntry>`, `Vec<Link>`, ...)
//! that don't have string-parseable forms and shouldn't appear
//! in `:set` autocomplete. The user can inspect them via
//! `:describe-buffer` but never edit them via `:set` /
//! `:customize`.
//!
//! ## Ownership and the `OWNER_MODE` rule
//!
//! Each local declares the mode that owns it
//! ([`BufferLocal::OWNER_MODE`]). At write time the
//! [`crate::ModeContext`] checks that the *currently
//! activating* mode matches the local's owner; cross-mode
//! mutation is rejected as a typed error. This keeps each
//! mode's runtime state encapsulated -- a `git-blame-mode`
//! plugin can't accidentally clobber `file-tree-mode`'s
//! entries map.
//!
//! Reads are unrestricted: any mode can read any local. This
//! lets, e.g., `lsp-completion-mode` read `file-tree-mode`'s
//! entries to populate path completion without a special-case
//! handshake.
//!
//! ## What does NOT live here
//!
//! - **Universal buffer state** (rope, cursor, scroll,
//!   version) -- direct fields on whatever struct holds them.
//!   Buffer-locals are for *mode-specific* runtime data.
//! - **User-facing options** -- those are `OptionDecl` in
//!   `lattice-config`. See `mode-architecture.md` §6.4.
//! - **Declarative contributions** (option overrides, keymap
//!   layers, decoration providers) -- modes return these from
//!   `Mode::options()` etc.; the registry applies them. The
//!   mode never writes to them directly.
//!
//! ## Storage shape
//!
//! `BufferLocals` is a `HashMap<TypeId, Box<dyn LocalDyn>>`.
//! `LocalDyn` is a sealed inner trait that lets us:
//!
//! - Read metadata (`name`, `doc`, `owner_mode`, `describe`)
//!   without knowing the concrete type — for `:describe-buffer`.
//! - Downcast back to the concrete `T` for typed reads /
//!   removes — for the typed accessors.

use std::any::{Any, TypeId};
use std::collections::HashMap;

/// Compile-time declaration of a mode-owned per-buffer local.
///
/// Implementing types are typically newtypes wrapping the
/// underlying data:
///
/// ```ignore
/// pub struct FileTreeEntries(pub Vec<FileTreeEntry>);
///
/// impl BufferLocal for FileTreeEntries {
///     const NAME: &'static str = "file-tree.entries";
///     const DOC: &'static str = "Tree-of-files entries for this buffer.";
///     const OWNER_MODE: &'static str = "file-tree-mode";
///     fn describe(&self) -> String {
///         format!("{} entries", self.0.len())
///     }
/// }
/// ```
///
/// `'static` bound: locals key on `TypeId`, which requires the
/// type to be `'static`. `Send + Sync` so a buffer can be
/// shared across threads.
pub trait BufferLocal: Any + Send + Sync + 'static {
    /// Public display name (`:describe-buffer` row label,
    /// debug logs). Convention: `<owner-mode-name>.<key>`,
    /// e.g. `"file-tree.entries"`. Not a registry key — locals
    /// aren't registered globally; the name is metadata for
    /// inspection.
    const NAME: &'static str;

    /// Doc string. Shown in `:describe-buffer` when the user
    /// expands a local for detail.
    const DOC: &'static str;

    /// Mode id that owns this local. Enforced at write time:
    /// `ModeContext::set_local::<T>` rejects if the current
    /// mode's id doesn't match this.
    const OWNER_MODE: &'static str;

    /// Single-line summary of the local's value for
    /// `:describe-buffer`'s tabular display. Implementations
    /// should be cheap (no heavy formatting); detailed
    /// inspection is via the mode's own commands.
    fn describe(&self) -> String;
}

/// Sealed inner trait: object-safe view of [`BufferLocal`]
/// that the typed-map can store as `Box<dyn LocalDyn>`.
///
/// Not part of the public API — consumers implement
/// `BufferLocal`, the blanket impl below provides `LocalDyn`.
pub(crate) trait LocalDyn: Any + Send + Sync {
    fn name(&self) -> &'static str;
    fn doc(&self) -> &'static str;
    fn owner_mode(&self) -> &'static str;
    fn describe(&self) -> String;
    fn as_any(&self) -> &dyn Any;
    fn as_any_mut(&mut self) -> &mut dyn Any;
    fn into_any(self: Box<Self>) -> Box<dyn Any + Send + Sync>;
}

impl<T: BufferLocal> LocalDyn for T {
    fn name(&self) -> &'static str {
        T::NAME
    }
    fn doc(&self) -> &'static str {
        T::DOC
    }
    fn owner_mode(&self) -> &'static str {
        T::OWNER_MODE
    }
    fn describe(&self) -> String {
        BufferLocal::describe(self)
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
    fn into_any(self: Box<Self>) -> Box<dyn Any + Send + Sync> {
        self
    }
}

/// Read-only descriptor for one buffer-local, returned by
/// [`BufferLocals::iter_descriptors`] for `:describe-buffer`.
#[derive(Debug, Clone)]
pub struct LocalDescriptor {
    pub name: &'static str,
    pub doc: &'static str,
    pub owner_mode: &'static str,
    pub describe: String,
}

/// Typed-map of buffer-local mode-internal state.
///
/// Stored on per-buffer App state (the App's BufferEntry in
/// `lattice-ui-tui`). Modes write entries through a borrowed
/// [`crate::ModeContext`] during `on_activate`, remove during
/// `on_deactivate`. Outside lifecycle hooks, code reads
/// directly via [`Self::get`].
#[derive(Default)]
pub struct BufferLocals {
    map: HashMap<TypeId, Box<dyn LocalDyn>>,
}

impl std::fmt::Debug for BufferLocals {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BufferLocals")
            .field("len", &self.map.len())
            .finish_non_exhaustive()
    }
}

impl BufferLocals {
    pub fn new() -> Self {
        Self::default()
    }

    /// Number of locals stored.
    pub fn len(&self) -> usize {
        self.map.len()
    }

    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }

    /// Insert (or replace) the local of type `T`. Public so
    /// the App can seed locals at buffer-construction time
    /// (e.g. help-mode parsing the markdown links into
    /// `HelpLinks` when a help buffer is constructed -- the
    /// "owner" semantically is help-mode, but the App is the
    /// caller because parsing lives in the constructor).
    ///
    /// The owner-mode check is intentionally NOT enforced
    /// here; that's [`crate::ModeContext::set_local`]'s job
    /// for *active modes' runtime writes*. App-level
    /// construction-time seeding is a separate path: the App
    /// is presumed to insert locals owned by the buffer's
    /// eventual major mode, and the local's `OWNER_MODE`
    /// field is metadata for `:describe-buffer` attribution
    /// rather than an access-control mechanism on this
    /// surface. Mirrors emacs's `setq-local` -- any code can
    /// set a buffer-local; the major mode's claim of
    /// ownership is by convention.
    pub fn insert<T: BufferLocal>(&mut self, value: T) {
        self.map.insert(TypeId::of::<T>(), Box::new(value));
    }

    /// Read the local of type `T` if present.
    pub fn get<T: BufferLocal>(&self) -> Option<&T> {
        self.map
            .get(&TypeId::of::<T>())
            .and_then(|entry| entry.as_any().downcast_ref::<T>())
    }

    /// Mutably borrow the local of type `T`. Used by the
    /// owner mode during `on_activate` if it needs to mutate
    /// in place (avoids the take/restore dance). `pub(crate)`
    /// so external code goes through the context's checked
    /// surface.
    pub(crate) fn get_mut<T: BufferLocal>(&mut self) -> Option<&mut T> {
        self.map
            .get_mut(&TypeId::of::<T>())
            .and_then(|entry| entry.as_any_mut().downcast_mut::<T>())
    }

    /// Remove and return the local of type `T`. `pub(crate)`
    /// so removal goes through the context's owner-mode check.
    pub(crate) fn remove<T: BufferLocal>(&mut self) -> Option<T> {
        let entry = self.map.remove(&TypeId::of::<T>())?;
        entry.into_any().downcast::<T>().ok().map(|b| *b)
    }

    /// Iterate over descriptors for inspection. Walks every
    /// local with its display metadata + a single-line
    /// summary. Used by `:describe-buffer` to enumerate all
    /// state on a buffer grouped by owner mode.
    pub fn iter_descriptors(&self) -> impl Iterator<Item = LocalDescriptor> + '_ {
        self.map.values().map(|entry| LocalDescriptor {
            name: entry.name(),
            doc: entry.doc(),
            owner_mode: entry.owner_mode(),
            describe: entry.describe(),
        })
    }

    /// True if a local of type `T` is currently stored.
    pub fn contains<T: BufferLocal>(&self) -> bool {
        self.map.contains_key(&TypeId::of::<T>())
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]
    use super::*;

    // Test fixture: one mode-owned local.
    struct TestEntries(Vec<String>);

    impl BufferLocal for TestEntries {
        const NAME: &'static str = "test.entries";
        const DOC: &'static str = "Test fixture for buffer-locals.";
        const OWNER_MODE: &'static str = "test-mode";
        fn describe(&self) -> String {
            format!("{} entries", self.0.len())
        }
    }

    // A second fixture for distinct-key tests.
    struct OtherFixture(i64);

    impl BufferLocal for OtherFixture {
        const NAME: &'static str = "test.other";
        const DOC: &'static str = "Second fixture.";
        const OWNER_MODE: &'static str = "other-mode";
        fn describe(&self) -> String {
            format!("value={}", self.0)
        }
    }

    #[test]
    fn empty_locals_have_no_entries() {
        let l = BufferLocals::new();
        assert!(l.is_empty());
        assert_eq!(l.len(), 0);
        assert!(l.get::<TestEntries>().is_none());
    }

    #[test]
    fn insert_get_round_trips() {
        let mut l = BufferLocals::new();
        l.insert(TestEntries(vec!["a".into(), "b".into()]));
        let got = l.get::<TestEntries>().expect("present");
        assert_eq!(got.0.len(), 2);
        assert_eq!(got.0[0], "a");
        assert!(l.contains::<TestEntries>());
    }

    #[test]
    fn second_insert_replaces() {
        let mut l = BufferLocals::new();
        l.insert(TestEntries(vec!["a".into()]));
        l.insert(TestEntries(vec!["x".into(), "y".into(), "z".into()]));
        assert_eq!(l.get::<TestEntries>().unwrap().0.len(), 3);
        assert_eq!(l.len(), 1);
    }

    #[test]
    fn get_mut_returns_mutable_reference() {
        let mut l = BufferLocals::new();
        l.insert(TestEntries(vec!["a".into()]));
        l.get_mut::<TestEntries>().unwrap().0.push("b".into());
        assert_eq!(l.get::<TestEntries>().unwrap().0.len(), 2);
    }

    #[test]
    fn remove_returns_owned_value() {
        let mut l = BufferLocals::new();
        l.insert(TestEntries(vec!["x".into()]));
        let removed = l.remove::<TestEntries>().expect("present");
        assert_eq!(removed.0[0], "x");
        assert!(l.is_empty());
    }

    #[test]
    fn distinct_types_coexist() {
        let mut l = BufferLocals::new();
        l.insert(TestEntries(vec!["a".into()]));
        l.insert(OtherFixture(42));
        assert_eq!(l.len(), 2);
        assert_eq!(l.get::<TestEntries>().unwrap().0.len(), 1);
        assert_eq!(l.get::<OtherFixture>().unwrap().0, 42);
    }

    #[test]
    fn iter_descriptors_yields_metadata_per_local() {
        let mut l = BufferLocals::new();
        l.insert(TestEntries(vec!["a".into()]));
        l.insert(OtherFixture(7));
        let mut descriptors: Vec<_> = l.iter_descriptors().collect();
        descriptors.sort_by(|a, b| a.name.cmp(b.name));
        assert_eq!(descriptors.len(), 2);
        assert_eq!(descriptors[0].name, "test.entries");
        assert_eq!(descriptors[0].owner_mode, "test-mode");
        assert_eq!(descriptors[0].describe, "1 entries");
        assert_eq!(descriptors[1].name, "test.other");
        assert_eq!(descriptors[1].owner_mode, "other-mode");
        assert_eq!(descriptors[1].describe, "value=7");
    }
}
