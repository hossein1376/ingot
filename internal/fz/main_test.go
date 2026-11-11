// Package fz_test mutates the examples and e2e sources (byte edits, token
// insertions, type and operator swaps) and requires that the compiler never
// panics and never fails without a diagnostic. INGOT_FZ_ITERS raises the
// iteration count; INGOT_FZ_DUMP=dir saves a sample of compiled mutants so the
// assembly can be checked with an assembler.
package fz_test

import (
	"math/rand"
	"os"
	"path/filepath"
	"strconv"
	"strings"
	"testing"

	"github.com/hossein1376/ingot/internal/codegen"
	"github.com/hossein1376/ingot/internal/diag"
	"github.com/hossein1376/ingot/internal/driver"
)

func TestMutate(t *testing.T) {
	files, _ := filepath.Glob("/workspace/ingot/examples/*.ig")
	more, _ := filepath.Glob("/workspace/ingot/tests/e2e/*.ig")
	files = append(files, more...)
	rng := rand.New(rand.NewSource(1))
	crashes, compiled := 0, 0
	defer func() { t.Logf("%d mutants compiled to assembly", compiled) }()
	iters := 4000
	if n, err := strconv.Atoi(os.Getenv("INGOT_FZ_ITERS")); err == nil {
		iters = n
	}
	for iter := 0; iter < iters && crashes < 15; iter++ {
		f := files[rng.Intn(len(files))]
		src, _ := os.ReadFile(f)
		s := string(src)
		for k := 0; k < 1+rng.Intn(3); k++ {
			if len(s) < 10 {
				break
			}
			i := rng.Intn(len(s))
			j := i + rng.Intn(12)
			if j > len(s) {
				j = len(s)
			}
			switch rng.Intn(6) {
			case 4, 5:
				// type and operator swaps keep the program parseable and stress the checker and backend
				subs := [][2]string{{"i64", "f64"}, {"i64", "u8"}, {"f32", "i32"}, {"i64", "*i64"}, {"f32x4", "i32x4"}, {"+", "*"}, {"==", "<"}, {"let", "var"}, {"u64", "i32"}, {"[", "[0+"}, {"*f32", "f32"}, {"return", "return -"}, {"(", "(&"}, {" as ", " as *"}, {"i32", "f64"}, {"while", "if"}}
				sb := subs[rng.Intn(len(subs))]
				idx := strings.Index(s[i:], sb[0])
				if idx >= 0 {
					s = s[:i+idx] + sb[1] + s[i+idx+len(sb[0]):]
				}
			case 0:
				s = s[:i] + s[j:]
			case 1:
				toks := []string{"(", ")", "{", "}", ";", "as i64", "try ", "@select(true, ", "@zero", ".*", "&", "*", "[", "]", "0", "1.5", "defer {", "catch e => 0", "inline ", "x", "u8", "!", "->", "f32x4", ","}
				s = s[:i] + toks[rng.Intn(len(toks))] + s[i:]
			case 2:
				s = s[:i] + s[j:] + s[i:j]
			case 3:
				lines := strings.Split(s, "\n")
				a := rng.Intn(len(lines))
				lines = append(lines[:a], lines[a+1:]...)
				s = strings.Join(lines, "\n")
			}
		}
		func() {
			defer func() {
				if r := recover(); r != nil {
					crashes++
					p := filepath.Join(os.TempDir(), "fzcrash", filepath.Base(f)+"."+strconv.Itoa(iter)+".ig")
					os.MkdirAll(filepath.Dir(p), 0o755)
					os.WriteFile(p, []byte(s), 0o644)
					t.Errorf("panic on %s: %v", p, r)
				}
			}()
			dl := &diag.List{}
			if asm, ok := driver.Compile(f, []byte(s), driver.Config{Target: [2]codegen.Target{codegen.Linux, codegen.MacOS}[iter%2], Safe: iter%3 != 0, Tests: true}, dl); ok {
				compiled++
				if dir := os.Getenv("INGOT_FZ_DUMP"); dir != "" && compiled%40 == 0 {
					os.MkdirAll(dir, 0o755)
					os.WriteFile(filepath.Join(dir, strconv.Itoa(iter)+".s"), []byte(asm), 0o644)
					os.WriteFile(filepath.Join(dir, strconv.Itoa(iter)+".ig"), []byte(s), 0o644)
				}
			} else if len(dl.Items) == 0 {
				t.Errorf("compile failed with no diagnostic: %s", f)
			}
		}()
	}
}
