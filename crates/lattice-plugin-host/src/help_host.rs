//! The `help` guest→host topic-registration seam (CR.3).
//!
//! Design:
//! [`contributable-registries.md`](../../../docs/dev/architecture/contributable-registries.md)
//! §3.1, and the `help` WIT interface.
//!
//! A help-contributing plugin implements the `help-plugin` world: it
//! **imports** the `help` API (`register-topic`) and **exports**
//! `register-help-topics`, which the host calls once to drive declaration.
//! The `theme-plugin` precedent, shape for shape.
//!
//! ## What this module does NOT do
//!
//! It does not touch `lattice-help`. The seam collects plain
//! [`HelpTopicSpec`] values and hands them back; `lattice-plugin-loader`
//! turns them into `HelpTopic`s and RCU-registers them into the
//! `HelpTopicRegistryHandle`. That keeps the dependency pointing the way it
//! already does — the loader is the crate that knows about the editor's
//! native registries, the host is the crate that knows about wasm — and
//! costs nothing, because a help body is inert data on both sides of that
//! line.
//!
//! ## Namespacing is host-side, from host ground truth
//!
//! [`namespaced_topic_name`] derives the registered name from the manifest
//! id the HOST holds, never from anything the guest passes. A guest cannot
//! name a topic outside its own namespace, so collisions with builtins and
//! between plugins are structurally impossible rather than a policy the
//! loader has to enforce and a user has to debug.

/// One topic a guest declared, ready for the loader to register.
///
/// Plain data — deliberately not a `lattice_help::HelpTopic`, see the module
/// docs. `name` is already namespaced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HelpTopicSpec {
    /// The registered (namespaced) topic name — what `:help <name>` opens.
    pub name: String,
    pub summary: String,
    /// Markdown, as the guest baked it into its component.
    pub body: String,
    /// Substring patterns `:describe-command` matches against command names
    /// to emit a `See also` cross-link.
    pub related_command_patterns: Vec<String>,
}

use crate::{
    Component, PluginBudget, PluginHost, PluginHostError, PluginManifest, TrustTier, arm_store,
    classify_trap,
};

pub(crate) mod bindings {
    wasmtime::component::bindgen!({
        world: "help-plugin",
        path: "../../wit",
        // Wired into the same async linker as WASI + the `help` host func, so
        // the export is async (the `theme-plugin` / `config-plugin`
        // precedent). Registration is off every hot path, so async costs
        // nothing.
        exports: { default: async },
        with: {
            "lattice:plugin-host/logging": crate::lattice::plugin_host::logging,
            "lattice:plugin-host/project": crate::lattice::plugin_host::project,
        },
    });
}

/// The registered name for a topic a plugin declared.
///
/// Auto-namespaced by plugin id, with one refinement for the common case: a
/// `name` that is empty, or that already equals the plugin id, lands at the
/// **bare** id. So a one-page plugin is `:help fugitive`, not `:help
/// fugitive.fugitive` — which is what a vim user expects and what plain
/// prefixing would have produced.
///
/// Returns `None` when the plugin has no identity, which is a harness shape
/// rather than a plugin error; the caller reports it as a rejection so the
/// topic is skipped rather than registered unnamespaced.
pub fn namespaced_topic_name(plugin_id: &str, name: &str) -> Option<String> {
    let plugin_id = plugin_id.trim();
    if plugin_id.is_empty() {
        return None;
    }
    let name = name.trim();
    if name.is_empty() || name == plugin_id {
        return Some(plugin_id.to_string());
    }
    Some(format!("{plugin_id}.{name}"))
}

/// Validate a guest's topic declaration, or reject it.
///
/// Guest output is untrusted. The two rejections are the ones a buggy guest
/// actually produces: a body that is empty or blank (a page that opens to
/// nothing looks like a broken editor, not a broken plugin), and a name the
/// host cannot namespace. Both are `Err`, which the seam turns into the WIT
/// `err` — the plugin's OTHER topics still register.
pub fn validate_topic(
    plugin_id: &str,
    name: &str,
    summary: &str,
    body: &str,
    related_commands: Vec<String>,
) -> Result<HelpTopicSpec, String> {
    let Some(name) = namespaced_topic_name(plugin_id, name) else {
        return Err("register-topic requires a plugin identity".to_string());
    };
    if body.trim().is_empty() {
        return Err(format!(
            "register-topic({name}): body is empty; a topic that opens to \
             nothing reads as a broken editor"
        ));
    }
    Ok(HelpTopicSpec {
        name,
        summary: summary.to_string(),
        body: body.to_string(),
        related_command_patterns: related_commands
            .into_iter()
            .filter(|p| !p.trim().is_empty())
            .collect(),
    })
}

impl PluginHost {
    /// Instantiate a `help-plugin` component under its capability grant,
    /// drive its `register-help-topics` export once, and return the
    /// host-issued id plus the topics it declared.
    ///
    /// Mirror of [`spawn_theme_plugin`](Self::spawn_theme_plugin). Nothing
    /// about the guest outlives this call: the bodies are already across, so
    /// the `Store` is dropped when the function returns and reading `:help`
    /// never touches wasm again.
    pub async fn spawn_help_plugin(
        &self,
        component: &Component,
        manifest: &PluginManifest,
        tier: TrustTier,
        budget: PluginBudget,
    ) -> Result<(crate::PluginId, Vec<HelpTopicSpec>), PluginHostError> {
        let (wasi, outcome, _data_dir) = self.build_plugin_wasi(manifest, tier);
        for denied in &outcome.denied {
            tracing::warn!(
                plugin = %manifest.id,
                capability = ?denied,
                "help plugin loaded with a withheld capability (reduced function)"
            );
        }
        let mut store = self.new_store(wasi, outcome.grant, budget, Some(&manifest.id))?;
        let bindings = bindings::HelpPlugin::instantiate_async(&mut store, component, &self.linker)
            .await
            .map_err(|e| PluginHostError::Instantiate(e.into()))?;

        let id = self.alloc_id();
        store.data_mut().log_ctx = self.log_ctx_for(id);

        arm_store(&mut store, budget)?;
        bindings
            .call_register_help_topics(&mut store)
            .await
            .map_err(|source| PluginHostError::Trap {
                func: "register-help-topics",
                kind: classify_trap(&source),
                source: source.into(),
            })?;

        let topics = std::mem::take(&mut store.data_mut().help_contributions);
        Ok((id, topics))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_sub_page_is_namespaced() {
        assert_eq!(
            namespaced_topic_name("fugitive", "status").as_deref(),
            Some("fugitive.status")
        );
    }

    /// The refinement that keeps the common case readable. Both spellings a
    /// one-page plugin plausibly uses must land on the bare id.
    #[test]
    fn the_single_page_case_keeps_the_bare_id() {
        assert_eq!(
            namespaced_topic_name("fugitive", "").as_deref(),
            Some("fugitive")
        );
        assert_eq!(
            namespaced_topic_name("fugitive", "fugitive").as_deref(),
            Some("fugitive"),
            "a plugin naming its page after itself must not get `fugitive.fugitive`"
        );
    }

    /// The property namespacing exists for: a guest cannot name a topic
    /// outside its own namespace, so it cannot shadow a builtin page.
    #[test]
    fn a_guest_cannot_squat_a_builtin_name() {
        let full = namespaced_topic_name("evil", "buffers").expect("named");
        assert_eq!(full, "evil.buffers");
        assert_ne!(full, "buffers");
    }

    #[test]
    fn a_topic_with_no_plugin_identity_is_rejected() {
        assert!(namespaced_topic_name("", "usage").is_none());
        assert!(namespaced_topic_name("   ", "usage").is_none());
        assert!(validate_topic("", "usage", "s", "body", Vec::new()).is_err());
    }

    #[test]
    fn an_empty_body_is_rejected() {
        assert!(validate_topic("p", "usage", "s", "", Vec::new()).is_err());
        assert!(validate_topic("p", "usage", "s", "  \n\t ", Vec::new()).is_err());
    }

    #[test]
    fn a_well_formed_topic_converts() {
        let spec = validate_topic(
            "fugitive",
            "status",
            "The status buffer.",
            "# Status\n\nHello.",
            vec!["magit-".to_string(), "  ".to_string()],
        )
        .expect("accepted");
        assert_eq!(spec.name, "fugitive.status");
        assert_eq!(spec.summary, "The status buffer.");
        assert_eq!(spec.body, "# Status\n\nHello.");
        // Blank patterns are dropped: a pattern that is whitespace is a
        // substring of every command name, so keeping it would cross-link
        // this topic from every `:describe-command`.
        assert_eq!(spec.related_command_patterns, vec!["magit-".to_string()]);
    }
}
