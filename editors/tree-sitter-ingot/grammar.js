// Tree-sitter grammar for Ingot, derived from docs/grammar.md in the compiler repo.
// MVP scope: highlighting-grade parse of the MVP language. Diagnostics stay with the compiler.

const commaSep = (rule) => seq(rule, repeat(seq(",", rule)), optional(","));

module.exports = grammar({
  name: "ingot",

  word: ($) => $.identifier,

  conflicts: ($) => [
    [$.path_type, $._primary],
    [$.builtin_call],
    [$._primary],
    [$.path_type],
    [$.expression],
    [$._type],
    [$.function_bound],
    [$.generic_parameter, $.path_type],
    [$._type, $._primary],
  ],

  extras: ($) => [/\s/, $.comment],

  rules: {
    module: ($) => repeat(choice($.import_declaration, $._item)),

    // ---- comments & literals ----
    comment: (_) => token(seq("//", /[^\n]*/)),

    string: ($) =>
      seq('"', repeat(choice($.escape_sequence, /[^"\\\n]/)), '"'),

    cstring: ($) =>
      seq('c"', repeat(choice($.escape_sequence, /[^"\\\n]/)), '"'),

    char_literal: (_) => /'([^'\\\n]|\\[^\n])'/,

    escape_sequence: (_) => /\\(x[0-9A-Fa-f]{2}|[^\n])/,

    integer: (_) =>
      token(
        choice(
          /0x[0-9A-Fa-f][0-9A-Fa-f_]*/,
          /0o[0-7][0-7_]*/,
          /0b[01][01_]*/,
          /[0-9][0-9_]*/
        )
      ),

    float: (_) =>
      token(
        choice(
          /[0-9][0-9_]*\.[0-9][0-9_]*([eE][+-]?[0-9][0-9_]*)?/,
          /[0-9][0-9_]*[eE][+-]?[0-9][0-9_]*/
        )
      ),

    boolean: (_) => choice("true", "false"),

    identifier: (_) => /[a-z_][A-Za-z0-9_]*/,

    type_identifier: (_) => /[A-Z][A-Za-z0-9_]*/,

    builtin: (_) => /@[a-z_][A-Za-z0-9_]*/,

    scalar_type: (_) =>
      choice(
        "i8", "i16", "i32", "i64",
        "u8", "u16", "u32", "u64",
        "f32", "f64",
        "bool", "void", "never"
      ),

    vector_type: (_) =>
      token(
        prec(1,
        seq(
          choice(
            "i8", "i16", "i32", "i64",
            "u8", "u16", "u32", "u64",
            "f32", "f64", "bool"
          ),
          "x",
          /[0-9]+/
        )
        )
      ),

    // ---- imports ----
    import_declaration: ($) => seq("import", $.import_path, ";"),

    import_path: ($) =>
      seq(
        $.identifier,
        repeat(seq(".", $.identifier)),
        optional(seq(".", $.type_identifier)),
        optional(seq("as", choice($.identifier, $.type_identifier)))
      ),

    // ---- items ----
    _item: ($) =>
      choice(
        seq(
          optional("pub"),
          choice(
            $.fn_decl,
            $.struct_decl,
            $.enum_decl,
            $.interface_decl,
            $.const_decl,
            $.static_decl,
            $.type_decl
          )
        ),
        $.impl_decl,
        $.extern_block,
        $.test_decl,
        $.bench_decl,
        $.when_items
      ),

    fn_modifier: (_) =>
      choice("inline", "noinline", "unsafe", "export", "kernel", "waitable"),

    fn_decl: ($) =>
      seq(
        repeat($.fn_modifier),
        "fn",
        field("name", $.identifier),
        optional($.generic_parameters),
        "(",
        optional($.parameters),
        ")",
        optional($.result),
        choice($.block, ";")
      ),

    parameters: ($) => commaSep($.parameter),

    parameter: ($) =>
      seq(
        field("name", $.identifier),
        ":",
        field("type", $._type),
        optional(seq("into", $.identifier))
      ),

    result: ($) =>
      seq(
        "->",
        $._type,
        optional(seq("!", $._type)),
        optional(seq("from", $.origin, repeat(seq(",", $.origin)))),
        optional(seq("reduce", "(", $.reduce_op, ")"))
      ),

    origin: ($) => choice(seq($.identifier, optional(".*")), "static"),

    reduce_op: ($) => choice("+", "*", "&", "|", "^", $.identifier),

    generic_parameters: ($) =>
      seq("[", $.generic_parameter, repeat(seq(",", $.generic_parameter)), optional(","), "]"),

    generic_parameter: ($) =>
      choice(
        seq(
          $.type_identifier,
          optional(seq(":", $.bound, repeat(seq("+", $.bound))))
        ),
        seq("const", $.type_identifier, ":", $._type)
      ),

    bound: ($) => choice($.path_type, $.function_bound),

    function_bound: ($) =>
      seq(
        choice("Fn", "FnMut", "FnOnce"),
        "(",
        optional(commaSep($._type)),
        ")",
        optional(seq("->", $._type, optional(seq("!", $._type))))
      ),

    struct_decl: ($) =>
      seq(
        optional(choice("copy", "linear")),
        "struct",
        field("name", $.type_identifier),
        optional($.generic_parameters),
        optional($.alignment),
        "{",
        optional(commaSep($.field)),
        "}"
      ),

    field: ($) =>
      seq(optional("pub"), $.identifier, ":", $._type, optional($.alignment)),

    alignment: ($) => seq("align", "(", $._expression, ")"),

    enum_decl: ($) =>
      seq(
        optional("copy"),
        "enum",
        field("name", $.type_identifier),
        optional($.generic_parameters),
        optional(seq(":", $._type)),
        "{",
        optional(commaSep($.variant)),
        "}"
      ),

    variant: ($) =>
      seq(
        field("name", $.identifier),
        optional(seq(":", optional("from"), $._type)),
        optional(seq("=", $._expression))
      ),

    interface_decl: ($) =>
      seq(
        "interface",
        field("name", $.type_identifier),
        optional($.generic_parameters),
        optional(seq(":", $.bound, repeat(seq("+", $.bound)))),
        "{",
        repeat($.interface_member),
        "}"
      ),

    interface_member: ($) => choice(seq("type", $.type_identifier, ";"), $.fn_decl),

    impl_decl: ($) =>
      seq(
        optional("unsafe"),
        "impl",
        optional($.generic_parameters),
        choice($._type, seq($.path_type, "for", $._type)),
        "{",
        repeat(seq(optional("pub"), choice($.fn_decl, $.const_decl, $.type_decl))),
        "}"
      ),

    const_decl: ($) =>
      seq("const", field("name", $.identifier), ":", $._type, "=", $._expression, ";"),

    static_decl: ($) =>
      seq(
        "static",
        field("name", $.identifier),
        ":",
        $._type,
        optional($.alignment),
        "=",
        $._expression,
        ";"
      ),

    type_decl: ($) =>
      seq("type", field("name", $.type_identifier), optional($.generic_parameters), "=", $._type, ";"),

    extern_block: ($) =>
      seq("extern", $.string, "{", repeat($.extern_function), "}"),

    extern_function: ($) =>
      seq(
        "fn",
        field("name", $.identifier),
        "(",
        optional(
          choice(
            seq($.parameter, repeat(seq(",", $.parameter)), optional(seq(",", "..."))),
            "..."
          )
        ),
        ")",
        optional(seq("->", $._type)),
        ";"
      ),

    test_decl: ($) => seq("test", field("name", $.string), $.block),

    bench_decl: ($) => seq("bench", field("name", $.string), $.block),

    when_items: ($) =>
      seq(
        "when",
        $._expression,
        "{",
        repeat($._item),
        "}",
        optional(
          seq("else", choice(seq("{", repeat($._item), "}"), $.when_items))
        )
      ),

    // ---- types ----
    _type: ($) =>
      choice(
        $.scalar_type,
        $.vector_type,
        $.path_type,
        seq("&", optional("mut"), choice($._type, seq("dyn", $.bound))),
        seq("[", "]", optional("mut"), $._type),
        seq("[", $._expression, "]", $._type),
        seq("*", optional("mut"), $._type),
        seq("?", $._type),
        seq("(", $._type, ",", commaSep($._type), ")"),
        seq(
          optional("extern"),
          "fn",
          "(",
          optional(commaSep($._type)),
          ")",
          optional(seq("->", $._type, optional(seq("!", $._type))))
        )
      ),

    path_type: ($) =>
      seq(
        repeat(seq($.identifier, ".")),
        $.type_identifier,
        optional($.generic_arguments),
        optional(seq(".", $.type_identifier))
      ),

    generic_arguments: ($) => seq("[", $.generic_argument, repeat(seq(",", $.generic_argument)), "]"),

    generic_argument: ($) =>
      choice($._type, $.integer, $.boolean, seq("{", $._expression, "}")),

    // ---- statements ----
    block: ($) => seq("{", repeat($._statement), "}"),

    _statement: ($) =>
      choice(
        $.variable_statement,
        $.assignment_statement,
        $.expression_statement,
        $.if_statement,
        $.while_statement,
        $.loop_statement,
        $.for_statement,
        $.return_statement,
        $.fail_statement,
        $.break_statement,
        $.continue_statement,
        $.defer_statement,
        $.scope_statement,
        $.select_statement,
        $.unsafe_statement,
        $.when_statement,
        $.block
      ),

    variable_statement: ($) =>
      seq(choice("let", "var"), $._pattern, optional(seq(":", $._type)), "=", $._expression, ";"),

    assign_op: (_) =>
      choice(
        "=", "+=", "-=", "*=", "/=", "%=",
        "+%=", "-%=", "*%=",
        "+|=", "-|=", "*|=",
        "&=", "|=", "^=", "<<=", ">>="
      ),

    assignment_statement: ($) =>
      seq(choice($.postfix_expression, "_"), $.assign_op, $._expression, ";"),

    expression_statement: ($) => seq($._expression, ";"),

    if_statement: ($) =>
      seq(
        "if",
        $.condition,
        $.block,
        optional(seq("else", choice($.block, $.if_statement)))
      ),

    condition: ($) => choice($.let_condition, $._expression),

    let_condition: ($) => seq("let", $._pattern, "=", $._expression),

    while_statement: ($) =>
      seq(optional(seq(field("label", $.identifier), ":")), "while", $.condition, $.block),

    loop_statement: ($) =>
      seq(optional(seq(field("label", $.identifier), ":")), "loop", $.block),

    for_statement: ($) =>
      seq(
        optional(seq(field("label", $.identifier), ":")),
        optional(
          seq("par", optional(seq("(", "grain", ":", $._expression, ")")))
        ),
        "for",
        $.identifier,
        optional(seq(",", $.identifier)),
        "in",
        $._expression,
        optional(
          seq(choice("..", "..="), $._expression, optional(seq("step", $._expression)))
        ),
        $.block
      ),

    return_statement: ($) => seq("return", optional($._expression), ";"),

    fail_statement: ($) => seq("fail", $._expression, ";"),

    break_statement: ($) => seq("break", optional($.identifier), ";"),

    continue_statement: ($) => seq("continue", optional($.identifier), ";"),

    defer_statement: ($) => seq(choice("defer", "errdefer"), $.block),

    scope_statement: ($) => seq("scope", $.identifier, $.block),

    select_statement: ($) =>
      seq(
        "select",
        "{",
        repeat($.select_arm),
        optional(seq("else", "=>", $.block)),
        "}"
      ),

    select_arm: ($) =>
      seq($._pattern, "=", $.call_expression, "=>", $.block),

    unsafe_statement: ($) => seq("unsafe", "(", $.string, ")", $.block),

    when_statement: ($) =>
      seq(
        "when",
        $._expression,
        $.block,
        optional(seq("else", choice($.block, $.when_statement)))
      ),

    // ---- match (statement and expression share arms) ----
    match_arm: ($) =>
      seq(
        choice(
          seq(
            $._pattern,
            repeat(seq("|", $._pattern)),
            optional(seq("if", $._expression))
          ),
          "else"
        ),
        "=>",
        choice($.block, seq($._expression, ","))
      ),

    // ---- patterns ----
    _pattern: ($) =>
      choice(
        "_",
        $.identifier,
        $.literal,
        seq($.literal, choice("..", "..="), $.literal),
        seq(optional($.path_type), ".", $.identifier, optional(seq("(", $._pattern, ")"))),
        seq("(", $._pattern, ",", commaSep($._pattern), ")"),
        $.struct_pattern
      ),

    field_pattern: ($) => seq($.identifier, optional(seq(":", $._pattern))),

    struct_pattern: ($) =>
      seq($.path_type, ".{", optional(commaSep($.field_pattern)), "}"),

    // ---- expressions ----
    _expression: ($) => $.expression,

    expression: ($) =>
      seq(
        $.or_expression,
        optional(
          choice(
            seq("orelse", $.handler),
            seq("catch", choice($.identifier, "_"), "=>", $.handler)
          )
        )
      ),

    handler: ($) => choice($.block, $.or_expression),

    or_expression: ($) =>
      prec.left(seq($.and_expression, repeat(seq("||", $.and_expression)))),

    and_expression: ($) =>
      prec.left(seq($.cmp_expression, repeat(seq("&&", $.cmp_expression)))),

    cmp_expression: ($) =>
      prec.left(seq(
        $.bit_expression,
        optional(seq(choice("==", "!=", "<", "<=", ">", ">="), $.bit_expression))
      )),

    bit_expression: ($) =>
      prec.left(seq($.shift_expression, repeat(seq(choice("&", "|", "^"), $.shift_expression)))),

    shift_expression: ($) =>
      prec.left(seq($.add_expression, repeat(seq(choice("<<", ">>"), $.add_expression)))),

    add_expression: ($) =>
      prec.left(seq(
        $.mul_expression,
        repeat(seq(choice("+", "-", "+%", "-%", "+|", "-|"), $.mul_expression))
      )),

    mul_expression: ($) =>
      prec.left(seq(
        $.cast_expression,
        repeat(seq(choice("*", "/", "%", "*%", "*|"), $.cast_expression))
      )),

    cast_expression: ($) => prec.left(seq($.prefix_expression, repeat(seq("as", $._type)))),

    prefix_expression: ($) =>
      choice(
        seq(
          choice("-", "-%", "!", "~", seq("&", optional("mut")), "try"),
          $.prefix_expression
        ),
        $.postfix_expression
      ),

    postfix_expression: ($) =>
      prec.left(seq(
        $._primary,
        repeat(
          choice(
            seq("(", optional($.arguments), ")"),
            seq("[", $._expression, optional(seq("..", optional($._expression))), "]"),
            seq(".", $.identifier),
            seq(".", $.integer),
            ".*",
            $.explicit_type_arguments
          )
        )
      )),

    call_expression: ($) => $.postfix_expression,

    explicit_type_arguments: ($) => seq(".[", commaSep($._type), "]"),

    arguments: ($) => commaSep($._expression),

    builtin_arguments: ($) =>
      seq(choice($._type, $._expression), repeat(seq(",", $._expression))),

    builtin_call: ($) =>
      seq($.builtin, optional(seq("(", optional($.builtin_arguments), ")"))),

    _primary: ($) =>
      choice(
        $.identifier,
        $.literal,
        $.builtin_call,
        $.path_type,
        seq("(", $._expression, ")"),
        seq("(", $._expression, ",", commaSep($._expression), ")"),
        $.struct_literal,
        $.array_literal,
        $.expected_type_literal,
        seq(".", $.identifier, optional(seq("(", optional($.arguments), ")"))),
        $.closure_expression,
        $.if_expression,
        $.match_expression,
        seq("spawn", $.call_expression, "in", $._expression),
        seq("dispatch", $.call_expression, "over", $.domain, "on", $._expression),
        seq("unsafe", "(", $.string, ")", "{", $._expression, "}")
      ),

    struct_literal: ($) =>
      seq($.path_type, ".{", optional($.field_inits), "}"),

    array_literal: ($) =>
      seq(
        "[",
        $._expression,
        "]",
        $._type,
        ".{",
        optional(commaSep($._expression)),
        "}"
      ),

    expected_type_literal: ($) => seq(".{", optional($.field_inits), "}"),

    field_inits: ($) => commaSep($.field_init),

    field_init: ($) => seq($.identifier, ":", $._expression),

    capture: ($) => seq(optional(seq("&", optional("mut"))), $.identifier),

    closure_expression: ($) =>
      seq(
        "fn",
        "[",
        optional(commaSep($.capture)),
        "]",
        "(",
        optional($.parameters),
        ")",
        optional($.result),
        $.block
      ),

    if_expression: ($) =>
      seq(
        "if",
        $.condition,
        "{",
        $._expression,
        "}",
        "else",
        choice(seq("{", $._expression, "}"), $.if_expression)
      ),

    match_expression: ($) =>
      seq("match", $._expression, "{", repeat($.match_arm), "}"),

    domain: ($) =>
      seq(
        $.or_expression,
        "..",
        $.or_expression,
        repeat(seq(",", $.or_expression, "..", $.or_expression))
      ),

    literal: ($) =>
      choice($.integer, $.float, $.string, $.cstring, $.char_literal, $.boolean),
  },
});
