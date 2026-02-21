package cluster

import (
	"fmt"
	"strings"

	"ployz/cmd/ployz/ui"
	config "ployz/pkg/sdk/cluster"

	"github.com/spf13/cobra"
)

func setCmd() *cobra.Command {
	var network string
	var socket string
	var setCurrent bool

	cmd := &cobra.Command{
		Use:   "set <name>",
		Short: "Create or update a cluster profile",
		Args:  cobra.ExactArgs(1),
		RunE: func(cmd *cobra.Command, args []string) error {
			name := strings.TrimSpace(args[0])
			if name == "" {
				return fmt.Errorf("cluster name is required")
			}

			cfg, err := config.LoadDefault()
			if err != nil {
				return err
			}

			entry, _ := cfg.Cluster(name)
			if cmd.Flags().Changed("network") {
				entry.Network = strings.TrimSpace(network)
			}
			if cmd.Flags().Changed("socket") {
				entry.Connections = []config.Connection{{Unix: strings.TrimSpace(socket)}}
			}

			if strings.TrimSpace(entry.Network) == "" {
				entry.Network = "default"
			}

			cfg.Upsert(name, entry)
			if setCurrent || strings.TrimSpace(cfg.CurrentCluster) == "" {
				cfg.CurrentCluster = name
			}

			if err := cfg.Save(); err != nil {
				return err
			}

			fmt.Println(ui.SuccessMsg("saved cluster %s", ui.Accent(name)))
			return nil
		},
	}

	cmd.Flags().StringVar(&network, "network", "", "Network name for this cluster")
	cmd.Flags().StringVar(&socket, "socket", "", "Daemon socket path for this cluster")
	cmd.Flags().BoolVar(&setCurrent, "current", true, "Set this cluster as current")
	return cmd
}
