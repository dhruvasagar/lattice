; Tree-sitter text-object ranges for C++.
; Capture-name convention follows nvim-treesitter / Helix
; (`@function.outer`, `@class.outer`, `@block.outer`). Read by
; Lattice's narrow-mode via `SyntaxSnapshot::scope_at_cursor`
; (innermost match whose capture name ends with the requested
; suffix and whose byte span contains the cursor).

; Functions.
(function_definition) @function.outer
(function_definition body: (compound_statement) @function.inner)

; "Class"-shaped definitions: struct, union, enum, class.
(struct_specifier) @class.outer
(union_specifier) @class.outer
(enum_specifier) @class.outer
(class_specifier) @class.outer
(struct_specifier body: (field_declaration_list) @class.inner)
(union_specifier body: (field_declaration_list) @class.inner)
(enum_specifier body: (enumerator_list) @class.inner)
(class_specifier body: (field_declaration_list) @class.inner)

; Blocks / scopes: control-flow statements.
(for_statement) @block.outer
(while_statement) @block.outer
(if_statement) @block.outer
(for_statement body: (compound_statement) @block.inner)
(while_statement body: (compound_statement) @block.inner)
(if_statement consequence: (compound_statement) @block.inner)

; Comments.
(comment) @comment.outer
(comment) @comment.inner

; Parameters (`aa` / `ia`).
(parameter_declaration) @parameter.outer
(parameter_declaration) @parameter.inner
