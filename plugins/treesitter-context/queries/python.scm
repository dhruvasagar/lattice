; TC.5 -- Python context scopes.
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
(function_definition body: (_) @context.end) @context
(class_definition body: (_) @context.end) @context
(if_statement consequence: (_) @context.end) @context
(elif_clause consequence: (_) @context.end) @context
(else_clause body: (_) @context.end) @context
(for_statement body: (_) @context.end) @context
(while_statement body: (_) @context.end) @context
(with_statement body: (_) @context.end) @context
(try_statement body: (_) @context.end) @context
(except_clause) @context
(match_statement body: (_) @context.end) @context
(case_clause consequence: (_) @context.end) @context
