; Refactor facts for Python.
;
; Facts only refactoring reads (refactor.md §9.1). Captures:
;   @refactor.statement             a node a declaration may go before, as
;                                   a child of the block that holds it
;   @refactor.hoist-barrier.<why>   code that does not run exactly once, in
;                                   order, when its statement runs; <why> is
;                                   short-circuit, branch, loop, closure,
;                                   comprehension or default, and names the
;                                   construct in the refusal

(module (_) @refactor.statement)
(block (_) @refactor.statement)

(boolean_operator right: (_) @refactor.hoist-barrier.short-circuit)
; `a if c else b`: the first and last run only one of them.
(conditional_expression . (_) @refactor.hoist-barrier.branch)
(conditional_expression (_) @refactor.hoist-barrier.branch .)
(elif_clause condition: (_) @refactor.hoist-barrier.branch)
(while_statement condition: (_) @refactor.hoist-barrier.loop)
(lambda body: (_) @refactor.hoist-barrier.closure)
(list_comprehension body: (_) @refactor.hoist-barrier.comprehension)
(set_comprehension body: (_) @refactor.hoist-barrier.comprehension)
(dictionary_comprehension body: (_) @refactor.hoist-barrier.comprehension)
(generator_expression body: (_) @refactor.hoist-barrier.comprehension)
(if_clause) @refactor.hoist-barrier.comprehension
