//! `:picker magit-branch-pick-base` source generator.
//!
//! Lists existing local branches so magit's branch-create wizard
//! (`c` in `magit-branch-mode`) can let the user pick a base branch
//! before typing the new branch's name — mirrors Emacs magit's own
//! two-step "pick base, then type name" flow. Accept emits
//! `PickerAcceptOutcome::OpenPrompt`, stashing the picked base in the
//! prompt buffer's synthetic name for
//! `action:magit-branch-create-finish` (registered in
//! `magit_global_mode`) to read back.

use std::path::PathBuf;
use std::sync::Arc;

use lattice_completion::{CandidateKind, RawCandidate};
use lattice_picker::{
    PickerAcceptOutcome, PickerContext, PickerInitResult, PickerSourceGenerator, PickerSourceSpec,
    RoutingPayload, SourceResult,
};
use lattice_vcs::{Branch, RefKind, Reference, Remote, Repository};

pub struct BranchPickBaseSource {
    spec: PickerSourceSpec,
}

impl BranchPickBaseSource {
    pub fn new() -> Self {
        Self {
            spec: PickerSourceSpec::no_args(
                "magit-branch-pick-base",
                "Pick an existing branch as the base for a new branch (magit branch-create wizard).",
            ),
        }
    }
}

impl Default for BranchPickBaseSource {
    fn default() -> Self {
        Self::new()
    }
}

impl PickerSourceGenerator for BranchPickBaseSource {
    fn spec(&self) -> &PickerSourceSpec {
        &self.spec
    }

    fn init(&self, _ctx: &PickerContext<'_>, _args: &[String]) -> SourceResult<PickerInitResult> {
        Ok(PickerInitResult::Future(Box::pin(async move {
            let branches = tokio::task::spawn_blocking(|| {
                let repo = Repository::discover(".")
                    .map_err(|e| format!("magit-branch-pick-base: repo discover failed: {e}"))?;
                Branch::list(&repo).map_err(|e| format!("magit-branch-pick-base: {e}"))
            })
            .await
            .map_err(|e| format!("magit-branch-pick-base: join error: {e}"))??;
            Ok(branches
                .into_iter()
                .map(|name| {
                    let cand = RawCandidate::plain(name.clone(), CandidateKind::Plain);
                    (cand, RoutingPayload::BranchBase { name })
                })
                .collect())
        })))
    }

    fn accept(
        &self,
        _ctx: &PickerContext<'_>,
        routing: &RoutingPayload,
    ) -> SourceResult<PickerAcceptOutcome> {
        match routing {
            RoutingPayload::BranchBase { name } => Ok(branch_create_prompt_outcome(name)),
            other => Err(format!(
                "magit-branch-pick-base: unexpected routing payload {other:?}"
            )),
        }
    }
}

/// MG.29: pick a branch and check it out.
///
/// The same listing as [`BranchPickBaseSource`] with a different
/// `accept` — the branch buffer's `<CR>` needs a cursor, and a menu
/// opened from anywhere has none. Same reasoning MG.23j applied to
/// `A` / `_` / `O`: the row asks rather than being gated away.
pub struct BranchCheckoutSource {
    spec: PickerSourceSpec,
}

impl BranchCheckoutSource {
    pub fn new() -> Self {
        Self {
            spec: PickerSourceSpec::no_args(
                BRANCH_CHECKOUT_SOURCE,
                "Pick a branch and check it out.",
            ),
        }
    }
}

impl Default for BranchCheckoutSource {
    fn default() -> Self {
        Self::new()
    }
}

pub const BRANCH_CHECKOUT_SOURCE: &str = "magit-branch-checkout-pick";

impl PickerSourceGenerator for BranchCheckoutSource {
    fn spec(&self) -> &PickerSourceSpec {
        &self.spec
    }

    fn init(&self, ctx: &PickerContext<'_>, args: &[String]) -> SourceResult<PickerInitResult> {
        // One listing, one place. A second copy of "enumerate the
        // branches" would drift from this one the first time either
        // grows a filter.
        BranchPickBaseSource::new().init(ctx, args)
    }

    fn accept(
        &self,
        _ctx: &PickerContext<'_>,
        routing: &RoutingPayload,
    ) -> SourceResult<PickerAcceptOutcome> {
        match routing {
            RoutingPayload::BranchBase { name } => Ok(branch_checkout_outcome(name)),
            other => Err(format!(
                "{BRANCH_CHECKOUT_SOURCE}: unexpected routing payload {other:?}"
            )),
        }
    }
}

/// What accepting a branch in the checkout picker does.
///
/// Pure and separate from `accept` for the same reason
/// [`branch_create_prompt_outcome`] is: `accept`'s signature needs a
/// `PickerContext` fixture this translation never reads.
fn branch_checkout_outcome(name: &str) -> PickerAcceptOutcome {
    PickerAcceptOutcome::InvokeCommand {
        id: "magit-checkout".to_string(),
        args: lattice_grammar::Args::String(name.to_string()),
    }
}

/// Build the `OpenPrompt` outcome for a picked base branch. Pulled
/// out of `accept` so it's testable without a full `PickerContext`
/// fixture (which `accept`'s trait signature requires but this
/// translation never actually reads).
fn branch_create_prompt_outcome(base: &str) -> PickerAcceptOutcome {
    PickerAcceptOutcome::OpenPrompt {
        prompt: format!("New branch name (from {base}):"),
        initial: String::new(),
        on_submit_action: "action:magit-branch-create-finish".to_string(),
        buffer_name: Some(format!(
            "{}{base}*",
            crate::magit_global_mode::BRANCH_CREATE_PROMPT_PREFIX
        )),
    }
}

// ── MG.32: the rest of magit's branch transient ──────────────────
//
// Three more sources, all listing the same branches. Each reuses
// `BranchPickBaseSource::init` rather than re-enumerating: a second
// copy of "list the branches" drifts the moment either grows a filter,
// which is the reason `BranchCheckoutSource` was built this way in
// MG.29 and the reason these follow it.

/// MG.32: `n` — magit's "new branch" *without* checking it out.
///
/// Distinct from `c` (`BranchPickBaseSource`) only in the accept: same
/// listing, same prompt shape, and the finish action passes
/// `checkout: false` to the same `Branch::create`. Magit keeps both
/// because "start a branch here" and "start a branch and go there" are
/// different intents, and the second is not always what you want when
/// you are mid-edit.
pub struct BranchCreateNoCheckoutSource {
    spec: PickerSourceSpec,
}

impl BranchCreateNoCheckoutSource {
    pub fn new() -> Self {
        Self {
            spec: PickerSourceSpec::no_args(
                BRANCH_CREATE_NO_CHECKOUT_SOURCE,
                "Pick a base, then name a new branch — without checking it out.",
            ),
        }
    }
}

impl Default for BranchCreateNoCheckoutSource {
    fn default() -> Self {
        Self::new()
    }
}

pub const BRANCH_CREATE_NO_CHECKOUT_SOURCE: &str = "magit-branch-create-no-checkout-pick";

impl PickerSourceGenerator for BranchCreateNoCheckoutSource {
    fn spec(&self) -> &PickerSourceSpec {
        &self.spec
    }

    fn init(&self, ctx: &PickerContext<'_>, args: &[String]) -> SourceResult<PickerInitResult> {
        BranchPickBaseSource::new().init(ctx, args)
    }

    fn accept(
        &self,
        _ctx: &PickerContext<'_>,
        routing: &RoutingPayload,
    ) -> SourceResult<PickerAcceptOutcome> {
        match routing {
            RoutingPayload::BranchBase { name } => Ok(branch_create_no_checkout_outcome(name)),
            other => Err(format!(
                "{BRANCH_CREATE_NO_CHECKOUT_SOURCE}: unexpected routing payload {other:?}"
            )),
        }
    }
}

fn branch_create_no_checkout_outcome(base: &str) -> PickerAcceptOutcome {
    PickerAcceptOutcome::OpenPrompt {
        prompt: format!("New branch name (from {base}, no checkout):"),
        initial: String::new(),
        on_submit_action: "action:magit-branch-create-no-checkout-finish".to_string(),
        buffer_name: Some(format!(
            "{}{base}*",
            crate::magit_global_mode::BRANCH_CREATE_NO_CHECKOUT_PROMPT_PREFIX
        )),
    }
}

/// MG.32: `m` — rename a branch.
///
/// Pick the branch to rename, then a prompt asks for its new name.
/// The old name rides in the prompt buffer's name, the same carry
/// `c`'s wizard uses for its base — one mechanism, not a second.
pub struct BranchRenameSource {
    spec: PickerSourceSpec,
}

impl BranchRenameSource {
    pub fn new() -> Self {
        Self {
            spec: PickerSourceSpec::no_args(
                BRANCH_RENAME_SOURCE,
                "Pick a branch, then type its new name.",
            ),
        }
    }
}

impl Default for BranchRenameSource {
    fn default() -> Self {
        Self::new()
    }
}

pub const BRANCH_RENAME_SOURCE: &str = "magit-branch-rename-pick";

impl PickerSourceGenerator for BranchRenameSource {
    fn spec(&self) -> &PickerSourceSpec {
        &self.spec
    }

    fn init(&self, ctx: &PickerContext<'_>, args: &[String]) -> SourceResult<PickerInitResult> {
        BranchPickBaseSource::new().init(ctx, args)
    }

    fn accept(
        &self,
        _ctx: &PickerContext<'_>,
        routing: &RoutingPayload,
    ) -> SourceResult<PickerAcceptOutcome> {
        match routing {
            RoutingPayload::BranchBase { name } => Ok(branch_rename_outcome(name)),
            other => Err(format!(
                "{BRANCH_RENAME_SOURCE}: unexpected routing payload {other:?}"
            )),
        }
    }
}

/// The rename prompt is pre-filled with the current name, because a
/// rename is usually an edit of it (a typo, a prefix) rather than a
/// fresh name typed from nothing.
fn branch_rename_outcome(old: &str) -> PickerAcceptOutcome {
    PickerAcceptOutcome::OpenPrompt {
        prompt: format!("Rename {old} to:"),
        initial: old.to_string(),
        on_submit_action: "action:magit-branch-rename-finish".to_string(),
        buffer_name: Some(format!(
            "{}{old}*",
            crate::magit_global_mode::BRANCH_RENAME_PROMPT_PREFIX
        )),
    }
}

/// MG.32: `x` — delete a branch (magit's `k`, moved by
/// evil-collection-magit).
///
/// Accept routes through the **ex-command**, not straight to a git
/// call, because deletion must ask first (MG.12) and a picker's accept
/// cannot raise an `Effect::Confirm` itself. `:magit-branch-delete
/// <name>` is that ask, and is the scriptable form besides.
pub struct BranchDeleteSource {
    spec: PickerSourceSpec,
}

impl BranchDeleteSource {
    pub fn new() -> Self {
        Self {
            spec: PickerSourceSpec::no_args(
                BRANCH_DELETE_SOURCE,
                "Pick a branch to delete — asks before deleting.",
            ),
        }
    }
}

impl Default for BranchDeleteSource {
    fn default() -> Self {
        Self::new()
    }
}

pub const BRANCH_DELETE_SOURCE: &str = "magit-branch-delete-pick";

impl PickerSourceGenerator for BranchDeleteSource {
    fn spec(&self) -> &PickerSourceSpec {
        &self.spec
    }

    fn init(&self, ctx: &PickerContext<'_>, args: &[String]) -> SourceResult<PickerInitResult> {
        BranchPickBaseSource::new().init(ctx, args)
    }

    fn accept(
        &self,
        _ctx: &PickerContext<'_>,
        routing: &RoutingPayload,
    ) -> SourceResult<PickerAcceptOutcome> {
        match routing {
            RoutingPayload::BranchBase { name } => Ok(branch_delete_outcome(name)),
            other => Err(format!(
                "{BRANCH_DELETE_SOURCE}: unexpected routing payload {other:?}"
            )),
        }
    }
}

fn branch_delete_outcome(name: &str) -> PickerAcceptOutcome {
    PickerAcceptOutcome::InvokeCommand {
        id: "magit-branch-delete".to_string(),
        args: lattice_grammar::Args::String(name.to_string()),
    }
}

/// MG.53.c: build the ex line a picked value runs.
///
/// `{}` in `command` is replaced by the pick; with no `{}` the pick is
/// appended. Both magit's picker sources go through this.
///
/// The placeholder exists because not every operation takes its
/// selection LAST. `magit-find-file` is `<rev> <path>` — appending a
/// revision to `magit-find-file src/main.rs` would produce
/// `<path> <rev>` and open a file named after a sha. The alternative
/// was registering order-adapter ex-commands that duplicate an existing
/// operation purely to move an argument, which is a second
/// implementation of the same thing.
pub(crate) fn picked_line(command: &str, value: &str) -> String {
    match command.find("{}") {
        Some(_) => command.replacen("{}", value, 1),
        None => format!("{command} {value}"),
    }
}

/// Register this crate's picker sources into the supplied registry.
/// Called from `lattice-host`'s `editor_boot.rs`, mirroring
/// `lattice_snippet::picker_sources::register` — the picker registry
/// is host-owned and populated by name at boot (no generic
/// `SubsystemBoot` seam for pickers today), so this is the
/// established shape for a feature crate to contribute a source.
/// MG.54: `config` reaches the revision source so it can read
/// `magit.revision-preview` at preview time. Passed in rather than
/// looked up because the picker registry is populated at boot, before
/// any service registry the source could consult exists — and because
/// the option is this crate's, so this crate reads it (the host learns
/// nothing about magit's options).
pub fn register(
    picker_registry: &mut lattice_picker::PickerRegistry,
    config: Option<Arc<lattice_config::ConfigRegistry>>,
) {
    picker_registry.register_generator(Arc::new(BranchPickBaseSource::new()));
    picker_registry.register_generator(Arc::new(BranchCheckoutSource::new()));
    // MG.32: the rest of the branch transient's picker-backed rows.
    picker_registry.register_generator(Arc::new(BranchCreateNoCheckoutSource::new()));
    picker_registry.register_generator(Arc::new(BranchRenameSource::new()));
    picker_registry.register_generator(Arc::new(BranchDeleteSource::new()));
    picker_registry.register_generator(Arc::new(CommitPickSource::new()));
    // The stash peer: the dispatch menu's apply / pop / drop / show
    // rows have no cursor to resolve a stash from.
    picker_registry.register_generator(Arc::new(StashPickSource::new()));
    // MG.52: the branch peer of `CommitPickSource` — one source for
    // every "which branch?" question, parameterised by what to do with
    // the answer.
    picker_registry.register_generator(Arc::new(BranchPickSource::new()));
    // MG.53.d/e: tag / remote / ref — one implementation, three scopes.
    picker_registry.register_generator(Arc::new(RefPickSource::new(RefScope::Tags)));
    picker_registry.register_generator(Arc::new(RefPickSource::new(RefScope::Remotes)));
    picker_registry.register_generator(Arc::new(RefPickSource::new(RefScope::AllRefs)));
    // MG.54: the revision scope is the one that previews (`C-c f v`), so
    // it is the one that needs the config handle.
    picker_registry.register_generator(Arc::new(RefPickSource::with_config(
        RefScope::Revisions,
        config,
    )));
}

/// MG.23j: `:picker magit-commit <ex-command>` — pick a commit, then
/// run that command on it.
///
/// The repo-level `A` / `_` / `O` rows need a commit and the root
/// dispatch has none under a cursor, so magit answers with a prompt
/// rather than with a predicate — its own `magit-cherry-pick` /
/// `magit-revert` / `magit-reset` sit in the **ungated** group of
/// `magit-dispatch` precisely because they are transients that ask.
/// This is that ask.
///
/// **The argument is an ex-command name, not an action name**, and
/// that is forced rather than chosen. A picked candidate can only
/// reach an operation through `RoutingPayload::InvokeCommand`, whose
/// host arm destructures its `args` away
/// (`InvokeCommand { id, .. }`) and runs `id` as an ex line — so the
/// commit has to travel *inside* the line, and the thing on the other
/// end has to be an ex-command. Every [`CommitOp`] carries its own
/// `ex_command` for exactly this.
pub struct CommitPickSource {
    spec: PickerSourceSpec,
}

/// Spec for a source that **takes the ex-command to run on the pick**.
///
/// Every "pick a thing, then act on it" source here shares one shape,
/// because a picked candidate reaches an operation only through
/// `RoutingPayload::InvokeCommand` — the host runs its `id` as an ex
/// line — so the operation has to arrive as an argument.
///
/// Declaring that argument is not paperwork. `args_schema` is what
/// `:picker <id> <Tab>` completes against and `args_hint` is what the
/// command line shows while you type one; a source that omits them
/// while its `init` rejects an empty `args` advertises "nothing to
/// type here" and then refuses to open. `a_source_that_needs_an_ex_command_declares_it`
/// holds the two together by asking every registered source to `init`
/// with no arguments and requiring the refusals to be exactly the
/// declarations.
fn takes_ex_command(id: &'static str, doc: &'static str, noun: &'static str) -> PickerSourceSpec {
    PickerSourceSpec {
        id: std::borrow::Cow::Borrowed(id),
        doc: std::borrow::Cow::Borrowed(doc),
        args_schema: vec![lattice_grammar::ArgSpec::required(
            "command",
            lattice_grammar::ArgKind::String,
            noun,
        )],
        args_hint: std::borrow::Cow::Borrowed("<magit ex-command>"),
        live: false,
    }
}

impl CommitPickSource {
    pub fn new() -> Self {
        Self {
            spec: takes_ex_command(
                COMMIT_PICK_SOURCE,
                "Pick a commit, then run the named magit ex-command on it.",
                "magit ex-command to run on the picked commit",
            ),
        }
    }
}

impl Default for CommitPickSource {
    fn default() -> Self {
        Self::new()
    }
}

/// The source id, shared by the generator and every
/// `Effect::OpenPicker` that names it — the drift `BranchPickBaseSource`'s
/// own test exists to catch, avoided here by there being one constant.
pub const COMMIT_PICK_SOURCE: &str = "magit-commit";

/// How many commits the picker offers. Bounded because this runs
/// `git log` synchronously on a blocking thread and a repository's
/// history has no upper size — the list is for picking a recent
/// commit, and anything older is reachable by typing the sha into the
/// ex-command directly.
const COMMIT_PICK_LIMIT: usize = 200;

impl PickerSourceGenerator for CommitPickSource {
    fn spec(&self) -> &PickerSourceSpec {
        &self.spec
    }

    fn init(&self, _ctx: &PickerContext<'_>, args: &[String]) -> SourceResult<PickerInitResult> {
        let command = args
            .first()
            .filter(|c| !c.is_empty())
            .ok_or_else(|| {
                format!("{COMMIT_PICK_SOURCE}: needs the ex-command to run on the picked commit")
            })?
            .clone();
        Ok(PickerInitResult::Future(Box::pin(async move {
            let rows = tokio::task::spawn_blocking(recent_commits)
                .await
                .map_err(|e| format!("{COMMIT_PICK_SOURCE}: join error: {e}"))??;
            Ok(rows
                .into_iter()
                .map(|(sha, display)| {
                    let cand = RawCandidate::plain(display, CandidateKind::Plain);
                    (
                        cand,
                        // The full sha, not the abbreviation shown: an
                        // abbreviation is ambiguous in principle and
                        // git resolves the ambiguity by refusing, which
                        // would surface as a picked commit that did
                        // nothing.
                        RoutingPayload::InvokeCommand {
                            id: picked_line(&command, &sha),
                            args: lattice_grammar::args::Args::None,
                        },
                    )
                })
                .collect())
        })))
    }

    fn accept(
        &self,
        _ctx: &PickerContext<'_>,
        routing: &RoutingPayload,
    ) -> SourceResult<PickerAcceptOutcome> {
        match routing {
            RoutingPayload::InvokeCommand { id, args } => Ok(PickerAcceptOutcome::InvokeCommand {
                id: id.clone(),
                args: args.clone(),
            }),
            other => Err(format!(
                "{COMMIT_PICK_SOURCE}: unexpected routing payload {other:?}"
            )),
        }
    }
}

/// Pick a stash, then run the named magit ex-command on it.
///
/// The stash peer of [`CommitPickSource`], and it exists for the same
/// reason: the stash chords resolve the stash under the cursor, and a
/// dispatch menu opened from an ordinary file has no cursor to read.
/// Before this the menu's apply / pop / drop / show rows rendered,
/// fired, resolved nothing and returned — a visible row that did
/// nothing, silently, which is the failure `BranchCheckoutSource`'s
/// note calls out for `<CR>` in the branch buffer.
///
/// The pick is the stash's **index**, not its message: `git stash`
/// addresses entries as `stash@{N}` and messages are neither unique
/// nor stable. The display carries the message because that is what a
/// person remembers, exactly as the commit picker shows the subject.
pub struct StashPickSource {
    spec: PickerSourceSpec,
}

impl StashPickSource {
    pub fn new() -> Self {
        Self {
            spec: takes_ex_command(
                STASH_PICK_SOURCE,
                "Pick a stash, then run the named magit ex-command on it.",
                "magit ex-command to run on the picked stash",
            ),
        }
    }
}

impl Default for StashPickSource {
    fn default() -> Self {
        Self::new()
    }
}

pub const STASH_PICK_SOURCE: &str = "magit-stash-pick";

impl PickerSourceGenerator for StashPickSource {
    fn spec(&self) -> &PickerSourceSpec {
        &self.spec
    }

    fn init(&self, _ctx: &PickerContext<'_>, args: &[String]) -> SourceResult<PickerInitResult> {
        let command = args
            .first()
            .filter(|c| !c.is_empty())
            .ok_or_else(|| {
                format!("{STASH_PICK_SOURCE}: needs the ex-command to run on the picked stash")
            })?
            .clone();
        Ok(PickerInitResult::Future(Box::pin(async move {
            let entries = tokio::task::spawn_blocking(|| {
                let repo = Repository::discover(".")
                    .map_err(|e| format!("{STASH_PICK_SOURCE}: repo discover failed: {e}"))?;
                lattice_vcs::Stash::list(&repo).map_err(|e| format!("{STASH_PICK_SOURCE}: {e}"))
            })
            .await
            .map_err(|e| format!("{STASH_PICK_SOURCE}: join error: {e}"))??;
            Ok(entries
                .into_iter()
                .map(|entry| {
                    let cand = RawCandidate::plain(
                        format!("stash@{{{}}} {}", entry.index, entry.message),
                        CandidateKind::Plain,
                    );
                    (
                        cand,
                        RoutingPayload::InvokeCommand {
                            id: picked_line(&command, &entry.index.to_string()),
                            args: lattice_grammar::args::Args::None,
                        },
                    )
                })
                .collect())
        })))
    }

    fn accept(
        &self,
        _ctx: &PickerContext<'_>,
        routing: &RoutingPayload,
    ) -> SourceResult<PickerAcceptOutcome> {
        match routing {
            RoutingPayload::InvokeCommand { id, args } => Ok(PickerAcceptOutcome::InvokeCommand {
                id: id.clone(),
                args: args.clone(),
            }),
            other => Err(format!(
                "{STASH_PICK_SOURCE}: unexpected routing payload {other:?}"
            )),
        }
    }
}

/// `(full-sha, display-row)` for the most recent commits, newest
/// first.
///
/// The display is `<short> <subject>` — what a log row looks like, so
/// the fuzzy filter matches on the subject, which is what anyone
/// actually remembers about a commit.
fn recent_commits() -> Result<Vec<(String, String)>, String> {
    let out = std::process::Command::new("git")
        .args([
            "log",
            &format!("-n{COMMIT_PICK_LIMIT}"),
            "--format=%H%x00%h %s",
        ])
        .output()
        .map_err(|e| format!("{COMMIT_PICK_SOURCE}: {e}"))?;
    if !out.status.success() {
        return Err(format!("{COMMIT_PICK_SOURCE}: git log failed"));
    }
    Ok(parse_commit_rows(&String::from_utf8_lossy(&out.stdout)))
}

/// Split `git log`'s NUL-separated `(sha, display)` pairs.
///
/// Pure so the shape is testable without a repository — and the shape
/// is what matters, because a row whose sha and display got swapped
/// would show a readable list that cherry-picks the wrong thing.
pub(crate) fn parse_commit_rows(raw: &str) -> Vec<(String, String)> {
    raw.lines()
        .filter_map(|line| {
            let (sha, display) = line.split_once('\0')?;
            (!sha.is_empty() && !display.is_empty()).then(|| (sha.to_string(), display.to_string()))
        })
        .collect()
}

#[cfg(test)]
mod commit_pick {
    use super::*;

    #[test]
    fn spec_id_matches_the_name_every_open_effect_uses() {
        let source = CommitPickSource::new();
        assert_eq!(source.spec().id, COMMIT_PICK_SOURCE);
        assert_eq!(
            source.spec().args_schema.len(),
            1,
            "the ex-command to run is the source's one argument"
        );
    }

    /// The sha must be the full one and it must reach the ex line.
    ///
    /// `InvokeCommand`'s `args` field was a dead end when this was
    /// written — the host destructured it away — so the value had to be
    /// *in* the line. That is fixed (2026-08-03), but the line form is
    /// kept: it is the exact text a user would type.
    #[test]
    fn a_picked_commit_becomes_the_ex_line_that_acts_on_it() {
        let rows = parse_commit_rows("abc123def\0abc123d fix the thing\n");
        assert_eq!(rows.len(), 1);
        let (sha, display) = &rows[0];
        assert_eq!(sha, "abc123def", "the FULL sha, not the abbreviation");
        assert_eq!(display, "abc123d fix the thing");
        assert_eq!(
            format!("magit-cherry-pick {sha}"),
            "magit-cherry-pick abc123def"
        );
    }

    /// Sha and display must not swap: a swapped row renders a
    /// perfectly readable list that cherry-picks the wrong thing.
    #[test]
    fn rows_keep_the_sha_and_the_display_on_their_own_sides() {
        // `\x00`, not `\0`: the separator is immediately followed by a
        // digit, which makes `\01` read as an octal escape.
        let rows = parse_commit_rows("1111111111\x001111111 first\n2222222222\x002222222 second\n");
        assert_eq!(rows[0].0, "1111111111");
        assert!(rows[0].1.ends_with("first"));
        assert_eq!(rows[1].0, "2222222222");
        assert!(rows[1].1.ends_with("second"));
    }

    #[test]
    fn malformed_rows_are_dropped_rather_than_half_parsed() {
        assert!(parse_commit_rows("no-nul-here\n").is_empty());
        assert!(parse_commit_rows("\0only-display\n").is_empty());
        assert!(parse_commit_rows("only-sha\0\n").is_empty());
    }

    /// Every `CommitOp` names an ex-command, and the picker fires it
    /// by that name — so an op whose name did not match a registered
    /// command would produce a picker that picks and does nothing.
    /// Pinned against the literal strings the registration uses.
    #[test]
    fn every_commit_op_names_a_distinct_ex_command() {
        use crate::magit_global_mode::CommitOp;
        let ops = [
            CommitOp::CHERRY_PICK,
            CommitOp::REVERT,
            CommitOp::RESET_SOFT,
            CommitOp::RESET_MIXED,
            CommitOp::RESET_HARD,
        ];
        let names: Vec<&str> = ops.iter().map(|o| o.ex_command).collect();
        assert_eq!(
            names,
            [
                "magit-cherry-pick",
                "magit-revert",
                "magit-reset-soft",
                "magit-reset-mixed",
                "magit-reset-hard"
            ]
        );
        let unique: std::collections::HashSet<_> = names.iter().collect();
        assert_eq!(unique.len(), names.len(), "one command per op");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// MG.32: **magit owns the inventory of magit's picker sources.**
    ///
    /// This assertion used to live in `lattice-ui-tui`'s
    /// `gen_picker_sources_emits_candidate_per_registered_source`, as
    /// part of one hardcoded list of every first-party source. That
    /// list taxed each crate that added a source and could be owned by
    /// none of them, so it rotted: MG.29 registered
    /// `magit-branch-checkout-pick` without extending it and left that
    /// test failing on `main` until MG.32 noticed. Here, adding a
    /// source and updating its test are the same edit in the same
    /// crate.
    ///
    /// Every id is also pinned against the constant the `Effect::
    /// OpenPicker` handler names, so a renamed source cannot leave a
    /// menu row opening a picker that no longer exists.
    #[test]
    fn magit_registers_exactly_the_sources_its_rows_open() {
        let mut registry = lattice_picker::PickerRegistry::new();
        register(&mut registry, None);
        let mut ids: Vec<&str> = registry.ids().collect();
        ids.sort_unstable();

        assert_eq!(
            ids,
            vec![
                BRANCH_PICK_SOURCE, // MG.52: `b b`, and every
                //                                   other "which branch?"
                BRANCH_CHECKOUT_SOURCE,           // MG.29: `b l`
                BRANCH_CREATE_NO_CHECKOUT_SOURCE, // MG.32: `b n`
                BRANCH_DELETE_SOURCE,             // MG.32: `b x`
                "magit-branch-pick-base",         // `b c`, and `c` in the branch buffer
                BRANCH_RENAME_SOURCE,             // MG.32: `b m`
                COMMIT_PICK_SOURCE,               // MG.23j: `A` / `_` / `O`
                REF_PICK_SOURCE,                  // MG.53.e: any ref
                REMOTE_PICK_SOURCE,               // MG.53.d: tag prune
                REVISION_PICK_SOURCE,             // MG.53.g: refs + commits
                STASH_PICK_SOURCE,                // `z a`/`z p`/`z k`/`z v`
                TAG_PICK_SOURCE,                  // MG.53.d: tag delete
            ],
            "magit's registered picker sources changed — update this list \
             together with `register`, and check every `Effect::OpenPicker` \
             that names one"
        );
    }

    /// A `PickerContext` with nothing in it.
    ///
    /// Every magit source takes `_ctx` — each asks git, not the editor —
    /// so an empty snapshot exercises them exactly as the real one does.
    fn empty_picker_ctx(buffer: &lattice_core::Buffer) -> lattice_picker::PickerContext<'_> {
        lattice_picker::PickerContext {
            active_buffer: lattice_picker::ActiveBufferSnapshot {
                buffer_id: 0,
                path: None,
                language: None,
                cursor: lattice_protocol::position::Position::new(0, 0),
                selection: None,
                buffer,
                syntax_symbols: Vec::new(),
                syntax_highlights: Vec::new(),
            },
            workspace_root: std::path::PathBuf::from("."),
            recent_files: &[],
            position_history: Vec::new(),
            buffers: Vec::new(),
            marks: Vec::new(),
            registers: Vec::new(),
            yank_ring: Vec::new(),
            active_modes: Vec::new(),
            command_history: Vec::new(),
            search_history: Vec::new(),
            pane_buffer_history: Vec::new(),
        }
    }

    /// A source that **requires** an ex-command must say so in its spec.
    ///
    /// `spec.args_schema` is not decoration: `:picker <id> <Tab>` reads
    /// it to offer the argument, and `args_hint` is what the command
    /// line shows while you are typing one. A source whose `init`
    /// rejects an empty `args` while its spec claims `no_args` tells the
    /// user there is nothing to type and then refuses the pick — the
    /// failure is only visible at the moment the picker declines to
    /// open, which is the worst moment to learn about an argument.
    ///
    /// Asserted by *behaviour*, not by reading the schema back: each
    /// source is asked to `init` with no arguments, and the ones that
    /// refuse must be exactly the ones declaring a required arg.
    #[test]
    fn a_source_that_needs_an_ex_command_declares_it() {
        let mut registry = lattice_picker::PickerRegistry::new();
        register(&mut registry, None);
        let buffer = lattice_core::Buffer::empty();
        let ctx = empty_picker_ctx(&buffer);
        let ids: Vec<String> = registry.ids().map(str::to_string).collect();
        for id in ids {
            let id = id.as_str();
            let source = registry.generator(id).expect("just enumerated");
            let declares = source
                .spec()
                .args_schema
                .first()
                .is_some_and(|a| matches!(a.default, lattice_grammar::ArgDefault::Required));
            let refuses = source.init(&ctx, &[]).is_err();
            assert_eq!(
                declares, refuses,
                "`{id}`: spec declares a required arg = {declares}, but \
                 init with no args refuses = {refuses}. These must agree — \
                 a source that needs an ex-command has to advertise it so \
                 `:picker {id} <Tab>` can complete one."
            );
            if declares {
                assert!(
                    !source.spec().args_hint.is_empty(),
                    "`{id}` declares a required arg but shows no args_hint, \
                     so the command line has nothing to display while the \
                     user types it"
                );
            }
        }
    }

    /// MG.53.g: **a revision is not only a commit.**
    ///
    /// `git log` answers "which commit on the branch I am on", which is
    /// the wrong question for *view this file as it is on
    /// `origin/main`*: a file that lives on another branch is not in
    /// this branch's history at all, so no number of commits would
    /// surface it. The scopes are what keep those two questions apart.
    #[test]
    fn the_revision_scope_spans_refs_and_commits_and_the_others_do_not() {
        use super::{RefPickSource, RefScope};
        // Distinct ids, so a row naming one cannot silently get another.
        let ids: Vec<String> = [
            RefScope::Tags,
            RefScope::Remotes,
            RefScope::AllRefs,
            RefScope::Revisions,
        ]
        .into_iter()
        .map(|sc| RefPickSource::new(sc).spec().id.to_string())
        .collect();
        let mut sorted = ids.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(
            sorted.len(),
            ids.len(),
            "each scope registers under its own id: {ids:?}"
        );
        assert_eq!(
            RefPickSource::new(RefScope::Revisions).spec().id,
            super::REVISION_PICK_SOURCE
        );
        // The commit-only picker is a DIFFERENT source and stays so —
        // cherry-pick / revert / reset genuinely want a commit, and
        // offering them a branch would be offering the wrong noun.
        assert_ne!(super::REVISION_PICK_SOURCE, super::COMMIT_PICK_SOURCE);
    }

    /// MG.53.c: **the pick does not always go last.**
    ///
    /// `magit-find-file` is `<rev> <path>`, so appending a revision to
    /// `magit-find-file src/main.rs` would produce `<path> <rev>` and
    /// open a file named after a sha. The alternative to a placeholder
    /// was an order-adapter ex-command duplicating an operation that
    /// already exists, purely to move an argument.
    #[test]
    fn a_placeholder_places_the_pick_and_absence_appends_it() {
        assert_eq!(
            super::picked_line("magit-find-file {} src/main.rs", "abc123"),
            "magit-find-file abc123 src/main.rs",
            "the pick goes where the placeholder is"
        );
        assert_eq!(
            super::picked_line("magit-checkout", "main"),
            "magit-checkout main",
            "no placeholder — appended, which is what every branch row does"
        );
        // Only the first is substituted: a command mentioning `{}` twice
        // wants one value in one slot, not the same value twice.
        assert_eq!(
            super::picked_line("cmd {} {}", "x"),
            "cmd x {}",
            "one pick fills one slot"
        );
    }

    /// MG.54: the preview answers `magit-find-file` and nothing else.
    ///
    /// The same `magit-revision` source fills `magit-checkout` (moves
    /// HEAD) and `magit-file-checkout` (overwrites the working tree).
    /// Showing a file's content beside either invites reading the pane
    /// as "this is what you'll get", when what you get is that content
    /// written over uncommitted work. This is the guard, so it is pinned
    /// against every line the menu rows actually build.
    #[test]
    fn only_the_find_file_line_is_previewable() {
        use super::find_file_preview_target;
        let (rev, path) =
            find_file_preview_target("magit-find-file abc123 src/main.rs").expect("previewable");
        assert_eq!(rev, "abc123");
        assert_eq!(path, std::path::Path::new("src/main.rs"));

        // The lines the other two rows build, verbatim from
        // `magit_global_mode` (`picked_line` has already substituted).
        assert!(
            find_file_preview_target("magit-checkout main").is_none(),
            "a checkout is an action, not a question about a file"
        );
        assert!(
            find_file_preview_target("magit-file-checkout abc123 src/main.rs").is_none(),
            "file-checkout OVERWRITES that path — previewing it reads as a promise"
        );
        // Malformed / partial lines refuse rather than fetching `HEAD:`.
        assert!(find_file_preview_target("magit-find-file abc123").is_none());
        assert!(find_file_preview_target("magit-find-file  ").is_none());
        assert!(find_file_preview_target("magit-find-files x y").is_none());
    }

    /// A path with spaces survives: everything after the revision is the
    /// path, because splitting on every space would truncate it.
    #[test]
    fn a_path_with_spaces_is_kept_whole() {
        let (rev, path) = super::find_file_preview_target("magit-find-file HEAD my dir/a b.rs")
            .expect("previewable");
        assert_eq!(rev, "HEAD");
        assert_eq!(path, std::path::Path::new("my dir/a b.rs"));
    }

    /// The window and the fetch are gated by the SAME switch. If they
    /// could disagree, turning the option off would still arm a timer on
    /// every arrow key (or worse, leave the fetch reachable inline).
    #[test]
    fn the_option_gates_the_window_and_the_fetch_together() {
        use super::{RefPickSource, RefScope};
        use lattice_picker::PickerSourceGenerator;

        let config = std::sync::Arc::new(lattice_config::ConfigRegistry::new());
        // `options! { … }` is a compile-time declaration; this is what
        // makes it a runtime fact in a registry.
        config.init_from_linkme();
        let src =
            RefPickSource::with_config(RefScope::Revisions, Some(std::sync::Arc::clone(&config)));
        assert!(
            src.preview_debounce().is_some(),
            "on by default ⇒ the settle window is declared"
        );

        config
            .set_typed::<crate::options::MagitRevisionPreview>(false)
            .expect("option is registered");
        assert!(
            src.preview_debounce().is_none(),
            "off ⇒ no window, so no timer per selection move either"
        );
    }

    /// The other scopes never preview, whatever the option says — they
    /// list tags / remotes / refs, and none of those is a file.
    #[test]
    fn only_the_revision_scope_declares_a_settle_window() {
        use super::{RefPickSource, RefScope};
        use lattice_picker::PickerSourceGenerator;
        for scope in [RefScope::Tags, RefScope::Remotes, RefScope::AllRefs] {
            assert!(
                RefPickSource::new(scope).preview_debounce().is_none(),
                "{scope:?} has nothing to preview"
            );
        }
    }

    #[test]
    fn spec_id_matches_the_effect_openpicker_source_name() {
        // magit_branch_mode's `c` handler hardcodes this string in
        // `Effect::OpenPicker { source: "magit-branch-pick-base", .. }`
        // — this assertion is the tripwire if the id ever drifts.
        let source = BranchPickBaseSource::new();
        assert_eq!(source.spec().id, "magit-branch-pick-base");
        assert!(source.spec().args_schema.is_empty());
    }

    /// MG.32: every branch flow that stashes its target in a prompt
    /// buffer's NAME must be readable back by the finish handler that
    /// consumes it.
    ///
    /// This is the MG.15 failure class, and it is silent: the writer
    /// lives here and the reader lives in `magit_global_mode`, so a
    /// prefix changed on one side leaves the other returning `None` —
    /// the prompt opens, you type a name, and nothing happens. Both
    /// sides now spell the prefix through one constant, and this
    /// round-trips through the real functions rather than through a
    /// literal that could itself drift.
    #[test]
    fn every_branch_prompt_name_round_trips_to_its_reader() {
        use lattice_picker::PickerAcceptOutcome as O;

        // Names with a `/` and a `-` — both appear in real branch names
        // and both would break a naive split-on-delimiter parser.
        for branch in ["feature/foo", "release-1.2", "main"] {
            let cases: Vec<(&str, PickerAcceptOutcome, &str)> = vec![
                (
                    "create",
                    branch_create_prompt_outcome(branch),
                    crate::magit_global_mode::BRANCH_CREATE_PROMPT_PREFIX,
                ),
                (
                    "create-no-checkout",
                    branch_create_no_checkout_outcome(branch),
                    crate::magit_global_mode::BRANCH_CREATE_NO_CHECKOUT_PROMPT_PREFIX,
                ),
                (
                    "rename",
                    branch_rename_outcome(branch),
                    crate::magit_global_mode::BRANCH_RENAME_PROMPT_PREFIX,
                ),
            ];
            for (what, outcome, prefix) in cases {
                let O::OpenPrompt { buffer_name, .. } = outcome else {
                    panic!("{what} must open a prompt");
                };
                let name = buffer_name.unwrap_or_else(|| panic!("{what} must name its buffer"));
                assert_eq!(
                    crate::magit_global_mode::branch_from_prompt_buffer_name_for_test(
                        &name, prefix
                    ),
                    Some(branch.to_string()),
                    "{what}'s prompt-buffer name `{name}` must read back as `{branch}`"
                );
            }
        }
    }

    /// The three prefixes must be mutually non-ambiguous, or a finish
    /// handler reads a name a different flow wrote and acts on the
    /// wrong branch with the wrong operation.
    ///
    /// Not hypothetical: `*magit:branch-create-from:` and
    /// `*magit:branch-create-nocheckout-from:` are one hyphen apart, and
    /// had the second been spelled `…create-from-nocheckout:` the
    /// create parser would match it and silently check the branch out.
    #[test]
    fn the_branch_prompt_prefixes_cannot_match_each_others_names() {
        use crate::magit_global_mode as g;
        let prefixes = [
            g::BRANCH_CREATE_PROMPT_PREFIX,
            g::BRANCH_CREATE_NO_CHECKOUT_PROMPT_PREFIX,
            g::BRANCH_RENAME_PROMPT_PREFIX,
        ];
        for writer in prefixes {
            let name = format!("{writer}topic*");
            for reader in prefixes {
                let parsed = g::branch_from_prompt_buffer_name_for_test(&name, reader);
                if writer == reader {
                    assert_eq!(parsed, Some("topic".to_string()));
                } else {
                    assert_eq!(
                        parsed, None,
                        "`{reader}` must not match a name written by `{writer}`"
                    );
                }
            }
        }
    }

    #[test]
    fn branch_delete_asks_through_the_ex_command_rather_than_deleting() {
        use lattice_picker::PickerAcceptOutcome as O;
        let O::InvokeCommand { id, args } = branch_delete_outcome("feature/foo") else {
            panic!("delete must route through a command");
        };
        assert_eq!(
            id, "magit-branch-delete",
            "the ex-command is what raises the MG.12 confirm; routing anywhere \
             else would delete without asking"
        );
        assert_eq!(args, lattice_grammar::Args::String("feature/foo".into()));
    }

    /// The rename prompt is pre-filled with the current name — a rename
    /// is usually an edit of it, not a fresh name typed from nothing.
    #[test]
    fn branch_rename_prefills_the_current_name() {
        use lattice_picker::PickerAcceptOutcome as O;
        let O::OpenPrompt {
            prompt, initial, ..
        } = branch_rename_outcome("feature/foo")
        else {
            panic!("rename must open a prompt");
        };
        assert_eq!(initial, "feature/foo");
        assert!(
            prompt.contains("feature/foo"),
            "the prompt names its target"
        );
    }

    #[test]
    fn branch_create_prompt_outcome_names_the_base_in_the_label_and_buffer_name() {
        let outcome = branch_create_prompt_outcome("feature/foo");
        match outcome {
            PickerAcceptOutcome::OpenPrompt {
                prompt,
                initial,
                on_submit_action,
                buffer_name,
            } => {
                assert!(prompt.contains("feature/foo"));
                assert_eq!(initial, "");
                assert_eq!(on_submit_action, "action:magit-branch-create-finish");
                assert_eq!(
                    buffer_name.as_deref(),
                    Some("*magit:branch-create-from:feature/foo*")
                );
            }
            other => panic!("expected OpenPrompt, got {other:?}"),
        }
    }
}

/// MG.52: the **branch picker**, parameterised by the ex-command it
/// hands the picked branch to.
///
/// One source, not one per operation. Checkout, merge, merge-into,
/// reset and rebase-onto all ask the same question — *which branch* —
/// and differ only in what they do with the answer, which is exactly
/// what an argument is for. [`CommitPickSource`] already established
/// this shape for commits; this is its peer for branches, down to the
/// same constraint on the argument.
///
/// **Branch selection does not accept free text.** A branch that does
/// not exist is not a merge target, a base to branch from, or a reset
/// destination — it is a typo, and git's error arrives long after the
/// keystroke that caused it. The prompt these replace accepted anything
/// and reported the mistake as a failed git call.
///
/// **The argument is an ex-command name, not an action name**, for the
/// reason [`CommitPickSource`] documents: a picked candidate reaches an
/// operation only through `RoutingPayload::InvokeCommand`, whose host
/// arm runs `id` as an ex line, so the branch has to travel inside that
/// line.
pub struct BranchPickSource {
    spec: PickerSourceSpec,
}

pub const BRANCH_PICK_SOURCE: &str = "magit-branch";

impl BranchPickSource {
    pub fn new() -> Self {
        Self {
            spec: takes_ex_command(
                BRANCH_PICK_SOURCE,
                "Pick a branch, then run the named magit ex-command on it.",
                "magit ex-command to run on the picked branch",
            ),
        }
    }
}

impl Default for BranchPickSource {
    fn default() -> Self {
        Self::new()
    }
}

impl PickerSourceGenerator for BranchPickSource {
    fn spec(&self) -> &PickerSourceSpec {
        &self.spec
    }

    fn init(&self, _ctx: &PickerContext<'_>, args: &[String]) -> SourceResult<PickerInitResult> {
        let command = args
            .first()
            .filter(|c| !c.is_empty())
            .ok_or_else(|| {
                format!("{BRANCH_PICK_SOURCE}: needs the ex-command to run on the picked branch")
            })?
            .clone();
        Ok(PickerInitResult::Future(Box::pin(async move {
            let branches = tokio::task::spawn_blocking(|| {
                let repo = Repository::discover(".")
                    .map_err(|e| format!("{BRANCH_PICK_SOURCE}: repo discover failed: {e}"))?;
                Branch::list(&repo).map_err(|e| format!("{BRANCH_PICK_SOURCE}: {e}"))
            })
            .await
            .map_err(|e| format!("{BRANCH_PICK_SOURCE}: join error: {e}"))??;
            Ok(branches
                .into_iter()
                .map(|name| {
                    let cand = RawCandidate::plain(name.clone(), CandidateKind::Plain);
                    (
                        cand,
                        RoutingPayload::InvokeCommand {
                            id: picked_line(&command, &name),
                            args: lattice_grammar::Args::None,
                        },
                    )
                })
                .collect())
        })))
    }

    fn accept(
        &self,
        _ctx: &PickerContext<'_>,
        routing: &RoutingPayload,
    ) -> SourceResult<PickerAcceptOutcome> {
        match routing {
            RoutingPayload::InvokeCommand { id, args } => Ok(PickerAcceptOutcome::InvokeCommand {
                id: id.clone(),
                args: args.clone(),
            }),
            other => Err(format!(
                "{BRANCH_PICK_SOURCE}: unexpected routing payload {other:?}"
            )),
        }
    }
}

/// MG.53.d/e: the **tag**, **remote** and **ref** pickers.
///
/// Three ids, one implementation, because they differ only in what they
/// list. `Reference::list` already returns branches, remotes and tags
/// in one `for-each-ref` walk tagged with a [`RefKind`], so a tag
/// picker is that walk filtered — building a separate `git tag` call
/// beside it would be a second way to ask the same question.
///
/// Parameterised by the ex-command that receives the pick, exactly as
/// [`BranchPickSource`] and [`CommitPickSource`] are, and for the same
/// reason: a picked candidate reaches an operation only as an ex line.
pub struct RefPickSource {
    spec: PickerSourceSpec,
    which: RefScope,
    /// MG.54: read at PREVIEW time for `magit.revision-preview`, so
    /// `:set` lands on the next selection rather than the next picker.
    /// `None` in a stripped harness (no config registry), which resolves
    /// to the option's own default — a test rig should behave like a
    /// default install.
    config: Option<Arc<lattice_config::ConfigRegistry>>,
}

/// What a [`RefPickSource`] lists.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefScope {
    /// `refs/tags/*` — `Delete tag`.
    Tags,
    /// Configured remotes (`git remote`), not `refs/remotes/*`: the
    /// operations that take one want `origin`, not `origin/main`.
    Remotes,
    /// Everything `for-each-ref` returns — branches, remote-tracking
    /// refs and tags. What a "ref" prompt means.
    AllRefs,
    /// MG.53.g: refs **and** recent commits — what "revision" means.
    ///
    /// `git log` alone answers "which commit on the branch I am on",
    /// which is the wrong question for *view this file as it is on
    /// `origin/main`*: a file on another branch is not in the current
    /// branch's history at all, so no number of commits would surface
    /// it. Emacs's `magit-find-file` completes over branches, tags and
    /// commits together for the same reason.
    ///
    /// Refs come first: reaching for another branch is the common ask,
    /// and a branch name is the thing a user can recognise.
    Revisions,
}

pub const TAG_PICK_SOURCE: &str = "magit-tag";
pub const REMOTE_PICK_SOURCE: &str = "magit-remote";
pub const REF_PICK_SOURCE: &str = "magit-ref";
pub const REVISION_PICK_SOURCE: &str = "magit-revision";

impl RefPickSource {
    pub fn new(which: RefScope) -> Self {
        Self::with_config(which, None)
    }

    pub fn with_config(
        which: RefScope,
        config: Option<Arc<lattice_config::ConfigRegistry>>,
    ) -> Self {
        let (id, doc, noun) = match which {
            RefScope::Tags => (
                TAG_PICK_SOURCE,
                "Pick a tag, then run the named magit ex-command on it.",
                "magit ex-command to run on the picked tag",
            ),
            RefScope::Remotes => (
                REMOTE_PICK_SOURCE,
                "Pick a remote, then run the named magit ex-command on it.",
                "magit ex-command to run on the picked remote",
            ),
            RefScope::AllRefs => (
                REF_PICK_SOURCE,
                "Pick a ref, then run the named magit ex-command on it.",
                "magit ex-command to run on the picked ref",
            ),
            RefScope::Revisions => (
                REVISION_PICK_SOURCE,
                "Pick a revision — a branch, tag or recent commit — then run \
                 the named magit ex-command on it.",
                "magit ex-command to run on the picked revision",
            ),
        };
        Self {
            spec: takes_ex_command(id, doc, noun),
            which,
            config,
        }
    }

    fn id(&self) -> &'static str {
        match self.which {
            RefScope::Tags => TAG_PICK_SOURCE,
            RefScope::Remotes => REMOTE_PICK_SOURCE,
            RefScope::AllRefs => REF_PICK_SOURCE,
            RefScope::Revisions => REVISION_PICK_SOURCE,
        }
    }

    /// MG.54: is the revision preview switched on? A missing config
    /// registry resolves to the option's default, not `false`.
    fn preview_enabled(&self) -> bool {
        self.config
            .as_ref()
            .and_then(|c| c.get_typed::<crate::options::MagitRevisionPreview>())
            .map(|v| *v)
            .unwrap_or(true)
    }
}

/// MG.54: the ex-line this picker will run, split into the revision and
/// the file — but ONLY for `magit-find-file`, the one command whose
/// answer is a file's content.
///
/// The same `magit-revision` source also fills `magit-checkout` and
/// `magit-file-checkout`. Those are *actions*: one moves HEAD, the other
/// overwrites the working tree. Previewing a checkout would mean showing
/// a file the command is about to replace, which invites reading the
/// pane as "this is what you'll get" when what you get is the file
/// written over your uncommitted work. Matching on the command name is
/// what keeps the preview to the question it can actually answer.
fn find_file_preview_target(line: &str) -> Option<(String, PathBuf)> {
    let rest = line.strip_prefix("magit-find-file ")?;
    let (rev, path) = rest.trim().split_once(char::is_whitespace)?;
    let path = path.trim();
    (!rev.is_empty() && !path.is_empty()).then(|| (rev.to_string(), PathBuf::from(path)))
}

impl PickerSourceGenerator for RefPickSource {
    fn spec(&self) -> &PickerSourceSpec {
        &self.spec
    }

    fn init(&self, _ctx: &PickerContext<'_>, args: &[String]) -> SourceResult<PickerInitResult> {
        let id = self.id();
        let which = self.which;
        let command = args
            .first()
            .filter(|c| !c.is_empty())
            .ok_or_else(|| format!("{id}: needs the ex-command to run on the pick"))?
            .clone();
        Ok(PickerInitResult::Future(Box::pin(async move {
            // (display, value): they differ for commits, where the row
            // must read as `abbrev subject` while what git receives is
            // the full sha — an abbreviation is ambiguous in principle
            // and git resolves the ambiguity by refusing.
            let names = tokio::task::spawn_blocking(move || {
                let repo = Repository::discover(".")
                    .map_err(|e| format!("{id}: repo discover failed: {e}"))?;
                Ok::<Vec<(String, String)>, String>(match which {
                    RefScope::Remotes => Remote::list(&repo)
                        .map_err(|e| format!("{id}: {e}"))?
                        .into_iter()
                        .map(|r| (r.name.clone(), r.name))
                        .collect(),
                    RefScope::Tags => Reference::list(&repo)
                        .map_err(|e| format!("{id}: {e}"))?
                        .into_iter()
                        .filter(|r| r.kind == RefKind::Tag)
                        .map(|r| (r.name.clone(), r.name))
                        .collect(),
                    RefScope::AllRefs => Reference::list(&repo)
                        .map_err(|e| format!("{id}: {e}"))?
                        .into_iter()
                        .map(|r| (r.name.clone(), r.name))
                        .collect(),
                    // Refs first, then commits: `origin/main` is what
                    // someone reaching for another branch recognises,
                    // and a sha they would have to read to identify.
                    RefScope::Revisions => {
                        let mut out: Vec<(String, String)> = Reference::list(&repo)
                            .map_err(|e| format!("{id}: {e}"))?
                            .into_iter()
                            .map(|r| (r.name.clone(), r.name))
                            .collect();
                        out.extend(
                            recent_commits()?
                                .into_iter()
                                .map(|(sha, display)| (display, sha)),
                        );
                        out
                    }
                })
            })
            .await
            .map_err(|e| format!("{id}: join error: {e}"))??;
            Ok(names
                .into_iter()
                .map(|(display, value)| {
                    let cand = RawCandidate::plain(display, CandidateKind::Plain);
                    (
                        cand,
                        RoutingPayload::InvokeCommand {
                            id: picked_line(&command, &value),
                            args: lattice_grammar::Args::None,
                        },
                    )
                })
                .collect())
        })))
    }

    fn accept(
        &self,
        _ctx: &PickerContext<'_>,
        routing: &RoutingPayload,
    ) -> SourceResult<PickerAcceptOutcome> {
        match routing {
            RoutingPayload::InvokeCommand { id, args } => Ok(PickerAcceptOutcome::InvokeCommand {
                id: id.clone(),
                args: args.clone(),
            }),
            other => Err(format!(
                "{}: unexpected routing payload {other:?}",
                self.id()
            )),
        }
    }

    /// MG.54: `C-c f v` used to show nothing until you accepted, so
    /// choosing between two revisions meant accepting one, looking,
    /// going back, and accepting the other. This answers the question
    /// the picker is asking.
    ///
    /// Synchronous `git show`, which is only sound because the host does
    /// not call this while the selection is moving — see
    /// [`Self::preview_debounce`]. The blob fetch itself is guarded
    /// (size, binary, line count) by `preview_blob`.
    ///
    /// The preview buffer takes the name `magit-find-file` would give
    /// the real one, so what the pane says while you are choosing is
    /// what the buffer is called once you accept.
    fn preview(
        &self,
        _ctx: &PickerContext<'_>,
        routing: &RoutingPayload,
    ) -> Option<lattice_picker::PickerPreviewOutcome> {
        if self.which != RefScope::Revisions || !self.preview_enabled() {
            return None;
        }
        let RoutingPayload::InvokeCommand { id, .. } = routing else {
            return None;
        };
        let (rev, path) = find_file_preview_target(id)?;
        let workdir = crate::workdir::magit_workdir()?;
        let text = crate::magit_file_revision_mode::preview_blob(&workdir, &rev, &path)?;
        Some(lattice_picker::PickerPreviewOutcome::Buffer {
            name: crate::magit_file_revision_mode::blob_buffer_name(
                &crate::workdir::repo_label(&workdir),
                &rev,
                &path,
            ),
            text,
            syntax_path: Some(path),
        })
    }

    /// MG.54: never while the user is still moving.
    ///
    /// `git show` on every arrow key would be a subprocess per keystroke
    /// on the actor thread. Declaring the window means a scroll through
    /// fifty revisions spawns nothing at all, and the one fetch that
    /// does run is for the revision the user stopped on — so there is
    /// nothing to cancel and no stale result to discard.
    ///
    /// `None` when the feature is off, so a user who turns it off pays
    /// for no timers either. The other scopes never previewed.
    fn preview_debounce(&self) -> Option<std::time::Duration> {
        (self.which == RefScope::Revisions && self.preview_enabled())
            .then(|| std::time::Duration::from_millis(150))
    }
}
