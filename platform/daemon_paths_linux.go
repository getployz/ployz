//go:build linux

package platform

const (
	DaemonSocketPath = "/var/run/ployzd.sock"
	DaemonDataRoot   = "/var/lib/ployz/networks"
)
