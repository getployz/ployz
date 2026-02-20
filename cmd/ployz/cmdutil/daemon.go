package cmdutil

import (
	"context"
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"strconv"
	"strings"
	"syscall"
	"time"

	"ployz/pkg/sdk/client"
)

func EnsureDaemon(ctx context.Context, socketPath, dataRoot string) error {
	if IsDaemonRunning(ctx, socketPath) {
		return nil
	}
	_, err := StartDaemon(ctx, socketPath, dataRoot)
	return err
}

func StartDaemon(ctx context.Context, socketPath, dataRoot string) (int, error) {
	pidPath := DaemonPIDPath(dataRoot)
	if pid, running := ReadRunningPID(pidPath); running {
		return pid, nil
	}

	if err := os.MkdirAll(dataRoot, 0o755); err != nil {
		return 0, fmt.Errorf("create daemon data directory: %w", err)
	}
	logFile, err := os.OpenFile(DaemonLogPath(dataRoot), os.O_APPEND|os.O_CREATE|os.O_WRONLY, 0o644)
	if err != nil {
		return 0, fmt.Errorf("open daemon log file: %w", err)
	}
	defer logFile.Close()

	exe, err := os.Executable()
	if err != nil {
		return 0, fmt.Errorf("resolve executable path: %w", err)
	}

	proc := exec.Command(exe, "daemon", "run", "--socket", socketPath, "--data-root", dataRoot)
	proc.Stdout = logFile
	proc.Stderr = logFile
	proc.Stdin = nil
	if err := proc.Start(); err != nil {
		return 0, fmt.Errorf("start daemon process: %w", err)
	}

	pid := proc.Process.Pid
	if err := os.WriteFile(pidPath, []byte(strconv.Itoa(pid)+"\n"), 0o644); err != nil {
		_ = proc.Process.Kill()
		return 0, fmt.Errorf("write daemon pid file: %w", err)
	}

	readyCtx, cancel := context.WithTimeout(ctx, 8*time.Second)
	defer cancel()
	for {
		if err := HealthCheck(readyCtx, socketPath); err == nil {
			return pid, nil
		}
		select {
		case <-readyCtx.Done():
			_ = proc.Process.Kill()
			_ = os.Remove(pidPath)
			return 0, fmt.Errorf("daemon did not become ready in time")
		case <-time.After(200 * time.Millisecond):
		}
	}
}

func StopDaemon(ctx context.Context, dataRoot string) error {
	pidPath := DaemonPIDPath(dataRoot)
	pid, running := ReadRunningPID(pidPath)
	if !running {
		_ = os.Remove(pidPath)
		return nil
	}

	proc, err := os.FindProcess(pid)
	if err != nil {
		return fmt.Errorf("find daemon process %d: %w", pid, err)
	}
	if err := proc.Signal(syscall.SIGTERM); err != nil {
		return fmt.Errorf("signal daemon process %d: %w", pid, err)
	}

	stopCtx, cancel := context.WithTimeout(ctx, 8*time.Second)
	defer cancel()
	for {
		if !ProcessRunning(pid) {
			_ = os.Remove(pidPath)
			return nil
		}
		select {
		case <-stopCtx.Done():
			_ = proc.Signal(syscall.SIGKILL)
			_ = os.Remove(pidPath)
			return fmt.Errorf("daemon did not stop gracefully")
		case <-time.After(200 * time.Millisecond):
		}
	}
}

func IsDaemonRunning(_ context.Context, socketPath string) bool {
	ctx, cancel := context.WithTimeout(context.Background(), 2*time.Second)
	defer cancel()
	return HealthCheck(ctx, socketPath) == nil
}

func DaemonPIDPath(dataRoot string) string {
	return filepath.Join(dataRoot, "ployzd.pid")
}

func DaemonLogPath(dataRoot string) string {
	return filepath.Join(dataRoot, "ployzd.log")
}

func ReadRunningPID(pidPath string) (int, bool) {
	data, err := os.ReadFile(pidPath)
	if err != nil {
		return 0, false
	}
	pid, err := strconv.Atoi(strings.TrimSpace(string(data)))
	if err != nil || pid <= 0 {
		return 0, false
	}
	return pid, ProcessRunning(pid)
}

func ProcessRunning(pid int) bool {
	proc, err := os.FindProcess(pid)
	if err != nil {
		return false
	}
	err = proc.Signal(syscall.Signal(0))
	return err == nil
}

func HealthCheck(ctx context.Context, socketPath string) error {
	checkCtx, cancel := context.WithTimeout(ctx, 2*time.Second)
	defer cancel()

	api, err := client.NewUnix(socketPath)
	if err != nil {
		return fmt.Errorf("connect to daemon: %w", err)
	}
	defer func() { _ = api.Close() }()

	if _, err := api.GetStatus(checkCtx, "default"); err != nil {
		return fmt.Errorf("daemon health check: %w", err)
	}
	return nil
}
