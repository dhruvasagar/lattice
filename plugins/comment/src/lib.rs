//! CM.3 — `comment`: `gc{motion}` / `gcc` / Visual `gc`, from WASM.
//!
//! The operator toggles line comments over the range the grammar hands it.
//! Everything the plugin owns — the operator, its chord, the option, the help
//! page — hangs off `comment-mode`, so `:set comment.enabled=false` and
//! `:plugin-unload comment` each take the whole surface with them.
//!
//! ## What the toggle does, and why
//!
//! Vim's commentary, Neovim's built-in `gc`, Helix and Zed all agree on the
//! three rules that matter, and disagreeing with them would be the wrong kind
//! of original:
//!
//! 1. **Uncomment only if EVERY non-blank line is already commented.** Mixed
//!    ranges comment. The alternative — per-line toggling — turns a mixed
//!    selection inside out, which is never what anyone wants.
//! 2. **Insert at the range's MINIMUM indent column**, not column 0. Commenting
//!    indented code at column 0 is the thing users notice first and forgive
//!    least.
//! 3. **Skip blank lines.** A commented blank line is trailing whitespace with
//!    extra steps, and it breaks rule 1 on the way back.

wit_bindgen::generate!({
    world: "comment-plugin",
    path: "../../wit",
});

use exports::lattice::plugin_host::grammar_callbacks::Guest as GrammarCallbacks;
use lattice::plugin_host::buffer::Document;
use lattice::plugin_host::config::OptionType;
use lattice::plugin_host::help;
use lattice::plugin_host::modes::{
    ActivationPolicy, ModeCapabilities, ModeDeclaration, ModeKind,
};
use lattice::plugin_host::tree_sitter::TreeSnapshot;
use lattice::plugin_host::types::{
    ActionContext, ApplyEditPayload, Args, Edit, EditKind, Effect, MotionContext, MotionResult,
    OperatorContext, OperatorSpec, Position, Range, TextObjectContext,
};
use lattice::plugin_host::{config, grammar, modes};

mod toggle;

struct Component;

/// The operator's guest-side callback id.
const CB_TOGGLE: u32 = 1;

impl GrammarCallbacks for Component {
    fn apply_operator(
        callback: u32,
        ctx: OperatorContext,
        doc: &Document,
    ) -> Result<Vec<Effect>, String> {
        if callback != CB_TOGGLE {
            return Err(format!("comment: unknown operator callback {callback}"));
        }

        // The grammar hands over an expanded range; the operator is linewise
        // regardless of how the motion arrived, which is what `gc$` doing the
        // whole line means.
        let first = ctx.range.start.line;
        let last = ctx.range.end.line;

        // Graceful and specific: the echo names the reason. A silent no-op
        // here is the failure mode the plugin-host rules keep legislating
        // against — the user presses `gc`, nothing happens, and nothing says
        // why.
        let path = doc.path();
        let Some(leader) = path.as_deref().and_then(toggle::leader_for_path) else {
            return Ok(vec![Effect::Echo(lattice::plugin_host::types::EchoPayload {
                level: lattice::plugin_host::types::EchoLevel::Warn,
                text: match path.as_deref() {
                    None => "comment: this buffer has no file, so no comment syntax".to_string(),
                    Some(p) => format!("comment: no comment syntax known for `{p}`"),
                },
            })]);
        };

        let mut nums = Vec::new();
        let mut texts = Vec::new();
        for n in first..=last {
            if let Some(text) = doc.line(n) {
                nums.push(n);
                texts.push(text);
            }
        }

        // `leader-space` is read per invocation rather than cached: a plugin
        // that snapshots an option at load answers from the value the user had
        // when the editor started, forever.
        // Read per invocation, not cached: a plugin that snapshots an option
        // at load answers from the value the user had when the editor started,
        // forever. `auto-pair::is_manual` reads its own option the same way.
        // Absent or unparseable ⇒ the registered default, `true`.
        let leader_space = config::get_option("leader-space")
            .map(|v| v != "false")
            .unwrap_or(true);

        let mut edits = Vec::new();
        for (i, next) in toggle::toggle(&texts, leader, leader_space)
            .into_iter()
            .enumerate()
        {
            // `None` means the line is unchanged — no edit, so a no-op `gc`
            // stays off the undo stack.
            let Some(next) = next else { continue };
            edits.push(Effect::ApplyEdit(ApplyEditPayload {
                // CM.3: the buffer the operator ran over. A guest holds a
                // read-only handle, so it asks the host to apply rather than
                // mutating — which is why `operator-context` had to carry an
                // id at all.
                target: ctx.buffer_id,
                edit: Edit {
                    range: Range {
                        start: Position {
                            line: nums[i],
                            byte: 0,
                        },
                        end: Position {
                            line: nums[i],
                            byte: texts[i].len() as u32,
                        },
                    },
                    kind: EditKind::Replace(next),
                },
                // Leave the caret where the user put it; vim's `gc` does not
                // move it.
                cursor: None,
            }));
        }
        Ok(edits)
    }

    fn apply_motion(
        _callback: u32,
        _ctx: MotionContext,
        _doc: &Document,
        _tree: Option<&TreeSnapshot>,
    ) -> Result<MotionResult, String> {
        Err("comment contributes no motions".to_string())
    }

    fn apply_text_object(
        _callback: u32,
        _ctx: TextObjectContext,
        _doc: &Document,
        _tree: Option<&TreeSnapshot>,
    ) -> Result<Range, String> {
        Err("comment contributes no text objects".to_string())
    }

    fn apply_action(
        _callback: u32,
        _ctx: ActionContext,
        _doc: &Document,
        _tree: Option<&TreeSnapshot>,
    ) -> Result<Vec<Effect>, String> {
        Err("comment contributes no actions".to_string())
    }

    fn parse_ex_args(_callback: u32, _rest: String, _bang: bool) -> Result<Args, String> {
        Err("comment contributes no ex-commands".to_string())
    }

    fn apply_ex_command(
        _callback: u32,
        _ctx: lattice::plugin_host::types::ExCommandContext,
        _doc: &Document,
        _tree: Option<&TreeSnapshot>,
    ) -> Result<Vec<Effect>, String> {
        Err("comment contributes no ex-commands".to_string())
    }
}

impl Guest for Component {
    fn register_grammar() {
        grammar::register_operator(
            "comment-toggle",
            "toggle line comments over the operated range",
            &OperatorSpec {
                repeatable: true,
                args_schema: Vec::new(),
                blockwise_per_row: false,
                post_motion_char: false,
                // CM.2: the chord travels with the operator. `doubled` is the
                // TRAILING key, so this is `gcc` — the spelling commentary and
                // Neovim use — rather than `gcgc`.
                chord: Some("gc".to_string()),
                doubled: Some("c".to_string()),
            },
            CB_TOGGLE,
        );
    }

    fn register_modes() {
        modes::register_mode(&ModeDeclaration {
            id: "comment-mode".to_string(),
            kind: ModeKind::Minor,
            // `global`, not `universal`: every DOCUMENT buffer. `gc` over
            // user-edited text is the point; `gc` in `*messages*`, the file
            // tree or a help popup is noise.
            activation_policy: ActivationPolicy::Global,
            capabilities: ModeCapabilities::empty(),
            // Not language-scoped: `gc` works in every document buffer, and
            // which leader to use is decided per-buffer from the path.
            target_language: None,
            // No option overrides — the mode changes how keys behave, not how
            // its buffers behave.
            options: Vec::new(),
            // No keymap here. The operator's chord is bound by the host into
            // the operator-pending layer (CM.2) — a plain binding could not
            // give `gc` a motion, and would kill `gcc` besides.
            keymap: Vec::new(),
        });
    }

    fn register_options() {
        // `comment.enabled` is auto-registered by the loader from
        // `default_modes`; this is the plugin's own knob.
        // Short name — the host namespaces it by plugin id, so this
        // registers as `comment.leader-space`. The default is a STRING parsed
        // against `ty`, not a typed literal.
        config::register_option(
            "leader-space",
            OptionType::Boolean,
            "true",
            "insert a space between the comment leader and the code (`// x`, not `//x`)",
        );
    }

    fn register_help_topics() {
        let _ = help::register_topic(
            "",
            "Toggle line comments with `gc` — an operator, so it takes any motion or text object.",
            include_str!("../doc/comment.md"),
            &["comment".to_string()],
        );
    }
}

export!(Component);
