//go:build darwin

package configure

import (
	"context"
	"crypto/rand"
	"encoding/base64"
	"fmt"
	"net"
	"os"
	"os/exec"
	"path/filepath"
	"strconv"
	"strings"
	"syscall"
	"time"

	"ployz/cmd/ployz/ui"
	"ployz/internal/adapter/wireguard"

	"golang.zx2c4.com/wireguard/tun"
)

func runConfigure(ctx context.Context, tunSocketPath, privSocketPath string, mtu int) error {
	if os.Geteuid() != 0 {
		return fmt.Errorf("configure requires root privileges; run `sudo ployz configure`")
	}
	if mtu <= 0 {
		return fmt.Errorf("invalid mtu %d", mtu)
	}
	tunSocketPath = strings.TrimSpace(tunSocketPath)
	if tunSocketPath == "" {
		return fmt.Errorf("invalid tun socket path")
	}
	privSocketPath = filepath.Clean(strings.TrimSpace(privSocketPath))
	if privSocketPath == "." {
		return fmt.Errorf("invalid privileged helper socket path")
	}

	secret, err := generateSocketSecret()
	if err != nil {
		return err
	}

	if err := startPrivilegedHelper(privSocketPath, secret); err != nil {
		return err
	}
	if err := waitForHelperSocket(ctx, privSocketPath, 2*time.Second); err != nil {
		return err
	}

	tunDev, err := tun.CreateTUN("utun", mtu)
	if err != nil {
		return fmt.Errorf("create macOS TUN: %w", err)
	}
	defer func() {
		_ = tunDev.Close()
	}()

	name, err := tunDev.Name()
	if err != nil {
		return fmt.Errorf("read TUN interface name: %w", err)
	}
	actualMTU := mtu
	if got, mtuErr := tunDev.MTU(); mtuErr == nil && got > 0 {
		actualMTU = got
	}

	if err := wireguard.SendTUN(tunSocketPath, tunDev.File(), name, actualMTU, privSocketPath, secret); err != nil {
		return fmt.Errorf("send TUN descriptor to daemon: %w", err)
	}

	fmt.Println(ui.SuccessMsg("sent %s to daemon", ui.Accent(name)))
	fmt.Print(ui.KeyValues("  ",
		ui.KV("tun socket", tunSocketPath),
		ui.KV("priv socket", privSocketPath),
		ui.KV("mtu", strconv.Itoa(actualMTU)),
	))

	return nil
}

func generateSocketSecret() (string, error) {
	buf := make([]byte, 32)
	if _, err := rand.Read(buf); err != nil {
		return "", fmt.Errorf("generate privileged helper secret: %w", err)
	}
	return base64.RawURLEncoding.EncodeToString(buf), nil
}

func startPrivilegedHelper(socketPath, secret string) error {
	if err := stopExistingPrivilegedHelper(socketPath); err != nil {
		return err
	}

	exe, err := os.Executable()
	if err != nil {
		return fmt.Errorf("resolve current executable: %w", err)
	}

	cmd := exec.Command(exe,
		"daemon", "priv-helper",
		"--socket", socketPath,
		"--token", secret,
	)
	cmd.Stdin = nil
	cmd.Stdout = nil
	cmd.Stderr = nil
	cmd.SysProcAttr = &syscall.SysProcAttr{Setpgid: true}
	if err := cmd.Start(); err != nil {
		return fmt.Errorf("start privileged helper: %w", err)
	}
	_ = cmd.Process.Release()
	return nil
}

func stopExistingPrivilegedHelper(socketPath string) error {
	pidData, err := os.ReadFile(socketPath + ".pid")
	if err != nil {
		if os.IsNotExist(err) {
			return nil
		}
		return fmt.Errorf("read privileged helper pid file: %w", err)
	}

	pid, err := strconv.Atoi(strings.TrimSpace(string(pidData)))
	if err != nil || pid <= 0 {
		return nil
	}
	if pid == os.Getpid() {
		return nil
	}

	if err := syscall.Kill(pid, syscall.SIGTERM); err != nil && err != syscall.ESRCH {
		return fmt.Errorf("stop existing privileged helper pid %d: %w", pid, err)
	}

	deadline := time.Now().Add(1 * time.Second)
	for {
		err := syscall.Kill(pid, 0)
		if err == syscall.ESRCH {
			return nil
		}
		if err != nil {
			return nil
		}
		if time.Now().After(deadline) {
			return nil
		}
		time.Sleep(100 * time.Millisecond)
	}
}

func waitForHelperSocket(ctx context.Context, socketPath string, timeout time.Duration) error {
	deadline := time.Now().Add(timeout)
	for {
		if _, err := os.Stat(socketPath); err == nil {
			conn, dialErr := net.DialTimeout("unix", socketPath, 200*time.Millisecond)
			if dialErr == nil {
				_ = conn.Close()
				return nil
			}
		}
		if time.Now().After(deadline) {
			return fmt.Errorf("privileged helper socket did not become ready at %s", socketPath)
		}
		select {
		case <-ctx.Done():
			return ctx.Err()
		case <-time.After(100 * time.Millisecond):
		}
	}
}
