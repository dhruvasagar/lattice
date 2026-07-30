//! MG.7: magit-blame major mode.
//!
//! Runs `git blame --line-porcelain <path>` on open, populates
//! buffer with annotated content. <CR> shows commit, p re-blames
//! at the parent of whatever revision is currently blamed.

use std::sync::{Arc, Mutex, OnceLock};

use lattice_config;
use lattice_grammar::Effect;
use lattice_mode::{
    ActionContext, ActionHandlerContribution, BufferStoreHandle, CapabilitySet, Keymap,
    KeymapEntry, LifecycleFuture, Mode, ModeContext, ModeId, ModeKind, OptionOverrideSet,
    keymap_entry,
};
use lattice_protocol::edit::Edit;
use lattice_protocol::position::{Position, Range};
use lattice_vcs::Repository;

use crate::buffer_state::{BufferStateGuard, BufferStates};
use crate::headerline::{self, MagitHeaderlineHandle};

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
            keymap_entry! { mode: Normal, chord: "<CR>", doc: "Show commit for blamed line", cmd: "action:magit-blame-show-commit" },
            keymap_entry! { mode: Normal, chord: "p", doc: "Re-blame at parent commit", cmd: "action:magit-blame-parent" },
        ]
    })
}

pub struct BlameState {
    buffer_id: lattice_core::BufferId,
    store: Arc<BufferStoreHandle>,
    workdir: std::path::PathBuf,
    path: String,
    /// The revision currently being blamed — `p` walks this back to
    /// its parent. Starts at "HEAD" (equivalent to blaming the
    /// working tree's current checkout).
    rev: String,
    pending_highlights: Option<lattice_mode::PendingSyntheticHighlightsHandle>,
    /// MG.14: the buffer's headerline. `p` walks `rev` back to its
    /// parent; without the header there is no way to tell how far
    /// back you have walked.
    headerline: Option<MagitHeaderlineHandle>,
}

/// MG.13: service alias for this mode's per-buffer state
/// (`feedback_servicesregistry_arc_typeid`).
pub type BlameStatesHandle = Arc<BufferStates<BlameState>>;

fn state(ctx: &ActionContext<'_>) -> Option<Arc<Mutex<BlameState>>> {
    crate::buffer_state::state_for::<BlameState>(ctx)
}

impl Mode for MagitBlameMode {
    type Guard = BufferStateGuard<BlameState>;

    fn id(&self) -> ModeId {
        Self::mode_id()
    }
    fn kind(&self) -> ModeKind {
        ModeKind::Major
    }
    fn target_buffer_kind(&self) -> Option<lattice_core::BufferKind> {
        None
    }

    fn options(&self) -> OptionOverrideSet {
        lattice_config::overrides! {
            lattice_config::ReadOnly = true,
            lattice_config::NoFile = true,
            lattice_config::Number = false,
        }
    }

    fn required_capabilities(&self) -> CapabilitySet {
        CapabilitySet::empty()
    }
    fn keymap(&self) -> Keymap {
        Keymap::from_entries(magit_blame_keymap_entries())
    }

    /// MG.13: boot-registered — see `buffer_state`'s module docs.
    fn action_handlers(&self) -> Vec<ActionHandlerContribution> {
        vec![
            // <CR> — show the commit for the blamed line at cursor.
            ActionHandlerContribution {
                action_name: "action:magit-blame-show-commit",
                handler: Arc::new(|ctx: &ActionContext<'_>| {
                    let s = state(ctx)?;
                    let g = s.lock().ok()?;
                    let handle = g.store.handle_for(g.buffer_id)?;
                    let snap = handle.snapshot();
                    let line = snap.buffer.line(ctx.cursor.line)?;
                    let sha = line.get(0..8)?;
                    if sha.trim().is_empty() || !sha.chars().all(|c| c.is_ascii_hexdigit()) {
                        return None;
                    }
                    Some(Effect::OpenSyntheticBuffer {
                        name: format!("*magit:commit:{sha}*"),
                        mode_id: "magit-revision-mode".to_string(),
                    })
                }),
            },
            // p — re-blame at the parent of the revision currently shown.
            ActionHandlerContribution {
                action_name: "action:magit-blame-parent",
                handler: Arc::new(|ctx: &ActionContext<'_>| {
                    let s = state(ctx)?;
                    let (handle, wd, path, rev, pending, buffer_id, hl) = {
                        let g = s.lock().ok()?;
                        (
                            g.store.handle_for(g.buffer_id)?,
                            g.workdir.clone(),
                            g.path.clone(),
                            g.rev.clone(),
                            g.pending_highlights.clone(),
                            g.buffer_id,
                            g.headerline.clone(),
                        )
                    };
                    let s2 = s.clone();
                    tokio::task::spawn(async move {
                        let wd2 = wd.clone();
                        let rev_for_lookup = rev.clone();
                        let parent = tokio::task::spawn_blocking(move || {
                            resolve_parent(&wd2, &rev_for_lookup)
                        })
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
                            g.rev = parent.clone();
                        }
                        // MG.14: the header is the only place the
                        // walked-to revision is visible — the blame
                        // body itself looks identical at every step.
                        headerline::publish(&hl, headerline::blame_fields(&path, &parent));
                        let wd3 = wd.clone();
                        let path2 = path.clone();
                        let parent2 = parent.clone();
                        let text =
                            tokio::task::spawn_blocking(move || run_blame(&wd3, &parent2, &path2))
                                .await
                                .unwrap_or_default();
                        let spans = crate::highlight::blame_styled_spans(&text);
                        apply_full_replace(&handle, text).await;
                        if let Some(ph) = pending {
                            ph.store_and_wake(buffer_id, spans);
                        }
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
            let Some(handle) = store.handle_for(buffer_id) else {
                return Ok(orphan());
            };
            let workdir = Repository::discover(".")
                .ok()
                .and_then(|r| r.workdir().map(|p| p.to_path_buf()))
                .unwrap_or_default();

            // Extract the blamed file path from the buffer name:
            // "*magit:blame:<path>*" → "<path>"
            let file_path = store
                .name_for(buffer_id)
                .and_then(|name| {
                    let s = name.strip_prefix("*magit:blame:")?;
                    Some(s.strip_suffix("*")?.to_string())
                })
                .unwrap_or_else(|| ".".to_string());

            let pending_highlights = ctx.service::<lattice_mode::PendingSyntheticHighlights>();

            // MG.14: path and revision are both known here, so the
            // header is complete before the blame itself runs.
            let (hl, hl_registration) =
                match headerline::install(&ctx, buffer_id, Self::mode_id().as_str()) {
                    Some((h, reg)) => (Some(h), Some(reg)),
                    None => (None, None),
                };
            headerline::publish(&hl, headerline::blame_fields(&file_path, "HEAD"));

            // MG.13: publish BEFORE the first `.await` — see the note
            // in `magit_branch_mode::on_activate`.
            let Some(states) = ctx.service::<BlameStatesHandle>() else {
                return Ok(orphan());
            };
            states.publish(
                buffer_id,
                BlameState {
                    buffer_id,
                    store: store.clone(),
                    workdir: workdir.clone(),
                    path: file_path.clone(),
                    rev: "HEAD".to_string(),
                    pending_highlights: pending_highlights.clone(),
                    headerline: hl.clone(),
                },
            );
            let guard = BufferStateGuard::new((*states).clone(), buffer_id)
                .with_headerline(hl_registration);

            // Populate blame: blocking I/O on spawn_blocking, then apply
            // edit on the current task (no Runtime::new()).
            let wd = workdir.clone();
            let fp = file_path.clone();
            let text = tokio::task::spawn_blocking(move || run_blame(&wd, "HEAD", &fp))
                .await
                .unwrap();
            let spans = crate::highlight::blame_styled_spans(&text);
            apply_full_replace(&handle, text).await;
            if let Some(ref ph) = pending_highlights {
                ph.store_and_wake(buffer_id, spans);
            }

            Ok(guard)
        })
    }
}

async fn apply_full_replace(handle: &Arc<dyn lattice_runtime::Document>, text: String) {
    let snap = handle.snapshot();
    let last = snap.buffer.line_count().saturating_sub(1);
    let last_line = snap.buffer.line(last).unwrap_or_default();
    let end = Position::new(last, last_line.len() as u32);
    let _ = handle
        .apply_edit_batch(vec![Edit::replace(Range::new(Position::ZERO, end), text)])
        .await;
}

/// Resolve `<rev>^`'s commit sha — `None` if `rev` has no parent
/// (the root commit) or resolution otherwise fails.
fn resolve_parent(workdir: &std::path::Path, rev: &str) -> Option<String> {
    let repo = Repository::discover(workdir).ok()?;
    repo.run_git_str(["rev-parse", &format!("{rev}^")])
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

fn run_blame(workdir: &std::path::Path, rev: &str, path: &str) -> String {
    if path.is_empty() || path == "." {
        return "No file to blame — open :magit-blame <file> or run from a file buffer.\n"
            .to_string();
    }
    let output = std::process::Command::new("git")
        .args(["blame", "--line-porcelain", rev, "--", path])
        .current_dir(workdir)
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .unwrap_or_else(|| format!("Could not blame {path}\n"));

    let result = format_blame_porcelain(&output);
    if result.is_empty() {
        format!("No blame data for {path}\n")
    } else {
        result
    }
}

/// Width of the author column. The styler colours a fixed span
/// (`highlight::blame_styled_spans`), so the column is padded *and*
/// truncated to this — a longer name would otherwise push the code
/// right and leave the rest of the name uncoloured.
const AUTHOR_WIDTH: usize = 12;

/// `git blame --line-porcelain` → one `<sha8> <author>  <code>` row per
/// source line.
///
/// Porcelain is a *stanza* format, not a line format: a header line
/// (`<40-hex-sha> <orig> <final>[ <count>]`) opens a group, key/value
/// lines follow, and the source line itself is the one prefixed with a
/// TAB. The sha, the author and the code therefore arrive on three
/// different lines and have to be carried forward to be emitted
/// together.
///
/// The version this replaces did not carry anything: it read the sha
/// from whichever line it happened to be on (`"author D"` for an author
/// line) and dropped every line shorter than 40 characters — which is
/// most `author …` lines and most source lines. What reached the buffer
/// was source lines over 40 characters with no blame prefix at all, so
/// the blame buffer opened, said nothing, and looked like a key that
/// did nothing.
///
/// Pure, so the shape is testable without a repository — and it is the
/// shape that matters, because `highlight::blame_styled_spans` colours
/// by column position and silently colours the wrong text if the
/// columns move.
pub(crate) fn format_blame_porcelain(porcelain: &str) -> String {
    let mut out = String::new();
    let mut sha = String::new();
    let mut author = String::new();

    for line in porcelain.lines() {
        if let Some(code) = line.strip_prefix('\t') {
            // The one line per stanza that carries the file's own text.
            let short: String = sha.chars().take(8).collect();
            let name: String = author.chars().take(AUTHOR_WIDTH).collect();
            out.push_str(&format!("{short:8} {name:>AUTHOR_WIDTH$}  {code}\n"));
        } else if let Some(rest) = line.strip_prefix("author ") {
            author = rest.to_string();
        } else if let Some(header_sha) = porcelain_header_sha(line) {
            // A new stanza: the author lines that follow belong to it.
            sha = header_sha;
        }
    }
    out
}

/// The sha of a porcelain stanza header, or `None` for any other line.
///
/// Checked rather than assumed: `author-mail`, `summary` and friends are
/// also space-separated key/value lines, and a summary that happened to
/// begin with a hex word would otherwise be read as a new commit.
fn porcelain_header_sha(line: &str) -> Option<String> {
    let first = line.split(' ').next()?;
    (first.len() >= 8 && first.chars().all(|c| c.is_ascii_hexdigit()))
        .then(|| first.to_string())
}


#[cfg(test)]
mod blame_format {
    use super::*;

    /// One stanza of real `--line-porcelain` output. The header, the
    /// author and the code are on three different lines, which is the
    /// whole reason the parser has to carry state.
    const ONE_STANZA: &str = "\
9a17f8e18e0e5e2b3c4d5e6f708192a3b4c5d6e7 1 1 1
author Jane Doe
author-mail <jane@example.com>
author-time 1700000000
author-tz +0530
committer Jane Doe
summary a1b2c3d4 looks like a sha but is a summary
filename src/main.rs
\tuse std::fs;
";

    #[test]
    fn a_stanza_becomes_one_row_of_sha_author_and_code() {
        let out = format_blame_porcelain(ONE_STANZA);
        assert_eq!(out, format!("9a17f8e1 {:>12}  use std::fs;\n", "Jane Doe"));
    }

    /// The bug this replaces: `author …` lines and short source lines
    /// were both dropped for being under 40 characters, so a normal
    /// repo's blame buffer came out with no blame in it.
    #[test]
    fn short_lines_are_not_dropped() {
        let out = format_blame_porcelain(ONE_STANZA);
        assert!(
            out.contains("use std::fs;"),
            "a 12-character source line must survive: {out:?}"
        );
        assert!(
            out.starts_with("9a17f8e1"),
            "the row must carry the commit, not the text of whatever \
             line the parser was on: {out:?}"
        );
        assert!(
            !out.contains("author D") && !out.contains("author "),
            "the sha column must never be the literal text `author …`: {out:?}"
        );
    }

    /// `summary` and `author-mail` are space-separated key/value lines
    /// too, so a summary beginning with a hex word must not be read as
    /// the start of a new commit — the following lines would then be
    /// blamed on it.
    #[test]
    fn a_hex_looking_summary_is_not_mistaken_for_a_stanza_header() {
        let out = format_blame_porcelain(ONE_STANZA);
        assert!(
            out.starts_with("9a17f8e1"),
            "the summary's `a1b2c3d4` must not have replaced the sha: {out:?}"
        );
    }

    /// The styler colours by column, so the columns must not move.
    #[test]
    fn the_author_column_is_padded_and_truncated_to_a_fixed_width() {
        let long = ONE_STANZA.replace("author Jane Doe", "author Bartholomew Featherstonehaugh");
        let out = format_blame_porcelain(&long);
        let row = out.lines().next().expect("a row");
        assert_eq!(
            row.find("  use std::fs;"),
            Some(9 + AUTHOR_WIDTH),
            "a long name must be truncated rather than pushing the code \
             right, or the highlight spans land on the wrong text: {row:?}"
        );
    }

    /// Two stanzas, two commits: the second must not inherit the
    /// first's sha or author.
    #[test]
    fn each_stanza_carries_its_own_commit() {
        let two = format!(
            "{ONE_STANZA}\
ffffffffffffffffffffffffffffffffffffffff 2 2 1
author Sam Patel
filename src/main.rs
\tfn main() {{}}
"
        );
        let out = format_blame_porcelain(&two);
        let rows: Vec<&str> = out.lines().collect();
        assert_eq!(rows.len(), 2);
        assert!(rows[0].starts_with("9a17f8e1") && rows[0].ends_with("use std::fs;"));
        assert!(rows[1].starts_with("ffffffff") && rows[1].ends_with("fn main() {}"));
        assert!(rows[1].contains("Sam Patel"));
    }
}

/// The parser against real `git blame` output, because the stanza
/// format is git's to define and a fixture only proves we parse our own
/// idea of it.
#[cfg(test)]
mod blame_round_trip {
    use super::*;
    use std::process::Command;

    fn git_ok(dir: &std::path::Path, args: &[&str]) {
        let st = Command::new("git")
            .args(args)
            .current_dir(dir)
            .status()
            .expect("git");
        assert!(st.success(), "git {args:?} failed");
    }

    #[test]
    fn every_source_line_reaches_the_blame_buffer_with_its_commit() {
        let dir = tempfile::tempdir().expect("tempdir");
        let p = dir.path();
        git_ok(p, &["init"]);
        git_ok(p, &["config", "user.email", "t@lattice.dev"]);
        git_ok(p, &["config", "user.name", "lattice-test"]);
        // Deliberately short lines — the length threshold that broke
        // this is invisible against long ones.
        std::fs::write(p.join("a.txt"), "one\ntwo\nthree\n").unwrap();
        git_ok(p, &["add", "a.txt"]);
        git_ok(p, &["commit", "-m", "base"]);

        let out = run_blame(p, "HEAD", "a.txt");
        let rows: Vec<&str> = out.lines().collect();
        assert_eq!(rows.len(), 3, "one row per source line: {out:?}");
        for (row, text) in rows.iter().zip(["one", "two", "three"]) {
            assert!(row.ends_with(text), "{row:?} must end with {text:?}");
            assert!(
                row[..8].chars().all(|c| c.is_ascii_hexdigit()),
                "{row:?} must start with a commit sha"
            );
            assert!(
                row.contains("lattice-test"),
                "{row:?} must name the author"
            );
        }
    }
}
