//! AI agent modes (AI-1b).
//!
//! - `ai-log-mode` (major) -- the read-only buffer backing the
//!   per-process `*ai:<provider>:<index>*` log view. Mirrors
//!   `lattice_lsp::modes::LspServerLogMode`: `on_activate` derives
//!   its [`SessionKey`](crate::ai_log::SessionKey) identity by
//!   parsing the buffer's synthetic name, seeds from the
//!   [`AiLogger`](crate::ai_log::AiLogger) ring, subscribes to
//!   [`AiLogPushed`](crate::ai_log::AiLogPushed), and spawns a
//!   drain task that appends matching records live. The returned
//!   `Subscription` guard unsubscribes on drop.
//!
//! The App/boot wiring that registers this mode, adds
//! `Editor.ai_logger`, wires the `AiLogPushed` publisher onto the
//! runtime bus, and creates/opens the buffers is Task 12 -- this
//! module only builds the mode itself, unit-testable without a
//! booted app (every missing service / unparseable name short-
//! circuits `on_activate` to `Ok(None)`).

use lattice_mode::{
    BufferStoreHandle, CapabilitySet, LifecycleFuture, Mode, ModeContext, ModeId, ModeKind,
    OptionOverrideSet, Subscription,
};

/// `ai-log-mode` -- major mode for the per-session
/// `*ai:<provider>:<index>*` buffer.
pub struct AiLogMode;

impl AiLogMode {
    pub fn mode_id() -> ModeId {
        ModeId::new("ai-log-mode")
    }
}

impl Mode for AiLogMode {
    type Guard = Option<Subscription>;
    fn id(&self) -> ModeId {
        Self::mode_id()
    }
    fn kind(&self) -> ModeKind {
        ModeKind::Major
    }
    fn options(&self) -> OptionOverrideSet {
        lattice_config::overrides! {
            lattice_config::ReadOnly = true,
            lattice_config::NoFile = true,
        }
    }
    fn required_capabilities(&self) -> CapabilitySet {
        CapabilitySet::empty()
    }

    fn on_activate(&self, ctx: ModeContext) -> LifecycleFuture<'_, Self::Guard> {
        Box::pin(async move {
            let buffer_id = lattice_core::BufferId(ctx.buffer_id().0 as u32);
            let Some(store) = ctx.service::<BufferStoreHandle>() else {
                return Ok(None);
            };
            let Some(name) = store.name_for(buffer_id) else {
                return Ok(None);
            };
            let Some(key) = crate::buffer_names::parse_ai_log_name(&name) else {
                return Ok(None);
            };
            let Some(handle) = store.handle_for(buffer_id) else {
                return Ok(None);
            };
            let Ok(runtime) = tokio::runtime::Handle::try_current() else {
                return Ok(None);
            };

            // Seed the buffer from the per-session ring so
            // pre-existing records are visible immediately. No
            // trace-filtering distinction -- one buffer per
            // session carries every record for that session.
            if let Some(logger) = ctx.service::<crate::ai_log::AiLogger>() {
                let snap = logger.snapshot_session(&key);
                let mut text = String::new();
                for record in snap.iter() {
                    let line = crate::ai_log::format_ai_log_line(
                        Some(&key),
                        crate::ai_log::level_tag(record.level),
                        record.source.tag(),
                        &record.message,
                    );
                    text.push_str(&line);
                    text.push('\n');
                }
                if !text.is_empty() {
                    let snapshot = handle.snapshot();
                    let last_line = snapshot.buffer.line_count().saturating_sub(1);
                    let line_text = snapshot.buffer.line(last_line).unwrap_or_default();
                    let pos = lattice_protocol::position::Position::new(
                        last_line,
                        line_text.len() as u32,
                    );
                    let edit = lattice_protocol::edit::Edit::insert(pos, text);
                    let handle_seed = handle.clone();
                    runtime.spawn(async move {
                        let _ = handle_seed.apply_edit_batch(vec![edit]).await;
                    });
                }
            }

            let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<crate::ai_log::AiLogPushed>();
            let sub_id = ctx
                .events()
                .subscribe_typed::<crate::ai_log::AiLogPushed>(tx);
            let bus_handle = ctx.events_handle();

            let filter_key = key.clone();
            runtime.spawn(async move {
                while let Some(first) = rx.recv().await {
                    let mut batch: Vec<crate::ai_log::AiLogPushed> = vec![first];
                    while let Ok(more) = rx.try_recv() {
                        batch.push(more);
                    }
                    let mut text = String::new();
                    for event in batch
                        .iter()
                        .filter(|e| e.session.as_ref() == Some(&filter_key))
                    {
                        let line = crate::ai_log::format_ai_log_line(
                            Some(&filter_key),
                            &event.level,
                            &event.source,
                            &event.message,
                        );
                        text.push_str(&line);
                        text.push('\n');
                    }
                    if text.is_empty() {
                        continue;
                    }
                    let snap = handle.snapshot();
                    let last_line = snap.buffer.line_count().saturating_sub(1);
                    let line_text = snap.buffer.line(last_line).unwrap_or_default();
                    let pos = lattice_protocol::position::Position::new(
                        last_line,
                        line_text.len() as u32,
                    );
                    let edit = lattice_protocol::edit::Edit::insert(pos, text);
                    let _ = handle.apply_edit_batch(vec![edit]).await;
                }
            });

            Ok(Some(Subscription::new(bus_handle, sub_id)))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mode_id_is_ai_log_mode() {
        assert_eq!(AiLogMode::mode_id(), ModeId::new("ai-log-mode"));
    }

    #[test]
    fn options_are_read_only_and_no_file() {
        let overrides = AiLogMode.options();
        let has_true = |type_id: std::any::TypeId| {
            overrides.iter().any(|ov| {
                ov.option_type_id == type_id && ov.downcast_value::<bool>() == Some(&true)
            })
        };
        assert!(
            has_true(std::any::TypeId::of::<lattice_config::ReadOnly>()),
            "expected ReadOnly = true override"
        );
        assert!(
            has_true(std::any::TypeId::of::<lattice_config::NoFile>()),
            "expected NoFile = true override"
        );
    }

    #[test]
    fn kind_is_major_with_no_capability_requirements() {
        assert_eq!(AiLogMode.kind(), ModeKind::Major);
        assert_eq!(AiLogMode.required_capabilities(), CapabilitySet::empty());
    }
}
