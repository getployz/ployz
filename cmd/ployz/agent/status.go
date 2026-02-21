package agent

import (
	"fmt"

	"ployz/cmd/ployz/ui"

	"github.com/spf13/cobra"
)

func statusCmd() *cobra.Command {
	return &cobra.Command{
		Use:   "status",
		Short: "Show agent service health",
		RunE: func(cmd *cobra.Command, args []string) error {
			svc := NewPlatformService()

			st, err := svc.Status(cmd.Context())
			if err != nil {
				return err
			}

			fmt.Print(ui.KeyValues("",
				ui.KV("platform", st.Platform),
				ui.KV("daemon installed", ui.Bool(st.DaemonInstalled)),
				ui.KV("daemon running", ui.Bool(st.DaemonRunning)),
				ui.KV("runtime installed", ui.Bool(st.RuntimeInstalled)),
				ui.KV("runtime running", ui.Bool(st.RuntimeRunning)),
			))
			return nil
		},
	}
}
