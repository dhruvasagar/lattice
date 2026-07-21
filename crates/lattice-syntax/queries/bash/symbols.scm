; Bash: definition-position identifiers surfaced by the
; `gen:tree-sitter-symbol` insert-completion source.
; Capture name `@symbol` is the only thing the collector reads.

; Function definitions — the name word after `function` or before `()`.
(function_definition name: (word) @symbol)

; Variable assignments inside declaration commands (local, declare, export, etc.).
(declaration_command
  (variable_assignment
    name: (variable_name) @symbol))

; Standalone variable assignments at the start of a simple command.
(variable_assignment
  name: (variable_name) @symbol)
