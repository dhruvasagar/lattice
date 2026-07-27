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
