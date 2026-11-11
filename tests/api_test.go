package tests

import (
	"testing"

	"github.com/hossein1376/ingot/internal/diag"
	"github.com/hossein1376/ingot/internal/driver"
)

// TestAPI checks that `ingot api` renders signatures and doc comments, and
// that every embedded std file parses.
func TestAPI(t *testing.T) {
	found := map[string]driver.APIEntry{}
	for _, f := range driver.StdFiles() {
		src, ok := driver.ReadSource(f)
		if !ok {
			t.Fatalf("cannot read %s", f)
		}
		var dl diag.List
		for _, e := range driver.API(f, src, &dl) {
			found[e.Name] = e
		}
		if dl.HasErrors() {
			t.Fatalf("%s: %v", f, dl.Items[0].Message)
		}
	}
	want := map[string]string{
		"vec_pop":    "fn vec_pop[T](v: *Vec.[T]) -> T ! VecErr",
		"Vec":        "struct Vec[T] { ptr: *T, len: i64, cap: i64 }",
		"write_file": "fn write_file(path: *u8, data: []u8) -> void ! IoErr",
		"printf":     "extern fn printf(fmt: *u8, ...) -> i32",
	}
	for name, sig := range want {
		if got := found[name].Signature; got != sig {
			t.Errorf("%s: got %q, want %q", name, got, sig)
		}
	}
	if found["vec_pop"].Doc != "Removes and returns the last element." {
		t.Errorf("vec_pop doc: %q", found["vec_pop"].Doc)
	}
}
