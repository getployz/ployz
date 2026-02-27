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

type ContextFlags struct {
	Context string
}

func (f *ContextFlags) Bind(cmd *cobra.Command) {
	cmd.Flags().StringVar(&f.Context, "context", "", "Context name (overrides current)")
}

func (f *ContextFlags) Resolve() (string, cluster.Cluster, error) {
	cfg, err := cluster.LoadDefault()
	if err != nil {
		return "", cluster.Cluster{}, fmt.Errorf("load config: %w", err)
	}

	name := strings.TrimSpace(f.Context)
	if name == "" {
		name = strings.TrimSpace(os.Getenv("PLOYZ_CONTEXT"))
	}
	if name != "" {
		cl, ok := cfg.Cluster(name)
		if !ok {
			return "", cluster.Cluster{}, fmt.Errorf("context %q not found in config", name)
		}
		return name, cl, nil
	}

	n, cl, ok := cfg.Current()
	if !ok {
		return "", cluster.Cluster{}, fmt.Errorf("no context configured. Run 'ployz network create default' first")
	}
	return n, cl, nil
}

func (f *ContextFlags) DialService(ctx context.Context) (string, *client.Client, cluster.Cluster, error) {
	name, cl, err := f.Resolve()
	if err != nil {
		return "", nil, cluster.Cluster{}, err
	}
	api, err := cl.Dial(ctx)
	if err != nil {
		return "", nil, cluster.Cluster{}, fmt.Errorf("connect to context %q: %w", name, err)
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
