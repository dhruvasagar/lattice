//! PL8.H.3 — the `plugins-mode` action handlers: reload / unload / describe /
//! refresh the plugin under the cursor.
//!
//! Each is a mode-owned [`ActionHandler`] closure (the `repl-mode` precedent),
//! bound to its `action:plugins-*` command by the host's
//! `register_mode_action_handlers` walk and gated to `plugins-mode`-active
//! buffers by the per-keystroke filter. The `action:*` commands are registered
//! (dead-body) at [`install`](crate::install) so their names resolve for the
//! keymap binding; the mode's handler closure intercepts before the grammar
//! Action gate, so those dead bodies never run.
//!
//! The cursor line maps to a plugin by index (`cursor.line - HEADER_LINES`),
//! matching the render order (`render_status` emits the header then
//! `plugin_status()` rows in order). Every handler reads the loader + buffer
//! store from the [`ActionContext`] service registry, so nothing is captured at
//! registration — a missing service degrades the action to a no-op, never a
//! panic. Reload / unload run off the actor thread; the view re-renders from the
//! fresh status afterward.

use std::sync::Arc;

use lattice_grammar::registry::ActionSpec;
use lattice_grammar::{CommandRegistry, EchoLevel, Effect};
use lattice_mode::{ActionContext, ActionHandler, BufferStoreHandle};
use lattice_plugin_host::TrustTier;
use lattice_plugin_loader::PluginLoaderHandle;

use crate::render::{self, HEADER_LINES};

/// The `action:plugins-*` command names. Used for the handler bindings +
/// dead-body registration; the keymap `cmd:` literals in `mode.rs` must match
/// these (pinned by `keymap_cmds_have_registered_handlers`).
pub const RELOAD: &str = "action:plugins-reload";
pub const UNLOAD: &str = "action:plugins-unload";
pub const DESCRIBE: &str = "action:plugins-describe";
pub const REFRESH: &str = "action:plugins-refresh";

/// Register the four `action:plugins-*` commands (dead-body — the mode's handler
/// closures do the work) so the keymap's `cmd:` names resolve at boot. The
/// `register_repl_mode_actions` precedent.
pub fn register_actions(commands: &mut CommandRegistry) {
    for (name, doc) in [
        (RELOAD, "plugins: reload the plugin under the cursor (mode-owned)."),
        (UNLOAD, "plugins: unload the plugin under the cursor (mode-owned)."),
        (
            DESCRIBE,
            "plugins: describe the plugin under the cursor (mode-owned).",
        ),
        (REFRESH, "plugins: refresh the plugin list (mode-owned)."),
    ] {
        commands.register_action(
            name,
            doc,
            ActionSpec {
                apply: Arc::new(|_| Ok(Effect::None)),
                args_schema: vec![],
            },
        );
    }
}

/// The manifest id of the plugin whose row the cursor is on, or `None` when the
/// cursor is on a header line / past the last row (the action then no-ops).
fn plugin_name_at(ctx: &ActionContext<'_>) -> Option<String> {
    let loader = ctx.services.get::<PluginLoaderHandle>()?;
    let idx = (ctx.cursor.line as usize).checked_sub(HEADER_LINES)?;
    loader.plugin_status().get(idx).map(|s| s.name.clone())
}

/// Re-render the manager buffer from the current status (off the actor thread).
fn refresh(ctx: &ActionContext<'_>) {
    let (Some(loader), Some(store)) = (
        ctx.services.get::<PluginLoaderHandle>(),
        ctx.services.get::<BufferStoreHandle>(),
    ) else {
        return;
    };
    let buffer_id = lattice_core::BufferId(ctx.buffer_id.0 as u32);
    let text = render::render_status(&loader.plugin_status());
    crate::mode::spawn_write(&store, buffer_id, text);
}

/// `r` — reload the plugin under the cursor (async: unload + re-instantiate from
/// disk with a fresh, untripped quarantine), then re-render.
pub fn reload_handler() -> ActionHandler {
    Arc::new(|ctx: &ActionContext<'_>| -> Option<Effect> {
        let name = plugin_name_at(ctx)?;
        let loader = ctx.services.get::<PluginLoaderHandle>()?;
        let store = ctx.services.get::<BufferStoreHandle>()?;
        let buffer_id = lattice_core::BufferId(ctx.buffer_id.0 as u32);
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            let name_c = name.clone();
            runtime.spawn(async move {
                let _ = loader.reload(&name_c, TrustTier::UserInstalled).await;
                if let Some(handle) = store.handle_for(buffer_id) {
                    let text = render::render_status(&loader.plugin_status());
                    crate::mode::write_all(&handle, text).await;
                }
            });
        }
        Some(Effect::Echo {
            level: EchoLevel::Info,
            text: format!("reloading `{name}`…"),
        })
    })
}

/// `x` — unload the plugin under the cursor (synchronous teardown), then
/// re-render to drop its row.
pub fn unload_handler() -> ActionHandler {
    Arc::new(|ctx: &ActionContext<'_>| -> Option<Effect> {
        let name = plugin_name_at(ctx)?;
        let loader = ctx.services.get::<PluginLoaderHandle>()?;
        let report = loader.unload(&name);
        refresh(ctx);
        Some(Effect::Echo {
            level: EchoLevel::Info,
            text: match report {
                Some(_) => format!("unloaded `{name}`"),
                None => format!("no loaded plugin `{name}`"),
            },
        })
    })
}

/// `K` / `<CR>` — open the plugin under the cursor's documentation
/// (`:describe-plugin`).
pub fn describe_handler() -> ActionHandler {
    Arc::new(|ctx: &ActionContext<'_>| -> Option<Effect> {
        let name = plugin_name_at(ctx)?;
        Some(Effect::DescribePlugin { name })
    })
}

/// `gr` — re-render the list from the current status (a plugin loaded / reloaded
/// out of band since the view opened shows up).
pub fn refresh_handler() -> ActionHandler {
    Arc::new(|ctx: &ActionContext<'_>| -> Option<Effect> {
        refresh(ctx);
        None
    })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn register_actions_registers_the_four_commands() {
        let mut commands = CommandRegistry::new();
        register_actions(&mut commands);
        for name in [RELOAD, UNLOAD, DESCRIBE, REFRESH] {
            assert!(
                commands.id_by_name(name).is_some(),
                "`{name}` must be registered so the keymap `cmd:` resolves"
            );
        }
    }
}
