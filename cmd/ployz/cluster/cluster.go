package cluster

import (
	"fmt"
	"strconv"
	"strings"

	"ployz/cmd/ployz/ui"
	"ployz/pkg/sdk/client"
	config "ployz/pkg/sdk/cluster"
	"ployz/pkg/sdk/defaults"

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
	cmd.AddCommand(setCmd())
	cmd.AddCommand(removeCmd())
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
		fmt.Println(ui.Muted("  run: ployz machine start --network <name>"))
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
		rows = append(rows, []string{
			name,
			current,
			valueOrDash(cl.Network),
			valueOrDash(cl.Socket),
			valueOrDash(cl.DataRoot),
		})
	}

	fmt.Println(ui.Table(
		[]string{"Name", "Current", "Network", "Socket", "Data Root"},
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
			fmt.Print(ui.KeyValues("",
				ui.KV("name", name),
				ui.KV("network", valueOrDefault(cl.Network, "default")),
				ui.KV("socket", valueOrDefault(cl.Socket, client.DefaultSocketPath())),
				ui.KV("data-root", valueOrDefault(cl.DataRoot, defaults.DataRoot())),
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
	var dataRoot string
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
				entry.Socket = strings.TrimSpace(socket)
			}
			if cmd.Flags().Changed("data-root") {
				entry.DataRoot = strings.TrimSpace(dataRoot)
			}

			if strings.TrimSpace(entry.Network) == "" {
				entry.Network = "default"
			}
			if strings.TrimSpace(entry.Socket) == "" {
				entry.Socket = client.DefaultSocketPath()
			}
			if strings.TrimSpace(entry.DataRoot) == "" {
				entry.DataRoot = defaults.DataRoot()
			}

			cfg.Upsert(name, entry)
			if setCurrent || strings.TrimSpace(cfg.CurrentCluster) == "" {
				cfg.CurrentCluster = name
			}

			if err := cfg.Save(); err != nil {
				return err
			}

			fmt.Println(ui.SuccessMsg("saved cluster %s", ui.Accent(name)))
			fmt.Print(ui.KeyValues("  ",
				ui.KV("network", entry.Network),
				ui.KV("socket", entry.Socket),
				ui.KV("data-root", entry.DataRoot),
				ui.KV("current", strconv.FormatBool(cfg.CurrentCluster == name)),
			))
			return nil
		},
	}

	cmd.Flags().StringVar(&network, "network", "", "Network name for this cluster")
	cmd.Flags().StringVar(&socket, "socket", "", "Daemon socket path for this cluster")
	cmd.Flags().StringVar(&dataRoot, "data-root", "", "Data root for this cluster")
	cmd.Flags().BoolVar(&setCurrent, "current", true, "Set this cluster as current")
	return cmd
}

func removeCmd() *cobra.Command {
	return &cobra.Command{
		Use:     "remove <name>",
		Aliases: []string{"rm", "delete"},
		Short:   "Remove a cluster profile",
		Args:    cobra.ExactArgs(1),
		RunE: func(cmd *cobra.Command, args []string) error {
			name := strings.TrimSpace(args[0])
			cfg, err := config.LoadDefault()
			if err != nil {
				return err
			}
			if _, ok := cfg.Cluster(name); !ok {
				return fmt.Errorf("cluster %q not found", name)
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

func valueOrDash(s string) string {
	s = strings.TrimSpace(s)
	if s == "" {
		return "-"
	}
	return s
}

func valueOrDefault(s, fallback string) string {
	s = strings.TrimSpace(s)
	if s == "" {
		return fallback
	}
	return s
}
