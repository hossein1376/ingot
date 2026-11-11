package diag

import (
	"bytes"
	"strings"
	"testing"
)

func sp(l1, c1, l2, c2 int) Span {
	return Span{File: "f.ig", Start: Pos{l1, c1}, End: Pos{l2, c2}}
}

func TestApplyFixesSkipsOverlaps(t *testing.T) {
	src := []byte("let x: i64 = 1;\nx = 2;\n")
	var l List
	l.Errorf("E0301", sp(2, 1, 2, 2), "a").Fix("var", sp(1, 1, 1, 4), "var")
	l.Errorf("E9999", sp(1, 2, 1, 3), "b").Fix("overlapping", sp(1, 2, 1, 6), "zzz")
	l.Errorf("E9998", sp(2, 7, 2, 7), "c").Fix("insert", sp(2, 7, 2, 7), " // ok")
	out, n := ApplyFixes(src, "f.ig", l.Items)
	if n != 2 {
		t.Fatalf("applied %d fixes, want 2", n)
	}
	if want := "var x: i64 = 1;\nx = 2; // ok\n"; string(out) != want {
		t.Fatalf("got %q, want %q", out, want)
	}
}

func TestJSONIsOneObjectPerLineAndUnescaped(t *testing.T) {
	var l List
	l.Errorf("E0106", sp(1, 1, 1, 2), "mixing & and == needs parentheses <here>")
	var b bytes.Buffer
	l.WriteJSON(&b)
	s := strings.TrimSpace(b.String())
	if strings.Count(s, "\n") != 0 {
		t.Fatalf("expected one line, got %q", s)
	}
	if !strings.Contains(s, "& and == needs parentheses <here>") {
		t.Fatalf("HTML escaping leaked into JSON: %s", s)
	}
}
