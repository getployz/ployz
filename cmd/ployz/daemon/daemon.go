package daemon

import (
	"fmt"
	"os"
	"os/signal"
	"strconv"
	"syscall"

	"ployz/cmd/ployz/cmdutil"
	"ployz/cmd/ployz/ui"
	"ployz/internal/daemon/app"

	"github.com/spf13/cobra"
)

type options struct {
	socket   string
	dataRoot string
}

func Cmd() *cobra.Command {
	opts := &options{}

	cmd := &cobra.Command{
		Use:   "daemon",
		Short: "Manage local ployzd lifecycle",
	}

	cmd.PersistentFlags().StringVar(&opts.socket, "socket", cmdutil.DefaultSocketPath(), "ployzd unix socket path")
	cmd.PersistentFlags().StringVar(&opts.dataRoot, "data-root", cmdutil.DefaultDataRoot(), "Machine data root")

	cmd.AddCommand(runCmd(opts))
	cmd.AddCommand(startCmd(opts))
	cmd.AddCommand(stopCmd(opts))
	cmd.AddCommand(statusCmd(opts))
	return cmd
}

func runCmd(opts *options) *cobra.Command {
	return &cobra.Command{
		Use:   "run",
		Short: "Run ployzd in foreground",
		RunE: func(cmd *cobra.Command, args []string) error {
			ctx, cancel := signal.NotifyContext(cmd.Context(), os.Interrupt, syscall.SIGTERM)
			defer cancel()
			return app.Run(ctx, opts.socket, opts.dataRoot)
		},
	}
}

func startCmd(opts *options) *cobra.Command {
	return &cobra.Command{
		Use:   "start",
		Short: "Start ployzd in background",
		RunE: func(cmd *cobra.Command, args []string) error {
			pid, err := cmdutil.StartDaemon(cmd.Context(), opts.socket, opts.dataRoot)
			if err != nil {
				return err
			}
			fmt.Println(ui.SuccessMsg("started ployzd (pid %d)", pid))
			fmt.Print(ui.KeyValues("  ",
				ui.KV("socket", opts.socket),
				ui.KV("pid file", cmdutil.DaemonPIDPath(opts.dataRoot)),
				ui.KV("log", cmdutil.DaemonLogPath(opts.dataRoot)),
			))
			return nil
		},
	}
}

func stopCmd(opts *options) *cobra.Command {
	return &cobra.Command{
		Use:   "stop",
		Short: "Stop background ployzd",
		RunE: func(cmd *cobra.Command, args []string) error {
			pidPath := cmdutil.DaemonPIDPath(opts.dataRoot)
			_, running := cmdutil.ReadRunningPID(pidPath)
			if !running {
				_ = os.Remove(pidPath)
				fmt.Println(ui.InfoMsg("ployzd is not running"))
				return nil
			}

			if err := cmdutil.StopDaemon(cmd.Context(), opts.dataRoot); err != nil {
				return err
			}
			fmt.Println(ui.SuccessMsg("stopped ployzd"))
			return nil
		},
	}
}

func statusCmd(opts *options) *cobra.Command {
	return &cobra.Command{
		Use:   "status",
		Short: "Show daemon status",
		RunE: func(cmd *cobra.Command, args []string) error {
			pidPath := cmdutil.DaemonPIDPath(opts.dataRoot)
			pid, running := cmdutil.ReadRunningPID(pidPath)
			healthErr := cmdutil.HealthCheck(cmd.Context(), opts.socket)
			healthy := healthErr == nil

			pidText := "-"
			if running {
				pidText = strconv.Itoa(pid)
			}
			healthText := "down"
			if healthy {
				healthText = "ok"
			}

			fmt.Print(ui.KeyValues("",
				ui.KV("running", ui.Bool(running)),
				ui.KV("health", healthText),
				ui.KV("pid", pidText),
				ui.KV("socket", opts.socket),
				ui.KV("pid file", pidPath),
				ui.KV("log", cmdutil.DaemonLogPath(opts.dataRoot)),
			))
			if healthErr != nil {
				fmt.Println(ui.Muted("  health check: " + healthErr.Error()))
			}
			return nil
		},
	}
}
