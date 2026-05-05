; Python: definition-position identifiers for the
; `gen:tree-sitter-symbol` insert-completion source.
; Only `@symbol` captures matter; the collector reads the
; matched text directly.

(function_definition name: (identifier) @symbol)
(class_definition name: (identifier) @symbol)

; Function / lambda parameters. `parameters` wraps both,
; and the grammar exposes named `(identifier)` for simple
; positional params plus `(default_parameter name: ...)` /
; `(typed_parameter ...)` for the more decorated forms.
(parameters (identifier) @symbol)
(default_parameter name: (identifier) @symbol)
(typed_parameter (identifier) @symbol)
(typed_default_parameter name: (identifier) @symbol)
(lambda_parameters (identifier) @symbol)

; Top-level + function-scope assignments. Tuple / list /
; pattern-matching unpacks aren't covered in v1 (tree
; structure differs across versions of the python grammar);
; simple `name = ...` carries the common case.
(assignment left: (identifier) @symbol)

; `for x in ...` introduces `x`.
(for_statement left: (identifier) @symbol)

; `with ... as x:` introduces `x`. The grammar's `with_item`
; carries the binding under `(as_pattern ... alias: ...)`,
; not as a direct field on the with_item itself.
(with_item value: (as_pattern alias: (as_pattern_target (identifier) @symbol)))
