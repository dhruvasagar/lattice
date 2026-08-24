//! The reference org language plugin.
//!
//! Implements the `language-plugin` world: imports `register-language`,
//! exports `register-languages`. The grammar is compiled to wasm by this
//! crate's build.rs and baked in with `include_bytes!`; the queries ship as
//! source and are compiled host-side at registration, so a malformed one
//! fails at load naming the file.
//!
//! OM.2 adds the third seam: `modes`, declaring `org-mode` as the MAJOR for
//! the language registered above. Until majors crossed the seam a `.org` file
//! opened in `text-mode` however good the grammar was, because
//! `major_mode_id_for_lang` is a hand-written match over the `Lang` enum and
//! has no arm for a language the host has never heard of.
//!
//! OM.3 adds the fourth: `grammar`, contributing the promote/demote actions
//! and binding them in `org-mode`'s own keymap layer. This is where the plugin
//! starts EDITING — the actions read the buffer through the `borrow<document>`
//! handle `apply-action` receives and return `Effect::ApplyEdit`.

// A plugin that provides TWO seams needs a world that imports both, and a
// component implements exactly one world. Bundled plugins get theirs written
// into lattice's own `wit/` (`auto-pair-plugin` imports six interfaces) — but
// an EXTERNAL plugin cannot add a world to someone else's package.
//
// It does not need to. WIT `include` composes worlds, and `wit-bindgen`
// resolves an `inline` package against the interfaces found at `path`, so the
// plugin declares its own world locally and gets ONE `Guest` trait carrying
// both exports. Nothing in lattice changes to allow it.
//
// Three details, each of which is a build error if missed:
//   * `include` needs the VERSION (`@0.1.0`) — the resolver knows
//     `lattice:plugin-host@0.1.0`, not `lattice:plugin-host`.
//   * `generate_all` — without it wit-bindgen demands a `with` mapping for
//     every interface reached through the include.
//   * the inline package needs its own name, distinct from lattice's.
wit_bindgen::generate!({
    inline: r#"
        package lattice:org-plugin@0.1.0;
        world org-plugin {
            include lattice:plugin-host/language-plugin@0.1.0;
            include lattice:plugin-host/help-plugin@0.1.0;
            include lattice:plugin-host/modes-plugin@0.1.0;
            include lattice:plugin-host/grammar-plugin@0.1.0;
        }
    "#,
    path: "../../wit",
    world: "org-plugin",
    generate_all,
});

mod headline;

use exports::lattice::plugin_host::grammar_callbacks::Guest as GrammarCallbacks;
use lattice::plugin_host::buffer::Document;
use lattice::plugin_host::grammar::{register_action, register_motion, register_text_object};
use lattice::plugin_host::help::register_topic;
use lattice::plugin_host::language::{LanguageSpec, register_language};
use lattice::plugin_host::modes::{
    ActivationPolicy, BindingMode, ModeCapabilities, ModeDeclaration, ModeKeymapBinding, ModeKind,
    register_mode,
};
use lattice::plugin_host::tree_sitter::TreeSnapshot;
use lattice::plugin_host::types::{
    ActionContext, ActionSpec, Args, Edit, EditKind, Effect, ExCommandContext, MotionContext,
    MotionResult, MotionSpec, OperatorContext, Position, Range, TextObjectContext,
    TextObjectSpec,
};

/// Callback ids for `apply-action`. The guest chooses these; the host only
/// hands them back (§6 — a plugin cannot forge a `CommandId`).
const PROMOTE_HEADLINE: u32 = 1;
const DEMOTE_HEADLINE: u32 = 2;
const PROMOTE_SUBTREE: u32 = 3;
const DEMOTE_SUBTREE: u32 = 4;

/// Callback ids for `apply-motion` — a separate space from the action ids
/// above, since the host dispatches each export by its own callback number.
const NEXT_HEADLINE: u32 = 1;
const PREV_HEADLINE: u32 = 2;
const PARENT_HEADLINE: u32 = 3;

/// Callback ids for `apply-text-object`.
const INNER_HEADLINE: u32 = 1;
const AROUND_HEADLINE: u32 = 2;
const INNER_SUBTREE: u32 = 3;
const AROUND_SUBTREE: u32 = 4;

const GRAMMAR: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/grammar.wasm"));

struct Component;

impl Guest for Component {
    fn register_languages() {
        let _ = register_language(&LanguageSpec {
            name: "org".to_string(),
            // The grammar's export is `tree_sitter_org`, which matches the
            // language name — so this is the common case and the field is
            // absent. It exists for grammars whose upstream name differs
            // (lattice's own `sql` rides `sequel`).
            grammar_name: None,
            extensions: vec!["org".to_string(), "org_archive".to_string()],
            grammar: GRAMMAR.to_vec(),
            highlights: Some(include_str!("../queries/highlights.scm").to_string()),
            folds: Some(include_str!("../queries/folds.scm").to_string()),
            injections: None,
            indents: None,
            textobjects: None,
        });
    }

    /// `org-mode`, the major for the language declared above (OM.2).
    ///
    /// `target_language` is the whole point: it names the language by the same
    /// canonical string `register_language` used, the host indexes it, and a
    /// `.org` document resolves onto this mode through the ordinary
    /// `resolve_major_mode` path — the same one `rust-mode` takes. There is no
    /// org branch anywhere in the host.
    ///
    /// `Manual` activation is not a contradiction: the policy governs
    /// *explicit* activation, while a major bound to a language is activated by
    /// language resolution. It is the minors (org-todo, org-table) that will
    /// carry `Majors(["org-mode"])`.
    ///
    /// OM.3: the mode now carries its chords. Each binds to an action THIS
    /// plugin registered through the `grammar` seam, resolved by name against
    /// the command registry at bind time — which is why the loader must drain
    /// `grammar` before `modes` (OM.0 made that structural rather than a
    /// comment in this file's manifest).
    ///
    /// `<leader>oh` / `ol` and their capitals are the adapted nvim-orgmode set.
    /// nvim binds `<<` / `>>` / `<s` / `>s`, none of which are reachable here:
    /// `<` and `>` are TERMINAL operator bindings, so the trie resolves them on
    /// the first key and the second never arrives. Shadowing the operators
    /// inside org buffers would have bought the literal chords at the price of
    /// `>ap` and `ciw`, which is a bad trade for one filetype (org-mode.md
    /// §5.1). The letters are evil-org's directional `h`/`l`, so the mnemonic
    /// survives the move.
    fn register_modes() {
        let bind = |chord: &str, command: &str| ModeKeymapBinding {
            binding_mode: BindingMode::Normal,
            chord: chord.to_string(),
            command: command.to_string(),
        };
        register_mode(&ModeDeclaration {
            id: "org-mode".to_string(),
            kind: ModeKind::Major,
            activation_policy: ActivationPolicy::Manual,
            capabilities: ModeCapabilities::empty(),
            keymap: vec![
                bind("<leader>oh", "org-promote-headline"),
                bind("<leader>ol", "org-demote-headline"),
                bind("<leader>oH", "org-promote-subtree"),
                bind("<leader>oL", "org-demote-subtree"),
                // Motions, kept verbatim from nvim-orgmode — `]` and `[` are
                // prefixes rather than terminal bindings, so unlike `>>` / `<<`
                // these transplant unchanged. `g{` is emacs's
                // `outline-up-heading`; lattice's own `zp` walks the FOLD
                // hierarchy, which coincides with the headline hierarchy in org
                // but is a different thing, so both earn their place.
                bind("]]", "org-next-headline"),
                bind("[[", "org-prev-headline"),
                bind("g{", "org-parent-headline"),
                // Text objects (OM.4b). The host sees that these name TEXT
                // OBJECTS rather than actions and expands each into
                // `<operator><chord>` rows in this mode's own layer, plus a
                // Visual binding — so `dar` deletes a subtree through the
                // ORDINARY delete operator and no org-specific chord is
                // involved.
                //
                // `ir` / `ar` for the subtree, not `is` / `as`: `s` is already
                // vim's SENTENCE object, and nvim-orgmode uses `r` (subtRee)
                // for exactly that reason.
                bind("ih", "org-inner-headline"),
                bind("ah", "org-around-headline"),
                bind("ir", "org-inner-subtree"),
                bind("ar", "org-around-subtree"),
            ],
            target_language: Some("org".to_string()),
        });
    }

    /// The promote/demote actions (OM.3).
    ///
    /// Registered as ACTIONS rather than operators: promote takes no motion and
    /// composes with nothing, which is what an action is for. The structural
    /// text objects that DO compose with operators (`ih`/`ah`/`is`/`as`) are a
    /// separate contribution, OM.4.
    fn register_grammar() {
        let spec = || ActionSpec {
            args_schema: Vec::new(),
        };
        // Motions, not actions: a motion composes with an operator, so `d]]`
        // deletes to the next headline and `3]]` takes a count — which is the
        // whole reason for the distinction (paramount goal #3). `jump: true`
        // records a position-history entry, matching `}` / `G` / `]]` in a
        // source file: a headline jump is somewhere you want `<C-o>` to bring
        // you back from.
        let motion = || MotionSpec {
            jump: true,
            // Exclusive: `d]]` deletes up to but not including the next
            // headline, which is what "delete this section" means.
            exclusive: true,
            args_schema: Vec::new(),
        };
        register_motion(
            "org-next-headline",
            "Move to the next headline, at any level",
            &motion(),
            NEXT_HEADLINE,
        );
        register_motion(
            "org-prev-headline",
            "Move to the previous headline, at any level",
            &motion(),
            PREV_HEADLINE,
        );
        register_motion(
            "org-parent-headline",
            "Move to the parent of the headline at the cursor",
            &motion(),
            PARENT_HEADLINE,
        );
        let tobj = || TextObjectSpec {
            args_schema: Vec::new(),
        };
        register_text_object(
            "org-inner-headline",
            "The headline's title, without its stars",
            &tobj(),
            INNER_HEADLINE,
        );
        register_text_object(
            "org-around-headline",
            "The whole headline line, stars included",
            &tobj(),
            AROUND_HEADLINE,
        );
        register_text_object(
            "org-inner-subtree",
            "A subtree's body — everything under the headline, not the headline",
            &tobj(),
            INNER_SUBTREE,
        );
        register_text_object(
            "org-around-subtree",
            "A whole subtree: the headline and everything under it",
            &tobj(),
            AROUND_SUBTREE,
        );
        register_action(
            "org-promote-headline",
            "Promote the headline at the cursor one level",
            &spec(),
            PROMOTE_HEADLINE,
        );
        register_action(
            "org-demote-headline",
            "Demote the headline at the cursor one level",
            &spec(),
            DEMOTE_HEADLINE,
        );
        register_action(
            "org-promote-subtree",
            "Promote the headline at the cursor and its whole subtree",
            &spec(),
            PROMOTE_SUBTREE,
        );
        register_action(
            "org-demote-subtree",
            "Demote the headline at the cursor and its whole subtree",
            &spec(),
            DEMOTE_SUBTREE,
        );
    }

    /// Org's manual, compiled into this component and handed over once at
    /// load — the `help` seam's premise: the docs travel with the thing they
    /// document, and unloading the plugin removes them.
    ///
    /// An empty name lands at the bare plugin id, so this is `:help org`
    /// rather than `:help org.org`.
    fn register_help_topics() {
        let _ = register_topic(
            "",
            "Org files: headlines, folding, and what this plugin does not do.",
            include_str!("../doc/org.md"),
            // `:describe-command` cross-links from any command whose name
            // contains these.
            &["fold".to_string()],
        );
    }
}

/// The shared body of all four promote/demote actions.
///
/// `delta` is -1 to promote, +1 to demote; `whole_subtree` decides whether the
/// rewritten span stops at the headline or runs to the end of its subtree.
///
/// Lines are read through the `document` handle ONE AT A TIME, on demand.
/// Materialising the buffer first would be simpler and would cost one
/// guest→host call per line — 10,000 of them on a 10,000-line file, every time
/// this key is pressed, which is a missed frame rather than a slow key
/// (paramount goal #1). As written, a headline op reads the handful of lines
/// between the caret and its headline, and a subtree op reads its subtree,
/// which it must read anyway to rewrite it.
///
/// Declines (rather than erroring) whenever there is nothing to do: the cursor
/// is in a file's preamble with no headline above it, or the shift is refused
/// at level 1. `Effect::Declined` means the chord was not consumed, so the
/// dispatcher re-resolves it against the layers below — `<leader>oh` in a
/// buffer with no headlines falls through instead of swallowing the key.
fn shift(ctx: &ActionContext, doc: &Document, delta: isize, whole_subtree: bool) -> Vec<Effect> {
    let line = |n: u32| doc.line(n);
    let Some((start, _level)) = headline::enclosing_headline(line, ctx.cursor.line) else {
        return vec![Effect::Declined];
    };
    let end = if whole_subtree {
        headline::subtree_end(line, start, doc.line_count())
    } else {
        start
    };
    let Some((text, end_len)) = headline::shift_headlines(line, start, end, delta) else {
        return vec![Effect::Declined];
    };

    // ONE edit over the whole span — see `shift_headlines`. The range runs from
    // column 0 of the root headline to the end of the last line in the span,
    // exclusive of its newline, so the replacement never disturbs the line
    // structure around it.
    vec![Effect::ApplyEdit(lattice::plugin_host::types::ApplyEditPayload {
        target: ctx.buffer_id,
        edit: Edit {
            range: Range {
                start: Position {
                    line: start,
                    byte: 0,
                },
                end: Position {
                    line: end,
                    byte: end_len,
                },
            },
            kind: EditKind::Replace(text),
        },
        // Keep the caret on its own line, and clamp its column: promoting
        // `*** Deep` to `** Deep` shortens the line, and a caret parked past
        // the new end would be a visible jump on a key that only restars.
        cursor: Some(clamped_cursor(ctx, line, start, end, delta)),
    })]
}

/// Where the caret lands after a shift: same line, column moved by the same
/// number of stars the line gained or lost, floored at 0.
///
/// Only headline lines within the span move, so a caret in body text stays
/// exactly where it was.
fn clamped_cursor(
    ctx: &ActionContext,
    line: impl Fn(u32) -> Option<String>,
    start: u32,
    end: u32,
    delta: isize,
) -> Position {
    let at = ctx.cursor.line;
    // One extra line read, and only when the caret is inside the rewritten
    // span — a caret in body text costs nothing.
    let moved = at >= start
        && at <= end
        && line(at)
            .as_deref()
            .and_then(headline::headline_level)
            .is_some();
    let byte = if moved {
        (ctx.cursor.byte as isize + delta).max(0) as u32
    } else {
        ctx.cursor.byte
    };
    Position {
        line: ctx.cursor.line,
        byte,
    }
}

impl GrammarCallbacks for Component {
    fn apply_action(
        callback: u32,
        ctx: ActionContext,
        doc: &Document,
        _tree: Option<&TreeSnapshot>,
    ) -> Result<Vec<Effect>, String> {
        match callback {
            PROMOTE_HEADLINE => Ok(shift(&ctx, doc, -1, false)),
            DEMOTE_HEADLINE => Ok(shift(&ctx, doc, 1, false)),
            PROMOTE_SUBTREE => Ok(shift(&ctx, doc, -1, true)),
            DEMOTE_SUBTREE => Ok(shift(&ctx, doc, 1, true)),
            other => Err(format!("org: unknown action callback {other}")),
        }
    }

    // Org contributes no motions, operators or ex-commands yet; the text
    // objects land at OM.4. An `err` here is logged and the contribution
    // no-ops, so an unreachable callback can never wedge the keystroke path.
    /// The headline motions (OM.4).
    ///
    /// A motion must return SOMEWHERE, so "no headline that way" resolves to
    /// the cursor's own line rather than an `err`: an `err` is logged and the
    /// contribution no-ops, which is right for a broken motion and wrong for
    /// `]]` at the last headline. Staying put is what `}` does at the end of a
    /// buffer.
    ///
    /// `count` repeats the step, so `3]]` walks three headlines — applied by
    /// stepping rather than by multiplying, since headlines are not evenly
    /// spaced. A count that runs off the end stops at the last one.
    fn apply_motion(
        callback: u32,
        ctx: MotionContext,
        doc: &Document,
    ) -> Result<MotionResult, String> {
        let line = |n: u32| doc.line(n);
        let count = ctx.count.max(1);
        let mut at = ctx.from.line;
        for _ in 0..count {
            let next = match callback {
                NEXT_HEADLINE => headline::next_headline(line, at, doc.line_count()),
                PREV_HEADLINE => headline::prev_headline(line, at),
                PARENT_HEADLINE => headline::parent_headline(line, at),
                other => return Err(format!("org: unknown motion callback {other}")),
            };
            match next {
                Some(target) => at = target,
                // Ran out part-way through a count: stop where we got to
                // rather than abandoning the whole motion.
                None => break,
            }
        }
        Ok(MotionResult {
            target: Position { line: at, byte: 0 },
            // Charwise: `d]]` should delete to the start of the next headline,
            // not swallow the headline's own line.
            linewise: false,
        })
    }
    fn apply_operator(_c: u32, _ctx: OperatorContext) -> Result<Vec<Effect>, String> {
        Err("org: no operators".into())
    }
    /// The headline / subtree text objects (OM.4b).
    ///
    /// `inner` vs `around` follows vim's own distinction rather than inventing
    /// one: *around* takes the structural marker with it, *inner* leaves it.
    /// So `ah` is the whole headline line and `ih` its title without the stars;
    /// `ar` is a subtree headline-and-all, `ir` its body with the headline left
    /// standing. `dir` empties a section, `dar` removes it.
    ///
    /// An `err` here is logged and the contribution no-ops, leaving the
    /// operator with nothing to act on — which is the right outcome for `dar`
    /// with the cursor in a file's preamble.
    fn apply_text_object(
        callback: u32,
        ctx: TextObjectContext,
        doc: &Document,
    ) -> Result<Range, String> {
        let line = |n: u32| doc.line(n);
        let (start, _level) = headline::enclosing_headline(line, ctx.at.line)
            .ok_or("org: no headline at or above the cursor")?;
        let head = line(start).ok_or("org: headline vanished mid-read")?;
        let stars = headline::headline_level(&head).ok_or("org: not a headline")?;

        let range = match callback {
            // The headline's title: after the stars and their space, to the
            // end of the line.
            INNER_HEADLINE => Range {
                start: Position {
                    line: start,
                    byte: (stars + 1) as u32,
                },
                end: Position {
                    line: start,
                    byte: head.len() as u32,
                },
            },
            // The whole headline line, stars included.
            AROUND_HEADLINE => Range {
                start: Position {
                    line: start,
                    byte: 0,
                },
                end: Position {
                    line: start,
                    byte: head.len() as u32,
                },
            },
            // The subtree's BODY — everything under the headline, the
            // headline itself left standing. `dir` empties a section.
            INNER_SUBTREE => {
                let end = headline::subtree_end(line, start, doc.line_count());
                if end == start {
                    // A childless headline has no inner subtree; an empty
                    // range at its end is the honest answer, and `dir` on it
                    // deletes nothing rather than eating the headline.
                    Range {
                        start: Position {
                            line: start,
                            byte: head.len() as u32,
                        },
                        end: Position {
                            line: start,
                            byte: head.len() as u32,
                        },
                    }
                } else {
                    Range {
                        start: Position {
                            line: start + 1,
                            byte: 0,
                        },
                        end: Position {
                            line: end,
                            byte: line(end).map(|l| l.len()).unwrap_or(0) as u32,
                        },
                    }
                }
            }
            // The whole subtree, headline and all. `dar` removes a section.
            AROUND_SUBTREE => {
                let end = headline::subtree_end(line, start, doc.line_count());
                Range {
                    start: Position {
                        line: start,
                        byte: 0,
                    },
                    end: Position {
                        line: end,
                        byte: line(end).map(|l| l.len()).unwrap_or(0) as u32,
                    },
                }
            }
            other => return Err(format!("org: unknown text-object callback {other}")),
        };
        Ok(range)
    }
    fn parse_ex_args(_c: u32, _rest: String, _bang: bool) -> Result<Args, String> {
        Err("org: no ex-commands".into())
    }
    fn apply_ex_command(_c: u32, _ctx: ExCommandContext) -> Result<Vec<Effect>, String> {
        Err("org: no ex-commands".into())
    }
}

export!(Component);
