//! `treesitter-context` — sticky scope headers, as a bundled plugin.
//!
//! Three seams from one component:
//!
//!   - **context** — the scope producer. Runs a per-language `@context` query
//!     against the call-scoped tree snapshot the host hands it and returns
//!     structural scopes. Never resolves *which* scopes a pane shows: that is
//!     a function of the cursor and viewport, and the host does it natively so
//!     no WASM call sits on the scroll path.
//!   - **config** — the ten `context.*` options.
//!   - **theme** — the four `context.*` elements.
//!
//! ## Why the header span comes from the `body` field
//!
//! A scope's header is everything before its body: `fn f(\n  a: u32,\n) {` is
//! three lines of header, not one. Rather than a second `@context.end` capture
//! (which every query would have to get right independently), the header is
//! derived from the node's `body` field — present on every construct that has
//! one, and absent exactly where the header IS the whole node. One rule, no
//! per-language bookkeeping, and `context.multiline-threshold` caps how much of
//! it a scope may actually spend.

wit_bindgen::generate!({
    world: "treesitter-context-plugin",
    path: "../../wit",
});

use exports::lattice::plugin_host::context::Guest as ContextGuest;
use lattice::plugin_host::config::{OptionType, register_option};
use lattice::plugin_host::theme::{ColorRef, ModifierSet, StyleSpec, register_element};
use lattice::plugin_host::tree_sitter::{Node, TreeSnapshot};
use lattice::plugin_host::types::{ContextRequest, ContextScope};

struct Component;

// ── Queries ──────────────────────────────────────────────────────────────────

/// The `@context` query for a grammar id, or `None` when the language has none.
///
/// A missing query is a NORMAL state, not a defect: most languages will not
/// have one, and the honest response is an empty scope set (no strip) rather
/// than an error that would blank a strip the user was reading.
fn query_for(language: &str) -> Option<&'static str> {
    Some(match language {
        "rust" => include_str!("../queries/rust.scm"),
        "python" => include_str!("../queries/python.scm"),
        "go" => include_str!("../queries/go.scm"),
        "javascript" => include_str!("../queries/javascript.scm"),
        "typescript" | "tsx" => include_str!("../queries/typescript.scm"),
        "c" | "cpp" => include_str!("../queries/c.scm"),
        "markdown" => include_str!("../queries/markdown.scm"),
        _ => return None,
    })
}

/// Derive a scope from a captured node.
///
/// `scope_start ..= scope_end` is the node's own line span. The header runs
/// from the node's first line to the line its `body` begins on — so a wrapped
/// signature yields a multi-line header and a bodyless construct yields a
/// single-line one, with no per-language special casing.
fn scope_from(node: &Node) -> ContextScope {
    let range = node.byte_range();
    let scope_start = range.start.line;
    let scope_end = range.end.line;
    // `body` is the near-universal field name for the block a construct opens.
    // Absent (a `struct` without one, a match arm) means the header is the
    // node's first line and nothing more.
    let header_end = node
        .child_by_field("body")
        .map(|body| body.byte_range().start.line)
        .unwrap_or(scope_start)
        // A body that starts before the node does is impossible from a real
        // tree, but clamping costs nothing and keeps a malformed grammar from
        // producing an inverted span the host would have to defend against.
        .max(scope_start);
    ContextScope {
        scope_start,
        scope_end,
        header_start: scope_start,
        header_end,
    }
}

fn scopes_from_tree(tree: &TreeSnapshot) -> Result<Vec<ContextScope>, String> {
    let language = tree.language();
    let Some(source) = query_for(&language) else {
        // No query for this grammar. Not an error — the strip simply has
        // nothing to show, and the host caches that as "no scopes".
        return Ok(Vec::new());
    };
    // Compiled per call rather than cached: the guest has no per-language
    // cache slot that survives a call, and this runs once per REPARSE (not per
    // keystroke, scroll, or frame), so the cost sits far off every hot path.
    // A cache would be the right move only if the producer were re-driven more
    // often, and the whole scopes-not-rows split exists to ensure it is not.
    let query = tree.compile_query(source)?;
    let mut scopes: Vec<ContextScope> = tree
        .run_query(&query, None)
        .into_iter()
        .filter(|c| c.name == "context")
        .map(|c| scope_from(&c.node))
        .collect();
    // A scope spanning a single line can never be a context: its header cannot
    // scroll away while the cursor is still inside it. Dropping them here keeps
    // the host's cache (and the resolver's scan) free of entries that can never
    // resolve to anything.
    scopes.retain(|s| s.scope_end > s.scope_start);
    Ok(scopes)
}

impl ContextGuest for Component {
    fn context_scopes(
        req: ContextRequest,
        tree: Option<&TreeSnapshot>,
    ) -> Result<Vec<ContextScope>, String> {
        if req.line_count == 0 {
            return Ok(Vec::new());
        }
        // No parse (plain text, or one still pending) is a normal state the
        // host caches as "no scopes" — never an error, which would make it keep
        // the previous buffer's structure.
        let Some(tree) = tree else {
            return Ok(Vec::new());
        };
        scopes_from_tree(tree)
    }
}

// ── Config ───────────────────────────────────────────────────────────────────

/// Names are registered SHORT and the host namespaces them by plugin id, so
/// `max-lines` becomes `treesitter-context.max-lines`.
impl Guest for Component {
    fn register_options() {
        let opts: &[(&str, OptionType, &str, &str)] = &[
            (
                "enabled",
                OptionType::Boolean,
                "true",
                "Show sticky scope headers above the text.",
            ),
            (
                "anchor",
                OptionType::String,
                "cursor",
                "Which line drives the context: `cursor` (where you are) or \
                 `topline` (what you are looking at).",
            ),
            (
                "max-lines",
                OptionType::Integer,
                "3",
                "Maximum context rows. Counts ROWS, so a wrapped signature \
                 spends more than one.",
            ),
            (
                "trim-scope",
                OptionType::String,
                "outer",
                "Which end to drop when over budget: `outer` keeps the scopes \
                 you are innermost in, `inner` keeps the outermost.",
            ),
            (
                "multiline-threshold",
                OptionType::Integer,
                "1",
                "Maximum rows one scope's header may use. Raise it to see a \
                 whole wrapped signature.",
            ),
            (
                "max-viewport-fraction",
                OptionType::Integer,
                "33",
                "Percent of the pane the whole sticky strip may occupy, \
                 headerline included.",
            ),
            (
                "separator",
                OptionType::String,
                "",
                "Glyph repeated as a rule under the context block. Empty \
                 disables it.",
            ),
            (
                "line-numbers",
                OptionType::Boolean,
                "true",
                "Show each context row's source line number in the gutter.",
            ),
            (
                "disabled-languages",
                OptionType::String,
                "",
                "Comma-separated grammar ids to skip (e.g. `markdown,yaml`).",
            ),
            (
                "max-file-lines",
                OptionType::Integer,
                "100000",
                "Skip the structural query above this line count; the feature \
                 turns itself off rather than stalling on generated files.",
            ),
        ];
        for (name, ty, default, doc) in opts {
            register_option(name, *ty, default, doc);
        }
    }

    // ── Theme ────────────────────────────────────────────────────────────────

    /// Four elements, and none of them a foreground for code.
    ///
    /// The context rows carry the source lines' OWN syntax highlighting — that
    /// is the point of building them from the same cell builder as the document
    /// — so these compose the backdrop and the gutter around that. An element
    /// that recoloured the code in the strip would be overriding syntax
    /// highlighting from a place nobody would think to look.
    fn register_theme_elements() {
        let plain = ModifierSet {
            bold: None,
            italic: None,
            underline: None,
            dim: None,
            reverse: None,
        };
        let _ = register_element(
            "background",
            "Sticky context strip: the row backdrop.",
            &StyleSpec {
                inherit: None,
                fg: None,
                // A palette KEY, not a literal: the strip recolours when the
                // user swaps colourscheme, which a baked colour could not.
                bg: Some(ColorRef::Palette("surface0".to_string())),
                modifiers: plain,
                scale: None,
            },
        );
        let _ = register_element(
            "separator",
            "Sticky context strip: the rule beneath it, when `separator` is set.",
            &StyleSpec {
                inherit: None,
                fg: Some(ColorRef::Palette("overlay".to_string())),
                bg: None,
                modifiers: plain,
                scale: None,
            },
        );
        let _ = register_element(
            "line-number",
            "Sticky context strip: source line numbers in the gutter.",
            &StyleSpec {
                inherit: None,
                fg: Some(ColorRef::Palette("overlay".to_string())),
                bg: None,
                modifiers: plain,
                scale: None,
            },
        );
        let _ = register_element(
            "active",
            "Sticky context strip: the innermost row — the scope you are in.",
            &StyleSpec {
                inherit: Some("treesitter-context.background".to_string()),
                fg: None,
                bg: None,
                modifiers: ModifierSet {
                    bold: Some(true),
                    ..plain
                },
                scale: None,
            },
        );
    }
}

export!(Component);
