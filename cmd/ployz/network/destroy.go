package network

import (
	"errors"
	"fmt"
	"strings"

	"ployz/cmd/ployz/cmdutil"
	"ployz/cmd/ployz/ui"
	"ployz/pkg/sdk/client"
	sdkmachine "ployz/pkg/sdk/machine"

	"github.com/spf13/cobra"
)

func destroyCmd() *cobra.Command {
	var (
		cf    cmdutil.ContextFlags
		purge bool
	)

	cmd := &cobra.Command{
		Use:   "destroy [network]",
		Short: "Destroy runtime resources for the selected context",
		Args:  cobra.MaximumNArgs(1),
		RunE: func(cmd *cobra.Command, args []string) error {
			contextName, api, cl, err := cf.DialService(cmd.Context())
			if err != nil {
				return err
			}
			defer func() {
				_ = api.Close()
			}()

			expectedNetwork := ""
			if len(args) == 1 {
				expectedNetwork = strings.TrimSpace(args[0])
			}
			selectedNetwork := strings.TrimSpace(cl.Network)
			if expectedNetwork != "" && selectedNetwork != expectedNetwork {
				return fmt.Errorf("context %q targets network %q, not %q", contextName, selectedNetwork, expectedNetwork)
			}

			svc := sdkmachine.New(api)
			if err := svc.Stop(cmd.Context(), purge); err != nil {
				return decorateDestroyError(contextName, err)
			}

			fmt.Println(ui.SuccessMsg("destroyed runtime for context %s on network %s", ui.Accent(contextName), ui.Accent(selectedNetwork)))
			return nil
		},
	}

	cf.Bind(cmd)
	cmd.Flags().BoolVar(&purge, "purge", true, "Purge local state while disabling network runtime")
	return cmd
}

func decorateDestroyError(contextName string, err error) error {
	if err == nil {
		return nil
	}
	if hint := client.PreconditionHint(err); hint != "" {
		return fmt.Errorf("destroy network runtime for context %q: %w. %s", contextName, err, hint)
	}
	if errors.Is(err, client.ErrNetworkDestroyHasWorkloads) {
		return fmt.Errorf("destroy network runtime for context %q: %w. remove workloads first with `ployz service remove <name>`", contextName, err)
	}
	if errors.Is(err, client.ErrNetworkDestroyHasMachines) {
		return fmt.Errorf("destroy network runtime for context %q: %w. remove attached machines first with `ployz machine remove <id>`", contextName, err)
	}
	return fmt.Errorf("destroy network runtime for context %q: %w", contextName, err)
}
