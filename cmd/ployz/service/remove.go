package service

import (
	"fmt"
	"strings"

	"ployz/cmd/ployz/cmdutil"
	"ployz/cmd/ployz/ui"

	"github.com/spf13/cobra"
)

func removeCmd() *cobra.Command {
	var cf cmdutil.ContextFlags

	cmd := &cobra.Command{
		Use:     "remove <name>",
		Aliases: []string{"rm", "delete"},
		Short:   "Remove a deployed service",
		Args:    cobra.ExactArgs(1),
		RunE: func(cmd *cobra.Command, args []string) error {
			contextName, api, _, err := cf.DialService(cmd.Context())
			if err != nil {
				return err
			}
			defer func() {
				_ = api.Close()
			}()

			name := strings.TrimSpace(args[0])
			if name == "" {
				return fmt.Errorf("service name is required")
			}
			if strings.ContainsAny(name, " \t\n\r") {
				return fmt.Errorf("service name %q must not contain whitespace", name)
			}

			if err := api.RemoveNamespace(cmd.Context(), name); err != nil {
				return fmt.Errorf("remove service %q via context %q: %w", name, contextName, err)
			}

			fmt.Println(ui.SuccessMsg("removed service %s via context %s", ui.Accent(name), ui.Accent(contextName)))
			return nil
		},
	}

	cf.Bind(cmd)
	return cmd
}
