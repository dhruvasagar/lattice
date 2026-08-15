; Tree-sitter indent rules for Ruby.
;
; Capture vocabulary follows Helix's `indents.scm` dialect; written for
; lattice rather than vendored (auto-indent.md §4.1).
;
; **Capture the CONSTRUCT, not the body.** tree-sitter-ruby's
; `body_statement` starts at the first statement, not at the `def` that
; opens it:
;
;     method          [rows 0..2]
;       body_statement [rows 1..1]   ← starts on the BODY row
;       end            [row 2]
;
; The engine indents a row when an `@indent` ancestor satisfies
; `start_row < row <= end_row`, so capturing `body_statement` indents
; nothing. Capturing `method` (rows 0..2) makes row 1 indent and lets
; `end` on row 2 dedent it back. Same quirk as Lua and Python.
;
; **Modifier forms need no special handling.** `return if x` parses to
; an `if` node spanning a single row, and a one-row construct can never
; satisfy `start_row < row <= end_row` for any row — so capturing `if`
; is safe and the modifier form simply contributes nothing. The row rule
; excludes it structurally rather than by a rule written for the case.

[
  ; Constructs that own an indented body.
  (method)
  (singleton_method)
  (class)
  (singleton_class)
  (module)
  (if)
  (unless)
  (while)
  (until)
  (for)
  (case)
  (begin)
  (do_block)
  (block)
  (lambda)
  (then)
  (else)
  (elsif)
  (when)
  (rescue)
  (ensure)

  ; Bracketed continuations.
  (argument_list)
  (method_parameters)
  (block_parameters)
  (array)
  (hash)
] @indent

[
  "end"
  "}"
  "]"
  ")"
] @outdent
