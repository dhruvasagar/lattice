//! MG.26b: `magit-blame-mode` — a **minor** mode that annotates the
//! buffer you are already looking at.
//!
//! **Why this is not a buffer.** The shape this replaces
//! (`*magit:blame:<path>*`) rendered the file as *text*, one
//! `<sha> <author>  <code>` row per line. That is why blame lost
//! syntax highlighting: the buffer stopped being the file, so there
//! was no language and no parser, and `blame_styled_spans` returned
//! nothing for the code column *by construction*. Every other editor
//! checked — magit, fugitive, Zed, GitLens, JetBrains — annotates the
//! real buffer, and highlighting survives because the file was never
//! replaced. Design:
//! [`../../../docs/dev/architecture/magit-blame.md`].
//!
//! **What it adds:** one virtual row above each chunk of lines sharing
//! a commit, carrying `<sha> <author> <date> <summary>` — magit's
//! `headings` style. Vertical cost instead of the horizontal cost a
//! per-line column would impose, so the code stays exactly where the
//! eye expects it and the commit is read once per chunk rather than
//! truncated onto every line.
//!
//! **The buffer goes read-only while blaming**, which is what re-frees
//! `<CR>` and `p` for blame use. A minor on an editable file buffer
//! cannot take grammar keys; magit resolves this the same way.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, RwLock};

use lattice_cells::{
    AnchorPosition, Cell, ProviderId, VirtualRow, VirtualRowKind, VirtualRowProvider,
};
use lattice_config;
use lattice_grammar::Effect;
use lattice_mode::{
    ActionContext, ActionHandlerContribution, ActivationPolicy, BufferStoreHandle, CapabilitySet,
    Keymap, KeymapEntry, LifecycleFuture, Mode, ModeContext, ModeId, ModeKind, OptionOverrideSet,
    VirtualRowRegistrar, keymap_entry,
};
use lattice_theme::{ElementId, ThemeRegistryHandle};
use lattice_vcs::Repository;

use crate::blame::{
    BlameChunk, Removal, RemovalCommit, heading_text, is_uncommitted, parse_blame_chunks,
};
use crate::buffer_state::{BufferStateGuard, BufferStates};

/// Provider id for the chunk-heading lane. One per buffer scope — a
/// buffer has at most one blame running on it.
pub const MAGIT_BLAME_PROVIDER_ID: ProviderId = 0x6d61_6769_745f_626c; // "magit_bl"

pub struct MagitBlameMode;

impl MagitBlameMode {
    pub fn mode_id() -> ModeId {
        ModeId::new("magit-blame-mode")
    }
}

fn magit_blame_keymap_entries() -> &'static [KeymapEntry] {
    static ENTRIES: OnceLock<Vec<KeymapEntry>> = OnceLock::new();
    ENTRIES.get_or_init(|| {
        vec![
            keymap_entry! { mode: Normal, chord: "<CR>", doc: "Show the commit for the chunk at cursor", cmd: "action:magit-blame-show-commit" },
            keymap_entry! { mode: Normal, chord: "p", doc: "Blame back one commit", cmd: "action:magit-blame-parent" },
            // `gq`, not magit's bare `q`. This mode can be active on a
            // *blob* buffer, where `magit-core-mode` is also active and
            // already binds `q` to close the buffer — two minors
            // binding one chord on one buffer resolves by registration
            // order, which is not a contract anyone should depend on.
            // `g` is a prefix, so `gq` shadows nothing (vim's `gq` is
            // the format operator, inert in a read-only buffer) and it
            // sits beside `gr` in the same namespace.
            keymap_entry! { mode: Normal, chord: "gq", doc: "Stop blaming (the buffer becomes editable again)", cmd: "action:magit-blame-quit" },
        ]
    })
}

/// MG.23f2: which question this blame answers.
///
/// Two directions, not two modes: the chunking, the headings and the
/// chords are identical and only the argv differs. **Mode state now,
/// not a buffer name** — a buffer was the only carrier the old shape
/// had, and it forced `p` in a reverse view to open a *new* buffer
/// rather than walk in place.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BlameDirection {
    /// `git blame <rev> -- <path>` — for each line, the commit that
    /// **introduced** it.
    #[default]
    Addition,
    /// `git blame --reverse <rev>..HEAD -- <path>` — for each line, the
    /// last commit in which it **still existed**. Lines annotated with
    /// something other than HEAD are the ones that have since gone
    /// away.
    Reverse,
}

/// The `git` argv for one blame run.
///
/// Pure and shared by the handler and the tests, so what the tests pin
/// is what runs.
///
/// `--` before the path is load-bearing rather than tidy: a path that
/// looks like a rev is otherwise ambiguous, and git resolves the
/// ambiguity in favour of the rev.
pub(crate) fn blame_argv(direction: BlameDirection, rev: &str, path: &str) -> Vec<String> {
    let mut argv = vec!["blame".to_string(), "--line-porcelain".to_string()];
    match direction {
        BlameDirection::Addition => argv.push(rev.to_string()),
        BlameDirection::Reverse => {
            argv.push("--reverse".to_string());
            // git accepts a bare `--reverse <rev>` and reads it as
            // `<rev>..HEAD`; spelling the range out says which end is
            // which at the call site and in the test.
            argv.push(format!("{rev}..HEAD"));
        }
    }
    argv.push("--".to_string());
    argv.push(path.to_string());
    argv
}

// ── The chunk-heading provider ───────────────────────────────────────

/// One virtual row above each blame chunk.
///
/// **No work per frame.** `collect` hands back rows built once, when a
/// blame run landed — the cells worker calls it on every rebuild, and
/// re-chunking or re-formatting there would be exactly the UI-thread
/// work paramount goal #1 forbids. Colours *are* resolved in `collect`,
/// which is one read-lock plus an `ArcSwap` load and is what
/// `MagitHeaderline::render` already does, so a `:colorscheme` repaints
/// the headings instead of leaving them on the old palette.
pub struct BlameProvider {
    chunks: RwLock<Arc<[BlameChunk]>>,
    /// Bumped when the chunks change. Folded with the theme's version
    /// in [`VirtualRowProvider::version`] so a palette swap repaints.
    version: AtomicU64,
    /// `None` in a harness with no theme registry — the fallback
    /// colours are used instead.
    theme: Option<ThemeRegistryHandle>,
    sha_element: Option<ElementId>,
    label_element: Option<ElementId>,
    /// Frozen once per blame run rather than read per frame: a heading
    /// that said "2 minutes ago" one frame and "3 minutes ago" the
    /// next would repaint the whole lane for nothing.
    now_secs: AtomicU64,
}

/// `ThemeRegistryHandle` is not `Debug`, and the provider trait wants
/// it — so the interesting state is printed and the handle is not.
impl std::fmt::Debug for BlameProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BlameProvider")
            .field("version", &self.version.load(Ordering::Acquire))
            .field("chunks", &self.chunks.read().map(|c| c.len()).unwrap_or(0))
            .finish()
    }
}

impl BlameProvider {
    fn new(theme: Option<ThemeRegistryHandle>, mode_id: &str) -> Arc<Self> {
        let (sha_element, label_element) = match &theme {
            Some(t) => {
                let (s, l) = crate::headerline::intern_blame_heading_elements(t, mode_id);
                (Some(s), Some(l))
            }
            None => (None, None),
        };
        Arc::new(Self {
            chunks: RwLock::new(Arc::from(Vec::new())),
            version: AtomicU64::new(0),
            theme,
            sha_element,
            label_element,
            now_secs: AtomicU64::new(0),
        })
    }

    /// Replace the chunks and repaint. `now_secs` is passed in rather
    /// than read here so the relative dates are testable.
    fn set_chunks(&self, chunks: Vec<BlameChunk>, now_secs: i64) {
        if let Ok(mut slot) = self.chunks.write() {
            *slot = Arc::from(chunks);
        }
        self.now_secs
            .store(now_secs.max(0) as u64, Ordering::Release);
        self.version.fetch_add(1, Ordering::Release);
    }

    fn colours(&self) -> (u32, u32) {
        let (fallback_sha, fallback_label) = crate::headerline::blame_heading_fallback();
        let Some(theme) = &self.theme else {
            return (fallback_sha, fallback_label);
        };
        let table = theme.resolved();
        let pick = |id: Option<ElementId>, fallback: u32| {
            id.and_then(|i| table.get(i).fg)
                .map(|c| c.to_rgb_u32(0))
                .unwrap_or(fallback)
        };
        (
            pick(self.sha_element, fallback_sha),
            pick(self.label_element, fallback_label),
        )
    }

    /// The chunk covering `line`, for the chunk-at-cursor chords.
    fn chunk_at(&self, line: u32) -> Option<BlameChunk> {
        let chunks = self.chunks.read().ok()?;
        chunks.iter().find(|c| c.contains(line)).cloned()
    }
}

impl VirtualRowProvider for BlameProvider {
    fn id(&self) -> ProviderId {
        MAGIT_BLAME_PROVIDER_ID
    }

    fn version(&self) -> u64 {
        let theme_version = self
            .theme
            .as_ref()
            .map(|t| t.resolved().version())
            .unwrap_or(0);
        self.version
            .load(Ordering::Acquire)
            .wrapping_add(theme_version)
    }

    fn collect(&self) -> Vec<VirtualRow> {
        let Ok(chunks) = self.chunks.read() else {
            return Vec::new();
        };
        let (sha_fg, label_fg) = self.colours();
        let now = self.now_secs.load(Ordering::Acquire) as i64;
        chunks
            .iter()
            .map(|chunk| {
                let text = heading_text(chunk, now);
                // The sha is its own colour so chunk boundaries are
                // scannable without reading the whole heading; an
                // uncommitted chunk has no sha to colour.
                let sha_len = if is_uncommitted(&chunk.sha) {
                    0
                } else {
                    text.chars().take_while(|c| !c.is_whitespace()).count()
                };
                let cells: Vec<Cell> = text
                    .chars()
                    .enumerate()
                    .map(|(i, c)| {
                        let fg = if i < sha_len { sha_fg } else { label_fg };
                        Cell::new(c as u32, fg, 0, 0)
                    })
                    .collect();
                VirtualRow {
                    anchor_line: chunk.start_line,
                    position: AnchorPosition::Above,
                    cells: Arc::from(cells),
                    height: 1,
                    kind: VirtualRowKind::Annotation,
                    bg: None,
                    scales: None,
                    gutter_line: None,
                    gutter_fg: None,
                }
            })
            .collect()
    }
}

/// Drops the provider registration when the mode deactivates — the
/// headings must go away with the blame, not outlive it.
pub struct BlameRegistration {
    registrar: Arc<dyn VirtualRowRegistrar>,
    buffer: lattice_core::BufferId,
}

impl Drop for BlameRegistration {
    fn drop(&mut self) {
        self.registrar
            .unregister(self.buffer, MAGIT_BLAME_PROVIDER_ID);
    }
}

// ── Per-buffer state ─────────────────────────────────────────────────

pub struct BlameState {
    workdir: std::path::PathBuf,
    /// Repo-relative path being blamed.
    path: String,
    /// The revision currently blamed — `p` walks this back to its
    /// parent **in place**, which is what direction-as-state buys.
    rev: String,
    direction: BlameDirection,
    provider: Arc<BlameProvider>,
    /// How a finished blame reaches the screen.
    ///
    /// **Bumping the provider's version is NOT a wake**, which is what
    /// the first version of this mode assumed. The cells worker
    /// re-reads providers when the editor is already redrawing; nothing
    /// *starts* a redraw. So the headings sat there until the user
    /// happened to press a key — "it works, but only after I hit
    /// something", the exact symptom `CLAUDE.md` describes and which a
    /// user reported here. `wake()` fires the waker without storing
    /// anything, which is the idiom for precisely this.
    pending_highlights: Option<lattice_mode::PendingSyntheticHighlightsHandle>,
    /// Kept so the guard's drop tears the heading lane down.
    _registration: Option<BlameRegistration>,
}

pub type BlameStatesHandle = Arc<BufferStates<BlameState>>;

fn state(ctx: &ActionContext<'_>) -> Option<Arc<Mutex<BlameState>>> {
    crate::buffer_state::state_for::<BlameState>(ctx)
}

/// MG.26b: what a *pending* blame should be, keyed by the buffer name
/// it will land on.
///
/// `Effect::ToggleMode` carries only a mode name, which is right — the
/// grammar crate must not learn about blame directions. So an action
/// that wants a non-default blame (reverse, or a specific revision)
/// leaves the request here first, and `on_activate` consumes it. Keyed
/// by *name* rather than `BufferId` because the buffer may not exist
/// yet: the reverse path opens a blob buffer and activates the mode on
/// it in one `Effect::Many`.
#[derive(Default)]
pub struct BlameRequests {
    map: Mutex<std::collections::HashMap<String, (BlameDirection, String)>>,
}

pub type BlameRequestsHandle = Arc<BlameRequests>;

impl BlameRequests {
    pub fn put(&self, buffer_name: String, direction: BlameDirection, rev: String) {
        if let Ok(mut m) = self.map.lock() {
            m.insert(buffer_name, (direction, rev));
        }
    }

    /// Read and remove — a request is for one activation. Leaving it
    /// would make the *next* plain `:magit-blame` on the same buffer
    /// silently reverse.
    pub fn take(&self, buffer_name: &str) -> Option<(BlameDirection, String)> {
        self.map.lock().ok()?.remove(buffer_name)
    }
}

impl Mode for MagitBlameMode {
    type Guard = BufferStateGuard<BlameState>;

    fn id(&self) -> ModeId {
        Self::mode_id()
    }
    fn kind(&self) -> ModeKind {
        ModeKind::Minor
    }

    /// Never automatic: blame is something you ask for on a buffer you
    /// are already reading. `Effect::ToggleMode` is the seam.
    fn activation_policy(&self) -> ActivationPolicy {
        ActivationPolicy::Manual
    }

    fn target_buffer_kind(&self) -> Option<lattice_core::BufferKind> {
        None
    }

    /// Read-only for the duration, which is what re-frees `<CR>`, `p`
    /// and `q` — a minor on an editable buffer cannot take grammar
    /// keys. The override reverts when the mode deactivates, so the
    /// file is editable again the moment blame stops.
    fn options(&self) -> OptionOverrideSet {
        lattice_config::overrides! {
            lattice_config::ReadOnly = true,
        }
    }

    fn required_capabilities(&self) -> CapabilitySet {
        CapabilitySet::empty()
    }
    fn keymap(&self) -> Keymap {
        Keymap::from_entries(magit_blame_keymap_entries())
    }

    fn action_handlers(&self) -> Vec<ActionHandlerContribution> {
        vec![
            // <CR> — the commit for the chunk under the cursor.
            //
            // Resolved from the stored chunks, not by reading the
            // buffer: the buffer is the user's own code now, and it
            // carries no sha to parse. That is the whole point.
            ActionHandlerContribution {
                action_name: "action:magit-blame-show-commit",
                handler: Arc::new(|ctx: &ActionContext<'_>| {
                    let s = state(ctx)?;
                    let g = s.lock().ok()?;
                    let chunk = g.provider.chunk_at(ctx.cursor.line)?;
                    if is_uncommitted(&chunk.sha) {
                        return Some(Effect::Echo {
                            level: lattice_grammar::EchoLevel::Info,
                            text: "magit: this line is not committed yet".to_string(),
                        });
                    }
                    Some(crate::magit_global_mode::open_repo_view_from_action_with(
                        ctx,
                        crate::magit_revision_mode::SHOW_VIEW,
                        "magit-revision-mode",
                        Some(&chunk.sha),
                    ))
                }),
            },
            // gq — stop blaming. Deactivating the mode is what removes
            // the headings and gives the buffer back its editability;
            // there is no buffer to close any more.
            ActionHandlerContribution {
                action_name: "action:magit-blame-quit",
                handler: Arc::new(|ctx: &ActionContext<'_>| {
                    let _ = state(ctx)?;
                    Some(Effect::ToggleMode {
                        mode_name: MagitBlameMode::mode_id().as_str().to_string(),
                    })
                }),
            },
            // p — re-blame at the parent of the revision currently
            // blamed, IN PLACE. The old shape had to open a new buffer
            // in the reverse direction because the direction lived in
            // the buffer's name; state has no such constraint.
            ActionHandlerContribution {
                action_name: "action:magit-blame-parent",
                handler: Arc::new(|ctx: &ActionContext<'_>| {
                    let s = state(ctx)?;
                    let (wd, rev) = {
                        let g = s.lock().ok()?;
                        (g.workdir.clone(), g.rev.clone())
                    };
                    let s2 = s.clone();
                    tokio::task::spawn(async move {
                        let wd2 = wd.clone();
                        let rev2 = rev.clone();
                        let parent =
                            tokio::task::spawn_blocking(move || resolve_parent(&wd2, &rev2))
                                .await
                                .ok()
                                .flatten();
                        let Some(parent) = parent else {
                            tracing::debug!(
                                target: "lattice_magit",
                                "blame: {rev} has no parent — already at the root commit",
                            );
                            return;
                        };
                        if let Ok(mut g) = s2.lock() {
                            g.rev = parent;
                        }
                        rerun_blame(s2).await;
                    });
                    None
                }),
            },
        ]
    }

    fn on_activate(&self, ctx: ModeContext) -> LifecycleFuture<'_, Self::Guard> {
        Box::pin(async move {
            let buffer_id = lattice_core::BufferId(ctx.buffer_id().0 as u32);
            let orphan = || BufferStateGuard::new(Arc::new(BufferStates::default()), buffer_id);
            let Some(store) = ctx.service::<BufferStoreHandle>() else {
                return Ok(orphan());
            };

            // What to blame: the buffer's own file, or — for the blob
            // buffer the reverse path opens — the path in its name.
            let buffer_name = store.name_for(buffer_id).unwrap_or_default();
            let scopes = ctx.service::<crate::repo_scope::RepoScopesHandle>();
            let Some((workdir, path)) = blame_target(
                &store,
                scopes.as_deref().map(|s| &**s),
                buffer_id,
                &buffer_name,
            ) else {
                // A scratch buffer, or a file outside any repository.
                // Nothing to annotate; the mode activates as a no-op
                // rather than failing, and `q` turns it back off.
                return Ok(orphan());
            };

            // A pending request (reverse, or a specific revision) wins
            // over the defaults; a plain toggle has none.
            let (direction, rev) = ctx
                .service::<BlameRequestsHandle>()
                .and_then(|r| r.take(&buffer_name))
                .unwrap_or((BlameDirection::Addition, "HEAD".to_string()));

            let theme = ctx
                .service::<ThemeRegistryHandle>()
                .map(|outer| (*outer).clone());
            let provider = BlameProvider::new(theme, Self::mode_id().as_str());

            let registration = ctx
                .service::<Arc<dyn VirtualRowRegistrar>>()
                .map(|outer| (*outer).clone())
                .map(|registrar| {
                    // `register` refuses to replace a live id, so clear
                    // whatever a previous activation left behind.
                    registrar.unregister(buffer_id, MAGIT_BLAME_PROVIDER_ID);
                    registrar.register(buffer_id, provider.clone() as Arc<dyn VirtualRowProvider>);
                    BlameRegistration {
                        registrar,
                        buffer: buffer_id,
                    }
                });

            // MG.13: publish BEFORE the first `.await`.
            let Some(states) = ctx.service::<BlameStatesHandle>() else {
                return Ok(orphan());
            };
            let state = states.publish(
                buffer_id,
                BlameState {
                    workdir: workdir.clone(),
                    path: path.clone(),
                    rev: rev.clone(),
                    direction,
                    provider: provider.clone(),
                    pending_highlights: ctx.service::<lattice_mode::PendingSyntheticHighlights>(),
                    _registration: registration,
                },
            );
            let guard = BufferStateGuard::new((*states).clone(), buffer_id);

            rerun_blame(state).await;
            Ok(guard)
        })
    }
}

/// Where the file being blamed lives, as `(workdir, repo-relative path)`.
///
/// Two sources, in order. A real file buffer has a path. A blob buffer
/// (`*magit:file:<rev>:<path>*`) has none — it is synthetic — but
/// carries its path in its name, which is the same trick
/// `magit-file-revision-mode` and `lattice-multibuffer` use.
fn blame_target(
    store: &BufferStoreHandle,
    scopes: Option<&crate::repo_scope::RepoScopes>,
    buffer_id: lattice_core::BufferId,
    buffer_name: &str,
) -> Option<(std::path::PathBuf, String)> {
    if let Some(p) = store.path_for(buffer_id) {
        let (workdir, rel) = crate::workdir::workdir_for_file(&p)?;
        return Some((workdir, rel.to_string_lossy().into_owned()));
    }
    // MR.4: a blob buffer has no file on disk, so its repository comes
    // from the name it was opened under rather than from the process.
    let rel = crate::magit_file_revision_mode::parse_buffer_name(buffer_name)?.1;
    let workdir = scopes
        .and_then(|s| {
            s.workdir_for(buffer_name).or_else(|| {
                crate::workdir::parse_magit_name(buffer_name)
                    .and_then(|n| n.repo)
                    .and_then(|label| s.workdir_for_label(label))
            })
        })
        .or_else(crate::workdir::magit_workdir)?;
    Some((workdir, rel.to_string_lossy().into_owned()))
}

/// Run the blame this state describes and hand the chunks to its
/// provider.
///
/// Off the actor thread on `spawn_blocking`, and the result reaches the
/// screen with no keypress — but **only because of the explicit wake at
/// the end**. Bumping the provider's version is not a wake: the cells
/// worker re-reads providers when a redraw is already happening, and
/// nothing starts one. Nothing here touches the buffer's text.
async fn rerun_blame(s: Arc<Mutex<BlameState>>) {
    let Some((wd, direction, rev, path, provider, wake)) = ({
        let g = s.lock().ok();
        g.map(|g| {
            (
                g.workdir.clone(),
                g.direction,
                g.rev.clone(),
                g.path.clone(),
                g.provider.clone(),
                g.pending_highlights.clone(),
            )
        })
    }) else {
        return;
    };
    // MG.33: the removal walk runs in the SAME `spawn_blocking` as the
    // blame. Splitting it would publish headings once without the
    // answer and again with it — a visible relabel of rows the user did
    // not touch, which the keystroke UX contract forbids.
    let chunks = tokio::task::spawn_blocking(move || {
        let mut chunks = parse_blame_chunks(&run_blame(&wd, direction, &rev, &path));
        if direction == BlameDirection::Reverse {
            resolve_removals(&wd, &path, &mut chunks);
        }
        chunks
    })
    .await
    .unwrap_or_default();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    provider.set_chunks(chunks, now);
    // The headings exist now; ask for a frame. Without this they appear
    // on the next keystroke instead of when the blame lands.
    if let Some(wake) = wake {
        wake.wake();
    }
}

/// MG.33: resolve what removed the lines a reverse-blame chunk covers.
///
/// **Blocking — call on `spawn_blocking`.** Up to two `git` invocations
/// per distinct blamed SHA, so callers dedupe by SHA first
/// ([`resolve_removals`]).
///
/// The walk is
/// `git rev-list --ancestry-path --reverse <sha>..HEAD -- <path>`:
/// every commit that is a descendant of `sha`, an ancestor of HEAD, and
/// touched the file, oldest first. The oldest is the one that removed
/// the lines.
///
/// - **Empty output** — nothing after `sha` touched the file on the way
///   to HEAD, so the lines are still there.
/// - **Two or more, and the second is not a descendant of the first** —
///   history forked at `sha` and parallel branches touched the file, so
///   several commits qualify. [`Removal::Ambiguous`] rather than a
///   guess: naming the wrong commit in a blame heading is worse than
///   naming none.
///
/// The `merge-base` check is the only reason for a second invocation
/// and it runs only when the list has more than one entry, so the
/// linear case — overwhelmingly the common one — costs one call.
fn resolve_removal(workdir: &std::path::Path, sha: &str, path: &str) -> Removal {
    let candidates = git_lines(
        workdir,
        &[
            "rev-list",
            "--ancestry-path",
            "--reverse",
            &format!("{sha}..HEAD"),
            "--",
            path,
        ],
    );
    let Some(first) = candidates.first() else {
        return Removal::StillPresent;
    };
    if let Some(second) = candidates.get(1) {
        // `--is-ancestor` exits 0 when it is one. If the second commit
        // does not descend from the first, they are on parallel
        // branches and "the first" is an artefact of traversal order.
        let linear = std::process::Command::new("git")
            .args(["merge-base", "--is-ancestor", first, second])
            .current_dir(workdir)
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if !linear {
            return Removal::Ambiguous;
        }
    }
    match commit_meta(workdir, first) {
        Some(c) => Removal::By(c),
        // The rev-list named it, so a failure to describe it is a git
        // problem rather than an absent commit. Declining beats
        // rendering a bare SHA with empty columns.
        None => Removal::Ambiguous,
    }
}

/// `author-name`, `author-time` and `subject` for one commit.
fn commit_meta(workdir: &std::path::Path, sha: &str) -> Option<RemovalCommit> {
    // `%x1f` is the ASCII unit separator: it cannot occur in an author
    // name or a subject, where a `|` or a tab easily can.
    let out = git_lines(workdir, &["show", "-s", "--format=%an%x1f%at%x1f%s", sha]);
    let line = out.first()?;
    let mut parts = line.split('\u{1f}');
    let author = parts.next()?.to_string();
    let time = parts.next()?.parse::<i64>().ok()?;
    let summary = parts.next().unwrap_or_default().to_string();
    Some(RemovalCommit {
        sha: sha.to_string(),
        author,
        time,
        summary,
    })
}

/// Run `git` and return stdout's non-empty lines, or nothing on
/// failure. Blame must degrade to fewer annotations, never to an error.
fn git_lines(workdir: &std::path::Path, args: &[&str]) -> Vec<String> {
    match std::process::Command::new("git")
        .args(args)
        .current_dir(workdir)
        .output()
    {
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout)
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(str::to_string)
            .collect(),
        Ok(o) => {
            tracing::debug!(
                target: "lattice_magit",
                "git {args:?}: {}",
                String::from_utf8_lossy(&o.stderr).trim(),
            );
            Vec::new()
        }
        Err(e) => {
            tracing::debug!(target: "lattice_magit", "git {args:?}: {e}");
            Vec::new()
        }
    }
}

/// MG.33: fill in `removal` for every chunk of a reverse blame.
///
/// **Deduped by SHA**, which is what keeps the cost sane: a file with
/// 200 chunks usually has far fewer distinct commits, and adjacent runs
/// of the same commit resolve once. Uncommitted lines are skipped —
/// they have no history to walk.
///
/// Blocking; runs inside the same `spawn_blocking` as the blame itself.
fn resolve_removals(workdir: &std::path::Path, path: &str, chunks: &mut [BlameChunk]) {
    let mut seen: std::collections::HashMap<String, Removal> = std::collections::HashMap::new();
    for chunk in chunks.iter_mut() {
        if is_uncommitted(&chunk.sha) {
            continue;
        }
        let removal = match seen.get(&chunk.sha) {
            Some(r) => r.clone(),
            None => {
                let r = resolve_removal(workdir, &chunk.sha, path);
                seen.insert(chunk.sha.clone(), r.clone());
                r
            }
        };
        chunk.removal = Some(removal);
    }
}

/// `git blame --line-porcelain`'s raw output, or empty on failure.
///
/// Returns the porcelain rather than formatted rows — the formatting
/// this used to do is what made blame a buffer instead of an
/// annotation.
fn run_blame(
    workdir: &std::path::Path,
    direction: BlameDirection,
    rev: &str,
    path: &str,
) -> String {
    if path.is_empty() || path == "." {
        return String::new();
    }
    match std::process::Command::new("git")
        .args(blame_argv(direction, rev, path))
        .current_dir(workdir)
        .output()
    {
        Ok(o) if o.status.success() => String::from_utf8(o.stdout).unwrap_or_default(),
        Ok(o) => {
            tracing::debug!(
                target: "lattice_magit",
                "blame {path}: {}",
                String::from_utf8_lossy(&o.stderr).trim(),
            );
            String::new()
        }
        Err(e) => {
            tracing::error!(target: "lattice_magit", "blame {path}: {e}");
            String::new()
        }
    }
}

/// Resolve `<rev>^`'s commit sha — `None` if `rev` has no parent (the
/// root commit) or resolution otherwise fails.
fn resolve_parent(workdir: &std::path::Path, rev: &str) -> Option<String> {
    let repo = Repository::discover(workdir).ok()?;
    repo.run_git_str(["rev-parse", &format!("{rev}^")])
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn forward_blame_names_the_revision_and_separates_the_path() {
        assert_eq!(
            blame_argv(BlameDirection::Addition, "HEAD", "src/main.rs"),
            vec!["blame", "--line-porcelain", "HEAD", "--", "src/main.rs"]
        );
    }

    /// The range is spelled out rather than relying on git reading a
    /// bare `--reverse <rev>` as `<rev>..HEAD`.
    #[test]
    fn reverse_blame_spells_out_the_range() {
        assert_eq!(
            blame_argv(BlameDirection::Reverse, "a1b2c3d", "src/main.rs"),
            vec![
                "blame",
                "--line-porcelain",
                "--reverse",
                "a1b2c3d..HEAD",
                "--",
                "src/main.rs"
            ]
        );
    }

    /// `--` is load-bearing: a path that looks like a rev is otherwise
    /// ambiguous and git resolves it in favour of the rev.
    #[test]
    fn a_path_that_looks_like_a_rev_is_still_a_path() {
        let argv = blame_argv(BlameDirection::Addition, "HEAD", "HEAD");
        let sep = argv.iter().position(|a| a == "--").expect("a separator");
        assert_eq!(argv[sep + 1], "HEAD", "the path sits after `--`: {argv:?}");
    }

    fn chunk(sha: &str, start: u32, count: u32) -> BlameChunk {
        BlameChunk {
            sha: sha.into(),
            author: "Jane Doe".into(),
            time: 1_700_000_000,
            summary: "do the thing".into(),
            start_line: start,
            line_count: count,
            removal: None,
        }
    }

    #[test]
    fn one_heading_is_emitted_above_each_chunk() {
        let p = BlameProvider::new(None, "magit-blame-mode");
        p.set_chunks(
            vec![chunk("aaaa1111", 0, 3), chunk("bbbb2222", 3, 2)],
            1_700_000_000,
        );
        let rows = p.collect();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].anchor_line, 0);
        assert_eq!(
            rows[1].anchor_line, 3,
            "the chunk's FIRST line, not its last"
        );
        for row in &rows {
            assert_eq!(
                row.position,
                AnchorPosition::Above,
                "a heading introduces its chunk"
            );
            assert_eq!(row.height, 1);
        }
    }

    #[test]
    fn a_heading_carries_the_commit_text() {
        let p = BlameProvider::new(None, "magit-blame-mode");
        p.set_chunks(vec![chunk("aaaa1111", 0, 1)], 1_700_000_000);
        let row = &p.collect()[0];
        let text: String = row
            .cells
            .iter()
            .map(|c| char::from_u32(c.codepoint).unwrap_or(' '))
            .collect();
        assert!(text.starts_with("aaaa1111"), "{text}");
        assert!(text.contains("Jane Doe"), "{text}");
        assert!(text.contains("do the thing"), "{text}");
    }

    /// The sha gets its own colour so chunk boundaries are scannable
    /// without reading the heading.
    #[test]
    fn the_sha_is_coloured_apart_from_the_rest_of_the_heading() {
        let p = BlameProvider::new(None, "magit-blame-mode");
        p.set_chunks(vec![chunk("aaaa1111", 0, 1)], 1_700_000_000);
        let row = &p.collect()[0];
        let (sha_fg, label_fg) = crate::headerline::blame_heading_fallback();
        assert_ne!(sha_fg, label_fg, "the two roles must be distinguishable");
        assert_eq!(row.cells[0].fg, sha_fg);
        assert_eq!(row.cells[7].fg, sha_fg, "…through the last sha character");
        assert_eq!(row.cells[8].fg, label_fg, "…and not past it");
    }

    /// MG.33: on a removed chunk the leading sha is the **removing**
    /// commit's, and it must be the one that gets the sha colour.
    ///
    /// This works because `sha_len` is measured from the rendered
    /// text's leading token rather than from `chunk.sha` — which reads
    /// like an implementation detail and is in fact the thing that
    /// keeps the two in step. Pinned so a later "tidy-up" to
    /// `chunk.sha.len()` (equal here only by coincidence of both shas
    /// being full-length) is caught.
    #[test]
    fn a_removed_headings_sha_colour_covers_the_removing_commit() {
        let p = BlameProvider::new(None, "magit-blame-mode");
        let mut c = chunk("aaaa1111", 0, 1);
        c.removal = Some(Removal::By(RemovalCommit {
            sha: "dead9999beef".into(),
            author: "Sam Patel".into(),
            time: 1_700_000_000,
            summary: "drop it".into(),
        }));
        p.set_chunks(vec![c], 1_700_000_000);
        let row = &p.collect()[0];
        let (sha_fg, label_fg) = crate::headerline::blame_heading_fallback();

        let text: String = row
            .cells
            .iter()
            .filter_map(|c| char::from_u32(c.codepoint))
            .collect();
        assert!(text.starts_with("dead9999"), "{text}");
        for i in 0..8 {
            assert_eq!(row.cells[i].fg, sha_fg, "char {i} of the removing sha");
        }
        assert_eq!(row.cells[8].fg, label_fg, "…and not past it");
    }

    /// An uncommitted chunk has no sha to colour, so the whole row is
    /// the label colour rather than eight characters of "sha" that is
    /// really the word "Uncommi".
    #[test]
    fn an_uncommitted_heading_colours_no_sha() {
        let p = BlameProvider::new(None, "magit-blame-mode");
        p.set_chunks(vec![chunk(&"0".repeat(40), 0, 1)], 1_700_000_000);
        let row = &p.collect()[0];
        let (_, label_fg) = crate::headerline::blame_heading_fallback();
        assert!(row.cells.iter().all(|c| c.fg == label_fg));
    }

    #[test]
    fn a_provider_with_no_blame_yet_emits_no_rows() {
        let p = BlameProvider::new(None, "magit-blame-mode");
        assert!(
            p.collect().is_empty(),
            "no headings until a blame lands — an empty lane, not a blank row"
        );
    }

    /// The worker short-circuits on the version fingerprint, so a new
    /// blame that did not bump it would never reach the screen.
    #[test]
    fn new_chunks_bump_the_version() {
        let p = BlameProvider::new(None, "magit-blame-mode");
        let before = p.version();
        p.set_chunks(vec![chunk("aaaa1111", 0, 1)], 1_700_000_000);
        assert!(p.version() > before);
    }

    #[test]
    fn the_chunk_at_the_cursor_is_the_one_covering_that_line() {
        let p = BlameProvider::new(None, "magit-blame-mode");
        p.set_chunks(
            vec![chunk("aaaa1111", 0, 3), chunk("bbbb2222", 3, 2)],
            1_700_000_000,
        );
        assert_eq!(p.chunk_at(0).unwrap().sha, "aaaa1111");
        assert_eq!(p.chunk_at(2).unwrap().sha, "aaaa1111");
        assert_eq!(p.chunk_at(3).unwrap().sha, "bbbb2222");
        assert!(p.chunk_at(99).is_none(), "past the end belongs to nothing");
    }

    /// A request is for one activation. Leaving it behind would make
    /// the next plain `:magit-blame` on the same buffer silently
    /// reverse.
    #[test]
    fn a_blame_request_is_consumed_by_the_activation_that_reads_it() {
        let r = BlameRequests::default();
        r.put(
            "*magit:file:a1b2c3d:src/main.rs*".into(),
            BlameDirection::Reverse,
            "a1b2c3d".into(),
        );
        assert_eq!(
            r.take("*magit:file:a1b2c3d:src/main.rs*"),
            Some((BlameDirection::Reverse, "a1b2c3d".into()))
        );
        assert_eq!(r.take("*magit:file:a1b2c3d:src/main.rs*"), None);
    }

    #[test]
    fn an_unrequested_buffer_takes_nothing() {
        let r = BlameRequests::default();
        assert_eq!(r.take("src/main.rs"), None);
    }

    /// `gq` turns blame off, and it must stay `g`-prefixed.
    ///
    /// Bare `q` is `magit-core-mode`'s (close the buffer), and this
    /// mode can be active on a blob buffer where that mode is also
    /// active — two minors on one chord resolves by registration order,
    /// which is not a contract.
    #[test]
    fn quitting_blame_is_g_prefixed_and_shares_no_chord_with_magit_core() {
        use lattice_mode::Mode;
        let chords: Vec<&str> = MagitBlameMode
            .keymap()
            .entries
            .iter()
            .map(|e| e.chord)
            .collect();
        assert!(chords.contains(&"gq"), "{chords:?}");
        assert!(
            !chords.contains(&"q"),
            "bare `q` belongs to magit-core-mode: {chords:?}"
        );
        let core: Vec<&str> = crate::MagitCoreMode
            .keymap()
            .entries
            .iter()
            .map(|e| e.chord)
            .collect();
        for c in &chords {
            assert!(
                !core.contains(c),
                "`{c}` is bound by both magit-blame-mode and magit-core-mode"
            );
        }
    }

    /// The regression a user reported: the headings appeared only after
    /// an unrelated redraw.
    ///
    /// Bumping the provider's version is NOT a wake — the cells worker
    /// re-reads providers when a redraw is already happening, and
    /// nothing starts one. `rerun_blame` must therefore fire the waker
    /// itself, and this pins that the state carries the handle it needs
    /// to. A `None` here is exactly the shipped bug.
    #[test]
    fn the_state_carries_the_handle_the_finished_blame_wakes_with() {
        // A structural pin: the field exists and is the wake-capable
        // handle, not a bare marker. The behavioural half lives in the
        // host's async-wake tests, which need a live editor.
        fn assert_wakeable(_: &Option<lattice_mode::PendingSyntheticHighlightsHandle>) {}
        let s = BlameState {
            workdir: std::path::PathBuf::new(),
            path: String::new(),
            rev: String::new(),
            direction: BlameDirection::Addition,
            provider: BlameProvider::new(None, "magit-blame-mode"),
            pending_highlights: None,
            _registration: None,
        };
        assert_wakeable(&s.pending_highlights);
    }

    /// The mode must stay Manual: an auto-activating blame would make
    /// every file read-only on open.
    #[test]
    fn blame_never_activates_on_its_own() {
        assert!(matches!(
            MagitBlameMode.activation_policy(),
            ActivationPolicy::Manual
        ));
        assert_eq!(MagitBlameMode.kind(), ModeKind::Minor);
    }
}

/// MG.33: resolving what removed a run of lines, against real git.
///
/// These drive actual repositories because the question is about
/// history topology — `--ancestry-path` behaviour across a merge is
/// exactly the thing a hand-built fixture would get wrong in the same
/// way the implementation might.
#[cfg(test)]
mod removal_resolution {
    use super::*;
    use std::path::Path;
    use std::process::Command;

    fn git(dir: &Path, args: &[&str]) -> String {
        let out = Command::new("git")
            .args(args)
            .current_dir(dir)
            .output()
            .expect("git");
        assert!(
            out.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    fn init(dir: &Path) {
        git(dir, &["init", "-b", "main"]);
        git(dir, &["config", "user.email", "t@lattice.dev"]);
        git(dir, &["config", "user.name", "lattice-test"]);
    }

    fn commit(dir: &Path, path: &str, body: &str, msg: &str) -> String {
        std::fs::write(dir.join(path), body).expect("write");
        git(dir, &["add", path]);
        git(dir, &["commit", "-m", msg]);
        git(dir, &["rev-parse", "HEAD"])
    }

    /// The case the feature is for: a line present at `base` and gone
    /// by HEAD resolves to the commit that took it out — not to `base`,
    /// which is what the heading showed before MG.33.
    #[test]
    fn a_removed_line_resolves_to_the_commit_that_removed_it() {
        let dir = tempfile::tempdir().expect("tempdir");
        let p = dir.path();
        init(p);
        let base = commit(p, "a.txt", "keep\ndoomed\n", "base");
        let remover = commit(p, "a.txt", "keep\n", "drop the doomed line");

        let Removal::By(c) = resolve_removal(p, &base, "a.txt") else {
            panic!("a line removed before HEAD must resolve to its remover");
        };
        assert_eq!(c.sha, remover, "must name the REMOVING commit, not `base`");
        assert_ne!(c.sha, base, "naming the last-containing commit is the bug");
        assert_eq!(c.summary, "drop the doomed line");
        assert_eq!(c.author, "lattice-test");
        assert!(c.time > 0, "the heading shows a relative date");
    }

    /// Lines still in the file at HEAD have no removing commit, and
    /// saying so is the point — inventing one would be the worst
    /// outcome of the three.
    #[test]
    fn a_surviving_line_resolves_to_still_present() {
        let dir = tempfile::tempdir().expect("tempdir");
        let p = dir.path();
        init(p);
        let base = commit(p, "a.txt", "keep\n", "base");
        // A later commit that does NOT touch the blamed file.
        commit(p, "b.txt", "other\n", "unrelated");

        assert_eq!(resolve_removal(p, &base, "a.txt"), Removal::StillPresent);
    }

    /// History forks at `base`, both branches touch the file, and both
    /// merge into HEAD. Two commits qualify and neither descends from
    /// the other, so we decline rather than pick by traversal order.
    ///
    /// This is the case the "within reason" clause of the UX rule is
    /// about: a confidently wrong attribution is worse than the honest
    /// "last contained here" it replaces.
    #[test]
    fn a_fork_that_both_branches_touched_is_ambiguous() {
        let dir = tempfile::tempdir().expect("tempdir");
        let p = dir.path();
        init(p);
        let base = commit(p, "a.txt", "one\ntwo\nthree\n", "base");

        git(p, &["checkout", "-b", "side"]);
        commit(p, "a.txt", "one\ntwo\n", "side drops three");
        git(p, &["checkout", "main"]);
        commit(p, "a.txt", "two\nthree\n", "main drops one");
        // Merge side in, resolving to something that keeps neither.
        let out = Command::new("git")
            .args(["merge", "side", "-m", "merge"])
            .current_dir(p)
            .output()
            .expect("git");
        if !out.status.success() {
            std::fs::write(p.join("a.txt"), "two\n").expect("write");
            git(p, &["add", "a.txt"]);
            git(p, &["commit", "-m", "merge"]);
        }

        assert_eq!(
            resolve_removal(p, &base, "a.txt"),
            Removal::Ambiguous,
            "two parallel commits touched the file after `base`; naming one \
             would be a guess presented as a fact"
        );
    }

    /// Uncommitted lines have no history to walk, so they are skipped
    /// rather than handed a bogus range (`0000000..HEAD` is not a rev).
    #[test]
    fn resolve_removals_skips_uncommitted_chunks_and_dedupes_by_sha() {
        let dir = tempfile::tempdir().expect("tempdir");
        let p = dir.path();
        init(p);
        let base = commit(p, "a.txt", "keep\ndoomed\n", "base");
        commit(p, "a.txt", "keep\n", "drop it");

        let mk = |sha: &str, start: u32| BlameChunk {
            sha: sha.into(),
            author: String::new(),
            time: 0,
            summary: String::new(),
            start_line: start,
            line_count: 1,
            removal: None,
        };
        // Two chunks share `base` — the dedupe path — plus an
        // uncommitted one that must be left alone.
        let mut chunks = vec![mk(&base, 0), mk(&"0".repeat(40), 1), mk(&base, 2)];
        resolve_removals(p, "a.txt", &mut chunks);

        assert!(matches!(chunks[0].removal, Some(Removal::By(_))));
        assert_eq!(
            chunks[0].removal, chunks[2].removal,
            "the same sha must resolve to the same answer, from one walk"
        );
        assert_eq!(
            chunks[1].removal, None,
            "an uncommitted chunk has no history to walk"
        );
    }
}
