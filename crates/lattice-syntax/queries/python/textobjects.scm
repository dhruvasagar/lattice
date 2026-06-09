; Tree-sitter text-object ranges for Python.
; Capture-name convention follows nvim-treesitter / Helix
; (`@function.outer`, `@class.outer`, `@block.outer`). Read by
; Lattice's narrow-mode via `SyntaxSnapshot::scope_at_cursor`
; (innermost match whose capture name ends with the requested
; suffix and whose byte span contains the cursor). Only `.outer`
; whole-construct variants ship in v1.

; Functions: `def`-functions / methods and lambdas. A lambda
; nested in a function is the innermost match.
(function_definition) @function.outer
(lambda) @function.outer

; Classes.
(class_definition) @class.outer

; Blocks / scopes. Python's indented suite is a `block`; the
; compound statements are listed alongside so positioning on the
; `if` / `for` / `while` / `with` / `try` keyword targets the whole
; construct rather than just its suite.
(block) @block.outer
(if_statement) @block.outer
(for_statement) @block.outer
(while_statement) @block.outer
(with_statement) @block.outer
(try_statement) @block.outer
(match_statement) @block.outer
