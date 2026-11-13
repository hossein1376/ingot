//! Tree-walking evaluator used by `ingot run`, `ingot test` and the conformance
//! runner.
//!
//! It is not a code generator. It exists so the front end can be exercised on
//! real programs before the LLVM back end lands, and it implements the same
//! traps the specification requires. Values are transferred through
//! `Rc<RefCell<...>>` places so `&mut` arguments, slices and deferred blocks all
//! observe the same storage.

use crate::ast::*;
use crate::check::{Analysis, ConstVal, Function, Program};
use crate::source::{Source, Span};
use crate::token::ScalarTy;
use crate::types::Type;
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

#[derive(Clone, Debug)]
pub enum Value {
    Unit,
    Int {
        value: i128,
        ty: ScalarTy,
    },
    Float {
        value: f64,
        ty: ScalarTy,
    },
    Bool(bool),
    Str(Vec<u8>),
    Array(Vec<Value>),
    Tuple(Vec<Value>),
    Struct {
        ty: String,
        fields: HashMap<String, Value>,
    },
    Enum {
        ty: String,
        variant: String,
        payload: Option<Box<Value>>,
    },
    Result {
        ok: Option<Box<Value>>,
        err: Option<Box<Value>>,
    },
    Vector {
        elem: ScalarTy,
        lanes: Vec<Value>,
    },
    Mask(Vec<bool>),
    Slice {
        root: Rc<RefCell<Value>>,
        start: usize,
        len: usize,
        mutable: bool,
    },
    Ref(Place),
    Atomic(Rc<RefCell<Value>>),
    Task(Box<Value>),
    External(String),
}

#[derive(Clone, Debug)]
pub struct Place {
    root: Rc<RefCell<Value>>,
    path: Vec<Seg>,
}

#[derive(Clone, Debug)]
enum Seg {
    Field(String),
    Index(usize),
}

#[derive(Clone, Debug)]
pub struct Trap {
    pub code: &'static str,
    pub message: String,
    pub span: Span,
}

#[derive(Clone, Debug)]
enum Flow {
    Normal,
    Return(Option<Value>),
    Fail(Value),
    Break(Option<String>),
    Continue(Option<String>),
}

#[derive(Clone, Debug)]
enum EvalErr {
    Trap(Trap),
    Flow(Flow),
}

type R<T> = Result<T, EvalErr>;

impl EvalErr {
    fn trap(code: &'static str, message: impl Into<String>, span: Span) -> Self {
        EvalErr::Trap(Trap {
            code,
            message: message.into(),
            span,
        })
    }
}

struct Frame {
    vars: HashMap<String, Rc<RefCell<Value>>>,
    ret: Type,
    error: Option<Type>,
    fallible: bool,
    module: String,
}

pub struct Outcome {
    pub status: i32,
    pub trap: Option<Trap>,
    pub stderr: String,
}

pub fn run_main(analysis: &Analysis) -> Result<i32, String> {
    let outcome = run(analysis)?;
    if let Some(_trap) = &outcome.trap {
        eprintln!("{}", outcome.stderr);
        return Ok(101);
    }
    Ok(outcome.status)
}

pub fn run(analysis: &Analysis) -> Result<Outcome, String> {
    if !analysis.diagnostics.is_empty() {
        return Err("cannot run a program with diagnostics".to_string());
    }
    let main_file = analysis
        .program
        .functions
        .get("main")
        .map(|f| f.file.clone());
    let source = main_file
        .as_ref()
        .and_then(|p| analysis.sources.iter().find(|s| &s.path == p))
        .or_else(|| analysis.sources.files.first())
        .ok_or_else(|| "no source file".to_string())?;
    let main_name = if analysis.program.root_module == "main" {
        "main".to_string()
    } else {
        format!("{}.main", analysis.program.root_module)
    };
    let Some(main) = analysis.program.functions.get(&main_name).cloned() else {
        return Err("no `main` function".to_string());
    };
    let mut interp = Interp::new(&analysis.program, source);
    let mut env_fields = HashMap::new();
    env_fields.insert("args".to_string(), Value::Array(Vec::new()));
    let env = Rc::new(RefCell::new(Value::Struct {
        ty: "std.Env".to_string(),
        fields: env_fields,
    }));
    let args = if main.decl.params.len() == 1 {
        vec![Value::Ref(Place {
            root: env,
            path: Vec::new(),
        })]
    } else {
        Vec::new()
    };
    match interp.call_function(&main, args) {
        Ok(v) => {
            let status = exit_status(&v);
            Ok(Outcome {
                status,
                trap: None,
                stderr: String::new(),
            })
        }
        Err(EvalErr::Trap(t)) => {
            let pos = source.position(t.span.start);
            let stderr = format!(
                "{}:{}:{}: trap {}: {}",
                source.path, pos.line, pos.col, t.code, t.message
            );
            Ok(Outcome {
                status: 101,
                trap: Some(t),
                stderr,
            })
        }
        Err(EvalErr::Flow(_)) => Err("control flow escaped `main`".to_string()),
    }
}

pub fn run_tests(
    program: &Program,
    sources: &crate::source::Sources,
) -> Result<(usize, usize), String> {
    let mut passed = 0;
    let mut failed = 0;
    for t in &program.tests {
        let source = sources
            .iter()
            .find(|s| s.path == t.file)
            .or_else(|| sources.files.first())
            .ok_or_else(|| "no source file for test".to_string())?;
        let mut interp = Interp::new(program, source);
        // A test body may use `try`; a failure that reaches its end fails the
        // test rather than the compiler.
        let test_fn = Function {
            key: format!("test.{}", t.decl.name),
            decl: FnDecl {
                name: t.decl.name.clone(),
                name_span: t.decl.span,
                params: Vec::new(),
                ret: Ty::Scalar(ScalarTy::Void, t.decl.span),
                error: Some(Ty::Path(PathTy {
                    segments: vec![("TestErr".to_string(), t.decl.span)],
                    args: Vec::new(),
                    args_span: None,
                    assoc: None,
                    span: t.decl.span,
                })),
                from: Vec::new(),
                reduce: None,
                mods: FnMods::default(),
                generics: Vec::new(),
                body: Some(t.decl.body.clone()),
                span: t.decl.span,
            },
            file: t.file.clone(),
            module: t.module.clone(),
            owner: None,
            interface: None,
            external: false,
            signature_ret: Type::Void,
            signature_error: Some(Type::External("TestErr".to_string())),
            signature_params: Vec::new(),
        };
        match interp.call_function(&test_fn, Vec::new()) {
            Ok(Value::Result { ok: Some(_), .. }) => passed += 1,
            _ => failed += 1,
        }
    }
    Ok((passed, failed))
}

fn exit_status(v: &Value) -> i32 {
    match v {
        Value::Int { value, .. } => *value as i32,
        Value::Result { ok: Some(v), .. } => exit_status(v),
        _ => 0,
    }
}

struct Interp<'a> {
    program: &'a Program,
    source: &'a Source,
    frames: Vec<Frame>,
    steps: u64,
}

impl<'a> Interp<'a> {
    fn new(program: &'a Program, source: &'a Source) -> Self {
        Interp {
            program,
            source,
            frames: Vec::new(),
            steps: 0,
        }
    }

    fn frame(&mut self) -> &mut Frame {
        self.frames.last_mut().unwrap()
    }

    fn lookup(&self, name: &str) -> Option<Rc<RefCell<Value>>> {
        self.frames.last().and_then(|f| f.vars.get(name).cloned())
    }

    fn current_module(&self) -> String {
        self.frames
            .last()
            .map(|f| f.module.clone())
            .unwrap_or_else(|| self.program.root_module.clone())
    }

    fn frame_error(&self) -> Option<Type> {
        self.frames.last().and_then(|f| f.error.clone())
    }

    fn frame_ret(&self) -> Type {
        self.frames
            .last()
            .map(|f| f.ret.clone())
            .unwrap_or(Type::Void)
    }

    fn resolve_ty(&self, ty: &Ty) -> Type {
        match ty {
            Ty::Scalar(s, _) => match s {
                ScalarTy::Void => Type::Void,
                ScalarTy::Never => Type::Never,
                ScalarTy::Bool => Type::Bool,
                ScalarTy::F32 | ScalarTy::F64 => Type::Float(*s),
                _ => Type::Int(*s),
            },
            Ty::Vector(v, _) => Type::Vector(*v),
            Ty::Path(p) => {
                let module = self.current_module();
                match self.program.resolve_type_path(&module, p) {
                    Some(crate::check::ResolvedName::Local(c)) => {
                        if self.program.defs.structs.contains_key(&c) {
                            Type::Struct(c, Vec::new())
                        } else if self.program.defs.enums.contains_key(&c) {
                            Type::Enum(c, Vec::new())
                        } else {
                            Type::External(c)
                        }
                    }
                    Some(crate::check::ResolvedName::External(base)) => Type::External(base),
                    None => {
                        let n = p.name();
                        if n == "Self" {
                            Type::External("Self".to_string())
                        } else {
                            Type::External(n.to_string())
                        }
                    }
                }
            }
            Ty::Borrow { mutable, inner, .. } => Type::Borrow {
                mutable: *mutable,
                inner: Box::new(self.resolve_ty(inner)),
            },
            Ty::Slice { mutable, elem, .. } => Type::Slice {
                mutable: *mutable,
                elem: Box::new(self.resolve_ty(elem)),
            },
            Ty::Array { len, elem, .. } => {
                let n = self.const_i64(len).unwrap_or(0);
                Type::Array(n, Box::new(self.resolve_ty(elem)))
            }
            Ty::Ptr { mutable, inner, .. } => Type::Ptr {
                mutable: *mutable,
                inner: Box::new(self.resolve_ty(inner)),
            },
            Ty::Optional { inner, .. } => Type::Optional(Box::new(self.resolve_ty(inner))),
            Ty::Tuple { elems, .. } => {
                Type::Tuple(elems.iter().map(|t| self.resolve_ty(t)).collect())
            }
            Ty::Fn {
                params, ret, error, ..
            } => Type::Fn {
                params: params.iter().map(|t| self.resolve_ty(t)).collect(),
                ret: Box::new(self.resolve_ty(ret)),
                error: error.as_ref().map(|e| Box::new(self.resolve_ty(e))),
            },
            Ty::Dyn { interface, .. } => Type::Dyn(interface.name().to_string()),
            Ty::Poison(_) => Type::Error,
        }
    }

    fn const_i64(&self, e: &Expr) -> Option<i64> {
        match &e.kind {
            ExprKind::Int {
                value, negative, ..
            } => Some(if *negative {
                -(*value as i64)
            } else {
                *value as i64
            }),
            ExprKind::Paren(x) => self.const_i64(x),
            ExprKind::Ident(n) => self.program.consts.get(n).and_then(|c| match c.value {
                Some(ConstVal::Int(v)) => Some(v as i64),
                _ => None,
            }),
            ExprKind::Binary { op, lhs, rhs, .. } => {
                let a = self.const_i64(lhs)?;
                let b = self.const_i64(rhs)?;
                match op {
                    BinOp::Add => a.checked_add(b),
                    BinOp::Sub => a.checked_sub(b),
                    BinOp::Mul => a.checked_mul(b),
                    _ => None,
                }
            }
            _ => None,
        }
    }

    // ------------------------------------------------------------- functions

    fn call_function(&mut self, f: &Function, args: Vec<Value>) -> R<Value> {
        self.steps += 1;
        if self.steps > 20_000_000 {
            return Err(EvalErr::trap(
                "T11",
                "evaluation step limit exceeded",
                f.decl.name_span,
            ));
        }
        if self.frames.len() > 512 {
            return Err(EvalErr::trap("T11", "stack exhausted", f.decl.name_span));
        }
        let mut vars = HashMap::new();
        for (i, p) in f.decl.params.iter().enumerate() {
            let arg = args.get(i).cloned().unwrap_or(Value::Unit);
            let expected = f.signature_params.get(i);
            let value = match expected {
                Some(Type::Borrow { .. } | Type::Slice { .. }) => arg,
                _ => arg,
            };
            vars.insert(p.name.clone(), Rc::new(RefCell::new(value)));
        }
        self.frames.push(Frame {
            vars,
            ret: f.signature_ret.clone(),
            error: f.signature_error.clone(),
            fallible: f.signature_error.is_some(),
            module: f.module.clone(),
        });
        let frame = self.frame();
        let fallible = frame.fallible;
        let body = f.decl.body.clone();
        let result = match body {
            Some(body) => self.exec_block(&body),
            None => Ok(Flow::Normal),
        };
        self.frames.pop();
        let flow = result?;
        let out = match flow {
            Flow::Return(Some(v)) => v,
            Flow::Return(None) | Flow::Normal => Value::Unit,
            Flow::Fail(e) => {
                if fallible {
                    return Ok(Value::Result {
                        ok: None,
                        err: Some(Box::new(e)),
                    });
                }
                return Ok(Value::Result {
                    ok: None,
                    err: Some(Box::new(e)),
                });
            }
            Flow::Break(_) | Flow::Continue(_) => Value::Unit,
        };
        if fallible {
            Ok(Value::Result {
                ok: Some(Box::new(out)),
                err: None,
            })
        } else {
            Ok(out)
        }
    }

    // ---------------------------------------------------------------- blocks

    fn exec_block(&mut self, block: &Block) -> R<Flow> {
        let mut defers: Vec<(Block, bool)> = Vec::new();
        for stmt in &block.stmts {
            if let Stmt::Defer { err, body, .. } = stmt {
                defers.push((body.clone(), *err));
                continue;
            }
            let flow = match self.exec_stmt(stmt) {
                Ok(flow) => flow,
                Err(EvalErr::Flow(flow)) => {
                    self.run_defers(&defers, &flow)?;
                    return Ok(flow);
                }
                Err(e) => return Err(e),
            };
            if !matches!(flow, Flow::Normal) {
                self.run_defers(&defers, &flow)?;
                return Ok(flow);
            }
        }
        self.run_defers(&defers, &Flow::Normal)?;
        Ok(Flow::Normal)
    }

    fn run_defers(&mut self, defers: &[(Block, bool)], flow: &Flow) -> R<()> {
        for (block, err) in defers.iter().rev() {
            if *err && !matches!(flow, Flow::Fail(_)) {
                continue;
            }
            let f = self.exec_block(block)?;
            match f {
                Flow::Normal | Flow::Return(_) | Flow::Fail(_) => {}
                Flow::Break(_) | Flow::Continue(_) => {}
            }
        }
        Ok(())
    }

    fn exec_stmt(&mut self, stmt: &Stmt) -> R<Flow> {
        match stmt {
            Stmt::Let {
                pattern, ty, init, ..
            } => {
                let expected = ty.as_ref().map(|t| self.resolve_ty(t));
                let value = self.eval_expr(init, expected.as_ref())?;
                self.bind_pattern(pattern, &value);
                Ok(Flow::Normal)
            }
            Stmt::Assign {
                place,
                op,
                value,
                span,
            } => {
                if matches!(&place.kind, ExprKind::Ident(n) if n == "_") {
                    self.eval_expr(value, None)?;
                    return Ok(Flow::Normal);
                }
                let p = self.eval_place(place)?;
                let current = read_place(&p);
                let new = if *op == AssignOp::Assign {
                    self.eval_expr(value, None)?
                } else {
                    let rhs = self.eval_expr(value, None)?;
                    let bin = assign_bin(*op);
                    apply_binary(bin, current, rhs, *span)?
                };
                write_place(&p, new);
                Ok(Flow::Normal)
            }
            Stmt::Expr { expr, .. } => match self.eval_expr(expr, None) {
                Ok(_) => Ok(Flow::Normal),
                Err(EvalErr::Flow(f)) => Ok(f),
                Err(e) => Err(e),
            },
            Stmt::Return { value, span } => {
                let ret = self.frame_ret();
                let v = match value {
                    Some(e) => Some(self.eval_expr(e, Some(&ret))?),
                    None => None,
                };
                let _ = span;
                Ok(Flow::Return(v))
            }
            Stmt::Fail { value, span } => {
                let err_ty = self.frame_error();
                let v = self.eval_expr(value, err_ty.as_ref())?;
                let _ = span;
                Ok(Flow::Fail(v))
            }
            Stmt::Break { label, .. } => Ok(Flow::Break(label.clone())),
            Stmt::Continue { label, .. } => Ok(Flow::Continue(label.clone())),
            Stmt::Defer { .. } => Ok(Flow::Normal),
            Stmt::Block(b) => self.exec_block(b),
            Stmt::If(x) => {
                if let Some(pattern) = &x.cond.let_pattern {
                    let v = self.eval_expr(&x.cond.expr, None)?;
                    if let Some(bindings) = self.match_pattern(pattern, &v) {
                        for (n, v) in bindings {
                            self.set_var(&n, v);
                        }
                        self.exec_block(&x.then)
                    } else if let Some(e) = &x.otherwise {
                        self.exec_stmt(e)
                    } else {
                        Ok(Flow::Normal)
                    }
                } else {
                    let c = self.eval_expr(&x.cond.expr, Some(&Type::Bool))?;
                    if truthy(&c) {
                        self.exec_block(&x.then)
                    } else if let Some(e) = &x.otherwise {
                        self.exec_stmt(e)
                    } else {
                        Ok(Flow::Normal)
                    }
                }
            }
            Stmt::While { cond, body, .. } => {
                let mut guard = 0u64;
                loop {
                    guard += 1;
                    if guard > 10_000_000 {
                        return Err(EvalErr::trap("T11", "loop step limit exceeded", body.span));
                    }
                    let go = if let Some(pattern) = &cond.let_pattern {
                        let v = self.eval_expr(&cond.expr, None)?;
                        if let Some(bindings) = self.match_pattern(pattern, &v) {
                            for (n, v) in bindings {
                                self.set_var(&n, v);
                            }
                            true
                        } else {
                            false
                        }
                    } else {
                        truthy(&self.eval_expr(&cond.expr, Some(&Type::Bool))?)
                    };
                    if !go {
                        break;
                    }
                    match self.exec_block(body)? {
                        Flow::Break(None) => break,
                        Flow::Continue(None) | Flow::Normal => {}
                        Flow::Return(v) => return Ok(Flow::Return(v)),
                        Flow::Fail(e) => return Ok(Flow::Fail(e)),
                        Flow::Break(Some(l)) => return Ok(Flow::Break(Some(l))),
                        Flow::Continue(Some(l)) => return Ok(Flow::Continue(Some(l))),
                    }
                }
                Ok(Flow::Normal)
            }
            Stmt::Loop { body, .. } => {
                let mut guard = 0u64;
                loop {
                    guard += 1;
                    if guard > 10_000_000 {
                        return Err(EvalErr::trap("T11", "loop step limit exceeded", body.span));
                    }
                    match self.exec_block(body)? {
                        Flow::Break(None) => break,
                        Flow::Continue(None) | Flow::Normal => {}
                        Flow::Return(v) => return Ok(Flow::Return(v)),
                        Flow::Fail(e) => return Ok(Flow::Fail(e)),
                        Flow::Break(Some(l)) => return Ok(Flow::Break(Some(l))),
                        Flow::Continue(Some(l)) => return Ok(Flow::Continue(Some(l))),
                    }
                }
                Ok(Flow::Normal)
            }
            Stmt::For {
                vars,
                index,
                iter,
                range_end,
                step,
                body,
                ..
            } => {
                let items = self.iter_values(iter, range_end.as_ref(), step.as_ref())?;
                for (i, item) in items.into_iter().enumerate() {
                    if let Some((idx, _)) = index {
                        self.set_var(
                            idx,
                            Value::Int {
                                value: i as i128,
                                ty: ScalarTy::I64,
                            },
                        );
                    }
                    if let Some((var, _)) = vars.first() {
                        self.set_var(var, item);
                    }
                    match self.exec_block(body)? {
                        Flow::Break(None) => break,
                        Flow::Continue(None) | Flow::Normal => {}
                        Flow::Return(v) => return Ok(Flow::Return(v)),
                        Flow::Fail(e) => return Ok(Flow::Fail(e)),
                        Flow::Break(Some(l)) => return Ok(Flow::Break(Some(l))),
                        Flow::Continue(Some(l)) => return Ok(Flow::Continue(Some(l))),
                    }
                }
                Ok(Flow::Normal)
            }
            Stmt::Match(m) => {
                let v = self.eval_expr(&m.scrutinee, None)?;
                self.exec_match(&v, &m.arms)
            }
            Stmt::Scope { name, body, .. } => {
                self.set_var(name, Value::External("Scope".to_string()));
                self.exec_block(body)
            }
            Stmt::Spawn { call, scope, .. } => {
                let _ = self.eval_expr(scope, None)?;
                self.eval_expr(call, None)?;
                Ok(Flow::Normal)
            }
            Stmt::Dispatch {
                call,
                domain,
                executor,
                ..
            } => {
                let _ = self.eval_expr(executor, None);
                self.dispatch(call, domain)?;
                Ok(Flow::Normal)
            }
            Stmt::Select { arms, .. } => {
                for arm in arms {
                    if let Ok(v) = self.eval_expr(&arm.call, None) {
                        let Some(bindings) = self.match_pattern(&arm.pattern, &v) else {
                            continue;
                        };
                        for (n, v) in bindings {
                            self.set_var(&n, v);
                        }
                        return self.exec_block(&arm.body);
                    }
                }
                Ok(Flow::Normal)
            }
            Stmt::Unsafe { body, .. } => self.exec_block(body),
            Stmt::When {
                then, otherwise, ..
            } => {
                // Target selection is a phase-4 concern; the first branch is the
                // host target in this MVP.
                if then.stmts.is_empty() && !otherwise.is_empty() {
                    self.exec_stmt(&otherwise[0])
                } else {
                    self.exec_block(then)
                }
            }
            Stmt::Poison => Ok(Flow::Normal),
        }
    }

    fn set_var(&mut self, name: &str, value: Value) {
        if let Some(cell) = self.lookup(name) {
            *cell.borrow_mut() = value;
        } else {
            self.frame()
                .vars
                .insert(name.to_string(), Rc::new(RefCell::new(value)));
        }
    }

    fn bind_pattern(&mut self, p: &Pattern, value: &Value) {
        match p {
            Pattern::Bind { name, .. } => self.set_var(name, value.clone()),
            Pattern::Tuple { elems, .. } => {
                if let Value::Tuple(vs) = value {
                    for (p, v) in elems.iter().zip(vs) {
                        self.bind_pattern(p, v);
                    }
                }
            }
            Pattern::Struct { fields, .. } => {
                if let Value::Struct { fields: fs, .. } = value {
                    for f in fields {
                        let v = fs.get(&f.name).cloned().unwrap_or(Value::Unit);
                        match &f.pattern {
                            Some(p) => self.bind_pattern(p, &v),
                            None => self.set_var(&f.name, v),
                        }
                    }
                }
            }
            Pattern::Variant { payload, .. } => {
                if let Some(p) = payload
                    && let Some(v) = variant_payload(value)
                {
                    self.bind_pattern(p, &v);
                }
            }
            _ => {}
        }
    }

    fn match_pattern(&self, p: &Pattern, value: &Value) -> Option<Vec<(String, Value)>> {
        let mut out = Vec::new();
        if self.match_pattern_into(p, value, &mut out) {
            Some(out)
        } else {
            None
        }
    }

    fn match_pattern_into(
        &self,
        p: &Pattern,
        value: &Value,
        out: &mut Vec<(String, Value)>,
    ) -> bool {
        match p {
            Pattern::Wild(_) => true,
            Pattern::Bind { name, .. } => {
                out.push((name.clone(), value.clone()));
                true
            }
            Pattern::Literal(e) => match &e.kind {
                ExprKind::Int {
                    value: n, negative, ..
                } => {
                    let n = if *negative { -(*n as i128) } else { *n as i128 };
                    matches!(value, Value::Int { value: v, .. } if *v == n)
                }
                ExprKind::Bool(b) => matches!(value, Value::Bool(v) if v == b),
                ExprKind::Float(f) => matches!(value, Value::Float { value: v, .. } if v == f),
                ExprKind::Str(bytes) => matches!(value, Value::Str(v) if v == bytes),
                _ => false,
            },
            Pattern::Range {
                lo, hi, inclusive, ..
            } => {
                let lo = literal_int(lo);
                let hi = literal_int(hi);
                if let (Some(lo), Some(hi), Value::Int { value: v, .. }) = (lo, hi, value) {
                    if *inclusive {
                        *v >= lo && *v <= hi
                    } else {
                        *v >= lo && *v < hi
                    }
                } else {
                    false
                }
            }
            Pattern::Variant { name, payload, .. } => {
                let matches = match value {
                    Value::Enum { variant, .. } => variant == name,
                    Value::Result { ok, .. } => match name.as_str() {
                        "ok" => ok.is_some(),
                        "err" => ok.is_none(),
                        _ => false,
                    },
                    _ => false,
                };
                if !matches {
                    return false;
                }
                if let Some(p) = payload {
                    if let Some(v) = variant_payload(value) {
                        return self.match_pattern_into(p, &v, out);
                    }
                    return false;
                }
                true
            }
            Pattern::Tuple { elems, .. } => {
                if let Value::Tuple(vs) = value {
                    elems.len() == vs.len()
                        && elems
                            .iter()
                            .zip(vs)
                            .all(|(p, v)| self.match_pattern_into(p, v, out))
                } else {
                    false
                }
            }
            Pattern::Struct { fields, .. } => {
                if let Value::Struct { fields: fs, .. } = value {
                    for f in fields {
                        let Some(v) = fs.get(&f.name) else {
                            return false;
                        };
                        let ok = match &f.pattern {
                            Some(p) => self.match_pattern_into(p, v, out),
                            None => {
                                out.push((f.name.clone(), v.clone()));
                                true
                            }
                        };
                        if !ok {
                            return false;
                        }
                    }
                    true
                } else {
                    false
                }
            }
            Pattern::Poison => false,
        }
    }

    fn exec_match(&mut self, value: &Value, arms: &[Arm]) -> R<Flow> {
        for arm in arms {
            if arm.is_else {
                for p in &arm.patterns {
                    if let Some(bindings) = self.match_pattern(p, value) {
                        for (n, v) in bindings {
                            self.set_var(&n, v);
                        }
                    }
                }
                return match &arm.body {
                    ArmBody::Block(b) => self.exec_block(b),
                    ArmBody::Expr(e) => {
                        self.eval_expr(e, None)?;
                        Ok(Flow::Normal)
                    }
                };
            }
            for p in &arm.patterns {
                if let Some(bindings) = self.match_pattern(p, value) {
                    let guard_ok = match &arm.guard {
                        Some(g) => truthy(&self.eval_expr(g, Some(&Type::Bool))?),
                        None => true,
                    };
                    if !guard_ok {
                        continue;
                    }
                    for (n, v) in bindings {
                        self.set_var(&n, v);
                    }
                    return match &arm.body {
                        ArmBody::Block(b) => self.exec_block(b),
                        ArmBody::Expr(e) => {
                            self.eval_expr(e, None)?;
                            Ok(Flow::Normal)
                        }
                    };
                }
            }
        }
        Ok(Flow::Normal)
    }

    fn iter_values(
        &mut self,
        iter: &Expr,
        range_end: Option<&(bool, Expr)>,
        step: Option<&Expr>,
    ) -> R<Vec<Value>> {
        if let Some((inclusive, end)) = range_end {
            let lo = self.eval_expr(iter, Some(&Type::i64()))?;
            let hi = self.eval_expr(end, None)?;
            let step = match step {
                Some(e) => match self.eval_expr(e, None)? {
                    Value::Int { value, .. } => value.max(1),
                    _ => 1,
                },
                None => 1,
            };
            let mut out = Vec::new();
            let mut i = int_of(&lo);
            let end = int_of(&hi);
            if *inclusive {
                while i <= end {
                    out.push(int_value(i, ScalarTy::I64));
                    i += step;
                }
            } else {
                while i < end {
                    out.push(int_value(i, ScalarTy::I64));
                    i += step;
                }
            }
            return Ok(out);
        }
        let v = self.eval_expr(iter, None)?;
        match v {
            Value::Array(items) => Ok(items),
            Value::Slice {
                root,
                start,
                len,
                mutable,
            } => {
                let mut out = Vec::new();
                for i in 0..len {
                    if mutable {
                        out.push(Value::Ref(Place {
                            root: root.clone(),
                            path: vec![Seg::Index(start + i)],
                        }));
                    } else {
                        out.push(read_place(&Place {
                            root: root.clone(),
                            path: vec![Seg::Index(start + i)],
                        }));
                    }
                }
                Ok(out)
            }
            _ => Ok(Vec::new()),
        }
    }

    // ------------------------------------------------------------ expressions

    fn eval_expr(&mut self, expr: &Expr, expected: Option<&Type>) -> R<Value> {
        self.steps += 1;
        match &expr.kind {
            ExprKind::Int {
                value, negative, ..
            } => {
                let n = if *negative {
                    -(*value as i128)
                } else {
                    *value as i128
                };
                let ty = expected_scalar(expected).unwrap_or(ScalarTy::I64);
                Ok(normalize_int(n, ty)
                    .map(|v| int_value(v, ty))
                    .unwrap_or_else(|| int_value(n, ty)))
            }
            ExprKind::Float(v) => {
                let ty = match expected {
                    Some(Type::Float(s)) => *s,
                    _ => ScalarTy::F64,
                };
                Ok(Value::Float { value: *v, ty })
            }
            ExprKind::Str(bytes) => Ok(Value::Str(bytes.clone())),
            ExprKind::CStr(bytes) => Ok(Value::Str(bytes.clone())),
            ExprKind::Bool(b) => Ok(Value::Bool(*b)),
            ExprKind::Ident(name) => {
                if let Some(cell) = self.lookup(name) {
                    return Ok(cell.borrow().clone());
                }
                if let Some(c) = self.program.const_value_in(&self.current_module(), name)
                    && let Some(v) = &c.value
                {
                    return Ok(const_value(v));
                }
                if name == "exec" {
                    return Ok(Value::External("exec".to_string()));
                }
                Ok(Value::Unit)
            }
            ExprKind::Path(p) => Ok(Value::External(p.name().to_string())),
            ExprKind::Paren(inner) => self.eval_expr(inner, expected),
            ExprKind::Unary {
                op,
                expr: inner,
                op_span,
            } => match op {
                UnOp::Neg => {
                    let v = self.eval_expr(inner, expected)?;
                    match v {
                        Value::Int { value, ty } => {
                            if ty.is_signed() {
                                let (min, _) = int_range(ty);
                                if value == min {
                                    return Err(EvalErr::trap("T03", "integer overflow", *op_span));
                                }
                                Ok(int_value(-value, ty))
                            } else {
                                Ok(int_value(wrap_int(-value, ty), ty))
                            }
                        }
                        Value::Float { value, ty } => Ok(Value::Float { value: -value, ty }),
                        other => Ok(other),
                    }
                }
                UnOp::NegWrap => {
                    let v = self.eval_expr(inner, expected)?;
                    match v {
                        Value::Int { value, ty } => Ok(int_value(wrap_int(-value, ty), ty)),
                        other => Ok(other),
                    }
                }
                UnOp::Not => {
                    let v = self.eval_expr(inner, Some(&Type::Bool))?;
                    Ok(Value::Bool(!truthy(&v)))
                }
                UnOp::BitNot => {
                    let v = self.eval_expr(inner, expected)?;
                    match v {
                        Value::Int { value, ty } => Ok(int_value(wrap_int(!value, ty), ty)),
                        other => Ok(other),
                    }
                }
                UnOp::Borrow | UnOp::BorrowMut => {
                    let mutable = *op == UnOp::BorrowMut;
                    if let ExprKind::Slice { base, lo, hi, .. } = &inner.kind {
                        let base_place = self.eval_place(base)?;
                        let lo = int_of(&self.eval_expr(lo, Some(&Type::i64()))?);
                        let hi = int_of(&self.eval_expr(hi, Some(&Type::i64()))?);
                        let len = place_len(&base_place);
                        if lo < 0 || hi < lo || hi > len as i128 {
                            return Err(EvalErr::trap(
                                "T02",
                                format!("slice {lo}..{hi} out of bounds for length {len}"),
                                *op_span,
                            ));
                        }
                        return Ok(Value::Slice {
                            root: base_place.root,
                            start: lo as usize,
                            len: (hi - lo) as usize,
                            mutable,
                        });
                    }
                    let place = self.eval_place(inner)?;
                    Ok(Value::Ref(place))
                }
                UnOp::Try => {
                    let v = self.eval_expr(inner, None)?;
                    match v {
                        Value::Result { ok: Some(v), .. } => Ok(*v),
                        Value::Result { ok: None, err } => {
                            let err = err.map(|e| *e).unwrap_or(Value::Unit);
                            let converted = self.convert_error(err, *op_span)?;
                            Err(EvalErr::Flow(Flow::Fail(converted)))
                        }
                        other => Ok(other),
                    }
                }
            },
            ExprKind::Binary {
                op,
                lhs,
                rhs,
                op_span,
            } => {
                if matches!(op, BinOp::And | BinOp::Or) {
                    let l = self.eval_expr(lhs, Some(&Type::Bool))?;
                    let short = truthy(&l);
                    if (*op == BinOp::And && !short) || (*op == BinOp::Or && short) {
                        return Ok(Value::Bool(short));
                    }
                    let r = self.eval_expr(rhs, Some(&Type::Bool))?;
                    return Ok(Value::Bool(truthy(&r)));
                }
                let (l, r) = if is_literal(lhs) && !is_literal(rhs) {
                    let r = self.eval_expr(rhs, expected)?;
                    let l = self.eval_expr(lhs, Some(&value_type(&r)))?;
                    (l, r)
                } else {
                    let l = self.eval_expr(lhs, expected)?;
                    let r = self.eval_expr(rhs, Some(&value_type(&l)))?;
                    (l, r)
                };
                apply_binary(*op, l, r, *op_span)
            }
            ExprKind::Call { callee, args, .. } => self.eval_call(callee, args, expr.span),
            ExprKind::Index {
                base,
                index,
                bracket,
            } => {
                let b = self.eval_expr(base, None)?;
                let i = int_of(&self.eval_expr(index, Some(&Type::i64()))?);
                index_value(&b, i, *bracket)
            }
            ExprKind::Slice {
                base,
                lo,
                hi,
                bracket,
            } => {
                let place = self.eval_place(base)?;
                let lo = int_of(&self.eval_expr(lo, Some(&Type::i64()))?);
                let hi = int_of(&self.eval_expr(hi, Some(&Type::i64()))?);
                let viewed = read_place(&place);
                if place.path.is_empty()
                    && let Value::Slice {
                        root,
                        start,
                        len,
                        mutable,
                    } = viewed
                {
                    if lo < 0 || hi < lo || hi > len as i128 {
                        return Err(EvalErr::trap(
                            "T02",
                            format!("slice {lo}..{hi} out of bounds for length {len}"),
                            *bracket,
                        ));
                    }
                    return Ok(Value::Slice {
                        root,
                        start: start + lo as usize,
                        len: (hi - lo) as usize,
                        mutable,
                    });
                }
                if place.path.is_empty()
                    && let Value::Str(bytes) = viewed
                {
                    if lo < 0 || hi < lo || hi > bytes.len() as i128 {
                        return Err(EvalErr::trap(
                            "T02",
                            format!("slice {lo}..{hi} out of bounds for length {}", bytes.len()),
                            *bracket,
                        ));
                    }
                    let items: Vec<Value> = bytes
                        .iter()
                        .map(|b| int_value(*b as i128, ScalarTy::U8))
                        .collect();
                    return Ok(Value::Slice {
                        root: Rc::new(RefCell::new(Value::Array(items))),
                        start: lo as usize,
                        len: (hi - lo) as usize,
                        mutable: false,
                    });
                }
                let len = place_len(&place);
                if lo < 0 || hi < lo || hi > len as i128 {
                    return Err(EvalErr::trap(
                        "T02",
                        format!("slice {lo}..{hi} out of bounds for length {len}"),
                        *bracket,
                    ));
                }
                Ok(Value::Slice {
                    root: place.root,
                    start: lo as usize,
                    len: (hi - lo) as usize,
                    mutable: false,
                })
            }
            ExprKind::Field { base, name, .. } => {
                if let ExprKind::Ident(alias) = &base.kind
                    && let Some(crate::check::ResolvedName::Local(canonical)) = self
                        .program
                        .resolve_qualified_name(&self.current_module(), alias, name)
                    && let Some(c) = self.program.consts.get(&canonical)
                    && let Some(v) = &c.value
                {
                    return Ok(const_value(v));
                }
                let b = self.eval_expr(base, None)?;
                self.field_value(&b, name)
            }
            ExprKind::TupleField { base, index, .. } => {
                let b = self.eval_expr(base, None)?;
                match b {
                    Value::Tuple(vs) => Ok(vs.get(*index as usize).cloned().unwrap_or(Value::Unit)),
                    _ => Ok(Value::Unit),
                }
            }
            ExprKind::Deref { base, .. } => {
                let b = self.eval_expr(base, None)?;
                match b {
                    Value::Ref(p) => Ok(read_place(&p)),
                    Value::Atomic(cell) => Ok(cell.borrow().clone()),
                    other => Ok(other),
                }
            }
            ExprKind::TypeApply { base, .. } => self.eval_expr(base, expected),
            ExprKind::Cast { expr: inner, .. } => self.eval_expr(inner, expected),
            ExprKind::StructLit { ty, fields, .. } => {
                let module = self.current_module();
                let name = match self.program.resolve_type_path(&module, ty) {
                    Some(crate::check::ResolvedName::Local(c)) => c,
                    Some(crate::check::ResolvedName::External(c)) => c,
                    None => ty.name().to_string(),
                };
                let mut out = HashMap::new();
                for f in fields {
                    let fty = self.program.defs.structs.get(&name).and_then(|d| {
                        d.fields
                            .iter()
                            .find(|(n, _, _)| n == &f.name)
                            .map(|(_, t, _)| t.clone())
                    });
                    out.insert(f.name.clone(), self.eval_expr(&f.value, fty.as_ref())?);
                }
                Ok(Value::Struct {
                    ty: name,
                    fields: out,
                })
            }
            ExprKind::StructShorthand { fields, .. } => {
                let name = match expected {
                    Some(Type::Struct(n, _)) => n.clone(),
                    _ => String::new(),
                };
                let mut out = HashMap::new();
                for f in fields {
                    out.insert(f.name.clone(), self.eval_expr(&f.value, None)?);
                }
                Ok(Value::Struct {
                    ty: name,
                    fields: out,
                })
            }
            ExprKind::EnumLit {
                path,
                variant,
                payload,
                ..
            } => {
                let ty = match path {
                    Some(p) => {
                        let module = self.current_module();
                        match self.program.resolve_type_path(&module, p) {
                            Some(crate::check::ResolvedName::Local(c)) => c,
                            Some(crate::check::ResolvedName::External(c)) => c,
                            None => p.name().to_string(),
                        }
                    }
                    None => match expected {
                        Some(Type::Enum(n, _)) => n.clone(),
                        Some(Type::Optional(_)) => expected.unwrap().to_string(),
                        Some(Type::Result(..)) => "Result".to_string(),
                        _ => String::new(),
                    },
                };
                let payload = match payload {
                    Some(p) => Some(Box::new(self.eval_expr(p, None)?)),
                    None => None,
                };
                if ty == "Result" || variant == "ok" || variant == "err" {
                    return Ok(if variant == "ok" {
                        Value::Result {
                            ok: payload,
                            err: None,
                        }
                    } else {
                        Value::Result {
                            ok: None,
                            err: payload,
                        }
                    });
                }
                Ok(Value::Enum {
                    ty,
                    variant: variant.clone(),
                    payload,
                })
            }
            ExprKind::ArrayLit { elem, elems, .. } => {
                let elem_ty = self.resolve_ty(elem);
                let mut out = Vec::new();
                for e in elems {
                    out.push(self.eval_expr(e, Some(&elem_ty))?);
                }
                Ok(Value::Array(out))
            }
            ExprKind::Range { lo, hi, .. } => {
                let lo = self.eval_expr(lo, None)?;
                let hi = self.eval_expr(hi, None)?;
                Ok(Value::Tuple(vec![lo, hi]))
            }
            ExprKind::Tuple { elems, .. } => {
                let mut out = Vec::new();
                for e in elems {
                    out.push(self.eval_expr(e, None)?);
                }
                Ok(Value::Tuple(out))
            }
            ExprKind::If {
                cond,
                then,
                otherwise,
                ..
            } => {
                let go = if let Some(pattern) = &cond.let_pattern {
                    let v = self.eval_expr(&cond.expr, None)?;
                    if let Some(bindings) = self.match_pattern(pattern, &v) {
                        for (n, v) in bindings {
                            self.set_var(&n, v);
                        }
                        true
                    } else {
                        false
                    }
                } else {
                    truthy(&self.eval_expr(&cond.expr, Some(&Type::Bool))?)
                };
                if go {
                    self.eval_expr(then, expected)
                } else {
                    self.eval_expr(otherwise, expected)
                }
            }
            ExprKind::Match {
                scrutinee, arms, ..
            } => {
                let v = self.eval_expr(scrutinee, None)?;
                for arm in arms {
                    if arm.is_else {
                        for p in &arm.patterns {
                            if let Some(bindings) = self.match_pattern(p, &v) {
                                for (n, v) in bindings {
                                    self.set_var(&n, v);
                                }
                            }
                        }
                        return match &arm.body {
                            ArmBody::Block(b) => {
                                let flow = self.exec_block(b)?;
                                match flow {
                                    Flow::Return(v) => Ok(v.unwrap_or(Value::Unit)),
                                    Flow::Fail(e) => Err(EvalErr::Flow(Flow::Fail(e))),
                                    _ => Ok(Value::Unit),
                                }
                            }
                            ArmBody::Expr(e) => self.eval_expr(e, expected),
                        };
                    }
                    for p in &arm.patterns {
                        if let Some(bindings) = self.match_pattern(p, &v) {
                            let guard_ok = match &arm.guard {
                                Some(g) => truthy(&self.eval_expr(g, Some(&Type::Bool))?),
                                None => true,
                            };
                            if !guard_ok {
                                continue;
                            }
                            for (n, v) in bindings {
                                self.set_var(&n, v);
                            }
                            return match &arm.body {
                                ArmBody::Block(b) => {
                                    let flow = self.exec_block(b)?;
                                    match flow {
                                        Flow::Return(v) => Ok(v.unwrap_or(Value::Unit)),
                                        Flow::Fail(e) => Err(EvalErr::Flow(Flow::Fail(e))),
                                        _ => Ok(Value::Unit),
                                    }
                                }
                                ArmBody::Expr(e) => self.eval_expr(e, expected),
                            };
                        }
                    }
                }
                Ok(Value::Unit)
            }
            ExprKind::Catch {
                value,
                binding,
                handler,
                ..
            } => {
                let v = self.eval_expr(value, None)?;
                match v {
                    Value::Result { ok: Some(v), .. } => Ok(*v),
                    Value::Result { ok: None, err } => {
                        let err = err.map(|e| *e).unwrap_or(Value::Unit);
                        self.run_handler(handler, binding.as_deref(), err, expected)
                    }
                    other => Ok(other),
                }
            }
            ExprKind::Orelse { value, handler, .. } => {
                let v = self.eval_expr(value, None)?;
                match &v {
                    Value::Enum {
                        variant, payload, ..
                    } if variant == "none" => {
                        self.run_handler(handler, None, Value::Unit, expected)
                    }
                    Value::Enum {
                        variant, payload, ..
                    } if variant == "some" => Ok(payload
                        .as_ref()
                        .map(|p| (**p).clone())
                        .unwrap_or(Value::Unit)),
                    other => Ok(other.clone()),
                }
            }
            ExprKind::Builtin {
                name,
                type_arg,
                args,
            } => self.eval_builtin(name, type_arg.as_deref(), args, expr.span, expected),
            ExprKind::Spawn { call, scope, .. } => {
                let _ = self.eval_expr(scope, None)?;
                let v = self.eval_expr(call, None)?;
                Ok(Value::Task(Box::new(v)))
            }
            ExprKind::Dispatch {
                call,
                domain,
                executor,
                ..
            } => {
                let _ = self.eval_expr(executor, None);
                self.dispatch(call, domain)?;
                Ok(Value::Unit)
            }
            ExprKind::Unsafe { body, .. } => self.eval_expr(body, expected),
            ExprKind::Closure { .. } => Ok(Value::Unit),
            ExprKind::Poison => Ok(Value::Unit),
        }
    }

    fn run_handler(
        &mut self,
        handler: &Handler,
        binding: Option<&str>,
        err: Value,
        expected: Option<&Type>,
    ) -> R<Value> {
        match handler {
            Handler::Expr(e) => self.eval_expr(e, expected),
            Handler::Block(b) => {
                if let Some(name) = binding {
                    self.set_var(name, err);
                }
                let flow = self.exec_block(b)?;
                match flow {
                    Flow::Return(v) => Ok(v.unwrap_or(Value::Unit)),
                    Flow::Fail(e) => Err(EvalErr::Flow(Flow::Fail(e))),
                    _ => Ok(Value::Unit),
                }
            }
        }
    }

    fn convert_error(&mut self, err: Value, span: Span) -> R<Value> {
        let Some(expected) = self.frame_error() else {
            return Err(EvalErr::Flow(Flow::Fail(err)));
        };
        if value_declared_type(&err).as_deref() == Some(&expected.to_string()) {
            return Ok(err);
        }
        if let Value::Result { err: Some(e), .. } = &err {
            return Ok((**e).clone());
        }
        if let Type::Enum(name, _) = &expected
            && let Some(def) = self.program.defs.enums.get(name)
        {
            let key = value_declared_type(&err).unwrap_or_default();
            if let Some(variant) = def.from.get(&key) {
                return Err(EvalErr::Flow(Flow::Fail(Value::Enum {
                    ty: name.clone(),
                    variant: variant.clone(),
                    payload: Some(Box::new(err)),
                })));
            }
            // The error may already be the right enum type.
            if value_declared_type(&err).as_deref() == Some(name) {
                return Ok(err);
            }
        }
        let _ = span;
        Err(EvalErr::Flow(Flow::Fail(err)))
    }

    fn eval_call(&mut self, callee: &Expr, args: &[Expr], span: Span) -> R<Value> {
        match &callee.kind {
            ExprKind::Ident(name) => {
                if let Some(f) = self
                    .program
                    .function_in(&self.current_module(), name)
                    .cloned()
                {
                    let mut vals = Vec::new();
                    for (i, a) in args.iter().enumerate() {
                        let expected = f.signature_params.get(i);
                        vals.push(self.eval_expr(a, expected)?);
                    }
                    return self.call_function(&f, vals);
                }
                if let Some(cell) = self.lookup(name) {
                    let v = cell.borrow().clone();
                    if let Value::Task(inner) = v {
                        return Ok(*inner);
                    }
                }
                Ok(Value::Unit)
            }
            ExprKind::Field { base, name, .. } => self.eval_method_call(base, name, args, span),
            ExprKind::TypeApply { base, .. } => self.eval_call(base, args, span),
            _ => {
                let _ = self.eval_expr(callee, None)?;
                Ok(Value::Unit)
            }
        }
    }

    fn eval_method_call(&mut self, base: &Expr, name: &str, args: &[Expr], span: Span) -> R<Value> {
        if let ExprKind::Ident(alias) = &base.kind {
            if let Some(f) = self
                .program
                .function_qualified(&self.current_module(), alias, name)
                .cloned()
            {
                let mut vals = Vec::new();
                for (i, a) in args.iter().enumerate() {
                    vals.push(self.eval_expr(a, f.signature_params.get(i))?);
                }
                return self.call_function(&f, vals);
            }
            if matches!(
                self.program
                    .resolve_qualified_name(&self.current_module(), alias, name),
                Some(crate::check::ResolvedName::External(_))
            ) {
                for a in args {
                    self.eval_expr(a, None)?;
                }
                return Ok(Value::Unit);
            }
        }
        // Path calls: `Atomic[T].init(v)`, `T.make(...)` and enum variants.
        if let ExprKind::Path(p) = &base.kind {
            if p.name() == "Atomic" && name == "init" {
                let v = args
                    .first()
                    .map(|a| self.eval_expr(a, None))
                    .transpose()?
                    .unwrap_or(Value::Unit);
                return Ok(Value::Atomic(Rc::new(RefCell::new(v))));
            }
            let module = self.current_module();
            let resolved = self.program.resolve_type_path(&module, p);
            if let Some(crate::check::ResolvedName::Local(canonical)) = resolved {
                if let Some(def) = self.program.defs.enums.get(&canonical)
                    && def.variants.iter().any(|(v, _)| v == name)
                {
                    let payload = match args.first() {
                        Some(a) => Some(Box::new(self.eval_expr(a, None)?)),
                        None => None,
                    };
                    return Ok(Value::Enum {
                        ty: canonical,
                        variant: name.to_string(),
                        payload,
                    });
                }
                let ty = if self.program.defs.structs.contains_key(&canonical) {
                    Type::Struct(canonical.clone(), Vec::new())
                } else if self.program.defs.enums.contains_key(&canonical) {
                    Type::Enum(canonical.clone(), Vec::new())
                } else {
                    Type::External(canonical.clone())
                };
                if let Some(f) = self.program.method(&ty, name).cloned() {
                    let mut vals = Vec::new();
                    for (i, a) in args.iter().enumerate() {
                        vals.push(self.eval_expr(a, f.signature_params.get(i))?);
                    }
                    return self.call_function(&f, vals);
                }
            }
            for a in args {
                self.eval_expr(a, None)?;
            }
            return Ok(Value::Unit);
        }
        let receiver_value = self.eval_expr(base, None)?;
        // Built-in methods.
        match (&receiver_value, name) {
            (Value::Task(inner), "join") => return Ok((**inner).clone()),
            (Value::Atomic(cell), "load") => return Ok(cell.borrow().clone()),
            (Value::Atomic(cell), "store") => {
                if let Some(a) = args.first() {
                    let v = self.eval_expr(a, None)?;
                    *cell.borrow_mut() = v;
                }
                return Ok(Value::Unit);
            }
            (Value::Atomic(cell), "add") => {
                if let Some(a) = args.first() {
                    let v = self.eval_expr(a, None)?;
                    let old = cell.borrow().clone();
                    let new = apply_binary(BinOp::Add, old, v, span)?;
                    *cell.borrow_mut() = new;
                }
                return Ok(Value::Unit);
            }
            (Value::External(e), _) if e == "Scope" => return Ok(Value::Unit),
            _ => {}
        }
        let recv_ty = self.value_type(&receiver_value);
        let Some(f) = self.program.method(&recv_ty, name).cloned() else {
            return Ok(Value::Unit);
        };
        let mut vals = Vec::new();
        // Receiver.
        let self_ty = f.signature_params.first().cloned().unwrap_or(Type::Void);
        let receiver = match &self_ty {
            Type::Borrow { .. } | Type::Slice { .. } => {
                let place = self.eval_place(base)?;
                Value::Ref(place)
            }
            _ => receiver_value,
        };
        vals.push(receiver);
        for (i, a) in args.iter().enumerate() {
            let expected = f.signature_params.get(i + 1);
            vals.push(self.eval_expr(a, expected)?);
        }
        self.call_function(&f, vals)
    }

    fn dispatch(&mut self, call: &Expr, domain: &Expr) -> R<()> {
        let ExprKind::Call { callee, args, .. } = &call.kind else {
            return Ok(());
        };
        let function = match &callee.kind {
            ExprKind::Ident(n) => self.program.function_in(&self.current_module(), n).cloned(),
            _ => None,
        };
        let Some(f) = function else {
            return Ok(());
        };
        let mut base_args = Vec::new();
        for a in args {
            base_args.push(self.eval_expr(a, None)?);
        }
        let ranges = match &domain.kind {
            ExprKind::Range {
                lo, hi, inclusive, ..
            } => {
                let lo = int_of(&self.eval_expr(lo, None)?);
                let hi = int_of(&self.eval_expr(hi, None)?);
                vec![(lo, hi, *inclusive)]
            }
            _ => {
                let v = self.eval_expr(domain, None)?;
                match v {
                    Value::Tuple(items) if items.len() == 2 => {
                        vec![(int_of(&items[0]), int_of(&items[1]), false)]
                    }
                    _ => vec![(0, 0, false)],
                }
            }
        };
        for (lo, hi, inclusive) in ranges {
            let end = if inclusive { hi + 1 } else { hi };
            let mut i = lo;
            while i < end {
                let mut vals = vec![int_value(i, ScalarTy::I64)];
                vals.extend(base_args.iter().cloned());
                self.call_function(&f, vals)?;
                i += 1;
            }
        }
        Ok(())
    }

    // -------------------------------------------------------------- builtins

    fn eval_builtin(
        &mut self,
        name: &str,
        type_arg: Option<&Ty>,
        args: &[Expr],
        span: Span,
        expected: Option<&Type>,
    ) -> R<Value> {
        match name {
            "assert" => {
                let cond = args
                    .first()
                    .map(|a| self.eval_expr(a, Some(&Type::Bool)))
                    .transpose()?
                    .unwrap_or(Value::Bool(false));
                if !truthy(&cond) {
                    let text = args
                        .first()
                        .map(|a| self.source.text_of(a.span).trim().to_string())
                        .unwrap_or_else(|| "false".to_string());
                    let extra = args.get(1).and_then(|a| match &a.kind {
                        ExprKind::Str(b) => Some(format!(": {}", String::from_utf8_lossy(b))),
                        _ => None,
                    });
                    return Err(EvalErr::trap(
                        "T08",
                        format!("assertion failed: {text}{}", extra.unwrap_or_default()),
                        span,
                    ));
                }
                Ok(Value::Unit)
            }
            "panic" => {
                let msg = args
                    .first()
                    .and_then(|a| match &a.kind {
                        ExprKind::Str(b) => Some(String::from_utf8_lossy(b).into_owned()),
                        _ => None,
                    })
                    .unwrap_or_default();
                Err(EvalErr::trap("T09", msg, span))
            }
            "unreachable" => Err(EvalErr::trap("T10", "entered unreachable code", span)),
            "debug" => Ok(Value::Unit),
            "zero" => {
                let t = type_arg
                    .map(|t| self.resolve_ty(t))
                    .or_else(|| expected.cloned())
                    .unwrap_or(Type::Void);
                Ok(zero_value(&t))
            }
            "wrap" | "sat" | "narrow" | "nearest" => {
                let t = type_arg.map(|t| self.resolve_ty(t)).unwrap_or(Type::Error);
                let x = args
                    .first()
                    .map(|a| self.eval_expr(a, None))
                    .transpose()?
                    .unwrap_or(Value::Unit);
                self.convert_builtin(name, t, x, span)
            }
            "bits" => {
                let t = type_arg.map(|t| self.resolve_ty(t)).unwrap_or(Type::Error);
                let x = args
                    .first()
                    .map(|a| self.eval_expr(a, None))
                    .transpose()?
                    .unwrap_or(Value::Unit);
                bits_value(t, x)
            }
            "sizeof" | "alignof" | "offsetof" => Ok(int_value(8, ScalarTy::I64)),
            "lanes" => {
                let t = type_arg.map(|t| self.resolve_ty(t)).unwrap_or(Type::Error);
                let n = match t {
                    Type::Vector(v) => v.lanes as i128,
                    _ => 0,
                };
                Ok(int_value(n, ScalarTy::I64))
            }
            "splat" => {
                let t = type_arg.map(|t| self.resolve_ty(t)).unwrap_or(Type::Error);
                let x = args
                    .first()
                    .map(|a| self.eval_expr(a, None))
                    .transpose()?
                    .unwrap_or(Value::Unit);
                if let Type::Vector(v) = t {
                    Ok(Value::Vector {
                        elem: v.elem,
                        lanes: vec![x; v.lanes as usize],
                    })
                } else {
                    Ok(Value::Unit)
                }
            }
            "load" | "load_tail" => {
                let t = type_arg.map(|t| self.resolve_ty(t)).unwrap_or(Type::Error);
                let data = args
                    .first()
                    .map(|a| self.eval_expr(a, None))
                    .transpose()?
                    .unwrap_or(Value::Unit);
                let i = args
                    .get(1)
                    .map(|a| self.eval_expr(a, Some(&Type::i64())))
                    .transpose()?
                    .map(|v| int_of(&v) as usize)
                    .unwrap_or(0);
                let lanes = match t {
                    Type::Vector(v) => v.lanes as usize,
                    _ => 0,
                };
                let elem_ty = match t {
                    Type::Vector(v) => v.elem,
                    _ => ScalarTy::I64,
                };
                match data {
                    Value::Slice {
                        root, start, len, ..
                    } => {
                        if i + lanes > len {
                            return Err(EvalErr::trap(
                                "T14",
                                format!("vector access at {i} out of bounds for length {len}"),
                                span,
                            ));
                        }
                        let v = root.borrow();
                        let mut out = Vec::new();
                        for k in 0..lanes {
                            out.push(read_path(&v, &[Seg::Index(start + i + k)]));
                        }
                        drop(v);
                        Ok(Value::Vector {
                            elem: elem_ty,
                            lanes: out,
                        })
                    }
                    Value::Array(items) => {
                        if i + lanes > items.len() {
                            return Err(EvalErr::trap(
                                "T14",
                                format!(
                                    "vector access at {i} out of bounds for length {}",
                                    items.len()
                                ),
                                span,
                            ));
                        }
                        Ok(Value::Vector {
                            elem: elem_ty,
                            lanes: items[i..i + lanes].to_vec(),
                        })
                    }
                    _ => Ok(Value::Vector {
                        elem: elem_ty,
                        lanes: vec![Value::Unit; lanes],
                    }),
                }
            }
            "store" | "store_tail" => {
                let data = args.first().map(|a| self.eval_place(a)).transpose()?;
                let i = args
                    .get(1)
                    .map(|a| self.eval_expr(a, Some(&Type::i64())))
                    .transpose()?
                    .map(|v| int_of(&v))
                    .unwrap_or(0);
                let v = args
                    .get(2)
                    .map(|a| self.eval_expr(a, None))
                    .transpose()?
                    .unwrap_or(Value::Unit);
                if let (Some(place), Value::Vector { lanes, .. }) = (data, v) {
                    let len = place_len(&place);
                    if i < 0 || i as usize + lanes.len() > len {
                        return Err(EvalErr::trap(
                            "T14",
                            format!("vector access at {i} out of bounds for length {len}"),
                            span,
                        ));
                    }
                    for (k, lane) in lanes.into_iter().enumerate() {
                        let mut p = place.clone();
                        if let Some(Seg::Index(x)) = p.path.last() {
                            let base = *x + k;
                            *p.path.last_mut().unwrap() = Seg::Index(base);
                        } else {
                            p.path.push(Seg::Index(i as usize + k));
                        }
                        write_place(&p, lane);
                    }
                }
                Ok(Value::Unit)
            }
            "count" => {
                let v = args
                    .first()
                    .map(|a| self.eval_expr(a, None))
                    .transpose()?
                    .unwrap_or(Value::Unit);
                let n = match v {
                    Value::Mask(bs) => bs.iter().filter(|b| **b).count() as i128,
                    _ => 0,
                };
                Ok(int_value(n, ScalarTy::I64))
            }
            "reduce_add" | "reduce_min" | "reduce_max" | "reduce_and" | "reduce_or" => {
                let v = args
                    .first()
                    .map(|a| self.eval_expr(a, None))
                    .transpose()?
                    .unwrap_or(Value::Unit);
                let mut lanes = match v {
                    Value::Vector { lanes, .. } => lanes,
                    _ => return Ok(Value::Unit),
                };
                if lanes.is_empty() {
                    return Ok(Value::Unit);
                }
                let mut acc = lanes.remove(0);
                for lane in lanes {
                    acc = match name {
                        "reduce_min" => {
                            if cmp_values(&lane, &acc) == Some(std::cmp::Ordering::Less) {
                                lane
                            } else {
                                acc
                            }
                        }
                        "reduce_max" => {
                            if cmp_values(&lane, &acc) == Some(std::cmp::Ordering::Greater) {
                                lane
                            } else {
                                acc
                            }
                        }
                        _ => apply_binary(BinOp::Add, acc, lane, span)?,
                    };
                }
                Ok(acc)
            }
            "select" => {
                let c = args
                    .first()
                    .map(|a| self.eval_expr(a, None))
                    .transpose()?
                    .unwrap_or(Value::Bool(false));
                let a = args
                    .get(1)
                    .map(|a| self.eval_expr(a, None))
                    .transpose()?
                    .unwrap_or(Value::Unit);
                let b = args
                    .get(2)
                    .map(|a| self.eval_expr(a, None))
                    .transpose()?
                    .unwrap_or(Value::Unit);
                match c {
                    Value::Mask(mask) => match (a, b) {
                        (Value::Vector { elem, lanes: x }, Value::Vector { lanes: y, .. }) => {
                            let _ = elem;
                            Ok(Value::Vector {
                                elem: ScalarTy::I64,
                                lanes: x
                                    .into_iter()
                                    .zip(y)
                                    .zip(mask)
                                    .map(|((x, y), m)| if m { x } else { y })
                                    .collect(),
                            })
                        }
                        _ => Ok(Value::Unit),
                    },
                    v if truthy(&v) => Ok(a),
                    _ => Ok(b),
                }
            }
            "insert" => Ok(args
                .first()
                .map(|a| self.eval_expr(a, None))
                .transpose()?
                .unwrap_or(Value::Unit)),
            "shuffle" => Ok(args
                .first()
                .map(|a| self.eval_expr(a, None))
                .transpose()?
                .unwrap_or(Value::Unit)),
            "fma" | "min" | "max" | "sqrt" | "abs" | "floor" | "ceil" | "round" | "rem" => {
                let mut vals = Vec::new();
                for a in args {
                    vals.push(self.eval_expr(a, None)?);
                }
                if vals.is_empty() {
                    return Ok(Value::Unit);
                }
                let a = vals.remove(0);
                match name {
                    "fma" if vals.len() >= 2 => apply_binary(
                        BinOp::Add,
                        apply_binary(BinOp::Mul, a, vals.remove(0), span)?,
                        vals.remove(0),
                        span,
                    ),
                    "sqrt" => match a {
                        Value::Float { value, ty } => Ok(Value::Float {
                            value: value.sqrt(),
                            ty,
                        }),
                        _ => Ok(a),
                    },
                    "abs" => match a {
                        Value::Int { value, ty } => Ok(int_value(value.abs(), ty)),
                        Value::Float { value, ty } => Ok(Value::Float {
                            value: value.abs(),
                            ty,
                        }),
                        _ => Ok(a),
                    },
                    "floor" => float_unary(a, f64::floor),
                    "ceil" => float_unary(a, f64::ceil),
                    "round" => float_unary(a, f64::round),
                    "min" if !vals.is_empty() => Ok(
                        if cmp_values(&vals[0], &a) == Some(std::cmp::Ordering::Less) {
                            vals.remove(0)
                        } else {
                            a
                        },
                    ),
                    "max" if !vals.is_empty() => Ok(
                        if cmp_values(&vals[0], &a) == Some(std::cmp::Ordering::Greater) {
                            vals.remove(0)
                        } else {
                            a
                        },
                    ),
                    "rem" if !vals.is_empty() => apply_binary(BinOp::Rem, a, vals.remove(0), span),
                    _ => Ok(a),
                }
            }
            "forget" => {
                if let Some(a) = args.first() {
                    self.eval_expr(a, None)?;
                }
                Ok(Value::Unit)
            }
            "replace" => {
                if let Some(a) = args.first() {
                    let p = self.eval_place(a)?;
                    let old = read_place(&p);
                    if let Some(v) = args.get(1) {
                        let new = self.eval_expr(v, None)?;
                        write_place(&p, new);
                    }
                    return Ok(old);
                }
                Ok(Value::Unit)
            }
            "swap" => {
                if args.len() == 2 {
                    let a = self.eval_place(&args[0])?;
                    let b = self.eval_place(&args[1])?;
                    let av = read_place(&a);
                    let bv = read_place(&b);
                    write_place(&a, bv);
                    write_place(&b, av);
                }
                Ok(Value::Unit)
            }
            "enum" => Ok(Value::Unit),
            "target" => Ok(Value::External("target".to_string())),
            "format" => Ok(Value::Unit),
            _ => Ok(Value::Unit),
        }
    }

    fn convert_builtin(&self, name: &str, to: Type, x: Value, span: Span) -> R<Value> {
        let target = match &to {
            Type::Int(s) | Type::Float(s) => *s,
            _ => return Ok(x),
        };
        match (name, x) {
            ("nearest", Value::Int { value, .. }) => {
                if target.is_float() {
                    Ok(Value::Float {
                        value: value as f64,
                        ty: target,
                    })
                } else {
                    Ok(int_value(value, target))
                }
            }
            ("nearest", Value::Float { value, .. }) => Ok(Value::Float {
                value: value as f32 as f64,
                ty: target,
            }),
            ("wrap", Value::Int { value, .. }) => Ok(int_value(wrap_int(value, target), target)),
            ("sat", Value::Int { value, .. }) => {
                let (min, max) = int_range(target);
                Ok(int_value(value.clamp(min, max), target))
            }
            ("sat", Value::Float { value, .. }) => {
                if value.is_nan() {
                    return Ok(int_value(0, target));
                }
                let (min, max) = int_range(target);
                let v = if value < min as f64 {
                    min
                } else if value > max as f64 {
                    max
                } else {
                    value.trunc() as i128
                };
                Ok(int_value(v, target))
            }
            ("narrow", Value::Int { value, .. }) => {
                let (min, max) = int_range(target);
                if value < min || value > max {
                    return Err(EvalErr::trap(
                        "T07",
                        format!("value does not fit in {}", target.name()),
                        span,
                    ));
                }
                Ok(int_value(value, target))
            }
            ("narrow", Value::Float { value, .. }) => {
                if value.is_nan() {
                    return Err(EvalErr::trap(
                        "T07",
                        format!("value does not fit in {}", target.name()),
                        span,
                    ));
                }
                let (min, max) = int_range(target);
                let v = value.trunc() as i128;
                if v < min || v > max {
                    return Err(EvalErr::trap(
                        "T07",
                        format!("value does not fit in {}", target.name()),
                        span,
                    ));
                }
                Ok(int_value(v, target))
            }
            (_, x) => Ok(x),
        }
    }

    // ------------------------------------------------------------- places

    fn eval_place(&mut self, e: &Expr) -> R<Place> {
        match &e.kind {
            ExprKind::Ident(name) => {
                if let Some(cell) = self.lookup(name) {
                    return Ok(Place {
                        root: cell,
                        path: Vec::new(),
                    });
                }
                Err(EvalErr::trap(
                    "T09",
                    format!("undefined place `{name}`"),
                    e.span,
                ))
            }
            ExprKind::Paren(x) => self.eval_place(x),
            ExprKind::Field { base, name, .. } => {
                let b = self.eval_expr(base, None)?;
                match b {
                    Value::Ref(p) => {
                        let mut place = p;
                        place.path.push(Seg::Field(name.clone()));
                        Ok(place)
                    }
                    Value::Atomic(cell) => Ok(Place {
                        root: cell,
                        path: vec![Seg::Field(name.clone())],
                    }),
                    _ => {
                        let p = self.eval_place(base)?;
                        let mut place = p;
                        place.path.push(Seg::Field(name.clone()));
                        Ok(place)
                    }
                }
            }
            ExprKind::TupleField { base, index, .. } => {
                let mut p = self.eval_place(base)?;
                p.path.push(Seg::Index(*index as usize));
                Ok(p)
            }
            ExprKind::Index { base, index, .. } => {
                let b = self.eval_expr(base, None)?;
                let i = int_of(&self.eval_expr(index, Some(&Type::i64()))?);
                match b {
                    Value::Slice {
                        root, start, len, ..
                    } => {
                        if i < 0 || i as usize >= len {
                            return Err(EvalErr::trap(
                                "T01",
                                format!("index {i} out of bounds for length {len}"),
                                index.span,
                            ));
                        }
                        Ok(Place {
                            root,
                            path: vec![Seg::Index(start + i as usize)],
                        })
                    }
                    _ => {
                        let p = self.eval_place(base)?;
                        let len = place_len(&p);
                        if i < 0 || i as usize >= len {
                            return Err(EvalErr::trap(
                                "T01",
                                format!("index {i} out of bounds for length {len}"),
                                index.span,
                            ));
                        }
                        let mut p = p;
                        p.path.push(Seg::Index(i as usize));
                        Ok(p)
                    }
                }
            }
            ExprKind::Deref { base, .. } => {
                let b = self.eval_expr(base, None)?;
                match b {
                    Value::Ref(p) => Ok(p),
                    Value::Atomic(cell) => Ok(Place {
                        root: cell,
                        path: Vec::new(),
                    }),
                    _ => self.eval_place(base),
                }
            }
            _ => Err(EvalErr::trap("T09", "not a place", e.span)),
        }
    }

    fn field_value(&mut self, v: &Value, name: &str) -> R<Value> {
        match v {
            Value::Struct { fields, .. } => Ok(fields.get(name).cloned().unwrap_or(Value::Unit)),
            Value::Ref(p) => {
                let mut p = p.clone();
                p.path.push(Seg::Field(name.to_string()));
                Ok(read_place(&p))
            }
            Value::Slice { len, .. } if name == "len" => Ok(int_value(*len as i128, ScalarTy::I64)),
            Value::Str(bytes) if name == "len" => Ok(int_value(bytes.len() as i128, ScalarTy::I64)),
            Value::Array(items) if name == "len" => {
                Ok(int_value(items.len() as i128, ScalarTy::I64))
            }
            Value::External(_) => Ok(Value::Unit),
            _ => Ok(Value::Unit),
        }
    }

    fn value_type(&self, v: &Value) -> Type {
        match v {
            Value::Struct { ty, .. } => Type::Struct(ty.clone(), Vec::new()),
            Value::Enum { ty, .. } => Type::Enum(ty.clone(), Vec::new()),
            Value::Int { ty, .. } => Type::Int(*ty),
            Value::Float { ty, .. } => Type::Float(*ty),
            Value::Bool(_) => Type::Bool,
            Value::Slice { .. } | Value::Str(_) => Type::Slice {
                mutable: false,
                elem: Box::new(Type::u8()),
            },
            Value::Array(items) => Type::Array(
                items.len() as i64,
                Box::new(
                    items
                        .first()
                        .map(|v| self.value_type(v))
                        .unwrap_or(Type::Error),
                ),
            ),
            Value::Vector { elem, lanes } => Type::Vector(crate::token::VectorTy {
                elem: *elem,
                lanes: lanes.len() as u16,
            }),
            _ => Type::Error,
        }
    }
}

// ------------------------------------------------------------------- helpers

fn int_value(value: i128, ty: ScalarTy) -> Value {
    Value::Int { value, ty }
}

fn int_of(v: &Value) -> i128 {
    match v {
        Value::Int { value, .. } => *value,
        Value::Bool(b) => *b as i128,
        _ => 0,
    }
}

fn truthy(v: &Value) -> bool {
    matches!(v, Value::Bool(true))
}

fn expected_scalar(t: Option<&Type>) -> Option<ScalarTy> {
    match t {
        Some(Type::Int(s)) | Some(Type::Float(s)) => Some(*s),
        _ => None,
    }
}

fn value_type(v: &Value) -> Type {
    match v {
        Value::Int { ty, .. } => Type::Int(*ty),
        Value::Float { ty, .. } => Type::Float(*ty),
        Value::Bool(_) => Type::Bool,
        _ => Type::Error,
    }
}

fn int_range(ty: ScalarTy) -> (i128, i128) {
    let bits = ty.int_bits().unwrap_or(64);
    if ty.is_signed() {
        (-(1i128 << (bits - 1)), (1i128 << (bits - 1)) - 1)
    } else {
        (0, (1i128 << bits) - 1)
    }
}

fn wrap_int(n: i128, ty: ScalarTy) -> i128 {
    let bits = ty.int_bits().unwrap_or(64);
    let mask = (1i128 << bits) - 1;
    let mut v = n & mask;
    if ty.is_signed() && v >= (1i128 << (bits - 1)) {
        v -= 1i128 << bits;
    }
    v
}

fn wrap_arith(x: i128, y: i128, ty: ScalarTy, op: BinOp) -> i128 {
    let bits = ty.int_bits().unwrap_or(64);
    let mask = (1u128 << bits) - 1;
    let ux = (x as u128) & mask;
    let uy = (y as u128) & mask;
    let raw = match op {
        BinOp::AddWrap => ux.wrapping_add(uy),
        BinOp::SubWrap => ux.wrapping_sub(uy),
        BinOp::MulWrap => ux.wrapping_mul(uy),
        _ => return wrap_int(0, ty),
    } & mask;
    if ty.is_signed() && raw >= (1u128 << (bits - 1)) {
        raw as i128 - (1i128 << bits)
    } else {
        raw as i128
    }
}

fn normalize_int(n: i128, ty: ScalarTy) -> Option<i128> {
    let (min, max) = int_range(ty);
    if n < min || n > max { None } else { Some(n) }
}

fn const_value(v: &ConstVal) -> Value {
    match v {
        ConstVal::Int(n) => int_value(*n, ScalarTy::I64),
        ConstVal::Float(f) => Value::Float {
            value: *f,
            ty: ScalarTy::F64,
        },
        ConstVal::Bool(b) => Value::Bool(*b),
        ConstVal::Str(b) => Value::Str(b.clone()),
    }
}

fn assign_bin(op: AssignOp) -> BinOp {
    match op {
        AssignOp::Assign => BinOp::Add,
        AssignOp::Add => BinOp::Add,
        AssignOp::Sub => BinOp::Sub,
        AssignOp::Mul => BinOp::Mul,
        AssignOp::Div => BinOp::Div,
        AssignOp::Rem => BinOp::Rem,
        AssignOp::AddWrap => BinOp::AddWrap,
        AssignOp::SubWrap => BinOp::SubWrap,
        AssignOp::MulWrap => BinOp::MulWrap,
        AssignOp::AddSat => BinOp::AddSat,
        AssignOp::SubSat => BinOp::SubSat,
        AssignOp::MulSat => BinOp::MulSat,
        AssignOp::BitAnd => BinOp::BitAnd,
        AssignOp::BitOr => BinOp::BitOr,
        AssignOp::BitXor => BinOp::BitXor,
        AssignOp::Shl => BinOp::Shl,
        AssignOp::Shr => BinOp::Shr,
    }
}

fn is_literal(e: &Expr) -> bool {
    matches!(e.kind, ExprKind::Int { .. } | ExprKind::Float(_))
}

fn literal_int(p: &Pattern) -> Option<i128> {
    match p {
        Pattern::Literal(Expr {
            kind: ExprKind::Int {
                value, negative, ..
            },
            ..
        }) => Some(if *negative {
            -(*value as i128)
        } else {
            *value as i128
        }),
        _ => None,
    }
}

fn variant_payload(v: &Value) -> Option<Value> {
    match v {
        Value::Enum { payload, .. } => payload.as_ref().map(|p| (**p).clone()),
        Value::Result { ok, err } => ok
            .as_ref()
            .map(|p| (**p).clone())
            .or_else(|| err.as_ref().map(|p| (**p).clone())),
        _ => None,
    }
}

fn value_declared_type(v: &Value) -> Option<String> {
    match v {
        Value::Enum { ty, .. } => Some(ty.clone()),
        Value::Struct { ty, .. } => Some(ty.clone()),
        _ => None,
    }
}

fn zero_value(t: &Type) -> Value {
    match t {
        Type::Bool => Value::Bool(false),
        Type::Int(s) => int_value(0, *s),
        Type::Float(s) => Value::Float { value: 0.0, ty: *s },
        Type::Optional(_) => Value::Enum {
            ty: t.to_string(),
            variant: "none".to_string(),
            payload: None,
        },
        Type::Array(n, e) => Value::Array(vec![zero_value(e); (*n).max(0) as usize]),
        Type::Tuple(es) => Value::Tuple(es.iter().map(zero_value).collect()),
        Type::Struct(name, _) => Value::Struct {
            ty: name.clone(),
            fields: HashMap::new(),
        },
        Type::Vector(v) => Value::Vector {
            elem: v.elem,
            lanes: vec![zero_value(&Type::Int(v.elem)); v.lanes as usize],
        },
        _ => Value::Unit,
    }
}

fn read_place(p: &Place) -> Value {
    let root = p.root.borrow();
    read_path(&root, &p.path)
}

fn read_path(v: &Value, path: &[Seg]) -> Value {
    let mut cur = v.clone();
    for seg in path {
        cur = match (seg, cur) {
            (Seg::Field(name), Value::Struct { fields, .. }) => {
                fields.get(name).cloned().unwrap_or(Value::Unit)
            }
            (Seg::Index(i), Value::Array(items)) => items.get(*i).cloned().unwrap_or(Value::Unit),
            (Seg::Index(i), Value::Tuple(items)) => items.get(*i).cloned().unwrap_or(Value::Unit),
            (Seg::Field(name), Value::Ref(p)) => {
                let mut p = p.clone();
                p.path.push(Seg::Field(name.clone()));
                read_place(&p)
            }
            _ => Value::Unit,
        };
    }
    cur
}

fn write_place(p: &Place, value: Value) {
    let mut root = p.root.borrow_mut();
    write_path(&mut root, &p.path, value);
}

fn write_path(cur: &mut Value, path: &[Seg], value: Value) {
    if path.is_empty() {
        *cur = value;
        return;
    }
    match (&path[0], cur) {
        (Seg::Field(name), Value::Struct { fields, .. }) => {
            if let Some(slot) = fields.get_mut(name) {
                if path.len() == 1 {
                    *slot = value;
                } else {
                    write_path(slot, &path[1..], value);
                }
            } else {
                if path.len() == 1 {
                    fields.insert(name.clone(), value);
                }
            }
        }
        (Seg::Index(i), Value::Array(items)) => {
            if let Some(slot) = items.get_mut(*i) {
                if path.len() == 1 {
                    *slot = value;
                } else {
                    write_path(slot, &path[1..], value);
                }
            }
        }
        (Seg::Index(i), Value::Tuple(items)) => {
            if let Some(slot) = items.get_mut(*i) {
                if path.len() == 1 {
                    *slot = value;
                } else {
                    write_path(slot, &path[1..], value);
                }
            }
        }
        _ => {}
    }
}

fn place_len(p: &Place) -> usize {
    match read_place(p) {
        Value::Array(items) => items.len(),
        Value::Tuple(items) => items.len(),
        Value::Slice { len, .. } => len,
        Value::Str(bytes) => bytes.len(),
        _ => 0,
    }
}

fn index_value(v: &Value, i: i128, span: Span) -> R<Value> {
    match v {
        Value::Array(items) => {
            let len = items.len();
            if i < 0 || i as usize >= len {
                return Err(EvalErr::trap(
                    "T01",
                    format!("index {i} out of bounds for length {len}"),
                    span,
                ));
            }
            Ok(items[i as usize].clone())
        }
        Value::Slice {
            root, start, len, ..
        } => {
            if i < 0 || i as usize >= *len {
                return Err(EvalErr::trap(
                    "T01",
                    format!("index {i} out of bounds for length {len}"),
                    span,
                ));
            }
            let v = root.borrow();
            Ok(read_path(&v, &[Seg::Index(start + i as usize)]))
        }
        Value::Vector { lanes, .. } => {
            let len = lanes.len();
            if i < 0 || i as usize >= len {
                return Err(EvalErr::trap(
                    "T01",
                    format!("index {i} out of bounds for length {len}"),
                    span,
                ));
            }
            Ok(lanes[i as usize].clone())
        }
        Value::Str(bytes) => {
            if i < 0 || i as usize >= bytes.len() {
                return Err(EvalErr::trap(
                    "T01",
                    format!("index {i} out of bounds for length {}", bytes.len()),
                    span,
                ));
            }
            Ok(int_value(bytes[i as usize] as i128, ScalarTy::U8))
        }
        _ => Ok(Value::Unit),
    }
}

fn apply_binary(op: BinOp, a: Value, b: Value, span: Span) -> R<Value> {
    if let (Value::Vector { elem, lanes: x }, Value::Vector { lanes: y, .. }) = (&a, &b) {
        if op.is_comparison() {
            let ord = |x: &Value, y: &Value| match (x, y) {
                (Value::Int { value: a, .. }, Value::Int { value: b, .. }) => a.cmp(b),
                (Value::Float { value: a, .. }, Value::Float { value: b, .. }) => {
                    a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal)
                }
                _ => std::cmp::Ordering::Equal,
            };
            let mask = x
                .iter()
                .zip(y)
                .map(|(a, b)| match op {
                    BinOp::Eq => ord(a, b) == std::cmp::Ordering::Equal,
                    BinOp::Ne => ord(a, b) != std::cmp::Ordering::Equal,
                    BinOp::Lt => ord(a, b) == std::cmp::Ordering::Less,
                    BinOp::Le => ord(a, b) != std::cmp::Ordering::Greater,
                    BinOp::Gt => ord(a, b) == std::cmp::Ordering::Greater,
                    BinOp::Ge => ord(a, b) != std::cmp::Ordering::Less,
                    _ => false,
                })
                .collect();
            return Ok(Value::Mask(mask));
        }
        let mut out = Vec::new();
        for (a, b) in x.iter().cloned().zip(y.iter().cloned()) {
            out.push(apply_binary(op, a, b, span)?);
        }
        return Ok(Value::Vector {
            elem: *elem,
            lanes: out,
        });
    }
    if matches!(op, BinOp::And | BinOp::Or) {
        let left = truthy(&a);
        let right = truthy(&b);
        return Ok(Value::Bool(if op == BinOp::And {
            left && right
        } else {
            left || right
        }));
    }
    if op.is_comparison() {
        let ord = cmp_values(&a, &b);
        let r = match (op, ord) {
            (BinOp::Eq, Some(std::cmp::Ordering::Equal)) => true,
            (BinOp::Ne, Some(std::cmp::Ordering::Equal)) => false,
            (BinOp::Ne, Some(_)) => true,
            (BinOp::Lt, Some(std::cmp::Ordering::Less)) => true,
            (BinOp::Le, Some(std::cmp::Ordering::Less | std::cmp::Ordering::Equal)) => true,
            (BinOp::Gt, Some(std::cmp::Ordering::Greater)) => true,
            (BinOp::Ge, Some(std::cmp::Ordering::Greater | std::cmp::Ordering::Equal)) => true,
            _ => false,
        };
        return Ok(Value::Bool(r));
    }
    match (a, b) {
        (Value::Int { value: x, ty }, Value::Int { value: y, .. }) => {
            if matches!(op, BinOp::Shl | BinOp::Shr) {
                let bits = ty.int_bits().unwrap_or(64) as i128;
                if y < 0 || y >= bits {
                    return Err(EvalErr::trap(
                        "T06",
                        format!("shift count {y} out of range for {bits}-bit value"),
                        span,
                    ));
                }
                let v = if op == BinOp::Shl { x << y } else { x >> y };
                return Ok(int_value(wrap_int(v, ty), ty));
            }
            if matches!(op, BinOp::Div | BinOp::Rem) {
                if y == 0 {
                    return Err(EvalErr::trap("T04", "division by zero", span));
                }
                let (min, _) = int_range(ty);
                if x == min && y == -1 {
                    return Err(EvalErr::trap("T05", "integer overflow in division", span));
                }
            }
            let wrapping = matches!(op, BinOp::AddWrap | BinOp::SubWrap | BinOp::MulWrap);
            let raw = match op {
                BinOp::Add | BinOp::AddSat => x.checked_add(y),
                BinOp::Sub | BinOp::SubSat => x.checked_sub(y),
                BinOp::Mul | BinOp::MulSat => x.checked_mul(y).or(Some(if (x < 0) != (y < 0) {
                    i128::MIN
                } else {
                    i128::MAX
                })),
                BinOp::Div => Some(x / y),
                BinOp::Rem => Some(x % y),
                BinOp::BitAnd => Some(x & y),
                BinOp::BitOr => Some(x | y),
                BinOp::BitXor => Some(x ^ y),
                _ => None,
            };
            let (min, max) = int_range(ty);
            let v = if wrapping {
                wrap_arith(x, y, ty, op)
            } else {
                let Some(raw) = raw else {
                    return Err(EvalErr::trap("T03", "integer overflow", span));
                };
                match op {
                    BinOp::AddSat | BinOp::SubSat | BinOp::MulSat => raw.clamp(min, max),
                    _ => {
                        if raw < min || raw > max {
                            return Err(EvalErr::trap("T03", "integer overflow", span));
                        }
                        raw
                    }
                }
            };
            Ok(int_value(v, ty))
        }
        (Value::Float { value: x, ty }, Value::Float { value: y, .. }) => {
            let v = match op {
                BinOp::Add => x + y,
                BinOp::Sub => x - y,
                BinOp::Mul => x * y,
                BinOp::Div => x / y,
                BinOp::Rem => x % y,
                _ => 0.0,
            };
            Ok(Value::Float { value: v, ty })
        }
        (Value::Vector { elem, lanes: x }, Value::Vector { lanes: y, .. }) => {
            let mut out = Vec::new();
            for (a, b) in x.into_iter().zip(y) {
                if op.is_comparison() {
                    out.push(Value::Bool(
                        cmp_values(&a, &b) == Some(std::cmp::Ordering::Equal),
                    ));
                } else {
                    out.push(apply_binary(op, a, b, span)?);
                }
            }
            if op.is_comparison() {
                Ok(Value::Mask(out.into_iter().map(|v| truthy(&v)).collect()))
            } else {
                Ok(Value::Vector { elem, lanes: out })
            }
        }
        (a, b) => Ok(Value::Bool(matches!(op, BinOp::Eq) && values_equal(&a, &b))),
    }
}

fn values_equal(a: &Value, b: &Value) -> bool {
    cmp_values(a, b) == Some(std::cmp::Ordering::Equal)
}

fn cmp_values(a: &Value, b: &Value) -> Option<std::cmp::Ordering> {
    match (a, b) {
        (Value::Int { value: x, .. }, Value::Int { value: y, .. }) => Some(x.cmp(y)),
        (Value::Float { value: x, .. }, Value::Float { value: y, .. }) => x.partial_cmp(y),
        (Value::Bool(x), Value::Bool(y)) => Some(x.cmp(y)),
        _ => None,
    }
}

fn float_unary(v: Value, f: fn(f64) -> f64) -> R<Value> {
    match v {
        Value::Float { value, ty } => Ok(Value::Float {
            value: f(value),
            ty,
        }),
        other => Ok(other),
    }
}

fn bits_value(t: Type, x: Value) -> R<Value> {
    match (&t, x) {
        (
            Type::Int(dst),
            Value::Float {
                value,
                ty: ScalarTy::F32,
            },
        ) => {
            let bits = (value as f32).to_bits();
            Ok(int_value(bits as i128, *dst))
        }
        (
            Type::Int(dst),
            Value::Float {
                value,
                ty: ScalarTy::F64,
            },
        ) => {
            let bits = value.to_bits();
            Ok(int_value(bits as i128, *dst))
        }
        (Type::Float(ScalarTy::F32), Value::Int { value, .. }) => Ok(Value::Float {
            value: f32::from_bits(value as u32) as f64,
            ty: ScalarTy::F32,
        }),
        (Type::Float(ScalarTy::F64), Value::Int { value, .. }) => Ok(Value::Float {
            value: f64::from_bits(value as u64),
            ty: ScalarTy::F64,
        }),
        (Type::Int(dst), Value::Int { value, .. }) => Ok(int_value(wrap_int(value, *dst), *dst)),
        _ => Ok(Value::Unit),
    }
}
