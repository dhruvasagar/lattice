; Tree-sitter fold ranges for Markdown (block grammar).
; Adapted from the Helix editor's runtime/queries/markdown/folds.scm
; (Mozilla Public License 2.0). The exact node-type set tracks
; tree-sitter-md 0.3.x's block grammar; nodes like `table` that the
; upstream Helix query references are excluded here because they
; aren't in this grammar version. Re-add when we upgrade.

[
  (section)
  (fenced_code_block)
  (indented_code_block)

  (list)
  (block_quote)
  (html_block)
] @fold

; @codeblock marks full-row code-block-background regions for the host's
; `syntax.code_block` tint (compute_code_block_lines). Separate from @fold so
; the concerns don't entangle: these nodes fold via @fold above AND tint via
; @codeblock here. A plugin grammar (e.g. org) declares its own blocks the same
; way to reuse the tint.
[
  (fenced_code_block)
  (indented_code_block)
] @codeblock
