package tests

import (
	"testing"

	"github.com/hossein1376/ingot/internal/codegen"
	"github.com/hossein1376/ingot/internal/diag"
	"github.com/hossein1376/ingot/internal/driver"
)

// TestFixesConverge applies the compiler's own fixes and expects a program
// that type-checks cleanly.
func TestFixesConverge(t *testing.T) {
	src := []byte(`fn f(x: i32) -> i64 {
    return x;
}
fn g(a: i64, b: i64) -> bool {
    let y: i64 = 1;
    y = 2;
    return a & b == 0;
}
fn h(n: int) -> i64 {
    if n { return 1; }
    return 0;
}
`)
	want := `fn f(x: i32) -> i64 {
    return (x) as i64;
}
fn g(a: i64, b: i64) -> bool {
    var y: i64 = 1;
    y = 2;
    return (a & b) == 0;
}
fn h(n: i64) -> i64 {
    if n != 0 { return 1; }
    return 0;
}
`
	cfg := driver.Config{Target: codegen.Linux, Safe: true, Tests: true}
	for round := 0; round < 8; round++ {
		var dl diag.List
		driver.Compile("f.ig", src, cfg, &dl)
		next, n := diag.ApplyFixes(src, "f.ig", dl.Items)
		if n == 0 {
			if dl.HasErrors() {
				t.Fatalf("errors remain after fixing:\n%s", src)
			}
			break
		}
		src = next
	}
	if string(src) != want {
		t.Errorf("got:\n%s\nwant:\n%s", src, want)
	}
}
