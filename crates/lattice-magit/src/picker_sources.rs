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

use std::sync::Arc;

use lattice_completion::{CandidateKind, RawCandidate};
use lattice_picker::{
    PickerAcceptOutcome, PickerContext, PickerInitResult, PickerSourceGenerator, PickerSourceSpec,
    RoutingPayload, SourceResult,
};
use lattice_vcs::{Branch, Repository};

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

/// Register this crate's picker sources into the supplied registry.
/// Called from `lattice-host`'s `editor_boot.rs`, mirroring
/// `lattice_snippet::picker_sources::register` — the picker registry
/// is host-owned and populated by name at boot (no generic
/// `SubsystemBoot` seam for pickers today), so this is the
/// established shape for a feature crate to contribute a source.
pub fn register(picker_registry: &mut lattice_picker::PickerRegistry) {
    picker_registry.register_generator(Arc::new(BranchPickBaseSource::new()));
    picker_registry.register_generator(Arc::new(BranchCheckoutSource::new()));
    // MG.32: the rest of the branch transient's picker-backed rows.
    picker_registry.register_generator(Arc::new(BranchCreateNoCheckoutSource::new()));
    picker_registry.register_generator(Arc::new(BranchRenameSource::new()));
    picker_registry.register_generator(Arc::new(BranchDeleteSource::new()));
    picker_registry.register_generator(Arc::new(CommitPickSource::new()));
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

impl CommitPickSource {
    pub fn new() -> Self {
        Self {
            spec: PickerSourceSpec {
                id: std::borrow::Cow::Borrowed(COMMIT_PICK_SOURCE),
                doc: std::borrow::Cow::Borrowed(
                    "Pick a commit, then run the named magit ex-command on it.",
                ),
                args_schema: vec![lattice_grammar::ArgSpec::required(
                    "command",
                    lattice_grammar::ArgKind::String,
                    "magit ex-command to run on the picked commit",
                )],
                args_hint: std::borrow::Cow::Borrowed("<magit ex-command>"),
                live: false,
            },
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
                            id: format!("{command} {sha}"),
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

    /// The sha must be the full one and it must reach the ex line —
    /// this is the whole route from a picked candidate to an
    /// operation, and `RoutingPayload::InvokeCommand`'s `args` field
    /// is a dead end (the host destructures it away and runs `id` as
    /// an ex line), so anything not *in* the line is lost.
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
        let rows = parse_commit_rows("1111111111\01111111 first\n2222222222\02222222 second\n");
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
        register(&mut registry);
        let mut ids: Vec<&str> = registry.ids().collect();
        ids.sort_unstable();

        assert_eq!(
            ids,
            vec![
                BRANCH_CHECKOUT_SOURCE,           // MG.29: `b l`
                BRANCH_CREATE_NO_CHECKOUT_SOURCE, // MG.32: `b n`
                BRANCH_DELETE_SOURCE,             // MG.32: `b x`
                "magit-branch-pick-base",         // `b c`, and `c` in the branch buffer
                BRANCH_RENAME_SOURCE,             // MG.32: `b m`
                COMMIT_PICK_SOURCE,               // MG.23j: `A` / `_` / `O`
            ],
            "magit's registered picker sources changed — update this list \
             together with `register`, and check every `Effect::OpenPicker` \
             that names one"
        );
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
