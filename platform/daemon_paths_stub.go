//go:build !linux && !darwin

package platform

const (
	DaemonSocketPath = "/var/run/ployzd.sock"
	DaemonDataRoot   = "/var/lib/ployz/networks"
)
