; Refactor facts for JavaScript.
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

(lexical_declaration (variable_declarator name: (identifier) @refactor.binding.name value: (_) @refactor.binding.value)) @refactor.binding
(variable_declaration (variable_declarator name: (identifier) @refactor.binding.name value: (_) @refactor.binding.value)) @refactor.binding

(assignment_expression left: (identifier) @refactor.assigned)
(augmented_assignment_expression left: (identifier) @refactor.assigned)
(update_expression argument: (identifier) @refactor.assigned)

[(call_expression) (new_expression) (array) (object) (arrow_function) (function_expression) (class) (await_expression) (yield_expression)] @refactor.effect
[(binary_expression) (unary_expression) (ternary_expression) (assignment_expression) (sequence_expression) (arrow_function) (await_expression) (yield_expression)] @refactor.compound
(shorthand_property_identifier) @refactor.shorthand

; Bodies that run sometimes or repeatedly. Extract never sees these as
; barriers (they hold statements of their own, and only a barrier inside
; the chosen statement counts); inline moves a value INTO one.
(switch_case body: (_) @refactor.hoist-barrier.branch)
(switch_default body: (_) @refactor.hoist-barrier.branch)
(for_in_statement body: (_) @refactor.hoist-barrier.loop)
(function_declaration body: (_) @refactor.hoist-barrier.closure)
