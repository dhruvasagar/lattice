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

(short_var_declaration left: (expression_list . (identifier) @refactor.binding.name .) right: (expression_list . (_) @refactor.binding.value .)) @refactor.binding
(var_declaration (var_spec name: (identifier) @refactor.binding.name value: (expression_list . (_) @refactor.binding.value .))) @refactor.binding

(assignment_statement left: (expression_list (identifier) @refactor.assigned))
(inc_statement (identifier) @refactor.assigned)
(dec_statement (identifier) @refactor.assigned)
(unary_expression operator: "&" operand: (identifier) @refactor.assigned)

[(call_expression) (composite_literal) (func_literal)] @refactor.effect
(unary_expression operator: "<-") @refactor.effect
[(binary_expression) (unary_expression)] @refactor.compound

; Bodies that run sometimes or repeatedly. Extract never sees these as
; barriers (they hold statements of their own, and only a barrier inside
; the chosen statement counts); inline moves a value INTO one.
(if_statement consequence: (_) @refactor.hoist-barrier.branch)
(if_statement alternative: (_) @refactor.hoist-barrier.branch)
(expression_case) @refactor.hoist-barrier.branch
(default_case) @refactor.hoist-barrier.branch
(for_statement body: (_) @refactor.hoist-barrier.loop)
(func_literal body: (_) @refactor.hoist-barrier.closure)
