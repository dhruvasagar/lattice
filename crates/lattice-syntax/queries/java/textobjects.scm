(method_declaration) @function.outer
(method_declaration
  body: (block) @function.inner)
(constructor_declaration) @function.outer
(constructor_declaration
  body: (constructor_body) @function.inner)
(class_declaration) @class.outer
(class_declaration
  body: (class_body) @class.inner)
(interface_declaration) @class.outer
(interface_declaration
  body: (interface_body) @class.inner)
(enum_declaration) @class.outer
(enum_declaration
  body: (enum_body) @class.inner)
(if_statement) @block.outer
(if_statement
  consequence: (statement) @block.inner)
(for_statement) @block.outer
(for_statement
  body: (block) @block.inner)
(while_statement) @block.outer
(while_statement
  body: (block) @block.inner)
(try_statement) @block.outer
(try_statement
  body: (block) @block.inner)
([
  (line_comment)
  (block_comment)
] @comment.outer)

([
  (line_comment)
  (block_comment)
] @comment.inner)
(formal_parameter) @parameter.outer
(formal_parameter) @parameter.inner
