//go:build linux

package configure

import (
	"context"
	"fmt"
	"os/exec"
	"strings"
)

const (
	dockerGroup    = "docker"
	daemonUserName = "ployzd"
)

func defaultEnsureDockerAccess(ctx context.Context) error {
	if exec.CommandContext(ctx, "getent", "group", dockerGroup).Run() != nil {
		return nil // docker group doesn't exist, skip
	}

	out, err := exec.CommandContext(ctx, "id", "-nG", daemonUserName).CombinedOutput()
	if err != nil {
		return fmt.Errorf("resolve groups for %q: %s: %w", daemonUserName, strings.TrimSpace(string(out)), err)
	}
	for _, group := range strings.Fields(strings.TrimSpace(string(out))) {
		if group == dockerGroup {
			return nil
		}
	}

	out, err = exec.CommandContext(ctx, "usermod", "-aG", dockerGroup, daemonUserName).CombinedOutput()
	if err != nil {
		return fmt.Errorf("add user %q to group %q: %s: %w", daemonUserName, dockerGroup, strings.TrimSpace(string(out)), err)
	}
	return nil
}
