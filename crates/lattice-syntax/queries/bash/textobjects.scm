; Tree-sitter text-object ranges for Bash.
; Capture-name convention follows nvim-treesitter / Helix
; (`@function.outer`, `@block.outer`, `@comment.outer`). Read by
; Lattice's narrow-mode via `SyntaxSnapshot::scope_at_cursor`
; (innermost match whose capture name ends with the requested
; suffix and whose byte span contains the cursor).

; --- Functions ---
(function_definition) @function.outer
(function_definition body: (_) @function.inner)

; --- Blocks ---
([
  (if_statement)
  (for_statement)
  (while_statement)
  (case_statement)
] @block.outer)

(for_statement body: (_) @block.inner)
(while_statement body: (_) @block.inner)
(case_statement) @block.inner

; --- Comments ---
(comment) @comment.outer
(comment) @comment.inner

; Note: tree-sitter-bash v0.23.3 has no formal parameter list node;
; the `()` in a function definition is pure grammar syntax.
