package agent

import (
	"context"
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"strings"

	"ployz/cmd/ployz/cmdutil"
)

const daemonUnit = "ployzd.service"

type linuxService struct{}

func NewPlatformService() PlatformService {
	return &linuxService{}
}

func (l *linuxService) Install(ctx context.Context, cfg InstallConfig) error {
	if err := os.MkdirAll(cfg.DataRoot, 0o755); err != nil {
		return fmt.Errorf("create data root: %w", err)
	}

	ployzBin, err := resolveBinary("ployz")
	if err != nil {
		return fmt.Errorf("resolve ployz binary: %w", err)
	}

	daemonContent := fmt.Sprintf(`[Unit]
Description=ployz daemon
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
ExecStart=%s daemon run --socket %s --data-root %s
Restart=always
RestartSec=5
StandardOutput=append:%s
StandardError=append:%s

[Install]
WantedBy=default.target
`, ployzBin, cfg.SocketPath, cfg.DataRoot,
		cmdutil.DaemonLogPath(cfg.DataRoot), cmdutil.DaemonLogPath(cfg.DataRoot))

	unitDir := "/etc/systemd/system"
	if err := os.WriteFile(filepath.Join(unitDir, daemonUnit), []byte(daemonContent), 0o644); err != nil {
		return fmt.Errorf("write daemon unit: %w", err)
	}

	if err := systemctl(ctx, "daemon-reload"); err != nil {
		return err
	}
	if err := systemctl(ctx, "enable", "--now", daemonUnit); err != nil {
		return fmt.Errorf("enable daemon: %w", err)
	}
	return nil
}

func (l *linuxService) Uninstall(ctx context.Context) error {
	_ = systemctl(ctx, "disable", "--now", daemonUnit)

	unitDir := "/etc/systemd/system"
	os.Remove(filepath.Join(unitDir, daemonUnit))

	_ = systemctl(ctx, "daemon-reload")
	return nil
}

func (l *linuxService) Status(ctx context.Context) (ServiceStatus, error) {
	return ServiceStatus{
		DaemonInstalled: systemctlEnabled(ctx, daemonUnit),
		DaemonRunning:   systemctlActive(ctx, daemonUnit),
		Platform:        "systemd",
	}, nil
}

// binary resolution

func resolveBinary(name string) (string, error) {
	if exePath, err := os.Executable(); err == nil {
		candidate := filepath.Join(filepath.Dir(exePath), name)
		if st, err := os.Stat(candidate); err == nil && !st.IsDir() {
			return candidate, nil
		}
	}
	if p, err := exec.LookPath(name); err == nil {
		return p, nil
	}
	return "", fmt.Errorf("%s not found in PATH or next to executable", name)
}

// systemd helpers

func systemctl(ctx context.Context, args ...string) error {
	out, err := exec.CommandContext(ctx, "systemctl", args...).CombinedOutput()
	if err != nil {
		return fmt.Errorf("systemctl %s: %s: %w", strings.Join(args, " "), strings.TrimSpace(string(out)), err)
	}
	return nil
}

func systemctlActive(ctx context.Context, unit string) bool {
	return exec.CommandContext(ctx, "systemctl", "is-active", "--quiet", unit).Run() == nil
}

func systemctlEnabled(ctx context.Context, unit string) bool {
	return exec.CommandContext(ctx, "systemctl", "is-enabled", "--quiet", unit).Run() == nil
}
