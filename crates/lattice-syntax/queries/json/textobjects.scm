; Tree-sitter text-object ranges for JSON.
; Capture-name convention follows nvim-treesitter / Helix
; (`@block.outer`).  Lattice's narrow-mode reads these via
; `SyntaxSnapshot::scope_at_cursor`, which matches the innermost
; capture whose name ends with the requested suffix and whose
; byte span contains the cursor.

(object) @block.outer
(object (pair) @block.inner)

(array) @block.outer
(array) @block.inner
