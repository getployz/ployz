package cmdutil

import "ployz/platform"

func DefaultSocketPath() string {
	return platform.DaemonSocketPath
}

func DefaultDataRoot() string {
	return platform.DaemonDataRoot
}
