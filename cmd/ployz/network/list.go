package network

import (
	"fmt"
	"maps"
	"slices"
	"strings"

	"ployz/cmd/ployz/ui"
	"ployz/pkg/sdk/cluster"

	"github.com/spf13/cobra"
)

func listCmd() *cobra.Command {
	return &cobra.Command{
		Use:     "list",
		Aliases: []string{"ls"},
		Short:   "List networks from configured contexts",
		RunE: func(cmd *cobra.Command, args []string) error {
			cfg, err := cluster.LoadDefault()
			if err != nil {
				return err
			}

			counts := make(map[string]int)
			for _, cl := range cfg.Clusters {
				network := strings.TrimSpace(cl.Network)
				if network == "" {
					continue
				}
				counts[network]++
			}

			if len(counts) == 0 {
				fmt.Println(ui.Muted("no networks configured"))
				return nil
			}

			currentNetwork := ""
			if _, cl, ok := cfg.Current(); ok {
				currentNetwork = strings.TrimSpace(cl.Network)
			}

			networks := slices.Sorted(maps.Keys(counts))
			rows := make([][]string, 0, len(networks))
			for _, network := range networks {
				current := ""
				if network == currentNetwork {
					current = "*"
				}
				rows = append(rows, []string{network, current, fmt.Sprintf("%d", counts[network])})
			}

			fmt.Println(ui.Table([]string{"Network", "Current", "Contexts"}, rows))
			return nil
		},
	}
}
