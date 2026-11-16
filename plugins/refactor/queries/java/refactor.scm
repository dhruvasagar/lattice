; Refactor facts for Java.
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
(constructor_body (_) @refactor.statement)
(switch_block_statement_group (switch_label) (_) @refactor.statement)

(binary_expression operator: ["&&" "||"] right: (_) @refactor.hoist-barrier.short-circuit)
(ternary_expression consequence: (_) @refactor.hoist-barrier.branch)
(ternary_expression alternative: (_) @refactor.hoist-barrier.branch)
(if_statement consequence: (_) @refactor.hoist-barrier.branch)
(if_statement alternative: (_) @refactor.hoist-barrier.branch)
(while_statement condition: (_) @refactor.hoist-barrier.loop)
(while_statement body: (_) @refactor.hoist-barrier.loop)
(do_statement condition: (_) @refactor.hoist-barrier.loop)
(for_statement condition: (_) @refactor.hoist-barrier.loop)
(for_statement update: (_) @refactor.hoist-barrier.loop)
(for_statement body: (_) @refactor.hoist-barrier.loop)
(lambda_expression body: (_) @refactor.hoist-barrier.closure)
