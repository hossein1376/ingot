package codegen

import (
	"sort"

	"github.com/hossein1376/ingot/internal/sema"
	"github.com/hossein1376/ingot/internal/syntax"
)

// Register promotion: scalar locals whose address is never taken live in
// callee-saved registers (x19-x28 from the top, d8-d15) instead of the
// stack frame. Vectors are promoted only in leaf functions, because AAPCS64
// preserves just the low 64 bits of v8-v15 across calls.

type preg struct {
	n  int
	fp bool
}

type useInfo struct {
	addrTaken map[*sema.Var]bool
	weight    map[*sema.Var]int
	leaf      bool
}

func (g *gen) analyze(b *sema.Body) useInfo {
	u := useInfo{addrTaken: map[*sema.Var]bool{}, weight: map[*sema.Var]int{}, leaf: true}
	var expr func(e syntax.Expr, w int)
	var stmt func(s syntax.Stmt, w int)
	visiting := map[*sema.Body]bool{}
	expr = func(e syntax.Expr, w int) {
		switch e := e.(type) {
		case *syntax.IdentExpr:
			if v, ok := g.info.Uses[e].(*sema.Var); ok {
				u.weight[v] += w
			}
		case *syntax.UnaryExpr:
			if e.Op == "&" {
				inner := e.X
				for {
					p, ok := inner.(*syntax.ParenExpr)
					if !ok {
						break
					}
					inner = p.X
				}
				if id, ok := inner.(*syntax.IdentExpr); ok {
					if v, ok := g.info.Uses[id].(*sema.Var); ok {
						u.addrTaken[v] = true
					}
				}
			}
			expr(e.X, w)
		case *syntax.BinaryExpr:
			expr(e.X, w)
			expr(e.Y, w)
		case *syntax.CallExpr:
			if cb := g.inlineBody(e); cb != nil && !visiting[cb] {
				// expanded in place: its locals live in this frame
				for _, a := range e.Args {
					expr(a, w)
				}
				for _, p := range cb.Params {
					u.weight[p] += w
				}
				visiting[cb] = true
				stmt(cb.Block, w)
				visiting[cb] = false
				break
			}
			u.leaf = false
			expr(e.Fn, w)
			for _, a := range e.Args {
				expr(a, w)
			}
		case *syntax.BuiltinCall:
			if e.Name == "qos" {
				u.leaf = false
			}
			for _, a := range e.Args {
				expr(a, w)
			}
		case *syntax.FieldExpr:
			expr(e.X, w)
		case *syntax.DerefExpr:
			expr(e.X, w)
		case *syntax.IndexExpr:
			expr(e.X, w)
			expr(e.Index, w)
		case *syntax.SliceExpr:
			expr(e.X, w)
			expr(e.Lo, w)
			expr(e.Hi, w)
		case *syntax.CastExpr:
			expr(e.X, w)
		case *syntax.CompositeLit:
			for _, v := range e.Values {
				expr(v, w)
			}
		case *syntax.ParenExpr:
			expr(e.X, w)
		case *syntax.TryExpr:
			expr(e.X, w)
		case *syntax.CatchExpr:
			expr(e.X, w)
			if v := g.info.Defs[e]; v != nil {
				u.weight[v] += w
			}
			if e.Handler != nil {
				stmt(e.Handler, w)
			} else {
				expr(e.Fallback, w)
			}
		}
	}
	stmt = func(s syntax.Stmt, w int) {
		switch s := s.(type) {
		case *syntax.Block:
			for _, x := range s.Stmts {
				stmt(x, w)
			}
		case *syntax.LetStmt:
			expr(s.Value, w)
			if v := g.info.Defs[s]; v != nil {
				u.weight[v] += w
			}
		case *syntax.AssignStmt:
			expr(s.Target, w)
			expr(s.Value, w)
		case *syntax.ExprStmt:
			expr(s.X, w)
		case *syntax.IfStmt:
			expr(s.Cond, w)
			stmt(s.Then, w)
			if s.Else != nil {
				stmt(s.Else, w)
			}
		case *syntax.WhileStmt:
			expr(s.Cond, w*8)
			stmt(s.Body, w*8)
		case *syntax.ForStmt:
			expr(s.Lo, w)
			expr(s.Hi, w)
			if v := g.info.Defs[s]; v != nil {
				u.weight[v] += w * 16
			}
			stmt(s.Body, w*8)
		case *syntax.ReturnStmt:
			if s.Value != nil {
				expr(s.Value, w)
			}
		case *syntax.DeferStmt:
			stmt(s.Body, w*2) // emitted at every exit
		case *syntax.SwitchStmt:
			expr(s.X, w)
			for _, cs := range s.Cases {
				stmt(cs.Body, w)
			}
			if s.Else != nil {
				stmt(s.Else, w)
			}
		}
	}
	stmt(b.Block, 1)
	return u
}

// choosePromotions picks up to maxG integer and maxF float/vector locals.
func (g *gen) choosePromotions(b *sema.Body, u useInfo, maxG, maxF int) map[*sema.Var]preg {
	var gc, fc []*sema.Var
	for _, v := range g.frameLocals(b) {
		t := v.Type
		if u.addrTaken[v] || t.IsAggregate() || u.weight[v] == 0 && !contains(b.Params, v) {
			continue
		}
		switch {
		case t.IsGPR():
			gc = append(gc, v)
		case t.Kind == sema.Float || (t.Kind == sema.Vec && u.leaf):
			fc = append(fc, v)
		}
	}
	byWeight := func(vs []*sema.Var) {
		sort.SliceStable(vs, func(i, j int) bool { return u.weight[vs[i]] > u.weight[vs[j]] })
	}
	byWeight(gc)
	byWeight(fc)
	out := map[*sema.Var]preg{}
	for i, v := range gc {
		if i >= maxG {
			break
		}
		out[v] = preg{n: gprBase + gprCount - 1 - i}
	}
	for i, v := range fc {
		if i >= maxF {
			break
		}
		out[v] = preg{n: 15 - i, fp: true}
	}
	return out
}

func pname(p preg, t *sema.Type) string {
	if !p.fp {
		return "x" + itoa(p.n)
	}
	switch {
	case t.Kind == sema.Vec:
		return "q" + itoa(p.n)
	case t.Size == 4:
		return "s" + itoa(p.n)
	}
	return "d" + itoa(p.n)
}

func itoa(n int) string {
	if n < 10 {
		return string(rune('0' + n))
	}
	return string(rune('0'+n/10)) + string(rune('0'+n%10))
}

// readPromoted copies a promoted variable into a fresh temporary.
func (g *gen) readPromoted(p preg, t *sema.Type, r reg) {
	switch {
	case !p.fp:
		g.emit("mov %s, x%d", x(r), p.n)
	case t.Kind == sema.Vec:
		g.emit("mov %s.16b, v%d.16b", v(r), p.n)
	default:
		g.emit("fmov %s, %s", rn(r, t), pname(p, t))
	}
}

// writePromoted stores a temporary into a promoted variable.
func (g *gen) writePromoted(p preg, t *sema.Type, r reg) {
	switch {
	case !p.fp:
		g.emit("mov x%d, %s", p.n, x(r))
	case t.Kind == sema.Vec:
		g.emit("mov v%d.16b, %s.16b", p.n, v(r))
	default:
		g.emit("fmov %s, %s", pname(p, t), rn(r, t))
	}
}

// inlineBody returns the body to expand for a direct call to an inline fn.
func (g *gen) inlineBody(e *syntax.CallExpr) *sema.Body {
	id, ok := e.Fn.(*syntax.IdentExpr)
	if !ok {
		return nil
	}
	fn, ok := g.info.Uses[id].(*sema.Func)
	if !ok || fn.Decl == nil || !fn.Decl.Inline {
		return nil
	}
	return g.bodies[fn]
}

// frameLocals lists b's locals plus those of every inline fn expanded into
// it (transitively), each once.
func (g *gen) frameLocals(b *sema.Body) []*sema.Var {
	out := append([]*sema.Var(nil), b.Locals...)
	seen := map[*sema.Body]bool{b: true}
	var walk func(n any)
	walk = func(n any) {
		syntax.Inspect(n, func(x any) bool {
			if c, ok := x.(*syntax.CallExpr); ok {
				if cb := g.inlineBody(c); cb != nil && !seen[cb] {
					seen[cb] = true
					out = append(out, cb.Locals...)
					walk(cb.Block)
				}
			}
			return true
		})
	}
	walk(b.Block)
	return out
}
