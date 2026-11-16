; Refactor facts for Ruby.
;
; Facts only refactoring reads (refactor.md §9.1). Captures:
;   @refactor.statement             a node a declaration may go before, as
;                                   a child of the block that holds it
;   @refactor.hoist-barrier.<why>   code that does not run exactly once, in
;                                   order, when its statement runs; <why> is
;                                   short-circuit, branch, loop, closure,
;                                   comprehension or default, and names the
;                                   construct in the refusal

(program (_) @refactor.statement)
(body_statement (_) @refactor.statement)
(block_body (_) @refactor.statement)
(then (_) @refactor.statement)
(else (_) @refactor.statement)
(do (_) @refactor.statement)

(binary operator: ["&&" "||" "and" "or"] right: (_) @refactor.hoist-barrier.short-circuit)
(conditional consequence: (_) @refactor.hoist-barrier.branch)
(conditional alternative: (_) @refactor.hoist-barrier.branch)
(elsif condition: (_) @refactor.hoist-barrier.branch)
(if_modifier body: (_) @refactor.hoist-barrier.branch)
(unless_modifier body: (_) @refactor.hoist-barrier.branch)
(while condition: (_) @refactor.hoist-barrier.loop)
(until condition: (_) @refactor.hoist-barrier.loop)
(while_modifier body: (_) @refactor.hoist-barrier.loop)
(block) @refactor.hoist-barrier.closure
(do_block) @refactor.hoist-barrier.closure
(lambda) @refactor.hoist-barrier.closure
