//go:build darwin

package process

import (
	"errors"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"
)

type fakeFileInfo struct {
	mode os.FileMode
}

func (f fakeFileInfo) Name() string       { return "corrosion" }
func (f fakeFileInfo) Size() int64        { return 1 }
func (f fakeFileInfo) Mode() os.FileMode  { return f.mode }
func (f fakeFileInfo) ModTime() time.Time { return time.Time{} }
func (f fakeFileInfo) IsDir() bool        { return f.mode.IsDir() }
func (f fakeFileInfo) Sys() any           { return nil }

func TestResolveCorrosionBinaryPrefersEnv(t *testing.T) {
	originalLookPath := corrosionLookPath
	originalStat := corrosionStat
	originalExecutable := corrosionExecutable
	originalGetenv := corrosionGetenv
	t.Cleanup(func() {
		corrosionLookPath = originalLookPath
		corrosionStat = originalStat
		corrosionExecutable = originalExecutable
		corrosionGetenv = originalGetenv
	})

	envPath := "/tmp/custom-corrosion"
	corrosionGetenv = func(key string) string {
		if key == corrosionBinaryEnv {
			return envPath
		}
		return ""
	}
	corrosionStat = func(path string) (os.FileInfo, error) {
		if path != envPath {
			return nil, os.ErrNotExist
		}
		return fakeFileInfo{mode: 0o755}, nil
	}
	corrosionLookPath = func(string) (string, error) {
		return "", errors.New("should not be called")
	}

	got, err := resolveCorrosionBinary()
	if err != nil {
		t.Fatalf("resolveCorrosionBinary() error = %v", err)
	}
	if got != envPath {
		t.Fatalf("resolveCorrosionBinary() path = %q, want %q", got, envPath)
	}
}

func TestResolveCorrosionBinaryUsesLookPath(t *testing.T) {
	originalLookPath := corrosionLookPath
	originalStat := corrosionStat
	originalExecutable := corrosionExecutable
	originalGetenv := corrosionGetenv
	t.Cleanup(func() {
		corrosionLookPath = originalLookPath
		corrosionStat = originalStat
		corrosionExecutable = originalExecutable
		corrosionGetenv = originalGetenv
	})

	corrosionGetenv = func(string) string { return "" }
	corrosionExecutable = func() (string, error) { return "", errors.New("unknown") }
	corrosionStat = func(string) (os.FileInfo, error) { return nil, os.ErrNotExist }
	corrosionLookPath = func(name string) (string, error) {
		if name != corrosionBinaryName {
			return "", errors.New("unexpected binary name")
		}
		return "/path/in/path/corrosion", nil
	}

	got, err := resolveCorrosionBinary()
	if err != nil {
		t.Fatalf("resolveCorrosionBinary() error = %v", err)
	}
	if got != "/path/in/path/corrosion" {
		t.Fatalf("resolveCorrosionBinary() path = %q, want %q", got, "/path/in/path/corrosion")
	}
}

func TestResolveCorrosionBinaryUsesSiblingFallback(t *testing.T) {
	originalLookPath := corrosionLookPath
	originalStat := corrosionStat
	originalExecutable := corrosionExecutable
	originalGetenv := corrosionGetenv
	t.Cleanup(func() {
		corrosionLookPath = originalLookPath
		corrosionStat = originalStat
		corrosionExecutable = originalExecutable
		corrosionGetenv = originalGetenv
	})

	corrosionGetenv = func(string) string { return "" }
	corrosionLookPath = func(string) (string, error) { return "", errors.New("not found") }
	corrosionExecutable = func() (string, error) {
		return "/usr/local/bin/ployz", nil
	}
	sibling := filepath.Join("/usr/local/bin", corrosionBinaryName)
	corrosionStat = func(path string) (os.FileInfo, error) {
		if path == sibling {
			return fakeFileInfo{mode: 0o755}, nil
		}
		return nil, os.ErrNotExist
	}

	got, err := resolveCorrosionBinary()
	if err != nil {
		t.Fatalf("resolveCorrosionBinary() error = %v", err)
	}
	if got != sibling {
		t.Fatalf("resolveCorrosionBinary() path = %q, want %q", got, sibling)
	}
}

func TestResolveCorrosionBinaryReturnsHelpfulError(t *testing.T) {
	originalLookPath := corrosionLookPath
	originalStat := corrosionStat
	originalExecutable := corrosionExecutable
	originalGetenv := corrosionGetenv
	t.Cleanup(func() {
		corrosionLookPath = originalLookPath
		corrosionStat = originalStat
		corrosionExecutable = originalExecutable
		corrosionGetenv = originalGetenv
	})

	corrosionGetenv = func(string) string { return "" }
	corrosionLookPath = func(string) (string, error) { return "", errors.New("not found") }
	corrosionExecutable = func() (string, error) { return "", errors.New("unknown") }
	corrosionStat = func(string) (os.FileInfo, error) { return nil, os.ErrNotExist }

	_, err := resolveCorrosionBinary()
	if err == nil {
		t.Fatal("resolveCorrosionBinary() error = nil, want not found error")
	}
	if !strings.Contains(err.Error(), "set PLOYZ_CORROSION_BIN") {
		t.Fatalf("resolveCorrosionBinary() error = %v, want hint about PLOYZ_CORROSION_BIN", err)
	}
}
