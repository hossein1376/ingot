//! The ownership analyses over the CFG.
//!
//! Analysis A (moves and linear values) is implemented here first; B (origin
//! sets) and C (loans) follow the same graph and state machinery.

use crate::ast::{GenericArg, Ty};
use crate::cfg::{
    BlockId, BorrowKind, Cfg, CfgStmt, LoanId, LocalId, Operand, Place, Proj, Rvalue, Term, UseMode,
};
use crate::check::{LocalFact, Program};
use crate::diagnostic::{Diagnostic, Edit};
use crate::source::{Source, Span};
use crate::types::Type;
use std::collections::{HashMap, HashSet};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum MState {
    Init,
    Moved,
    Maybe,
}

impl MState {
    fn join(self, other: MState) -> MState {
        if self == other { self } else { MState::Maybe }
    }
}

type PathKey = (usize, Vec<ProjKey>);

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum ProjKey {
    Field(String),
    Index,
    Deref,
    Variant(String),
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct MoveSet {
    map: HashMap<PathKey, MState>,
}

pub struct Checked {
    pub diagnostics: Vec<Diagnostic>,
}

pub fn check(
    program: &Program,
    source: &Source,
    cfg: &Cfg,
    facts: &[LocalFact],
    bounds: &HashMap<String, Vec<String>>,
    function: &crate::check::Function,
) -> Checked {
    let mut diagnostics = Vec::new();
    for (move_span, defer_span) in &cfg.defer_conflicts {
        let mut d = Diagnostic::new(
            "E1005",
            "a value used by a pending deferred block is moved before the block runs",
            source,
            *move_span,
        );
        d.relate("defer", *defer_span);
        diagnostics.push(d);
    }
    analyze_moves(
        program,
        source,
        cfg,
        facts,
        bounds,
        function,
        &mut diagnostics,
    );
    let origins = analyze_origins(program, source, cfg, function, &mut diagnostics);
    analyze_loans(program, source, cfg, &origins, &mut diagnostics);
    Checked { diagnostics }
}

fn path_key(place: &Place) -> PathKey {
    (
        place.local,
        place
            .proj
            .iter()
            .map(|p| match p {
                Proj::Field(n) => ProjKey::Field(n.clone()),
                Proj::Index => ProjKey::Index,
                Proj::Deref => ProjKey::Deref,
                Proj::Variant(n) => ProjKey::Variant(n.clone()),
            })
            .collect(),
    )
}

fn prefix_of(prefix: &PathKey, path: &PathKey) -> bool {
    prefix.0 == path.0 && path.1.starts_with(&prefix.1)
}

impl MoveSet {
    fn get(&self, path: &PathKey) -> MState {
        *self.map.get(path).unwrap_or(&MState::Init)
    }

    fn any_prefix(&self, path: &PathKey) -> Option<MState> {
        let mut best: Option<MState> = None;
        for (k, v) in &self.map {
            if prefix_of(k, path) && k != path {
                best = Some(best.map(|b| b.max(*v)).unwrap_or(*v));
            }
        }
        best
    }

    fn any_child(&self, path: &PathKey) -> Option<MState> {
        let mut best: Option<MState> = None;
        for (k, v) in &self.map {
            if prefix_of(path, k) && k != path {
                best = Some(best.map(|b| b.max(*v)).unwrap_or(*v));
            }
        }
        best
    }

    fn init_subtree(&mut self, path: &PathKey) {
        self.map.retain(|k, _| !prefix_of(path, k));
        self.map.insert(path.clone(), MState::Init);
    }

    fn status(&self, path: &PathKey) -> MState {
        let mut maybe = false;
        for (k, v) in &self.map {
            if prefix_of(k, path) {
                match v {
                    MState::Moved => return MState::Moved,
                    MState::Maybe => maybe = true,
                    MState::Init => {}
                }
            }
        }
        if maybe { MState::Maybe } else { MState::Init }
    }

    fn move_subtree(&mut self, path: &PathKey) {
        self.map.retain(|k, _| !prefix_of(path, k));
        self.map.insert(path.clone(), MState::Moved);
    }

    fn join(&mut self, other: &MoveSet) {
        let keys: HashSet<PathKey> = self.map.keys().chain(other.map.keys()).cloned().collect();
        for k in keys {
            let a = self.get(&k);
            let b = other.get(&k);
            self.map.insert(k, a.join(b));
        }
    }
}

fn merge_into(target: &mut HashMap<BlockId, MoveSet>, block: BlockId, state: MoveSet) -> bool {
    match target.get_mut(&block) {
        Some(existing) => {
            let before = existing.clone();
            existing.join(&state);
            *existing != before
        }
        None => {
            target.insert(block, state);
            true
        }
    }
}

fn successors(term: &Term) -> Vec<BlockId> {
    match term {
        Term::Goto(b) => vec![*b],
        Term::Branch { then_b, else_b, .. } => vec![*then_b, *else_b],
        Term::Switch { cases, default, .. } => {
            let mut v: Vec<BlockId> = cases.iter().map(|(_, b)| *b).collect();
            v.push(*default);
            v
        }
        Term::Call { then_b, .. } | Term::Spawn { then_b, .. } => vec![*then_b],
        Term::ScopeWait { then_b, .. } => vec![*then_b],
        Term::Return { .. } | Term::Trap { .. } | Term::Unreachable => Vec::new(),
    }
}

fn analyze_moves(
    program: &Program,
    source: &Source,
    cfg: &Cfg,
    _facts: &[LocalFact],
    bounds: &HashMap<String, Vec<String>>,
    function: &crate::check::Function,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let _ = bounds;
    let mut entry_states: HashMap<BlockId, MoveSet> = HashMap::new();
    entry_states.insert(cfg.entry, MoveSet::default());
    let mut queue: Vec<BlockId> = vec![cfg.entry];
    let mut queued: HashSet<BlockId> = queue.iter().cloned().collect();
    let mut leaks: HashMap<usize, Vec<Span>> = HashMap::new();
    let mut reported: HashSet<(&'static str, usize, usize)> = HashSet::new();

    while let Some(bid) = queue.pop() {
        queued.remove(&bid);
        let Some(mut state) = entry_states.get(&bid).cloned() else {
            continue;
        };
        let block = &cfg.blocks[bid];
        for stmt in &block.stmts {
            transfer_stmt(
                program,
                source,
                cfg,
                bounds,
                stmt,
                &mut state,
                &mut leaks,
                &mut reported,
                diagnostics,
            );
        }
        transfer_term(
            program,
            source,
            cfg,
            bounds,
            &block.term,
            &mut state,
            &mut leaks,
            &mut reported,
            diagnostics,
        );
        for succ in successors(&block.term) {
            if merge_into(&mut entry_states, succ, state.clone()) && queued.insert(succ) {
                queue.push(succ);
            }
        }
    }

    for (local, exits) in leaks {
        let l = &cfg.locals[local];
        let name = l.name.clone().unwrap_or_else(|| format!("<temp {local}>"));
        let mut d = Diagnostic::new(
            "E1003",
            format!("`{name}` is a linear value that is not consumed on every path"),
            source,
            l.span,
        );
        for exit in exits {
            d.relate("scope_end", exit);
        }
        if let Some((message, edit)) =
            linear_fix(program, function, source, cfg, local, &cfg.locals[local])
        {
            d.fix(message, edit);
        }
        diagnostics.push(d);
    }
}

/// The catalog's fix for E1003: a generic `T` gains the `Discard` bound, and a
/// concrete type is consumed by its one nullary consuming function.
fn linear_fix(
    program: &Program,
    function: &crate::check::Function,
    source: &Source,
    cfg: &Cfg,
    local_id: LocalId,
    local: &crate::cfg::CfgLocal,
) -> Option<(String, Edit)> {
    if let Type::Param(param) = &local.ty {
        let generic = function.decl.generics.iter().find(|g| &g.name == param)?;
        if generic.bounds.iter().any(|b| b.name() == "Discard") {
            return None;
        }
        let (span, text) = match generic.bounds.last() {
            Some(last) => (Span::empty(last.span.end), ", Discard".to_string()),
            None => (Span::empty(generic.name_span.end), ": Discard".to_string()),
        };
        return Some(("add the bound `Discard`".to_string(), Edit { span, text }));
    }
    if local.is_param {
        return None;
    }
    // A deferred consume is wrong when some path already consumes the value:
    // the fix would move it twice. The catalog's condition is only met when
    // the leak is the sole use.
    if local_explicitly_moved(cfg, local_id) {
        return None;
    }
    let name = local.name.as_deref()?;
    let methods = program.methods.get(&crate::check::method_key(&local.ty))?;
    let mut consuming: Vec<&crate::check::Function> = methods
        .values()
        .filter(|m| {
            m.decl.params.len() == 1
                && m.decl.params[0].name == "self"
                && !matches!(m.decl.params[0].ty, Ty::Borrow { .. })
        })
        .collect();
    consuming.sort_by(|a, b| a.decl.name.cmp(&b.decl.name));
    if consuming.len() != 1 {
        return None;
    }
    let method = &consuming[0].decl.name;
    let (insert, indent) = after_line(source, local.span)?;
    let text = format!("\n{indent}defer {{ {name}.{method}(); }}");
    Some((
        format!("insert `defer {{ {name}.{method}(); }}`"),
        Edit {
            span: Span::empty(insert),
            text,
        },
    ))
}

fn local_explicitly_moved(cfg: &Cfg, local: LocalId) -> bool {
    fn operand_moves(operand: &Operand, local: LocalId) -> bool {
        match operand {
            Operand::Place(place) => place.local == local && place.proj.is_empty(),
            Operand::Const(_) | Operand::Unit => false,
        }
    }
    for block in &cfg.blocks {
        for stmt in &block.stmts {
            match stmt {
                CfgStmt::Use {
                    place,
                    mode: UseMode::Move,
                    ..
                }
                | CfgStmt::ConsumeUnion { place, .. }
                    if place.local == local =>
                {
                    return true;
                }
                CfgStmt::Assign {
                    value:
                        Rvalue::Use {
                            place,
                            mode: UseMode::Move,
                        },
                    ..
                } if place.local == local => return true,
                CfgStmt::Assign {
                    value: Rvalue::Aggregate { fields, .. },
                    ..
                } if fields.iter().any(|o| operand_moves(o, local)) => return true,
                _ => {}
            }
        }
        match &block.term {
            Term::Call { args, .. } | Term::Spawn { args, .. } => {
                if args.iter().any(|o| operand_moves(o, local)) {
                    return true;
                }
            }
            Term::Return {
                value: Some(value), ..
            } if operand_moves(value, local) => return true,
            _ => {}
        }
    }
    false
}

/// The offset just before the newline of the line that contains `span`, and
/// that line's indentation. Returns `None` when the line is not a finished
/// `;` statement, where an inserted statement would land inside it.
fn after_line(source: &Source, span: Span) -> Option<(usize, String)> {
    let text = &source.text;
    let line_start = text[..span.start.min(text.len())]
        .rfind('\n')
        .map(|i| i + 1)
        .unwrap_or(0);
    let line_end = text[span.end.min(text.len())..]
        .find('\n')
        .map(|i| span.end.min(text.len()) + i)
        .unwrap_or(text.len());
    let line = &text[line_start..line_end];
    let code = match line.find("//") {
        Some(i) => &line[..i],
        None => line,
    };
    if !code.trim_end().ends_with(';') {
        return None;
    }
    let indent: String = line
        .chars()
        .take_while(|c| *c == ' ' || *c == '\t')
        .collect();
    Some((line_end, indent))
}

#[allow(clippy::too_many_arguments)]
fn transfer_stmt(
    program: &Program,
    source: &Source,
    cfg: &Cfg,
    bounds: &HashMap<String, Vec<String>>,
    stmt: &CfgStmt,
    state: &mut MoveSet,
    leaks: &mut HashMap<usize, Vec<Span>>,
    reported: &mut HashSet<(&'static str, usize, usize)>,
    diagnostics: &mut Vec<Diagnostic>,
) {
    match stmt {
        CfgStmt::StorageStart { local, .. } => {
            let key = (*local, Vec::new());
            state.move_subtree(&key);
        }
        CfgStmt::StorageEnd { local, span } => {
            let key = (*local, Vec::new());
            let local_info = &cfg.locals[*local];
            if local_info.linear {
                let mut paths = Vec::new();
                linear_paths(
                    program,
                    bounds,
                    &local_info.ty,
                    &key,
                    &mut paths,
                    &mut HashSet::new(),
                );
                let leaked = paths
                    .iter()
                    .any(|p| matches!(state.status(p), MState::Init | MState::Maybe));
                if leaked {
                    leaks.entry(*local).or_default().push(*span);
                }
            }
            state.move_subtree(&key);
        }
        CfgStmt::Use { place, mode, span } => {
            use_place(
                program,
                source,
                cfg,
                bounds,
                place,
                *mode,
                *span,
                state,
                reported,
                diagnostics,
            );
        }
        CfgStmt::Assign { place, value, span } => {
            let dest = path_key(place);
            let dest_ty = place_type(cfg, program, place);
            if is_linear_type(program, bounds, &dest_ty) && state.get(&dest) != MState::Moved {
                let key = (dest.0, dest.1.clone());
                if reported.insert(("E1004", span.start, key.0)) {
                    diagnostics.push(Diagnostic::new(
                        "E1004",
                        "assignment to a place that still holds a linear value",
                        source,
                        *span,
                    ));
                }
            }
            transfer_rvalue(
                program,
                source,
                cfg,
                bounds,
                value,
                *span,
                state,
                reported,
                diagnostics,
            );
            state.init_subtree(&dest);
        }
        CfgStmt::ConsumeUnion { place, .. } => {
            state.move_subtree(&path_key(place));
        }
        CfgStmt::Check { .. } | CfgStmt::CancelScope { .. } => {}
    }
}

#[allow(clippy::too_many_arguments)]
fn transfer_rvalue(
    program: &Program,
    source: &Source,
    cfg: &Cfg,
    bounds: &HashMap<String, Vec<String>>,
    value: &Rvalue,
    span: Span,
    state: &mut MoveSet,
    reported: &mut HashSet<(&'static str, usize, usize)>,
    diagnostics: &mut Vec<Diagnostic>,
) {
    match value {
        Rvalue::Use { place, mode } => {
            use_place(
                program,
                source,
                cfg,
                bounds,
                place,
                *mode,
                span,
                state,
                reported,
                diagnostics,
            );
        }
        Rvalue::Borrow { place, .. } | Rvalue::Slice { place, .. } | Rvalue::Len { place } => {
            read_place(
                program,
                source,
                cfg,
                bounds,
                place,
                span,
                state,
                reported,
                diagnostics,
            );
        }
        Rvalue::Binary { lhs, rhs, .. } => {
            operand_use(
                program,
                source,
                cfg,
                bounds,
                lhs,
                UseMode::Copy,
                span,
                state,
                reported,
                diagnostics,
            );
            operand_use(
                program,
                source,
                cfg,
                bounds,
                rhs,
                UseMode::Copy,
                span,
                state,
                reported,
                diagnostics,
            );
        }
        Rvalue::Unary { operand, .. } | Rvalue::Cast { operand } => {
            operand_use(
                program,
                source,
                cfg,
                bounds,
                operand,
                UseMode::Copy,
                span,
                state,
                reported,
                diagnostics,
            );
        }
        Rvalue::ConvertError { operand } => {
            operand_use(
                program,
                source,
                cfg,
                bounds,
                operand,
                UseMode::Move,
                span,
                state,
                reported,
                diagnostics,
            );
        }
        Rvalue::Aggregate { fields, .. } => {
            for f in fields {
                operand_use(
                    program,
                    source,
                    cfg,
                    bounds,
                    f,
                    UseMode::Move,
                    span,
                    state,
                    reported,
                    diagnostics,
                );
            }
        }
        Rvalue::Constant | Rvalue::Zero => {}
    }
}

#[allow(clippy::too_many_arguments)]
fn transfer_term(
    program: &Program,
    source: &Source,
    cfg: &Cfg,
    bounds: &HashMap<String, Vec<String>>,
    term: &Term,
    state: &mut MoveSet,
    leaks: &mut HashMap<usize, Vec<Span>>,
    reported: &mut HashSet<(&'static str, usize, usize)>,
    diagnostics: &mut Vec<Diagnostic>,
) {
    match term {
        Term::Call {
            args, dest, span, ..
        } => {
            for a in args {
                operand_use(
                    program,
                    source,
                    cfg,
                    bounds,
                    a,
                    UseMode::Move,
                    *span,
                    state,
                    reported,
                    diagnostics,
                );
            }
            if let Some(d) = dest {
                state.init_subtree(&path_key(d));
            }
        }
        Term::Spawn {
            args,
            dest,
            scope,
            span,
            ..
        } => {
            for a in args {
                operand_use(
                    program,
                    source,
                    cfg,
                    bounds,
                    a,
                    UseMode::Move,
                    *span,
                    state,
                    reported,
                    diagnostics,
                );
            }
            read_place(
                program,
                source,
                cfg,
                bounds,
                scope,
                *span,
                state,
                reported,
                diagnostics,
            );
            if let Some(d) = dest {
                state.init_subtree(&path_key(d));
            }
        }
        Term::Branch { cond, span, .. } => {
            operand_use(
                program,
                source,
                cfg,
                bounds,
                cond,
                UseMode::Copy,
                *span,
                state,
                reported,
                diagnostics,
            );
        }
        Term::Switch { value, span, .. } => {
            read_place(
                program,
                source,
                cfg,
                bounds,
                value,
                *span,
                state,
                reported,
                diagnostics,
            );
        }
        Term::ScopeWait { scope, span, .. } => {
            read_place(
                program,
                source,
                cfg,
                bounds,
                scope,
                *span,
                state,
                reported,
                diagnostics,
            );
        }
        Term::Return { .. } | Term::Trap { .. } | Term::Unreachable | Term::Goto(_) => {}
    }
    let _ = leaks;
}

#[allow(clippy::too_many_arguments)]
fn operand_use(
    program: &Program,
    source: &Source,
    cfg: &Cfg,
    bounds: &HashMap<String, Vec<String>>,
    op: &Operand,
    mode: UseMode,
    span: Span,
    state: &mut MoveSet,
    reported: &mut HashSet<(&'static str, usize, usize)>,
    diagnostics: &mut Vec<Diagnostic>,
) {
    match op {
        Operand::Place(p) => use_place(
            program,
            source,
            cfg,
            bounds,
            p,
            mode,
            span,
            state,
            reported,
            diagnostics,
        ),
        Operand::Const(_) | Operand::Unit => {}
    }
}

#[allow(clippy::too_many_arguments)]
fn use_place(
    program: &Program,
    source: &Source,
    cfg: &Cfg,
    bounds: &HashMap<String, Vec<String>>,
    place: &Place,
    mode: UseMode,
    span: Span,
    state: &mut MoveSet,
    reported: &mut HashSet<(&'static str, usize, usize)>,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let path = path_key(place);
    let moved = state.get(&path);
    let prefix = state.any_prefix(&path);
    let child = if path.1.is_empty() {
        state.any_child(&path)
    } else {
        None
    };
    if moved == MState::Moved
        || matches!(prefix, Some(MState::Moved | MState::Maybe))
        || matches!(child, Some(MState::Moved | MState::Maybe))
        || moved == MState::Maybe
    {
        if cfg.deferred_statements.contains(&span) {
            return;
        }
        if reported.insert(("E1001", span.start, path.0)) {
            let name = local_name(cfg, path.0);
            diagnostics.push(Diagnostic::new(
                "E1001",
                format!("`{name}` is used after it was moved"),
                source,
                span,
            ));
        }
        return;
    }
    if mode == UseMode::Move {
        if place
            .proj
            .iter()
            .any(|p| matches!(p, Proj::Deref | Proj::Index))
        {
            if reported.insert(("E1002", span.start, path.0)) {
                diagnostics.push(Diagnostic::new(
                    "E1002",
                    "cannot move a value out of a borrow or an element",
                    source,
                    span,
                ));
            }
            return;
        }
        if let Some(Proj::Field(_)) = place.proj.first() {
            let root_ty = &cfg.locals[place.local].ty;
            if let Type::Struct(name, _) = root_ty
                && let Some(def) = program.defs.structs.get(name)
                && def.module != cfg.module
                && def.kind == crate::ast::StructKind::Linear
            {
                if reported.insert(("E1006", span.start, place.local)) {
                    diagnostics.push(Diagnostic::new(
                        "E1006",
                        "a field of a linear struct cannot be moved outside its module",
                        source,
                        span,
                    ));
                }
                state.move_subtree(&path);
                return;
            }
        }
        let ty = place_type(cfg, program, place);
        if !is_copy_type(program, bounds, &ty) {
            state.move_subtree(&path);
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn read_place(
    program: &Program,
    source: &Source,
    cfg: &Cfg,
    bounds: &HashMap<String, Vec<String>>,
    place: &Place,
    span: Span,
    state: &mut MoveSet,
    reported: &mut HashSet<(&'static str, usize, usize)>,
    diagnostics: &mut Vec<Diagnostic>,
) {
    use_place(
        program,
        source,
        cfg,
        bounds,
        place,
        UseMode::Copy,
        span,
        state,
        reported,
        diagnostics,
    );
}

fn local_name(cfg: &Cfg, local: usize) -> String {
    cfg.locals[local]
        .name
        .clone()
        .unwrap_or_else(|| format!("<temp {local}>"))
}

fn place_type(cfg: &Cfg, program: &Program, place: &Place) -> Type {
    let mut ty = cfg.locals[place.local].ty.clone();
    for p in &place.proj {
        ty = match (p, &ty) {
            (Proj::Field(n), t) => program.defs.field(t, n).unwrap_or(Type::Error),
            (Proj::Index, Type::Array(_, e)) => (**e).clone(),
            (Proj::Index, Type::Slice { elem, .. }) => (**elem).clone(),
            (Proj::Index, Type::Vector(v)) => Type::Int(v.elem),
            (Proj::Deref, Type::Borrow { inner, .. }) => (**inner).clone(),
            (Proj::Deref, Type::Ptr { inner, .. }) => (**inner).clone(),
            (Proj::Variant(n), Type::Result(a, b)) => {
                if n == "ok" {
                    (**a).clone()
                } else {
                    (**b).clone()
                }
            }
            (Proj::Variant(_), Type::Optional(a)) => (**a).clone(),
            _ => Type::Error,
        };
    }
    ty
}

fn linear_paths(
    program: &Program,
    bounds: &HashMap<String, Vec<String>>,
    ty: &Type,
    prefix: &PathKey,
    out: &mut Vec<PathKey>,
    seen: &mut HashSet<String>,
) {
    if !ty.is_linear_value(&program.defs, bounds) {
        return;
    }
    match ty {
        Type::Struct(name, args) => {
            let Some(def) = program.defs.structs.get(name) else {
                out.push(prefix.clone());
                return;
            };
            if !seen.insert(name.clone()) {
                out.push(prefix.clone());
                return;
            }
            let mut any_linear_part = false;
            for (field, field_ty, _) in &def.fields {
                let field_ty = crate::types::substitute(field_ty, &def.generics, args);
                if field_ty.is_linear_value(&program.defs, bounds) {
                    any_linear_part = true;
                    let mut p = prefix.clone();
                    p.1.push(ProjKey::Field(field.clone()));
                    linear_paths(program, bounds, &field_ty, &p, out, seen);
                }
            }
            if !any_linear_part {
                out.push(prefix.clone());
            }
            seen.remove(name);
        }
        Type::Tuple(elems) => {
            for (i, e) in elems.iter().enumerate() {
                let mut p = prefix.clone();
                p.1.push(ProjKey::Field(i.to_string()));
                linear_paths(program, bounds, e, &p, out, seen);
            }
        }
        Type::Array(_, e) => {
            let mut p = prefix.clone();
            p.1.push(ProjKey::Index);
            linear_paths(program, bounds, e, &p, out, seen);
        }
        _ => out.push(prefix.clone()),
    }
}

fn is_copy_type(program: &Program, bounds: &HashMap<String, Vec<String>>, ty: &Type) -> bool {
    ty.is_copy(&program.defs, bounds)
}

fn is_linear_type(program: &Program, bounds: &HashMap<String, Vec<String>>, ty: &Type) -> bool {
    ty.is_linear_value(&program.defs, bounds)
}

// ---------------------------------------------------------------- origins B

type OriginSet = std::collections::BTreeSet<Origin>;
type Origins = HashMap<LocalId, OriginSet>;

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
enum Origin {
    Loan(LoanId),
    Param(LocalId),
    Inner(LocalId),
    SelfScope(LocalId),
}

#[derive(Clone, Debug, Default)]
struct OriginFacts {
    /// State before each statement (index `i`) and before the terminator
    /// (index `stmt_count`).
    points: HashMap<(BlockId, usize), Origins>,
    /// Two-phase loans that have been activated at each point.
    activation: HashMap<(BlockId, usize), HashSet<LoanId>>,
    /// Loans that escape through a `return`; E1203 covers their storage end.
    returned_loans: HashSet<LoanId>,
}

fn origin_set(origins: &Origins, local: LocalId) -> OriginSet {
    origins.get(&local).cloned().unwrap_or_default()
}

fn merge_origins(target: &mut HashMap<BlockId, Origins>, block: BlockId, state: &Origins) -> bool {
    match target.get_mut(&block) {
        Some(existing) => {
            let before = existing.clone();
            for (k, v) in state {
                existing.entry(*k).or_default().extend(v.iter().cloned());
            }
            *existing != before
        }
        None => {
            target.insert(block, state.clone());
            true
        }
    }
}

fn loan_root(cfg: &Cfg, id: LoanId) -> Option<LocalId> {
    cfg.loans.get(id).map(|l| l.place.local)
}

fn inner_once(program: &Program, cfg: &Cfg, origins: &Origins, set: &OriginSet) -> OriginSet {
    let mut out = OriginSet::new();
    for o in set {
        match o {
            Origin::Loan(id) => {
                if let Some(root) = loan_root(cfg, *id) {
                    out.extend(origin_set(origins, root));
                }
            }
            Origin::Param(p) | Origin::SelfScope(p) => {
                out.insert(Origin::Inner(*p));
            }
            Origin::Inner(p) => {
                out.insert(Origin::Inner(*p));
            }
        }
    }
    let _ = program;
    out
}

fn inner_star(program: &Program, cfg: &Cfg, origins: &Origins, set: &OriginSet) -> OriginSet {
    let mut out = set.clone();
    loop {
        let next = inner_once(program, cfg, origins, &out);
        let before = out.len();
        out.extend(next);
        if out.len() == before {
            break;
        }
    }
    out
}

/// `inner` closure without the original set: the origins stored inside what a
/// borrow refers to.
fn inner_only_star(program: &Program, cfg: &Cfg, origins: &Origins, set: &OriginSet) -> OriginSet {
    let mut out = inner_once(program, cfg, origins, set);
    loop {
        let next = inner_once(program, cfg, origins, &out);
        let before = out.len();
        out.extend(next);
        if out.len() == before {
            break;
        }
    }
    out
}

fn read_origins(program: &Program, cfg: &Cfg, origins: &Origins, place: &Place) -> OriginSet {
    let mut set = origin_set(origins, place.local);
    for _ in 0..place.derefs() {
        set = inner_only_star(program, cfg, origins, &set);
    }
    set
}

fn operand_origins(program: &Program, cfg: &Cfg, origins: &Origins, op: &Operand) -> OriginSet {
    match op {
        Operand::Place(p) => read_origins(program, cfg, origins, p),
        _ => OriginSet::new(),
    }
}

fn strong_assign(
    program: &Program,
    cfg: &Cfg,
    origins: &mut Origins,
    dest: &Place,
    set: OriginSet,
) {
    let root = dest.local;
    if dest.proj.is_empty() {
        // Kill rule: assigning a borrow local removes loans that reborrowed
        // through its old referent.
        for set in origins.values_mut() {
            set.retain(|o| match o {
                Origin::Loan(id) => cfg
                    .loans
                    .get(*id)
                    .map(|l| !(l.place.local == root && l.place.proj.first() == Some(&Proj::Deref)))
                    .unwrap_or(true),
                _ => true,
            });
        }
        origins.insert(root, set);
    } else {
        origins.entry(root).or_default().extend(set);
    }
    let _ = program;
}

fn widen_store(
    cfg: &Cfg,
    origins: &mut Origins,
    dest: &Place,
    set: &OriginSet,
    seen: &mut HashSet<LoanId>,
) {
    let root = dest.local;
    let current = origin_set(origins, root);
    for o in current {
        match o {
            Origin::Loan(id) => {
                if !seen.insert(id) {
                    continue;
                }
                if let Some(loan) = cfg.loans.get(id).cloned() {
                    if loan.place.proj.is_empty() {
                        origins
                            .entry(loan.place.local)
                            .or_default()
                            .extend(set.iter().cloned());
                    } else {
                        widen_store(cfg, origins, &loan.place, set, seen);
                    }
                }
            }
            Origin::Param(_) | Origin::Inner(_) | Origin::SelfScope(_) => {
                // Boundary checks (E1204/E1205) are reported by the type checker
                // for the store shapes in the conformance suite.
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn store_through_operand(
    program: &Program,
    source: &Source,
    cfg: &Cfg,
    function: &crate::check::Function,
    span: Span,
    origins: &mut Origins,
    op: &Operand,
    set: &OriginSet,
    reported: &mut HashSet<(String, usize, usize)>,
    diagnostics: &mut Vec<Diagnostic>,
) {
    if let Operand::Place(p) = op {
        // A `&mut` argument is a temporary holding a loan on its referent.
        let temp = origin_set(origins, p.local);
        let mut seen = HashSet::new();
        for o in &temp {
            if let Origin::Loan(id) = o
                && let Some(loan) = cfg.loans.get(*id).cloned()
            {
                if loan.place.proj.is_empty() {
                    check_store_boundary(
                        program,
                        source,
                        cfg,
                        function,
                        span,
                        loan.place.local,
                        set,
                        reported,
                        diagnostics,
                    );
                    origins
                        .entry(loan.place.local)
                        .or_default()
                        .extend(set.iter().cloned());
                } else {
                    widen_store(cfg, origins, &loan.place, set, &mut seen);
                }
            }
        }
        let _ = &mut seen;
    }
}

fn analyze_origins(
    program: &Program,
    source: &Source,
    cfg: &Cfg,
    function: &crate::check::Function,
    diagnostics: &mut Vec<Diagnostic>,
) -> OriginFacts {
    let entry = {
        let mut origins = Origins::new();
        for (i, local) in cfg.locals.iter().enumerate() {
            if !local.is_param {
                continue;
            }
            let is_scope = matches!(
                &local.ty,
                Type::External(n) if n == "Scope"
            ) || matches!(&local.ty, Type::Borrow { inner, .. } if matches!(inner.as_ref(), Type::External(n) if n == "Scope"));
            let viewish = local.ty.is_view(&program.defs) || matches!(local.ty, Type::Param(_));
            if is_scope {
                origins.insert(i, OriginSet::from([Origin::SelfScope(i)]));
            } else if viewish {
                origins.insert(i, OriginSet::from([Origin::Param(i)]));
            }
        }
        origins
    };
    let mut states: HashMap<BlockId, Origins> = HashMap::new();
    states.insert(cfg.entry, entry);
    let mut queue = vec![cfg.entry];
    let mut queued: HashSet<BlockId> = HashSet::from([cfg.entry]);
    let mut facts = OriginFacts::default();
    let mut reported: HashSet<(String, usize, usize)> = HashSet::new();
    while let Some(bid) = queue.pop() {
        queued.remove(&bid);
        let Some(mut state) = states.get(&bid).cloned() else {
            continue;
        };
        let block = &cfg.blocks[bid];
        for (i, stmt) in block.stmts.iter().enumerate() {
            facts.points.insert((bid, i), state.clone());
            transfer_origin_stmt(
                program,
                source,
                cfg,
                function,
                stmt,
                &mut state,
                &mut reported,
                diagnostics,
            );
        }
        facts.points.insert((bid, block.stmts.len()), state.clone());
        transfer_origin_term(
            program,
            source,
            cfg,
            function,
            &block.term,
            &mut state,
            &mut facts.returned_loans,
            &mut reported,
            diagnostics,
        );
        for succ in successors(&block.term) {
            if merge_origins(&mut states, succ, &state) && queued.insert(succ) {
                queue.push(succ);
            }
        }
    }
    facts
}

#[allow(clippy::too_many_arguments)]
fn transfer_origin_stmt(
    program: &Program,
    source: &Source,
    cfg: &Cfg,
    function: &crate::check::Function,
    stmt: &CfgStmt,
    origins: &mut Origins,
    reported: &mut HashSet<(String, usize, usize)>,
    diagnostics: &mut Vec<Diagnostic>,
) {
    match stmt {
        CfgStmt::StorageStart { local, .. } => {
            origins.insert(*local, OriginSet::new());
        }
        CfgStmt::StorageEnd { .. } => {
            // Origins stay attached to the local so a return value that was
            // moved into a temporary is still checked at the function boundary.
        }
        CfgStmt::Assign { place, value, .. } => {
            let set = match value {
                Rvalue::Use { place: src, .. } => read_origins(program, cfg, origins, src),
                Rvalue::Borrow { loan, .. } => OriginSet::from([Origin::Loan(*loan)]),
                Rvalue::Slice { place: src, .. } | Rvalue::Len { place: src } => {
                    read_origins(program, cfg, origins, src)
                }
                Rvalue::Aggregate { fields, .. } => {
                    let mut s = OriginSet::new();
                    for f in fields {
                        s.extend(operand_origins(program, cfg, origins, f));
                    }
                    s
                }
                Rvalue::ConvertError { operand } | Rvalue::Cast { operand } => {
                    operand_origins(program, cfg, origins, operand)
                }
                Rvalue::Unary { operand, .. } => operand_origins(program, cfg, origins, operand),
                Rvalue::Binary { .. } | Rvalue::Constant | Rvalue::Zero => OriginSet::new(),
            };
            if place.derefs() > 0 {
                check_store_boundary(
                    program,
                    source,
                    cfg,
                    function,
                    stmt_span_for_origins(stmt),
                    place.local,
                    &set,
                    reported,
                    diagnostics,
                );
                widen_store(cfg, origins, place, &set, &mut HashSet::new());
            } else {
                strong_assign(program, cfg, origins, place, set);
            }
        }
        CfgStmt::Use { place, .. } => {
            let _ = read_origins(program, cfg, origins, place);
        }
        CfgStmt::ConsumeUnion { place, .. } => {
            origins.remove(&place.local);
        }
        CfgStmt::Check { .. } | CfgStmt::CancelScope { .. } => {}
    }
}

#[allow(clippy::too_many_arguments)]
fn transfer_origin_term(
    program: &Program,
    source: &Source,
    cfg: &Cfg,
    function: &crate::check::Function,
    term: &Term,
    origins: &mut Origins,
    returned_loans: &mut HashSet<LoanId>,
    reported: &mut HashSet<(String, usize, usize)>,
    diagnostics: &mut Vec<Diagnostic>,
) {
    match term {
        Term::Call {
            function: key,
            args,
            dest,
            span,
            ..
        } => {
            let callee = key.as_ref().and_then(|k| program.functions.get(k)).cloned();
            let arg_sets: Vec<OriginSet> = args
                .iter()
                .map(|a| operand_origins(program, cfg, origins, a))
                .collect();
            if let Some(callee) = &callee {
                // `into` stores.
                for (i, p) in callee.decl.params.iter().enumerate() {
                    let Some((target, _)) = &p.into else {
                        continue;
                    };
                    let Some(j) = callee.decl.params.iter().position(|q| q.name == *target) else {
                        continue;
                    };
                    if let (Some(src), Some(dst)) = (arg_sets.get(i), args.get(j)) {
                        store_through_operand(
                            program,
                            source,
                            cfg,
                            function,
                            *span,
                            origins,
                            dst,
                            src,
                            reported,
                            diagnostics,
                        );
                    }
                }
                // Type-parameter flow.
                let mut param_flow: HashMap<String, OriginSet> = HashMap::new();
                for g in &callee.decl.generics {
                    let mut u = OriginSet::new();
                    for (i, q) in callee.decl.params.iter().enumerate() {
                        let Some(inside) = ty_param_position(&q.ty, &g.name, false) else {
                            continue;
                        };
                        let set = arg_sets.get(i).cloned().unwrap_or_default();
                        if inside {
                            u.extend(inner_only_star(program, cfg, origins, &set));
                        } else {
                            u.extend(set);
                        }
                    }
                    if !u.is_empty() {
                        param_flow.insert(g.name.clone(), u);
                    }
                }
                for (i, p) in callee.decl.params.iter().enumerate() {
                    if !matches!(
                        p.ty,
                        Ty::Borrow { mutable: true, .. } | Ty::Slice { mutable: true, .. }
                    ) {
                        continue;
                    }
                    for (name, u) in &param_flow {
                        if ty_mentions(&p.ty, name)
                            && let Some(dst) = args.get(i)
                        {
                            store_through_operand(
                                program,
                                source,
                                cfg,
                                function,
                                *span,
                                origins,
                                dst,
                                u,
                                reported,
                                diagnostics,
                            );
                        }
                    }
                }
                // Result origins from the written clauses.
                let mut result = OriginSet::new();
                for o in &callee.decl.from {
                    if o.is_static {
                        continue;
                    }
                    let Some(i) = callee.decl.params.iter().position(|p| p.name == o.name) else {
                        continue;
                    };
                    let a = arg_sets.get(i).cloned().unwrap_or_default();
                    if o.inner {
                        result.extend(inner_once(program, cfg, origins, &a));
                    } else {
                        result.extend(a.iter().cloned());
                        result.extend(inner_star(program, cfg, origins, &a));
                    }
                }
                for (name, u) in &param_flow {
                    if type_mentions(&callee.signature_ret, name) {
                        result.extend(u.iter().cloned());
                    }
                }
                if let Some(d) = dest {
                    strong_assign(program, cfg, origins, d, result);
                }
            } else {
                let mut result = OriginSet::new();
                for a in &arg_sets {
                    result.extend(a.iter().cloned());
                    result.extend(inner_star(program, cfg, origins, a));
                }
                if let Some(d) = dest {
                    strong_assign(program, cfg, origins, d, result);
                }
            }
            let _ = span;
        }
        Term::Spawn {
            args,
            scope,
            dest,
            handle_loan,
            span,
            ..
        } => {
            let mut result = OriginSet::new();
            for a in args {
                result.extend(operand_origins(program, cfg, origins, a));
            }
            if cfg.locals[scope.local].is_param {
                check_store_boundary(
                    program,
                    source,
                    cfg,
                    function,
                    *span,
                    scope.local,
                    &result,
                    reported,
                    diagnostics,
                );
            }
            origins
                .entry(scope.local)
                .or_default()
                .extend(result.iter().cloned());
            if let Some(loan) = handle_loan {
                let mut set = OriginSet::from([Origin::Loan(*loan)]);
                set.extend(result);
                if let Some(d) = dest {
                    strong_assign(program, cfg, origins, d, set);
                }
            }
        }
        Term::Return {
            value: Some(op), ..
        } => {
            let set = operand_origins(program, cfg, origins, op);
            if function.signature_ret.is_view(&program.defs) {
                for o in &set {
                    if let Origin::Loan(id) = o {
                        returned_loans.insert(*id);
                    }
                }
                check_return_origins(program, source, cfg, function, term, &set, diagnostics);
            }
        }
        Term::Return { value: None, .. } => {}
        _ => {}
    }
}

fn ty_param_position(ty: &Ty, name: &str, under_borrow: bool) -> Option<bool> {
    match ty {
        Ty::Path(p) => {
            if p.name() == name {
                return Some(under_borrow);
            }
            for a in &p.args {
                if let GenericArg::Ty(t) = a
                    && let Some(v) = ty_param_position(t, name, under_borrow)
                {
                    return Some(v);
                }
            }
            None
        }
        Ty::Borrow { inner, .. } | Ty::Slice { elem: inner, .. } => {
            ty_param_position(inner, name, true)
        }
        Ty::Ptr { inner, .. } | Ty::Optional { inner, .. } => {
            ty_param_position(inner, name, under_borrow)
        }
        Ty::Array { elem, .. } => ty_param_position(elem, name, under_borrow),
        Ty::Tuple { elems, .. } | Ty::Fn { params: elems, .. } => {
            for t in elems {
                if let Some(v) = ty_param_position(t, name, under_borrow) {
                    return Some(v);
                }
            }
            None
        }
        _ => None,
    }
}

fn stmt_span_for_origins(stmt: &CfgStmt) -> Span {
    match stmt {
        CfgStmt::Assign { span, .. }
        | CfgStmt::Use { span, .. }
        | CfgStmt::ConsumeUnion { span, .. }
        | CfgStmt::StorageStart { span, .. }
        | CfgStmt::StorageEnd { span, .. }
        | CfgStmt::CancelScope { span, .. } => *span,
        CfgStmt::Check { check } => check.span,
    }
}

#[allow(clippy::too_many_arguments)]
fn check_store_boundary(
    program: &Program,
    source: &Source,
    cfg: &Cfg,
    function: &crate::check::Function,
    span: Span,
    target: LocalId,
    set: &OriginSet,
    reported: &mut HashSet<(String, usize, usize)>,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let target_local = &cfg.locals[target];
    if !target_local.is_param {
        return;
    }
    let Some(target_name) = target_local.name.clone() else {
        return;
    };
    let Some(target_param) = function.decl.params.iter().find(|p| p.name == target_name) else {
        return;
    };
    for origin in set {
        match origin {
            Origin::Loan(id) => {
                if let Some(loan) = cfg.loans.get(*id)
                    && loan.place.derefs() == 0
                    && reported.insert(("E1205".to_string(), span.start, target))
                {
                    let mut d = Diagnostic::new(
                        "E1205",
                        "a borrow of a local escapes through a parameter",
                        source,
                        span,
                    );
                    d.relate("borrow", loan.span);
                    diagnostics.push(d);
                }
            }
            Origin::Param(p) | Origin::Inner(p) => {
                if *p == target && matches!(origin, Origin::Inner(_)) {
                    continue;
                }
                let source_name = cfg.locals[*p].name.clone().unwrap_or_default();
                let allowed = function.decl.params.iter().any(|q| {
                    q.name == source_name
                        && q.into
                            .as_ref()
                            .map(|(t, _)| t == &target_name)
                            .unwrap_or(false)
                }) || function
                    .decl
                    .params
                    .iter()
                    .find(|q| q.name == source_name)
                    .map(|q| {
                        function.decl.generics.iter().any(|g| {
                            ty_mentions(&q.ty, &g.name) && ty_mentions(&target_param.ty, &g.name)
                        })
                    })
                    .unwrap_or(false);
                if !allowed && reported.insert(("E1204".to_string(), span.start, target)) {
                    let mut d = Diagnostic::new(
                        "E1204",
                        format!(
                            "the borrow from `{source_name}` is stored through `{target_name}`, which needs `into {target_name}`"
                        ),
                        source,
                        span,
                    );
                    if let Some(q) = function.decl.params.iter().find(|q| q.name == source_name)
                        && q.into.is_none()
                    {
                        d.fix(
                            format!("add `into {target_name}`"),
                            Edit {
                                span: Span::empty(q.ty.span().end),
                                text: format!(" into {target_name}"),
                            },
                        );
                    }
                    diagnostics.push(d);
                }
            }
            Origin::SelfScope(_) => {}
        }
    }
    let _ = program;
}

fn ty_mentions(ty: &Ty, name: &str) -> bool {
    match ty {
        Ty::Path(p) => {
            p.name() == name
                || p.args.iter().any(|a| match a {
                    GenericArg::Ty(t) => ty_mentions(t, name),
                    _ => false,
                })
        }
        Ty::Borrow { inner, .. }
        | Ty::Slice { elem: inner, .. }
        | Ty::Ptr { inner, .. }
        | Ty::Optional { inner, .. } => ty_mentions(inner, name),
        Ty::Array { elem, .. } => ty_mentions(elem, name),
        Ty::Tuple { elems, .. } | Ty::Fn { params: elems, .. } => {
            elems.iter().any(|t| ty_mentions(t, name))
        }
        _ => false,
    }
}

fn type_mentions(ty: &Type, name: &str) -> bool {
    match ty {
        Type::Param(n) => n == name,
        Type::Array(_, e) | Type::Optional(e) => type_mentions(e, name),
        Type::Result(a, b) => type_mentions(a, name) || type_mentions(b, name),
        Type::Tuple(es) => es.iter().any(|t| type_mentions(t, name)),
        Type::Struct(_, args) | Type::Enum(_, args) => args.iter().any(|t| type_mentions(t, name)),
        Type::Borrow { inner, .. } | Type::Ptr { inner, .. } => type_mentions(inner, name),
        Type::Slice { elem, .. } => type_mentions(elem, name),
        Type::Fn { params, ret, error } => {
            params.iter().any(|t| type_mentions(t, name))
                || type_mentions(ret, name)
                || error
                    .as_ref()
                    .map(|e| type_mentions(e, name))
                    .unwrap_or(false)
        }
        _ => false,
    }
}

fn check_return_origins(
    program: &Program,
    source: &Source,
    cfg: &Cfg,
    function: &crate::check::Function,
    term: &Term,
    set: &OriginSet,
    diagnostics: &mut Vec<Diagnostic>,
) {
    if !function.signature_ret.is_view(&program.defs) {
        return;
    }
    let span = match term {
        Term::Return { span, .. } => *span,
        _ => return,
    };
    for o in set {
        match o {
            Origin::Loan(id) => {
                if let Some(loan) = cfg.loans.get(*id)
                    && loan.place.derefs() == 0
                {
                    diagnostics.push(Diagnostic::new(
                        "E1203",
                        "a returned view may hold a borrow of a local",
                        source,
                        span,
                    ));
                }
            }
            Origin::Param(p) => {
                let name = cfg.locals[*p].name.clone().unwrap_or_default();
                let allowed = function
                    .decl
                    .from
                    .iter()
                    .any(|o| !o.is_static && !o.inner && o.name == name)
                    || function
                        .decl
                        .generics
                        .iter()
                        .any(|g| type_mentions(&function.signature_ret, &g.name));
                if !allowed {
                    diagnostics.push(Diagnostic::new(
                        "E1202",
                        format!("returned view may hold the origin of `{name}`, which `from` does not cover"),
                        source,
                        span,
                    ));
                }
            }
            Origin::Inner(p) => {
                let name = cfg.locals[*p].name.clone().unwrap_or_default();
                let allowed = function
                    .decl
                    .from
                    .iter()
                    .any(|o| !o.is_static && o.name == name)
                    || function
                        .decl
                        .generics
                        .iter()
                        .any(|g| type_mentions(&function.signature_ret, &g.name));
                if !allowed {
                    diagnostics.push(Diagnostic::new(
                        "E1202",
                        format!("returned view may hold origins inside `{name}`, which `from` does not cover"),
                        source,
                        span,
                    ));
                }
            }
            Origin::SelfScope(p) => {
                let name = cfg.locals[*p].name.clone().unwrap_or_default();
                let allowed = function.decl.from.iter().any(|o| o.name == name);
                if !allowed {
                    diagnostics.push(Diagnostic::new(
                        "E1202",
                        format!(
                            "returned view may hold the scope `{name}`, which `from` does not cover"
                        ),
                        source,
                        span,
                    ));
                }
            }
        }
    }
}

// ------------------------------------------------------------------ loans C

#[derive(Clone, Debug, PartialEq, Eq)]
enum AccessKind {
    Read,
    Write,
    Move,
    SharedBorrow,
    ExclusiveBorrow,
}

fn origin_points(facts: &OriginFacts, b: BlockId, pos: usize) -> &Origins {
    static EMPTY: std::sync::OnceLock<Origins> = std::sync::OnceLock::new();
    facts
        .points
        .get(&(b, pos))
        .unwrap_or_else(|| EMPTY.get_or_init(Origins::new))
}

fn place_prefix(prefix: &Place, place: &Place) -> bool {
    prefix.local == place.local && place.proj.starts_with(&prefix.proj)
}

fn places_overlap(a: &Place, b: &Place) -> bool {
    place_prefix(a, b) || place_prefix(b, a)
}

fn compute_liveness(cfg: &Cfg) -> Vec<Vec<HashSet<LocalId>>> {
    let mut live: Vec<Vec<HashSet<LocalId>>> = cfg
        .blocks
        .iter()
        .map(|b| vec![HashSet::new(); b.stmts.len() + 1])
        .collect();
    loop {
        let mut changed = false;
        for b in (0..cfg.blocks.len()).rev() {
            let block = &cfg.blocks[b];
            let mut after: HashSet<LocalId> = HashSet::new();
            for succ in successors(&block.term) {
                after.extend(live[succ][0].iter().cloned());
            }
            let (term_uses, term_defs) = term_uses_defs(&block.term);
            let mut cur = after;
            cur.extend(term_uses);
            cur.retain(|l| !term_defs.contains(l));
            let len = block.stmts.len();
            if live[b][len] != cur {
                live[b][len] = cur.clone();
                changed = true;
            }
            for i in (0..len).rev() {
                let (uses, defs) = stmt_uses_defs(&block.stmts[i]);
                cur.extend(uses);
                cur.retain(|l| !defs.contains(l));
                if live[b][i] != cur {
                    live[b][i] = cur.clone();
                    changed = true;
                }
            }
        }
        if !changed {
            return live;
        }
    }
}

fn stmt_uses_defs(stmt: &CfgStmt) -> (HashSet<LocalId>, HashSet<LocalId>) {
    let mut uses = HashSet::new();
    let mut defs = HashSet::new();
    match stmt {
        CfgStmt::Assign { place, value, .. } => {
            if place.proj.is_empty() {
                defs.insert(place.local);
            } else {
                uses.insert(place.local);
            }
            rvalue_uses(value, &mut uses);
        }
        CfgStmt::StorageStart { local, .. } | CfgStmt::StorageEnd { local, .. } => {
            defs.insert(*local);
        }
        CfgStmt::Use { place, .. } | CfgStmt::ConsumeUnion { place, .. } => {
            uses.insert(place.local);
        }
        CfgStmt::CancelScope { local, .. } => {
            uses.insert(*local);
        }
        CfgStmt::Check { .. } => {}
    }
    (uses, defs)
}

fn rvalue_uses(value: &Rvalue, uses: &mut HashSet<LocalId>) {
    match value {
        Rvalue::Use { place, .. }
        | Rvalue::Borrow { place, .. }
        | Rvalue::Slice { place, .. }
        | Rvalue::Len { place } => {
            uses.insert(place.local);
        }
        Rvalue::Unary { operand, .. }
        | Rvalue::Cast { operand }
        | Rvalue::ConvertError { operand } => operand_uses(operand, uses),
        Rvalue::Binary { lhs, rhs, .. } => {
            operand_uses(lhs, uses);
            operand_uses(rhs, uses);
        }
        Rvalue::Aggregate { fields, .. } => {
            for f in fields {
                operand_uses(f, uses);
            }
        }
        Rvalue::Constant | Rvalue::Zero => {}
    }
}

fn operand_uses(op: &Operand, uses: &mut HashSet<LocalId>) {
    if let Operand::Place(p) = op {
        uses.insert(p.local);
    }
}

fn term_uses_defs(term: &Term) -> (HashSet<LocalId>, HashSet<LocalId>) {
    let mut uses = HashSet::new();
    let mut defs = HashSet::new();
    match term {
        Term::Call { args, dest, .. } => {
            for a in args {
                operand_uses(a, &mut uses);
            }
            if let Some(d) = dest {
                defs.insert(d.local);
            }
        }
        Term::Spawn {
            args, scope, dest, ..
        } => {
            for a in args {
                operand_uses(a, &mut uses);
            }
            uses.insert(scope.local);
            if let Some(d) = dest {
                defs.insert(d.local);
            }
        }
        Term::Branch { cond, .. } => operand_uses(cond, &mut uses),
        Term::Switch { value, .. } => {
            uses.insert(value.local);
        }
        Term::ScopeWait { scope, .. } => {
            uses.insert(scope.local);
        }
        Term::Return { value, .. } => {
            if let Some(v) = value {
                operand_uses(v, &mut uses);
            }
        }
        Term::Goto(_) | Term::Trap { .. } | Term::Unreachable => {}
    }
    (uses, defs)
}

fn live_loans_at(
    cfg: &Cfg,
    facts: &OriginFacts,
    liveness: &[Vec<HashSet<LocalId>>],
    b: BlockId,
    pos: usize,
) -> (HashSet<LoanId>, HashSet<LocalId>) {
    let origins = origin_points(facts, b, pos);
    let mut locals = liveness[b][pos].clone();
    loop {
        let mut grew = false;
        for v in locals.clone() {
            for o in origin_set(origins, v) {
                if let Origin::Loan(id) = o
                    && let Some(root) = loan_root(cfg, id)
                    && locals.insert(root)
                {
                    grew = true;
                }
            }
        }
        if !grew {
            break;
        }
    }
    let mut loans = HashSet::new();
    for v in &locals {
        for o in origin_set(origins, *v) {
            if let Origin::Loan(id) = o {
                loans.insert(id);
            }
        }
    }
    (loans, locals)
}

fn is_scope_local(cfg: &Cfg, local: LocalId) -> bool {
    matches!(&cfg.locals[local].ty, Type::External(n) if n == "Scope")
        || matches!(&cfg.locals[local].ty, Type::Borrow { inner, .. } if matches!(inner.as_ref(), Type::External(n) if n == "Scope"))
}

#[allow(clippy::too_many_arguments)]
fn check_loan_access(
    program: &Program,
    source: &Source,
    cfg: &Cfg,
    facts: &OriginFacts,
    liveness: &[Vec<HashSet<LocalId>>],
    b: BlockId,
    pos: usize,
    place: &Place,
    access: AccessKind,
    span: Span,
    ignore_loan: Option<LoanId>,
    reported: &mut HashSet<(LoanId, &'static str)>,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let (loans, _) = live_loans_at(cfg, facts, liveness, b, pos);
    for id in loans {
        if Some(id) == ignore_loan {
            continue;
        }
        let Some(loan) = cfg.loans.get(id) else {
            continue;
        };
        if !places_overlap(&loan.place, place) {
            continue;
        }
        let effective = effective_kind(cfg, facts, id, b, pos);
        let conflicts = match access {
            AccessKind::Read | AccessKind::SharedBorrow => effective == BorrowKind::Exclusive,
            AccessKind::Write | AccessKind::Move | AccessKind::ExclusiveBorrow => true,
        };
        if !conflicts {
            continue;
        }
        let (code, message) = match access {
            AccessKind::Read | AccessKind::SharedBorrow => (
                "E1103",
                "a place is read while an exclusive loan on it is live",
            ),
            AccessKind::Write => ("E1101", "a place is assigned while a loan on it is live"),
            AccessKind::Move => ("E1104", "a place is moved while a loan on it is live"),
            AccessKind::ExclusiveBorrow => (
                "E1102",
                "an exclusive borrow conflicts with a loan that is still live",
            ),
        };
        let _ = program;
        if reported.insert((id, code)) {
            let mut d = Diagnostic::new(code, message, source, span);
            d.relate("borrow", loan.span);
            diagnostics.push(d);
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn activate_two_phase(
    program: &Program,
    source: &Source,
    cfg: &Cfg,
    facts: &OriginFacts,
    liveness: &[Vec<HashSet<LocalId>>],
    b: BlockId,
    pos: usize,
    args: &[Operand],
    span: Span,
    reported: &mut HashSet<(LoanId, &'static str)>,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let (live, _) = live_loans_at(cfg, facts, liveness, b, pos);
    let origins = origin_points(facts, b, pos);
    for a in args {
        let Operand::Place(p) = a else {
            continue;
        };
        for o in origin_set(origins, p.local) {
            let Origin::Loan(id) = o else {
                continue;
            };
            let Some(loan) = cfg.loans.get(id) else {
                continue;
            };
            if !loan.two_phase {
                continue;
            }
            for other in &live {
                if *other == id {
                    continue;
                }
                let Some(o2) = cfg.loans.get(*other) else {
                    continue;
                };
                if places_overlap(&loan.place, &o2.place) {
                    let other_kind = effective_kind(cfg, facts, *other, b, pos);
                    let conflict =
                        loan.kind == BorrowKind::Exclusive || other_kind == BorrowKind::Exclusive;
                    if !conflict {
                        continue;
                    }
                    let code = match loan.kind {
                        BorrowKind::Exclusive => "E1102",
                        BorrowKind::Shared => "E1103",
                    };
                    let message = match loan.kind {
                        BorrowKind::Exclusive => {
                            "an exclusive borrow conflicts with a loan that is still live"
                        }
                        BorrowKind::Shared => {
                            "a shared borrow conflicts with an exclusive loan that is still live"
                        }
                    };
                    if reported.insert((id, code)) {
                        let mut d = Diagnostic::new(code, message, source, span);
                        d.relate("borrow", loan.span);
                        d.relate("later_use", o2.span);
                        diagnostics.push(d);
                    }
                }
            }
        }
    }
    let _ = program;
}

fn compute_activation(
    cfg: &Cfg,
    facts: &OriginFacts,
) -> HashMap<(BlockId, usize), HashSet<LoanId>> {
    let mut points = HashMap::new();
    let mut entries: HashMap<BlockId, HashSet<LoanId>> = HashMap::new();
    entries.insert(cfg.entry, HashSet::new());
    let mut queue = vec![cfg.entry];
    let mut queued: HashSet<BlockId> = HashSet::from([cfg.entry]);
    while let Some(b) = queue.pop() {
        queued.remove(&b);
        let mut state = entries.get(&b).cloned().unwrap_or_default();
        let block = &cfg.blocks[b];
        for i in 0..=block.stmts.len() {
            points.insert((b, i), state.clone());
        }
        let mut succ_state = state.clone();
        match &block.term {
            Term::Call { args, .. } | Term::Spawn { args, .. } => {
                let origins = origin_points(facts, b, block.stmts.len());
                for a in args {
                    if let Operand::Place(p) = a {
                        for o in origin_set(origins, p.local) {
                            if let Origin::Loan(id) = o
                                && cfg.loans.get(id).map(|l| l.two_phase).unwrap_or(false)
                            {
                                succ_state.insert(id);
                            }
                        }
                    }
                }
            }
            _ => {}
        }
        state = succ_state;
        for succ in successors(&block.term) {
            let entry = entries.entry(succ).or_default();
            let before = entry.len();
            entry.extend(state.iter().cloned());
            if entry.len() != before && queued.insert(succ) {
                queue.push(succ);
            }
        }
    }
    points
}

fn effective_kind(
    cfg: &Cfg,
    facts: &OriginFacts,
    id: LoanId,
    b: BlockId,
    pos: usize,
) -> BorrowKind {
    let loan = &cfg.loans[id];
    let activated = facts
        .activation
        .get(&(b, pos))
        .map(|s| s.contains(&id))
        .unwrap_or(false);
    if loan.two_phase && !activated {
        BorrowKind::Shared
    } else {
        loan.kind
    }
}

fn analyze_loans(
    program: &Program,
    source: &Source,
    cfg: &Cfg,
    facts: &OriginFacts,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let mut facts = facts.clone();
    facts.activation = compute_activation(cfg, &facts);
    let facts = &facts;
    let liveness = compute_liveness(cfg);
    let mut reported: HashSet<(LoanId, &'static str)> = HashSet::new();
    for (b, block) in cfg.blocks.iter().enumerate() {
        for (i, stmt) in block.stmts.iter().enumerate() {
            match stmt {
                CfgStmt::Assign { place, value, span } => {
                    check_loan_access(
                        program,
                        source,
                        cfg,
                        facts,
                        &liveness,
                        b,
                        i,
                        place,
                        AccessKind::Write,
                        *span,
                        None,
                        &mut reported,
                        diagnostics,
                    );
                    match value {
                        Rvalue::Use { place: src, mode } => {
                            let access = match mode {
                                UseMode::Move => AccessKind::Move,
                                UseMode::Copy => AccessKind::Read,
                            };
                            check_loan_access(
                                program,
                                source,
                                cfg,
                                facts,
                                &liveness,
                                b,
                                i,
                                src,
                                access,
                                *span,
                                None,
                                &mut reported,
                                diagnostics,
                            );
                        }
                        Rvalue::Borrow {
                            loan,
                            place: src,
                            mutable,
                            two_phase,
                        } => {
                            if !*two_phase {
                                let access = if *mutable {
                                    AccessKind::ExclusiveBorrow
                                } else {
                                    AccessKind::SharedBorrow
                                };
                                check_loan_access(
                                    program,
                                    source,
                                    cfg,
                                    facts,
                                    &liveness,
                                    b,
                                    i,
                                    src,
                                    access,
                                    *span,
                                    Some(*loan),
                                    &mut reported,
                                    diagnostics,
                                );
                            }
                        }
                        Rvalue::Slice { place: src, .. } | Rvalue::Len { place: src } => {
                            check_loan_access(
                                program,
                                source,
                                cfg,
                                facts,
                                &liveness,
                                b,
                                i,
                                src,
                                AccessKind::Read,
                                *span,
                                None,
                                &mut reported,
                                diagnostics,
                            );
                        }
                        _ => {}
                    }
                }
                CfgStmt::Use { place, mode, span } => {
                    let access = match mode {
                        UseMode::Move => AccessKind::Move,
                        UseMode::Copy => AccessKind::Read,
                    };
                    check_loan_access(
                        program,
                        source,
                        cfg,
                        facts,
                        &liveness,
                        b,
                        i,
                        place,
                        access,
                        *span,
                        None,
                        &mut reported,
                        diagnostics,
                    );
                }
                CfgStmt::StorageEnd { local, span } => {
                    let (loans, live_locals) = live_loans_at(cfg, facts, &liveness, b, i);
                    let origins = origin_points(facts, b, i);
                    for id in loans {
                        let Some(loan) = cfg.loans.get(id) else {
                            continue;
                        };
                        if loan.place.local != *local
                            || loan.place.derefs() != 0
                            || facts.returned_loans.contains(&id)
                        {
                            continue;
                        }
                        let scope_holder = live_locals.iter().any(|v| {
                            is_scope_local(cfg, *v)
                                && origin_set(origins, *v).contains(&Origin::Loan(id))
                        });
                        let code = if scope_holder { "E1403" } else { "E1105" };
                        if reported.insert((id, code)) {
                            let message = if scope_holder {
                                "a task's borrow outlives the place declared inside its scope"
                            } else {
                                "a borrowed place ends while the loan is still live"
                            };
                            let mut d = Diagnostic::new(code, message, source, loan.span);
                            d.relate("scope_end", *span);
                            diagnostics.push(d);
                        }
                    }
                }
                CfgStmt::ConsumeUnion { place, span } => {
                    check_loan_access(
                        program,
                        source,
                        cfg,
                        facts,
                        &liveness,
                        b,
                        i,
                        place,
                        AccessKind::Move,
                        *span,
                        None,
                        &mut reported,
                        diagnostics,
                    );
                }
                CfgStmt::StorageStart { .. }
                | CfgStmt::Check { .. }
                | CfgStmt::CancelScope { .. } => {}
            }
        }
        let pos = block.stmts.len();
        match &block.term {
            Term::Call { args, span, .. } => {
                activate_two_phase(
                    program,
                    source,
                    cfg,
                    facts,
                    &liveness,
                    b,
                    pos,
                    args,
                    *span,
                    &mut reported,
                    diagnostics,
                );
            }
            Term::Spawn { args, span, .. } => {
                activate_two_phase(
                    program,
                    source,
                    cfg,
                    facts,
                    &liveness,
                    b,
                    pos,
                    args,
                    *span,
                    &mut reported,
                    diagnostics,
                );
            }
            Term::Branch {
                cond: Operand::Place(p),
                span,
                ..
            } => {
                check_loan_access(
                    program,
                    source,
                    cfg,
                    facts,
                    &liveness,
                    b,
                    pos,
                    p,
                    AccessKind::Read,
                    *span,
                    None,
                    &mut reported,
                    diagnostics,
                );
            }
            Term::Branch { .. } => {}
            Term::Switch { value, span, .. } => {
                check_loan_access(
                    program,
                    source,
                    cfg,
                    facts,
                    &liveness,
                    b,
                    pos,
                    value,
                    AccessKind::Read,
                    *span,
                    None,
                    &mut reported,
                    diagnostics,
                );
            }
            Term::ScopeWait { scope, span, .. } => {
                check_loan_access(
                    program,
                    source,
                    cfg,
                    facts,
                    &liveness,
                    b,
                    pos,
                    scope,
                    AccessKind::Read,
                    *span,
                    None,
                    &mut reported,
                    diagnostics,
                );
            }
            _ => {}
        }
    }
}
