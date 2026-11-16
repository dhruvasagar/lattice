; Refactor facts for Go.
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
(expression_case value: (_) (_) @refactor.statement)
(default_case (_) @refactor.statement)

(binary_expression operator: ["&&" "||"] right: (_) @refactor.hoist-barrier.short-circuit)
(if_statement alternative: (if_statement) @refactor.hoist-barrier.branch)
(expression_case value: (_) @refactor.hoist-barrier.branch)
(for_clause condition: (_) @refactor.hoist-barrier.loop)
(for_clause update: (_) @refactor.hoist-barrier.loop)
; `for cond {`: the condition is the loop's first, unfielded child.
(for_statement . [
  (binary_expression)
  (unary_expression)
  (call_expression)
  (identifier)
  (selector_expression)
  (parenthesized_expression)
  (index_expression)
] @refactor.hoist-barrier.loop)
(func_literal body: (_) @refactor.hoist-barrier.closure)
