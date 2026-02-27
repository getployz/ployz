package cluster

import (
	"fmt"
	"strings"

	"ployz/cmd/ployz/ui"
	config "ployz/pkg/sdk/cluster"

	"github.com/spf13/cobra"
)

func currentCmd() *cobra.Command {
	return &cobra.Command{
		Use:     "current",
		Aliases: []string{"show"},
		Short:   "Show current context",
		RunE: func(cmd *cobra.Command, args []string) error {
			cfg, err := config.LoadDefault()
			if err != nil {
				return err
			}
			name, cl, ok := cfg.Current()
			if !ok {
				return fmt.Errorf("no current context configured")
			}
			connSummary := "-"
			if len(cl.Connections) > 0 {
				c := cl.Connections[0]
				switch {
				case c.Unix != "":
					connSummary = "unix:" + c.Unix
				case c.SSH != "":
					connSummary = "ssh:" + c.SSH
				}
			}
			network := strings.TrimSpace(cl.Network)
			if network == "" {
				network = "default"
			}
			fmt.Print(ui.KeyValues("",
				ui.KV("name", name),
				ui.KV("network", network),
				ui.KV("connection", connSummary),
			))
			return nil
		},
	}
}
