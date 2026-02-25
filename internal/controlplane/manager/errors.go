package manager

import "errors"

var (
	ErrNetworkNotConfigured       = errors.New("network is not configured")
	ErrRuntimeNotReadyForServices = errors.New("runtime is not ready for service operations")
)
