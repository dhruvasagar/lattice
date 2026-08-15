; Tree-sitter indent rules for Python.
;
; Capture vocabulary follows Helix's `indents.scm` dialect; written for
; lattice rather than vendored (auto-indent.md §4.1).
;
; **Capture the CONSTRUCT, not the block.** tree-sitter-python's `block`
; node starts at the first statement of the suite, not at the `:` that
; opens it:
;
;     function_definition [rows 0..1]
;       :                 [row 0]
;       block             [rows 1..1]   ← starts on the BODY row
;
; The engine indents a row when an `@indent` ancestor satisfies
; `start_row < row <= end_row`, so capturing `block` indents nothing.
; Capturing `function_definition` (rows 0..1) makes the body row indent.
; Same structural quirk as Lua and Ruby; the brace family does not have
; it because `{` sits inside the block node.
;
; Python is also the language where indentation IS the syntax, with two
; consequences:
;
; 1. A suite has no closing token. Nothing `@outdent`s a block — the
;    body simply stops when the construct's `end_row` is passed. The
;    `@outdent` set covers only brackets.
; 2. A triple-quoted docstring carries its leading whitespace as data.
;    The engine refuses to answer inside a string scope at all, so that
;    is handled outside this file.

[
  ; Compound statements — the constructs that own a suite.
  (function_definition)
  (class_definition)
  (if_statement)
  (elif_clause)
  (else_clause)
  (for_statement)
  (while_statement)
  (with_statement)
  (try_statement)
  (except_clause)
  (finally_clause)
  (decorated_definition)

  ; Bracketed continuations — these include their delimiters, so they
  ; behave like the brace family.
  (argument_list)
  (parameters)
  (list)
  (dictionary)
  (set)
  (tuple)
  (parenthesized_expression)
  (list_comprehension)
  (dictionary_comprehension)
  (set_comprehension)
  (generator_expression)
] @indent

[
  "}"
  "]"
  ")"
] @outdent
