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

(variable_declaration (assignment_statement (variable_list . name: (identifier) @refactor.binding.name .) (expression_list . value: (_) @refactor.binding.value .))) @refactor.binding

(assignment_statement (variable_list name: (identifier) @refactor.assigned))

[(function_call) (table_constructor) (function_definition)] @refactor.effect
[(binary_expression) (unary_expression) (function_definition)] @refactor.compound

; Bodies that run sometimes or repeatedly. Extract never sees these as
; barriers (they hold statements of their own, and only a barrier inside
; the chosen statement counts); inline moves a value INTO one.
(if_statement consequence: (_) @refactor.hoist-barrier.branch)
(elseif_statement consequence: (_) @refactor.hoist-barrier.branch)
(else_statement body: (_) @refactor.hoist-barrier.branch)
(while_statement body: (_) @refactor.hoist-barrier.loop)
(repeat_statement body: (_) @refactor.hoist-barrier.loop)
(for_statement body: (_) @refactor.hoist-barrier.loop)
