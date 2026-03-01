package main

import (
	"log/slog"
	"os"
	"os/signal"
	"runtime"
	"syscall"

	daemonruntime "ployz/daemon"
	"ployz/internal/logging"
	"ployz/internal/support/buildinfo"

	"github.com/spf13/cobra"
)

func main() {
	if err := logging.Configure(logging.LevelInfo); err != nil {
		_, _ = os.Stderr.WriteString("configure logger: " + err.Error() + "\n")
		os.Exit(1)
	}

	if err := rootCmd().Execute(); err != nil {
		slog.Error("command failed", "err", err)
		os.Exit(1)
	}
}

func rootCmd() *cobra.Command {
	var socketPath string
	var dataRoot string
	var debug bool

	cmd := &cobra.Command{
		Use:     "ployzd",
		Short:   "Ployz network daemon",
		Version: buildinfo.Version,
		PersistentPreRunE: func(cmd *cobra.Command, args []string) error {
			level := logging.LevelInfo
			if debug {
				level = logging.LevelDebug
			}
			return logging.Configure(level)
		},
		RunE: func(cmd *cobra.Command, args []string) error {
			ctx, stop := signal.NotifyContext(cmd.Context(), os.Interrupt, syscall.SIGTERM)
			defer stop()
			return daemonruntime.Run(ctx, dataRoot, socketPath)
		},
	}

	cmd.PersistentFlags().BoolVar(&debug, "debug", false, "Enable debug logging")
	cmd.Flags().StringVar(&socketPath, "socket", defaultSocketPath(), "Unix socket path")
	cmd.Flags().StringVar(&dataRoot, "data-root", defaultDataRoot(), "Machine data root")
	cmd.AddCommand(dialStdioCmd())
	return cmd
}

func defaultSocketPath() string {
	if runtime.GOOS == "darwin" {
		return "/tmp/ployzd.sock"
	}
	return "/var/run/ployzd.sock"
}

func defaultDataRoot() string {
	if runtime.GOOS == "darwin" {
		return "/usr/local/var/lib/ployz/networks"
	}
	return "/var/lib/ployz/networks"
}
