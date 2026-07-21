; Tree-sitter fold ranges for Bash.
; Node types target tree-sitter-bash v0.23.3 grammar: function
; definitions, control-flow blocks, and heredoc bodies.

[
  (function_definition)

  (if_statement)
  (for_statement)
  (while_statement)
  (case_statement)

  (heredoc_body)
] @fold
