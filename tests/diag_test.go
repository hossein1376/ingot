package tests

import (
	"bytes"
	"flag"
	"os"
	"path/filepath"
	"strings"
	"testing"

	"github.com/hossein1376/ingot/internal/codegen"
	"github.com/hossein1376/ingot/internal/diag"
	"github.com/hossein1376/ingot/internal/driver"
)

var update = flag.Bool("update", false, "rewrite golden files")

// TestDiagnostics compiles every tests/diag/*.ig and compares the JSON
// diagnostics with the .json golden file next to it.
func TestDiagnostics(t *testing.T) {
	files, _ := filepath.Glob("diag/*.ig")
	if len(files) == 0 {
		t.Fatal("no diagnostic cases")
	}
	for _, f := range files {
		t.Run(filepath.Base(f), func(t *testing.T) {
			src, err := os.ReadFile(f)
			if err != nil {
				t.Fatal(err)
			}
			var dl diag.List
			// tests mode so files without main still type-check
			driver.Compile(f, src, driver.Config{Target: codegen.Linux, Safe: true, Tests: true}, &dl)
			if !dl.HasErrors() {
				t.Fatalf("expected at least one error")
			}
			var got bytes.Buffer
			dl.WriteJSON(&got)
			golden := strings.TrimSuffix(f, ".ig") + ".json"
			if *update {
				if err := os.WriteFile(golden, got.Bytes(), 0o644); err != nil {
					t.Fatal(err)
				}
				return
			}
			want, err := os.ReadFile(golden)
			if err != nil {
				t.Fatalf("missing golden (run go test ./tests -update): %v", err)
			}
			if !bytes.Equal(got.Bytes(), want) {
				t.Errorf("diagnostics differ\n--- got\n%s--- want\n%s", got.String(), want)
			}
		})
	}
}
