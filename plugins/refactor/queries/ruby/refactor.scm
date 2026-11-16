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

(assignment left: (identifier) @refactor.binding.name right: (_) @refactor.binding.value) @refactor.binding

(assignment left: (identifier) @refactor.assigned)
(operator_assignment left: (identifier) @refactor.assigned)

[(call) (array) (hash) (lambda) (block) (do_block)] @refactor.effect
[(binary) (unary) (conditional) (range)] @refactor.compound

; Bodies that run sometimes or repeatedly. Extract never sees these as
; barriers (they hold statements of their own, and only a barrier inside
; the chosen statement counts); inline moves a value INTO one.
(if consequence: (_) @refactor.hoist-barrier.branch)
(if alternative: (_) @refactor.hoist-barrier.branch)
(unless consequence: (_) @refactor.hoist-barrier.branch)
(unless alternative: (_) @refactor.hoist-barrier.branch)
(when body: (_) @refactor.hoist-barrier.branch)
(while body: (_) @refactor.hoist-barrier.loop)
(until body: (_) @refactor.hoist-barrier.loop)
(for body: (_) @refactor.hoist-barrier.loop)
