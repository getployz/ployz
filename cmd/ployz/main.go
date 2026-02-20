package main

import (
	"fmt"
	"os"

	"github.com/spf13/cobra"
)

func main() {
	root := &cobra.Command{
		Use:   "ployz",
		Short: "Container orchestration with overlay networking",
	}

	root.AddCommand(machineCmd())
	root.AddCommand(hostCmd())
	root.AddCommand(controllerCmd())
	root.AddCommand(devCmd())
	root.AddCommand(configureCmd())

	if err := root.Execute(); err != nil {
		fmt.Fprintf(os.Stderr, "error: %v\n", err)
		os.Exit(1)
	}
}
