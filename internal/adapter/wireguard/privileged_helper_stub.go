//go:build !darwin

package wireguard

import (
	"context"
	"fmt"
	"runtime"
)

func RunPrivilegedHelper(_ context.Context, _ string, _ string) error {
	return fmt.Errorf("privileged helper is only supported on macOS (current: %s)", runtime.GOOS)
}
