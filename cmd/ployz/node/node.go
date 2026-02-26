package node

import "github.com/spf13/cobra"

func Cmd() *cobra.Command {
	cmd := &cobra.Command{
		Use:     "node",
		Aliases: []string{"machine", "machines"},
		Short:   "Manage cluster nodes",
	}
	cmd.AddCommand(addCmd())
	cmd.AddCommand(listCmd())
	cmd.AddCommand(removeCmd())
	cmd.AddCommand(statusCmd())
	cmd.AddCommand(doctorCmd())
	cmd.AddCommand(lagCmd())
	return cmd
}

func AddCmd() *cobra.Command    { return addCmd() }
func ListCmd() *cobra.Command   { return listCmd() }
func RemoveCmd() *cobra.Command { return removeCmd() }
func StatusCmd() *cobra.Command { return statusCmd() }
func DoctorCmd() *cobra.Command { return doctorCmd() }
