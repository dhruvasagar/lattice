; JavaScript: definition-position identifiers for the
; `gen:tree-sitter-symbol` insert-completion source.

(function_declaration name: (identifier) @symbol)
(generator_function_declaration name: (identifier) @symbol)
(class_declaration name: (identifier) @symbol)
(method_definition name: (property_identifier) @symbol)

(variable_declarator name: (identifier) @symbol)
(lexical_declaration (variable_declarator name: (identifier) @symbol))

; Function / arrow / method parameters. `(identifier)` covers
; the simple positional form; rest / default / destructuring
; cases are deferred.
(formal_parameters (identifier) @symbol)

(arrow_function parameter: (identifier) @symbol)
