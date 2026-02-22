//go:build !darwin

package supervisor

import "context"

func startPlatformServices(context.Context) error {
	return nil
}
