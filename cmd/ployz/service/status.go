package service

import (
	"fmt"
	"strconv"
	"strings"

	"ployz/cmd/ployz/cmdutil"
	"ployz/cmd/ployz/ui"

	"github.com/spf13/cobra"
)

func statusCmd() *cobra.Command {
	var cf cmdutil.ContextFlags

	cmd := &cobra.Command{
		Use:   "status <service>",
		Short: "Show service deployment and container state",
		Args:  cobra.ExactArgs(1),
		RunE: func(cmd *cobra.Command, args []string) error {
			contextName, api, cl, err := cf.DialService(cmd.Context())
			if err != nil {
				return err
			}
			defer func() {
				_ = api.Close()
			}()

			namespace := strings.TrimSpace(args[0])
			if namespace == "" {
				return fmt.Errorf("service name is required")
			}

			deployments, err := api.ListDeployments(cmd.Context(), namespace)
			if err != nil {
				return err
			}
			if len(deployments) == 0 {
				return fmt.Errorf("service %q is not deployed", namespace)
			}

			latest := deployments[0]
			machines := strconv.Itoa(len(latest.MachineIDs))
			if strings.TrimSpace(latest.UpdatedAt) == "" {
				latest.UpdatedAt = "-"
			}
			owner := strings.TrimSpace(latest.Owner)
			if owner == "" {
				owner = "-"
			}

			fmt.Print(ui.KeyValues("",
				ui.KV("context", contextName),
				ui.KV("network", cl.Network),
				ui.KV("service", namespace),
				ui.KV("deployment", latest.ID),
				ui.KV("status", latest.Status),
				ui.KV("owner", owner),
				ui.KV("machines", machines),
				ui.KV("updated", latest.UpdatedAt),
			))

			states, err := api.ReadContainerState(cmd.Context(), namespace)
			if err != nil {
				return err
			}
			if len(states) == 0 {
				fmt.Println(ui.Muted("no local containers found for this service"))
				return nil
			}

			rows := make([][]string, 0, len(states))
			for _, state := range states {
				running := ui.Bool(state.Running)
				healthy := ui.Bool(state.Healthy)
				rows = append(rows, []string{state.ContainerName, state.Image, running, healthy})
			}

			fmt.Println(ui.Table([]string{"Container", "Image", "Running", "Healthy"}, rows))
			return nil
		},
	}

	cf.Bind(cmd)
	return cmd
}
