//! The two front-ends that declare plugins, and how their declarations meet.
//!
//! `init.rs` declares through the `plugin-manager` seam (`require`,
//! `require-release`); `lattice.toml` declares release plugins in its
//! `[plugins]` table. Both lower to [`RequiredSpec`] and share one queue, so
//! everything after it (install, reconcile, `:plugins`, `:plugin-update`) is
//! the same code whichever file named the plugin.
//!
//! When both name one plugin, `init.rs` wins and the whole spec is taken from
//! it: a spec assembled from two files is one nobody wrote. See
//! `docs/dev/architecture/plugin-toml.md` §4.

use std::collections::HashMap;

use lattice_config::{PluginDeclarations, RejectedDeclaration};
use lattice_plugin_host::plugin_manager_host::is_safe_plugin_name;

use crate::pipeline::RequiredSpec;
use crate::resolve::PluginSource;

/// Which configuration file declared a plugin.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DeclaredBy {
    /// The user's `init.rs`, through the `plugin-manager` seam.
    InitRs,
    /// The user's `lattice.toml`, through its `[plugins]` table.
    LatticeToml,
}

impl DeclaredBy {
    /// The file's name, as the user knows it.
    pub fn label(self) -> &'static str {
        match self {
            Self::InitRs => "init.rs",
            Self::LatticeToml => "lattice.toml",
        }
    }

    /// `init.rs` outranks `lattice.toml`: it can express everything the table
    /// can, so naming a plugin there is the more specific statement.
    pub(crate) fn outranks(self, other: Self) -> bool {
        self == Self::InitRs && other == Self::LatticeToml
    }
}

/// Lower shape-checked `lattice.toml` declarations to install specs.
///
/// The id passes the same path-safety gate `init.rs`'s names pass in the host;
/// an id that fails it joins the rejections. Repo and tag syntax are left to
/// the install step, which checks them for both front-ends.
pub fn lower_declarations(
    declarations: PluginDeclarations,
) -> (Vec<RequiredSpec>, Vec<RejectedDeclaration>) {
    let mut rejected = declarations.rejected;
    let mut specs = Vec::with_capacity(declarations.declared.len());
    for d in declarations.declared {
        if !is_safe_plugin_name(&d.id) {
            rejected.push(RejectedDeclaration {
                reason: format!(
                    "`plugins.{}`: a plugin id is one path component of letters, digits, \
                     `-` and `_`",
                    d.id
                ),
                id: d.id,
            });
            continue;
        }
        specs.push(RequiredSpec {
            name: d.id,
            source: PluginSource::Release {
                repo: d.repo,
                version: d.version,
                sha256: d.sha256,
            },
            enable_mode: d.enable_mode,
            // A release is never built, so there is no checkout to pin.
            pinned: false,
        });
    }
    (specs, rejected)
}

/// Collapse the queue to one spec per plugin, in first-declared order.
///
/// A higher-ranked origin replaces a lower one; within one origin the later
/// declaration replaces the earlier. A plugin named by both files with
/// different specs gets one `warn!` naming both, because the user wrote two
/// things and only one of them takes effect.
pub fn merge_declared(queue: Vec<(DeclaredBy, RequiredSpec)>) -> Vec<RequiredSpec> {
    let mut order: Vec<String> = Vec::new();
    let mut chosen: HashMap<String, (DeclaredBy, RequiredSpec)> = HashMap::new();
    for (by, spec) in queue {
        let Some((kept_by, kept)) = chosen.get(&spec.name) else {
            order.push(spec.name.clone());
            chosen.insert(spec.name.clone(), (by, spec));
            continue;
        };
        let (kept_by, differs) = (*kept_by, *kept != spec);
        let replace = !kept_by.outranks(by);
        if kept_by != by && differs {
            let (winner, loser) = if replace {
                (by, kept_by)
            } else {
                (kept_by, by)
            };
            tracing::warn!(
                plugin = %spec.name,
                "`{}` is declared in both {} and {}, differently; using {}'s and \
                 ignoring {}'s",
                spec.name,
                winner.label(),
                loser.label(),
                winner.label(),
                loser.label(),
            );
        } else {
            tracing::debug!(plugin = %spec.name, "plugin declared twice; keeping one");
        }
        if replace {
            chosen.insert(spec.name.clone(), (by, spec));
        }
    }
    order
        .into_iter()
        .filter_map(|name| chosen.remove(&name).map(|(_, spec)| spec))
        .collect()
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]
    use super::*;
    use lattice_config::PluginDeclaration;

    fn release(name: &str, repo: &str) -> RequiredSpec {
        RequiredSpec {
            name: name.into(),
            source: PluginSource::Release {
                repo: repo.into(),
                version: None,
                sha256: None,
            },
            enable_mode: None,
            pinned: false,
        }
    }

    fn repo_of(spec: &RequiredSpec) -> &str {
        match &spec.source {
            PluginSource::Release { repo, .. } => repo,
            other => panic!("not a release: {other:?}"),
        }
    }

    #[test]
    fn lowering_maps_every_field_to_a_release() {
        let (specs, rejected) = lower_declarations(PluginDeclarations {
            declared: vec![PluginDeclaration {
                id: "org".into(),
                repo: "owner/org".into(),
                version: Some("v1".into()),
                sha256: Some("a".repeat(64)),
                enable_mode: Some("org-mode".into()),
            }],
            rejected: vec![],
        });
        assert!(rejected.is_empty());
        assert_eq!(
            specs,
            vec![RequiredSpec {
                name: "org".into(),
                source: PluginSource::Release {
                    repo: "owner/org".into(),
                    version: Some("v1".into()),
                    sha256: Some("a".repeat(64)),
                },
                enable_mode: Some("org-mode".into()),
                pinned: false,
            }]
        );
    }

    /// The id becomes a directory under the user's plugin root; a TOML key can
    /// be any string, so it gets the gate `init.rs`'s names get.
    #[test]
    fn lowering_rejects_an_id_that_is_not_a_safe_path_component() {
        let decl = |id: &str| PluginDeclaration {
            id: id.into(),
            repo: "owner/x".into(),
            version: None,
            sha256: None,
            enable_mode: None,
        };
        let (specs, rejected) = lower_declarations(PluginDeclarations {
            declared: vec![decl("../../.ssh"), decl("ok"), decl(".hidden")],
            rejected: vec![RejectedDeclaration {
                id: "earlier".into(),
                reason: "kept".into(),
            }],
        });
        assert_eq!(
            specs.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(),
            ["ok"]
        );
        let ids: Vec<_> = rejected.iter().map(|r| r.id.as_str()).collect();
        assert_eq!(ids, ["earlier", "../../.ssh", ".hidden"]);
        assert!(rejected[1].reason.contains("path component"));
    }

    #[test]
    fn init_rs_wins_over_lattice_toml_whichever_comes_first() {
        for queue in [
            vec![
                (DeclaredBy::LatticeToml, release("org", "toml/org")),
                (DeclaredBy::InitRs, release("org", "init/org")),
            ],
            vec![
                (DeclaredBy::InitRs, release("org", "init/org")),
                (DeclaredBy::LatticeToml, release("org", "toml/org")),
            ],
        ] {
            let merged = merge_declared(queue);
            assert_eq!(merged.len(), 1);
            assert_eq!(repo_of(&merged[0]), "init/org");
        }
    }

    #[test]
    fn plugins_named_once_pass_through_in_declared_order() {
        let merged = merge_declared(vec![
            (DeclaredBy::LatticeToml, release("b", "o/b")),
            (DeclaredBy::LatticeToml, release("a", "o/a")),
            (DeclaredBy::InitRs, release("c", "o/c")),
        ]);
        let names: Vec<_> = merged.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, ["b", "a", "c"]);
    }

    /// Two `require`s of one name used to install it twice, the second over
    /// the first; one install of the later spec is what the user meant.
    #[test]
    fn a_repeat_within_one_file_keeps_the_later_spec_once() {
        let merged = merge_declared(vec![
            (DeclaredBy::InitRs, release("org", "first/org")),
            (DeclaredBy::InitRs, release("x", "o/x")),
            (DeclaredBy::InitRs, release("org", "second/org")),
        ]);
        let names: Vec<_> = merged.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, ["org", "x"]);
        assert_eq!(repo_of(&merged[0]), "second/org");
    }
}
