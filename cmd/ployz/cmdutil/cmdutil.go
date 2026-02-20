package cmdutil

import (
	"context"
	"fmt"
	"os"
	"os/exec"
	"strings"

	"ployz/internal/machine"

	"github.com/spf13/cobra"
)

type NetworkFlags struct {
	Network     string
	DataRoot    string
	HelperImage string
}

func (f *NetworkFlags) Bind(cmd *cobra.Command) {
	cmd.Flags().StringVar(&f.Network, "network", "default", "Network identifier")
	cmd.Flags().StringVar(&f.DataRoot, "data-root", machine.DefaultDataRoot(), "Machine data root")
	cmd.Flags().StringVar(&f.HelperImage, "helper-image", "", "Linux helper image for macOS")
}

func (f *NetworkFlags) Config() machine.Config {
	return machine.Config{
		Network:     f.Network,
		DataRoot:    f.DataRoot,
		HelperImage: f.HelperImage,
	}
}

func ShellQuote(s string) string {
	return "'" + strings.ReplaceAll(s, "'", "'\"'\"'") + "'"
}

func RunDockerExecScript(ctx context.Context, containerName, script string) error {
	_, err := RunDockerExecScriptOutput(ctx, containerName, script)
	return err
}

func RunDockerExecScriptOutput(ctx context.Context, containerName, script string) (string, error) {
	cmd := exec.CommandContext(ctx, "docker", "exec", containerName, "sh", "-lc", script)
	out, err := cmd.CombinedOutput()
	if err != nil {
		msg := strings.TrimSpace(string(out))
		if msg == "" {
			return "", fmt.Errorf("docker exec %s: %w", containerName, err)
		}
		return "", fmt.Errorf("docker exec %s: %w: %s", containerName, err, msg)
	}
	return strings.TrimSpace(string(out)), nil
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
