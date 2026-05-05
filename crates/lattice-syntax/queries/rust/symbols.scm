; Rust: definition-position identifiers the
; `gen:tree-sitter-symbol` insert-completion source
; surfaces. Capture name `@symbol` is the only thing the
; collector reads -- the host extracts the matched text and
; pushes it into the candidate set.
;
; v1 captures definition-style positions (where a name is
; *introduced*) rather than references. Walking the syntax
; tree per popup-trigger keeps the source sync-cheap; full
; locals analysis (scope-aware filtering) lands later.

(function_item name: (identifier) @symbol)
(function_signature_item name: (identifier) @symbol)

(struct_item name: (type_identifier) @symbol)
(enum_item name: (type_identifier) @symbol)
(union_item name: (type_identifier) @symbol)
(trait_item name: (type_identifier) @symbol)
(type_item name: (type_identifier) @symbol)

(const_item name: (identifier) @symbol)
(static_item name: (identifier) @symbol)

(let_declaration pattern: (identifier) @symbol)
(parameter pattern: (identifier) @symbol)

(mod_item name: (identifier) @symbol)
