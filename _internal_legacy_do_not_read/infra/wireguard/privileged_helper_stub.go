//go:build !darwin && !linux

package wireguard

import (
	"context"
	"fmt"
	"runtime"
)

func RunPrivilegedHelper(_ context.Context, _ HelperConfig) error {
	return fmt.Errorf("privileged helper is only supported on linux and macOS (current: %s)", runtime.GOOS)
}
