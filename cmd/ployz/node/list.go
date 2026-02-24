package node

import (
	"context"
	"fmt"
	"strconv"
	"strings"

	"ployz/cmd/ployz/cmdutil"
	"ployz/cmd/ployz/ui"
	sdkmachine "ployz/pkg/sdk/machine"
	"ployz/pkg/sdk/types"

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

			status, statusErr := svc.Status(cmd.Context(), cl.Network)

			machines, err := svc.ListMachines(cmd.Context(), cl.Network)
			if err != nil {
				if statusErr != nil || status.Corrosion {
					return err
				}

				fallback, fallbackErr := fallbackLocalMachine(cmd.Context(), svc, cl.Network)
				if fallbackErr != nil {
					return err
				}
				machines = fallback
				fmt.Println(ui.WarnMsg("corrosion is unhealthy; showing local runtime only"))
				fmt.Println(ui.Muted("  " + err.Error()))
			} else if statusErr == nil && !status.Corrosion {
				fmt.Println(ui.WarnMsg("corrosion is unhealthy; membership data may be stale"))
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
				lag := "-"
				if m.ReplicationLag > 0 {
					lag = fmt.Sprintf("%.0fms", float64(m.ReplicationLag.Milliseconds()))
				}
				freshness := "-"
				if m.Freshness > 0 {
					freshness = fmt.Sprintf("%.0fms", float64(m.Freshness.Milliseconds()))
					if m.Stale {
						freshness = ui.Warn(freshness)
					}
				}
				rows[i] = []string{
					strconv.Itoa(i + 1),
					m.ID,
					m.Subnet,
					m.ManagementIP,
					m.Endpoint,
					version,
					lag,
					freshness,
					updated,
				}
			}

			fmt.Println(ui.Table(
				[]string{"#", "ID", "Subnet", "Management", "Endpoint", "Version", "Lag", "Freshness", "Updated"},
				rows,
			))
			return nil
		},
	}

	cf.Bind(cmd)
	return cmd
}

func fallbackLocalMachine(ctx context.Context, svc *sdkmachine.Service, network string) ([]types.MachineEntry, error) {
	identity, err := svc.Identity(ctx, network)
	if err != nil {
		return nil, err
	}

	entry, err := localMachineFromIdentity(identity)
	if err != nil {
		return nil, err
	}
	return []types.MachineEntry{entry}, nil
}

func localMachineFromIdentity(identity types.Identity) (types.MachineEntry, error) {
	id := strings.TrimSpace(identity.ID)
	if id == "" {
		id = strings.TrimSpace(identity.PublicKey)
	}
	if id == "" {
		return types.MachineEntry{}, fmt.Errorf("missing machine identity")
	}

	return types.MachineEntry{
		ID:           id,
		PublicKey:    strings.TrimSpace(identity.PublicKey),
		Subnet:       strings.TrimSpace(identity.Subnet),
		ManagementIP: strings.TrimSpace(identity.ManagementIP),
		Endpoint:     strings.TrimSpace(identity.AdvertiseEndpoint),
	}, nil
}
