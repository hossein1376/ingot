package codegen

import (
	"fmt"

	"github.com/hossein1376/ingot/internal/sema"
	"github.com/hossein1376/ingot/internal/syntax"
)

func (g *gen) block(b *syntax.Block) {
	for _, s := range b.Stmts {
		g.stmt(s)
	}
}

func (g *gen) typeOf(e syntax.Expr) *sema.Type { return g.info.Types[e] }

func (g *gen) stmt(s syntax.Stmt) {
	switch s := s.(type) {
	case *syntax.Block:
		g.block(s)
	case *syntax.LetStmt:
		lv := g.info.Defs[s]
		g.initVar(lv, s.Value)
	case *syntax.AssignStmt:
		g.assign(s)
	case *syntax.ExprStmt:
		r, ok := g.expr(s.X)
		if ok {
			g.free(r)
		}
	case *syntax.IfStmt:
		if val, known := g.constCond(s.Cond); known {
			// compile-time condition (e.g. @is_macos()): the untaken branch is not compiled
			if val {
				g.block(s.Then)
			} else if s.Else != nil {
				g.stmt(s.Else)
			}
			return
		}
		end := g.newLabel("endif")
		els := end
		if s.Else != nil {
			els = g.newLabel("else")
		}
		g.jumpIf(s.Cond, els, false)
		g.block(s.Then)
		if s.Else != nil {
			g.emit("b %s", end)
			g.label(els)
			g.stmt(s.Else)
		}
		g.label(end)
	case *syntax.WhileStmt:
		top, cont, end := g.newLabel("while"), g.newLabel("wcond"), g.newLabel("wend")
		// rotated loop: test at the bottom, one branch per iteration
		g.emit("b %s", cont)
		g.label(top)
		g.loops = append(g.loops, loopLabels{brk: end, cont: cont})
		g.block(s.Body)
		g.loops = g.loops[:len(g.loops)-1]
		g.label(cont)
		g.jumpIf(s.Cond, top, true)
		g.label(end)
	case *syntax.ForStmt:
		lv := g.info.Defs[s]
		t := lv.Type
		pr, promoted := g.promoted[lv]
		lo, _ := g.expr(s.Lo)
		if promoted {
			g.writePromoted(pr, t, lo)
		} else {
			g.store(lo, g.mem(g.slots[lv]), t)
		}
		g.free(lo)
		hiSlot := g.slot(8, 8)
		hi, _ := g.expr(s.Hi)
		g.emit("str %s, %s", x(hi), g.mem(hiSlot))
		g.free(hi)
		top, cont, test, end := g.newLabel("for"), g.newLabel("fnext"), g.newLabel("ftest"), g.newLabel("fend")
		g.emit("b %s", test)
		g.label(top)
		g.loops = append(g.loops, loopLabels{brk: end, cont: cont})
		g.block(s.Body)
		g.loops = g.loops[:len(g.loops)-1]
		g.label(cont)
		ctr := fmt.Sprintf("x%d", pr.n)
		if promoted {
			g.emit("add %s, %s, #1", ctr, ctr)
		} else {
			ctr = "x9"
			g.emit("ldr x9, %s", g.mem(g.slots[lv]))
			g.emit("add x9, x9, #1")
			g.emit("str x9, %s", g.mem(g.slots[lv]))
		}
		g.label(test)
		if !promoted {
			g.emit("ldr x9, %s", g.mem(g.slots[lv]))
		}
		g.emit("ldr x10, %s", g.mem(hiSlot))
		g.emit("cmp %s, x10", ctr)
		if t.Signed {
			g.emit("b.lt %s", top)
		} else {
			g.emit("b.lo %s", top)
		}
		g.label(end)
	case *syntax.ReturnStmt:
		if s.Value != nil {
			t := g.typeOf(s.Value)
			r, _ := g.expr(s.Value)
			switch {
			case t.IsAggregate():
				g.emit("ldr x14, %s", g.mem(g.retSlot))
				g.copyMem("x14", x(r), t.Size)
			case r.fp:
				if t.Kind == sema.Vec {
					g.emit("mov v0.16b, %s.16b", v(r))
				} else {
					g.emit("fmov %s, %s", retF(t), rn(r, t))
				}
			default:
				g.emit("mov x0, %s", x(r))
			}
			g.free(r)
		}
		g.emit("b %s", g.retLabel)
	case *syntax.BreakStmt:
		g.emit("b %s", g.loops[len(g.loops)-1].brk)
	case *syntax.ContinueStmt:
		g.emit("b %s", g.loops[len(g.loops)-1].cont)
	}
}

// constCond evaluates conditions known at compile time for the current target.
func (g *gen) constCond(e syntax.Expr) (bool, bool) {
	if k, ok := g.info.Consts[e]; ok && g.typeOf(e) == sema.TBool {
		return k.I != 0, true
	}
	switch e := e.(type) {
	case *syntax.ParenExpr:
		return g.constCond(e.X)
	case *syntax.BuiltinCall:
		if e.Name == "is_macos" {
			return g.t.Darwin, true
		}
	case *syntax.UnaryExpr:
		if e.Op == "!" {
			v, ok := g.constCond(e.X)
			return !v, ok
		}
	}
	return false, false
}

func retF(t *sema.Type) string {
	if t.Size == 4 {
		return "s0"
	}
	return "d0"
}

// initVar stores the initial value of a local.
func (g *gen) initVar(lv *sema.Var, val syntax.Expr) {
	t := lv.Type
	off := g.slots[lv]
	if t.IsAggregate() {
		if b, ok := val.(*syntax.BuiltinCall); ok && b.Name == "zero" {
			g.subImm("x14", "x29", off)
			g.zeroMem("x14", t.Size)
			return
		}
		r, _ := g.expr(val)
		g.subImm("x14", "x29", off)
		g.copyMem("x14", x(r), t.Size)
		g.free(r)
		return
	}
	r, _ := g.expr(val)
	if pr, ok := g.promoted[lv]; ok {
		g.writePromoted(pr, t, r)
	} else {
		g.store(r, g.mem(off), t)
	}
	g.free(r)
}

// promotedTarget returns the promoted variable an assignment targets, if any.
func (g *gen) promotedTarget(e syntax.Expr) (*sema.Var, preg, bool) {
	for {
		p, ok := e.(*syntax.ParenExpr)
		if !ok {
			break
		}
		e = p.X
	}
	id, ok := e.(*syntax.IdentExpr)
	if !ok {
		return nil, preg{}, false
	}
	lv, ok := g.info.Uses[id].(*sema.Var)
	if !ok {
		return nil, preg{}, false
	}
	pr, ok := g.promoted[lv]
	return lv, pr, ok
}

func (g *gen) assign(s *syntax.AssignStmt) {
	t := g.typeOf(s.Target)
	if _, pr, ok := g.promotedTarget(s.Target); ok {
		if s.Op == "=" {
			r, _ := g.expr(s.Value)
			g.writePromoted(pr, t, r)
			g.free(r)
			return
		}
		cur := g.alloc(t, s.Sp)
		g.readPromoted(pr, t, cur)
		g.arithExpr(s.Op[:len(s.Op)-1], cur, s.Value, t, s.Sp)
		g.writePromoted(pr, t, cur)
		g.free(cur)
		return
	}
	addr := g.addr(s.Target)
	if t.IsAggregate() {
		if b, ok := s.Value.(*syntax.BuiltinCall); ok && b.Name == "zero" {
			g.zeroMem(x(addr), t.Size)
			g.free(addr)
			return
		}
		r, _ := g.expr(s.Value)
		g.copyMem(x(addr), x(r), t.Size)
		g.free(r)
		g.free(addr)
		return
	}
	if s.Op == "=" {
		r, _ := g.expr(s.Value)
		g.store(r, "["+x(addr)+"]", t)
		g.free(r)
		g.free(addr)
		return
	}
	cur := g.alloc(t, s.Sp)
	g.load(cur, "["+x(addr)+"]", t)
	g.arithExpr(s.Op[:len(s.Op)-1], cur, s.Value, t, s.Sp)
	g.store(cur, "["+x(addr)+"]", t)
	g.free(cur)
	g.free(addr)
}
