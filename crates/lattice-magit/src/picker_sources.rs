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
        buffer_name: Some(format!("*magit:branch-create-from:{base}*")),
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

    #[test]
    fn spec_id_matches_the_effect_openpicker_source_name() {
        // magit_branch_mode's `c` handler hardcodes this string in
        // `Effect::OpenPicker { source: "magit-branch-pick-base", .. }`
        // — this assertion is the tripwire if the id ever drifts.
        let source = BranchPickBaseSource::new();
        assert_eq!(source.spec().id, "magit-branch-pick-base");
        assert!(source.spec().args_schema.is_empty());
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
