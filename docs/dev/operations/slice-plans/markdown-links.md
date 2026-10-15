# Markdown links — slice plan (MDL)

> Design: [`../../architecture/markdown-links.md`](../../architecture/markdown-links.md).
> Precedent: [`archive/conceal-and-org-links.md`](archive/conceal-and-org-links.md)
> (the same two halves — render and follow — for org).

Status icons: ✅ done · 🚧 in progress · 📝 planned · ⛔ deferred (not yet) · ❌ dropped (not at all).

**Status:** 🚧 in progress (started 2026-10-15).

| Slice | What | Status |
|---|---|---|
| MDL.1 | Markdown link scanning + heading slugs move to `lattice-syntax::markdown`; `lattice-help` builds on it. No behaviour change. | ✅ |
| MDL.2 | `markdown-mode` owns `<CR>` → `action:markdown-follow-link`: `#anchor`, file, `file#anchor`, external URL, autolink; declines off a link. | 📝 |
| MDL.3 | Native languages can declare conceal rules (`conceal_source`); markdown declares inline-link + autolink rules. Bench case. | 📝 |

## MDL.1 — one scanner

`scan_link`, `find_unescaped`, `escape_link_text` / `unescape_link_text`,
`is_external_url`, `slugify_heading` and the heading walk move out of
`lattice-help` into `lattice-syntax/src/markdown.rs`, which also gains the
markdown-file classification (`LinkDestination`) and `link_at`. Help re-exports
the two helpers its formatters import, so no caller path changes.

**Done when:** `cargo test -p lattice-help` passes unchanged and the new module
has its own unit tests.

## MDL.2 — `<CR>` follows

`MarkdownMode` leaves the `lang_mode!` macro for a hand-written impl in
`markdown_mode.rs` with `keymap()` and `action_handlers()`. The command is
declared by `register_markdown_commands`, called at boot beside
`register_syntax_motions`.

**Tests:** unit — path resolution, missing file, `file#heading` + jump record.
Host integration — `<CR>` on `[t](#h)` moves the cursor and `<C-o>` returns; on
`[t](other.md)` opens the file; off a link changes nothing; `<CR>` in a help
buffer still follows the help link (help-mode's minor shadows the major).

## MDL.3 — conceal

`registry.rs` builds native `LangConfig`s with an empty rule list. Add
`conceal_source(name) -> &[(pattern, hide, slot)]`, compiled through the same
`compile_rules` plugins use. Markdown's rules hide `[` + `](url)` (guarded
against `![` images, `\[` escapes and a leading backtick) and `<` + `>` of an
autolink.

**Tests:** the rules compile; a link conceals to its label; an image, an
escaped bracket and a code-span-leading link do not. **Bench:** a link-dense
markdown viewport in `benches/conceal.rs`.
