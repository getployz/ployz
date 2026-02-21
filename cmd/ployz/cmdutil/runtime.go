package cmdutil

import "path/filepath"

func RuntimeLogPath(dataRoot string) string {
	return filepath.Join(dataRoot, "ployz-runtime.log")
}
