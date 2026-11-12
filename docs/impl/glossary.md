# Glossary

Each term has one meaning throughout the specification and the implementer's documents.
"§" numbers are sections of the specification.

## Outcomes

| Term | Meaning | § |
|---|---|---|
| **error** | The compiler rejects the program and emits a diagnostic with a code `E....`. There are no warnings | 30 |
| **trap** | The running program prints one line with a code `T..` and the process exits with status 101. Deterministic, the same in every build, no unwinding | 16.1 |
| **undefined behavior** | No guarantee of any kind. Reachable only from `unsafe` or foreign code | 16.2 |
| **safe code** | Code outside every `unsafe` block | 15 |
| **fix** | A set of text edits attached to a diagnostic that removes it | 30 |

## Program structure

| Term | Meaning | § |
|---|---|---|
| **module** | One `.ingot` file | 2 |
| **package** | A directory tree of modules with one `ingot.toml` | 33 |
| **item** | A declaration at module level or inside an `impl`: function, struct, enum, interface, constant, static, type alias, test | 4 |
| **value identifier** | A name beginning with a lowercase letter or `_` | 1 |
| **type identifier** | A name beginning with an uppercase letter. Also used for generic and constant parameters | 1 |
| **builtin** | A name spelled `@name`, defined by the language | 1 |
| **signature** | Everything of a function except its body: parameters, result, error type, `from` and `into` clauses, bounds, modifiers | 4.1 |
| **receiver** | A first parameter named `self`. The one argument that is borrowed automatically | 4.4 |
| **interface** | A named set of required functions and associated types | 4.4 |
| **bound** | A requirement on a generic parameter: an interface or a built-in property | 8 |
| **instantiation** | A generic item with concrete arguments. Each distinct one is compiled separately | 8 |
| **constant expression** | An expression the compiler evaluates | 9 |
| **target** | One CPU model under one operating system | 24 |

## Types and their properties

| Term | Meaning | § |
|---|---|---|
| **scalar** | An integer, float or `bool` | 3 |
| **aggregate** | An array, tuple, struct or enum | 3 |
| **payload** | The value carried by one enum variant | 4.3 |
| **`Copy`** | May be duplicated by assignment or passing | 10 |
| **`Discard`** | May go out of scope unconsumed. The opposite of linear | 10 |
| **linear** | Must be consumed exactly once on every path | 10 |
| **view type** | A type that contains a borrow | 10 |
| **self-contained**, **`Owned`** | A type that contains no borrow | 10 |
| **`Send`** | A value may be moved to another task | 20 |
| **`Sync`** | A shared borrow may be used from several tasks at once | 20 |
| **interior mutability** | The ability to change a value through a shared borrow. Only `std.sync` types and the arena have it in safe code | 13.6 |
| **capability** | A value whose possession allows an effect: `Heap`, `Vm`, `&Fs`, `&Net`, `&Clock`, `gpu.Device` | 33.1 |
| **FFI type** | A type allowed in an `extern` or `export` signature | 29 |

## Values, places and ownership

| Term | Meaning | § |
|---|---|---|
| **place** | Something that holds a value and can be assigned or borrowed: a variable, a field of a place, an element of a place, or `r.*` | 5.5 |
| **place path** | A place written from its root variable: `x`, `x.a`, `x.a[_]`, `x.*.b`. All elements of one array or slice are the same path | 12 |
| **move** | Transferring a non-`Copy` value out of a place, after which the place may not be read | 11 |
| **consume** | To move a value for the last time: pass it by value, return it, store it, destructure it | 10 |
| **consuming function** | A function that takes a linear type by value and takes it apart | 10 |
| **initialized**, **moved**, **maybe moved** | The three states a place path can be in at a program point | 11 |

## Borrows and origins

| Term | Meaning | § |
|---|---|---|
| **borrow** | A value of type `&T`, `&mut T`, `[]T`, `[]mut T` or `&dyn I` | 3 |
| **shared**, **exclusive** | The two kinds of borrow. Exclusive is the `mut` kind | 12 |
| **loan** | The record that a particular borrow expression borrowed a particular place path in a particular kind | 12 |
| **live** (of a loan) | Some value that may hold the loan may still be used | 12 |
| **reborrow** | A borrow made through an existing borrow: `&r.field`, `&mut r.*`, `s[a..b]` | 12 |
| **two-phase** | The rule that a borrow written as a call argument becomes active only when the call begins | 12 |
| **origin** | Something a view may borrow from: a loan, a parameter origin, or `static` | 13 |
| **origin set** | The set of origins a view value may hold at a program point | 13 |
| **parameter origin `p`** | Stands for whatever the borrow passed as parameter `p` borrows, in the caller | 13.1 |
| **inner origin `p.*`** | Stands for the borrows stored inside what `p` refers to | 13.1 |
| **`from` clause** | The part of a signature listing the origins a returned view may hold | 13.1 |
| **`into` clause** | The part of a parameter saying its borrows may be stored through another parameter | 13.2 |
| **widening** | Adding the origins of a stored view to every variable the store may reach | 13 |
| **type-parameter flow** | The derived rule that borrows inside a `T` flow wherever `T` appears in a signature | 13.3 |

## Errors and cleanup

| Term | Meaning | § |
|---|---|---|
| **fallible function** | A function declared `-> T ! E` | 6.2 |
| **result** | A value of type `T ! E`. Linear | 6.2 |
| **propagate** | To leave the enclosing function with an error by `try` | 6.3 |
| **`from` variant** | An enum variant declared `name: from E`, used by `try` to wrap an `E` | 4.3 |
| **exit** (of a block) | Any way control leaves it: falling off the end, `return`, `fail`, `break`, `continue`, a propagating `try` | 6.4 |
| **error exit** | An exit caused by `fail` or a propagating `try` | 6.4 |
| **deferred block** | A `defer` or `errdefer` body, run at the exits of its enclosing block | 6.4 |

## Concurrency

| Term | Meaning | § |
|---|---|---|
| **task** | A function call running on its own stack, scheduled by the runtime | 17 |
| **worker** | An OS thread of the runtime that runs tasks | 23 |
| **scope** | A `scope s { }` block and the value `s` that names it. Every task belongs to one | 18 |
| **handle** | A `Task[R]`, the value of an expression-form `spawn` | 18 |
| **park** | To suspend a task until something it waits for happens. The worker runs other tasks | 23 |
| **park point** | A place where a task may park: `join`, a channel operation, a lock, a timer, I/O, the end of a scope | 23 |
| **steal** | A worker taking an unstarted task from another worker's queue | 23 |
| **waitable** | A function the scheduler can wait for without committing to it, usable as a `select` arm | 21 |
| **canceled** | The state of a scope after `cancel()` or an error exit. Blocking operations in it fail | 22 |
| **grain** | The smallest number of items one chunk of a `par for` handles | 19 |

## Machine

| Term | Meaning | § |
|---|---|---|
| **lane** | One element of a vector | 26 |
| **mask** | A `boolxN`: one truth value per lane | 26 |
| **native width** | The widest vector that is one register: 128 bits on the M4, 512 on the 7800X3D | 26 |
| **kernel** | A `kernel fn`: a function applied independently to every index of a range | 27 |
| **uniform**, **input**, **output** | The three kinds of kernel parameter after the index | 27.1 |
| **executor** | The value after `on` in a `dispatch`, which decides how a kernel runs | 27.2 |
| **dispatch** | Running a kernel over a domain on an executor | 27.2 |
| **domain** | The index range, or ranges, of a dispatch | 27.2 |
| **check** | Code that tests a condition and traps: bounds, overflow, division, shift | 31 |
| **required removal** | A check the compiler must not emit, because section 31 says so | 31 |

## Implementation terms

These are used by [pipeline.md](pipeline.md) and [checker.md](checker.md) and do not
appear in the specification.

| Term | Meaning |
|---|---|
| **AST** | The syntax tree, with a span on every node and comments kept |
| **typed tree** | The AST after name resolution and type checking, with a type on every expression |
| **CFG** | The control-flow-graph form of one function body on which ownership is checked |
| **local** | A variable of the CFG: a parameter, a `let`/`var`, or a compiler temporary |
| **point** | A position in the CFG: before one statement or terminator |
| **storage end** | The point where a local goes out of scope |
| **primary span** | The source range a diagnostic is reported at |
| **related span** | Another source range that explains a diagnostic, with a role |
| **poison** | A type or value produced after an error, which suppresses further diagnostics that depend on it |
