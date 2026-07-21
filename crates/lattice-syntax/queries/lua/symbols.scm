; Lua: definition-position identifiers for the
; `gen:tree-sitter-symbol` insert-completion source.

(function_declaration name: (_) @symbol)

(assignment_statement (variable_list (identifier) @symbol))
