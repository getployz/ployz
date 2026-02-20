package daemon

import (
	"context"
	"fmt"
	"os"
	"os/exec"
	"os/signal"
	"path/filepath"
	"strconv"
	"strings"
	"syscall"
	"time"

	"ployz/cmd/ployz/cmdutil"
	"ployz/cmd/ployz/ui"
	"ployz/pkg/sdk/client"
	sdkdaemon "ployz/pkg/sdk/daemon"

	"github.com/spf13/cobra"
)

type options struct {
	socket   string
	dataRoot string
}

func Cmd() *cobra.Command {
	opts := &options{}

	cmd := &cobra.Command{
		Use:   "daemon",
		Short: "Manage local ployzd lifecycle",
	}

	cmd.PersistentFlags().StringVar(&opts.socket, "socket", cmdutil.DefaultSocketPath(), "ployzd unix socket path")
	cmd.PersistentFlags().StringVar(&opts.dataRoot, "data-root", cmdutil.DefaultDataRoot(), "Machine data root")

	cmd.AddCommand(runCmd(opts))
	cmd.AddCommand(startCmd(opts))
	cmd.AddCommand(stopCmd(opts))
	cmd.AddCommand(statusCmd(opts))
	return cmd
}

func runCmd(opts *options) *cobra.Command {
	return &cobra.Command{
		Use:   "run",
		Short: "Run ployzd in foreground",
		RunE: func(cmd *cobra.Command, args []string) error {
			ctx, cancel := signal.NotifyContext(cmd.Context(), os.Interrupt, syscall.SIGTERM)
			defer cancel()
			return sdkdaemon.Run(ctx, opts.socket, opts.dataRoot)
		},
	}
}

func startCmd(opts *options) *cobra.Command {
	return &cobra.Command{
		Use:   "start",
		Short: "Start ployzd in background",
		RunE: func(cmd *cobra.Command, args []string) error {
			pidPath := daemonPIDPath(opts.dataRoot)
			if pid, running := readRunningPID(pidPath); running {
				fmt.Println(ui.InfoMsg("ployzd already running (pid %d)", pid))
				return nil
			}

			if err := os.MkdirAll(opts.dataRoot, 0o755); err != nil {
				return fmt.Errorf("create daemon data directory: %w", err)
			}
			logFile, err := os.OpenFile(daemonLogPath(opts.dataRoot), os.O_APPEND|os.O_CREATE|os.O_WRONLY, 0o644)
			if err != nil {
				return fmt.Errorf("open daemon log file: %w", err)
			}
			defer logFile.Close()

			exe, err := os.Executable()
			if err != nil {
				return fmt.Errorf("resolve executable path: %w", err)
			}

			proc := exec.Command(exe, "daemon", "run", "--socket", opts.socket, "--data-root", opts.dataRoot)
			proc.Stdout = logFile
			proc.Stderr = logFile
			proc.Stdin = nil
			if err := proc.Start(); err != nil {
				return fmt.Errorf("start daemon process: %w", err)
			}

			if err := os.WriteFile(pidPath, []byte(strconv.Itoa(proc.Process.Pid)+"\n"), 0o644); err != nil {
				_ = proc.Process.Kill()
				return fmt.Errorf("write daemon pid file: %w", err)
			}

			readyCtx, cancel := context.WithTimeout(cmd.Context(), 8*time.Second)
			defer cancel()
			for {
				if err := healthCheck(readyCtx, opts.socket); err == nil {
					fmt.Println(ui.SuccessMsg("started ployzd (pid %d)", proc.Process.Pid))
					fmt.Print(ui.KeyValues("  ",
						ui.KV("socket", opts.socket),
						ui.KV("pid file", pidPath),
						ui.KV("log", daemonLogPath(opts.dataRoot)),
					))
					return nil
				}

				select {
				case <-readyCtx.Done():
					_ = proc.Process.Kill()
					_ = os.Remove(pidPath)
					return fmt.Errorf("daemon did not become ready in time")
				case <-time.After(200 * time.Millisecond):
				}
			}
		},
	}
}

func stopCmd(opts *options) *cobra.Command {
	return &cobra.Command{
		Use:   "stop",
		Short: "Stop background ployzd",
		RunE: func(cmd *cobra.Command, args []string) error {
			pidPath := daemonPIDPath(opts.dataRoot)
			pid, running := readRunningPID(pidPath)
			if !running {
				_ = os.Remove(pidPath)
				fmt.Println(ui.InfoMsg("ployzd is not running"))
				return nil
			}

			proc, err := os.FindProcess(pid)
			if err != nil {
				return fmt.Errorf("find daemon process %d: %w", pid, err)
			}
			if err := proc.Signal(syscall.SIGTERM); err != nil {
				return fmt.Errorf("signal daemon process %d: %w", pid, err)
			}

			stopCtx, cancel := context.WithTimeout(cmd.Context(), 8*time.Second)
			defer cancel()
			for {
				if !processRunning(pid) {
					_ = os.Remove(pidPath)
					fmt.Println(ui.SuccessMsg("stopped ployzd"))
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
		},
	}
}

func statusCmd(opts *options) *cobra.Command {
	return &cobra.Command{
		Use:   "status",
		Short: "Show daemon status",
		RunE: func(cmd *cobra.Command, args []string) error {
			pidPath := daemonPIDPath(opts.dataRoot)
			pid, running := readRunningPID(pidPath)
			healthErr := healthCheck(cmd.Context(), opts.socket)
			healthy := healthErr == nil

			pidText := "-"
			if running {
				pidText = strconv.Itoa(pid)
			}
			healthText := "down"
			if healthy {
				healthText = "ok"
			}

			fmt.Print(ui.KeyValues("",
				ui.KV("running", ui.Bool(running)),
				ui.KV("health", healthText),
				ui.KV("pid", pidText),
				ui.KV("socket", opts.socket),
				ui.KV("pid file", pidPath),
				ui.KV("log", daemonLogPath(opts.dataRoot)),
			))
			if healthErr != nil {
				fmt.Println(ui.Muted("  health check: " + healthErr.Error()))
			}
			return nil
		},
	}
}

func daemonPIDPath(dataRoot string) string {
	return filepath.Join(dataRoot, "ployzd.pid")
}

func daemonLogPath(dataRoot string) string {
	return filepath.Join(dataRoot, "ployzd.log")
}

func readRunningPID(pidPath string) (int, bool) {
	data, err := os.ReadFile(pidPath)
	if err != nil {
		return 0, false
	}
	pid, err := strconv.Atoi(strings.TrimSpace(string(data)))
	if err != nil || pid <= 0 {
		return 0, false
	}
	return pid, processRunning(pid)
}

func processRunning(pid int) bool {
	proc, err := os.FindProcess(pid)
	if err != nil {
		return false
	}
	err = proc.Signal(syscall.Signal(0))
	return err == nil
}

func healthCheck(ctx context.Context, socketPath string) error {
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
