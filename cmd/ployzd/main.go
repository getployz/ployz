package main

import (
	"fmt"
	"os"
	"os/signal"
	"syscall"

	"ployz/internal/daemon/app"
	"ployz/pkg/sdk/client"
	"ployz/pkg/sdk/defaults"

	"github.com/spf13/cobra"
)

func main() {
	if err := rootCmd().Execute(); err != nil {
		fmt.Fprintf(os.Stderr, "error: %v\n", err)
		os.Exit(1)
	}
}

func rootCmd() *cobra.Command {
	var socketPath string
	var dataRoot string

	cmd := &cobra.Command{
		Use:   "ployzd",
		Short: "Ployz network daemon",
		RunE: func(cmd *cobra.Command, args []string) error {
			ctx, stop := signal.NotifyContext(cmd.Context(), os.Interrupt, syscall.SIGTERM)
			defer stop()
			return app.Run(ctx, socketPath, dataRoot)
		},
	}

	cmd.Flags().StringVar(&socketPath, "socket", client.DefaultSocketPath(), "Unix socket path")
	cmd.Flags().StringVar(&dataRoot, "data-root", defaults.DataRoot(), "Machine data root")
	cmd.AddCommand(dialStdioCmd())
	return cmd
}
