; Refactor facts for C++.
;
; Facts only refactoring reads (refactor.md §9.1). Captures:
;   @refactor.statement             a node a declaration may go before, as
;                                   a child of the block that holds it
;   @refactor.hoist-barrier.<why>   code that does not run exactly once, in
;                                   order, when its statement runs; <why> is
;                                   short-circuit, branch, loop, closure,
;                                   comprehension or default, and names the
;                                   construct in the refusal

(compound_statement (_) @refactor.statement)
(case_statement value: (_) (_) @refactor.statement)
(case_statement !value (_) @refactor.statement)

(binary_expression operator: ["&&" "||"] right: (_) @refactor.hoist-barrier.short-circuit)
(conditional_expression consequence: (_) @refactor.hoist-barrier.branch)
(conditional_expression alternative: (_) @refactor.hoist-barrier.branch)
(if_statement consequence: (_) @refactor.hoist-barrier.branch)
(else_clause (_) @refactor.hoist-barrier.branch)
(while_statement condition: (_) @refactor.hoist-barrier.loop)
(while_statement body: (_) @refactor.hoist-barrier.loop)
(do_statement condition: (_) @refactor.hoist-barrier.loop)
(for_statement condition: (_) @refactor.hoist-barrier.loop)
(for_statement update: (_) @refactor.hoist-barrier.loop)
(for_statement body: (_) @refactor.hoist-barrier.loop)
(lambda_expression body: (_) @refactor.hoist-barrier.closure)
(optional_parameter_declaration default_value: (_) @refactor.hoist-barrier.default)
