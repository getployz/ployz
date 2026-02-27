package wireguard

import "runtime"

const defaultTUNSocketPath = "/tmp/ployz-tun.sock"

const (
	defaultPrivilegedSocketPathDarwin = "/tmp/ployz-priv.sock"
	defaultPrivilegedSocketPathLinux  = "/run/ployz/helper.sock"

	defaultPrivilegedTokenPathDarwin = "/var/db/ployz/private/helper.token"
	defaultPrivilegedTokenPathLinux  = "/var/lib/ployz/private/helper.token"
)

func DefaultTUNSocketPath() string {
	return defaultTUNSocketPath
}

func DefaultPrivilegedSocketPath() string {
	if runtime.GOOS == "linux" {
		return defaultPrivilegedSocketPathLinux
	}
	return defaultPrivilegedSocketPathDarwin
}

func DefaultPrivilegedTokenPath() string {
	if runtime.GOOS == "linux" {
		return defaultPrivilegedTokenPathLinux
	}
	return defaultPrivilegedTokenPathDarwin
}
