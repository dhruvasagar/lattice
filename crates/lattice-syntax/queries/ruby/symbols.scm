; Ruby: definition-position identifiers for the
; `gen:tree-sitter-symbol` insert-completion source.

(method name: (_) @symbol)
(singleton_method name: (_) @symbol)
(class name: (constant) @symbol)
(module name: (constant) @symbol)
(assignment left: (_) @symbol)
(call method: (_) @symbol)
