; Appended to tree-sitter-md's own block injections (see `registry.rs`).
;
; The upstream query hands `(inline)` nodes to the inline grammar and stops
; there. A pipe table's cells are not `inline` nodes — `pipe_table_cell` is a
; leaf of the block grammar — so once a table is recognised as one (a header
; row and a delimiter row), nothing inside it is parsed for emphasis, code
; spans or links, and the whole table reads as plain text. The same rows with
; no delimiter are an ordinary paragraph and are highlighted, which makes a
; correct table look worse than a broken one.
((pipe_table_cell) @injection.content
  (#set! injection.language "markdown_inline"))
