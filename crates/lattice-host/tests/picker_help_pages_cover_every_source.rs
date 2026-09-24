//! PH.5 — every builtin picker has a help page, and the page names the keys
//! its source enables.
//!
//! Design: `docs/dev/architecture/picker.md` §4.2quinquies. Slice plan:
//! `slice-plans/picker-help.md` PH.5.
//!
//! The pages are prose, and prose drifts: a source gains `<C-d>` and its page
//! keeps saying the key does nothing, or a new source ships with no page and
//! `<C-h>` quietly opens the general one. This walks the REGISTRY — not a list
//! kept here — so a new source fails until it is documented, and it reads the
//! spec's own declarations, so a page cannot miss a key its source gained.
//!
//! Plugin sources are not in a booted editor's registry (plugins are opt-in),
//! and document themselves through their own help seam; the `project` plugin's
//! pages are pinned by `lattice-plugin-loader/tests/project_picker_help.rs`.

#![allow(clippy::unwrap_used, clippy::panic)]

use lattice_core::Document as CoreDocument;
use lattice_host::editor::Editor;

/// The page `<C-h>` opens for `id`: the declared topic, else `picker-<id>`.
/// Mirrors rungs 1–2 of `Editor::do_picker_help`; rung 3 (the general page)
/// is exactly what this test exists to rule out.
fn page_for(editor: &Editor, id: &str) -> Option<(String, String)> {
    let registry = editor.picker_registry.load();
    let spec = &registry.entry(id)?.spec;
    let topic = spec
        .help_topic
        .as_ref()
        .map(|t| t.to_string())
        .unwrap_or_else(|| format!("picker-{id}"));
    let help = editor.help_topics.load();
    let body = help.lookup(&topic)?.body.render();
    Some((topic, body))
}

/// The TABLE ROWS of the "Keys in this picker" section (or "these pickers",
/// for a shared page) — rows only, so a key named in the prose around the
/// table ("why these keys") cannot stand in for a missing row.
fn keys_section(body: &str) -> Option<String> {
    let start = body
        .find("## Keys in this picker")
        .or_else(|| body.find("## Keys in these pickers"))?;
    let rest = &body[start..];
    let end = rest[3..].find("\n## ").map_or(rest.len(), |i| i + 3);
    let rows: Vec<&str> = rest[..end]
        .lines()
        .filter(|l| l.trim_start().starts_with('|'))
        .collect();
    (!rows.is_empty()).then(|| rows.join("\n"))
}

#[test]
fn every_builtin_source_has_a_page_with_a_keys_table() {
    let editor = Editor::boot(CoreDocument::from_text("x\n"));
    let ids: Vec<String> = editor
        .picker_registry
        .load()
        .ids()
        .map(str::to_string)
        .collect();
    assert!(
        !ids.is_empty(),
        "precondition: the boot registry has sources"
    );

    let mut problems = Vec::new();
    for id in &ids {
        let Some((topic, body)) = page_for(&editor, id) else {
            problems.push(format!(
                "`{id}`: no page — add docs/user/picker-{id}.md (or declare a shared \
                 one with PickerSourceSpec::with_help_topic)"
            ));
            continue;
        };
        let Some(keys) = keys_section(&body) else {
            problems.push(format!(
                "`{id}` ({topic}): no \"## Keys in this picker\" section"
            ));
            continue;
        };
        if !keys.contains("`<C-h>`") {
            problems.push(format!(
                "`{id}` ({topic}): keys table does not list `<C-h>`"
            ));
        }
    }
    assert!(problems.is_empty(), "\n  {}", problems.join("\n  "));
}

/// The spec's own declarations, against the page's keys table. Each check is
/// one way a page can go stale while the rest of it still reads fine.
#[test]
fn every_page_names_the_keys_its_source_enables() {
    let editor = Editor::boot(CoreDocument::from_text("x\n"));
    let registry = editor.picker_registry.load();
    let mut problems = Vec::new();

    for (id, spec) in registry.iter() {
        let Some((topic, body)) = page_for(&editor, id) else {
            continue; // reported by the test above
        };
        let Some(keys) = keys_section(&body) else {
            continue;
        };
        let mut need = |key: &str, why: &str| {
            if !keys.contains(key) {
                problems.push(format!(
                    "`{id}` ({topic}): {why}, but its keys table never mentions {key}"
                ));
            }
        };

        if spec.delete_command.is_some() {
            need("`<C-d>`", "declares a delete command");
        }
        // Depth is `ascend` answering at all — the same question
        // `do_picker_ascend_or_delete_word` asks.
        let has_depth = registry
            .entry(id)
            .and_then(|e| e.generator.as_ref())
            .is_some_and(|g| g.ascend("/a/b/").is_some());
        if has_depth {
            need("`<C-l>`", "has depth (descend)");
            need("`<C-w>`", "has depth (ascend)");
            need("`<Tab>`", "has depth (`<Tab>` drills in)");
        }
        if spec.create_label.is_some() {
            // The create row's label is the source's own words; the page must
            // at least say a row offers to create what was typed.
            if !body.contains("create") && !body.contains("remember") {
                problems.push(format!(
                    "`{id}` ({topic}): offers a create row, but the page never describes it"
                ));
            }
        }
    }
    assert!(problems.is_empty(), "\n  {}", problems.join("\n  "));
}

/// Pickers seated WITHOUT a registry id (LSP result lists, `:lsp-log`,
/// `:ai-log`, a server's question, `:b`) are reached through
/// `PickerSource::help_topic`, which names a page nothing else checks.
#[test]
fn every_id_less_picker_names_a_page_that_exists() {
    use lattice_picker::PickerSource;
    let editor = Editor::boot(CoreDocument::from_text("x\n"));
    let help = editor.help_topics.load();
    for source in [
        PickerSource::Buffers,
        PickerSource::LspLocations,
        PickerSource::LspInstances { prefilter: None },
        PickerSource::AiSessions { prefilter: None },
        PickerSource::LspShowMessageRequest {
            request_id: 0,
            server_id: String::new(),
        },
    ] {
        let topic = source
            .help_topic()
            .unwrap_or_else(|| panic!("{source:?} names no page"));
        let body = help
            .lookup(topic)
            .unwrap_or_else(|| panic!("{source:?} names `{topic}`, which is not a registered page"))
            .body
            .render();
        assert!(
            keys_section(&body).is_some(),
            "`{topic}` has no \"## Keys in this picker\" section"
        );
    }
}
