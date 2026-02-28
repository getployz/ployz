package corrorun

import (
	"context"
	"fmt"
	"log/slog"
	"net/netip"
	"os"
	"os/exec"
)

// Exec runs Corrosion as a child process.
// Implements machine.StoreRuntime.
type Exec struct {
	binary  string
	paths   Paths
	apiAddr netip.AddrPort

	cmd *exec.Cmd
}

// ExecOption configures an Exec runtime.
type ExecOption func(*Exec)

// WithBinary sets the path to the corrosion binary. Defaults to "corrosion"
// (found via PATH).
func WithBinary(path string) ExecOption {
	return func(e *Exec) { e.binary = path }
}

// NewExec creates a child-process-based Corrosion runtime.
func NewExec(paths Paths, apiAddr netip.AddrPort, opts ...ExecOption) *Exec {
	e := &Exec{
		binary:  "corrosion",
		paths:   paths,
		apiAddr: apiAddr,
	}
	for _, opt := range opts {
		opt(e)
	}
	return e
}

// Start launches the corrosion process and waits for it to be ready.
func (e *Exec) Start(ctx context.Context) error {
	e.cmd = exec.CommandContext(ctx, e.binary, "agent", "-c", e.paths.Config)
	e.cmd.Stdout = os.Stdout
	e.cmd.Stderr = os.Stderr

	if err := e.cmd.Start(); err != nil {
		return fmt.Errorf("start corrosion process: %w", err)
	}

	if err := WaitReady(ctx, e.apiAddr); err != nil {
		_ = e.cmd.Process.Kill() // best-effort cleanup
		return err
	}

	slog.Info("Corrosion process started.", "pid", e.cmd.Process.Pid)
	return nil
}

// Stop kills the corrosion process and waits for it to exit.
func (e *Exec) Stop(ctx context.Context) error {
	if e.cmd == nil || e.cmd.Process == nil {
		return nil
	}

	if err := e.cmd.Process.Signal(os.Interrupt); err != nil {
		// Process may have already exited.
		return nil
	}

	done := make(chan error, 1)
	go func() {
		done <- e.cmd.Wait()
	}()

	select {
	case <-ctx.Done():
		_ = e.cmd.Process.Kill() // best-effort force kill
		return fmt.Errorf("stop corrosion process: %w", ctx.Err())
	case err := <-done:
		if err != nil {
			// Exit status != 0 is expected on interrupt.
			slog.Debug("Corrosion process exited.", "err", err)
		}
		return nil
	}
}
