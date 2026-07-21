; Tree-sitter text-object ranges for Ruby.
; Capture-name convention follows nvim-treesitter / Helix.

; Functions.
(method) @function.outer
(singleton_method) @function.outer
(method body: (body_statement) @function.inner)
(singleton_method body: (body_statement) @function.inner)

; Classes.
(class) @class.outer
(module) @class.outer
(class body: (body_statement) @class.inner)
(module body: (body_statement) @class.inner)

; Blocks.
(if) @block.outer
(unless) @block.outer
(case) @block.outer
(while) @block.outer
(until) @block.outer
(for) @block.outer
(begin) @block.outer
(if consequence: (_) @block.inner)
(unless consequence: (_) @block.inner)
(case (_) @block.inner)
(while body: (_) @block.inner)
(until body: (_) @block.inner)
(for body: (_) @block.inner)
(begin (_) @block.inner)

; Comments.
(comment) @comment.outer
(comment) @comment.inner

; Parameters.
(method_parameters (_) @parameter.outer)
(method_parameters (_) @parameter.inner)
(block_parameters (_) @parameter.outer)
(block_parameters (_) @parameter.inner)
