; Tree-sitter text-object ranges for SQL (tree-sitter-sequel v0.3.x).

; Blocks.
(select) @block.outer
(insert) @block.outer
(update) @block.outer
(delete) @block.outer

; Functions.
(create_function) @function.outer
(create_trigger) @function.outer

; Comments.
(comment) @comment.outer
(comment) @comment.inner
