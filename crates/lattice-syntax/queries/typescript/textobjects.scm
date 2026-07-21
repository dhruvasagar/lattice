; Tree-sitter text-object ranges for TypeScript (tree-sitter-typescript v0.23.2).

; Functions: declarations, arrows, and methods.
(function_declaration) @function.outer
(arrow_function) @function.outer
(method_definition) @function.outer

; Class-shaped definitions.
(class_declaration) @class.outer
(interface_declaration) @class.outer
(enum_declaration) @class.outer

; Blocks / scopes.
(statement_block) @block.outer
(if_statement) @block.outer
(for_statement) @block.outer
(for_in_statement) @block.outer
(while_statement) @block.outer
(do_statement) @block.outer
(switch_statement) @block.outer
(try_statement) @block.outer

; Comments.
(comment) @comment.outer
(comment) @comment.inner

; Parameters.
(required_parameter) @parameter.outer
(required_parameter) @parameter.inner
(optional_parameter) @parameter.outer
(optional_parameter) @parameter.inner

; Inner bodies.

; Function bodies.
(function_declaration body: (statement_block) @function.inner)
(arrow_function body: (_) @function.inner)
(method_definition body: (statement_block) @function.inner)

; Type bodies.
(class_declaration body: (class_body) @class.inner)
(interface_declaration body: (interface_body) @class.inner)
(enum_declaration body: (enum_body) @class.inner)

; Blocks.
(if_statement consequence: (statement_block) @block.inner)
(for_statement body: (statement_block) @block.inner)
(for_in_statement body: (statement_block) @block.inner)
(while_statement body: (statement_block) @block.inner)
(do_statement body: (statement_block) @block.inner)
(try_statement body: (statement_block) @block.inner)
