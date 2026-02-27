package initcmd

import (
	"context"
	"fmt"
	"strings"
	"time"

	"ployz/cmd/ployz/cmdutil"
	"ployz/cmd/ployz/ui"
	"ployz/pkg/sdk/agent"
	"ployz/pkg/sdk/client"
	"ployz/pkg/sdk/cluster"
	"ployz/pkg/sdk/defaults"
	sdkmachine "ployz/pkg/sdk/machine"
	"ployz/pkg/sdk/telemetry"
	"ployz/pkg/sdk/types"

	"github.com/spf13/cobra"
)

func Cmd() *cobra.Command {
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
		Short: "Bootstrap a network context and install the agent",
		Long:  "Creates or refreshes a context, installs the agent if needed, and optionally adds a remote machine.",
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

			telemetryOut := ui.NewTelemetryOutput()
			defer telemetryOut.Close()
			tracer := telemetryOut.Tracer("ployz/cmd/network-create")

			planSteps := []telemetry.PlannedStep{
				{ID: "validate", Title: "validating context setup"},
				{ID: "ensure_agent", Title: "ensuring local agent"},
				{ID: "connect_daemon", Title: "connecting to daemon"},
				{ID: "apply", Title: "applying network spec"},
				{ID: "apply/rpc", ParentID: "apply", Title: "submitting spec to daemon"},
				{ID: "apply/status", ParentID: "apply", Title: "probing daemon status"},
				{ID: "apply/diagnose", ParentID: "apply", Title: "evaluating runtime state machine"},
				{ID: "persist", Title: "saving context config"},
			}
			if len(args) >= 2 {
				planSteps = append(planSteps, telemetry.PlannedStep{ID: "add_machine", Title: "adding remote machine"})
			}

			op, err := telemetry.EmitPlan(cmd.Context(), tracer, "context.create", telemetry.Plan{Steps: planSteps})
			if err != nil {
				return err
			}
			var opErr error
			defer func() {
				op.End(opErr)
			}()

			var (
				cfg           *cluster.Config
				contextExists bool
			)
			opErr = op.RunStep(op.Context(), "validate", func(context.Context) error {
				loaded, loadErr := cluster.LoadDefault()
				if loadErr != nil {
					return loadErr
				}
				cfg = loaded
				_, contextExists = cfg.Cluster(name)
				return nil
			})
			if opErr != nil {
				return opErr
			}

			fmt.Println(ui.InfoMsg("initializing context %s", ui.Accent(name)))
			if contextExists && !force {
				fmt.Println(ui.Muted("  context already exists; refreshing network runtime and local context settings"))
			}

			opErr = op.RunStep(op.Context(), "ensure_agent", func(stepCtx context.Context) error {
				if cmdutil.IsDaemonRunning(stepCtx, socketPath) {
					return nil
				}

				platform := agent.NewPlatformService()
				if installErr := platform.Install(stepCtx, agent.InstallConfig{
					DataRoot:   dataRoot,
					SocketPath: socketPath,
				}); installErr != nil {
					return fmt.Errorf("install agent: %w", installErr)
				}
				if readyErr := agent.WaitReady(stepCtx, socketPath, 15*time.Second); readyErr != nil {
					return fmt.Errorf("agent not ready: %w", readyErr)
				}
				return nil
			})
			if opErr != nil {
				return opErr
			}

			var (
				api *client.Client
				svc *sdkmachine.Service
			)
			opErr = op.RunStep(op.Context(), "connect_daemon", func(context.Context) error {
				conn, connectErr := client.NewUnix(socketPath)
				if connectErr != nil {
					return fmt.Errorf("connect to daemon: %w", connectErr)
				}
				api = conn
				svc = sdkmachine.New(conn)
				return nil
			})
			if opErr != nil {
				return opErr
			}
			defer func() {
				if api != nil {
					_ = api.Close()
				}
			}()

			spec := types.NetworkSpec{
				Network:           name,
				DataRoot:          dataRoot,
				NetworkCIDR:       cidr,
				WGPort:            wgPort,
				AdvertiseEndpoint: advertiseEndpoint,
				HelperImage:       helperImage,
			}

			var (
				out       types.ApplyResult
				diag      sdkmachine.Diagnosis
				haveDiag  bool
				warnings  []string
				probeWait = 20 * time.Second
			)

			opErr = op.RunStep(op.Context(), "apply", func(applyCtx context.Context) error {
				if err := op.RunStep(applyCtx, "apply/rpc", func(stepCtx context.Context) error {
					applied, applyErr := svc.Start(stepCtx, spec)
					if applyErr != nil {
						return fmt.Errorf("apply network spec: %w", applyErr)
					}
					out = applied
					return nil
				}); err != nil {
					return err
				}

				if err := op.RunStep(applyCtx, "apply/status", func(stepCtx context.Context) error {
					probeCtx, cancel := context.WithTimeout(stepCtx, probeWait)
					defer cancel()
					ticker := time.NewTicker(500 * time.Millisecond)
					defer ticker.Stop()

					for {
						current, diagErr := svc.Diagnose(probeCtx)
						if diagErr == nil {
							diag = current
							haveDiag = true
							if diag.ControlPlaneReady() {
								return nil
							}
						}

						select {
						case <-probeCtx.Done():
							if !haveDiag {
								return fmt.Errorf("runtime diagnose probe timed out before daemon returned status")
							}
							warnings = append(warnings, "status probe timed out before runtime reached ready phase")
							return nil
						case <-ticker.C:
						}
					}
				}); err != nil {
					return err
				}

				if err := op.RunStep(applyCtx, "apply/diagnose", func(context.Context) error {
					for _, blocker := range diag.ControlPlaneBlockers {
						warnings = append(warnings, blocker.Component+": "+blocker.Message)
					}
					for _, warning := range diag.Warnings {
						warnings = append(warnings, warning.Component+": "+warning.Message)
					}
					if len(diag.ControlPlaneBlockers) > 0 {
						return fmt.Errorf("runtime not ready after apply: %s", strings.Join(warnings, "; "))
					}
					return nil
				}); err != nil {
					return err
				}

				return nil
			})
			if opErr != nil {
				return opErr
			}

			opErr = op.RunStep(op.Context(), "persist", func(context.Context) error {
				entry := cluster.Cluster{
					Network: name,
					Connections: []cluster.Connection{{
						Unix:     socketPath,
						DataRoot: dataRoot,
					}},
				}
				if existing, ok := cfg.Cluster(name); ok && !force {
					entry = mergeContextEntry(existing, entry)
				}
				cfg.Upsert(name, entry)
				cfg.CurrentCluster = name
				if saveErr := cfg.Save(); saveErr != nil {
					return fmt.Errorf("save context config: %w", saveErr)
				}
				return nil
			})
			if opErr != nil {
				return opErr
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
			fmt.Println(ui.SuccessMsg("context %s ready", ui.Accent(name)))
			fmt.Print(ui.KeyValues("  ", kvs...))
			for _, warning := range warnings {
				fmt.Println(ui.WarnMsg("%s", warning))
			}

			if len(args) >= 2 {
				target := args[1]
				fmt.Println(ui.InfoMsg("adding machine %s", ui.Accent(target)))
				opErr = op.RunStep(op.Context(), "add_machine", func(stepCtx context.Context) error {
					result, addErr := svc.AddMachine(stepCtx, sdkmachine.AddOptions{
						Network:  name,
						DataRoot: dataRoot,
						Target:   target,
						SSHPort:  sshPort,
						SSHKey:   sshKey,
						WGPort:   wgPort,
						Tracer:   tracer,
					})
					if addErr != nil {
						return fmt.Errorf("add machine: %w", addErr)
					}

					cfg, addLoadErr := cluster.LoadDefault()
					if addLoadErr != nil {
						return fmt.Errorf("load context config: %w", addLoadErr)
					}
					entry, ok := cfg.Cluster(name)
					if !ok {
						return fmt.Errorf("context %q not found in config", name)
					}
					entry.Connections = append(entry.Connections, cluster.Connection{
						SSH:        target,
						SSHKeyFile: sshKey,
					})
					cfg.Upsert(name, entry)
					if saveErr := cfg.Save(); saveErr != nil {
						return fmt.Errorf("save context config: %w", saveErr)
					}

					fmt.Println(ui.SuccessMsg("added machine %s", ui.Accent(target)))
					fmt.Print(ui.KeyValues("  ",
						ui.KV("endpoint", result.Machine.Endpoint),
						ui.KV("subnet", result.Machine.Subnet),
						ui.KV("peers", fmt.Sprintf("%d", result.Peers)),
					))
					return nil
				})
				if opErr != nil {
					return opErr
				}
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
	cmd.Flags().BoolVar(&force, "force", false, "Re-create context entry and replace local connections")
	return cmd
}

func mergeContextEntry(existing, preferred cluster.Cluster) cluster.Cluster {
	merged := cluster.Cluster{Network: preferred.Network}
	merged.Connections = make([]cluster.Connection, 0, len(existing.Connections)+len(preferred.Connections))
	merged.Connections = append(merged.Connections, preferred.Connections...)
	for _, conn := range existing.Connections {
		if strings.TrimSpace(conn.Unix) != "" {
			continue
		}
		merged.Connections = append(merged.Connections, conn)
	}
	return merged
}
