//! Every string-typed command argument offers completion, or says why not.
//!
//! The grammar IS the public command API (paramount goal #3), and an argument
//! you cannot discover is API you cannot use. `:describe-plugin <Tab>` and
//! `:describe-plugin-api <Tab>` shipped blind for exactly as long as it took
//! someone to try them — both carried a `// completion is a follow-up` comment
//! that nobody grepped after the thing it waited on (the Phase-8 loader)
//! landed, and `lattice-plugin-loader`'s `string_arg` carried a third.
//!
//! A convention that has already failed three times as a comment is not a
//! convention, so it is a test. Adding a command with a `String` argument now
//! means wiring a generator or writing down, here, why the argument has no
//! candidate set.

use lattice_core::Document as CoreDocument;
use lattice_grammar::args::ArgKind;
use lattice_host::editor::Editor;

/// `(command, arg)` pairs whose argument genuinely has no enumerable domain.
///
/// The reason is the point of the entry. "It is hard" is not a reason; "a
/// regex has no candidate set" is. If a domain later becomes enumerable —
/// shell history for `:terminal`, say — delete the row and wire a generator.
const NO_CANDIDATE_SET: &[(&str, &str, &str)] = &[
    (
        "ex:substitute",
        "flags",
        "a substitute flag string is typed as one token, not picked",
    ),
    (
        "ex:global",
        "inverted",
        "internal bool distinguishing `:g` from `:v`; never typed",
    ),
    (
        "ex:apropos",
        "pattern",
        "a free-form search pattern is the question, not an answer",
    ),
    (
        "ex:describe-key",
        "chord",
        "ArgKind::Chord puts the cmdline into chord-capture; keys are pressed, not completed",
    ),
    (
        "ex:terminal",
        "cmd",
        "an arbitrary shell command line; no enumerable domain",
    ),
    ("ex:tabterminal", "cmd", "as `:terminal`"),
    ("ex:hover", "markdown", "the popup body is authored text"),
    (
        "ex:lsp-rename",
        "new-name",
        "the new identifier does not exist yet — that is the point",
    ),
    (
        "ex:lsp-workspace-symbol",
        "query",
        "a server-side substring filter, resolved remotely",
    ),
    (
        "ex:tutor",
        "lesson",
        "a lesson number; ArgKind::Int, bounded by the tutor itself",
    ),
    (
        "compile",
        "command",
        "an arbitrary shell command line; no enumerable domain",
    ),
    ("make", "command", "as `compile` — a build command override"),
    (
        "search",
        "query",
        "a free-form search query is the question, not an answer",
    ),
    (
        "diffput",
        "bufnr",
        "a buffer number. Enumerable in principle, but there is no `gen:buffers` \
         yet and `:ls` is how you learn one today. Wire a generator and delete \
         this row.",
    ),
    ("diffget", "bufnr", "as `diffput`"),
    // Both found by this test on its first run, which is the point of it.
    (
        "ex:substitute",
        "replacement",
        "the replacement text is authored, not chosen",
    ),
    (
        "ex:global",
        "body",
        "a nested ex-command line. It HAS a candidate set (`gen:commands`), but \
         completing it needs the cmdline to re-enter command completion at a \
         sub-offset inside the argument, which the pipeline does not do. Wire a \
         generator when it can, and delete this row.",
    ),
];

fn editor() -> Editor {
    // Hermetic: no developer `~/.config/lattice` plugins contributing schemas.
    lattice_plugin_loader::disable_autoload();
    Editor::boot(CoreDocument::from_text("fn main() {}\n"))
}

#[test]
fn every_string_argument_completes_or_is_excused() {
    let ed = editor();
    let reg = ed.registry.load();

    let mut blind: Vec<String> = Vec::new();
    let mut magit_blind = 0usize;
    for name in reg.names() {
        let Some(spec) = reg.lookup_by_name(name) else {
            continue;
        };
        for arg in &spec.args_schema {
            // Only text the user types free-hand can be completed. `Bool`,
            // `Char` and `Body` are shaped by the parser, not by a candidate
            // list.
            if !matches!(arg.kind, ArgKind::String | ArgKind::Raw) {
                continue;
            }
            if arg.completion.is_some() || arg.picker.is_some() {
                continue;
            }
            if NO_CANDIDATE_SET
                .iter()
                .any(|(c, a, _)| *c == name && *a == arg.name.as_ref())
            {
                continue;
            }
            // Magit is excluded as a BLOCK, not row by row, and counted rather
            // than blessed. Its convention is "omit to pick one": every one of
            // these commands opens a source-backed picker when called with no
            // argument, so the value is discoverable — what is missing is `<Tab>`
            // and an `ArgSpec::picker`, which is a mapping job across
            // `lattice-magit` (commit / stash / branch / file → four or five
            // existing picker sources) rather than a missing mechanism.
            //
            // Eighty hand-written excuses would be the blanket
            // `the_allowlist_has_no_stale_entries` exists to prevent, so the
            // count is pinned instead: a NEW magit command with a blind argument
            // still fails this test.
            if name.starts_with("magit-") || name.starts_with("action:magit-") {
                magit_blind += 1;
                continue;
            }
            blind.push(format!("  {name} <{}>  — {}", arg.name, arg.doc));
        }
    }

    assert!(
        blind.is_empty(),
        "these command arguments offer no completion and no picker.\n\
         Wire a generator (see `crates/lattice-host/src/host_generators.rs`, whose \
         header documents the two steps), or add the pair to NO_CANDIDATE_SET in \
         this file WITH a reason:\n{}",
        blind.join("\n")
    );

    // See the block comment above. Lower this number when magit arguments get
    // pickers; never raise it.
    const MAGIT_BLIND: usize = 80;
    assert!(
        magit_blind <= MAGIT_BLIND,
        "magit grew {} argument(s) with no completion and no picker (was {MAGIT_BLIND}). \
         Set `picker:` on the new ArgSpec — commit / stash / branch / file args each \
         have a picker source in `lattice-magit/src/picker_sources.rs` already.",
        magit_blind - MAGIT_BLIND
    );
    assert_eq!(
        magit_blind, MAGIT_BLIND,
        "magit blind-argument count dropped to {magit_blind} — lower MAGIT_BLIND to \
         match, so the ratchet keeps its new floor"
    );
}

/// The allowlist must not outlive what it excuses. An entry naming a command
/// or argument that no longer exists is a stale excuse, and a stale excuse is
/// how an allowlist quietly becomes a blanket.
#[test]
fn the_allowlist_has_no_stale_entries() {
    let ed = editor();
    let reg = ed.registry.load();

    let mut stale: Vec<String> = Vec::new();
    for (cmd, arg, _why) in NO_CANDIDATE_SET {
        match reg.lookup_by_name(cmd) {
            None => stale.push(format!("  {cmd} — no such command")),
            Some(spec) => {
                let found = spec.args_schema.iter().find(|a| a.name.as_ref() == *arg);
                match found {
                    None => stale.push(format!("  {cmd} <{arg}> — no such argument")),
                    Some(a) if a.completion.is_some() || a.picker.is_some() => stale.push(format!(
                        "  {cmd} <{arg}> — now HAS completion; drop the excuse"
                    )),
                    Some(_) => {}
                }
            }
        }
    }

    assert!(
        stale.is_empty(),
        "NO_CANDIDATE_SET entries that no longer describe reality:\n{}",
        stale.join("\n")
    );
}

/// The four arguments this rule was written for, pinned by name so a later
/// refactor cannot quietly drop one back to `None`.
#[test]
fn the_plugin_and_theme_arguments_name_their_generators() {
    let ed = editor();
    let reg = ed.registry.load();

    for (cmd, arg, generator) in [
        ("ex:describe-plugin", "name", "gen:plugins"),
        // No `ex:` prefix — `lattice-plugin-loader` registers these under bare
        // names while `lattice-grammar` prefixes its own. Asserted verbatim so
        // this test fails if either convention shifts.
        ("plugin-unload", "target", "gen:plugins"),
        ("plugin-reload", "target", "gen:plugins"),
        ("plugin-update", "target", "gen:plugins"),
        ("plugin-load", "path", "gen:files"),
        ("ex:describe-plugin-api", "seam", "gen:plugin-api-seams"),
        ("ex:export-plugin-api", "format", "gen:plugin-api-formats"),
        ("ex:colorscheme", "name", "gen:themes"),
    ] {
        let spec = reg
            .lookup_by_name(cmd)
            .unwrap_or_else(|| panic!("`{cmd}` must be registered"));
        let a = spec
            .args_schema
            .iter()
            .find(|a| a.name.as_ref() == arg)
            .unwrap_or_else(|| panic!("`{cmd}` must take an arg named `{arg}`"));
        assert_eq!(
            a.completion.as_deref(),
            Some(generator),
            "`{cmd} <{arg}>` must complete through `{generator}`"
        );
    }
}
