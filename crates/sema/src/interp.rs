//! The boundary between constant evaluation and the interpreter (`ingot-cfg`).
//!
//! Sema cannot call lowering: `ingot-cfg` depends on `ingot-sema`. So phase 6 and
//! phase 7 ask an implementation of [`Interpreter`] to evaluate the constant
//! expressions outside the direct evaluator's subset, and give it a [`Host`] to read
//! and extend the declarations. `ingot-cfg` implements both the interpreter
//! (`crates/cfg/src/eval.rs`) and the driver passes it in. With no implementation
//! (sema's own unit tests), the direct path records `constant evaluation of ...` and
//! never guesses a value.

use crate::consteval::Fail;
use crate::defs::{DefId, Sema};
use crate::hir;
use crate::hir::ExprId;
use crate::sigs::{Declarations, FnSig};
use crate::ty::{ConstValue, TypeId};
use crate::typed::TypedBody;
use crate::value::Value;
use ingot_diag::source::{Sources, Span};

/// What is known about a constant's value.
#[derive(Clone, Debug)]
pub enum ConstLookup {
    /// A value the interpreter can use.
    Value(Value),
    /// The direct evaluator found a value of the small type-level kind.
    Scalar(ConstValue),
    /// Its evaluation failed; nothing more is said.
    Failed(Fail),
    /// No value yet. The interpreter must type and lower the initializer.
    NeedInitializer,
}

/// The data lowering needs, borrowed together.
pub struct LoweringParts<'a> {
    pub sources: &'a Sources,
    pub sema: &'a Sema,
    pub packages: &'a [hir::Package],
    pub declarations: &'a mut Declarations,
}

/// What the interpreter may ask phase 6 and phase 7 to do.
pub trait Host {
    /// The sources, the resolved program, the typed bodies so far and the type table,
    /// for lowering a body the interpreter reached.
    fn lowering(&mut self) -> LoweringParts<'_>;

    /// The typed body of `def`, typed on demand and stored (phase 7 reuses it). `None`
    /// when the definition has no body or its signature is poison.
    fn typed_body(&mut self, def: DefId) -> Option<TypedBody>;

    /// The typed body of one constant expression `expr` in `body` (a signature
    /// constant or an `@static_assert` condition), typed on demand and stored.
    fn typed_expr(&mut self, owner: DefId, expr: ExprId, ty: TypeId) -> Option<TypedBody>;

    /// The signature of `def`, built on demand.
    fn fn_sig(&mut self, def: DefId) -> Option<FnSig>;

    /// True when the signature of `def` is being built: a call that needs it is a
    /// constant that depends on the signature that asked for it (E0812).
    fn sig_in_progress(&self, def: DefId) -> bool;

    /// True when the body of `def` is being typed: a call that needs it is E0812.
    fn body_in_progress(&self, def: DefId) -> bool;

    /// The call depth of the evaluation in progress and its outermost call, for a
    /// nested evaluation (pipeline section 4.6).
    fn set_eval_context(&mut self, depth: usize, outer: Option<(usize, Span)>);

    /// What is known about the constant `def`; runs the direct evaluator when nothing
    /// has asked for it yet.
    fn const_lookup(&mut self, def: DefId) -> ConstLookup;

    /// Record that the value of `def` is being computed (a later read is E0812).
    fn mark_const_in_progress(&mut self, def: DefId);

    /// Record that the interpreter is evaluating the initializer of `def` (a signature
    /// asked for meanwhile is E0812 at it).
    fn enter_const(&mut self, def: DefId);

    fn leave_const(&mut self);

    /// Store the value (or the failure) of `def`.
    fn set_const_value(&mut self, def: DefId, result: Result<Value, Fail>);

    /// Report a diagnostic whose primary span is in `file`, with an optional `call`
    /// label in another file.
    fn report(
        &mut self,
        code: &'static str,
        message: String,
        file: usize,
        span: Span,
        label: Option<(usize, Span)>,
    );

    /// Report E0812 at the constant's name.
    fn report_cycle(&mut self, def: DefId);

    /// The value of `@target...`-style direct expressions the interpreter met is not
    /// available to it; record that evaluation needs `what`.
    fn not_implemented(&mut self, what: String);

    /// The layout of a type, for `@sizeof`, `@zero` and the live-value limit.
    fn layout(&mut self, ty: TypeId) -> Option<crate::Layout>;

    /// The declarations, for types and signatures.
    fn declarations(&self) -> &Declarations;

    fn declarations_mut(&mut self) -> &mut Declarations;

    /// A type as text, for diagnostics.
    fn type_name(&self, ty: TypeId) -> String;

    /// What `def` is (a function, a `const`, an `extern` function, ...).
    fn def_kind(&self, def: DefId) -> crate::defs::DefKind;

    /// The name of `def`, for resolving a call through a bound.
    fn def_name(&self, def: DefId) -> String;
}

/// What the driver gives sema: a way to make an interpreter for one constant
/// expression. Each evaluation gets a fresh one, so an evaluation can start while
/// another is suspended (a body pulled forward by the first one can evaluate its own
/// constants), with its own step, depth and byte counters.
pub trait InterpreterFactory {
    /// A new interpreter for one constant expression. `depth` is the number of calls
    /// in progress in the evaluations that pulled this expression's body (they count
    /// toward the same 1024-call limit, pipeline section 4.6); `outer` is the
    /// outermost call for E0808.
    fn make(&self, depth: usize, outer: Option<(usize, Span)>) -> Box<dyn Interpreter + '_>;
}

/// An implementation of the interpretation path (pipeline section 4.6).
pub trait Interpreter {
    /// Evaluate the initializer of the `const` or `static` `def` and return its value.
    /// The interpreter asks the host for the typed body and, through it, for every
    /// body the evaluation reaches.
    fn eval_initializer(
        &mut self,
        host: &mut dyn Host,
        def: DefId,
        file: usize,
        span: Span,
    ) -> Result<Value, Fail>;

    /// Evaluate `expr` of the typed body `body`, of type `ty`: the condition of an
    /// `@static_assert`, or a constant expression of a signature (an array length, an
    /// enum tag value, an `align(...)` argument).
    fn eval_expr(
        &mut self,
        host: &mut dyn Host,
        body: &TypedBody,
        expr: ExprId,
        ty: TypeId,
        file: usize,
        span: Span,
    ) -> Result<Value, Fail>;
}
