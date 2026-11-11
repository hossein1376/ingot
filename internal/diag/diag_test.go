package diag

import (
	"bytes"
	"io/fs"
	"os"
	"path/filepath"
	"regexp"
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

func TestEveryEmittedCodeIsDocumented(t *testing.T) {
	re := regexp.MustCompile(`"(E\d{4})"`)
	err := filepath.WalkDir("../..", func(p string, d fs.DirEntry, err error) error {
		if err != nil || d.IsDir() || !strings.HasSuffix(p, ".go") || strings.HasSuffix(p, "_test.go") || strings.HasSuffix(p, "codes.go") {
			return err
		}
		src, err := os.ReadFile(p)
		if err != nil {
			return err
		}
		for _, m := range re.FindAllStringSubmatch(string(src), -1) {
			if _, ok := LookupCode(m[1]); !ok {
				t.Errorf("%s uses %s, which has no entry in diag.Codes", p, m[1])
			}
		}
		return nil
	})
	if err != nil {
		t.Fatal(err)
	}
}
