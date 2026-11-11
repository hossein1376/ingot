package syntax

import (
	"testing"

	"github.com/hossein1376/ingot/internal/diag"
)

func texts(toks []Tok) []string {
	var out []string
	for _, t := range toks {
		if t.Kind != EOF {
			out = append(out, t.Text)
		}
	}
	return out
}

func TestLexLongestMatch(t *testing.T) {
	var dl diag.List
	got := texts(Lex("t.ig", []byte("a <<= b.* ..= T.{ x => @splat // c\n"), &dl))
	want := []string{"a", "<<=", "b", ".*", "..", "=", "T", ".{", "x", "=>", "@splat"}
	if len(got) != len(want) {
		t.Fatalf("got %q, want %q", got, want)
	}
	for i := range want {
		if got[i] != want[i] {
			t.Fatalf("token %d: got %q, want %q (all: %q)", i, got[i], want[i], got)
		}
	}
	if dl.HasErrors() {
		t.Fatalf("unexpected errors: %+v", dl.Items[0])
	}
}

func TestLexKinds(t *testing.T) {
	var dl diag.List
	toks := Lex("t.ig", []byte(`defer c"hi\n" "s" 0x1F 1.5 @fma`), &dl)
	kinds := []Kind{Keyword, CString, String, Int, Float, Builtin, EOF}
	if len(toks) != len(kinds) {
		t.Fatalf("got %d tokens: %v", len(toks), toks)
	}
	for i, k := range kinds {
		if toks[i].Kind != k {
			t.Errorf("token %d (%v): kind %d, want %d", i, toks[i], toks[i].Kind, k)
		}
	}
	if toks[1].Text != "hi\n" {
		t.Errorf("c-string not decoded: %q", toks[1].Text)
	}
}

func TestParseIntLit(t *testing.T) {
	for s, want := range map[string]uint64{"0": 0, "42": 42, "0x1F": 31, "0b101": 5, "1_000": 1000} {
		got, err := ParseIntLit(s)
		if err != nil || got != want {
			t.Errorf("ParseIntLit(%q) = %d, %v; want %d", s, got, err, want)
		}
	}
}

func parseOne(t *testing.T, src string) (*File, *diag.List) {
	t.Helper()
	dl := &diag.List{}
	f := Parse("t.ig", []byte(src), dl)
	return f, dl
}

func codes(dl *diag.List) []string {
	var out []string
	for _, d := range dl.Items {
		out = append(out, d.Code)
	}
	return out
}

func TestAddrOfBindsTighterThanAs(t *testing.T) {
	f, dl := parseOne(t, "fn f() { let p: *u8 = &x as *u8; }")
	if dl.HasErrors() {
		t.Fatalf("errors: %v", codes(dl))
	}
	let := f.Fns[0].Body.Stmts[0].(*LetStmt)
	cast, ok := let.Value.(*CastExpr)
	if !ok {
		t.Fatalf("top of initializer is %T, want *CastExpr", let.Value)
	}
	if _, ok := cast.X.(*UnaryExpr); !ok {
		t.Fatalf("cast operand is %T, want the & expression", cast.X)
	}
}

func TestParseRejectsAmbiguity(t *testing.T) {
	cases := map[string]string{
		"fn f() -> bool { return a & b == 0; }":  "E0106",
		"fn f() -> bool { return a < b < c; }":   "",
		"fn f() -> i64 { return 1 << 2 + 3; }":   "E0106",
		"fn f() -> i64 { return (1 << 2) + 3; }": "-",
		"fn f() { defer { g(); } }":              "-",
	}
	for src, want := range cases {
		_, dl := parseOne(t, src)
		got := codes(dl)
		switch want {
		case "-":
			if len(got) != 0 {
				t.Errorf("%s: unexpected %v", src, got)
			}
		case "":
			if len(got) == 0 {
				t.Errorf("%s: expected an error", src)
			}
		default:
			if len(got) == 0 || got[0] != want {
				t.Errorf("%s: got %v, want %s", src, got, want)
			}
		}
	}
}
