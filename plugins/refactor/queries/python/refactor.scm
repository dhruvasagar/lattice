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

(expression_statement (assignment left: (identifier) @refactor.binding.name right: (_) @refactor.binding.value)) @refactor.binding

(assignment left: (identifier) @refactor.assigned)
(augmented_assignment left: (identifier) @refactor.assigned)
(for_statement left: (identifier) @refactor.assigned)
(named_expression name: (identifier) @refactor.assigned)

[(call) (list) (dictionary) (set) (list_comprehension) (set_comprehension) (dictionary_comprehension) (generator_expression) (lambda) (await) (yield)] @refactor.effect
[(binary_operator) (boolean_operator) (not_operator) (comparison_operator) (unary_operator) (conditional_expression) (lambda) (await)] @refactor.compound

; Bodies that run sometimes or repeatedly. Extract never sees these as
; barriers (they hold statements of their own, and only a barrier inside
; the chosen statement counts); inline moves a value INTO one.
(if_statement consequence: (_) @refactor.hoist-barrier.branch)
(elif_clause consequence: (_) @refactor.hoist-barrier.branch)
(else_clause body: (_) @refactor.hoist-barrier.branch)
(while_statement body: (_) @refactor.hoist-barrier.loop)
(for_statement body: (_) @refactor.hoist-barrier.loop)
(function_definition body: (_) @refactor.hoist-barrier.closure)
