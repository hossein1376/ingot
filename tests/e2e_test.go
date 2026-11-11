package tests

import (
	"bytes"
	"encoding/binary"
	"os"
	"os/exec"
	"path/filepath"
	"strconv"
	"strings"
	"testing"

	"github.com/hossein1376/ingot/internal/codegen"
	"github.com/hossein1376/ingot/internal/diag"
	"github.com/hossein1376/ingot/internal/driver"
)

func sources(t *testing.T) []string {
	ex, _ := filepath.Glob("../examples/*.ig")
	e2e, _ := filepath.Glob("e2e/*.ig")
	return append(ex, e2e...)
}

func compile(t *testing.T, path string, cfg driver.Config) string {
	t.Helper()
	src, err := os.ReadFile(path)
	if err != nil {
		t.Fatal(err)
	}
	var dl diag.List
	asm, ok := driver.Compile(path, src, cfg, &dl)
	if !ok {
		var b bytes.Buffer
		dl.WriteText(&b)
		t.Fatalf("compile failed:\n%s", b.String())
	}
	return asm
}

func haveToolchain(t *testing.T) {
	t.Helper()
	cc := driver.CC()
	if _, err := exec.LookPath(cc[0]); err != nil {
		t.Skipf("no assembler/linker (%s); set INGOT_CC", cc[0])
	}
}

func buildBin(t *testing.T, path string, cfg driver.Config) string {
	t.Helper()
	asm := compile(t, path, cfg)
	dir := t.TempDir()
	asmPath := filepath.Join(dir, "out.s")
	if err := os.WriteFile(asmPath, []byte(asm), 0o644); err != nil {
		t.Fatal(err)
	}
	bin := filepath.Join(dir, "out")
	if err := driver.Link(asmPath, bin, cfg.Target); err != nil {
		t.Fatal(err)
	}
	return bin
}

func runBin(t *testing.T, bin string) (string, string, int) {
	t.Helper()
	runner, err := driver.Runner(codegen.Linux)
	if err != nil {
		t.Skip(err)
	}
	argv := append(runner, bin)
	cmd := exec.Command(argv[0], argv[1:]...)
	var out, errb bytes.Buffer
	cmd.Stdout, cmd.Stderr = &out, &errb
	err = cmd.Run()
	code := 0
	if ee, ok := err.(*exec.ExitError); ok {
		code = ee.ExitCode()
	} else if err != nil {
		t.Fatal(err)
	}
	return out.String(), errb.String(), code
}

// TestGolden runs each example that has tests/golden/<name>.out and compares
// stdout (and the exit code from <name>.code, default 0).
func TestGolden(t *testing.T) {
	haveToolchain(t)
	for _, src := range sources(t) {
		name := strings.TrimSuffix(filepath.Base(src), ".ig")
		want, err := os.ReadFile(filepath.Join("golden", name+".out"))
		if err != nil {
			continue
		}
		t.Run(name, func(t *testing.T) {
			bin := buildBin(t, src, driver.Config{Target: codegen.Linux, Safe: true})
			got, stderr, code := runBin(t, bin)
			wantCode := 0
			if b, err := os.ReadFile(filepath.Join("golden", name+".code")); err == nil {
				wantCode, _ = strconv.Atoi(strings.TrimSpace(string(b)))
			}
			if *update {
				_ = os.WriteFile(filepath.Join("golden", name+".out"), []byte(got), 0o644)
				return
			}
			if got != string(want) || code != wantCode {
				t.Errorf("exit %d (want %d)\n--- got\n%s--- want\n%s--- stderr\n%s", code, wantCode, got, want, stderr)
			}
		})
	}
}

// TestInLanguageTests runs `test` blocks of every source that has them.
func TestInLanguageTests(t *testing.T) {
	haveToolchain(t)
	for _, src := range sources(t) {
		b, _ := os.ReadFile(src)
		if !bytes.Contains(b, []byte("\ntest \"")) {
			continue
		}
		t.Run(filepath.Base(src), func(t *testing.T) {
			bin := buildBin(t, src, driver.Config{Target: codegen.Linux, Safe: true, Tests: true})
			out, stderr, code := runBin(t, bin)
			if code != 0 || !strings.Contains(out, "tests passed") {
				t.Errorf("exit %d\n%s\n%s", code, out, stderr)
			}
		})
	}
}

// TestMachO links every example for macos-arm64 and checks the Mach-O
// header (MH_MAGIC_64, CPU_TYPE_ARM64, MH_EXECUTE). Running them needs a Mac.
func TestMachO(t *testing.T) {
	haveToolchain(t)
	if !strings.Contains(strings.Join(driver.CC(), " "), "zig") && os.Getenv("GOOS") != "darwin" {
		if _, err := os.Stat("/usr/lib/libSystem.B.dylib"); err != nil {
			t.Skip("Mach-O linking needs zig cc or a macOS SDK")
		}
	}
	for _, src := range sources(t) {
		t.Run(filepath.Base(src), func(t *testing.T) {
			tests := !bytes.Contains(must(os.ReadFile(src)), []byte("pub fn main"))
			bin := buildBin(t, src, driver.Config{Target: codegen.MacOS, Safe: true, Tests: tests})
			data := must(os.ReadFile(bin))
			le := binary.LittleEndian
			magic, cpu, ftype, ncmds := le.Uint32(data[0:]), le.Uint32(data[4:]), le.Uint32(data[12:]), le.Uint32(data[16:])
			if magic != 0xfeedfacf || cpu != 0x0100000c || ftype != 2 {
				t.Fatalf("bad Mach-O header: magic=%#x cpu=%#x filetype=%d", magic, cpu, ftype)
			}
			seen := map[uint32]bool{}
			off := uint32(32)
			for i := uint32(0); i < ncmds; i++ {
				seen[le.Uint32(data[off:])] = true
				off += le.Uint32(data[off+4:])
			}
			const lcMain, lcCodeSig, lcBuildVersion = 0x80000028, 0x1d, 0x32
			for _, lc := range []uint32{lcMain, lcCodeSig, lcBuildVersion} {
				if !seen[lc] {
					t.Errorf("missing load command %#x", lc)
				}
			}
		})
	}
}

// TestDeterministic checks that compiling twice yields byte-identical output.
func TestDeterministic(t *testing.T) {
	for _, src := range sources(t) {
		for _, tgt := range []codegen.Target{codegen.Linux, codegen.MacOS} {
			cfg := driver.Config{Target: tgt, Safe: true, Tests: true}
			a, b := compile(t, src, cfg), compile(t, src, cfg)
			if a != b {
				t.Errorf("%s/%s: nondeterministic output", src, tgt.Name)
			}
		}
	}
}

func must[T any](v T, err error) T {
	if err != nil {
		panic(err)
	}
	return v
}
