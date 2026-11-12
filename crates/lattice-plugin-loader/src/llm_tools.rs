//! AT.5c — a plugin's `llm-tools` export, adapted to lattice-ai's `ToolPlugin`.
//!
//! The plugin host's [`LlmToolsClient`] speaks WIT records and knows nothing
//! of the assistant; lattice-ai's `PluginToolSource` decides what a plugin
//! tool may do and knows nothing of wasm. This module is the only place the
//! two meet, which is why it lives in the loader: the loader already depends
//! on every subsystem it registers a seam into, so neither of the other two
//! has to compile the other.
//!
//! Design: `docs/dev/architecture/llm-assistant-tools.md` §5.

use lattice_ai::llm::tools::BoxFuture;
use lattice_ai::llm::tools::plugin::{
    PluginGrant, PluginProfileSpec, PluginTool, PluginToolKind, PluginToolResult, ProposedEdit,
    Reach, ToolPlugin,
};
use lattice_plugin_host::llm_tools_task::{LlmToolKind, LlmToolResult};
use lattice_plugin_host::{CapabilityGrant, LlmToolsClient};

/// What `grant` lets a plugin reach. Anything that can send a tool's
/// arguments off the machine or run a program is outward, and its tools ask
/// before each call. Filesystem reach does not count: on this seam every
/// prefix is read-only (`llm_tools_task`), and a read is what `lattice.read`
/// already runs unasked.
pub(crate) fn reach(grant: &CapabilityGrant) -> Reach {
    if grant.net_http.is_empty() && !grant.proc_spawn {
        Reach::Contained
    } else {
        Reach::Outward
    }
}

/// What lattice-ai needs of `grant`: its [`reach`], and its `fs:write`
/// prefixes, where its proposals may land besides the project (AT.8). The
/// prefixes are the grant's own, already filtered by trust tier, so a
/// proposal reaches no further than the plugin's other seams already do.
pub(crate) fn grant(grant: &CapabilityGrant) -> PluginGrant {
    PluginGrant {
        reach: reach(grant),
        writes: grant
            .fs
            .iter()
            .filter(|g| g.write)
            .map(|g| g.prefix.clone())
            .collect(),
    }
}

/// A loaded plugin's tools, as lattice-ai calls them.
pub(crate) struct ClientPlugin {
    client: LlmToolsClient,
    /// The plugin's id, so a host-side failure names it.
    plugin: String,
}

impl ClientPlugin {
    pub(crate) fn new(client: LlmToolsClient, plugin: String) -> Self {
        Self { client, plugin }
    }
}

impl ToolPlugin for ClientPlugin {
    fn tools(&self) -> BoxFuture<'_, Result<Vec<PluginTool>, String>> {
        Box::pin(async move {
            let tools = self
                .client
                .tools()
                .await
                .map_err(|e| format!("plugin `{}`: {e}", self.plugin))?;
            Ok(tools
                .into_iter()
                .map(|t| PluginTool {
                    name: t.name,
                    description: t.description,
                    input_schema: t.input_schema,
                    kind: match t.kind {
                        LlmToolKind::Read => PluginToolKind::Read,
                        LlmToolKind::Propose => PluginToolKind::Propose,
                    },
                })
                .collect())
        })
    }

    fn call(&self, name: &str, args: String) -> BoxFuture<'_, Result<PluginToolResult, String>> {
        let name = name.to_string();
        Box::pin(async move {
            // A trap, a quarantine or an ended actor is the host's failure; a
            // guest `err` is the plugin's answer. Both reach the model as the
            // tool failing, the first naming the plugin so the user can tell
            // a broken plugin from a query that found nothing.
            let result = self
                .client
                .call(name, args)
                .await
                .map_err(|e| format!("plugin `{}`: {e}", self.plugin))??;
            Ok(from_wit(result))
        })
    }

    fn profiles(&self) -> BoxFuture<'_, Result<Vec<PluginProfileSpec>, String>> {
        Box::pin(async move {
            let profiles = self
                .client
                .profiles()
                .await
                .map_err(|e| format!("plugin `{}`: {e}", self.plugin))?;
            Ok(profiles
                .into_iter()
                .map(|p| PluginProfileSpec {
                    name: p.name,
                    description: p.description,
                    settings: p.settings,
                })
                .collect())
        })
    }
}

fn from_wit(result: LlmToolResult) -> PluginToolResult {
    PluginToolResult {
        text: result.text,
        proposals: result
            .proposals
            .into_iter()
            .map(|p| ProposedEdit {
                path: p.path,
                old_string: p.old_string,
                new_string: p.new_string,
            })
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// AT.8: only the `fs:write` prefixes widen where a proposal may land.
    #[test]
    fn a_proposal_may_land_where_the_grant_writes() {
        let mut grant = CapabilityGrant::default();
        for (prefix, write) in [
            ("/home/u/org", true),
            ("/home/u/docs", false),
            ("/home/u/roam", true),
        ] {
            grant.fs.push(lattice_plugin_host::FsGrant {
                prefix: prefix.into(),
                write,
            });
        }
        assert_eq!(
            super::grant(&grant),
            PluginGrant {
                reach: Reach::Contained,
                writes: vec!["/home/u/org".into(), "/home/u/roam".into()],
            }
        );
    }

    #[test]
    fn network_or_processes_make_a_plugin_outward_and_files_do_not() {
        let mut grant = CapabilityGrant::default();
        assert_eq!(reach(&grant), Reach::Contained);
        grant.fs.push(lattice_plugin_host::FsGrant {
            prefix: "/notes".into(),
            write: true,
        });
        assert_eq!(reach(&grant), Reach::Contained);
        grant.net_http.push("api.example.com".into());
        assert_eq!(reach(&grant), Reach::Outward);
        let grant = CapabilityGrant {
            proc_spawn: true,
            ..CapabilityGrant::default()
        };
        assert_eq!(reach(&grant), Reach::Outward);
    }
}
