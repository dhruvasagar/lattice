; TypeScript: definition-position identifiers for the
; `gen:tree-sitter-symbol` insert-completion source.

(function_declaration name: (identifier) @symbol)
(method_definition name: (property_identifier) @symbol)
(class_declaration name: (type_identifier) @symbol)
(interface_declaration name: (type_identifier) @symbol)
(enum_declaration name: (identifier) @symbol)
(internal_module name: (identifier) @symbol)

(variable_declarator name: (identifier) @symbol)

(type_alias_declaration name: (type_identifier) @symbol)
(property_signature name: (property_identifier) @symbol)

(required_parameter name: (identifier) @symbol)
(optional_parameter name: (identifier) @symbol)
