//! MR.2: which repository each magit buffer is acting on.
//!
//! A magit buffer's name carries the repository's *basename* because
//! that is what a user recognises in `:ls`. A basename cannot round-trip
//! to a path and two checkouts can share one, so the name is not the
//! source of truth — this is (design §3.1). The trigger resolves the
//! repository and records it here; the view reads it back when it
//! activates, and (MR.4) every action body in that buffer reads it
//! instead of re-resolving.
//!
//! **Keyed by buffer name, not id.** The trigger runs *before* the
//! buffer exists — that is the whole reason a side channel is needed at
//! all (`on_activate` cannot see what the trigger saw) — so there is no
//! id to key on yet. `BufferStore::name_for` then makes id → name →
//! workdir a lookup rather than a second map to keep in sync.
//!
//! **Not one-shot.** `ViewArgsRequests` and `BlameRequests`, the two
//! side channels this shape comes from, are *taken* on activation
//! because a request is for one activation. This one is read for the
//! buffer's whole life: `s` in a status buffer stages into the repo the
//! buffer is showing, every time it is pressed, or the buffer is worse
//! than it was before MR.2 (design §4).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use lattice_protocol::ids::DocumentId;

/// Buffer name → the repository that buffer acts on.
///
/// The `DocumentId` index is not redundant with the name map:
/// `Event::DocumentClosed` carries a `DocumentId`, and the two are not
/// interchangeable — the same reason `ProjectDiffService` keeps its own
/// `by_document`.
#[derive(Default)]
pub struct RepoScopes {
    by_name: Mutex<HashMap<String, PathBuf>>,
    by_document: Mutex<HashMap<DocumentId, String>>,
}

impl std::fmt::Debug for RepoScopes {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RepoScopes")
            .field("tracked", &self.tracked())
            .finish_non_exhaustive()
    }
}

impl RepoScopes {
    /// Record (or re-point) the repository `name` acts on.
    ///
    /// Overwrites rather than accumulating: re-triggering `C-x g` for a
    /// repository must find the buffer you already have, not stack a
    /// second record behind it.
    pub fn record(&self, name: impl Into<String>, workdir: PathBuf) {
        if let Ok(mut m) = self.by_name.lock() {
            m.insert(name.into(), workdir);
        }
    }

    /// The repository `name` acts on, if one was recorded.
    ///
    /// `None` is a real answer, not a bug: a magit buffer reopened by
    /// `:b` after a restart has a name but no record, and the view falls
    /// back to resolving from scratch.
    pub fn workdir_for(&self, name: &str) -> Option<PathBuf> {
        self.by_name.lock().ok()?.get(name).cloned()
    }

    /// MR.3b: the repository behind a *label*, recovered from any magit
    /// buffer already recorded against it.
    ///
    /// This is what lets a view opened from *inside* another magit
    /// buffer — `<CR>` on a commit in the log, a file at a revision —
    /// name itself correctly without reaching for services it does not
    /// have. Those producers sit in helpers holding only their own
    /// buffer's state, so all they can carry across is the label their
    /// own name already spells; this turns that label back into a path.
    ///
    /// Sound because labels are unique among *open* magit buffers by
    /// construction: two checkouts sharing a basename qualify at the
    /// trigger ([`RepoScopes::collides`]) precisely so that one label
    /// never names two repositories at once.
    pub fn workdir_for_label(&self, label: &str) -> Option<PathBuf> {
        let map = self.by_name.lock().ok()?;
        map.iter()
            .find(|(name, _)| {
                crate::workdir::parse_magit_name(name).and_then(|n| n.repo) == Some(label)
            })
            .map(|(_, workdir)| workdir.clone())
    }

    /// Is `name` already recorded against a *different* repository?
    ///
    /// The collision question, asked by the trigger before it settles on
    /// a name. Merging two repositories into one buffer is the worst
    /// outcome available here — the staging chords would act on whichever
    /// was recorded last (design §3.1).
    pub fn collides(&self, name: &str, workdir: &Path) -> bool {
        self.workdir_for(name)
            .is_some_and(|recorded| recorded != workdir)
    }

    /// Index the document behind `name`, so closing the buffer drops the
    /// record. Called by the view when it activates — the first moment
    /// the document exists.
    pub fn index_document(&self, document: DocumentId, name: impl Into<String>) {
        if let Ok(mut m) = self.by_document.lock() {
            m.insert(document, name.into());
        }
    }

    /// Cleanup entry point for the `DocumentClosed` subscriber.
    ///
    /// Returns whether anything was dropped, which is what makes the
    /// wiring testable without reaching into the maps.
    pub fn forget_by_document_id(&self, document: DocumentId) -> bool {
        let name = match self.by_document.lock() {
            Ok(mut m) => m.remove(&document),
            Err(_) => None,
        };
        match name {
            Some(name) => {
                if let Ok(mut m) = self.by_name.lock() {
                    m.remove(&name);
                }
                true
            }
            None => false,
        }
    }

    /// How many buffers have a recorded repository. For tests and
    /// `Debug`; the accumulation failure mode is only visible as a count.
    pub fn tracked(&self) -> usize {
        self.by_name.lock().map(|m| m.len()).unwrap_or(0)
    }
}

/// Typed handle for `ServiceRegistry` lookup — register and look up
/// under THIS alias (`feedback_servicesregistry_arc_typeid`).
pub type RepoScopesHandle = Arc<RepoScopes>;

/// MR.2: the single path from "a magit trigger fired" to "the buffer it
/// opens" — resolve the repository, name the buffer for it, record
/// which repository that buffer acts on.
///
/// **Both surfaces call exactly this.** `C-x g` reaches it from an
/// action handler (which has services and a buffer id) and
/// `:magit-status` from an ex-command closure (which has a buffer id and
/// a handle it captured at boot). They had different reach before MR.2
/// and letting them diverge was never on the table: the same command
/// meaning two things depending on how it was reached is worse than
/// either meaning on its own.
///
/// `active` is the buffer the trigger fired in — `ExCommandContext::
/// buffer_id` or `ActionContext::buffer_id`, which are the same fact
/// from the same dispatch.
pub fn open_repo_view(
    view: &str,
    mode_id: &str,
    store: &lattice_mode::BufferStoreHandle,
    scopes: &RepoScopes,
    active: lattice_core::BufferId,
) -> lattice_grammar::Effect {
    lattice_grammar::Effect::OpenSyntheticBuffer {
        name: repo_view_name(view, store, scopes, active),
        mode_id: mode_id.to_string(),
    }
}

/// MR.3: the repository a magit view acts on, read at activation — the
/// other end of what [`open_repo_view`] wrote.
///
/// Every view's `on_activate` asks exactly this, and asks it the same
/// way: the record under this buffer's name, else the working directory.
/// The fallback is not defensive padding — a magit buffer reopened by
/// `:b` after a restart has a name and no record, and the working
/// directory is the answer magit gave for that buffer before MR.2.
///
/// Also indexes the document, so closing the buffer drops the record.
/// Here rather than at the trigger because this is the first moment the
/// document exists — and in the same helper as the read so a new view
/// cannot pick up one half and forget the other.
pub fn view_workdir(
    ctx: &lattice_mode::ModeContext,
    buffer: lattice_core::BufferId,
    handle: &std::sync::Arc<dyn lattice_runtime::Document>,
) -> Option<PathBuf> {
    let name = ctx
        .service::<lattice_mode::BufferStoreHandle>()
        .and_then(|store| store.name_for(buffer));
    let scopes = ctx.service::<RepoScopesHandle>();

    if let (Some(scopes), Some(name)) = (scopes.as_ref(), name.as_ref()) {
        scopes.index_document(handle.id(), name.clone());
        if let Some(recorded) = scopes.workdir_for(name) {
            return Some(recorded);
        }
        // MR.3b: no record, but the name carries a label — this buffer
        // was opened from inside another magit buffer, by a producer
        // that had the label and no way to record a path. Recover the
        // path from whichever sibling IS recorded against that label,
        // and record it here so the buffer's own actions (MR.4) can read
        // it like any other.
        if let Some(recovered) = crate::workdir::parse_magit_name(name)
            .and_then(|n| n.repo)
            .and_then(|label| scopes.workdir_for_label(label))
        {
            scopes.record(name.clone(), recovered.clone());
            return Some(recovered);
        }
    }
    crate::workdir::magit_workdir()
}

/// MR.3b: the repository label a magit buffer's own name carries, for a
/// producer that has the buffer store but no services.
///
/// Empty when the buffer is not a magit buffer or carries no label —
/// which composes correctly with the name producers, since an empty
/// label is the outside-a-repository form.
pub fn label_of_buffer(
    store: &lattice_mode::BufferStoreHandle,
    buffer: lattice_core::BufferId,
) -> String {
    store
        .name_for(buffer)
        .and_then(|name| {
            crate::workdir::parse_magit_name(&name).and_then(|n| n.repo.map(str::to_string))
        })
        .unwrap_or_default()
}

/// MR.3: [`open_repo_view`] for a view that encodes parameters of its
/// own — the commit family's target (`*magit:augment:<repo>:<sha>*`) and,
/// from MR.3b, the path- and revision-scoped views.
///
/// `rest` is the view's own encoding, verbatim; this function only puts
/// the repository in front of it.
pub fn open_repo_view_with(
    view: &str,
    mode_id: &str,
    rest: &str,
    store: &lattice_mode::BufferStoreHandle,
    scopes: &RepoScopes,
    active: lattice_core::BufferId,
) -> lattice_grammar::Effect {
    lattice_grammar::Effect::OpenSyntheticBuffer {
        name: repo_view_name_with(view, Some(rest), store, scopes, active),
        mode_id: mode_id.to_string(),
    }
}

/// The naming half of [`open_repo_view`], split out so a test can assert
/// which buffer a trigger lands on without an `Effect` in the way.
pub fn repo_view_name(
    view: &str,
    store: &lattice_mode::BufferStoreHandle,
    scopes: &RepoScopes,
    active: lattice_core::BufferId,
) -> String {
    repo_view_name_with(view, None, store, scopes, active)
}

/// Resolve the repository, compose the name, record what the buffer acts
/// on. The single body under every magit trigger.
pub fn repo_view_name_with(
    view: &str,
    rest: Option<&str>,
    store: &lattice_mode::BufferStoreHandle,
    scopes: &RepoScopes,
    active: lattice_core::BufferId,
) -> String {
    use crate::workdir;

    let compose = |label: &str| match rest {
        Some(rest) => workdir::magit_buffer_name_with(view, label, rest),
        None => workdir::magit_buffer_name(view, label),
    };

    // Question 1 (design §2): the active buffer is itself a magit
    // buffer, so it already knows which repository it is showing.
    // Reading it from the record rather than re-resolving is what stops
    // `C-x g` inside repo B's log from walking you back to the cwd repo.
    let from_magit_buffer = store
        .name_for(active)
        .filter(|name| workdir::is_magit_buffer_name(name))
        .and_then(|name| scopes.workdir_for(&name));
    // Question 2: the file in front of you. Question 3 (the working
    // directory) is inside the resolver.
    let active_file = store.path_for(active);

    let Some(repo) = workdir::repo_for_trigger(from_magit_buffer, active_file.as_deref()) else {
        // Not in a repository from any of the three directions. The
        // unqualified name is what magit always used, and the view says
        // "Not a git repository." exactly as it did before.
        return compose("");
    };

    let mut name = compose(&workdir::repo_label(&repo));
    if scopes.collides(&name, &repo) {
        // Two checkouts sharing a basename. Qualifying is the only
        // outcome that is not "both repositories share one buffer".
        name = compose(&workdir::qualified_repo_label(&repo));
    }
    scopes.record(name.clone(), repo);
    name
}

/// A `BufferStore` that knows only what a trigger asks it: what the
/// active buffer is called and which file it holds.
///
/// Those are the two questions [`repo_view_name`] puts to the store, so
/// stubbing the rest keeps a trigger test about resolution rather than
/// about standing up a buffer registry. Shared with the ex-command
/// registration tests, which need *a* store handle and do not care what
/// is in it.
#[cfg(test)]
pub(crate) mod test_support {
    use std::path::PathBuf;
    use std::sync::Arc;

    use lattice_mode::BufferStoreHandle;

    #[derive(Default)]
    pub(crate) struct StubStore {
        pub name: Option<String>,
        pub path: Option<PathBuf>,
    }

    impl lattice_mode::BufferStore for StubStore {
        fn find_by_name(&self, _name: &str) -> Option<lattice_core::BufferId> {
            None
        }
        fn handle_for(
            &self,
            _id: lattice_core::BufferId,
        ) -> Option<Arc<dyn lattice_runtime::Document>> {
            None
        }
        fn name_for(&self, _id: lattice_core::BufferId) -> Option<String> {
            self.name.clone()
        }
        fn path_for(&self, _id: lattice_core::BufferId) -> Option<PathBuf> {
            self.path.clone()
        }
        fn insert_document_buffer(
            &self,
            _id: lattice_core::BufferId,
            _kind: lattice_core::BufferKind,
            _handle: Arc<dyn lattice_runtime::Document>,
            _flags: lattice_core::BufferFlags,
            _name: Option<String>,
        ) {
        }
    }

    /// A store holding nothing — the "no file, no name" buffer.
    pub(crate) fn empty_store() -> BufferStoreHandle {
        BufferStoreHandle::new(Arc::new(StubStore::default()))
    }

    pub(crate) fn store_showing(name: Option<&str>, path: Option<PathBuf>) -> BufferStoreHandle {
        BufferStoreHandle::new(Arc::new(StubStore {
            name: name.map(str::to_string),
            path,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn doc(n: u64) -> DocumentId {
        DocumentId::new(n)
    }

    /// The gap the record exists to cross: written by the trigger,
    /// before the buffer exists; read by the view, after it does.
    #[test]
    fn the_record_survives_the_trigger_to_activation_gap() {
        let scopes = RepoScopes::default();
        scopes.record("*magit:status:api*", PathBuf::from("/work/api"));

        assert_eq!(
            scopes.workdir_for("*magit:status:api*"),
            Some(PathBuf::from("/work/api"))
        );
    }

    /// Re-triggering must re-point the buffer you have, not stack a
    /// second record behind it — the accumulation is invisible except
    /// as a count, which is why the count is asserted.
    #[test]
    fn a_second_trigger_for_the_same_buffer_overwrites() {
        let scopes = RepoScopes::default();
        scopes.record("*magit:status:api*", PathBuf::from("/work/api"));
        scopes.record("*magit:status:api*", PathBuf::from("/work/api"));

        assert_eq!(scopes.tracked(), 1, "one buffer, one record");
    }

    #[test]
    fn closing_the_buffer_drops_the_record() {
        let scopes = RepoScopes::default();
        scopes.record("*magit:status:api*", PathBuf::from("/work/api"));
        scopes.index_document(doc(7), "*magit:status:api*");

        assert!(scopes.forget_by_document_id(doc(7)), "it was tracked");
        assert_eq!(scopes.workdir_for("*magit:status:api*"), None);
        assert_eq!(scopes.tracked(), 0);
        assert!(
            !scopes.forget_by_document_id(doc(7)),
            "and a second close has nothing to drop"
        );
    }

    /// A document nobody indexed — every non-magit buffer in the editor,
    /// closed all the time — must not disturb the records that exist.
    #[test]
    fn closing_an_unrelated_document_drops_nothing() {
        let scopes = RepoScopes::default();
        scopes.record("*magit:status:api*", PathBuf::from("/work/api"));
        scopes.index_document(doc(7), "*magit:status:api*");

        assert!(!scopes.forget_by_document_id(doc(99)));
        assert_eq!(scopes.tracked(), 1);
    }

    // ── MR.2: the trigger ────────────────────────────────────────

    use std::process::Command;
    use test_support::{empty_store, store_showing};

    fn git_init(dir: &Path) {
        let st = Command::new("git")
            .args(["init"])
            .current_dir(dir)
            .status()
            .expect("git");
        assert!(st.success(), "git init failed");
    }

    fn active() -> lattice_core::BufferId {
        lattice_core::BufferId(1)
    }

    /// The change, stated as the trigger sees it: a file from another
    /// checkout opens THAT checkout's status buffer, named for it, with
    /// the repository recorded against the name.
    ///
    /// The name and the record are asserted together because either one
    /// alone is a half-fix: the right name over the wrong workdir is a
    /// buffer that lies, and the right workdir under the shared name is
    /// two repositories in one buffer.
    #[test]
    fn a_file_from_another_checkout_opens_that_checkouts_buffer() {
        let dir = tempfile::tempdir().expect("tempdir");
        let repo = dir.path().join("api");
        std::fs::create_dir_all(repo.join("src")).unwrap();
        git_init(&repo);
        let file = repo.join("src").join("main.rs");
        std::fs::write(&file, "fn main() {}\n").unwrap();

        let scopes = RepoScopes::default();
        let store = store_showing(Some("src/main.rs"), Some(file));
        let name = repo_view_name("status", &store, &scopes, active());

        assert_eq!(name, "*magit:status:api*");
        assert_eq!(
            scopes
                .workdir_for(&name)
                .and_then(|w| w.canonicalize().ok()),
            repo.canonicalize().ok(),
            "the buffer must be recorded against the file's repo, not the cwd"
        );
    }

    /// Nothing open, or nothing with a file: the working directory, and
    /// the name magit always had. A fresh editor still answers `C-x g`,
    /// which is what keeps MR.2 a widening rather than a trade.
    #[test]
    fn with_no_file_the_trigger_falls_back_to_the_working_directory() {
        let scopes = RepoScopes::default();
        let store = empty_store();
        let name = repo_view_name("status", &store, &scopes, active());

        match crate::workdir::magit_workdir() {
            // The test process runs inside lattice's own checkout, so
            // this is the branch that fires here.
            Some(cwd) => {
                assert_eq!(
                    name,
                    crate::workdir::magit_buffer_name("status", &crate::workdir::repo_label(&cwd))
                );
                assert_eq!(scopes.workdir_for(&name), Some(cwd));
            }
            None => assert_eq!(name, "*magit:status*"),
        }
    }

    /// A magit chord pressed inside magit must not change which
    /// repository you are working on — question 1 of design §2, and the
    /// one that would otherwise walk you back to the cwd repo from repo
    /// B's own status buffer.
    ///
    /// The store here reports a file as well, and a file that resolves
    /// somewhere else: the point is that the magit buffer's record wins
    /// over it.
    #[test]
    fn a_trigger_inside_a_magit_buffer_stays_in_its_repository() {
        let dir = tempfile::tempdir().expect("tempdir");
        let other = dir.path().join("elsewhere");
        std::fs::create_dir_all(&other).unwrap();
        git_init(&other);
        let file = other.join("a.rs");
        std::fs::write(&file, "\n").unwrap();

        let scopes = RepoScopes::default();
        scopes.record("*magit:status:api*", PathBuf::from("/work/api"));
        let store = store_showing(Some("*magit:status:api*"), Some(file));

        let name = repo_view_name("status", &store, &scopes, active());
        assert_eq!(
            name, "*magit:status:api*",
            "the buffer in front of you decides"
        );
        assert_eq!(scopes.workdir_for(&name), Some(PathBuf::from("/work/api")));
    }

    /// Re-triggering for the same repository must land on the buffer you
    /// already have. Idempotence is not cosmetic here: a second name
    /// would open a second status buffer for one repository, and `gr` in
    /// either would refresh only itself.
    #[test]
    fn triggering_twice_for_one_repository_lands_on_one_buffer() {
        let scopes = RepoScopes::default();
        scopes.record("*magit:status:api*", PathBuf::from("/work/api"));
        let store = store_showing(Some("*magit:status:api*"), None);

        let first = repo_view_name("status", &store, &scopes, active());
        let second = repo_view_name("status", &store, &scopes, active());

        assert_eq!(first, second);
        assert_eq!(scopes.tracked(), 1, "one repository, one record");
    }

    /// Two checkouts sharing a basename get two buffers, not one. The
    /// merged outcome is the one that must not happen: `s` in the shared
    /// buffer would stage into whichever repo was recorded last, which
    /// is data-loss-shaped.
    #[test]
    fn a_second_repo_with_the_same_basename_gets_its_own_buffer() {
        let dir = tempfile::tempdir().expect("tempdir");
        let second = dir.path().join("oss").join("api");
        std::fs::create_dir_all(second.join("src")).unwrap();
        git_init(&second);
        let file = second.join("src").join("lib.rs");
        std::fs::write(&file, "\n").unwrap();

        let scopes = RepoScopes::default();
        // A *different* repository already holds the plain name.
        scopes.record("*magit:status:api*", PathBuf::from("/work/api"));

        let store = store_showing(Some("src/lib.rs"), Some(file));
        let name = repo_view_name("status", &store, &scopes, active());

        assert_ne!(name, "*magit:status:api*", "the two must not merge");
        assert!(
            name.starts_with("*magit:status:oss/"),
            "the qualified name names its parent directory: {name}"
        );
        assert_eq!(scopes.tracked(), 2, "two repositories, two records");
    }

    /// MR.3: every view that has moved resolves from the buffer in front
    /// of you, and each gets its OWN buffer per repository.
    ///
    /// Table-driven because the failure this guards is a view left
    /// behind: a conversion that does eight of nine, and the ninth still
    /// opening the working directory's repository — which looks correct
    /// from inside the repository you happen to be in, and is invisible
    /// until someone works across two.
    #[test]
    fn every_converted_view_resolves_from_the_buffer_it_was_triggered_in() {
        let dir = tempfile::tempdir().expect("tempdir");
        let repo = dir.path().join("api");
        std::fs::create_dir_all(repo.join("src")).unwrap();
        git_init(&repo);
        let file = repo.join("src").join("main.rs");
        std::fs::write(&file, "fn main() {}\n").unwrap();

        let scopes = RepoScopes::default();
        let store = store_showing(Some("src/main.rs"), Some(file));

        for view in [
            "status",
            "commit",
            "amend",
            "reword",
            "branch",
            "remote",
            "submodule",
            "refs",
        ] {
            let name = repo_view_name(view, &store, &scopes, active());
            assert_eq!(
                name,
                format!("*magit:{view}:api*"),
                "`{view}` must open the file's repository"
            );
            assert_eq!(
                scopes
                    .workdir_for(&name)
                    .and_then(|w| w.canonicalize().ok()),
                repo.canonicalize().ok(),
                "…and record it, or its `on_activate` reads the cwd back"
            );
        }
    }

    /// The commit family's targeted intents keep their target AND gain
    /// the repository, in that order: `*magit:augment:<repo>:<sha>*`.
    ///
    /// Both halves matter and they fail differently — losing the repo
    /// squashes into the wrong checkout, losing the target composes a
    /// squash for nothing.
    #[test]
    fn a_targeted_commit_buffer_carries_both_repo_and_target() {
        let dir = tempfile::tempdir().expect("tempdir");
        let repo = dir.path().join("api");
        std::fs::create_dir_all(&repo).unwrap();
        git_init(&repo);
        let file = repo.join("a.rs");
        std::fs::write(&file, "\n").unwrap();

        let scopes = RepoScopes::default();
        let store = store_showing(Some("a.rs"), Some(file));

        let name = repo_view_name_with("augment", Some("abc123"), &store, &scopes, active());
        assert_eq!(name, "*magit:augment:api:abc123*");
        assert_eq!(
            crate::magit_commit_mode::CommitIntent::from_buffer_name(&name),
            crate::magit_commit_mode::CommitIntent::Augment {
                target: "abc123".to_string()
            },
            "the intent must survive the repository being in the name"
        );
        assert!(scopes.workdir_for(&name).is_some(), "…and be recorded");
    }

    /// `C-x g` and `:magit-status` must land on the same buffer from the
    /// same place. This is the requirement MR.2 was asked for, and the
    /// one a future slice can quietly break: converting the ex-command
    /// to repo scoping while leaving the chord on the fixed name (or the
    /// reverse) leaves a magit that behaves differently depending on how
    /// you reached it, and neither half looks wrong on its own.
    ///
    /// Both are fired against ONE store and ONE record, so a divergence
    /// can only come from the resolution path itself.
    #[test]
    fn the_chord_and_the_ex_command_open_the_same_buffer() {
        use lattice_grammar::{Args, CommandRegistry, Effect};
        use lattice_mode::Mode;

        let dir = tempfile::tempdir().expect("tempdir");
        let repo = dir.path().join("api");
        std::fs::create_dir_all(repo.join("src")).unwrap();
        git_init(&repo);
        let file = repo.join("src").join("main.rs");
        std::fs::write(&file, "fn main() {}\n").unwrap();

        let scopes: RepoScopesHandle = Arc::new(RepoScopes::default());
        let store = store_showing(Some("src/main.rs"), Some(file));

        // The `:` surface, through the registry it is registered in.
        let mut registry = CommandRegistry::new();
        crate::register_ex_commands(
            &mut registry,
            Default::default(),
            store.clone(),
            scopes.clone(),
        );
        let id = registry
            .id_by_name("magit-status")
            .expect("`:magit-status` is registered");
        let spec = registry
            .ex_command_spec(id)
            .expect("`:magit-status` is an ex-command");
        let ex_ctx = lattice_grammar::ExCommandContext {
            bang: false,
            args: Args::None,
            range: None,
            register: Default::default(),
            count: Default::default(),
            buffer_id: active(),
            cancel: lattice_protocol::CancellationToken::never(),
        };
        let from_ex = (spec.apply)(&ex_ctx).expect("apply");

        // The chord surface, through the services a handler reads.
        // Registered under the exact aliases the handler looks up —
        // an `Arc<BufferStoreHandle>` here would be filed under a type
        // nobody asks for and the handler would fall back to the fixed
        // name, which is a passing-looking failure.
        let mut services = lattice_mode::ServiceRegistry::new();
        services.register(store);
        services.register::<RepoScopesHandle>(scopes);
        let events = lattice_runtime::EventBus::new();
        let handler = crate::magit_global_mode::MagitGlobalMode
            .action_handlers()
            .into_iter()
            .find(|c| c.action_name == "action:magit-global-status")
            .expect("`C-x g`'s handler is contributed")
            .handler;
        let from_chord = handler(&lattice_mode::ActionContext {
            buffer_id: lattice_protocol::ids::BufferId::new(active().0 as u64),
            cursor: lattice_protocol::position::Position::new(0, 0),
            selection: None,
            services: &services,
            events: &events,
            prompt_value: None,
            args: Args::None,
        })
        .expect("the chord opens something");

        match (&from_ex, &from_chord) {
            (
                Effect::OpenSyntheticBuffer { name: ex, .. },
                Effect::OpenSyntheticBuffer { name: chord, .. },
            ) => {
                assert_eq!(ex, chord, "the two surfaces must not diverge");
                assert_eq!(ex, "*magit:status:api*", "…on the file's repository");
            }
            other => panic!("both surfaces must open a synthetic buffer, got {other:?}"),
        }
    }

    /// The collision question is about the *path*, not the name: the
    /// same repository asked twice is not a collision (it is the
    /// idempotent re-trigger), two paths under one name is.
    #[test]
    fn a_collision_is_two_paths_under_one_name() {
        let scopes = RepoScopes::default();
        scopes.record("*magit:status:api*", PathBuf::from("/work/api"));

        assert!(
            !scopes.collides("*magit:status:api*", Path::new("/work/api")),
            "the same repo asked twice is the buffer you already have"
        );
        assert!(
            scopes.collides("*magit:status:api*", Path::new("/oss/api")),
            "a different repo under the same name must qualify instead"
        );
        assert!(
            !scopes.collides("*magit:status:lattice*", Path::new("/src/lattice")),
            "an unrecorded name collides with nothing"
        );
    }
}
