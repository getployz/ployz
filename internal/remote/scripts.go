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
for c in ployzd docker ip wg iptables; do
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
pid_file="$data_root/ployzd.pid"
log_file="$data_root/ployzd.log"

if [ -S "$socket" ]; then
  exit 0
fi

$SUDO mkdir -p "$data_root"

if command -v systemctl >/dev/null 2>&1; then
  $SUDO systemctl start ployzd >/dev/null 2>&1 || true
fi

if [ ! -S "$socket" ]; then
  $SUDO sh -lc "nohup ployzd --socket %s --data-root %s > %s 2>&1 < /dev/null & echo \$! > %s"
fi

for i in $(seq 1 30); do
  if [ -S "$socket" ]; then
    exit 0
  fi
  sleep 1
done

echo "remote ployzd did not become ready at $socket" >&2
exit 1`, shellQuote(socketPath), shellQuote(dataRoot), shellQuote(socketPath), shellQuote(dataRoot), shellQuote(logFilePath(dataRoot)), shellQuote(pidFilePath(dataRoot)))) + "\n"
}

func logFilePath(dataRoot string) string {
	return strings.TrimRight(dataRoot, "/") + "/ployzd.log"
}

func pidFilePath(dataRoot string) string {
	return strings.TrimRight(dataRoot, "/") + "/ployzd.pid"
}

func shellQuote(s string) string {
	return "'" + strings.ReplaceAll(s, "'", "'\"'\"'") + "'"
}
