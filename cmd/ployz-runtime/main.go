package main

import (
	"fmt"
	"os"
	"os/signal"
	"syscall"

	"ployz/internal/buildinfo"
	"ployz/internal/runtime/engine"
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
	var dataRoot string

	cmd := &cobra.Command{
		Use:     "ployz-runtime",
		Short:   "Ployz runtime reconciler",
		Version: buildinfo.Version,
		RunE: func(cmd *cobra.Command, args []string) error {
			ctx, stop := signal.NotifyContext(cmd.Context(), os.Interrupt, syscall.SIGTERM)
			defer stop()
			return engine.Run(ctx, dataRoot)
		},
	}

	cmd.Flags().StringVar(&dataRoot, "data-root", defaults.DataRoot(), "Machine data root")
	return cmd
}
