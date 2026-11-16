; Refactor facts for Rust.
;
; Facts only refactoring reads (refactor.md §9.1). Captures:
;   @refactor.statement             a node a declaration may go before, as
;                                   a child of the block that holds it
;   @refactor.hoist-barrier.<why>   code that does not run exactly once, in
;                                   order, when its statement runs; <why> is
;                                   short-circuit, branch, loop, closure,
;                                   comprehension or default, and names the
;                                   construct in the refusal

(block (_) @refactor.statement)

(binary_expression operator: ["&&" "||"] right: (_) @refactor.hoist-barrier.short-circuit)
(if_expression consequence: (_) @refactor.hoist-barrier.branch)
(else_clause (_) @refactor.hoist-barrier.branch)
(match_arm value: (_) @refactor.hoist-barrier.branch)
(while_expression condition: (_) @refactor.hoist-barrier.loop)
(closure_expression body: (_) @refactor.hoist-barrier.closure)
