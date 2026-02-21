package main

import (
	"fmt"
	"time"

	"ployz/cmd/ployz/agent"
	"ployz/cmd/ployz/cmdutil"
	"ployz/cmd/ployz/ui"
	"ployz/pkg/sdk/client"
	"ployz/pkg/sdk/cluster"
	"ployz/pkg/sdk/defaults"
	sdkmachine "ployz/pkg/sdk/machine"
	"ployz/pkg/sdk/types"

	"github.com/spf13/cobra"
)

func initCmd() *cobra.Command {
	var (
		cidr              string
		advertiseEndpoint string
		wgPort            int
		dataRoot          string
		helperImage       string
		sshPort           int
		sshKey            string
		force             bool
	)

	cmd := &cobra.Command{
		Use:   "init [name] [user@host]",
		Short: "Bootstrap a cluster and install the agent",
		Long:  "Creates a new cluster, installs the agent if needed, and optionally adds a remote machine.",
		Args:  cobra.MaximumNArgs(2),
		RunE: func(cmd *cobra.Command, args []string) error {
			name := "default"
			if len(args) >= 1 {
				name = args[0]
			}

			if dataRoot == "" {
				dataRoot = defaults.DataRoot()
			}
			socketPath := client.DefaultSocketPath()

			cfg, err := cluster.LoadDefault()
			if err != nil {
				return err
			}
			if _, exists := cfg.Cluster(name); exists && !force {
				return fmt.Errorf("cluster %q already exists (use --force to re-create)", name)
			}

			fmt.Println(ui.InfoMsg("initializing cluster %s", ui.Accent(name)))

			if !cmdutil.IsDaemonRunning(cmd.Context(), socketPath) {
				platform := agent.NewPlatformService()
				if err := platform.Install(cmd.Context(), agent.InstallConfig{
					DataRoot:   dataRoot,
					SocketPath: socketPath,
				}); err != nil {
					return fmt.Errorf("install agent: %w", err)
				}
				if err := agent.WaitReady(cmd.Context(), socketPath, 15*time.Second); err != nil {
					return fmt.Errorf("agent not ready: %w", err)
				}
			}

			api, err := client.NewUnix(socketPath)
			if err != nil {
				return fmt.Errorf("connect to daemon: %w", err)
			}
			svc := sdkmachine.New(api)

			spec := types.NetworkSpec{
				Network:           name,
				DataRoot:          dataRoot,
				NetworkCIDR:       cidr,
				WGPort:            wgPort,
				AdvertiseEndpoint: advertiseEndpoint,
				HelperImage:       helperImage,
			}
			out, err := svc.Start(cmd.Context(), spec)
			if err != nil {
				return fmt.Errorf("apply network spec: %w", err)
			}

			entry := cluster.Cluster{
				Network: name,
				Connections: []cluster.Connection{{
					Unix:     socketPath,
					DataRoot: dataRoot,
				}},
			}
			cfg.Upsert(name, entry)
			cfg.CurrentCluster = name
			if err := cfg.Save(); err != nil {
				fmt.Println(ui.WarnMsg("could not save cluster config: %v", err))
			}

			kvs := []ui.Pair{
				ui.KV("network", out.Network),
				ui.KV("cidr", out.NetworkCIDR),
				ui.KV("subnet", out.Subnet),
				ui.KV("wireguard", out.WGInterface),
			}
			if out.AdvertiseEndpoint != "" {
				kvs = append(kvs, ui.KV("advertise", out.AdvertiseEndpoint))
			}
			fmt.Println(ui.SuccessMsg("cluster %s created", ui.Accent(name)))
			fmt.Print(ui.KeyValues("  ", kvs...))

			if len(args) >= 2 {
				target := args[1]
				fmt.Println(ui.InfoMsg("adding node %s", ui.Accent(target)))
				result, err := svc.AddMachine(cmd.Context(), sdkmachine.AddOptions{
					Network:  name,
					DataRoot: dataRoot,
					Target:   target,
					SSHPort:  sshPort,
					SSHKey:   sshKey,
					WGPort:   wgPort,
				})
				if err != nil {
					return fmt.Errorf("add node: %w", err)
				}

				cfg, _ = cluster.LoadDefault()
				entry, _ := cfg.Cluster(name)
				entry.Connections = append(entry.Connections, cluster.Connection{
					SSH:        target,
					SSHKeyFile: sshKey,
				})
				cfg.Upsert(name, entry)
				_ = cfg.Save()

				fmt.Println(ui.SuccessMsg("added node %s", ui.Accent(target)))
				fmt.Print(ui.KeyValues("  ",
					ui.KV("endpoint", result.Machine.Endpoint),
					ui.KV("subnet", result.Machine.Subnet),
					ui.KV("peers", fmt.Sprintf("%d", result.Peers)),
				))
			}

			return nil
		},
	}

	cmd.Flags().StringVar(&cidr, "cidr", "", "Network CIDR pool")
	cmd.Flags().StringVar(&advertiseEndpoint, "advertise-endpoint", "", "WireGuard endpoint (ip:port)")
	cmd.Flags().IntVar(&wgPort, "wg-port", 0, "WireGuard listen port")
	cmd.Flags().StringVar(&dataRoot, "data-root", "", "Machine data root")
	cmd.Flags().StringVar(&helperImage, "helper-image", "", "Linux helper image (macOS)")
	cmd.Flags().IntVar(&sshPort, "ssh-port", 22, "SSH port for remote")
	cmd.Flags().StringVar(&sshKey, "ssh-key", "", "SSH key for remote")
	cmd.Flags().BoolVar(&force, "force", false, "Re-create if cluster already exists")
	return cmd
}
