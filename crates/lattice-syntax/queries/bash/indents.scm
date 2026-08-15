; Tree-sitter indent rules for Bash.
;
; Capture vocabulary follows Helix's `indents.scm` dialect; written for
; lattice rather than vendored (auto-indent.md §4.1).
;
; Bash closes with words (`fi`, `done`, `esac`) and with `}` / `)`, so
; the `@outdent` set spans both.
;
; `elif_clause` / `else_clause` are captured as `@indent` (their bodies
; are deeper) while `elif` / `else` / `fi` are `@outdent` (the keyword
; line itself sits back at the `if`'s level).
;
; **Heredocs are the hazard here**, and they are handled outside this
; file: the engine refuses to answer inside a string scope at all, so a
; `<<EOF` body keeps whatever indentation the user typed rather than
; being given the enclosing block's. Indentation inside a heredoc is
; DATA — silently reindenting it changes what the script emits, and for
; `<<-EOF` (tab-stripping) it can change whether the terminator is
; recognised at all.

[
  (do_group)
  (if_statement)
  (elif_clause)
  (else_clause)
  (case_statement)
  (case_item)
  (compound_statement)
  (subshell)
  (function_definition)
] @indent

[
  "fi"
  "done"
  "esac"
  "elif"
  "else"
  "}"
  ")"
] @outdent
