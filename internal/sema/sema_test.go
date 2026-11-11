package sema

import (
	"testing"

	"github.com/hossein1376/ingot/internal/diag"
	"github.com/hossein1376/ingot/internal/syntax"
)

func check(t *testing.T, src string) (*Info, []string) {
	t.Helper()
	dl := &diag.List{}
	f := syntax.Parse("t.ig", []byte(src), dl)
	if dl.HasErrors() {
		t.Fatalf("parse errors in %q: %+v", src, dl.Items[0])
	}
	info := Check(f, dl)
	var cs []string
	for _, d := range dl.Items {
		cs = append(cs, d.Code)
	}
	return info, cs
}

// Every rule that makes Ingot "no implicit behavior" has a stable code.
func TestRules(t *testing.T) {
	cases := []struct{ name, src, code string }{
		{"ok", "fn f(a: i64) -> i64 { return a + 1; }", ""},
		{"implicit widening", "fn f(a: i32) -> i64 { return a; }", "E0205"},
		{"explicit widening", "fn f(a: i32) -> i64 { return a as i64; }", ""},
		{"mixed operands", "fn f(a: i32, b: i64) -> i64 { return (a as i64) + b; }", ""},
		{"redeclaration", "fn f(a: i64) { let a: i64 = 1; }", "E0200"},
		{"shadowing", "fn f() { let b: i64 = 1; if true { let b: i64 = 2; } }", "E0211"},
		{"truthiness", "fn f(a: i64) { if a { } }", "E0307"},
		{"assign to let", "fn f() { let x: i64 = 1; x = 2; }", "E0301"},
		{"no-effect statement", "fn f(a: i64) { a + 1; }", "E0303"},
		{"missing return value", "fn f() -> i64 { return; }", "E0305"},
		{"break outside loop", "fn f() { break; }", "E0306"},
		{"return in defer", "fn f() { defer { return; } }", "E0308"},
		{"break out of defer", "fn f() { while true { defer { break; } } }", "E0308"},
		{"loop inside defer", "fn f() { defer { while true { break; } } }", ""},
		{"nested defer", "fn f() { defer { defer { } } }", "E0308"},
		{"unhandled error", "enum E: u8 { a = 1 }\nfn g() -> i64 ! E { return 1; }\nfn f() -> i64 { return g(); }", "E0250"},
		{"try in infallible fn", "enum E: u8 { a = 1 }\nfn g() -> i64 ! E { return 1; }\nfn f() -> i64 { return try g(); }", "E0251"},
		{"catch must leave", "enum E: u8 { a = 1 }\nfn g() -> i64 ! E { return 1; }\nfn f() -> i64 { let v: i64 = g() catch e => { }; return v; }", "E0252"},
		{"void catch may fall through", "enum E: u8 { a = 1 }\nfn g() -> void ! E { return; }\nfn f() { g() catch e => { }; }", ""},
		{"err outside return", "enum E: u8 { a = 1 }\nfn f() { @err(E.a); }", "E0253"},
		{"zero error member", "enum E: u8 { a = 0 }\nfn g() -> i64 ! E { return 1; }", "E0242"},
		{"try on infallible call", "enum E: u8 { a = 1 }\nfn h() -> i64 { return 1; }\nfn g() -> i64 ! E { return try h(); }", "E0250"},
		{"inline ok", "inline fn sq(x: i64) -> i64 { return x * x; }\nfn f() -> i64 { return sq(sq(2)); }", ""},
		{"inline recursion", "inline fn a(x: i64) -> i64 { return b(x); }\ninline fn b(x: i64) -> i64 { return a(x); }", "E0245"},
		{"inline aggregate", "struct P { x: i64 }\ninline fn f(p: P) -> i64 { return p.x; }", "E0244"},
		{"undefined name", "fn f() -> i64 { return y; }", "E0212"},
		{"literal overflow", "fn f() -> u8 { return 300; }", "E0206"},
	}
	for _, c := range cases {
		_, got := check(t, c.src)
		if c.code == "" {
			if len(got) != 0 {
				t.Errorf("%s: unexpected diagnostics %v", c.name, got)
			}
			continue
		}
		if len(got) == 0 || got[0] != c.code {
			t.Errorf("%s: got %v, want %s", c.name, got, c.code)
		}
	}
}

// Struct layout follows C: natural alignment, padding, size rounded to align.
func TestStructLayoutIsC(t *testing.T) {
	info, cs := check(t, "struct S { a: u8, b: i64, c: u16, d: f32x4 }\nfn f(s: *S) -> u8 { return s.*.a; }")
	if len(cs) != 0 {
		t.Fatalf("diagnostics: %v", cs)
	}
	var st *Type
	for _, ty := range info.Types {
		if ty.Kind == Ptr && ty.Elem != nil && ty.Elem.Kind == Struct {
			st = ty.Elem
		}
		if ty.Kind == Struct {
			st = ty
		}
	}
	if st == nil {
		t.Fatal("struct type not found")
	}
	want := map[string]int64{"a": 0, "b": 8, "c": 16, "d": 32}
	for _, f := range st.Fields {
		if want[f.Name] != f.Offset {
			t.Errorf("field %s at %d, want %d", f.Name, f.Offset, want[f.Name])
		}
	}
	if st.Size != 48 || st.Align != 16 {
		t.Errorf("size/align = %d/%d, want 48/16", st.Size, st.Align)
	}
}

func TestConstantFoldingUnsigned(t *testing.T) {
	info, cs := check(t, "const K: u64 = 0xFFFFFFFFFFFFFFFF >> 60;\nfn f() -> u64 { return K; }")
	if len(cs) != 0 {
		t.Fatalf("diagnostics: %v", cs)
	}
	found := false
	for _, k := range info.Consts {
		if uint64(k.I) == 15 {
			found = true
		}
	}
	if !found {
		t.Error("0xFFFF_FFFF_FFFF_FFFF >> 60 did not fold to 15 as an unsigned shift")
	}
}
