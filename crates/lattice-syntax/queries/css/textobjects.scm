; Tree-sitter text-object ranges for CSS.
; Capture-name convention follows nvim-treesitter / Helix
; (`@block.outer`, @block.inner`). Inner captures the
; body/block child; if the child has no body field, inner
; falls back to the parent node.

; Blocks: rule sets, media queries, keyframes.
(rule_set) @block.outer
(media_statement) @block.outer
(keyframes_statement) @block.outer

(rule_set (block) @block.inner)
(media_statement (block) @block.inner)
(keyframes_statement (keyframe_block_list) @block.inner)

; Comments.
(comment) @comment.outer
(comment) @comment.inner
