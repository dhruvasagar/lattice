; Tree-sitter text-object ranges for Rust.
; Capture-name convention follows nvim-treesitter / Helix
; (`@function.outer`, `@class.outer`, `@block.outer`). Lattice's
; narrow-mode reads these via `SyntaxSnapshot::scope_at_cursor`,
; which matches a capture whose name *ends with* the requested
; suffix and returns the innermost (smallest-span) match that
; contains the cursor. Only `.outer` (whole-construct, brace-to-
; brace) variants ship in v1; `.inner` (body-without-delimiters)
; is deferred to the text-object operator slice alongside
; partial-line narrowing (N.2).

; Functions: free functions, methods (also `function_item`), and
; closures. A closure nested in a function is the innermost match,
; so `:narrow-function` inside a closure narrows the closure.
(function_item) @function.outer
(closure_expression) @function.outer

; "Class"-shaped definitions. Rust has no `class`, so the analogue
; set is the nominal-type + impl constructs a user means by "narrow
; this type". A method's enclosing `impl_item` is the innermost
; class.outer, so `:narrow-class` from inside a method narrows the
; whole impl block.
(struct_item) @class.outer
(enum_item) @class.outer
(union_item) @class.outer
(trait_item) @class.outer
(impl_item) @class.outer

; Blocks / scopes. Bare brace blocks plus the multi-line control-
; flow expressions. Innermost-wins means a cursor on a statement
; lands on the tightest enclosing brace block; positioning on the
; `if` / `match` / loop keyword line instead targets the whole
; control-flow construct.
(block) @block.outer
(if_expression) @block.outer
(match_expression) @block.outer
(while_expression) @block.outer
(for_expression) @block.outer
(loop_expression) @block.outer
