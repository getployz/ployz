package cluster

import (
	"fmt"
	"strings"

	"ployz/cmd/ployz/ui"
	config "ployz/pkg/sdk/cluster"

	"github.com/spf13/cobra"
)

func listCmd() *cobra.Command {
	return &cobra.Command{
		Use:     "list",
		Aliases: []string{"ls"},
		Short:   "List configured clusters",
		RunE: func(cmd *cobra.Command, args []string) error {
			return runList()
		},
	}
}

func runList() error {
	cfg, err := config.LoadDefault()
	if err != nil {
		return err
	}
	names := cfg.ClusterNames()
	if len(names) == 0 {
		fmt.Println(ui.Muted("no clusters configured"))
		fmt.Println(ui.Muted("  run: ployz init"))
		return nil
	}
	currentName, _, hasCurrent := cfg.Current()

	rows := make([][]string, 0, len(names))
	for _, name := range names {
		cl, _ := cfg.Cluster(name)
		current := ""
		if hasCurrent && currentName == name {
			current = "*"
		}
		network := strings.TrimSpace(cl.Network)
		if network == "" {
			network = "-"
		}
		connType := "-"
		if len(cl.Connections) > 0 {
			connType = cl.Connections[0].Type()
		}
		rows = append(rows, []string{
			name,
			current,
			network,
			connType,
		})
	}

	fmt.Println(ui.Table(
		[]string{"Name", "Current", "Network", "Connection"},
		rows,
	))
	fmt.Println(ui.Muted("config: " + cfg.Path()))
	return nil
}
