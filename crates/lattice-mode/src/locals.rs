//! Buffer-local mode-internal state — Shape A from
//! `mode-architecture.md` §9.4 (M.3.2.a).
//!
//! A typed analogue of emacs's `buffer-local-variables`. Each
//! piece of mode-internal data declares an `OptionDecl`-style
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
/// Slice 3c.final.B.9: `Clone` is required so the typed-map
/// can be deep-cloned for the `BufferLocalsRenderState` per-publish
/// snapshot. Every existing impl is already a wrapper around
/// Clone primitives (`Vec<T>` / `PathBuf` / scalars), so the bound
/// adds no real constraint — just lets `LocalDyn::clone_box` work
/// through the dyn trait object.
pub trait BufferLocal: Any + Clone + Send + Sync + 'static {
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
    // Designed-in API: `as_any_mut` for owner-mode in-place
    // mutation, `into_any` for the `remove<T>` typed downcast.
    // Both reach `pub(crate)` BufferLocals methods that aren't
    // yet wired into an owner mode; flagging would penalise a
    // deliberate completeness choice.
    #[allow(dead_code)]
    fn as_any_mut(&mut self) -> &mut dyn Any;
    #[allow(dead_code)]
    fn into_any(self: Box<Self>) -> Box<dyn Any + Send + Sync>;
    /// Slice 3c.final.B.9: clone the inner local into a fresh
    /// `Box<dyn LocalDyn>` so [`BufferLocals`] (and the
    /// `BufferLocalsRenderState` lift on top of it) can be
    /// cloned per publish.
    fn clone_box(&self) -> Box<dyn LocalDyn>;
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
    fn clone_box(&self) -> Box<dyn LocalDyn> {
        Box::new(self.clone())
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

impl Clone for BufferLocals {
    /// Slice 3c.final.B.9: walks the typed-map and `clone_box`'s
    /// each entry so the whole `BufferLocals` can be deep-cloned
    /// for `BufferLocalsRenderState` publishes.
    fn clone(&self) -> Self {
        let map = self.map.iter().map(|(k, v)| (*k, v.clone_box())).collect();
        Self { map }
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
    /// surface. Designed-in API: no owner-mode wired today.
    #[allow(dead_code)]
    pub(crate) fn get_mut<T: BufferLocal>(&mut self) -> Option<&mut T> {
        self.map
            .get_mut(&TypeId::of::<T>())
            .and_then(|entry| entry.as_any_mut().downcast_mut::<T>())
    }

    /// Remove and return the local of type `T`. `pub(crate)`
    /// so removal goes through the context's owner-mode check.
    /// Designed-in API: no owner-mode wired today.
    #[allow(dead_code)]
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

/// The directory a buffer is *about*, when that is not its own path.
///
/// A magit status buffer, an oil listing, a file tree, a search or agenda
/// view — none of them is a file, so none has a path, and every one of them
/// still belongs somewhere. Without this the editor's project resolution
/// takes its only other branch and answers with the **process working
/// directory**, so `:files` in a magit buffer for `~/work/api` listed
/// whatever tree the editor happened to be launched in.
///
/// ## A directory, not a project root
///
/// The writer records where the buffer *is*; the host resolves that to a
/// project through the ordinary `ProjectResolverHandle`. So an oil buffer on
/// `/repo/src` records `/repo/src` and `:files` still lists `/repo` — the
/// provider does not have to know what a project is, and the two notions
/// cannot drift apart by being recorded twice.
///
/// ## Universal, hence `text-mode`
///
/// Owned by no single mode, like `text-mode.extra-highlights`: any buffer may
/// have one and most do not. Written through
/// [`ModeActivator::set_buffer_scope_dir`](crate::ModeActivator::set_buffer_scope_dir)
/// so a provider sets it from its trigger, where it already knows the answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BufferScopeDir(pub std::path::PathBuf);

impl BufferLocal for BufferScopeDir {
    const NAME: &'static str = "text-mode.scope-dir";
    const DOC: &'static str = "The directory this buffer is about, for a buffer that is not \
         itself a file — a magit repository, an oil listing's directory, a \
         file tree's root, a search or agenda view's scan root. Project \
         resolution (`:files`, `:search`) reads it before falling back to \
         the buffer's own path, and then to the working directory.";
    const OWNER_MODE: &'static str = "text-mode";
    fn describe(&self) -> String {
        self.0.display().to_string()
    }
}

/// A provider's answer to "which directory is the buffer *called this*
/// about?", asked by the host the moment it creates a synthetic buffer.
///
/// ## Why by name, and why a pull rather than a push
///
/// A provider that opens its buffer with `Effect::OpenSyntheticBuffer` never
/// touches the buffer: it returns a name and a mode id, and the host does the
/// rest. So it has no `BufferId` to attach a
/// [`BufferScopeDir`](crate::BufferScopeDir) to, and by the time its mode's
/// `on_activate` runs there is no `&mut` reach into the buffer-local map
/// either.
///
/// It does, however, know the directory *before* the buffer exists — that is
/// the whole reason magit's `RepoScopes` is keyed by name rather than by id.
/// So the host asks, at creation, with the one thing both sides have: the
/// name.
///
/// The alternative was a `scope_dir` field on `Effect::OpenSyntheticBuffer`,
/// which is ABI churn on a widely-constructed effect (and on its WIT peer) to
/// carry data most callers do not have.
pub trait BufferScopeSource: Send + Sync + std::fmt::Debug {
    /// The directory a buffer named `buffer_name` is about, if this source
    /// knows. `None` for a name it does not recognise — every source is asked
    /// and most will not know.
    fn scope_dir_for_name(&self, buffer_name: &str) -> Option<std::path::PathBuf>;
}

/// Registered [`BufferScopeSource`]s. A `Vec` rather than a single handle
/// because two providers naming buffers is normal and a single slot would let
/// the second registration silently displace the first.
#[derive(Default, Clone)]
pub struct BufferScopeSourceRegistry {
    sources: Vec<std::sync::Arc<dyn BufferScopeSource>>,
}

impl std::fmt::Debug for BufferScopeSourceRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BufferScopeSourceRegistry")
            .field("sources", &self.sources.len())
            .finish()
    }
}

impl BufferScopeSourceRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&mut self, source: std::sync::Arc<dyn BufferScopeSource>) {
        self.sources.push(source);
    }

    /// The first source that recognises `buffer_name`. First-answer-wins:
    /// two providers claiming one name is a naming collision they have to
    /// resolve between themselves, and picking arbitrarily is no worse than
    /// picking last.
    pub fn scope_dir_for_name(&self, buffer_name: &str) -> Option<std::path::PathBuf> {
        self.sources
            .iter()
            .find_map(|s| s.scope_dir_for_name(buffer_name))
    }

    pub fn len(&self) -> usize {
        self.sources.len()
    }

    pub fn is_empty(&self) -> bool {
        self.sources.is_empty()
    }
}

/// Register **and** look up with this exact alias (the `ServiceRegistry`
/// `TypeId` rule).
pub type BufferScopeSourceRegistryHandle =
    std::sync::Arc<arc_swap::ArcSwap<BufferScopeSourceRegistry>>;

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]
    use super::*;

    // Test fixture: one mode-owned local.
    #[derive(Clone)]
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
    #[derive(Clone)]
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
