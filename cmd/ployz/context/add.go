package contextcmd

import (
	"fmt"

	"ployz/cmd/ployz/ui"
	"ployz/config"

	"github.com/spf13/cobra"
)

func addCmd() *cobra.Command {
	var host, socket string

	cmd := &cobra.Command{
		Use:   "add <name>",
		Short: "Add or update a context",
		Args:  cobra.ExactArgs(1),
		RunE: func(_ *cobra.Command, args []string) error {
			name := args[0]

			if host == "" && socket == "" {
				return fmt.Errorf("at least one of --host or --socket is required")
			}

			cfg, err := config.Load()
			if err != nil {
				return err
			}

			cfg.Set(name, config.Context{
				Host:   host,
				Socket: socket,
			})

			if err := cfg.Save(); err != nil {
				return err
			}

			fmt.Println(ui.SuccessMsg("Context %s saved.", ui.Bold(name)))
			return nil
		},
	}

	cmd.Flags().StringVar(&host, "host", "", "SSH target (e.g. root@host)")
	cmd.Flags().StringVar(&socket, "socket", "", "Unix socket path")
	return cmd
}
