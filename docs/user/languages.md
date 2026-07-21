---
summary: "Bundled languages, coverage roadmap, and how to add a new language (tree-sitter or otherwise)."
related: [language, syntax]
---

# Languages

Lattice's syntax features (highlighting, folding, indent, navigation
by AST) are driven by [tree-sitter](https://tree-sitter.github.io/)
grammars plus per-language `*.scm` query files. This doc explains
how to add support for a new language -- both the common case
(language has a tree-sitter grammar) and the fallback case
(language doesn't, or you only want lightweight features).

## Quick reference

| Goal                                        | What to do                         |
|---------------------------------------------|------------------------------------|
| Add a tree-sitter language to **core**      | [Section 1](#1-tree-sitter-core)   |
| Add a language as a **plugin**              | [Section 2](#2-tree-sitter-plugin) |
| Add a language **without tree-sitter**      | [Section 3](#3-non-tree-sitter)    |
| Update queries when a grammar version bumps | [Section 4](#4-grammar-bumps)      |

## Currently bundled

The default `LangRegistry` ships with these tree-sitter grammars
plus their `highlights.scm` / `injections.scm` / `folds.scm` /
`symbols.scm` / `textobjects.scm` (and `locals.scm` for
JavaScript). *Symbols* feed the `gen:tree-sitter-symbol` completion
source and the outline picker; *text objects* feed narrow-mode and
the structural text objects (see
[modal-editing.md](modal-editing.md#tree-sitter-text-objects)):

| Language   | Extensions              | Highlights | Injections                     | Locals | Folds | Symbols | Text obj |
|------------|-------------------------|------------|--------------------------------|--------|-------|---------|----------|
| Rust       | `rs`                    | ✅         | ✅                             | —      | ✅    | ✅      | ✅       |
| Python     | `py`/`pyw`              | ✅         | —                              | —      | ✅    | ✅      | ✅       |
| JavaScript | `js`/`mjs`/`cjs`        | ✅         | ✅                             | ✅     | ✅    | ✅      | ✅       |
| TypeScript | `ts`/`mts`/`cts`        | ✅         | —                              | ✅     | ✅    | ✅      | ✅       |
| TSX        | `tsx`                   | ✅         | —                              | ✅     | ✅    | ✅      | ✅       |
| Go         | `go`                    | ✅         | —                              | —      | ✅    | ✅      | ✅       |
| C          | `c`/`h`                 | ✅         | —                              | —      | ✅    | ✅      | ✅       |
| C++        | `cpp`/`cc`/`cxx`/`hpp`/`hh`/`hxx` | ✅ | —                    | —      | ✅    | ✅      | ✅       |
| Java       | `java`                  | ✅         | —                              | —      | ✅    | ✅      | ✅       |
| Ruby       | `rb`/`ruby`             | ✅         | —                              | —      | ✅    | ✅      | ✅       |
| HTML       | `html`/`htm`/`xhtml`    | ✅         | ✅                             | —      | ✅    | ✅      | ✅       |
| CSS        | `css`                   | ✅         | —                              | —      | ✅    | ✅      | ✅       |
| JSON       | `json`                  | ✅         | —                              | —      | ✅    | ✅      | ✅       |
| YAML       | `yaml`/`yml`            | ✅         | —                              | —      | ✅    | ✅      | ✅       |
| TOML       | `toml`                  | ✅         | —                              | —      | ✅    | ✅      | ✅       |
| Bash       | `sh`/`bash`/`zsh`/`fish` | ✅        | —                              | —      | ✅    | ✅      | ✅       |
| SQL        | `sql`                   | ✅         | —                              | —      | ✅    | ✅      | ✅       |
| Lua        | `lua`                   | ✅         | —                              | —      | ✅    | ✅      | ✅       |
| Markdown   | `md`/`markdown`/`mdown`/`mkd` | ✅  | ✅ (block→inline, fenced code) | —      | ✅    | —       | —        |

Plain text and any unrecognised extension fall through to the
`Plain` language: no parse, no styled spans. The renderer treats
that as untyped buffer content.

## Coverage roadmap

The plan is a tiered rollout, not "Helix parity in one PR":

- **Tier 1 (in core, target by v1.0)**: 19 mainstream languages
  — Rust, Python, JavaScript, TypeScript, TSX, Go, C, C++, Java,
  Ruby, HTML, CSS, JSON, YAML, TOML, Bash, SQL, Lua, Markdown.
- **Tier 2+ (plugin-host era, post Phase 7)**: anything beyond
  tier 1 ships as a `lattice-language-<name>` WASM plugin that
  contributes the grammar + queries through the `lattice:syntax`
  WIT interface. No core release needed; users opt in by
  installing the plugin.

This split matches the §5.5 design intent: extension is the right
substrate for the long tail, core is the right place for the
high-traffic 20% that 80% of users edit.

---

## 1. Tree-sitter, core

The path of least surprise. Use this when:

- The language has a maintained `tree-sitter-<lang>` Rust crate.
- You want it bundled with every Lattice install.
- You're a Lattice maintainer (or sending a contribution that
  meets the bar to land in core).

### Step 1 -- pin the grammar crate

In the workspace `Cargo.toml`:

```toml
[workspace.dependencies]
tree-sitter-go = "0.23"
```

Pin a specific minor version. Grammar updates rename node kinds;
floating versions silently break query files.

In `crates/lattice-syntax/Cargo.toml`:

```toml
tree-sitter-go = { workspace = true }
```

### Step 2 -- add a `Lang` variant

`crates/lattice-syntax/src/lang.rs`:

```rust
pub enum Lang {
    Plain,
    Rust,
    Python,
    JavaScript,
    Markdown,
    Go,            // ← new
}

impl Lang {
    pub fn name(&self) -> &'static str {
        match self {
            ...
            Lang::Go => "go",
        }
    }

    pub fn detect_from_path(path: &Path) -> Self {
        match ext.as_str() {
            ...
            "go" => Lang::Go,
            _ => Lang::Plain,
        }
    }
}
```

### Step 3 -- register in the standard registry

`crates/lattice-syntax/src/registry.rs`:

```rust
configs.insert(
    "go",
    build_config(
        tree_sitter_go::LANGUAGE.into(),
        "go",
        tree_sitter_go::HIGHLIGHTS_QUERY,
        "",                          // injections.scm if any
        "",                          // locals.scm if any
        Some(GO_FOLDS_QUERY),        // folds.scm
        Some(GO_SYMBOLS_QUERY),      // symbols.scm   (None if not shipped)
        Some(GO_TEXTOBJECTS_QUERY),  // textobjects.scm (None if not shipped)
    )?,
);
```

`symbols.scm` and `textobjects.scm` are optional (`None` skips
them); a language with no `textobjects.scm` simply has no
tree-sitter text objects or narrow-mode targets.

If the crate doesn't expose `HIGHLIGHTS_QUERY` as a constant
(some don't), copy the upstream `queries/highlights.scm` into
`crates/lattice-syntax/queries/go/highlights.scm` and
`include_str!` it the same way `folds.scm` is loaded.

### Step 4 -- ship the query files

Lattice looks for query files under
`crates/lattice-syntax/queries/<name>/`. `folds.scm`, `symbols.scm`,
and `textobjects.scm` are loaded from there via `include_str!`;
highlights and injections come from the grammar crate's exposed
constants (a later migration moves those into the queries dir too,
for consistency).

A typical `folds.scm` (adapted from Helix's `runtime/queries/<lang>/folds.scm`):

```scheme
[
  (function_declaration)
  (method_declaration)
  (type_declaration)
  (struct_type)
  (interface_type)
  (block)
  (composite_literal)
] @fold
```

Verify each captured node type actually exists in the grammar
version you pinned -- `tree_sitter::Query::new` returns
`Query error at L:C. Invalid node type <name>` when a query
references a missing node. Drop those captures.

### Step 5 -- smoke test

Add a test under `crates/lattice-syntax/src/syntax.rs` proving
a sample buffer highlights + folds correctly:

```rust
#[test]
fn go_function_produces_a_fold() {
    let mut s = Syntax::for_language(Lang::Go).unwrap().unwrap();
    s.parse("func main() {\n    return\n}\n");
    let folds = compute_syntax_folds(&s).unwrap();
    assert!(folds.iter().any(|f| f.start_line == 0 && f.end_line >= 1));
}
```

Run `cargo test -p lattice-syntax`. If it passes, run
`cargo clippy --workspace --all-targets`.

### Step 6 -- update docs

- Add a row to the bundled-languages table at the top of this doc.
- Update `docs/dev/operations/implementation.md` if you advance a checklist item.
- The DESIGN.md §5.4 list of supported languages doesn't need a
  per-language update; the registry is the source of truth.

---

## 2. Tree-sitter, plugin

Once the §5.5 plugin host is built (Phase 7), this path becomes
the default for tier 2+ languages. **Currently planned, not yet
implemented**, but the API surface is settled enough to describe
here so contributors can target it ahead of time.

A language plugin is a WASM Component that exports the
`lattice:syntax/grammar@0.1` WIT interface. The interface lets
the plugin contribute:

- The compiled tree-sitter grammar (one parser dylib bundled into
  the WASM, or a host-side reference to a registered Language).
- `highlights.scm` / `injections.scm` / `locals.scm` /
  `folds.scm` / `indents.scm` / `textobjects.scm` as embedded
  string constants.
- The list of file extensions / shebangs / language IDs the
  plugin claims.

The host loads the plugin lazily: it's instantiated only when
the user opens a buffer that the plugin claims. Closed-source or
proprietary languages can ship as plugins without going through
core review.

A reference template is **planned** at
`https://github.com/<lattice-org>/lattice-language-template`.
Until that exists, follow Section 1 (core) and we'll migrate the
contribution to the plugin path once the host lands.

---

## 3. Non-tree-sitter languages

When the language doesn't have a tree-sitter grammar -- or the
grammar exists but is too immature -- you can still get a useful
editing experience:

### What you lose

- AST-driven highlighting (token-stream falls back to plain text).
- Tree-sitter folds, textobjects, indents.

### What you can still wire up

- **Indent folds**: every buffer gets `compute_indent_folds`
  for free as long as the file has consistent indentation.
  `:set foldmethod=indent` is the universal fallback.
- **`comments.scm` substitute**: there's a planned
  comment-style registry (single-line / block delimiters per
  language) so `gcc` / `gbc` can comment lines without parsing.
  Until that lands, plain `:s/^/\/\//` works.
- **External LSP for diagnostics + completion + hover** (Phase 4):
  any language with an LSP server (which is most of them) gets
  diagnostics, completion, hover, go-to-definition without a
  tree-sitter grammar at all. Set the LSP server in
  `init.rs` (Phase 7 config) -- no grammar needed.

### When tree-sitter is the right answer anyway

Even for "small" languages, writing a tree-sitter grammar is a
few-day project (`tree-sitter init`, `tree-sitter generate`, test
against samples) and the result drops into Lattice through
Section 1 or 2. tree-sitter scales down better than its
reputation -- the grammar for `gitcommit` or a config DSL is
~50 lines of `grammar.js`. If a language doesn't have one and
the niche has more than a handful of users, writing one is the
high-leverage move.

---

## 4. Grammar bumps

When you bump `tree-sitter-<lang>` from one minor version to the
next:

1. Run `cargo build -p lattice-syntax`. The `Query::new` calls in
   `LangRegistry::standard()` happen at registry construction,
   so any "Invalid node type X" error from a renamed node fails
   the build immediately -- you don't have to wait for a runtime
   query.
2. Open `crates/lattice-syntax/queries/<lang>/folds.scm` (and
   `highlights.scm` etc. once we move them in). Cross-reference
   against the upstream `queries/folds.scm` for the new grammar
   version (linked from the grammar crate's README, usually).
3. Drop or rename captures that point at vanished node types.
4. Re-run the smoke tests for that language.

The `tree-sitter` CLI (`cargo install tree-sitter-cli`) has a
`tree-sitter test` and `tree-sitter parse <file>` workflow that
makes this fast: parse a representative file, eyeball the
output, identify which node names changed.

---

## See also

- [`folding.md`](folding.md) -- how `folds.scm` captures map to
  the user-facing fold model (`zc` / `zo` / `zi` / etc.).
- `docs/dev/architecture/design.md` §5.3 -- the syntax subsystem design.
- `docs/dev/architecture/design.md` §5.5 -- plugin host architecture (Phase 7).
- The [tree-sitter docs](https://tree-sitter.github.io/) for the
  query language and the per-grammar conventions.
