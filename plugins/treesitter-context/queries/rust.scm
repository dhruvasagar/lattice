; TC.5 — Rust context scopes.
;
; A `@context` capture marks a node whose header should pin when it scrolls
; away. The header span is derived host-... no: guest-side from the node's
; `body` field, so a wrapped signature pins as many lines as it occupies (up to
; `context.multiline-threshold`) without needing a second capture.
;
; Branch arms (`if` / `else` / `match`) are included deliberately: knowing which
; branch you are inside is exactly what a long function makes hard to see, and
; it is the case folds cannot serve because nobody folds an `if`.
(function_item) @context
(impl_item) @context
(trait_item) @context
(struct_item) @context
(enum_item) @context
(union_item) @context
(mod_item) @context
(macro_definition) @context
(if_expression) @context
(else_clause) @context
(match_expression) @context
(match_arm) @context
(for_expression) @context
(while_expression) @context
(loop_expression) @context
(closure_expression) @context
