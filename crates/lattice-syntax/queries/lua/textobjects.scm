; Tree-sitter text-object ranges for Lua.
; Capture-name convention follows nvim-treesitter / Helix.

; Functions.
(function_declaration) @function.outer
(function_definition) @function.outer
(function_declaration body: (_) @function.inner)
(function_definition body: (_) @function.inner)

; Blocks.
(if_statement) @block.outer
(for_statement) @block.outer
(while_statement) @block.outer
(repeat_statement) @block.outer
(if_statement consequence: (_) @block.inner)
(for_statement body: (_) @block.inner)
(while_statement body: (_) @block.inner)
(repeat_statement body: (_) @block.inner)

; Comments.
(comment) @comment.outer
(comment) @comment.inner

; Note: tree-sitter-lua v0.5.0 has no single `parameter` node;
; see `parameters` (a list node). Parameter textobjects are
; unsupported in this grammar version.
