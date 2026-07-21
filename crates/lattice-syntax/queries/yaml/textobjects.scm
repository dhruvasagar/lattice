; Tree-sitter text-object ranges for YAML.
; Capture-name convention follows nvim-treesitter / Helix
; (`@block.outer`, `@comment.outer`).  Lattice's narrow-mode reads
; these via `SyntaxSnapshot::scope_at_cursor`.

(block_mapping_pair) @block.outer
(block_mapping_pair value: (_) @block.inner)

(block_sequence) @block.outer
(block_sequence) @block.inner

(comment) @comment.outer
(comment) @comment.inner
