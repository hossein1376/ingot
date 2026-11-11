package format

import (
	"os"
	"path/filepath"
	"testing"
)

func TestReindent(t *testing.T) {
	in := "fn f(a: i64,\n  b: i64) -> i64 {\n\t\tif a > b {   // c {\nreturn a;   \n        } else {\n  return b;\n}\n}\n\n\n\nconst S: []u8 = \"x { y\";"
	want := "fn f(a: i64,\n    b: i64) -> i64 {\n    if a > b {   // c {\n        return a;\n    } else {\n        return b;\n    }\n}\n\nconst S: []u8 = \"x { y\";\n"
	got, err := Source("t.ig", []byte(in))
	if err != nil {
		t.Fatal(err)
	}
	if string(got) != want {
		t.Fatalf("got:\n%s\nwant:\n%s", got, want)
	}
	again, _ := Source("t.ig", got)
	if string(again) != string(got) {
		t.Fatal("formatting is not idempotent")
	}
}

// Every Ingot file in the repository is already in canonical form.
func TestRepositoryIsFormatted(t *testing.T) {
	var files []string
	for _, g := range []string{"../../examples/*.ig", "../../examples/sme/*.ig", "../../std/*.ig", "../../tests/e2e/*.ig", "../../bench/*.ig"} {
		m, _ := filepath.Glob(g)
		files = append(files, m...)
	}
	if len(files) < 10 {
		t.Fatalf("found only %d files", len(files))
	}
	for _, f := range files {
		src, err := os.ReadFile(f)
		if err != nil {
			t.Fatal(err)
		}
		got, err := Source(f, src)
		if err != nil {
			t.Fatal(err)
		}
		if string(got) != string(src) {
			t.Errorf("%s is not formatted (run ingot fmt)", f)
		}
	}
}
