package contextcmd

import (
	"fmt"
	"sort"

	"ployz/cmd/ployz/cmdutil"
	"ployz/cmd/ployz/ui"
	"ployz/config"

	"github.com/spf13/cobra"
)

func listCmd() *cobra.Command {
	return &cobra.Command{
		Use:   "list",
		Short: "List available contexts",
		Args:  cobra.NoArgs,
		RunE: func(cmd *cobra.Command, _ []string) error {
			// Auto-discover local daemon before listing.
			if err := cmdutil.Discover(cmd.Context()); err != nil {
				return err
			}

			cfg, err := config.Load()
			if err != nil {
				return err
			}

			if len(cfg.Contexts) == 0 {
				fmt.Println(ui.InfoMsg("No contexts configured."))
				return nil
			}

			names := make([]string, 0, len(cfg.Contexts))
			for name := range cfg.Contexts {
				names = append(names, name)
			}
			sort.Strings(names)

			var rows [][]string
			for _, name := range names {
				c := cfg.Contexts[name]

				current := ""
				if name == cfg.CurrentContext {
					current = "*"
				}

				kind := "ssh"
				target := c.Host
				if c.Socket != "" {
					kind = "local"
					target = c.Socket
				}

				rows = append(rows, []string{current, name, kind, target})
			}

			fmt.Println(ui.Table([]string{"", "NAME", "TYPE", "TARGET"}, rows))
			return nil
		},
	}
}
