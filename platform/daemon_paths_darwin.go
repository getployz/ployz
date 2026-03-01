//go:build darwin

package platform

import (
	"os"
	"path/filepath"
)

var (
	DaemonSocketPath = "/tmp/ployzd.sock"
	DaemonDataRoot   = defaultDataRoot()
)

func defaultDataRoot() string {
	home, err := os.UserHomeDir()
	if err != nil {
		return "/usr/local/var/lib/ployz/networks"
	}
	return filepath.Join(home, "Library", "Application Support", "ployz", "networks")
}
