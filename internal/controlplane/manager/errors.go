package manager

import "errors"

var (
	ErrNetworkNotConfigured       = errors.New("network is not configured")
	ErrRuntimeNotReadyForServices = errors.New("runtime is not ready for service operations")
	ErrNetworkDestroyHasWorkloads = errors.New("network destroy blocked by managed workloads")
	ErrNetworkDestroyHasMachines  = errors.New("network destroy blocked by attached machines")
)
