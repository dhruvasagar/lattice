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

; Inline (RF.4f). Captures:
;   @refactor.binding (+ .name, .value)  a declaration of one name with a
;                                        value: what inline deletes
;   @refactor.assigned                   a name written to after it is bound
;   @refactor.effect                     a value that runs code or makes a
;                                        new object each time it runs
;   @refactor.compound                   a value that needs parentheses
;                                        where an operand goes
;   @refactor.shorthand                  a use that names a field too
;                                        (`S { x }`): it becomes `x: value`
;   @refactor.opaque                     text that may read a name without
;                                        a node for it (a format string)
;   @refactor.declarator-only            a value that only means something
;                                        in its declaration

(declaration declarator: (init_declarator declarator: (identifier) @refactor.binding.name value: (_) @refactor.binding.value)) @refactor.binding

(assignment_expression left: (identifier) @refactor.assigned)
(update_expression argument: (identifier) @refactor.assigned)
(pointer_expression operator: "&" argument: (identifier) @refactor.assigned)

[(call_expression) (new_expression) (lambda_expression)] @refactor.effect
[(binary_expression) (unary_expression) (conditional_expression) (cast_expression) (comma_expression) (lambda_expression) (pointer_expression)] @refactor.compound
(init_declarator value: [(initializer_list) (argument_list)] @refactor.declarator-only)

; Bodies that run sometimes or repeatedly. Extract never sees these as
; barriers (they hold statements of their own, and only a barrier inside
; the chosen statement counts); inline moves a value INTO one.
(case_statement) @refactor.hoist-barrier.branch
(for_range_loop body: (_) @refactor.hoist-barrier.loop)
