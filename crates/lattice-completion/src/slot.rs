//! Command-line slot detection (DESIGN.md §5.11.3).
//!
//! Given the current text on the `:` line and a cursor position,
//! produce a [`CommandLineSlot`] describing what the cursor is
//! pointing at -- the command name, the N-th positional arg, etc. --
//! plus the prefix (text the user has already typed in this slot)
//! and the byte range to replace when a candidate is accepted.
//!
//! The host (cmdline parser in `lattice-ui-tui`) calls
//! [`current_slot`] to figure out *which* completion source to
//! invoke. The `ArgSpec.completion` field on each command's
//! arg-schema declares the source per-arg, so the slot resolution
//! pulls the right generator id straight from the registry.

use lattice_grammar::{ArgSpec, CommandKind, CommandRegistry};

/// What the cursor on the `:` line is pointing at.
#[derive(Debug, Clone, PartialEq)]
pub enum CommandLineSlot {
    /// Cursor is in (or right after) the command name. The
    /// completion source is conventionally `gen:commands`.
    CommandName {
        /// Partial text typed so far for the command name.
        prefix: String,
        /// Byte offset within the cmdline where the prefix begins
        /// (so the host can replace `[replace_start, cursor)` when
        /// the user accepts a candidate).
        replace_start: usize,
    },
    /// Cursor is in the N-th positional arg of the resolved
    /// command. `arg_spec` is the schema entry; `command_name` is
    /// the canonical registry name (so the host can re-look-up if
    /// it needs to).
    Arg {
        command_name: String,
        arg_index: usize,
        arg_spec: ArgSpec,
        prefix: String,
        replace_start: usize,
    },
    /// Cursor is past the schema's declared args (e.g. extra
    /// trailing whitespace on a no-arg command). No completion.
    BeyondSchema { command_name: String },
    /// Cursor is on a delimiter-syntax command body
    /// (`:s/pattern/replacement/flags`, `:g/pattern/body`). v1
    /// returns this without slot specifics; the cmdline parser may
    /// special-case if it wants to (e.g. complete commands inside
    /// `:g` body). Default: no completion.
    DelimiterBody { command_name: String, body: String },
    /// Couldn't resolve the command word to a registered command.
    /// No completion possible (we don't know the schema).
    UnknownCommand { word: String, replace_start: usize },
    /// Empty cmdline. Treated as command-name slot with empty
    /// prefix.
    Empty,
}

impl CommandLineSlot {
    /// Convenience: get the prefix the matcher should run against.
    pub fn prefix(&self) -> &str {
        match self {
            Self::CommandName { prefix, .. } => prefix,
            Self::Arg { prefix, .. } => prefix,
            Self::UnknownCommand { word, .. } => word,
            Self::DelimiterBody { body, .. } => body,
            Self::BeyondSchema { .. } | Self::Empty => "",
        }
    }

    pub fn replace_start(&self) -> Option<usize> {
        match self {
            Self::CommandName { replace_start, .. } => Some(*replace_start),
            Self::Arg { replace_start, .. } => Some(*replace_start),
            Self::UnknownCommand { replace_start, .. } => Some(*replace_start),
            Self::Empty => Some(0),
            Self::BeyondSchema { .. } | Self::DelimiterBody { .. } => None,
        }
    }
}

/// Detect what slot the cursor is on. `line` is the text on the
/// command line (without the leading `:`); `cursor` is a byte
/// offset within `line` (`0..=line.len()`).
///
/// `alias_resolve` is a callback the host supplies to translate
/// short forms (`w` -> `ex:write`) before looking up the command
/// in the registry. Living in the host means
/// `lattice-completion` doesn't bake in TUI-specific aliases.
pub fn current_slot(
    line: &str,
    cursor: usize,
    registry: &CommandRegistry,
    alias_resolve: &dyn Fn(&str) -> Option<String>,
) -> CommandLineSlot {
    let line = &line[..cursor.min(line.len())];

    // Empty / all-whitespace -> command-name slot, empty prefix.
    if line.trim().is_empty() {
        return CommandLineSlot::Empty;
    }

    // Delimiter-syntax detection. The :s/.../.../ and :g/.../body
    // forms are flagged but not slot-typed deeply in v1 (Q9 from
    // the design discussion).
    if let Some(rest) = line.strip_prefix("s/") {
        return CommandLineSlot::DelimiterBody {
            command_name: "ex:substitute".into(),
            body: rest.to_string(),
        };
    }
    if let Some(rest) = line.strip_prefix("%s/") {
        return CommandLineSlot::DelimiterBody {
            command_name: "ex:substitute".into(),
            body: rest.to_string(),
        };
    }
    if let Some(rest) = line.strip_prefix("g/") {
        return CommandLineSlot::DelimiterBody {
            command_name: "ex:global".into(),
            body: rest.to_string(),
        };
    }
    if let Some(rest) = line.strip_prefix("v/") {
        return CommandLineSlot::DelimiterBody {
            command_name: "ex:global".into(),
            body: rest.to_string(),
        };
    }

    // Tokenize on whitespace. The first whitespace splits the
    // command word from the rest.
    let leading_ws = line.bytes().take_while(|b| b.is_ascii_whitespace()).count();
    let after_ws = &line[leading_ws..];
    let cmd_end = after_ws.find(char::is_whitespace).unwrap_or(after_ws.len());

    if cmd_end == after_ws.len() {
        // Cursor is still in the command word (no whitespace yet).
        return CommandLineSlot::CommandName {
            prefix: after_ws.to_string(),
            replace_start: leading_ws,
        };
    }

    // We have a command word + trailing text.
    let cmd_word_with_bang = &after_ws[..cmd_end];
    // Strip a trailing `!` for alias resolution but keep the bang
    // accounted for in offsets.
    let cmd_word = cmd_word_with_bang.trim_end_matches('!');
    // Resolution order matches `excommand::parse_invocation`:
    // try the typed text as a canonical registry name first
    // (so `ex:describe-key` resolves), then alias-expand if that
    // misses (so `describe-key` resolves too). Both forms reach
    // the same spec; this is what lets the slot detector agree
    // with the parser regardless of which form the user typed.
    let id = if let Some(id) = registry.id_by_name(cmd_word) {
        id
    } else if let Some(canonical) = alias_resolve(cmd_word) {
        match registry.id_by_name(&canonical) {
            Some(id) => id,
            None => {
                return CommandLineSlot::UnknownCommand {
                    word: cmd_word.to_string(),
                    replace_start: leading_ws,
                };
            }
        }
    } else {
        return CommandLineSlot::UnknownCommand {
            word: cmd_word.to_string(),
            replace_start: leading_ws,
        };
    };
    let spec = match registry.lookup(id) {
        Some(s) => s,
        None => {
            return CommandLineSlot::UnknownCommand {
                word: cmd_word.to_string(),
                replace_start: leading_ws,
            };
        }
    };
    if spec.kind != CommandKind::ExCommand {
        return CommandLineSlot::UnknownCommand {
            word: cmd_word.to_string(),
            replace_start: leading_ws,
        };
    }

    // Walk past the command word + the separating whitespace.
    let after_cmd = &after_ws[cmd_end..];
    let post_cmd_ws = after_cmd
        .bytes()
        .take_while(|b| b.is_ascii_whitespace())
        .count();
    let arg_text = &after_cmd[post_cmd_ws..];

    // Identify which positional arg the cursor is on: count
    // whitespace-separated arg-tokens before the cursor.
    let arg_index = arg_text.split_ascii_whitespace().count();
    // The "current arg" is the last whitespace-delimited token.
    // If `arg_text` ends with whitespace, the user hasn't typed
    // anything yet for the next arg; that's still a valid slot
    // (with empty prefix).
    let (current_prefix, prefix_offset) =
        if arg_text.ends_with(char::is_whitespace) || arg_text.is_empty() {
            ("", arg_text.len())
        } else {
            // Find the last whitespace boundary before cursor.
            let last_ws = arg_text
                .rfind(char::is_whitespace)
                .map(|i| i + 1)
                .unwrap_or(0);
            (&arg_text[last_ws..], last_ws)
        };
    let arg_index = if arg_text.ends_with(char::is_whitespace) || arg_text.is_empty() {
        arg_index // pointing at the next, untyped slot
    } else {
        arg_index - 1 // pointing at the last typed token
    };

    let replace_start = leading_ws + cmd_end + post_cmd_ws + prefix_offset;

    if arg_index >= spec.args_schema.len() {
        return CommandLineSlot::BeyondSchema {
            command_name: spec.name.clone(),
        };
    }
    CommandLineSlot::Arg {
        command_name: spec.name.clone(),
        arg_index,
        arg_spec: spec.args_schema[arg_index].clone(),
        prefix: current_prefix.to_string(),
        replace_start,
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]
    use super::*;
    use lattice_grammar::ex_commands;

    fn fixture() -> CommandRegistry {
        let mut r = CommandRegistry::new();
        let _ = lattice_grammar::builtins::populate(&mut r);
        let _ = ex_commands::populate(&mut r);
        r
    }

    fn aliases(short: &str) -> Option<String> {
        let table: &[(&str, &str)] = &[
            ("w", "ex:write"),
            ("write", "ex:write"),
            ("q", "ex:quit"),
            ("quit", "ex:quit"),
            ("e", "ex:edit"),
            ("edit", "ex:edit"),
            ("set", "ex:set"),
            ("describe-command", "ex:describe-command"),
            ("describe-key", "ex:describe-key"),
            ("apropos", "ex:apropos"),
        ];
        table
            .iter()
            .find_map(|(s, c)| (*s == short).then(|| (*c).to_string()))
    }

    #[test]
    fn empty_line_is_empty_slot() {
        let r = fixture();
        let slot = current_slot("", 0, &r, &aliases);
        assert!(matches!(slot, CommandLineSlot::Empty));
    }

    #[test]
    fn whitespace_only_line_is_empty_slot() {
        let r = fixture();
        let slot = current_slot("   ", 3, &r, &aliases);
        assert!(matches!(slot, CommandLineSlot::Empty));
    }

    #[test]
    fn partial_command_word_is_command_name_slot() {
        let r = fixture();
        let slot = current_slot("descri", 6, &r, &aliases);
        match slot {
            CommandLineSlot::CommandName {
                prefix,
                replace_start,
            } => {
                assert_eq!(prefix, "descri");
                assert_eq!(replace_start, 0);
            }
            other => panic!("expected CommandName, got {other:?}"),
        }
    }

    #[test]
    fn unknown_command_is_unknown_slot() {
        let r = fixture();
        let slot = current_slot("xyzzy ", 6, &r, &aliases);
        assert!(matches!(slot, CommandLineSlot::UnknownCommand { .. }));
    }

    #[test]
    fn first_arg_after_known_command_is_arg_slot_index_zero() {
        let r = fixture();
        // ":describe-command moti" -- cursor at end. arg 0 of
        // describe-command is "name" with String kind.
        let line = "describe-command moti";
        let slot = current_slot(line, line.len(), &r, &aliases);
        match slot {
            CommandLineSlot::Arg {
                command_name,
                arg_index,
                arg_spec,
                prefix,
                ..
            } => {
                assert_eq!(command_name, "ex:describe-command");
                assert_eq!(arg_index, 0);
                assert_eq!(arg_spec.name, "name");
                assert_eq!(prefix, "moti");
            }
            other => panic!("expected Arg, got {other:?}"),
        }
    }

    #[test]
    fn replace_start_points_at_prefix_start() {
        let r = fixture();
        let line = "describe-command moti";
        // "moti" begins at byte 17.
        let slot = current_slot(line, line.len(), &r, &aliases);
        match slot {
            CommandLineSlot::Arg { replace_start, .. } => {
                assert_eq!(replace_start, 17);
            }
            other => panic!("expected Arg, got {other:?}"),
        }
    }

    #[test]
    fn cursor_on_trailing_whitespace_after_command_is_next_arg_with_empty_prefix() {
        let r = fixture();
        let line = "describe-command ";
        let slot = current_slot(line, line.len(), &r, &aliases);
        match slot {
            CommandLineSlot::Arg {
                arg_index, prefix, ..
            } => {
                assert_eq!(arg_index, 0);
                assert_eq!(prefix, "");
            }
            other => panic!("expected Arg with empty prefix, got {other:?}"),
        }
    }

    #[test]
    fn cursor_past_schema_args_is_beyond_schema() {
        let r = fixture();
        // ex:quit has no args. Trailing text is beyond schema.
        let line = "quit foo";
        let slot = current_slot(line, line.len(), &r, &aliases);
        assert!(matches!(slot, CommandLineSlot::BeyondSchema { .. }));
    }

    #[test]
    fn substitute_form_is_delimiter_body() {
        let r = fixture();
        let line = "s/foo/bar/";
        let slot = current_slot(line, line.len(), &r, &aliases);
        match slot {
            CommandLineSlot::DelimiterBody { command_name, body } => {
                assert_eq!(command_name, "ex:substitute");
                assert_eq!(body, "foo/bar/");
            }
            other => panic!("expected DelimiterBody, got {other:?}"),
        }
    }

    #[test]
    fn percent_substitute_form_is_delimiter_body() {
        let r = fixture();
        let line = "%s/foo/bar/g";
        let slot = current_slot(line, line.len(), &r, &aliases);
        match slot {
            CommandLineSlot::DelimiterBody { command_name, .. } => {
                assert_eq!(command_name, "ex:substitute");
            }
            other => panic!("expected DelimiterBody, got {other:?}"),
        }
    }

    #[test]
    fn global_form_is_delimiter_body() {
        let r = fixture();
        let line = "g/pattern/d";
        let slot = current_slot(line, line.len(), &r, &aliases);
        match slot {
            CommandLineSlot::DelimiterBody { command_name, .. } => {
                assert_eq!(command_name, "ex:global");
            }
            other => panic!("expected DelimiterBody, got {other:?}"),
        }
    }

    #[test]
    fn vglobal_form_is_delimiter_body() {
        let r = fixture();
        let line = "v/pattern/d";
        let slot = current_slot(line, line.len(), &r, &aliases);
        assert!(matches!(slot, CommandLineSlot::DelimiterBody { .. }));
    }

    #[test]
    fn nested_path_arg_keeps_full_prefix_across_slash() {
        // Repro for `:e crates/latt<Tab>` -- the slot detector
        // must return prefix = "crates/latt" (NOT just "latt")
        // so `gen:files` sees the directory part it needs to
        // know which directory to read.
        let r = fixture();
        let line = "e crates/latt";
        let slot = current_slot(line, line.len(), &r, &aliases);
        match slot {
            CommandLineSlot::Arg {
                prefix,
                command_name,
                ..
            } => {
                assert_eq!(command_name, "ex:edit");
                assert_eq!(prefix, "crates/latt", "prefix should retain dir part");
            }
            other => panic!("expected Arg(ex:edit, prefix=crates/latt), got {other:?}"),
        }
    }

    #[test]
    fn cursor_in_middle_of_arg_truncates_prefix_at_cursor() {
        let r = fixture();
        // "describe-command motion" with cursor at byte 21 (just
        // after "moti"). Prefix should be "moti".
        let line = "describe-command motion";
        let slot = current_slot(line, 21, &r, &aliases);
        match slot {
            CommandLineSlot::Arg { prefix, .. } => assert_eq!(prefix, "moti"),
            other => panic!("expected Arg, got {other:?}"),
        }
    }

    #[test]
    fn bang_after_command_does_not_break_resolution() {
        let r = fixture();
        let line = "q!";
        let slot = current_slot(line, line.len(), &r, &aliases);
        // q! with no args is still command name slot (cursor
        // hasn't moved past the bang).
        assert!(matches!(slot, CommandLineSlot::CommandName { .. }));
    }

    #[test]
    fn slot_prefix_helper_returns_correct_text_for_each_variant() {
        assert_eq!(
            CommandLineSlot::CommandName {
                prefix: "abc".into(),
                replace_start: 0,
            }
            .prefix(),
            "abc"
        );
        assert_eq!(CommandLineSlot::Empty.prefix(), "");
        assert_eq!(
            CommandLineSlot::BeyondSchema {
                command_name: "x".into()
            }
            .prefix(),
            ""
        );
    }
}
