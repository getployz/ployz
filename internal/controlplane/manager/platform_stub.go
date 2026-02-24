//go:build !darwin

package manager

import "context"

func startPlatformServices(context.Context) error {
	return nil
}
