; Refactor facts for TypeScript and TSX (one file: TSX's grammar is a
; superset of TypeScript's).
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
(statement_block (_) @refactor.statement)
(switch_case body: (_) @refactor.statement)
(switch_default body: (_) @refactor.statement)

(binary_expression operator: ["&&" "||" "??"] right: (_) @refactor.hoist-barrier.short-circuit)
(ternary_expression consequence: (_) @refactor.hoist-barrier.branch)
(ternary_expression alternative: (_) @refactor.hoist-barrier.branch)
(if_statement consequence: (_) @refactor.hoist-barrier.branch)
(else_clause (_) @refactor.hoist-barrier.branch)
(switch_case value: (_) @refactor.hoist-barrier.branch)
(while_statement condition: (_) @refactor.hoist-barrier.loop)
(while_statement body: (_) @refactor.hoist-barrier.loop)
(do_statement condition: (_) @refactor.hoist-barrier.loop)
(for_statement condition: (_) @refactor.hoist-barrier.loop)
(for_statement increment: (_) @refactor.hoist-barrier.loop)
(for_statement body: (_) @refactor.hoist-barrier.loop)
(arrow_function body: (_) @refactor.hoist-barrier.closure)
(function_expression body: (_) @refactor.hoist-barrier.closure)
(assignment_pattern right: (_) @refactor.hoist-barrier.default)
(required_parameter value: (_) @refactor.hoist-barrier.default)
(optional_parameter value: (_) @refactor.hoist-barrier.default)
