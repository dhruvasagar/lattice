; TC.5 -- Rust context scopes.
;
; A `@context` capture marks a node whose header should pin when it scrolls
; away. Its `@context.end` capture marks where the BODY begins, so the header
; runs from the node's first line to that line -- a wrapped signature pins as
; many lines as it occupies (up to `context.multiline-threshold`).
;
; `@context.end` is a query capture rather than a guest-side `body` field
; lookup because reading a field costs a node RESOURCE across the plugin
; boundary, one per scope, and the whole-file query is the plugin's only
; expensive call. A capture is a value. `tests/treesitter_context_queries.rs`
; compiles every query here against its real grammar, so a wrong field name
; fails a test instead of silently disabling the strip for one language.
;
; A pattern with no `@context.end` is deliberate: its header IS its first line
; (`} else {`, a `case` label, a markdown heading).
;
; Branch arms (`if` / `else` / `match`) are included deliberately: knowing which
; branch you are inside is exactly what a long function makes hard to see, and
; it is the case folds cannot serve because nobody folds an `if`.
(function_item body: (_) @context.end) @context
(impl_item body: (_) @context.end) @context
(trait_item body: (_) @context.end) @context
(struct_item body: (_) @context.end) @context
(enum_item body: (_) @context.end) @context
(union_item body: (_) @context.end) @context
(mod_item body: (_) @context.end) @context
(macro_definition) @context
(if_expression consequence: (_) @context.end) @context
(else_clause) @context
(match_expression body: (_) @context.end) @context
(match_arm value: (_) @context.end) @context
(for_expression body: (_) @context.end) @context
(while_expression body: (_) @context.end) @context
(loop_expression body: (_) @context.end) @context
(closure_expression body: (_) @context.end) @context
