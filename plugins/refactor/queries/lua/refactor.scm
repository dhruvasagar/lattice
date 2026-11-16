; Refactor facts for Lua.
;
; Facts only refactoring reads (refactor.md §9.1). Captures:
;   @refactor.statement             a node a declaration may go before, as
;                                   a child of the block that holds it
;   @refactor.hoist-barrier.<why>   code that does not run exactly once, in
;                                   order, when its statement runs; <why> is
;                                   short-circuit, branch, loop, closure,
;                                   comprehension or default, and names the
;                                   construct in the refusal

(chunk (_) @refactor.statement)
(block (_) @refactor.statement)

(binary_expression operator: ["and" "or"] right: (_) @refactor.hoist-barrier.short-circuit)
(elseif_statement condition: (_) @refactor.hoist-barrier.branch)
(while_statement condition: (_) @refactor.hoist-barrier.loop)
(repeat_statement condition: (_) @refactor.hoist-barrier.loop)
(function_definition body: (_) @refactor.hoist-barrier.closure)
