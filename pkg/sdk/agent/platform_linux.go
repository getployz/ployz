//go:build linux

package agent

import (
	"context"
	"fmt"
	"os"
	"os/exec"
	"os/user"
	"path/filepath"
	"strconv"
	"strings"

	"ployz/pkg/sdk/defaults"
)

const daemonUnit = "ployzd.service"
const daemonUser = "ployzd"
const daemonGroup = "ployzd"

type linuxService struct{}

func NewPlatformService() PlatformService {
	return &linuxService{}
}

func (l *linuxService) Install(ctx context.Context, cfg InstallConfig) error {
	if os.Geteuid() != 0 {
		return fmt.Errorf("agent install requires root - run with sudo")
	}
	if err := removeStaleSocket(cfg.SocketPath); err != nil {
		return fmt.Errorf("remove stale daemon socket: %w", err)
	}

	if err := ensureSystemGroup(ctx, daemonGroup); err != nil {
		return err
	}
	if err := ensureSystemUser(ctx, daemonUser, daemonGroup, cfg.DataRoot); err != nil {
		return err
	}

	uid, gid, err := userNumericIDs(daemonUser)
	if err != nil {
		return err
	}
	if err := reconcileDaemonDataPaths(cfg.DataRoot, uid, gid, "linux"); err != nil {
		return fmt.Errorf("prepare data paths: %w", err)
	}

	logPath := DaemonLogPath(cfg.DataRoot)
	logFile, err := os.OpenFile(logPath, os.O_CREATE|os.O_APPEND|os.O_WRONLY, 0o644)
	if err != nil {
		return fmt.Errorf("create daemon log file: %w", err)
	}
	_ = logFile.Close()
	if err := os.Chown(logPath, uid, gid); err != nil {
		return fmt.Errorf("set daemon log owner: %w", err)
	}

	ployzBin, err := resolveBinary("ployz")
	if err != nil {
		return fmt.Errorf("resolve ployz binary: %w", err)
	}

	daemonContent := fmt.Sprintf(`[Unit]
Description=ployz daemon
After=network-online.target ployz-helper.service
Wants=network-online.target ployz-helper.service

[Service]
Type=simple
ExecStart=%s daemon run --socket %s --data-root %s
User=%s
Group=%s
RuntimeDirectory=ployz
RuntimeDirectoryMode=0755
Restart=always
RestartSec=5
StandardOutput=append:%s
StandardError=append:%s

[Install]
WantedBy=multi-user.target
`, ployzBin, cfg.SocketPath, cfg.DataRoot,
		daemonUser, daemonGroup,
		DaemonLogPath(cfg.DataRoot), DaemonLogPath(cfg.DataRoot))

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
	if err := systemctl(ctx, "restart", daemonUnit); err != nil {
		return fmt.Errorf("restart daemon: %w", err)
	}
	return nil
}

func (l *linuxService) Uninstall(ctx context.Context) error {
	if os.Geteuid() != 0 {
		return fmt.Errorf("agent uninstall requires root - run with sudo")
	}

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

func resolveBinary(name string) (string, error) {
	if exePath, err := os.Executable(); err == nil && !isGoRunExecutablePath(exePath) {
		candidate := filepath.Join(filepath.Dir(exePath), name)
		if st, statErr := os.Stat(candidate); statErr == nil && !st.IsDir() {
			return candidate, nil
		}
	}
	if p, err := exec.LookPath(name); err == nil {
		return p, nil
	}
	defaultInstalled := filepath.Join("/usr/local/bin", name)
	if st, err := os.Stat(defaultInstalled); err == nil && !st.IsDir() {
		return defaultInstalled, nil
	}
	if exePath, err := os.Executable(); err == nil && isGoRunExecutablePath(exePath) {
		return "", fmt.Errorf("%s not found in PATH and current executable is a temporary go run binary (%s); run `just install` and retry", name, exePath)
	}
	return "", fmt.Errorf("%s not found in PATH or next to executable", name)
}

func isGoRunExecutablePath(path string) bool {
	trimmed := strings.TrimSpace(path)
	if trimmed == "" {
		return false
	}
	return strings.Contains(filepath.Clean(trimmed), string(filepath.Separator)+"go-build")
}

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

func EnsureDaemonUser(ctx context.Context, dataRoot string) (int, int, error) {
	if strings.TrimSpace(dataRoot) == "" {
		dataRoot = defaults.DataRoot()
	}
	if err := ensureSystemGroup(ctx, daemonGroup); err != nil {
		return 0, 0, err
	}
	if err := ensureSystemUser(ctx, daemonUser, daemonGroup, dataRoot); err != nil {
		return 0, 0, err
	}
	uid, gid, err := userNumericIDs(daemonUser)
	if err != nil {
		return 0, 0, err
	}
	return uid, gid, nil
}

func ensureSystemGroup(ctx context.Context, groupName string) error {
	if exec.CommandContext(ctx, "getent", "group", groupName).Run() == nil {
		return nil
	}
	out, err := exec.CommandContext(ctx, "groupadd", "--system", groupName).CombinedOutput()
	if err != nil {
		return fmt.Errorf("create group %q: %w: %s", groupName, err, strings.TrimSpace(string(out)))
	}
	return nil
}

func ensureSystemUser(ctx context.Context, userName, groupName, homeDir string) error {
	if exec.CommandContext(ctx, "id", "-u", userName).Run() == nil {
		return nil
	}

	shell := "/usr/sbin/nologin"
	if _, err := os.Stat(shell); err != nil {
		shell = "/sbin/nologin"
	}
	out, err := exec.CommandContext(ctx,
		"useradd",
		"--system",
		"--gid", groupName,
		"--home-dir", homeDir,
		"--shell", shell,
		userName,
	).CombinedOutput()
	if err != nil {
		return fmt.Errorf("create user %q: %w: %s", userName, err, strings.TrimSpace(string(out)))
	}
	return nil
}

func userNumericIDs(userName string) (int, int, error) {
	u, err := user.Lookup(userName)
	if err != nil {
		return 0, 0, fmt.Errorf("lookup user %q: %w", userName, err)
	}
	uid, err := strconv.Atoi(u.Uid)
	if err != nil {
		return 0, 0, fmt.Errorf("parse uid for %q: %w", userName, err)
	}
	gid, err := strconv.Atoi(u.Gid)
	if err != nil {
		return 0, 0, fmt.Errorf("parse gid for %q: %w", userName, err)
	}
	return uid, gid, nil
}
