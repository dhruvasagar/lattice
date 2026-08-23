; Org fold ranges.
;
; The same pipeline markdown's `(section)` fold uses — `compute_syntax_folds`
; resolves `folds.scm` by language name through the live registry and knows
; nothing about where the grammar came from — so this is queries and tests
; rather than mechanism.
;
; `(section)` is the load-bearing one: in this grammar a section is a headline
; plus everything beneath it *including nested sections*, so folding a headline
; hides its whole subtree, which is what org users mean by folding. The nested
; sections are separate `(section)` nodes and fold independently, giving the
; per-level cycling behaviour for free.
;
; Single-line captures are dropped by the pipeline (nothing to hide), so a
; childless headline or a one-line block simply is not foldable — no special
; case needed here.

[
  (section)

  ; `#+BEGIN_SRC` … `#+END_SRC` and friends. `dynamic_block` is
  ; `#+BEGIN:` … `#+END:`, a different node in this grammar.
  (block)
  (dynamic_block)

  ; `:PROPERTIES:` … `:END:` and hand-rolled `:DRAWER:` blocks. Both are
  ; noise most of the time, which is exactly what folding is for.
  (drawer)
  (property_drawer)

  ; Multi-line lists and tables, matching markdown's `(list)`.
  (list)
  (table)

  ; `\begin{...}` … `\end{...}`.
  (latex_env)
] @fold
