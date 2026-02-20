package cmdutil

import (
	"context"
	"fmt"
	"os"
	"os/exec"
	"strings"

	"ployz/pkg/sdk/client"
	"ployz/pkg/sdk/cluster"
	"ployz/pkg/sdk/defaults"

	"github.com/spf13/cobra"
)

type NetworkFlags struct {
	Network     string
	DataRoot    string
	HelperImage string
}

func (f *NetworkFlags) Bind(cmd *cobra.Command) {
	def := currentDefaults()
	cmd.Flags().StringVar(&f.Network, "network", def.network, "Network identifier")
	cmd.Flags().StringVar(&f.DataRoot, "data-root", def.dataRoot, "Machine data root")
	cmd.Flags().StringVar(&f.HelperImage, "helper-image", "", "Linux helper image for macOS")
}

func DefaultSocketPath() string {
	def := currentDefaults()
	return def.socket
}

func DefaultDataRoot() string {
	def := currentDefaults()
	return def.dataRoot
}

func DefaultNetwork() string {
	def := currentDefaults()
	return def.network
}

func ResolveSocketPath(in string) (string, error) {
	if strings.TrimSpace(in) != "" {
		return strings.TrimSpace(in), nil
	}
	return currentDefaults().socket, nil
}

func SaveOrUpdateCurrentCluster(network, dataRoot, socketPath string) (string, error) {
	cfg, err := cluster.LoadDefault()
	if err != nil {
		return "", fmt.Errorf("read cluster config: %w", err)
	}

	network = strings.TrimSpace(network)
	if network == "" {
		network = "default"
	}
	dataRoot = strings.TrimSpace(dataRoot)
	if dataRoot == "" {
		dataRoot = defaults.DataRoot()
	}
	socketPath = strings.TrimSpace(socketPath)
	if socketPath == "" {
		socketPath = client.DefaultSocketPath()
	}

	name := strings.TrimSpace(cfg.CurrentCluster)
	if env := strings.TrimSpace(os.Getenv("PLOYZ_CLUSTER")); env != "" {
		name = env
	}
	if name == "" {
		name = network
	}

	entry, _ := cfg.Cluster(name)
	entry.Network = network
	entry.DataRoot = dataRoot
	entry.Socket = socketPath
	cfg.Upsert(name, entry)
	cfg.CurrentCluster = name

	if err := cfg.Save(); err != nil {
		return "", fmt.Errorf("save cluster config: %w", err)
	}
	return name, nil
}

type defaultsSnapshot struct {
	network  string
	dataRoot string
	socket   string
}

func currentDefaults() defaultsSnapshot {
	out := defaultsSnapshot{
		network:  "default",
		dataRoot: defaults.DataRoot(),
		socket:   client.DefaultSocketPath(),
	}

	cfg, err := cluster.LoadDefault()
	if err != nil {
		return out
	}
	_, cl, ok := cfg.Current()
	if !ok {
		return out
	}

	if n := strings.TrimSpace(cl.Network); n != "" {
		out.network = n
	}
	if d := strings.TrimSpace(cl.DataRoot); d != "" {
		out.dataRoot = d
	}
	if s := strings.TrimSpace(cl.Socket); s != "" {
		out.socket = s
	}
	return out
}

func RunSudo(ctx context.Context, name string, args ...string) error {
	if os.Geteuid() == 0 {
		cmd := exec.CommandContext(ctx, name, args...)
		out, err := cmd.CombinedOutput()
		if err != nil {
			msg := strings.TrimSpace(string(out))
			if msg == "" {
				return fmt.Errorf("%s failed: %w", name, err)
			}
			return fmt.Errorf("%s failed: %w: %s", name, err, msg)
		}
		return nil
	}

	all := append([]string{name}, args...)
	cmd := exec.CommandContext(ctx, "sudo", append([]string{"-n"}, all...)...)
	out, err := cmd.CombinedOutput()
	if err != nil {
		msg := strings.TrimSpace(string(out))
		if msg == "" {
			return fmt.Errorf("sudo %s failed: %w (run command with sudo privileges)", name, err)
		}
		return fmt.Errorf("sudo %s failed: %w: %s", name, err, msg)
	}
	return nil
}
