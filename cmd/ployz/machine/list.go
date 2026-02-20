package machine

import (
	"fmt"
	"strconv"
	"strings"

	"ployz/cmd/ployz/cmdutil"
	"ployz/cmd/ployz/ui"
	machinelib "ployz/internal/machine"

	"github.com/spf13/cobra"
)

func listCmd() *cobra.Command {
	var nf cmdutil.NetworkFlags

	cmd := &cobra.Command{
		Use:     "list",
		Aliases: []string{"ls"},
		Short:   "List machines in the network",
		RunE: func(cmd *cobra.Command, args []string) error {
			ctrl, err := machinelib.New()
			if err != nil {
				return err
			}
			defer ctrl.Close()

			machines, err := ctrl.ListMachines(cmd.Context(), nf.Config())
			if err != nil {
				return err
			}
			if len(machines) == 0 {
				fmt.Println(ui.Muted("no machines registered"))
				return nil
			}

			rows := make([][]string, len(machines))
			for i, m := range machines {
				updated := strings.TrimSpace(m.LastUpdated)
				if updated == "" {
					updated = "-"
				}
				rows[i] = []string{
					strconv.Itoa(i + 1),
					m.ID,
					m.Subnet,
					m.Management,
					m.Endpoint,
					updated,
				}
			}

			fmt.Println(ui.Table(
				[]string{"#", "ID", "Subnet", "Management", "Endpoint", "Updated"},
				rows,
			))
			return nil
		},
	}

	nf.Bind(cmd)
	return cmd
}
