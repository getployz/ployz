package cluster

import (
	"fmt"
	"strings"

	"ployz/cmd/ployz/ui"
	config "ployz/pkg/sdk/cluster"

	"github.com/spf13/cobra"
)

func removeCmd() *cobra.Command {
	return &cobra.Command{
		Use:     "remove <name>",
		Aliases: []string{"rm", "delete"},
		Short:   "Remove a local context from config",
		Long:    "Remove a local context from config only. This command does not stop or disable any network runtime.",
		Args:    cobra.ExactArgs(1),
		RunE: func(cmd *cobra.Command, args []string) error {
			name := strings.TrimSpace(args[0])
			cfg, err := config.LoadDefault()
			if err != nil {
				return err
			}
			_, ok := cfg.Cluster(name)
			if !ok {
				return fmt.Errorf("context %q not found", name)
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

			fmt.Println(ui.SuccessMsg("removed context %s", ui.Accent(name)))
			fmt.Println(ui.Muted("  note: use 'ployz network destroy --context " + name + "' to tear down runtime resources"))
			if strings.TrimSpace(cfg.CurrentCluster) != "" {
				fmt.Println(ui.Muted("  current context: " + cfg.CurrentCluster))
			}
			return nil
		},
	}
}
