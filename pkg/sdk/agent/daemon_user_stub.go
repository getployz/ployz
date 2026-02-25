//go:build !linux && !darwin

package agent

import (
	"context"
	"fmt"
	"runtime"
)

func EnsureDaemonUser(_ context.Context, _ string) (int, int, error) {
	return 0, 0, fmt.Errorf("daemon user provisioning unsupported on %s", runtime.GOOS)
}
