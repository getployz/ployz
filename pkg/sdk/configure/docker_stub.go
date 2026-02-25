//go:build !darwin && !linux

package configure

import "context"

func defaultEnsureDockerAccess(_ context.Context) error {
	return nil
}
