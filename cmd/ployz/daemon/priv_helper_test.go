package daemon

import (
	"context"
	"errors"
	"io"
	"os"
	"path/filepath"
	"strings"
	"testing"

	"ployz/infra/wireguard"
)

func TestPrivHelperCmdReadsTokenFileAndRunsHelper(t *testing.T) {
	tokenPath := filepath.Join(t.TempDir(), "helper.token")
	if err := os.WriteFile(tokenPath, []byte("secret-token\n"), 0o600); err != nil {
		t.Fatalf("write token file: %v", err)
	}

	originalRun := runPrivilegedHelper
	originalEUID := helperGetEUID
	originalReadFile := helperReadFile
	t.Cleanup(func() {
		runPrivilegedHelper = originalRun
		helperGetEUID = originalEUID
		helperReadFile = originalReadFile
	})

	helperGetEUID = func() int { return 0 }
	helperReadFile = os.ReadFile

	called := false
	runPrivilegedHelper = func(_ context.Context, cfg wireguard.HelperConfig) error {
		called = true
		if cfg.SocketPath != "/tmp/ployz-test-priv.sock" {
			return errors.New("unexpected socket path")
		}
		if cfg.Token != "secret-token" {
			return errors.New("unexpected token")
		}
		if cfg.TUNSocketPath != "/tmp/ployz-test-tun.sock" {
			return errors.New("unexpected tun socket path")
		}
		if cfg.MTU != 1420 {
			return errors.New("unexpected mtu")
		}
		return nil
	}

	cmd := privHelperCmd()
	cmd.SetOut(io.Discard)
	cmd.SetErr(io.Discard)
	cmd.SetArgs([]string{
		"--socket", "/tmp/ployz-test-priv.sock",
		"--token-file", tokenPath,
		"--tun-socket", "/tmp/ployz-test-tun.sock",
		"--mtu", "1420",
	})

	if err := cmd.Execute(); err != nil {
		t.Fatalf("execute helper command: %v", err)
	}
	if !called {
		t.Fatalf("expected helper to be invoked")
	}
}

func TestPrivHelperCmdRequiresRoot(t *testing.T) {
	originalRun := runPrivilegedHelper
	originalEUID := helperGetEUID
	t.Cleanup(func() {
		runPrivilegedHelper = originalRun
		helperGetEUID = originalEUID
	})

	helperGetEUID = func() int { return 501 }

	invoked := false
	runPrivilegedHelper = func(_ context.Context, _ wireguard.HelperConfig) error {
		invoked = true
		return nil
	}

	cmd := privHelperCmd()
	cmd.SetOut(io.Discard)
	cmd.SetErr(io.Discard)
	cmd.SetArgs([]string{"--token", "secret-token"})

	err := cmd.Execute()
	if err == nil {
		t.Fatalf("expected root error")
	}
	if !strings.Contains(err.Error(), "helper requires root") {
		t.Fatalf("expected root error, got %v", err)
	}
	if invoked {
		t.Fatalf("helper should not run when not root")
	}
}

func TestResolvePrivilegedTokenPrefersExplicitToken(t *testing.T) {
	originalReadFile := helperReadFile
	t.Cleanup(func() {
		helperReadFile = originalReadFile
	})

	helperReadFile = func(string) ([]byte, error) {
		return nil, errors.New("read file should not be called")
	}

	token, err := resolvePrivilegedToken("  direct-token  ", "/ignored")
	if err != nil {
		t.Fatalf("resolve token: %v", err)
	}
	if token != "direct-token" {
		t.Fatalf("expected trimmed token, got %q", token)
	}
}
