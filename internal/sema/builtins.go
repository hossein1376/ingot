package sema

import (
	"github.com/hossein1376/ingot/internal/diag"
	"github.com/hossein1376/ingot/internal/syntax"
)

// Builtins documents every '@' builtin: name -> signature (for docs and diagnostics).
var Builtins = map[string]string{
	"zero":         "@zero : T                       zero value of the expected type",
	"sizeof":       "@sizeof(T) -> i64               size of T in bytes",
	"assert":       "@assert(cond: bool [, msg: []u8])   aborts with file:line on failure (always on)",
	"panic":        "@panic(msg: []u8)               prints msg with location and exits(101)",
	"trap":         "@trap()                          brk #1",
	"unreachable":  "@unreachable()                   traps if reached",
	"splat":        "@splat(V, x: E) -> V            broadcast scalar to all lanes",
	"load":         "@load(V, p: *E) -> V            unaligned 128-bit vector load",
	"store":        "@store(p: *E, v: V)             unaligned 128-bit vector store",
	"reduce_add":   "@reduce_add(v: V) -> E          horizontal sum",
	"reduce_min":   "@reduce_min(v: V) -> E          horizontal min",
	"reduce_max":   "@reduce_max(v: V) -> E          horizontal max",
	"sqrt":         "@sqrt(x: F) -> F                square root (scalar or float vector)",
	"fma":          "@fma(a, b, c) -> T              fused a*b + c (scalar or float vector)",
	"min":          "@min(a: T, b: T) -> T           minimum",
	"max":          "@max(a: T, b: T) -> T           maximum",
	"ticks":        "@ticks() -> i64                 CNTVCT_EL0 virtual counter",
	"tick_freq":    "@tick_freq() -> i64             CNTFRQ_EL0 counter frequency (Hz)",
	"atomic_load":  "@atomic_load(p: *T) -> T        acquire load (LDAR)",
	"atomic_store": "@atomic_store(p: *T, v: T)      release store (STLR)",
	"atomic_add":   "@atomic_add(p: *T, v: T) -> T   seq-cst fetch-add (LSE LDADDAL), returns old",
	"atomic_cas":   "@atomic_cas(p: *T, old: T, new: T) -> T   seq-cst CAS (LSE CASAL), returns previous",
	"fence":        "@fence()                         full barrier (DMB ISH)",
	"qos":          "@qos(class: i32) -> i32         macOS: pthread_set_qos_class_self_np (P/E core steering); Linux: no-op, returns 0",
	"prefetch":     "@prefetch(p: *T)                PRFM PLDL1KEEP",
}

func (c *checker) builtin(e *syntax.BuiltinCall, want *Type) *Type {
	if _, ok := Builtins[e.Name]; !ok {
		c.dl.Errorf("E0230", e.Sp, "unknown builtin @%s", e.Name).Note("see docs/spec.md, section Builtins")
		for _, a := range e.Args {
			c.expr(a, nil)
		}
		return nil
	}
	nargs := func(n ...int) bool {
		for _, k := range n {
			if len(e.Args) == k {
				return true
			}
		}
		c.dl.Errorf("E0215", e.Sp, "@%s: wrong number of arguments (%d); signature: %s", e.Name, len(e.Args), Builtins[e.Name])
		return false
	}
	var ty *Type
	if e.Type != nil {
		ty = c.resolve(e.Type)
	}
	ptrArg := func(i int) *Type {
		t := c.expr(e.Args[i], nil)
		if t.Kind != Ptr {
			c.dl.Errorf("E0231", e.Args[i].ESpan(), "@%s expects a pointer, found %s", e.Name, t)
			return TI64
		}
		return t.Elem
	}
	switch e.Name {
	case "zero":
		if want == nil {
			return TNull // caller reports missing annotation
		}
		return want
	case "sizeof":
		if ty == nil {
			c.dl.Errorf("E0215", e.Sp, "@sizeof requires a type argument")
			return TI64
		}
		c.info.Consts[e] = Const{I: ty.Size}
		return TI64
	case "assert":
		if nargs(1, 2) {
			c.cond(e.Args[0])
			if len(e.Args) == 2 {
				if _, ok := e.Args[1].(*syntax.StringLit); !ok {
					c.dl.Errorf("E0231", e.Args[1].ESpan(), "@assert message must be a string literal")
				}
				c.expr(e.Args[1], nil)
			}
		}
		return TVoid
	case "panic":
		if nargs(1) {
			if _, ok := e.Args[0].(*syntax.StringLit); !ok {
				c.dl.Errorf("E0231", e.Args[0].ESpan(), "@panic message must be a string literal")
			}
			c.expr(e.Args[0], nil)
		}
		return TVoid
	case "trap", "unreachable", "fence":
		nargs(0)
		return TVoid
	case "ticks", "tick_freq":
		nargs(0)
		return TI64
	case "splat", "load":
		if ty == nil || ty.Kind != Vec {
			c.dl.Errorf("E0231", e.Sp, "@%s requires a vector type as first argument (f32x4, f64x2, i32x4, i64x2, u8x16)", e.Name)
			return TF32x4
		}
		if nargs(1) {
			if e.Name == "splat" {
				c.assignable(c.expr(e.Args[0], ty.Elem), ty.Elem, e.Args[0])
			} else {
				el := ptrArg(0)
				if !Identical(el, ty.Elem) {
					c.dl.Errorf("E0205", e.Args[0].ESpan(), "@load(%s, ...) needs a *%s, found *%s", ty, ty.Elem, el)
				}
			}
		}
		return ty
	case "store":
		if nargs(2) {
			el := ptrArg(0)
			vt := c.expr(e.Args[1], nil)
			if vt.Kind != Vec || !Identical(vt.Elem, el) {
				c.dl.Errorf("E0205", e.Args[1].ESpan(), "@store to *%s needs a vector of %s, found %s", el, el, vt)
			}
		}
		return TVoid
	case "reduce_add", "reduce_min", "reduce_max":
		if !nargs(1) {
			return TI64
		}
		vt := c.expr(e.Args[0], nil)
		if vt.Kind != Vec {
			c.dl.Errorf("E0231", e.Args[0].ESpan(), "@%s expects a vector, found %s", e.Name, vt)
			return TI64
		}
		if vt == TF64x2 && e.Name != "reduce_add" || vt == TI64x2 && e.Name != "reduce_add" {
			c.dl.Errorf("E0231", e.Args[0].ESpan(), "@%s is not available for %s on AArch64 NEON", e.Name, vt)
		}
		return vt.Elem
	case "sqrt":
		if !nargs(1) {
			return TF64
		}
		t := c.expr(e.Args[0], want)
		if t.Kind != Float && !(t.Kind == Vec && t.Elem.Kind == Float) {
			c.dl.Errorf("E0231", e.Args[0].ESpan(), "@sqrt expects a float or float vector, found %s", t)
		}
		return t
	case "fma", "min", "max":
		n := 2
		if e.Name == "fma" {
			n = 3
		}
		if !nargs(n) {
			return TF64
		}
		t := c.expr(e.Args[0], want)
		if isUntyped(e.Args[0]) {
			t = c.expr(e.Args[1], want)
			c.expr(e.Args[0], t)
		}
		for _, a := range e.Args[1:] {
			c.assignable(c.expr(a, t), t, a)
		}
		ok := t.Kind == Float || (t.Kind == Vec && t.Elem.Kind == Float)
		if e.Name != "fma" {
			ok = ok || t.Kind == Int || (t.Kind == Vec && t.Elem.Size < 8)
		}
		if !ok {
			c.dl.Errorf("E0231", e.Sp, "@%s is not defined for %s", e.Name, t)
		}
		return t
	case "atomic_load":
		if !nargs(1) {
			return TI64
		}
		el := ptrArg(0)
		c.atomicType(el, e.Args[0].ESpan())
		return el
	case "atomic_store", "atomic_add":
		if !nargs(2) {
			return TI64
		}
		el := ptrArg(0)
		c.atomicType(el, e.Args[0].ESpan())
		c.assignable(c.expr(e.Args[1], el), el, e.Args[1])
		if e.Name == "atomic_store" {
			return TVoid
		}
		return el
	case "atomic_cas":
		if !nargs(3) {
			return TI64
		}
		el := ptrArg(0)
		c.atomicType(el, e.Args[0].ESpan())
		c.assignable(c.expr(e.Args[1], el), el, e.Args[1])
		c.assignable(c.expr(e.Args[2], el), el, e.Args[2])
		return el
	case "qos":
		if nargs(1) {
			c.assignable(c.expr(e.Args[0], TI32), TI32, e.Args[0])
		}
		return TI32
	case "prefetch":
		if nargs(1) {
			ptrArg(0)
		}
		return TVoid
	}
	return TVoid
}

func (c *checker) atomicType(t *Type, sp diag.Span) {
	if t.Kind != Int || (t.Size != 4 && t.Size != 8) {
		c.dl.Errorf("E0232", sp, "atomics require *i32, *u32, *i64 or *u64, found *%s", t)
	}
}
