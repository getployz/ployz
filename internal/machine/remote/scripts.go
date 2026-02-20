package remote

import (
	"context"
	"fmt"
	"net/netip"
	"strconv"
	"strings"

	"ployz/internal/machine"
)

func PreflightScript() string {
	return strings.TrimSpace(`set -eu
if [ "$(uname -s)" != "Linux" ]; then
  echo "remote host must be Linux" >&2
  exit 1
fi
for c in ployz docker ip wg iptables; do
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

func StartScript(network string, plan machine.JoinPlan, remoteEP string, wgPort int) string {
	var b strings.Builder
	b.WriteString("set -eu\n")
	b.WriteString("SUDO=\"\"\n")
	b.WriteString("if [ \"$(id -u)\" -ne 0 ]; then\n")
	b.WriteString("  if ! command -v sudo >/dev/null 2>&1; then\n")
	b.WriteString("    echo \"sudo is required for non-root remote user\" >&2\n")
	b.WriteString("    exit 1\n")
	b.WriteString("  fi\n")
	b.WriteString("  SUDO=\"sudo\"\n")
	b.WriteString("fi\n")

	parts := []string{
		"${SUDO}", "ployz", "machine", "start",
		"--network", shellQuote(network),
		"--cidr", shellQuote(plan.NetworkCIDR.String()),
		"--subnet", shellQuote(plan.Subnet.String()),
		"--advertise-endpoint", shellQuote(remoteEP),
		"--wg-port", strconv.Itoa(wgPort),
	}
	for _, bs := range plan.Bootstrap {
		parts = append(parts, "--bootstrap", shellQuote(bs))
	}
	b.WriteString(strings.Join(parts, " "))
	b.WriteString("\n")
	return b.String()
}

func FetchWGPublicKey(ctx context.Context, target string, opts SSHOptions, network string) (string, error) {
	script := strings.TrimSpace(fmt.Sprintf(`set -eu
SUDO=""
if [ "$(id -u)" -ne 0 ]; then
  if ! command -v sudo >/dev/null 2>&1; then
    echo "sudo is required for non-root remote user" >&2
    exit 1
  fi
  SUDO="sudo"
fi
for i in $(seq 1 10); do
  out="$($SUDO ployz machine ls --network %s 2>&1 || true)"
  pub=$(printf '%%s\n' "$out" | awk '/^[0-9]+\)/ {print $2; exit}')
  if [ -n "$pub" ]; then
    printf '%%s\n' "$pub"
    exit 0
  fi
  sleep 1
done
echo "unable to read remote machine public key" >&2
exit 1`, shellQuote(network)))

	out, err := RunScriptOutput(ctx, target, opts, script)
	if err != nil {
		return "", err
	}
	pub := strings.TrimSpace(out)
	if pub == "" {
		return "", fmt.Errorf("remote machine returned empty wireguard public key")
	}
	return pub, nil
}

func WireGuardBootstrapScript(network, localPublicKey string, localSubnet netip.Prefix, localMgmt netip.Addr) string {
	iface := interfaceName(network)
	allowedIPs := localSubnet.String()
	localMgmtPrefix := ""
	if localMgmt.IsValid() {
		bits := 32
		if localMgmt.Is6() {
			bits = 128
		}
		localMgmtPrefix = netip.PrefixFrom(localMgmt, bits).String()
		allowedIPs += "," + localMgmtPrefix
	}
	mgmtRouteCmd := "ip route replace"
	if localMgmt.Is6() {
		mgmtRouteCmd = "ip -6 route replace"
	}
	return strings.TrimSpace(fmt.Sprintf(`set -eu
SUDO=""
if [ "$(id -u)" -ne 0 ]; then
  if ! command -v sudo >/dev/null 2>&1; then
    echo "sudo is required for non-root remote user" >&2
    exit 1
  fi
  SUDO="sudo"
fi
iface=%s
if ! $SUDO ip link show "$iface" >/dev/null 2>&1; then
  echo "wireguard interface $iface not found" >&2
  exit 1
fi
$SUDO wg set "$iface" peer %s persistent-keepalive 25 allowed-ips %s
$SUDO ip route replace %s dev "$iface"
%s`, shellQuote(iface), shellQuote(localPublicKey), shellQuote(allowedIPs), shellQuote(localSubnet.String()), wireGuardBootstrapManagementRouteLine(localMgmtPrefix, mgmtRouteCmd))) + "\n"
}

func ReconcileRetryScript(network string) string {
	return strings.TrimSpace(fmt.Sprintf(`set -eu
SUDO=""
if [ "$(id -u)" -ne 0 ]; then
  if ! command -v sudo >/dev/null 2>&1; then
    echo "sudo is required for non-root remote user" >&2
    exit 1
  fi
  SUDO="sudo"
fi
synced=0
for i in $(seq 1 20); do
  out="$($SUDO ployz machine ls --network %s 2>&1 || true)"
  if [ -n "$out" ]; then
    printf '%%s\n' "$out"
  fi
  count=$(printf '%%s\n' "$out" | awk '/^[0-9]+\)/ {c++} END {print c+0}')
  if [ "$count" -gt 1 ]; then
    synced=1
    break
  fi
  sleep 1
done
if [ "$synced" -eq 0 ]; then
  echo "warning: timed out waiting for Corrosion to sync other machines; keeping bootstrap peer config" >&2
  exit 0
fi
$SUDO ployz machine reconcile --network %s`, shellQuote(network), shellQuote(network))) + "\n"
}

func interfaceName(network string) string {
	name := "plz-" + strings.TrimSpace(network)
	if len(name) <= 15 {
		return name
	}
	return name[:15]
}

func wireGuardBootstrapManagementRouteLine(localMgmtPrefix, routeCmd string) string {
	if strings.TrimSpace(localMgmtPrefix) == "" {
		return "true"
	}
	return fmt.Sprintf(`$SUDO %s %s dev "$iface"`, routeCmd, shellQuote(localMgmtPrefix))
}

func shellQuote(s string) string {
	return "'" + strings.ReplaceAll(s, "'", "'\"'\"'") + "'"
}
