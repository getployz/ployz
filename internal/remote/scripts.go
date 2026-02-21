package remote

import "strings"

func InstallScript(version string) string {
	return strings.Replace(installScript, "__PLOYZ_VERSION__", version, 1)
}
