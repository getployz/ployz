package cluster

import (
	"fmt"
	"strings"

	"ployz/cmd/ployz/ui"
	config "ployz/pkg/sdk/cluster"

	"github.com/spf13/cobra"
)

func useCmd() *cobra.Command {
	return &cobra.Command{
		Use:   "use <name>",
		Short: "Switch current context",
		Args:  cobra.ExactArgs(1),
		RunE: func(cmd *cobra.Command, args []string) error {
			name := strings.TrimSpace(args[0])
			cfg, err := config.LoadDefault()
			if err != nil {
				return err
			}
			if _, ok := cfg.Cluster(name); !ok {
				return fmt.Errorf("context %q not found", name)
			}
			cfg.CurrentCluster = name
			if err := cfg.Save(); err != nil {
				return err
			}
			fmt.Println(ui.SuccessMsg("current context is now %s", ui.Accent(name)))
			return nil
		},
	}
}
