; Tree-sitter text-object ranges for HTML.
; Capture-name convention follows nvim-treesitter / Helix
; (`@block.outer`, @block.inner`).

; Blocks: elements and comments. Inner captures the same
; node (tree-sitter-html has no body child for element).
(element) @block.outer
(comment) @block.outer

(element) @block.inner
(comment) @block.inner

; Comments.
(comment) @comment.outer
(comment) @comment.inner
