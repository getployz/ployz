//go:build darwin

package configure

import (
	"context"
	"errors"
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
)

const (
	dockerSocketPath = "/var/run/docker.sock"
	daemonUserName   = "ployzd"
)

func defaultEnsureDockerAccess(ctx context.Context) error {
	resolvedPath, err := filepath.EvalSymlinks(dockerSocketPath)
	if err != nil {
		if errors.Is(err, os.ErrNotExist) {
			return fmt.Errorf("docker socket %q not found; start Docker/OrbStack and retry", dockerSocketPath)
		}
		return fmt.Errorf("resolve docker socket %q: %w", dockerSocketPath, err)
	}

	info, err := os.Stat(resolvedPath)
	if err != nil {
		if errors.Is(err, os.ErrNotExist) {
			return fmt.Errorf("docker socket %q not found; start Docker/OrbStack and retry", resolvedPath)
		}
		return fmt.Errorf("stat docker socket %q: %w", resolvedPath, err)
	}
	if info.Mode()&os.ModeSocket == 0 {
		return fmt.Errorf("docker socket path %q is not a unix socket", resolvedPath)
	}

	aclOut, err := exec.CommandContext(ctx, "ls", "-le", resolvedPath).CombinedOutput()
	if err != nil {
		return fmt.Errorf("inspect docker socket permissions %q: %s: %w", resolvedPath, strings.TrimSpace(string(aclOut)), err)
	}
	aclMarker := "user:" + daemonUserName + " allow"
	if strings.Contains(string(aclOut), aclMarker) {
		return nil
	}

	aclRule := fmt.Sprintf("user:%s allow read,write", daemonUserName)
	chmodOut, err := exec.CommandContext(ctx, "chmod", "+a", aclRule, resolvedPath).CombinedOutput()
	if err != nil {
		return fmt.Errorf("grant %s access to docker socket %q: %s: %w", daemonUserName, resolvedPath, strings.TrimSpace(string(chmodOut)), err)
	}

	return nil
}
