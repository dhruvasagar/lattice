//! TR.2b — the `WasmTransientSource` adapter (boundary + registry builder).
//!
//! Turns a transient plugin's [`TransientClient`] bridge into the closure the
//! [`TransientSourceRegistry`](lattice_picker::TransientSourceRegistry) stores.
//! Everything about the guest ends here: the registry sees a
//! `Fn(&TransientContext) -> TransientBuildFuture` and cannot tell a plugin
//! menu from a native one.
//!
//! Three things happen at the boundary, and each is a decision:
//!
//! - **The context projects host→guest only** (`project_transient_context`),
//!   the `picker-context` precedent. A builder reads where the menu was opened
//!   from; it never sends one back.
//! - **A row's command crosses as a NAME**, resolved against the
//!   `CommandRegistry` here. A `CommandId` is host-issued and a plugin must not
//!   be able to forge one — the same rule the `register_*` seams follow.
//! - **An unresolvable name drops that row**, with a `debug!`, and the rest of
//!   the menu survives. A plugin whose sixth row references a command it failed
//!   to register should still get the other five; refusing the whole menu makes
//!   one bad row cost the feature.
//!
//! Design: `docs/dev/architecture/plugin-transients.md` §5–6.

use lattice_grammar::CommandRegistryHandle;
use lattice_picker::{
    TransientBuildFuture, TransientContext as NativeTransientContext,
    TransientGroup as NativeTransientGroup, TransientItem as NativeTransientItem,
    TransientItemKind as NativeTransientItemKind, TransientSpec as NativeTransientSpec,
};

use crate::WitBoundary;
use crate::transient_task::{
    TransientClient, TransientContext as WitTransientContext, TransientItemKind as WitItemKind,
    TransientSpec as WitTransientSpec,
};

/// Project a live [`TransientContext`](NativeTransientContext) into its owned
/// WIT mirror. Host→guest only.
///
/// Fallible only because of TR.3a's `args`: an `Args` variant that has no WIT
/// form is a typed error rather than a silently-dropped field, which would
/// hand the builder `none` and have it build the wrong menu.
pub fn project_transient_context(
    ctx: &NativeTransientContext,
) -> Result<WitTransientContext, String> {
    Ok(WitTransientContext {
        major_mode: ctx.major_mode.clone(),
        minor_modes: ctx.minor_modes.clone(),
        buffer: ctx.buffer.map(|b| b.0),
        // TR.3a: what the open was FOR — the row's args when a menu drilled
        // down into another. A projection failure would silently hand the
        // builder `none` and it would build the wrong menu, so it is a typed
        // error like every other boundary conversion.
        args: ctx.args.to_wit()?,
    })
}

/// Convert a guest-built spec into the native one, resolving each action row's
/// command name against `registry`.
///
/// Never fails: a row the host cannot honour is dropped, not escalated. The
/// only way a plugin loses its whole menu is by returning an `err` from
/// `build`, which is a statement rather than an accident.
pub fn spec_from_wit(
    wit: WitTransientSpec,
    registry: &CommandRegistryHandle,
    plugin: &str,
) -> NativeTransientSpec {
    let commands = registry.load();
    let groups = wit
        .groups
        .into_iter()
        .map(|g| NativeTransientGroup {
            label: g.label,
            items: g
                .items
                .into_iter()
                .filter_map(|item| {
                    let kind = match item.kind {
                        WitItemKind::Dismiss => NativeTransientItemKind::Dismiss,
                        WitItemKind::Action(action) => {
                            let Some(command) = commands.id_by_name(&action.command) else {
                                // `debug!` and not `warn!`: this fires per row
                                // of a menu the user just opened, and the row
                                // simply not being there is the visible signal.
                                tracing::debug!(
                                    plugin,
                                    command = %action.command,
                                    key = ?item.key,
                                    "transient row names an unregistered command; dropping the row"
                                );
                                return None;
                            };
                            let args = match lattice_grammar::Args::from_wit(action.args) {
                                Ok(args) => args,
                                Err(e) => {
                                    tracing::debug!(
                                        plugin,
                                        command = %action.command,
                                        error = %e,
                                        "transient row's args did not cross; dropping the row"
                                    );
                                    return None;
                                }
                            };
                            NativeTransientItemKind::Action { command, args }
                        }
                    };
                    Some(NativeTransientItem {
                        key: item.key,
                        label: item.label,
                        description: item.description,
                        kind,
                    })
                })
                .collect(),
        })
        .collect();

    NativeTransientSpec {
        title: wit.title,
        groups,
        // A closure has no WIT form, so a guest menu has no live preview pane.
        // Stated in the design fragment rather than discovered here.
        preview: None,
        footer: wit.footer,
    }
}

/// The registry builder for a transient plugin: projects the open context
/// synchronously, then awaits the guest's `build` on the plugin's own actor
/// task and converts the result.
///
/// The synchronous prelude matters — it is what lets the returned future be
/// `'static`, so nothing borrows the editor across the await.
pub fn transient_builder(
    client: TransientClient,
    registry: CommandRegistryHandle,
    plugin: String,
) -> impl Fn(&NativeTransientContext) -> TransientBuildFuture + Send + Sync + 'static {
    move |ctx: &NativeTransientContext| {
        let wit_ctx = project_transient_context(ctx);
        let client = client.clone();
        let registry = registry.clone();
        let plugin = plugin.clone();
        Box::pin(async move {
            let wit_ctx = wit_ctx?;
            // The host surface (trap / gone / quarantined) and the guest's own
            // `err` both mean the menu does not open. They are kept distinct in
            // the message so the echo says which.
            let wit = match client.build(wit_ctx).await {
                Ok(inner) => inner?,
                Err(host_err) => return Err(format!("{plugin}: {host_err}")),
            };
            Ok(spec_from_wit(wit, &registry, &plugin))
        })
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use crate::transient_task::{
        TransientGroup as WitGroup, TransientItem as WitItem, TransientItemKind,
    };
    use lattice_core::BufferId;
    use std::sync::Arc;

    use crate::lattice::plugin_host::types::{Args as WitArgs, TransientAction};

    fn registry_with(names: &[&str]) -> CommandRegistryHandle {
        let mut reg = lattice_grammar::CommandRegistry::new();
        for name in names {
            reg.register_action(
                name,
                "test action",
                lattice_grammar::registry::ActionSpec {
                    args_schema: Vec::new(),
                    apply: Arc::new(|_| Ok(lattice_grammar::Effect::None)),
                },
            );
        }
        Arc::new(arc_swap::ArcSwap::from_pointee(reg))
    }

    fn action_row(key: &str, command: &str, arg: Option<&str>) -> WitItem {
        WitItem {
            key: vec![key.to_string()],
            label: key.to_string(),
            description: String::new(),
            kind: TransientItemKind::Action(TransientAction {
                command: command.to_string(),
                args: match arg {
                    Some(a) => WitArgs::String(a.to_string()),
                    None => WitArgs::None,
                },
            }),
        }
    }

    fn spec(items: Vec<WitItem>) -> WitTransientSpec {
        WitTransientSpec {
            title: "Capture".into(),
            groups: vec![WitGroup {
                label: "Templates".into(),
                items,
            }],
            footer: Some("q quit".into()),
        }
    }

    /// The happy path: rows cross with their key, label, args, and a resolved
    /// `CommandId`.
    #[test]
    fn an_action_row_crosses_with_its_command_resolved_and_its_args_intact() {
        let registry = registry_with(&["org-capture-key"]);
        let native = spec_from_wit(
            spec(vec![action_row("t", "org-capture-key", Some("todo"))]),
            &registry,
            "org",
        );

        assert_eq!(native.title, "Capture");
        assert_eq!(native.footer.as_deref(), Some("q quit"));
        let item = &native.groups[0].items[0];
        assert_eq!(item.key, vec!["t".to_string()]);
        match &item.kind {
            NativeTransientItemKind::Action { command, args } => {
                assert_eq!(
                    Some(*command),
                    registry.load().id_by_name("org-capture-key")
                );
                assert!(matches!(args, lattice_grammar::Args::String(s) if s == "todo"));
            }
            other => panic!("expected an action row, got {other:?}"),
        }
    }

    /// The failure rule that matters: one unresolvable row costs that row, not
    /// the menu. The alternative — refusing the whole spec — makes a single
    /// typo in a plugin's sixth row take the other five with it.
    #[test]
    fn an_unresolvable_command_drops_only_its_own_row() {
        let registry = registry_with(&["org-capture-key"]);
        let native = spec_from_wit(
            spec(vec![
                action_row("t", "org-capture-key", Some("todo")),
                action_row("x", "org-command-that-never-registered", None),
                WitItem {
                    key: vec!["q".into()],
                    label: "quit".into(),
                    description: String::new(),
                    kind: TransientItemKind::Dismiss,
                },
            ]),
            &registry,
            "org",
        );

        let keys: Vec<&str> = native.groups[0]
            .items
            .iter()
            .map(|i| i.key[0].as_str())
            .collect();
        assert_eq!(
            keys,
            vec!["t", "q"],
            "the bad row is gone and the good ones survive"
        );
    }

    /// A guest spec never carries a preview: the native field is a closure, and
    /// a closure has no WIT form.
    #[test]
    fn a_guest_spec_has_no_preview() {
        let registry = registry_with(&[]);
        let native = spec_from_wit(spec(Vec::new()), &registry, "org");
        assert!(native.preview.is_none());
    }

    /// The context projects field-for-field. `buffer` is the one that could
    /// silently rot: it is `Option<BufferId>` natively and `option<u32>` over
    /// the wire.
    #[test]
    fn the_context_projects_both_mode_axes_and_the_buffer() {
        let ctx = NativeTransientContext {
            major_mode: Some("org-mode".into()),
            minor_modes: vec!["org-global-mode".into(), "auto-pair-mode".into()],
            buffer: Some(BufferId(7)),
            args: Default::default(),
        };
        let wit = project_transient_context(&ctx).expect("projects");
        assert_eq!(wit.major_mode.as_deref(), Some("org-mode"));
        assert_eq!(wit.minor_modes.len(), 2);
        assert_eq!(wit.buffer, Some(7));

        let empty =
            project_transient_context(&NativeTransientContext::default()).expect("projects");
        assert_eq!(empty.major_mode, None);
        assert!(empty.minor_modes.is_empty());
        assert_eq!(empty.buffer, None);
    }
}
