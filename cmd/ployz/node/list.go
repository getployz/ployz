package node

import (
	"fmt"
	"strconv"
	"strings"

	"ployz/cmd/ployz/cmdutil"
	"ployz/cmd/ployz/ui"

	"github.com/spf13/cobra"
)

func listCmd() *cobra.Command {
	var cf cmdutil.ClusterFlags

	cmd := &cobra.Command{
		Use:     "list",
		Aliases: []string{"ls"},
		Short:   "List nodes in the cluster",
		RunE: func(cmd *cobra.Command, args []string) error {
			_, svc, cl, err := service(cmd.Context(), &cf)
			if err != nil {
				return err
			}

			machines, err := svc.ListMachines(cmd.Context(), cl.Network)
			if err != nil {
				return err
			}
			if len(machines) == 0 {
				fmt.Println(ui.Muted("no nodes registered"))
				return nil
			}

			rows := make([][]string, len(machines))
			for i, m := range machines {
				updated := strings.TrimSpace(m.LastUpdated)
				if updated == "" {
					updated = "-"
				}
				version := "-"
				if m.Version > 0 {
					version = strconv.FormatInt(m.Version, 10)
				}
				rows[i] = []string{
					strconv.Itoa(i + 1),
					m.ID,
					m.Subnet,
					m.ManagementIP,
					m.Endpoint,
					version,
					updated,
				}
			}

			fmt.Println(ui.Table(
				[]string{"#", "ID", "Subnet", "Management", "Endpoint", "Version", "Updated"},
				rows,
			))
			return nil
		},
	}

	cf.Bind(cmd)
	return cmd
}
