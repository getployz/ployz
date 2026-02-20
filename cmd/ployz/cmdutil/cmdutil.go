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

type ClusterFlags struct {
	Cluster string
}

func (f *ClusterFlags) Bind(cmd *cobra.Command) {
	cmd.Flags().StringVar(&f.Cluster, "cluster", "", "Cluster name (overrides current)")
}

func (f *ClusterFlags) Resolve() (string, cluster.Cluster, error) {
	cfg, err := cluster.LoadDefault()
	if err != nil {
		return "", cluster.Cluster{}, fmt.Errorf("load config: %w", err)
	}

	name := strings.TrimSpace(f.Cluster)
	if name == "" {
		name = strings.TrimSpace(os.Getenv("PLOYZ_CLUSTER"))
	}
	if name != "" {
		cl, ok := cfg.Cluster(name)
		if !ok {
			return "", cluster.Cluster{}, fmt.Errorf("cluster %q not found in config", name)
		}
		return name, cl, nil
	}

	n, cl, ok := cfg.Current()
	if !ok {
		return "", cluster.Cluster{}, fmt.Errorf("no cluster configured. Run 'ployz init' first.")
	}
	return n, cl, nil
}

func (f *ClusterFlags) DialService(ctx context.Context) (string, *client.Client, cluster.Cluster, error) {
	name, cl, err := f.Resolve()
	if err != nil {
		return "", nil, cluster.Cluster{}, err
	}
	api, err := cl.Dial(ctx)
	if err != nil {
		return "", nil, cluster.Cluster{}, fmt.Errorf("connect to cluster %q: %w", name, err)
	}
	return name, api, cl, nil
}

func DefaultSocketPath() string {
	cfg, err := cluster.LoadDefault()
	if err != nil {
		return client.DefaultSocketPath()
	}
	_, cl, ok := cfg.Current()
	if !ok {
		return client.DefaultSocketPath()
	}
	if s := cl.SocketPath(); s != "" {
		return s
	}
	return client.DefaultSocketPath()
}

func DefaultDataRoot() string {
	cfg, err := cluster.LoadDefault()
	if err != nil {
		return defaults.DataRoot()
	}
	_, cl, ok := cfg.Current()
	if !ok {
		return defaults.DataRoot()
	}
	if d := cl.DataRootFromConnections(); d != "" {
		return d
	}
	return defaults.DataRoot()
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
