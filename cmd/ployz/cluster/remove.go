package cluster

import (
	"fmt"
	"strings"

	"ployz/cmd/ployz/ui"
	config "ployz/pkg/sdk/cluster"
	sdkmachine "ployz/pkg/sdk/machine"

	"github.com/spf13/cobra"
)

func removeCmd() *cobra.Command {
	return &cobra.Command{
		Use:     "remove <name>",
		Aliases: []string{"rm", "delete"},
		Short:   "Remove a cluster and tear down its runtime",
		Args:    cobra.ExactArgs(1),
		RunE: func(cmd *cobra.Command, args []string) error {
			name := strings.TrimSpace(args[0])
			cfg, err := config.LoadDefault()
			if err != nil {
				return err
			}
			cl, ok := cfg.Cluster(name)
			if !ok {
				return fmt.Errorf("cluster %q not found", name)
			}

			// Tear down runtime via daemon.
			api, dialErr := cl.Dial(cmd.Context())
			if dialErr == nil {
				svc := sdkmachine.New(api)
				if err := svc.Stop(cmd.Context(), true); err != nil {
					fmt.Println(ui.WarnMsg("could not stop runtime: %v", err))
				} else {
					fmt.Println(ui.SuccessMsg("tore down runtime for %s", ui.Accent(cl.Network)))
				}
				_ = api.Close()
			}

			cfg.Delete(name)
			if strings.TrimSpace(cfg.CurrentCluster) == name {
				names := cfg.ClusterNames()
				if len(names) > 0 {
					cfg.CurrentCluster = names[0]
				} else {
					cfg.CurrentCluster = ""
				}
			}
			if err := cfg.Save(); err != nil {
				return err
			}

			fmt.Println(ui.SuccessMsg("removed cluster %s", ui.Accent(name)))
			if strings.TrimSpace(cfg.CurrentCluster) != "" {
				fmt.Println(ui.Muted("  current cluster: " + cfg.CurrentCluster))
			}
			return nil
		},
	}
}
