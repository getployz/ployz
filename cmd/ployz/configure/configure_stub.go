//go:build !darwin

package configure

import (
	"context"
	"fmt"
	"runtime"
)

func runConfigure(_ context.Context, _ string, _ string, _ int) error {
	return fmt.Errorf("configure is only supported on macOS (current: %s)", runtime.GOOS)
}
