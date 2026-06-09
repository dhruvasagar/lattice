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
