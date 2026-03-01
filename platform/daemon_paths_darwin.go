//go:build darwin

package platform

const (
	DaemonSocketPath = "/tmp/ployzd.sock"
	DaemonDataRoot   = "/usr/local/var/lib/ployz/networks"
)
