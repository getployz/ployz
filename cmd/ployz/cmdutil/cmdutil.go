package cmdutil

import "runtime"

const (
	defaultSocketDarwin = "/tmp/ployzd.sock"
	defaultSocketLinux  = "/var/run/ployzd.sock"

	defaultDataRootDarwin = "/usr/local/var/lib/ployz/networks"
	defaultDataRootLinux  = "/var/lib/ployz/networks"
)

func DefaultSocketPath() string {
	if runtime.GOOS == "darwin" {
		return defaultSocketDarwin
	}
	return defaultSocketLinux
}

func DefaultDataRoot() string {
	if runtime.GOOS == "darwin" {
		return defaultDataRootDarwin
	}
	return defaultDataRootLinux
}