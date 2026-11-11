// Package driver wires the compiler phases together and invokes the
// platform assembler/linker.
package driver

import (
	"bytes"
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"runtime"
	"strings"

	"github.com/hossein1376/ingot/internal/codegen"
	"github.com/hossein1376/ingot/internal/diag"
	"github.com/hossein1376/ingot/internal/sema"
)

// Config holds compiler settings.
type Config struct {
	Target codegen.Target
	Safe   bool
	Tests  bool
}

// DefaultTarget is the host-appropriate default.
func DefaultTarget() codegen.Target {
	if runtime.GOOS == "darwin" {
		return codegen.MacOS
	}
	return codegen.Linux
}

// Compile turns one source file into assembly text. Diagnostics go to dl.
func Compile(path string, src []byte, cfg Config, dl *diag.List) (string, bool) {
	f := Load(path, src, dl)
	if dl.HasErrors() {
		return "", false
	}
	info := sema.Check(f, dl)
	AnnotateInstances(f, dl)
	if dl.HasErrors() {
		return "", false
	}
	if !cfg.Tests && !hasMain(info) {
		dl.Errorf("E0004", diag.Span{File: path, Start: diag.Pos{Line: 1, Col: 1}, End: diag.Pos{Line: 1, Col: 1}},
			"program has no entry point; declare 'pub fn main() -> i32' (or run 'ingot test' for test blocks)")
		return "", false
	}
	asm, ok := codegen.Generate(cfg.Target, info, codegen.Options{Safe: cfg.Safe, Tests: cfg.Tests, Src: src}, dl)
	return asm, ok && !dl.HasErrors()
}

func hasMain(info *sema.Info) bool {
	for _, b := range info.Bodies {
		if b.Name == "main" {
			return true
		}
	}
	return false
}

// CC returns the C toolchain command used to assemble and link.
// INGOT_CC overrides it (e.g. "zig cc" or "clang").
func CC() []string {
	if s := os.Getenv("INGOT_CC"); s != "" {
		return strings.Fields(s)
	}
	if runtime.GOOS == "darwin" {
		return []string{"clang"}
	}
	if p, err := exec.LookPath("zig"); err == nil {
		return []string{p, "cc"}
	}
	return []string{"clang"}
}

// LinkArgs returns toolchain flags for the target.
func LinkArgs(cc []string, t codegen.Target) []string {
	zig := strings.Contains(strings.Join(cc, " "), "zig")
	switch {
	case t.Darwin && runtime.GOOS == "darwin" && !zig:
		return []string{"-arch", "arm64"}
	case t.Darwin && zig:
		return []string{"-target", "aarch64-macos"}
	case t.Darwin:
		return []string{"--target=arm64-apple-macos13"}
	case zig:
		return []string{"-target", "aarch64-linux-musl", "-static"}
	}
	return []string{"--target=aarch64-linux-gnu", "-static"}
}

// Link assembles asmPath, plus any extra assembly (.s/.S), C or object
// files, into an executable at out.
func Link(asmPath, out string, t codegen.Target, extra ...string) error {
	cc := CC()
	args := append(append([]string{}, cc[1:]...), LinkArgs(cc, t)...)
	args = append(args, "-o", out, asmPath)
	args = append(args, extra...)
	cmd := exec.Command(cc[0], args...)
	var stderr bytes.Buffer
	cmd.Stderr = &stderr
	if err := cmd.Run(); err != nil {
		return fmt.Errorf("assembling/linking with %s failed: %v\n%s", strings.Join(cc, " "), err, stderr.String())
	}
	return nil
}

// Runner returns the command prefix needed to execute a binary for t on this host.
func Runner(t codegen.Target) ([]string, error) {
	if t.Darwin {
		if runtime.GOOS == "darwin" && runtime.GOARCH == "arm64" {
			return nil, nil
		}
		return nil, fmt.Errorf("cannot run macos-arm64 binaries on %s/%s; use --target linux-arm64 to test under emulation", runtime.GOOS, runtime.GOARCH)
	}
	if runtime.GOOS == "linux" && runtime.GOARCH == "arm64" {
		return nil, nil
	}
	if s := os.Getenv("INGOT_RUNNER"); s != "" {
		return strings.Fields(s), nil
	}
	for _, q := range []string{"qemu-aarch64", "qemu-aarch64-static"} {
		if p, err := exec.LookPath(q); err == nil {
			return []string{p}, nil
		}
	}
	return nil, fmt.Errorf("no way to run linux-arm64 binaries here; install qemu-user or set INGOT_RUNNER")
}

// BuildDir returns a scratch directory for intermediate files.
func BuildDir() string {
	d := filepath.Join(os.TempDir(), "ingot-build")
	_ = os.MkdirAll(d, 0o755)
	return d
}
