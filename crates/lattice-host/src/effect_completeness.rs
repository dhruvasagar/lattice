//! Which effects the host finishes by itself.
//!
//! [`Editor::dispatch`](crate::editor::Editor) applies the host's half of
//! every effect and hands the list back in `DispatchOutcome::effects` so a
//! renderer peer can run ITS half: `OpenBuffer` ends in a window-management
//! call, `QuitEditor` in closing a terminal or a window. Many effects have no
//! such half. A cursor move, an edit, a yank, a mode change are finished the
//! moment the host has applied them, and a peer's arm for them is `=> {}`.
//!
//! [`Editor::dispatch_fused`](crate::editor::Editor::dispatch_fused) needs to
//! tell the two apart. It runs the post-dispatch tail in the same actor
//! crossing as the dispatch only when the peer has nothing to do first, and
//! it used to decide that by `effects.is_empty()` — which is true for a typed
//! character and false for every motion and operator, whose effects are all
//! of the finished kind. So `j` cost several blocking round-trips where `x`
//! in Insert cost one, and held-key motion is where latency is felt most.

use lattice_grammar::Effect;

/// Has the host finished this effect, leaving a renderer peer nothing to do?
///
/// `true` only where BOTH peers' renderer-coupled arm for the variant is a
/// no-op (`App::apply_effect_app_arms` in the TUI, `GpuiApp::apply_effect_gpui`
/// in the GUI). The list is deliberately short — the effects a keystroke on
/// the editing hot path produces — not every variant that happens to qualify
/// today: an effect answered `false` costs a few round-trips on a path nobody
/// holds a key down on, and one wrongly answered `true` reorders a peer's
/// handler behind the tail.
///
/// **No wildcard arm.** A new `Effect` variant does not compile until it is
/// placed, and `false` is the answer that is always safe.
///
/// A wrong `true` is contained rather than silent: both peers still run every
/// returned effect through their arms on the fused path (a no-op for these),
/// so a misplaced variant runs late, not never.
pub fn effect_is_host_complete(effect: &Effect) -> bool {
    match effect {
        Effect::Many(parts) => parts.iter().all(effect_is_host_complete),

        Effect::None
        | Effect::Declined
        | Effect::Edits(..)
        | Effect::SelectionChange(..)
        | Effect::CursorMove(..)
        | Effect::CursorMoveIn { .. }
        | Effect::Yank { .. }
        | Effect::EnterMode(..)
        | Effect::ClearSearchHighlight
        | Effect::Echo { .. }
        | Effect::RecordJump => true,

        Effect::ApplyEdit { .. }
        | Effect::WriteToFile { .. }
        | Effect::SaveBuffer { .. }
        | Effect::QuitEditor { .. }
        | Effect::OpenBuffer { .. }
        | Effect::OpenBufferAt { .. }
        | Effect::OpenInTarget { .. }
        | Effect::OilNavigate { .. }
        | Effect::FileTreeToggle { .. }
        | Effect::OpenExternalUri { .. }
        | Effect::OpenBufferAtColumn { .. }
        | Effect::SpawnTerminal { .. }
        | Effect::TerminalInput(..)
        | Effect::SetOption { .. }
        | Effect::SetLocalOption { .. }
        | Effect::SetGlobalOption { .. }
        | Effect::SetColorscheme(..)
        | Effect::ShowDiagnosticsPopup { .. }
        | Effect::Lsp(..)
        | Effect::EchoRegisters
        | Effect::EchoMarks
        | Effect::Substitute { .. }
        | Effect::Global { .. }
        | Effect::DeleteCurrentLine
        | Effect::FilterLines { .. }
        | Effect::DescribeCommand { .. }
        | Effect::DescribeBuffer
        | Effect::Apropos { .. }
        | Effect::DescribeKey { .. }
        | Effect::ListKeymap
        | Effect::BufferNext
        | Effect::BufferPrev
        | Effect::FocusBuffer(..)
        | Effect::InvokeCommand { .. }
        | Effect::ListBuffers
        | Effect::ChangeDir(..)
        | Effect::PrintWorkingDir
        | Effect::PrintProjectRoot
        | Effect::OpenBufferPicker
        | Effect::OpenPicker { .. }
        | Effect::BufferDelete { .. }
        | Effect::OpenFileTree { .. }
        | Effect::CloseFileTree
        | Effect::OpenOil { .. }
        | Effect::DescribeOption { .. }
        | Effect::DescribeElement { .. }
        | Effect::ListOptions
        | Effect::DescribePluginApi { .. }
        | Effect::ListPluginApis
        | Effect::ExportPluginApi { .. }
        | Effect::ListCommands
        | Effect::DescribePlugin { .. }
        | Effect::ListPlugins
        | Effect::OpenHover { .. }
        | Effect::DismissPopup
        | Effect::DismissPopupNamed { .. }
        | Effect::OpenPopup { .. }
        | Effect::OpenHelpTopic { .. }
        | Effect::ListDiagnostics
        | Effect::ListErrors
        | Effect::NextDiagnostic
        | Effect::PrevDiagnostic
        | Effect::OpenLspLog { .. }
        | Effect::OpenAiLog { .. }
        | Effect::OpenSyntheticBuffer { .. }
        | Effect::OpenSyntheticBufferAt { .. }
        | Effect::OpenMessages
        | Effect::OpenDashboard
        | Effect::ToggleLspTrace { .. }
        | Effect::OpenLspTraceLog { .. }
        | Effect::LspStatus
        | Effect::LspDiagnosticsToErrorList
        | Effect::LspServerLogListing
        | Effect::LspRestart { .. }
        | Effect::LspProgressCancel { .. }
        | Effect::LspExpandRegion
        | Effect::LspShrinkRegion
        | Effect::SetLspLogLevel { .. }
        | Effect::LspLogClear { .. }
        | Effect::LspDocumentSymbol
        | Effect::LspWorkspaceSymbol { .. }
        | Effect::LspIncomingCalls
        | Effect::LspOutgoingCalls
        | Effect::LspSupertypes
        | Effect::LspSubtypes
        | Effect::LspMoniker
        | Effect::LspCodeLens
        | Effect::LspColorPresentation
        | Effect::LspFormat
        | Effect::Format
        | Effect::LspFormatRange
        | Effect::LspSignatureHelp
        | Effect::LspComplete
        | Effect::LspRename { .. }
        | Effect::LspCodeAction
        | Effect::ExpandSnippet { .. }
        | Effect::ReloadSnippets
        | Effect::DescribeEvents
        | Effect::DescribeDiff
        | Effect::DiffOpen
        | Effect::DiffOff { .. }
        | Effect::Diffthis
        | Effect::Diffsplit { .. }
        | Effect::DiffGetCmd { .. }
        | Effect::DiffPutCmd { .. }
        | Effect::DiffAccept
        | Effect::DiffReject
        | Effect::DiffAcceptAll
        | Effect::DiffRejectAll
        | Effect::CloseSessionDiffs { .. }
        | Effect::CloseAllSessionDiffs { .. }
        | Effect::NextHunk
        | Effect::PrevHunk
        | Effect::DescribeEvent { .. }
        | Effect::ListModes
        | Effect::DescribeMode { .. }
        | Effect::DescribeActiveModes
        | Effect::DescribeActiveBindings
        | Effect::DescribeOptionResolution { .. }
        | Effect::Customize { .. }
        | Effect::Tutor { .. }
        | Effect::ToggleMode { .. }
        | Effect::AppAction(..)
        | Effect::Confirm { .. }
        | Effect::BuryBuffer
        | Effect::KillBuffer
        | Effect::OpenTransient { .. }
        | Effect::OpenPrompt { .. } => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lattice_protocol::position::Position;

    #[test]
    fn a_cursor_move_is_host_complete_and_an_open_is_not() {
        assert!(effect_is_host_complete(&Effect::CursorMove(Position::new(
            1, 0
        ))));
        assert!(!effect_is_host_complete(&Effect::OpenBuffer {
            path: None,
            force: false,
        }));
    }

    /// One renderer-coupled part makes the whole group renderer-coupled: the
    /// peer has to run before the tail for that part, so it does for all.
    #[test]
    fn a_group_is_host_complete_only_if_every_part_is() {
        let mv = || Effect::CursorMove(Position::new(1, 0));
        assert!(effect_is_host_complete(&Effect::Many(vec![mv(), mv()])));
        assert!(!effect_is_host_complete(&Effect::Many(vec![
            mv(),
            Effect::OpenBuffer {
                path: None,
                force: false,
            },
        ])));
        assert!(
            effect_is_host_complete(&Effect::Many(Vec::new())),
            "an empty group asks nothing of anyone"
        );
    }
}
