//! PC.6 — `project-switch-commands`: what you can do to a project once you have
//! chosen one.
//!
//! Design: `docs/dev/architecture/project-commands.md` §7.
//!
//! ## The extension contract is one sentence
//!
//! **A project command is an ex-command whose first argument is a project root.**
//! The menu invokes `:<command> <root>`; anything registered under that
//! convention can be a row, so a third-party plugin joins by shipping a command
//! and the user (or that plugin's own default config) adds a line.
//!
//! Extensibility is config-driven and NOT a contributable registry. That pattern
//! exists (`contributable-registries.md`) and fits its two users because the
//! owner is a host crate; here the owner is a plugin, so plugin→plugin
//! contribution would need the host to broker between two guests — and the host
//! would have to learn what a project command *is*, which `project.wit` refuses
//! in as many words.
//!
//! ## Why the rows are records rather than a string blob
//!
//! `org-capture.md` §2 argued that "no option can hold a record" and fell back
//! to a TOML document inside a string option. That was true when it was written
//! and is not now: TC.4/TC.5 landed `register-structured-option`, so this
//! declares a real `list<record>` and `:describe-option` shows a schema instead
//! of a blob.
//!
//! The schema arena is built by hand here rather than through
//! `lattice-plugin-sdk`'s `#[derive(ConfigShape)]`. One option does not justify
//! pulling the SDK into a bundled guest whose entire dependency list is
//! `wit-bindgen`; the derive generates exactly the three nodes below.

use crate::lattice::plugin_host::config::{
    ConfigSchema, ConfigValue, OptionType, SchemaField, SchemaNode, ValueNode,
};

/// The option name, namespaced to the plugin by the host.
pub const OPTION: &str = "switch-commands";

/// One row of the menu.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SwitchCommand {
    pub key: String,
    pub label: String,
    pub command: String,
}

/// The rows a user gets without configuring anything.
///
/// Keys follow `project.el`'s own map so the muscle memory transfers: `f`
/// find-file, `d` dired, `g` find-regexp, `s` shell, `v` vc-dir.
///
/// **`v` names `magit-status`, not a wrapper**, and that is the extension
/// contract demonstrating itself. A wrapper existed in the plan only because
/// `:magit-status` ignored its arguments; PC.3 gave it an optional path, so it
/// now IS "an ex-command whose first argument is a project root" and needs
/// nothing from this plugin. Magit keeps owning what a status buffer is, which
/// repository it acts on, and how two checkouts sharing a basename are told
/// apart — including its own "Not a git repository." for a project that is not
/// one, rather than a second opinion from here.
///
/// A row naming a command that is not registered (magit absent) renders with
/// the reason rather than vanishing: a missing row is invisible, a labelled one
/// tells you what did not load.
pub fn defaults() -> Vec<SwitchCommand> {
    [
        ("f", "Find file", "project-find-file"),
        // PB.1: `b`, `project.el`'s letter, in `project.el`'s position —
        // second, right after find-file. Here rather than only on `<leader>pb`
        // because the menu is what you get after CHOOSING a project, and
        // "which of that project's buffers are already open" is a question you
        // ask exactly then.
        ("b", "Buffers", "project-buffers"),
        ("d", "Browse tree", "project-dired"),
        ("g", "Find regexp", "project-grep"),
        ("s", "Shell", "project-shell"),
        // `m` for Magit, not project.el's `v`. `v` is emacs shorthand for
        // `project-vc-dir` — version control, the generic verb — and magit
        // rebinds it there because it is REPLACING that command. Nothing here
        // is replacing anything: the row names magit outright, and `m` is what
        // it reads as. The letter is worth more as a mnemonic than as a
        // transplant.
        ("m", "Magit", "magit-status"),
    ]
    .into_iter()
    .map(|(key, label, command)| SwitchCommand {
        key: key.to_string(),
        label: label.to_string(),
        command: command.to_string(),
    })
    .collect()
}

/// The declared shape: `list<{ key, label, command }>`.
///
/// An arena with child links as indices, which is how WIT carries a recursive
/// type. Node 0 is the scalar every field shares, node 1 the record, node 2 the
/// list — and `root` is stated explicitly rather than assumed to be 0.
pub fn schema() -> ConfigSchema {
    let field = |name: &str, doc: &str| SchemaField {
        name: name.to_string(),
        schema: 0,
        required: true,
        doc: doc.to_string(),
    };
    ConfigSchema {
        nodes: vec![
            SchemaNode::Scalar(OptionType::String),
            SchemaNode::Record(vec![
                field("key", "the key that fires this row"),
                field("label", "what the menu shows"),
                field(
                    "command",
                    "an ex-command taking a project root as its first argument",
                ),
            ]),
            SchemaNode::ListOf(1),
        ],
        root: 2,
    }
}

/// Encode rows as a `config-value` arena — the mirror of [`schema`].
pub fn to_value(rows: &[SwitchCommand]) -> ConfigValue {
    let mut nodes: Vec<ValueNode> = Vec::new();
    let mut record_indices = Vec::new();
    for row in rows {
        let key = push(&mut nodes, ValueNode::String(row.key.clone()));
        let label = push(&mut nodes, ValueNode::String(row.label.clone()));
        let command = push(&mut nodes, ValueNode::String(row.command.clone()));
        record_indices.push(push(
            &mut nodes,
            ValueNode::Record(vec![
                ("key".to_string(), key),
                ("label".to_string(), label),
                ("command".to_string(), command),
            ]),
        ));
    }
    let root = push(&mut nodes, ValueNode::List(record_indices));
    ConfigValue { nodes, root }
}

fn push(nodes: &mut Vec<ValueNode>, node: ValueNode) -> u32 {
    nodes.push(node);
    (nodes.len() - 1) as u32
}

/// Decode a `config-value` arena back into rows.
///
/// **A malformed value falls back to [`defaults`] rather than to an empty
/// menu.** The host validates against the schema before storing, so this should
/// not fire — but a menu with no rows is indistinguishable from a broken chord,
/// and that is the one outcome worth spending a fallback on.
pub fn from_value(value: &ConfigValue) -> Vec<SwitchCommand> {
    let rows = decode(value).unwrap_or_default();
    if rows.is_empty() {
        defaults()
    } else {
        rows
    }
}

fn decode(value: &ConfigValue) -> Option<Vec<SwitchCommand>> {
    let ValueNode::List(items) = value.nodes.get(value.root as usize)? else {
        return None;
    };
    let mut out = Vec::with_capacity(items.len());
    for &idx in items {
        let ValueNode::Record(fields) = value.nodes.get(idx as usize)? else {
            return None;
        };
        let field = |name: &str| -> Option<String> {
            let (_, at) = fields.iter().find(|(n, _)| n == name)?;
            match value.nodes.get(*at as usize)? {
                ValueNode::String(s) => Some(s.clone()),
                _ => None,
            }
        };
        let row = SwitchCommand {
            key: field("key")?,
            label: field("label")?,
            command: field("command")?,
        };
        // A row with no key can never fire and a row with no command has
        // nothing to fire — both are dropped rather than shown as dead menu
        // entries the user would press and get nothing from.
        if !row.key.is_empty() && !row.command.is_empty() {
            out.push(row);
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_value_round_trips_through_the_arena() {
        let rows = defaults();
        assert_eq!(from_value(&to_value(&rows)), rows);
    }

    #[test]
    fn an_empty_value_falls_back_to_the_defaults() {
        let empty = ConfigValue {
            nodes: vec![ValueNode::List(Vec::new())],
            root: 0,
        };
        assert_eq!(from_value(&empty), defaults());
    }

    /// A menu with no rows is indistinguishable from a broken chord, so a
    /// malformed value must not produce one.
    #[test]
    fn a_malformed_value_falls_back_rather_than_emptying_the_menu() {
        let bogus = ConfigValue {
            nodes: vec![ValueNode::String("not a list".to_string())],
            root: 0,
        };
        assert_eq!(from_value(&bogus), defaults());
    }

    #[test]
    fn a_row_that_could_never_fire_is_dropped() {
        let rows = vec![
            SwitchCommand {
                key: String::new(),
                label: "no key".to_string(),
                command: "project-find-file".to_string(),
            },
            SwitchCommand {
                key: "f".to_string(),
                label: "fine".to_string(),
                command: "project-find-file".to_string(),
            },
        ];
        let decoded = from_value(&to_value(&rows));
        assert_eq!(decoded.len(), 1);
        assert_eq!(decoded[0].key, "f");
    }

    /// The default keys are project.el's, because that muscle memory is the
    /// thing being imported — the ORDER too, which is why this is a sequence
    /// comparison and not a set one. `b` sits second, where `project.el` puts
    /// `project-switch-to-buffer`.
    #[test]
    fn the_default_keys_match_project_el() {
        let keys: Vec<String> = defaults().into_iter().map(|r| r.key).collect();
        assert_eq!(keys, vec!["f", "b", "d", "g", "s", "m"]);
    }

    /// **PK.1: every menu row is also a direct chord, and the letter is the
    /// same one.**
    ///
    /// §6's two entry points are the same verbs reached two ways — `<C-x>pf`
    /// acts on the project you are in, and `<C-x>pp` picks a project first and
    /// then offers the identical list. A row with no chord breaks that
    /// symmetry silently: the verb exists, works, and is reachable only after
    /// choosing a project you are already standing in. `g`, `s` and `v` were
    /// in exactly that state until PK.1.
    ///
    /// Pinned HERE rather than only in the keymap test because this is the
    /// list that grows — a new row is added to `defaults` and the chord is the
    /// thing that gets forgotten.
    #[test]
    fn every_menu_row_has_a_chord() {
        // Mirrors `register_modes`' `verbs`, minus `p` — the picker itself is
        // a chord with no menu row, which is right: it is what OPENS the menu,
        // and a row for it would be the menu offering itself.
        let chorded = ["f", "b", "d", "g", "s", "m"];
        for row in defaults() {
            assert!(
                chorded.contains(&row.key.as_str()),
                "the `{}` row ({}) has no `<leader>p` / `<C-x>p` chord — add it \
                 to `register_modes`' verbs, or this verb is reachable only \
                 after choosing a project you are already in",
                row.key,
                row.command
            );
        }
    }

    /// The magit row names magit's OWN command, not a wrapper — PC.3 made
    /// `:magit-status <path>` satisfy the extension contract, so there is
    /// nothing left for this plugin to add. Pinned because reintroducing a
    /// `project-magit` wrapper would look tidy and would only re-describe what
    /// magit already does.
    #[test]
    fn the_magit_row_names_magits_own_command() {
        // `m`, not project.el's `v`: `v` is emacs shorthand for
        // `project-vc-dir` and magit takes that key there because it REPLACES
        // that command. This row replaces nothing — it names magit outright,
        // so it reads as `m`.
        let row = defaults().into_iter().find(|r| r.key == "m").unwrap();
        assert_eq!(row.command, "magit-status");
    }
}
