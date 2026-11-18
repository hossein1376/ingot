(function_item
  (visibility)? @context
  "fn" @context
  name: (identifier) @name
  (generic_parameters)? @context) @item

(extern_function
  "fn" @context
  name: (identifier) @name) @item

(struct_item
  (visibility)? @context
  "struct" @context
  name: (type_identifier) @name
  (generic_parameters)? @context) @item

(enum_item
  (visibility)? @context
  "enum" @context
  name: (type_identifier) @name
  (generic_parameters)? @context) @item

(interface_item
  (visibility)? @context
  "interface" @context
  name: (type_identifier) @name
  (generic_parameters)? @context) @item

(impl_item
  "impl" @context
  (generic_parameters)? @context
  interface: (_)? @name
  "for"? @context
  type: (_) @name) @item

(const_item
  (visibility)? @context
  "const" @context
  name: (identifier) @name) @item

(static_item
  (visibility)? @context
  "static" @context
  name: (identifier) @name) @item

(type_item
  (visibility)? @context
  "type" @context
  name: (type_identifier) @name
  (generic_parameters)? @context) @item

(associated_type
  "type" @context
  name: (type_identifier) @name) @item

(test_item
  "test" @context
  name: (string) @name) @item

(bench_item
  "bench" @context
  name: (string) @name) @item
