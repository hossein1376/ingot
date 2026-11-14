# Diagnostic and trap catalog

Every condition under which a conforming compiler rejects a program has a code here, and
every way a running program can trap has one too. The specification says *that*
something is an error. This file fixes the code, the span and the fix.

## 1. Output contract

One JSON object per line on standard output, for `check`, `build`, `test` and `fix`:

```json
{"type":"diagnostic","code":"E1104","name":"move_while_borrowed","message":"`buf` is moved while it is borrowed",
 "span":{"file":"io.ingot","start":{"line":41,"col":18},"end":{"line":41,"col":21}},
 "related":[{"role":"borrow","span":{...}},{"role":"later_use","span":{...}}],
 "fixes":[{"message":"...","edits":[{"span":{...},"text":"..."}]}]}
```

* The line format shared by all commands, including the `type` field and the shape of
  positions, is in [tools.md](tools.md).
* `code` and `name` come from this file. `message` is free text and may change between
  compiler versions. Tools match on `code`.
* **Positions.** Lines and columns are 1-based. Columns count bytes. Spans are half-open.
  `file` is the path relative to the package root, with `/` separators on every system.
* **Order.** Diagnostics are sorted by file path (byte order), then by the start of the
  primary span, then by code. The order never depends on how the compiler was scheduled.
* **Completeness.** All independent errors of a run are reported. An error whose only
  cause is an earlier error is not (see poisoning in [pipeline.md](pipeline.md)).
* **Exit status** of the compiler: 0 with no diagnostics, 1 with diagnostics, 2 for a
  failure of the compiler itself.

### Related roles

`related` is a list of `{role, span}`. The roles are a closed set:

| Role | Points at |
|---|---|
| `declared` | where the variable, type or item was declared |
| `previous` | an earlier definition, import or arm that this one collides with |
| `expected` | what fixed the expected type: an annotation, a parameter, a return type |
| `borrow` | the borrow expression that created a loan |
| `move` | where a value was moved |
| `later_use` | a later use that keeps a loan live, or a use after a move |
| `scope_end` | the exit at which a value or place ends |
| `defer` | a deferred block that consumes or uses the value |
| `signature` | the clause of a signature that the body or the call disagrees with |
| `callee` | the declaration of the function being called |
| `spawn` | the `spawn` that gave a borrow to a task |
| `loop` | the loop whose next iteration reaches the error |
| `instantiation` | where a generic was instantiated, innermost first |
| `candidate` | one of several things the author may have meant |

### Fixes

* A fix is a list of edits. An edit replaces a span with text. An empty span inserts.
* A fix must make its diagnostic disappear and must not change the meaning of any code
  that was already accepted.
* A fix is offered only when exactly one edit is right. Where the author has to choose
  between behaviors (for example how a lossy conversion should treat values that do not
  fit), there is no fix, and the candidates are listed in `message` and `related`.
* An inserted statement goes on its own line, indented like the line it follows.
* An inserted member of an `impl` or `interface` goes on its own lines, indented one
  level (four spaces) deeper than the line of the opening brace, after the last member.
  An empty `{}` is opened so that the braces end up on their own lines. A body of
  `@panic("todo")` is written as the block `{ @panic("todo"); }` laid out the same way.
* `ingot fix` applies the first fix of each diagnostic that does not overlap a fix
  already taken in this round, re-checks, and repeats until a round applies nothing.

In the tables, "—" in the Fix column means no fix is offered.

## 2. Source text and tokens (E01xx)

A file with any E01xx or E02xx error is not analyzed further. Other files still are.

| Code | Name | Reported when | Primary span | Fix |
|---|---|---|---|---|
| E0101 | `invalid_utf8` | the file is not valid UTF-8 | the first bad byte | — |
| E0102 | `carriage_return` | a CR byte occurs anywhere in the file | the CR | delete it |
| E0103 | `non_ascii` | a non-ASCII character occurs outside a string literal or comment | the character | — |
| E0104 | `bad_identifier` | a name fits neither class: lowercase start with an uppercase letter in it, or uppercase start with `_` in it. For an all-capitals name with `_` (`MAX_LEN`) the message says that constants are lower_snake | the name | for an all-capitals name, the lower_snake spelling |
| E0105 | `bad_number` | malformed number: `1.`, `.5`, `0x`, a misplaced `_`, an integer that fits no integer type | the literal | — |
| E0106 | `bad_escape` | an escape other than `\n \t \r \0 \\ \" \' \xHH` | the escape | — |
| E0107 | `unterminated_literal` | a string or character literal is not closed on its line | the opening quote | — |
| E0108 | `bad_char_literal` | a character literal is not exactly one byte | the literal | — |
| E0109 | `unknown_builtin` | `@name` is not a builtin | the name | replace with the closest builtin if one is within edit distance 2 |
| E0110 | `block_comment` | `/*` | the `/*` | — |
| E0111 | `unknown_token` | a character that starts no token | the character | — |

## 3. Syntax (E02xx)

| Code | Name | Reported when | Primary span | Fix |
|---|---|---|---|---|
| E0201 | `unexpected_token` | the grammar allows no continuation with this token | the token | — |
| E0202 | `missing_semicolon` | a statement or item ends without `;` and the next token starts a new one | the empty span after the statement | insert `;` |
| E0203 | `missing_braces` | a body of `if`, `else`, `while`, `for`, `loop`, `defer`, an arm or a function is not a block | the body's first token | — |
| E0204 | `mixed_operators` | a bitwise operator or shift is mixed with another operator class without parentheses (§5.1) | the inner operator's operands and operator | parenthesize the sub-expression that binds tighter in the precedence table |
| E0205 | `chained_comparison` | `a < b < c` | the second comparison operator | — |
| E0206 | `not_a_statement` | an expression statement that is not a call, `spawn` or `dispatch`, optionally under `try` or followed by `catch` | the expression | — |
| E0207 | `block_has_no_value` | a branch of an expression-form `if`, `match` or `unsafe` holds anything but one expression, other than a block that cannot reach its end (one that ends in `return`, `fail`, `break`, `continue` or a trap) | the branch | — |
| E0208 | `missing_capture_list` | a closure written without `[...]` after `fn` | the `fn` | insert `[]` |
| E0209 | `struct_literal_syntax` | a type followed directly by `{` in expression position | the `{` | insert `.` before it |
| E0210 | `item_not_allowed_here` | an item in the wrong place: `import` after an item, an item inside a function body, `pub` on `impl`, `test`, `bench` or `extern`, or on a member of `impl I for T` (its visibility is the interface's) | the `pub` | delete it |
| E0211 | `type_args_syntax` | `name[Type](...)` on a value identifier whose brackets hold only types | the `[` | insert `.` before it |
| E0212 | `keyword_as_name` | a keyword where a name is required | the keyword | — |
| E0213 | `bad_modifier` | a repeated, conflicting or misplaced modifier: `inline noinline`, `copy linear`, `kernel` with `export` | the second modifier | — |

## 4. Names, modules and visibility (E03xx)

| Code | Name | Reported when | Primary span | Fix |
|---|---|---|---|---|
| E0301 | `unresolved_import` | an import path names no module and no public item | the first segment that fails | — |
| E0302 | `duplicate_import_name` | two imports bind the same name | the second binding (`previous`: the first) | — |
| E0303 | `package_cycle` | packages depend on each other in a cycle | the import that closes it | — |
| E0304 | `undefined_name` | a value identifier resolves to nothing | the name | if exactly one imported module exports the name, qualify it; else if one name in scope is within edit distance 2, replace |
| E0305 | `undefined_type` | a type identifier resolves to nothing | the name | as E0304 |
| E0306 | `private_item` | a non-`pub` item is named from another module | the name (`declared`) | — |
| E0307 | `private_field` | a non-`pub` field is read, written or matched from another module; a struct pattern ending in `..` matches every field it leaves out, so it cannot take apart a struct with a private field there (§4.2) | the field name (`declared`) | — |
| E0308 | `duplicate_definition` | two items, fields, variants, parameters, generic parameters or labels with one name in one scope, or an item and an import of one name in one module. The fields, variants, methods, associated functions and associated constants of one type share one namespace. Function overloading is this error | the second, or the item (`previous`: the import) | — |
| E0309 | `shadowing` | a local or pattern binding has the name of a local or parameter in scope, or of a `const` or `static` visible in its module | the new binding (`previous`) | — |
| E0310 | `unused_import` | an imported name is never used. A use inside any branch of a `when`, taken on this target or not, counts | the import | delete the import line |
| E0311 | `unused_binding` | a `let`, `var` or pattern binding, a `for` variable or the error name of `catch e` is never read. Assigning to the whole variable (`x = ...`) is not a read; a compound assignment or an assignment to a part is. Parameters, captures and scope names are exempt | the name | — |
| E0312 | `var_never_mutated` | a `var` is never assigned and never exclusively borrowed after its initialization | the `var` keyword | replace with `let` |
| E0313 | `ambiguous_method` | `value.name(...)` matches a method in two interfaces and none in `impl T` | the method name (`candidate` each) | — |
| E0314 | `unknown_member` | no field, variant, method or associated item of that name on the type | the name | replace if one member is within edit distance 2 |
| E0315 | `private_construction` | a struct literal, `@zero` or `@bits` would make a value of a type with a field that is not visible here, or of a fieldless struct of another module (§4.2) | the expression (`declared`) | — |
| E0316 | `ambiguous_import` | the last segment of an import names both a module and an item | the segment | — |
| E0317 | `undefined_label` | `break`/`continue` names a label that encloses nothing here | the label | — |

## 5. Types and conversions (E04xx)

| Code | Name | Reported when | Primary span | Fix |
|---|---|---|---|---|
| E0401 | `type_mismatch` | an expression of type `U` occurs where `T` is expected and no more specific code below applies, including a closure where a `fn` type is expected | the expression (`expected`) | append ` as T` if that conversion is lossless (§5.4), parenthesizing the expression if it binds looser than `as` |
| E0402 | `lossy_as` | `x as T` where some value of the source type is not a value of `T` | the `as T` (`candidate`: none) | — (the message lists `@wrap`, `@narrow`, `@sat`, `@nearest` as they apply) |
| E0403 | `condition_not_bool` | a condition of `if`, `while`, `@assert`, a guard, `&&`, `\|\|` or `!` is not `bool` | the expression | for an integer, append ` != 0` |
| E0404 | `cannot_infer_type` | a `let` without annotation whose initializer has no type of its own: `.name`, `.{...}`, `@zero`, an unsuffixed use that needs context | the initializer | — |
| E0405 | `literal_out_of_range` | a numeric literal does not fit the type its context gives it; where the expected type is a type parameter, a literal that is not a value of every type its numeric bound allows (§3) | the literal | — |
| E0406 | `invalid_operand` | an operator is not defined for its operand type: arithmetic on `bool`, `%` on floats, unary `-` on unsigned, `==` or `<` on a struct or payload enum, differing operand types | the operator | for `==` on a type implementing `Eq`, rewrite to `a.eq(&b)` |
| E0407 | `int_vector_plain_arith` | `+ - *` on integer vectors (§26.2) | the operator | — |
| E0408 | `bad_bits` | `@bits(T, x)` with different sizes (E1503 instead when either type is a vector or a mask), padding in the source, a borrow in either type, or a `T` with invalid bit patterns | the builtin call | — |
| E0409 | `bad_zero` | `@zero` for a type that does not allow it (§7) | the `@zero` | — |
| E0410 | `wrong_argument_count` | a call, variant constructor or builtin with the wrong number of arguments, including a variant with a payload named without one (`T.variant` as a value) | the argument list (`callee`) | — |
| E0411 | `not_callable` | a call on something that is not a function, closure, variant constructor or `fn` value | the callee expression | — |
| E0412 | `missing_return` | a path through a function with a result reaches the end of the body | the closing brace | — |
| E0413 | `unused_result` | a call statement whose value is not `void` (and not a result, which is E0701) | the call | insert `_ = ` before it if the type is `Discard` |
| E0414 | `discard_not_allowed` | `_ = expr`, or a `_` pattern (in a `match`, `let` or `catch _`), where the discarded type is linear | the `_` | — |
| E0415 | `index_not_i64` | an index or slice bound is not `i64` | the index expression | append ` as i64` if lossless |
| E0416 | `not_indexable` | `x[...]` on something that is not an array, slice, vector or borrow of one | the `[` | — |
| E0417 | `bad_enum_conversion` | `as` on an enum with payloads, or `@enum(E, x)` where `E` has payloads or `x` is not `E`'s tag type | the conversion | — |
| E0418 | `raw_pointer_member` | `p.name` or `p[i]` on a raw pointer | the `.` or `[` | insert `.*` before it |
| E0419 | `needs_shared_reborrow` | an exclusive borrow occurs where the shared borrow of the same type is expected | the expression | rewrite `r` to `&r.*`, or append `.shared()` for a slice |
| E0420 | `array_slice_needs_borrow` | `a[lo..hi]` on an array place without `&` or `&mut` | the expression | insert `&` before it |
| E0421 | `value_in_void_return` | `return expr;` in a function without a result, or `return;` in one with a result | the `return` | — |
| E0422 | `bad_format_string` | a format string of `@format` or `@debug` (§35) with a malformed placeholder, a spec the argument's type does not support, or a placeholder count that differs from the argument count | the placeholder inside the literal, or the first extra argument | — |

## 6. Declarations (E05xx)

| Code | Name | Reported when | Primary span | Fix |
|---|---|---|---|---|
| E0501 | `copy_needs_copy_parts` | a `copy struct` or `copy enum` has a field or payload that is not `Copy` | the field's type | — |
| E0502 | `missing_linear` | a struct has a linear field and is not declared `linear` | the `struct` keyword | insert `linear ` before it |
| E0503 | `struct_literal_fields` | a struct literal omits a field, repeats one or names one that does not exist | the literal's braces, or the bad name | — |
| E0504 | `duplicate_from_variant` | two variants of one enum are `from` the same type | the second (`previous`) | — |
| E0505 | `bad_enum_tag` | an explicit tag value without a tag type, a value that does not fit, or two variants with one value | the value | — |
| E0506 | `impl_wrong_module` | `impl I for T` in a module that declares neither `I` nor `T`, or an inherent `impl T` outside the module that declares `T` (for a built-in type, outside `std`) | the `impl` | — |
| E0507 | `duplicate_impl` | `I` is implemented for one type twice, including an `impl` of a built-in interface for a type the language already implements it for (§4.4) | the second (`previous`), or the `impl` | — |
| E0508 | `impl_for_type_parameter` | `impl I for T` or `impl T` where `T` is a bare type parameter | the type | — |
| E0509 | `impl_mismatch` | an implementation lacks a required function, adds a function or a constant the interface does not declare, or declares a signature that differs from the interface's, including a `from` or `into` clause the interface does not have | the function name, or the `impl` for a missing one (`signature`) | for a missing function, insert its signature with a body of `@panic("todo")` |
| E0510 | `not_dyn_capable` | `dyn I` where `I` declares an associated type, or a function of `I` has no `self`, takes `self` by value, has generic parameters, or mentions `Self` outside the receiver | the `dyn I` (`declared`: the function or associated type) | — |
| E0511 | `static_not_sync` | the type of a `static` is not `Sync` | the type | — |
| E0512 | `initializer_not_constant` | **retired**: a non-constant initializer is E0807 | — | — |
| E0513 | `static_borrows_non_static` | **retired**: at item level the only borrowable places are statics and literals; a borrow of a `const` is E0607 | — | — |
| E0514 | `bad_main` | the root module's `main` is missing `pub`, has parameters other than `env: &Env`, or returns something other than `void` or `u8`, optionally fallible | the signature | — |
| E0515 | `bad_align` | `align(n)` where `n` is not a constant power of two, is below the natural alignment, or is above 65536 | the argument | — |
| E0516 | `infinite_type` | a struct or enum contains itself by value | the field's type | — |
| E0517 | `bad_self_type` | a parameter named `self` whose type is not the implementing type, `&` of it or `&mut` of it; or `self` outside an `impl` or interface | the type | — |
| E0518 | `missing_body` | a function without a body outside an interface or `extern` block, or with one inside an `extern` block | the function name | — |
| E0519 | `bad_associated_type` | an implementation omits an associated type, or a path names one the interface does not declare | the name, or the `impl` for an omitted one (`signature`: the `type` line of the interface) | — |
| E0520 | `alias_cycle` | a type alias expands, directly or through other aliases, to a type that contains itself | the alias name | — |
| E0521 | `negative_length` | an array length is negative or above 2^62 | the length | — |
| E0522 | `bad_tag_type` | the tag type of an enum is not an integer type | the type | — |
| E0523 | `not_an_interface` | an `impl ... for`, a bound or a `dyn` names something that is not an interface or a built-in bound, or `Fn`, `FnMut` or `FnOnce` is written without a signature | the name | — |
| E0524 | `interface_as_type` | an interface or a bound is used as a type | the name | — |
| E0525 | `impl_not_allowed` | an `impl` of `Copy`, `Discard`, `Owned`, a numeric bound or an `Fn` bound (these are properties, not interfaces), or of `Executor` outside `std` | the interface name | — |

## 7. Statements, patterns and control flow (E06xx)

| Code | Name | Reported when | Primary span | Fix |
|---|---|---|---|---|
| E0601 | `non_exhaustive_match` | a `match` does not cover every value of its scrutinee | the `match` keyword | insert one arm per missing variant with a body of `@panic("todo")`; for other types insert an `else` arm |
| E0602 | `unreachable_arm` | an arm can never be chosen because earlier arms cover it | the arm's pattern (`previous`) | delete the arm |
| E0603 | `bad_pattern` | a pattern does not fit the type it is matched against, including a struct pattern that leaves out a field without ending in `..`; alternatives `p \| q` that do not bind the same names with the same types; a bare name that is a variant of the scrutinee's enum | the pattern, or the alternative that differs | for the bare variant name, insert `.` before it |
| E0604 | `refutable_pattern` | a pattern in `let`, `var` or a `for` binding can fail to match | the pattern | — |
| E0605 | `break_outside_loop` | `break` or `continue` with no enclosing loop | the keyword | — |
| E0606 | `assign_to_immutable` | assignment to a `let`, a parameter, or a place reached through a shared borrow | the place (`declared`) | for a `let`, change it to `var` |
| E0607 | `not_a_place` | assignment to, or a borrow of, something that is not a place | the expression | — |
| E0608 | `not_iterable` | `for x in e` where `e` is not a range, array, slice or `Iter`; or a shared slice whose element type is not `Copy` | the expression | for the non-`Copy` slice, append `.refs()` |
| E0609 | `bad_range` | range bounds of different types, a `step` that is not a positive constant, a non-integer range, or a second (counter) variable on a `for` over a range | the offending bound, or the second variable | — |
| E0610 | `mut_borrow_of_immutable` | `&mut x` of a `let`, a parameter or a `static`, or through a shared borrow | the borrow (`declared`) | for a `let`, change it to `var` |
| E0611 | `bad_capture` | a closure body uses a local that is not in its capture list, or the list names something that is not a local, or names one twice | the use, or the capture | for a missing capture, add `&name` to the list |
| E0612 | `when_not_constant` | a `when` condition uses anything but `@target`, literals, comparisons and `&& \|\| !` (§7.3) | the offending sub-expression | — |
| E0613 | `early_exit_of_consuming_loop` | a `for` over an array value of linear elements contains `break`, `return`, `fail` or a propagating `try` | the exit | — |

## 8. Errors and cleanup (E07xx)

| Code | Name | Reported when | Primary span | Fix |
|---|---|---|---|---|
| E0701 | `unhandled_result` | a value of type `T ! E` occurs where `T` is expected, or as a statement | the expression | insert `try ` before it if the enclosing function can propagate `E` |
| E0702 | `try_cannot_propagate` | `try` in a function that is not fallible and is not a test body | the `try` | — |
| E0703 | `try_error_mismatch` | the enclosing function's error type is neither `E` nor an enum with a variant `from E` | the `try` (`signature`) | — |
| E0704 | `not_a_result` | `try` or `catch` on a value that is not a result; `orelse` on one that is not optional | the operator | — |
| E0705 | `fail_not_fallible` | `fail` in a function that is not fallible | the `fail` | — |
| E0706 | `fail_type_mismatch` | the operand of `fail` is not the function's error type | the operand (`signature`) | if the error enum has a variant `v: from T` for the operand's type, wrap it as `.v(...)` |
| E0707 | `handler_falls_through` | a `catch` or `orelse` block can reach its end although a value is needed | the block's closing brace | — |
| E0708 | `exit_in_deferred_block` | `return`, `fail`, `try`, or a `break`/`continue` that leaves, inside `defer` or `errdefer` | the statement | — |
| E0709 | `errdefer_not_fallible` | `errdefer` in a function that is not fallible | the keyword | replace with `defer` |
| E0710 | `return_of_result` | `return expr;` in a fallible function where `expr` has the function's own type `T ! E` | the expression | insert `try ` before it |

## 9. Generics and compile-time evaluation (E08xx)

| Code | Name | Reported when | Primary span | Fix |
|---|---|---|---|---|
| E0801 | `bound_not_satisfied` | a type argument does not have a bound's property or implement its interface. This includes a view type where `Owned` is required, and the type of an `impl I for T` that does not implement a supertrait of `I` | the argument, or the `impl` (`signature`: the bound) | — |
| E0802 | `missing_bound` | a generic body uses an operation on `T` that its bounds do not grant | the operation (`declared`: the parameter) | add the one built-in bound that grants it, if there is exactly one |
| E0803 | `cannot_infer_type_argument` | a type parameter is not determined by the argument types | the callee name | — |
| E0804 | `wrong_type_argument_count` | too few or too many generic arguments | the argument list | — |
| E0805 | `not_generic` | generic arguments on something that takes none | the argument list | delete them |
| E0806 | `constant_evaluation_trapped` | a constant expression traps. The message carries the trap code | the operation that trapped (`instantiation` if inside a generic) | — |
| E0807 | `not_constant` | a constant expression reads a local variable or parameter (as in the condition of `@static_assert` in a function body) or a `static`, enters `unsafe`, calls an `extern` function or spawns | the operation | — |
| E0808 | `evaluation_limit` | constant evaluation exceeds the step limit | the outermost call | — |
| E0809 | `instantiation_depth` | generic instantiation nests more than 64 deep | the innermost use (`instantiation`) | — |
| E0810 | `static_assert_failed` | `@static_assert(cond, msg)` with a false `cond`. A `cond` that depends on a generic parameter is checked for each instance (phase 11) | the builtin call | — |
| E0811 | `layout_depends_on_parameter` | `@bits` or `@zero` on a type parameter with no bound that grants it | the builtin call | — |
| E0812 | `constant_cycle` | the value of a constant depends on itself | the constant's name | — |
| E0813 | `wrong_generic_argument_kind` | a type is given for a constant generic parameter, or a constant for a type parameter | the argument | — |

## 10. Moves and linear values (E10xx)

| Code | Name | Reported when | Primary span | Fix |
|---|---|---|---|---|
| E1001 | `use_after_move` | a place is read, borrowed or moved while it is moved or maybe moved | the use (`move`, and `loop` if the move is in an earlier iteration) | — |
| E1002 | `move_out_of_borrow` | a non-`Copy` value is moved out of `r.*`, an array or slice element, or a `static` | the expression | — (the message names `@replace`, `@swap` and `.take()`) |
| E1003 | `linear_not_consumed` | a linear value is still owned at an exit of its scope on some path, or a linear temporary is not consumed by the end of its statement. In a generic body, a `T` without `Discard` counts as linear | the binding at its declaration, or the expression that made the temporary (`scope_end`: each exit that leaks it) | if the type has exactly one consuming function with no other parameters and no error type, insert `defer { x.f(); }` after the declaration; otherwise the message lists the candidates. In a generic body, add the bound `Discard` instead, if the value is a `T` |
| E1004 | `linear_overwritten` | assignment to a place that holds a linear value | the assignment | — |
| E1005 | `moved_but_deferred` | a value that a pending deferred block consumes or uses is moved before the block runs | the move (`defer`) | — |
| E1006 | `partial_move_of_linear` | a field is moved out of a linear struct without destructuring the whole, outside its module | the expression | — |

## 11. Borrows (E11xx)

Each of these carries `borrow` (where the conflicting loan was made) and `later_use`
(a use that keeps it live).

| Code | Name | Reported when | Primary span | Fix |
|---|---|---|---|---|
| E1101 | `assign_while_borrowed` | a place is assigned while any loan on it, on a part of it, or on something it is part of, is live | the assignment | — |
| E1102 | `exclusive_borrow_conflict` | an exclusive borrow is taken while another loan that overlaps it is live | the new borrow, or the call whose receiver takes it | — |
| E1103 | `use_while_exclusively_borrowed` | a place is read or borrowed shared while an exclusive loan that overlaps it is live | the use | — |
| E1104 | `move_while_borrowed` | a place is moved, or consumed by `@forget`, while a loan on it is live | the move | — |
| E1105 | `borrow_outlives_place` | a place's storage ends while a loan on it is live, including a temporary whose statement ends while a borrow of it is still used | the borrow expression (`scope_end`, `later_use`) | — |

## 12. Origins (E12xx)

| Code | Name | Reported when | Primary span | Fix |
|---|---|---|---|---|
| E1201 | `missing_from` | a return type is written with a borrow in it and the signature has no `from` clause | the return type | append the `from` clause the body requires |
| E1202 | `origin_not_declared` | a returned view may hold a parameter origin that the `from` clause does not cover | the returned expression (`signature`) | add the missing origin to the clause |
| E1203 | `returns_local_borrow` | a returned view may hold a loan on a local of this function | the returned expression (`borrow`) | — |
| E1204 | `store_needs_into` | a view from parameter `x` is stored where it reaches parameter `p`, including a `spawn` into a scope parameter, and `x` is not declared `into p` | the store or `spawn` (`signature`) | when `p` is `&mut` or `&Scope`, append ` into p` to parameter `x`; otherwise none |
| E1205 | `local_borrow_escapes_through_parameter` | a loan on a local of this function is stored where it reaches a parameter | the store or `spawn` (`borrow`) | — |
| E1206 | `bad_origin_clause` | `from x` or `into p` names something that is not a parameter; `p.*` on a parameter that is not a borrow; `into p` where `p` is neither `&mut` nor `&Scope`; `from` on a return type that can hold no borrow (one with no borrow written in it and no `Self`, associated type or type parameter) | the clause | for the last case, delete the clause |
| E1207 | `into_blocks_fn_conversion` | a function with an `into` clause is used as a `fn` value or to satisfy an `Fn` bound | the function name | — |

## 13. `unsafe` (E13xx)

| Code | Name | Reported when | Primary span | Fix |
|---|---|---|---|---|
| E1301 | `needs_unsafe` | outside `unsafe`: a raw pointer dereference, a call of an `unsafe fn` or `extern` function or through an `extern fn` pointer, `@uninit`, `@assume`, or an `@unchecked_*` or `@*_from_raw` builtin (§15) | the operation | — |
| E1302 | `missing_unsafe_reason` | the reason string of an `unsafe` block is empty or only white space | the string | — |
| E1303 | `unsafe_impl_mismatch` | `impl Send`/`impl Sync` without `unsafe`, or `unsafe impl` of any other interface | the `impl` | — |
| E1304 | `unneeded_unsafe` | an `unsafe` block contains no operation that E1301 would reject. An operation counts only for the innermost `unsafe` block around it, so an outer block whose only such operations are inside an inner one is reported | the `unsafe` keyword | — |
| E1305 | `unsafe_fn_value` | an `unsafe fn` used as a value rather than called | the name | — |

## 14. Concurrency (E14xx)

| Code | Name | Reported when | Primary span | Fix |
|---|---|---|---|---|
| E1401 | `not_send` | a `spawn` argument, the result type of a spawned call, or a `par for` item is not `Send` | the argument, call or iterated expression (`declared`: the part that is not `Send`) | — |
| E1402 | `capture_not_sync` | a `par for` body reads an outer variable whose type is not `Sync` | the use (`declared`) | — |
| E1403 | `task_borrows_scope_local` | a `spawn` argument may hold a loan on a place declared inside the `scope` block | the borrow expression (`spawn`, `declared`) | — |
| E1404 | `spawn_statement_not_void` | a statement-form `spawn` of a call that returns something | the call | — |
| E1405 | `not_a_scope` | the expression after `in` is not a `&Scope` | the expression | — |
| E1406 | `par_for_not_splittable` | the iterated expression of a `par for` is not a range, slice or `par.Split` | the expression | — |
| E1407 | `par_for_outer_mutation` | a `par for` body assigns to, moves or exclusively borrows something declared outside the loop | the operation (`declared`) | — |
| E1408 | `par_for_exit` | `break`, a `continue` of an outer loop, `return`, `fail` or `try` inside a `par for` body | the statement | — |
| E1409 | `not_waitable` | a `select` arm's operation is not a call of a `waitable fn` | the call (`callee`) | — |
| E1410 | `bad_waitable` | a `waitable fn` whose receiver is not a shared borrow, or that has no `self` parameter | the `self` parameter, or the function's name when it has none | — |

A task that would use the same data twice is not a separate code: it is E1102 or E1103
with a `spawn` related span.

## 15. SIMD and kernels (E15xx)

| Code | Name | Reported when | Primary span | Fix |
|---|---|---|---|---|
| E1501 | `bad_vector_type` | a vector type whose total width is not 128, 256 or 512 bits, or a `boolxN` with `N` not in 2, 4, 8, 16, 32, 64 | the type | — |
| E1502 | `bad_lane_index` | `v[k]` or `@insert(v, k, x)` with `k` not a constant in range | the index | — |
| E1503 | `vector_shape_mismatch` | operands, a mask or a conversion with different lane counts, or `@bits` with different sizes where either type is a vector or a mask | the operation | — |
| E1504 | `bad_shuffle` | `@shuffle` indices that are not constant or out of range | the index array | — |
| E1510 | `bad_kernel_signature` | a kernel (§27.1) with a parameter after the index that is not a uniform, input or output, a result without `reduce`, or an error type; a kernel helper with a parameter or result that is not a uniform type, or an error type; generic parameters on a kernel or helper; an index type anywhere but inside a kernel | the parameter, the result type, the generic parameters, or the type that names the index | — |
| E1511 | `forbidden_in_kernel` | a kernel or kernel helper body does something §27.1 does not allow: allocates, borrows (`&`, `&mut`), calls an ordinary function, calls through a function value, closure, `&dyn` or interface bound, writes a closure, spawns or opens a `scope`, uses `par for`, `dispatch` or `select`, blocks, fails, uses `defer` or `unsafe`, loops with `for` over a non-constant range, matches on an enum with payloads, or has a local or value of a type that is not an element or uniform type; or helpers call each other in a cycle | the construct, or the call that closes the cycle | — |
| E1512 | `trapping_op_in_kernel` | integer `+ - * / %` or unary `-`, a shift by a non-constant count, `@assert`, `@panic`, `@unreachable` or `@narrow` in a kernel or kernel helper | the operation | — (the message names the wrapping, saturating and masking forms) |
| E1513 | `bad_kernel_index` | an output written or read at anything but the kernel's own index, or an input indexed with `[]` at anything but the kernel's own index | the index expression | — |
| E1514 | `kernel_type_unsupported` | an element or uniform type outside §27.1, or one the dispatch's executor does not support | the type, or the `dispatch` | — |
| E1515 | `not_a_kernel` | `dispatch` of something that is not a kernel (an ordinary function or a kernel helper), or a kernel called directly or used as a value | the callee | — |
| E1516 | `not_an_executor` | the expression after `on` does not implement `Executor` | the expression | — |
| E1517 | `domain_mismatch` | the number of ranges in the domain differs from the kernel's index dimension | the domain | — |
| E1518 | `dispatch_argument_mismatch` | an argument's storage does not match the executor: a slice for a GPU executor, a buffer for a CPU one | the argument | — |
| E1519 | `bad_reduce` | `reduce` on a function that is not a kernel, on a kernel with no result, or with an operator its result type does not support; a `dispatch` of a `reduce` kernel used as a statement | the clause, or the `dispatch` | — |

## 16. Foreign functions and targets (E16xx)

| Code | Name | Reported when | Primary span | Fix |
|---|---|---|---|---|
| E1601 | `not_ffi_type` | a parameter or result of an `extern` declaration or `export fn` is not an FFI type (§29); a fallible result `T ! E` never is (for an `export fn` that is E1604 instead) | the type | — |
| E1602 | `bad_variadic_argument` | a variadic argument that is an integer narrower than 32 bits, a `bool`, an `f32`, or not a scalar or raw pointer | the argument | following C's default argument promotions: append ` as i32` to a narrower integer or a `bool`, ` as f64` to an `f32` |
| E1603 | `unknown_abi` | an ABI string other than `"c"`, `"c-blocking"`, `"c-bigstack"` | the string | — |
| E1604 | `bad_export` | an `export fn` that is generic, fallible, or shares its symbol with another | the name | — |
| E1605 | `not_on_this_target` | an item that exists only on other targets is used outside a `when` that selects one of them | the use | — |
| E1606 | `unknown_target_field` | `@target.name` or a feature in `.has(...)` that does not exist | the name | — |

## 17. Packages and tools (E17xx)

Their `file` is `ingot.toml` or the file concerned.

| Code | Name | Reported when | Primary span | Fix |
|---|---|---|---|---|
| E1701 | `bad_manifest` | `ingot.toml` is missing, is not valid TOML, or has an unknown or ill-typed key | the key, or 1:1 | — |
| E1702 | `unknown_target` | a target name that is not one of the three | the name | — |
| E1703 | `dependency_hash_mismatch` | a dependency's content does not match its pinned hash | the hash | — |
| E1704 | `no_entry_point` | `build` or `run` of a package with no `main.ingot` or no `main` in it | 1:1 of `main.ingot`, or of `ingot.toml` | — |
| E1705 | `bad_module_name` | a file or directory name that is not a value identifier | 1:1 of the file | — |
| E1706 | `test_only` | `@test_env()` outside a `test` or `bench` block | the builtin | — |
| E1707 | `duplicate_test_name` | two `test` blocks, or two `bench` blocks, with one name in a module | the second (`previous`) | — |

## 18. Traps

A trap writes exactly one line to standard error and exits with status 101:

```
src/parse.ingot:88:17: trap T01: index 12 out of bounds for length 12
```

The position is the start of the expression that trapped. The text after the code is
fixed as below, with the bracketed parts filled in.

| Code | Name | Raised by | Text |
|---|---|---|---|
| T01 | `index_out_of_bounds` | `a[i]` | `index {i} out of bounds for length {len}` |
| T02 | `slice_out_of_bounds` | `s[lo..hi]`, `&a[lo..hi]` | `slice {lo}..{hi} out of bounds for length {len}` |
| T03 | `integer_overflow` | `+ - *`, unary `-`, and their assignment forms | `integer overflow in {op}` |
| T04 | `division_by_zero` | `/ %` | `division by zero` |
| T05 | `division_overflow` | `MIN / -1`, `MIN % -1` | `integer overflow in division` |
| T06 | `shift_out_of_range` | `<< >>` | `shift count {n} out of range for {bits}-bit value` |
| T07 | `narrow_out_of_range` | `@narrow(T, x)` | `value does not fit in {T}` |
| T08 | `assertion_failed` | `@assert` | `assertion failed: {source text of the condition}`, then `: {msg}` if a message was given |
| T09 | `panic` | `@panic(msg)` | `{msg}` |
| T10 | `unreachable` | `@unreachable()` | `entered unreachable code` |
| T11 | `stack_exhausted` | function entry | `stack exhausted` |
| T12 | `task_memory_exhausted` | `spawn`, `par for`, parking | `no memory for a task` |
| T13 | `deadlock` | the scheduler | `deadlock: all {n} tasks are waiting`, followed on the same line by nothing else. The list of tasks follows on further lines |
| T14 | `vector_access_out_of_bounds` | `@load`, `@store`, `@load_tail`, `@store_tail`, `@gather`, `@scatter` | `vector access at {i} out of bounds for length {len}` |
| T15 | `scatter_index_repeated` | `@scatter` | `scatter index {i} occurs twice` |
| T16 | `dispatch_out_of_bounds` | `dispatch` | `dispatch range {lo}..{hi} exceeds a buffer of length {len}`, or `dispatch range {lo}..{hi} is outside 0..2147483647` |
| T17 | `not_on_a_task` | a blocking operation, `spawn` or `par for` on a thread the runtime did not create | `blocking operation outside a task` |

T13 is the one trap that prints more than one line. For T11, T12, T13 and T17 the
position is the innermost Ingot source position known, or `<runtime>:0:0` if there is
none.

A trap during constant evaluation is not a trap of the program. It is error E0806, and
its message contains the trap's code and text.
