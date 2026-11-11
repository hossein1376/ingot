package syntax

// Inspect calls f for n and every expression and statement below it, in
// source order. If f returns false, the children of that node are skipped.
func Inspect(n any, f func(any) bool) {
	if n == nil || !f(n) {
		return
	}
	e := func(x Expr) {
		if x != nil {
			Inspect(x, f)
		}
	}
	s := func(x Stmt) {
		if x != nil {
			Inspect(x, f)
		}
	}
	switch n := n.(type) {
	case *UnaryExpr:
		e(n.X)
	case *BinaryExpr:
		e(n.X)
		e(n.Y)
	case *CallExpr:
		e(n.Fn)
		for _, a := range n.Args {
			e(a)
		}
	case *BuiltinCall:
		for _, a := range n.Args {
			e(a)
		}
	case *FieldExpr:
		e(n.X)
	case *DerefExpr:
		e(n.X)
	case *IndexExpr:
		e(n.X)
		e(n.Index)
	case *SliceExpr:
		e(n.X)
		e(n.Lo)
		e(n.Hi)
	case *CastExpr:
		e(n.X)
	case *CompositeLit:
		for _, v := range n.Values {
			e(v)
		}
	case *ParenExpr:
		e(n.X)
	case *TryExpr:
		e(n.X)
	case *CatchExpr:
		e(n.X)
		if n.Handler != nil {
			s(n.Handler)
		}
		e(n.Fallback)
	case *Block:
		for _, x := range n.Stmts {
			s(x)
		}
	case *LetStmt:
		e(n.Value)
	case *AssignStmt:
		e(n.Target)
		e(n.Value)
	case *ExprStmt:
		e(n.X)
	case *IfStmt:
		e(n.Cond)
		s(n.Then)
		if n.Else != nil {
			s(n.Else)
		}
	case *WhileStmt:
		e(n.Cond)
		s(n.Body)
	case *ForStmt:
		e(n.Lo)
		e(n.Hi)
		s(n.Body)
	case *ReturnStmt:
		e(n.Value)
	case *DeferStmt:
		s(n.Body)
	case *SwitchStmt:
		e(n.X)
		for _, c := range n.Cases {
			for _, v := range c.Values {
				e(v)
			}
			s(c.Body)
		}
		if n.Else != nil {
			s(n.Else)
		}
	}
}
