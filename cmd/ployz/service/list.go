package service

import (
	"errors"
	"fmt"
	"strconv"
	"strings"

	"ployz/cmd/ployz/cmdutil"
	"ployz/cmd/ployz/ui"
	"ployz/pkg/sdk/client"
	"ployz/pkg/sdk/types"

	"github.com/spf13/cobra"
)

func listCmd() *cobra.Command {
	var cf cmdutil.ContextFlags

	cmd := &cobra.Command{
		Use:     "list [service]",
		Aliases: []string{"ls"},
		Short:   "List services in the selected context",
		Args:    cobra.MaximumNArgs(1),
		RunE: func(cmd *cobra.Command, args []string) error {
			return deployUnavailableError()

			contextName, api, _, err := cf.DialService(cmd.Context())
			if err != nil {
				return err
			}
			defer func() {
				_ = api.Close()
			}()

			namespace := ""
			if len(args) == 1 {
				namespace = strings.TrimSpace(args[0])
				if namespace == "" {
					return fmt.Errorf("service name is required")
				}
			}

			rows, err := api.ListDeployments(cmd.Context(), namespace)
			if err != nil {
				if namespace == "" && errors.Is(err, client.ErrValidation) {
					return fmt.Errorf("list all services requires a newer daemon API; restart ployzd and retry, or run `ployz service list <service>`")
				}
				return err
			}

			latest := latestDeploymentsByNamespace(rows)
			if len(latest) == 0 {
				if namespace == "" {
					fmt.Println(ui.Muted("no services deployed"))
					return nil
				}
				fmt.Println(ui.Muted("service " + namespace + " is not deployed"))
				return nil
			}

			tableRows := make([][]string, 0, len(latest))
			for _, dep := range latest {
				owner := strings.TrimSpace(dep.Owner)
				if owner == "" {
					owner = "-"
				}
				updated := strings.TrimSpace(dep.UpdatedAt)
				if updated == "" {
					updated = "-"
				}
				machines := strconv.Itoa(len(dep.MachineIDs))
				tableRows = append(tableRows, []string{
					dep.Namespace,
					dep.Status,
					owner,
					dep.ID,
					machines,
					updated,
				})
			}

			fmt.Println(ui.InfoMsg("services in context %s", ui.Accent(contextName)))
			fmt.Println(ui.Table(
				[]string{"Service", "Status", "Owner", "Deployment", "Machines", "Updated"},
				tableRows,
			))
			return nil
		},
	}

	cf.Bind(cmd)
	return cmd
}

func latestDeploymentsByNamespace(rows []types.DeploymentEntry) []types.DeploymentEntry {
	if len(rows) == 0 {
		return nil
	}
	byNamespace := make(map[string]types.DeploymentEntry, len(rows))
	orderedNamespaces := make([]string, 0, len(rows))
	for _, row := range rows {
		namespace := strings.TrimSpace(row.Namespace)
		if namespace == "" {
			continue
		}
		if _, exists := byNamespace[namespace]; exists {
			continue
		}
		byNamespace[namespace] = row
		orderedNamespaces = append(orderedNamespaces, namespace)
	}
	out := make([]types.DeploymentEntry, 0, len(orderedNamespaces))
	for _, namespace := range orderedNamespaces {
		out = append(out, byNamespace[namespace])
	}
	return out
}
