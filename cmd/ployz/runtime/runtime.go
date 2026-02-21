package runtimecmd

import (
	"fmt"
	"os"
	"os/signal"
	"strconv"
	"syscall"

	"ployz/cmd/ployz/cmdutil"
	"ployz/cmd/ployz/ui"
	"ployz/internal/runtime/engine"

	"github.com/spf13/cobra"
)

type options struct {
	dataRoot string
}

func Cmd() *cobra.Command {
	opts := &options{}

	cmd := &cobra.Command{
		Use:   "runtime",
		Short: "Manage local ployz runtime lifecycle",
	}

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
		Short: "Run ployz runtime in foreground",
		RunE: func(cmd *cobra.Command, args []string) error {
			ctx, cancel := signal.NotifyContext(cmd.Context(), os.Interrupt, syscall.SIGTERM)
			defer cancel()
			return engine.Run(ctx, opts.dataRoot)
		},
	}
}

func startCmd(opts *options) *cobra.Command {
	return &cobra.Command{
		Use:   "start",
		Short: "Start ployz runtime in background",
		RunE: func(cmd *cobra.Command, args []string) error {
			pid, err := cmdutil.StartRuntime(cmd.Context(), opts.dataRoot)
			if err != nil {
				return err
			}
			fmt.Println(ui.SuccessMsg("started ployz runtime (pid %d)", pid))
			fmt.Print(ui.KeyValues("  ",
				ui.KV("pid file", cmdutil.RuntimePIDPath(opts.dataRoot)),
				ui.KV("log", cmdutil.RuntimeLogPath(opts.dataRoot)),
			))
			return nil
		},
	}
}

func stopCmd(opts *options) *cobra.Command {
	return &cobra.Command{
		Use:   "stop",
		Short: "Stop background ployz runtime",
		RunE: func(cmd *cobra.Command, args []string) error {
			pidPath := cmdutil.RuntimePIDPath(opts.dataRoot)
			_, running := cmdutil.ReadRunningPID(pidPath)
			if !running {
				_ = os.Remove(pidPath)
				fmt.Println(ui.InfoMsg("ployz runtime is not running"))
				return nil
			}

			if err := cmdutil.StopRuntime(cmd.Context(), opts.dataRoot); err != nil {
				return err
			}
			fmt.Println(ui.SuccessMsg("stopped ployz runtime"))
			return nil
		},
	}
}

func statusCmd(opts *options) *cobra.Command {
	return &cobra.Command{
		Use:   "status",
		Short: "Show runtime status",
		RunE: func(cmd *cobra.Command, args []string) error {
			pidPath := cmdutil.RuntimePIDPath(opts.dataRoot)
			pid, running := cmdutil.ReadRunningPID(pidPath)

			pidText := "-"
			if running {
				pidText = strconv.Itoa(pid)
			}

			fmt.Print(ui.KeyValues("",
				ui.KV("running", ui.Bool(running)),
				ui.KV("pid", pidText),
				ui.KV("pid file", pidPath),
				ui.KV("log", cmdutil.RuntimeLogPath(opts.dataRoot)),
			))
			return nil
		},
	}
}
