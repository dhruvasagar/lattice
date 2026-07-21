; Tree-sitter fold ranges for SQL (tree-sitter-sequel v0.3.x).
; Node types adapted to the sequel grammar's actual CST nodes.

[
  (select)
  (insert)
  (update)
  (delete)
  (create_table)
  (create_view)
  (create_function)
  (create_trigger)
  (block)
  (case)
] @fold
