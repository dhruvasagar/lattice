//! Where an oil listing's entries come from.
//!
//! Oil is a view of *a* directory tree, and the filesystem is only the
//! default one. A directory at a git revision is the other case worth
//! having — fugitive's `:Gedit <rev>:<dir>/`, where `-` from a file at a
//! revision lists its directory *at that revision* and `<CR>` keeps you
//! there — and it differs from the filesystem in exactly the places this
//! trait names: where the entries come from, what opening a file means,
//! whether the listing can be written, and where the top is.
//!
//! The source is per buffer ([`OilSourceLocal`]), so everything that
//! happens inside a listing — `<CR>` into a directory, `-` to the parent,
//! a re-list — stays in the source the listing was opened from without
//! being told. A buffer with no source local is a filesystem listing,
//! which is every oil buffer that existed before this seam.
//!
//! See `docs/dev/architecture/directory-listing-mode.md` §"Listing
//! sources".

use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use lattice_core::ui::pane::OpenTarget;
use lattice_grammar::Effect;
use lattice_mode::BufferLocal;

use super::OilEntry;

/// A tree an oil buffer can list.
///
/// Directories are absolute paths for every source, including ones that
/// are not the filesystem: a revision's tree is addressed by the path its
/// directory would have in the working tree. That keeps the parts of oil
/// that only move between directories — joining an entry onto the
/// listed directory, stepping to its parent, landing on the child you
/// came from — identical across sources.
pub trait OilSource: Send + Sync + fmt::Debug {
    /// Identity. Two listings of one directory are the same buffer only
    /// when their sources share an id, so `-` from a file at a revision
    /// never reuses the working tree's listing of that directory.
    fn id(&self) -> &str;

    /// The entries of `dir`, directories first and then by name — the
    /// order [`crate::dir::read_dir_sorted`] gives the filesystem.
    fn list(&self, dir: &Path) -> std::io::Result<Vec<OilEntry>>;

    /// How `dir` reads in the echo area and, behind `[oil] `, in the
    /// modeline.
    fn display(&self, dir: &Path) -> String {
        dir.display().to_string()
    }

    /// Why the listing cannot be changed, or `None` when it can. A
    /// read-only listing refuses edits and `:w`, and says this.
    fn read_only(&self) -> Option<&str> {
        None
    }

    /// The directory `-` steps up to from `dir`. `None` is the top of the
    /// tree, where `-` says so instead of moving.
    fn parent(&self, dir: &Path) -> Option<PathBuf> {
        dir.parent().map(Path::to_path_buf)
    }

    /// Open the entry at `path`, a file in place (`target: None`) or any
    /// entry in a split, vsplit or tab. A directory in place is not asked
    /// for: oil re-lists itself, in this source, without the source's
    /// help.
    fn open_entry(&self, path: &Path, is_dir: bool, target: Option<OpenTarget>) -> Effect;
}

/// The working tree on disk. Writable; `:w` turns edits into renames,
/// deletes and creates.
#[derive(Debug, Clone, Copy, Default)]
pub struct FsSource;

impl FsSource {
    /// [`OilSource::id`] of the filesystem.
    pub const ID: &'static str = "fs";
}

impl OilSource for FsSource {
    fn id(&self) -> &str {
        Self::ID
    }

    fn list(&self, dir: &Path) -> std::io::Result<Vec<OilEntry>> {
        super::read_dir_entries(dir)
    }

    fn open_entry(&self, path: &Path, _is_dir: bool, target: Option<OpenTarget>) -> Effect {
        match target {
            None => Effect::OpenBufferAt {
                path: Some(path.to_path_buf()),
                position: lattice_protocol::Position::ZERO,
                force: false,
                content: None,
                activate_minor: None,
            },
            // A directory path resolves to `DoEditOutcome::Directory` in
            // the new pane, which opens oil there — so files and
            // directories share the effect.
            Some(target) => Effect::OpenInTarget {
                path: Some(path.to_path_buf()),
                position: lattice_protocol::Position::ZERO,
                target,
            },
        }
    }
}

/// The source an oil buffer lists from. Absent means [`FsSource`].
#[derive(Debug, Clone)]
pub struct OilSourceLocal(pub Arc<dyn OilSource>);

impl BufferLocal for OilSourceLocal {
    const NAME: &'static str = "oil-mode.source";
    const DOC: &'static str = "Where the oil buffer's entries come from: the filesystem, or \
         a tree another subsystem supplies (a directory at a git revision). \
         `<CR>`, `-` and re-lists stay in this source.";
    const OWNER_MODE: &'static str = "oil-mode";
    fn describe(&self) -> String {
        self.0.id().to_string()
    }
}

/// Where `-` takes a buffer that is not a file: a directory, the source to
/// list it from, and the entry to land on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OilOrigin {
    /// The [`OilSource::id`] to list from, resolved by
    /// [`OilSourceRegistry::source`].
    pub source: String,
    /// The directory, absolute.
    pub dir: PathBuf,
    /// The entry the buffer *is*, to land on.
    pub focus: Option<String>,
}

/// A subsystem whose buffers have a place in a tree oil can list —
/// magit's `*magit:file:<rev>:<path>*` is `<path>`'s directory at
/// `<rev>`.
///
/// ## Why by buffer name, and two questions rather than one
///
/// `-` is oil's chord, live in every buffer (`oil-global-mode`), and its
/// handler is oil's. What oil cannot know is where a buffer it did not
/// make lives, so it asks — by name, the one thing a provider and oil
/// both have for a buffer the provider opened by `OpenSyntheticBuffer`
/// (the [`BufferScopeSource`](lattice_mode::BufferScopeSource) shape). The
/// answer travels as an [`Effect`], which carries data, not an
/// `Arc<dyn OilSource>`; so the source is named by id there and resolved
/// again when the host opens the listing.
pub trait OilSourceProvider: Send + Sync + fmt::Debug {
    /// Where `-` goes from the buffer named `buffer_name`, if this
    /// provider made it. Most names are not this provider's: `None`.
    fn origin_for(&self, buffer_name: &str) -> Option<OilOrigin>;

    /// The source with id `id`, if this provider makes such sources.
    fn source(&self, id: &str) -> Option<Arc<dyn OilSource>>;
}

/// The registered [`OilSourceProvider`]s. Asked in registration order;
/// the first answer wins, as for the scope sources.
#[derive(Debug, Default, Clone)]
pub struct OilSourceRegistry {
    providers: Vec<Arc<dyn OilSourceProvider>>,
}

impl OilSourceRegistry {
    /// An empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a provider.
    pub fn register(&mut self, provider: Arc<dyn OilSourceProvider>) {
        self.providers.push(provider);
    }

    /// Where `-` goes from the buffer named `buffer_name`.
    pub fn origin_for(&self, buffer_name: &str) -> Option<OilOrigin> {
        self.providers
            .iter()
            .find_map(|p| p.origin_for(buffer_name))
    }

    /// The source with id `id`. The filesystem is always known.
    pub fn source(&self, id: &str) -> Option<Arc<dyn OilSource>> {
        if id == FsSource::ID {
            return Some(Arc::new(FsSource));
        }
        self.providers.iter().find_map(|p| p.source(id))
    }
}

/// The registry service. Register **and** look up with this exact alias
/// (the `ServiceRegistry` `TypeId` rule). Published by the host ahead of
/// every subsystem `install`, so a provider can always register.
pub type OilSourceRegistryHandle = Arc<arc_swap::ArcSwap<OilSourceRegistry>>;

/// The modeline label of an oil buffer listing `dir` from `source`.
pub fn source_label_for(source: &dyn OilSource, dir: &Path) -> String {
    format!("[oil] {}", source.display(dir))
}

/// The source `local` names, or the filesystem.
pub fn source_or_fs(local: Option<&OilSourceLocal>) -> Arc<dyn OilSource> {
    match local {
        Some(local) => local.0.clone(),
        None => Arc::new(FsSource),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn a_buffer_without_a_source_lists_the_filesystem() {
        assert_eq!(source_or_fs(None).id(), FsSource::ID);
    }

    #[test]
    fn the_filesystem_opens_a_file_in_place_and_anything_in_a_target() {
        let path = Path::new("/r/a.txt");
        assert!(matches!(
            FsSource.open_entry(path, false, None),
            Effect::OpenBufferAt { path: Some(p), .. } if p == path
        ));
        assert!(matches!(
            FsSource.open_entry(path, true, Some(OpenTarget::VSplit)),
            Effect::OpenInTarget { path: Some(p), target: OpenTarget::VSplit, .. } if p == path
        ));
    }

    #[test]
    fn the_filesystem_is_writable_and_tops_out_at_the_root() {
        assert_eq!(FsSource.read_only(), None);
        assert_eq!(
            FsSource.parent(Path::new("/r/src")),
            Some(PathBuf::from("/r"))
        );
        assert_eq!(FsSource.parent(Path::new("/")), None);
    }
}
