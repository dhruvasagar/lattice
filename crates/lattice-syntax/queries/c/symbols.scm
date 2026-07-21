; C: definition-position identifiers for the
; `gen:tree-sitter-symbol` insert-completion source.

(function_declarator declarator: (identifier) @symbol)

(struct_specifier name: (type_identifier) @symbol)
(union_specifier name: (type_identifier) @symbol)
(enum_specifier name: (type_identifier) @symbol)
