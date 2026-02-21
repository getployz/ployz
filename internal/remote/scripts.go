package remote

import (
	"fmt"
	"strings"
)

func PreflightScript() string {
	return strings.TrimSpace(`set -eu
if [ "$(uname -s)" != "Linux" ]; then
  echo "remote host must be Linux" >&2
  exit 1
fi
for c in ployzd ployz-runtime docker ip wg iptables systemctl; do
  if ! command -v "$c" >/dev/null 2>&1; then
    echo "missing prerequisite: $c" >&2
    exit 1
  fi
done
if ! docker info >/dev/null 2>&1; then
  echo "docker daemon is not running or accessible" >&2
  exit 1
fi`) + "\n"
}

func EnsureDaemonScript(socketPath, dataRoot string) string {
	if strings.TrimSpace(socketPath) == "" {
		socketPath = "/var/run/ployzd.sock"
	}
	if strings.TrimSpace(dataRoot) == "" {
		dataRoot = "/var/lib/ployz/networks"
	}

	return strings.TrimSpace(fmt.Sprintf(`set -eu
SUDO=""
if [ "$(id -u)" -ne 0 ]; then
  if ! command -v sudo >/dev/null 2>&1; then
    echo "sudo is required for non-root remote user" >&2
    exit 1
  fi
  sudo -n true >/dev/null 2>&1 || {
    echo "passwordless sudo is required for remote daemon bootstrap" >&2
    exit 1
  }
  SUDO="sudo"
fi

PATH="/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin:$PATH"

socket=%s
data_root=%s

if [ "$socket" != "/var/run/ployzd.sock" ]; then
  echo "remote service mode currently requires socket /var/run/ployzd.sock" >&2
  exit 1
fi
if [ "$data_root" != "/var/lib/ployz/networks" ]; then
  echo "remote service mode currently requires data root /var/lib/ployz/networks" >&2
  exit 1
fi

if [ -S "$socket" ]; then
  exit 0
fi

$SUDO mkdir -p "$data_root"

if ! command -v systemctl >/dev/null 2>&1; then
  echo "systemctl is required on remote host" >&2
  exit 1
fi

for unit in ployzd.service ployz-runtime.service; do
  if ! $SUDO systemctl list-unit-files "$unit" >/dev/null 2>&1; then
    echo "required service unit $unit is not installed" >&2
    exit 1
  fi
  $SUDO systemctl enable --now "$unit"
done

if ! $SUDO systemctl is-active --quiet ployzd.service; then
  echo "ployzd.service is not active after start" >&2
  exit 1
fi
if ! $SUDO systemctl is-active --quiet ployz-runtime.service; then
  echo "ployz-runtime.service is not active after start" >&2
  exit 1
fi

for i in $(seq 1 30); do
  if [ -S "$socket" ]; then
    exit 0
  fi
  sleep 1
done

echo "remote ployzd did not become ready at $socket" >&2
exit 1`, shellQuote(socketPath), shellQuote(dataRoot))) + "\n"
}

func shellQuote(s string) string {
	return "'" + strings.ReplaceAll(s, "'", "'\"'\"'") + "'"
}
