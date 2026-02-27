package node

import "github.com/spf13/cobra"

func Cmd() *cobra.Command {
	cmd := &cobra.Command{
		Use:   "machine",
		Short: "Manage machines",
	}
	cmd.AddCommand(addCmd())
	cmd.AddCommand(listCmd())
	cmd.AddCommand(removeCmd())
	cmd.AddCommand(lagCmd())
	return cmd
}

func StatusCmd() *cobra.Command { return statusCmd() }
func DoctorCmd() *cobra.Command { return doctorCmd() }
