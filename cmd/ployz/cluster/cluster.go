package cluster

import (
	"fmt"
	"strings"

	"ployz/cmd/ployz/ui"
	config "ployz/pkg/sdk/cluster"
	sdkmachine "ployz/pkg/sdk/machine"

	"github.com/spf13/cobra"
)

func Cmd() *cobra.Command {
	cmd := &cobra.Command{
		Use:     "cluster",
		Aliases: []string{"ctx", "context"},
		Short:   "Manage local cluster profiles",
		RunE: func(cmd *cobra.Command, args []string) error {
			return runList()
		},
	}

	cmd.AddCommand(listCmd())
	cmd.AddCommand(currentCmd())
	cmd.AddCommand(useCmd())
	cmd.AddCommand(removeCmd())

	set := setCmd()
	set.Hidden = true
	cmd.AddCommand(set)

	return cmd
}

func listCmd() *cobra.Command {
	return &cobra.Command{
		Use:     "list",
		Aliases: []string{"ls"},
		Short:   "List configured clusters",
		RunE: func(cmd *cobra.Command, args []string) error {
			return runList()
		},
	}
}

func runList() error {
	cfg, err := config.LoadDefault()
	if err != nil {
		return err
	}
	names := cfg.ClusterNames()
	if len(names) == 0 {
		fmt.Println(ui.Muted("no clusters configured"))
		fmt.Println(ui.Muted("  run: ployz init"))
		return nil
	}
	currentName, _, hasCurrent := cfg.Current()

	rows := make([][]string, 0, len(names))
	for _, name := range names {
		cl, _ := cfg.Cluster(name)
		current := ""
		if hasCurrent && currentName == name {
			current = "*"
		}
		network := strings.TrimSpace(cl.Network)
		if network == "" {
			network = "-"
		}
		connType := "-"
		if len(cl.Connections) > 0 {
			connType = cl.Connections[0].Type()
		}
		rows = append(rows, []string{
			name,
			current,
			network,
			connType,
		})
	}

	fmt.Println(ui.Table(
		[]string{"Name", "Current", "Network", "Connection"},
		rows,
	))
	fmt.Println(ui.Muted("config: " + cfg.Path()))
	return nil
}

func currentCmd() *cobra.Command {
	return &cobra.Command{
		Use:   "current",
		Short: "Show current cluster",
		RunE: func(cmd *cobra.Command, args []string) error {
			cfg, err := config.LoadDefault()
			if err != nil {
				return err
			}
			name, cl, ok := cfg.Current()
			if !ok {
				return fmt.Errorf("no current cluster configured")
			}
			connSummary := "-"
			if len(cl.Connections) > 0 {
				c := cl.Connections[0]
				switch {
				case c.Unix != "":
					connSummary = "unix:" + c.Unix
				case c.SSH != "":
					connSummary = "ssh:" + c.SSH
				}
			}
			network := strings.TrimSpace(cl.Network)
			if network == "" {
				network = "default"
			}
			fmt.Print(ui.KeyValues("",
				ui.KV("name", name),
				ui.KV("network", network),
				ui.KV("connection", connSummary),
			))
			return nil
		},
	}
}

func useCmd() *cobra.Command {
	return &cobra.Command{
		Use:   "use <name>",
		Short: "Switch current cluster",
		Args:  cobra.ExactArgs(1),
		RunE: func(cmd *cobra.Command, args []string) error {
			name := strings.TrimSpace(args[0])
			cfg, err := config.LoadDefault()
			if err != nil {
				return err
			}
			if _, ok := cfg.Cluster(name); !ok {
				return fmt.Errorf("cluster %q not found", name)
			}
			cfg.CurrentCluster = name
			if err := cfg.Save(); err != nil {
				return err
			}
			fmt.Println(ui.SuccessMsg("current cluster is now %s", ui.Accent(name)))
			return nil
		},
	}
}

func setCmd() *cobra.Command {
	var network string
	var socket string
	var setCurrent bool

	cmd := &cobra.Command{
		Use:   "set <name>",
		Short: "Create or update a cluster profile",
		Args:  cobra.ExactArgs(1),
		RunE: func(cmd *cobra.Command, args []string) error {
			name := strings.TrimSpace(args[0])
			if name == "" {
				return fmt.Errorf("cluster name is required")
			}

			cfg, err := config.LoadDefault()
			if err != nil {
				return err
			}

			entry, _ := cfg.Cluster(name)
			if cmd.Flags().Changed("network") {
				entry.Network = strings.TrimSpace(network)
			}
			if cmd.Flags().Changed("socket") {
				entry.Connections = []config.Connection{{Unix: strings.TrimSpace(socket)}}
			}

			if strings.TrimSpace(entry.Network) == "" {
				entry.Network = "default"
			}

			cfg.Upsert(name, entry)
			if setCurrent || strings.TrimSpace(cfg.CurrentCluster) == "" {
				cfg.CurrentCluster = name
			}

			if err := cfg.Save(); err != nil {
				return err
			}

			fmt.Println(ui.SuccessMsg("saved cluster %s", ui.Accent(name)))
			return nil
		},
	}

	cmd.Flags().StringVar(&network, "network", "", "Network name for this cluster")
	cmd.Flags().StringVar(&socket, "socket", "", "Daemon socket path for this cluster")
	cmd.Flags().BoolVar(&setCurrent, "current", true, "Set this cluster as current")
	return cmd
}

func removeCmd() *cobra.Command {
	return &cobra.Command{
		Use:     "remove <name>",
		Aliases: []string{"rm", "delete"},
		Short:   "Remove a cluster and tear down its runtime",
		Args:    cobra.ExactArgs(1),
		RunE: func(cmd *cobra.Command, args []string) error {
			name := strings.TrimSpace(args[0])
			cfg, err := config.LoadDefault()
			if err != nil {
				return err
			}
			cl, ok := cfg.Cluster(name)
			if !ok {
				return fmt.Errorf("cluster %q not found", name)
			}

			// Tear down runtime via daemon.
			api, dialErr := cl.Dial(cmd.Context())
			if dialErr == nil {
				svc := sdkmachine.New(api)
				if err := svc.Stop(cmd.Context(), cl.Network, true); err != nil {
					fmt.Println(ui.WarnMsg("could not stop runtime: %v", err))
				} else {
					fmt.Println(ui.SuccessMsg("tore down runtime for %s", ui.Accent(cl.Network)))
				}
				_ = api.Close()
			}

			cfg.Delete(name)
			if strings.TrimSpace(cfg.CurrentCluster) == name {
				names := cfg.ClusterNames()
				if len(names) > 0 {
					cfg.CurrentCluster = names[0]
				} else {
					cfg.CurrentCluster = ""
				}
			}
			if err := cfg.Save(); err != nil {
				return err
			}

			fmt.Println(ui.SuccessMsg("removed cluster %s", ui.Accent(name)))
			if strings.TrimSpace(cfg.CurrentCluster) != "" {
				fmt.Println(ui.Muted("  current cluster: " + cfg.CurrentCluster))
			}
			return nil
		},
	}
}

