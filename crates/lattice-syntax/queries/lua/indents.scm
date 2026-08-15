; Tree-sitter indent rules for Lua.
;
; Capture vocabulary follows Helix's `indents.scm` dialect; written for
; lattice rather than vendored (auto-indent.md §4.1).
;
; **Capture the CONSTRUCT, not the block.** tree-sitter-lua's `block`
; node starts at the first statement inside the body, not at the `then`
; / `do` that opens it:
;
;     if_statement [rows 0..2]
;       then       [row 0]
;       block      [rows 1..1]   ← starts on the BODY row
;       end        [row 2]
;
; The engine indents a row when an `@indent` ancestor satisfies
; `start_row < row <= end_row`, so capturing `block` indents nothing —
; its start row IS the body row. Capturing `if_statement` (rows 0..2)
; makes row 1 indent and lets the `end` on row 2 dedent it back.
;
; This differs from the brace family, where `{` is inside the block node
; and the block therefore starts on the opener's row. It is a real
; structural difference between grammars, not a style choice.
;
; Word closers are `@outdent`: `end`, `until`, and also `else` /
; `elseif`, which continue the construct but sit back at its level.

[
  (function_declaration)
  (function_definition)
  (if_statement)
  (elseif_statement)
  (else_statement)
  (while_statement)
  (for_statement)
  (repeat_statement)
  (do_statement)

  ; Bracketed continuations — these DO include their delimiters, so
  ; they behave like the brace family.
  (table_constructor)
  (arguments)
  (parameters)
] @indent

[
  "end"
  "until"
  "else"
  "elseif"
  "}"
  "]"
  ")"
] @outdent
