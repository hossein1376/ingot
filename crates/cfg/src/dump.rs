//! The textual dump of a CFG, for review by eye and for the golden tests.
//!
//! # Format
//!
//! A dump is one or more `cfg N:` sections in CFG-id order (the item's own body
//! first, then the closures and `par for` bodies it contains). A section is:
//!
//! ```text
//! cfg 0: fn app.main
//!   (file main.ingot)
//!   result: void
//!   params: %0 n: i64
//!   locals:
//!     %1 total: i64                  ; 4:9
//!     %2 <temp>: bool                ; 4:12
//!   block 0:
//!     storage_start %1               ; 4:9
//!     %1 = const 0                   ; 4:17
//!     check overflow (T03) operands=(copy %1, const 1) : i64  ; 4:19
//!     ...
//!     return                         ; 9:1
//! ```
//!
//! * `%N` names local N; a user local keeps its source name, a temporary is
//!   `<temp>`. Static roots are `static#D` for definition D.
//! * A place is a root followed by `.field`, `.0` (tuple), `[index]`, `.*` or `.vN`
//!   (the payload of variant N).
//! * An operand is `copy %N`, `move %N`, or a constant: `const 5`, `const true`,
//!   `const "text"`, `@zero i64`, `@uninit i64`, `const-param name`, `const name`,
//!   `fn app.f`.
//! * Every statement and every terminator ends with `; line:col`, its source span.
//!   A statement copied from a deferred block adds `[defer 3:4 exit 9:1]`: where the
//!   `defer` was, and which exit triggered the copy.
//! * Statements are `storage_start %N`, `storage_end %N`, `PLACE = RVALUE`,
//!   `check ...`, `cancel %N`, `abandon %N` and `use OPERAND`.
//! * Checks name their trap code: `bounds (T01)`, `bounds (T02)`, `bounds (T14)`,
//!   `overflow (T03)`, `division (T04/T05)`, `shift (T06)`, `narrow (T07)`,
//!   `gather (T14)`, `scatter_distinct (T15)` and `dispatch_cover (T16)`. A check prints
//!   its container (`place=`), its index or range (`lo=`/`hi=`), its other operands
//!   (`operands=(...)`), further containers (`buffers=(...)`) and its type (`: i64`).
//! * An error exit is marked `(error)` on `return`. `[ends %N]` on a `return` says the
//!   return value's temporary ends at the terminator, after the value is read (it must
//!   survive the deferred code and every scope wait).
//! * Terminators: `goto B`; `branch OP -> T else E`; `switch OP { tag -> B, ...,
//!   else -> B }`; `return [OP] [(error)]`; `call CALLEE(ARGS) -> DEST then B` (no
//!   `->` for a void call); `trap TNN (name) [ARGS]`; `spawn`; `scope_wait %N then B`;
//!   `select { ... }` (one line per arm); `par for body N [captures] over ITERAND
//!   then B`; `dispatch KERNEL over LO..HI on EXEC args (...) then B`; and
//!   `unreachable`.
//!
//! The dump is deterministic: it depends only on the CFG, never on hash order.

use crate::model::*;
use ingot_diag::source::{Sources, Span};
use ingot_sema::ty::Mutability;
use ingot_sema::typed::CallMode;

/// Dump every CFG in `cfgs`.
pub fn dump(cfgs: &Cfgs, sources: &Sources) -> String {
    let mut out = String::new();
    for (id, cfg) in cfgs.iter() {
        out.push_str(&dump_one(id, cfg, sources));
    }
    out
}

/// Dump one CFG.
pub fn dump_one(id: CfgId, cfg: &Cfg, sources: &Sources) -> String {
    let mut d = Dumper {
        sources,
        out: String::new(),
        file: cfg.file,
    };
    let kind = match cfg.key.body {
        BodyKey::Item => "fn",
        BodyKey::Closure(_) => "closure",
        BodyKey::ParFor(_) => "par for",
    };
    let file = sources
        .files
        .get(cfg.file)
        .map(|s| s.path.clone())
        .unwrap_or_default();
    d.line(&format!("cfg {}: {} {}", id.0, kind, cfg.name));
    d.line(&format!("  (file {file})"));
    d.line(&format!("  result: {}", cfg.ret_name));
    if let Some(error) = &cfg.error_name {
        d.line(&format!("  error: {error}"));
    }
    if !cfg.params.is_empty() {
        let mut line = String::from("  params:");
        for (i, &p) in cfg.params.iter().enumerate() {
            if i > 0 {
                line.push(',');
            }
            line.push(' ');
            line.push_str(&d.local_ref(p));
            line.push_str(": ");
            line.push_str(&cfg.local(p).ty_name);
        }
        d.line(&line);
    }
    d.line("  locals:");
    for (i, local) in cfg.locals.iter().enumerate() {
        if cfg.params.contains(&(i as Local)) {
            continue;
        }
        let name = match &local.name {
            Some(n) => n.clone(),
            None => "<temp>".to_string(),
        };
        let mut line = format!("    %{i} {name}: {}", local.ty_name);
        let pos = d.pos(local.span);
        line.push_str(&format!("  ; {pos}"));
        d.line(&line);
    }
    for (i, block) in cfg.blocks.iter().enumerate() {
        d.line(&format!("  block {i}:"));
        for stmt in &block.stmts {
            let text = d.stmt(stmt);
            let pos = d.pos(stmt.span);
            let mut comment = pos;
            if let Some(copy) = stmt.defer {
                comment.push_str(&format!(
                    " [defer {} exit {}]",
                    d.pos(copy.defer_span),
                    d.pos(copy.exit_span)
                ));
            }
            d.line(&format!("    {text}  ; {comment}"));
        }
        let text = d.term(&block.term);
        let pos = d.pos(block.term_span);
        d.line(&format!("    {text}  ; {pos}"));
    }
    d.out
}

struct Dumper<'a> {
    sources: &'a Sources,
    out: String,
    file: usize,
}

impl<'a> Dumper<'a> {
    fn line(&mut self, s: &str) {
        self.out.push_str(s);
        self.out.push('\n');
    }

    fn pos(&self, span: Span) -> String {
        match self.sources.files.get(self.file) {
            Some(source) => {
                let p = source.position(span.start);
                format!("{}:{}", p.line, p.col)
            }
            None => format!("{}", span.start),
        }
    }

    fn local_ref(&self, l: Local) -> String {
        format!("%{l}")
    }

    fn place(&self, p: &Place) -> String {
        let mut out = match p.base {
            PlaceBase::Local(l) => self.local_ref(l),
            PlaceBase::Static(d) => format!("static#{}", d.0),
        };
        for proj in &p.proj {
            match proj {
                Projection::Field { index, name } => match name {
                    Some(n) => {
                        out.push('.');
                        out.push_str(n);
                    }
                    None => out.push_str(&format!(".{index}")),
                },
                Projection::TupleField(i) => out.push_str(&format!(".{i}")),
                Projection::Index(op) => {
                    out.push('[');
                    out.push_str(&self.operand(op));
                    out.push(']');
                }
                Projection::Deref => out.push_str(".*"),
                Projection::DerefRaw => out.push_str(".*raw"),
                Projection::Payload { variant } => out.push_str(&format!(".v{variant}")),
            }
        }
        out
    }

    fn operand(&self, op: &Operand) -> String {
        match op {
            Operand::Place(p, PlaceUse::Copy) => format!("copy {}", self.place(p)),
            Operand::Place(p, PlaceUse::Move) => format!("move {}", self.place(p)),
            Operand::Const(c) => self.const_(c),
        }
    }

    fn const_(&self, c: &Const) -> String {
        match c {
            Const::Int { value, .. } => format!("const {value}"),
            Const::Float { value, .. } => format!("const {value}"),
            Const::Bool(b) => format!("const {b}"),
            Const::Unit => "const ()".to_string(),
            Const::Str(bytes) => format!("const {:?}", String::from_utf8_lossy(bytes)),
            Const::CStr(bytes) => format!("c{:?}", String::from_utf8_lossy(bytes)),
            Const::Zero { name, .. } => format!("@zero {name}"),
            Const::Uninit { name, .. } => format!("@uninit {name}"),
            Const::Param { name, .. } => format!("const-param {name}"),
            Const::Named { name, .. } => format!("const {name}"),
            Const::Fn { name, .. } => format!("fn {name}"),
        }
    }

    fn mutability(m: Mutability) -> &'static str {
        match m {
            Mutability::Shared => "",
            Mutability::Mut => "mut ",
        }
    }

    fn rvalue(&self, r: &Rvalue) -> String {
        match r {
            Rvalue::Use(op) => self.operand(op),
            Rvalue::Unary { op, operand } => {
                let sym = match op {
                    ingot_syntax::ast::UnOp::Neg => "-",
                    ingot_syntax::ast::UnOp::NegWrap => "-%",
                    ingot_syntax::ast::UnOp::Not => "!",
                    ingot_syntax::ast::UnOp::BitNot => "~",
                    _ => "op",
                };
                format!("{sym}{}", self.operand(operand))
            }
            Rvalue::Binary { op, lhs, rhs } => {
                format!(
                    "{} {} {}",
                    self.operand(lhs),
                    op.symbol(),
                    self.operand(rhs)
                )
            }
            Rvalue::Borrow {
                place,
                mutability,
                two_phase,
            } => {
                let mut s = format!("&{}{}", Self::mutability(*mutability), self.place(place));
                if *two_phase {
                    s.push_str(" [two-phase]");
                }
                s
            }
            Rvalue::Reborrow { place, two_phase } => {
                let mut s = format!("reborrow {}", self.place(place));
                if *two_phase {
                    s.push_str(" [two-phase]");
                }
                s
            }
            Rvalue::Aggregate {
                kind,
                ty_name,
                fields,
                ..
            } => {
                let head = match kind {
                    AggregateKind::Struct(_) => ty_name.clone(),
                    AggregateKind::Tuple => "tuple".to_string(),
                    AggregateKind::Array => "array".to_string(),
                    AggregateKind::Variant { index, .. } => format!("variant {index} of {ty_name}"),
                    AggregateKind::Closure(e) => format!("closure {}", e.0),
                };
                format!("{head} ({})", self.operands(fields))
            }
            Rvalue::Cast { kind, operand } => {
                let name = match kind {
                    CastKind::IntWiden => "int-widen",
                    CastKind::ToFloat => "to-float",
                    CastKind::BoolToInt => "bool-to-int",
                    CastKind::EnumToTag => "enum-to-tag",
                    CastKind::RefToPtr => "ref-to-ptr",
                    CastKind::Identity => "identity",
                };
                format!("cast {name} {}", self.operand(operand))
            }
            Rvalue::DynCast { operand, coercion } => match coercion {
                DynCoercionKind::Interface(c) => {
                    format!("dyn-cast iface#{} {}", c.interface.0, self.operand(operand))
                }
                DynCoercionKind::Fn(c) => {
                    format!("dyn-cast {} {}", c.kind.name(), self.operand(operand))
                }
            },
            Rvalue::Discriminant(p) => format!("tag {}", self.place(p)),
            Rvalue::SliceLen(p) => format!("{}.len", self.place(p)),
            Rvalue::SlicePtr(p) => format!("{}.ptr", self.place(p)),
            Rvalue::Slice {
                base,
                lo,
                hi,
                borrow,
                two_phase,
            } => {
                let mut s = String::new();
                if let Some(m) = borrow {
                    s.push('&');
                    s.push_str(Self::mutability(*m));
                }
                s.push_str(&format!(
                    "{}[{}..{}]",
                    self.place(base),
                    self.operand(lo),
                    self.operand(hi)
                ));
                if *two_phase {
                    s.push_str(" [two-phase]");
                }
                s
            }
            Rvalue::SliceElem { base, index } => {
                format!("elem {}[{}]", self.place(base), self.operand(index))
            }
            Rvalue::Builtin {
                name,
                type_arg_name,
                args,
                ..
            } => {
                let mut s = format!("@{name}");
                if let Some(t) = type_arg_name {
                    s.push_str(&format!("[{t}]"));
                }
                s.push('(');
                for (i, a) in args.iter().enumerate() {
                    if i > 0 {
                        s.push_str(", ");
                    }
                    match a {
                        BuiltinArg::Value(v) => s.push_str(&self.operand(v)),
                        BuiltinArg::Place(p) => {
                            s.push_str("place ");
                            s.push_str(&self.place(p));
                        }
                    }
                }
                s.push(')');
                s
            }
        }
    }

    fn operands(&self, ops: &[Operand]) -> String {
        ops.iter()
            .map(|o| self.operand(o))
            .collect::<Vec<_>>()
            .join(", ")
    }

    fn stmt(&self, stmt: &Stmt) -> String {
        match &stmt.kind {
            StmtKind::StorageStart(l) => format!("storage_start {}", self.local_ref(*l)),
            StmtKind::StorageEnd(l) => format!("storage_end {}", self.local_ref(*l)),
            StmtKind::Assign { place, value } => {
                format!("{} = {}", self.place(place), self.rvalue(value))
            }
            StmtKind::Cancel(l) => format!("cancel {}", self.local_ref(*l)),
            StmtKind::Abandon(l) => format!("abandon {}", self.local_ref(*l)),
            StmtKind::Check(check) => self.check(check),
            StmtKind::Use(op) => format!("use {}", self.operand(op)),
        }
    }

    fn check(&self, check: &Check) -> String {
        let mut s = String::from("check ");
        match check.kind {
            CheckKind::Bounds(BoundsKind::Index) => s.push_str("bounds (T01)"),
            CheckKind::Bounds(BoundsKind::Slice) => s.push_str("bounds (T02)"),
            CheckKind::Bounds(BoundsKind::Vector { width, tail }) => {
                s.push_str(&format!(
                    "bounds (T14) width={width}{}",
                    if tail { " tail" } else { "" }
                ));
            }
            CheckKind::Overflow => s.push_str("overflow (T03)"),
            CheckKind::Division => s.push_str("division (T04/T05)"),
            CheckKind::Shift => s.push_str("shift (T06)"),
            CheckKind::Narrow => s.push_str("narrow (T07)"),
            CheckKind::Gather => s.push_str("gather (T14)"),
            CheckKind::ScatterDistinct => s.push_str("scatter_distinct (T15)"),
            CheckKind::DispatchCover => s.push_str("dispatch_cover (T16)"),
        }
        if let Some(p) = &check.place {
            s.push_str(&format!(" place={}", self.place(p)));
        }
        if let Some(lo) = &check.lo {
            s.push_str(&format!(" lo={}", self.operand(lo)));
        }
        if let Some(hi) = &check.hi {
            s.push_str(&format!(" hi={}", self.operand(hi)));
        }
        if !check.operands.is_empty() {
            s.push_str(&format!(" operands=({})", self.operands(&check.operands)));
        }
        if !check.places.is_empty() {
            s.push_str(&format!(
                " buffers=({})",
                check
                    .places
                    .iter()
                    .map(|p| self.place(p))
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
        if let Some(name) = &check.ty_name {
            s.push_str(&format!(" : {name}"));
        }
        s
    }

    fn callee(&self, target: &CallTarget) -> String {
        match target {
            CallTarget::Fn { name, .. } => name.clone(),
            CallTarget::Value { callee, kind, mode } => {
                let kind = match kind {
                    ValueKind::FnPtr => "fn".to_string(),
                    ValueKind::Closure(e) => format!("closure{}", e.0),
                    ValueKind::Bound => "Fn-bound".to_string(),
                    ValueKind::DynFn => "dyn Fn".to_string(),
                };
                let mode = match mode {
                    Some(CallMode::Shared) => " shared",
                    Some(CallMode::Exclusive) => " exclusive",
                    Some(CallMode::Move) => " move",
                    None => "",
                };
                format!("*{} ({kind}{mode})", self.operand(callee))
            }
        }
    }

    fn term(&self, term: &Terminator) -> String {
        match term {
            Terminator::Goto(b) => format!("goto {b}"),
            Terminator::Branch {
                cond,
                then_block,
                else_block,
            } => format!(
                "branch {} -> {} else {}",
                self.operand(cond),
                then_block,
                else_block
            ),
            Terminator::Switch {
                value,
                cases,
                default,
            } => {
                let mut s = format!("switch {} {{ ", self.operand(value));
                for (tag, b) in cases {
                    s.push_str(&format!("{tag} -> {b}, "));
                }
                s.push_str(&format!("else -> {default} }}"));
                s
            }
            Terminator::Return {
                value,
                is_error,
                end,
            } => {
                let mut s = String::from("return");
                if let Some(v) = value {
                    s.push(' ');
                    s.push_str(&self.operand(v));
                }
                if *is_error {
                    s.push_str(" (error)");
                }
                if let Some(l) = end {
                    s.push_str(&format!(" [ends {}]", self.local_ref(*l)));
                }
                s
            }
            Terminator::Call {
                target,
                args,
                dest,
                next,
            } => {
                let mut s = format!("call {}({})", self.callee(target), self.operands(args));
                if let Some(d) = dest {
                    s.push_str(&format!(" -> {}", self.place(d)));
                }
                s.push_str(&format!(" then {next}"));
                s
            }
            Terminator::Trap { code, operands } => {
                format!(
                    "trap {} ({}) [{}]",
                    code.code(),
                    code.name(),
                    self.operands(operands)
                )
            }
            Terminator::Spawn {
                target,
                args,
                scope,
                dest,
                next,
            } => {
                let mut s = format!(
                    "spawn {}({}) in {}",
                    self.callee(target),
                    self.operands(args),
                    self.operand(scope)
                );
                if let Some(d) = dest {
                    s.push_str(&format!(" -> {}", self.place(d)));
                }
                s.push_str(&format!(" then {next}"));
                s
            }
            Terminator::ScopeWait { scope, next } => {
                format!("scope_wait {} then {next}", self.local_ref(*scope))
            }
            Terminator::Select { ops } => {
                let mut s = String::from("select { ");
                for (i, op) in ops.iter().enumerate() {
                    if i > 0 {
                        s.push_str("; ");
                    }
                    match &op.call {
                        None => {
                            s.push_str(&format!("else -> {}", op.successor));
                        }
                        Some(call) => {
                            s.push_str(&format!(
                                "{} = {}({}) then {}",
                                self.place(&call.dest),
                                self.callee(&call.target),
                                self.operands(&call.args),
                                op.successor
                            ));
                        }
                    }
                }
                s.push_str(" }");
                s
            }
            Terminator::ParFor {
                body,
                body_name,
                captures,
                capture_places,
                capture_names,
                iterand,
                grain,
                next,
            } => {
                let mut s = format!("par for body {} ({})", body.0, body_name);
                if !capture_names.is_empty() {
                    s.push_str(&format!(" captures [{}]", capture_names.join(", ")));
                    s.push_str(&format!(" ({})", self.operands(captures)));
                    s.push_str(&format!(
                        " from ({})",
                        capture_places
                            .iter()
                            .map(|p| self.place(p))
                            .collect::<Vec<_>>()
                            .join(", ")
                    ));
                }
                match iterand {
                    ParIterand::Value(v) => {
                        s.push_str(&format!(" over {}", self.operand(v)));
                    }
                    ParIterand::Range {
                        lo,
                        hi,
                        inclusive,
                        step,
                    } => {
                        s.push_str(&format!(
                            " over {}{}{}",
                            self.operand(lo),
                            if *inclusive { "..=" } else { ".." },
                            self.operand(hi)
                        ));
                        if let Some(st) = step {
                            s.push_str(&format!(" step {}", self.operand(st)));
                        }
                    }
                }
                if let Some(g) = grain {
                    s.push_str(&format!(" grain {}", self.operand(g)));
                }
                s.push_str(&format!(" then {next}"));
                s
            }
            Terminator::Dispatch {
                kernel_name,
                kernel_args_name,
                executor,
                lo,
                hi,
                args,
                dest,
                next,
                ..
            } => {
                let mut s = format!(
                    "dispatch {}{} over {}..{} on {}",
                    kernel_name,
                    kernel_args_name,
                    self.operand(lo),
                    self.operand(hi),
                    self.operand(executor)
                );
                if !args.is_empty() {
                    s.push_str(" args (");
                    for (i, a) in args.iter().enumerate() {
                        if i > 0 {
                            s.push_str(", ");
                        }
                        s.push_str(&self.operand(&a.value));
                        match a.borrow {
                            Some(Mutability::Shared) => s.push_str(" [shared]"),
                            Some(Mutability::Mut) => s.push_str(" [exclusive]"),
                            None => {}
                        }
                        if let Some(p) = &a.place {
                            s.push_str(&format!(" from {}", self.place(p)));
                        }
                    }
                    s.push(')');
                }
                if let Some(d) = dest {
                    s.push_str(&format!(" -> {}", self.place(d)));
                }
                s.push_str(&format!(" then {next}"));
                s
            }
            Terminator::Unreachable => "unreachable".to_string(),
        }
    }
}
