package remote

import (
	"context"
	"fmt"
	"os/exec"
	"strconv"
	"strings"
)

type SSHOptions struct {
	Port    int
	KeyPath string
}

func RunScript(ctx context.Context, target string, opts SSHOptions, script string) error {
	_, err := RunScriptOutput(ctx, target, opts, script)
	return err
}

func RunScriptOutput(ctx context.Context, target string, opts SSHOptions, script string) (string, error) {
	args := []string{"-o", "BatchMode=yes", "-o", "StrictHostKeyChecking=accept-new"}
	if opts.Port > 0 {
		args = append(args, "-p", strconv.Itoa(opts.Port))
	}
	if strings.TrimSpace(opts.KeyPath) != "" {
		args = append(args, "-i", opts.KeyPath)
	}
	args = append(args, target, "sh", "-s")

	cmd := exec.CommandContext(ctx, "ssh", args...)
	cmd.Stdin = strings.NewReader(script)
	out, err := cmd.CombinedOutput()
	if err != nil {
		output := strings.TrimSpace(string(out))
		if output == "" {
			return "", fmt.Errorf("ssh %s failed: %w", target, err)
		}
		return "", fmt.Errorf("ssh %s failed: %w: %s", target, err, output)
	}
	return strings.TrimSpace(string(out)), nil
}
