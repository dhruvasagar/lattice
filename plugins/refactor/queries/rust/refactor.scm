; Refactor facts for Rust.
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

(binary_expression operator: ["&&" "||"] right: (_) @refactor.hoist-barrier.short-circuit)
(if_expression consequence: (_) @refactor.hoist-barrier.branch)
(else_clause (_) @refactor.hoist-barrier.branch)
(match_arm value: (_) @refactor.hoist-barrier.branch)
(while_expression condition: (_) @refactor.hoist-barrier.loop)
(closure_expression body: (_) @refactor.hoist-barrier.closure)

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

(let_declaration . pattern: (identifier) @refactor.binding.name value: (_) @refactor.binding.value !alternative) @refactor.binding

(assignment_expression left: (identifier) @refactor.assigned)
(compound_assignment_expr left: (identifier) @refactor.assigned)

[(call_expression) (macro_invocation) (array_expression) (struct_expression) (closure_expression) (await_expression)] @refactor.effect
[(binary_expression) (unary_expression) (range_expression) (type_cast_expression) (reference_expression) (closure_expression)] @refactor.compound
(shorthand_field_initializer (identifier) @refactor.shorthand)
(macro_invocation (token_tree (string_literal) @refactor.opaque))

; Bodies that run sometimes or repeatedly. Extract never sees these as
; barriers (they hold statements of their own, and only a barrier inside
; the chosen statement counts); inline moves a value INTO one.
(while_expression body: (_) @refactor.hoist-barrier.loop)
(loop_expression body: (_) @refactor.hoist-barrier.loop)
(for_expression body: (_) @refactor.hoist-barrier.loop)
