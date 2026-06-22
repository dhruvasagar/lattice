; Tree-sitter text-object ranges for JavaScript.
; Capture-name convention follows nvim-treesitter / Helix
; (`@function.outer`, `@class.outer`, `@block.outer`). Read by
; Lattice's narrow-mode via `SyntaxSnapshot::scope_at_cursor`
; (innermost match whose capture name ends with the requested
; suffix and whose byte span contains the cursor). Only `.outer`
; whole-construct variants ship in v1.

; Functions: declarations, expressions, arrows, generators, and
; methods. A nested arrow / function expression is the innermost
; match.
(function_declaration) @function.outer
(function_expression) @function.outer
(arrow_function) @function.outer
(generator_function_declaration) @function.outer
(method_definition) @function.outer

; Classes.
(class_declaration) @class.outer

; Blocks / scopes. The brace-delimited `statement_block` plus the
; multi-line compound statements, so positioning on the `if` /
; `for` / `while` / `switch` / `try` keyword targets the whole
; construct rather than just its body block.
(statement_block) @block.outer
(if_statement) @block.outer
(for_statement) @block.outer
(for_in_statement) @block.outer
(while_statement) @block.outer
(switch_statement) @block.outer
(try_statement) @block.outer

; --- N.1.4c: inner bodies, parameters, and loop objects ---
; `.inner` captures the body block (braces included, v1). Arrow bodies
; may be an expression or a block. Parameters fall back to the bare
; param node (`aa` == `ia`).

; Function bodies (`if`).
(function_declaration body: (statement_block) @function.inner)
(function_expression body: (statement_block) @function.inner)
(arrow_function body: (_) @function.inner)
(generator_function_declaration body: (statement_block) @function.inner)
(method_definition body: (statement_block) @function.inner)

; Class body (`ic`).
(class_declaration body: (class_body) @class.inner)

; Parameters (`aa` / `ia`).
(formal_parameters (_) @parameter.outer)
(formal_parameters (_) @parameter.inner)

; Loops (`al` / `il`).
(for_statement) @loop.outer
(for_in_statement) @loop.outer
(while_statement) @loop.outer
(do_statement) @loop.outer
(for_statement body: (statement_block) @loop.inner)
(for_in_statement body: (statement_block) @loop.inner)
(while_statement body: (statement_block) @loop.inner)
(do_statement body: (statement_block) @loop.inner)
