package cmdutil

import (
	"context"
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"strconv"
	"syscall"
	"time"
)

func EnsureRuntime(ctx context.Context, dataRoot string) error {
	if pid, running := ReadRunningPID(RuntimePIDPath(dataRoot)); running {
		if ProcessRunning(pid) {
			return nil
		}
	}
	_, err := StartRuntime(ctx, dataRoot)
	return err
}

func StartRuntime(ctx context.Context, dataRoot string) (int, error) {
	pidPath := RuntimePIDPath(dataRoot)
	if pid, running := ReadRunningPID(pidPath); running {
		return pid, nil
	}

	if err := os.MkdirAll(dataRoot, 0o755); err != nil {
		return 0, fmt.Errorf("create runtime data directory: %w", err)
	}
	logFile, err := os.OpenFile(RuntimeLogPath(dataRoot), os.O_APPEND|os.O_CREATE|os.O_WRONLY, 0o644)
	if err != nil {
		return 0, fmt.Errorf("open runtime log file: %w", err)
	}
	defer logFile.Close()

	proc, err := newRuntimeCommand(dataRoot)
	if err != nil {
		return 0, err
	}
	proc.Stdout = logFile
	proc.Stderr = logFile
	proc.Stdin = nil
	if err := proc.Start(); err != nil {
		return 0, fmt.Errorf("start runtime process: %w", err)
	}

	pid := proc.Process.Pid
	if err := os.WriteFile(pidPath, []byte(strconv.Itoa(pid)+"\n"), 0o644); err != nil {
		_ = proc.Process.Kill()
		return 0, fmt.Errorf("write runtime pid file: %w", err)
	}

	readyCtx, cancel := context.WithTimeout(ctx, 5*time.Second)
	defer cancel()
	for {
		if ProcessRunning(pid) {
			return pid, nil
		}
		select {
		case <-readyCtx.Done():
			_ = os.Remove(pidPath)
			return 0, fmt.Errorf("runtime did not stay running")
		case <-time.After(200 * time.Millisecond):
		}
	}
}

func StopRuntime(ctx context.Context, dataRoot string) error {
	pidPath := RuntimePIDPath(dataRoot)
	pid, running := ReadRunningPID(pidPath)
	if !running {
		_ = os.Remove(pidPath)
		return nil
	}

	proc, err := os.FindProcess(pid)
	if err != nil {
		return fmt.Errorf("find runtime process %d: %w", pid, err)
	}
	if err := proc.Signal(syscall.SIGTERM); err != nil {
		return fmt.Errorf("signal runtime process %d: %w", pid, err)
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
			return fmt.Errorf("runtime did not stop gracefully")
		case <-time.After(200 * time.Millisecond):
		}
	}
}

func RuntimePIDPath(dataRoot string) string {
	return filepath.Join(dataRoot, "ployz-runtime.pid")
}

func RuntimeLogPath(dataRoot string) string {
	return filepath.Join(dataRoot, "ployz-runtime.log")
}

func newRuntimeCommand(dataRoot string) (*exec.Cmd, error) {
	if exePath, err := os.Executable(); err == nil {
		dir := filepath.Dir(exePath)
		candidate := filepath.Join(dir, "ployz-runtime")
		if st, statErr := os.Stat(candidate); statErr == nil && !st.IsDir() {
			return exec.Command(candidate, "--data-root", dataRoot), nil
		}
	}

	if runtimeExe, err := exec.LookPath("ployz-runtime"); err == nil {
		return exec.Command(runtimeExe, "--data-root", dataRoot), nil
	}

	exePath, err := os.Executable()
	if err == nil {
		return exec.Command(exePath, "runtime", "run", "--data-root", dataRoot), nil
	}

	return nil, fmt.Errorf("find ployz-runtime executable: %w", err)
}
