//go:build darwin

package wireguard

import (
	"context"
	"fmt"
	"net/netip"
	"strings"
)

// RunFunc executes a shell script in a privileged Linux context.
type RunFunc func(ctx context.Context, script string) error

func ConfigureWithHelper(ctx context.Context, run RunFunc, iface string, mtu int,
	privateKey string, port int, machineIP, mgmtIP netip.Addr, peers []PeerConfig) error {

	var script strings.Builder
	script.WriteString("set -eu\n")
	fmt.Fprintf(&script, "iface=%q\n", iface)
	fmt.Fprintf(&script, "priv=%q\n", privateKey)
	fmt.Fprintf(&script, "port=%d\n", port)
	fmt.Fprintf(&script, "machine_addr=%q\n", machineIP.String())
	fmt.Fprintf(&script, "machine_bits=%d\n", addrBits(machineIP))
	fmt.Fprintf(&script, "mgmt_addr=%q\n", mgmtIP.String())
	fmt.Fprintf(&script, "mgmt_bits=%d\n", addrBits(mgmtIP))
	script.WriteString("modprobe wireguard >/dev/null 2>&1 || true\n")
	script.WriteString("if ! ip link show \"$iface\" >/dev/null 2>&1; then\n")
	script.WriteString("  ip link add dev \"$iface\" type wireguard\n")
	script.WriteString("fi\n")
	fmt.Fprintf(&script, "ip link set dev \"$iface\" mtu %d\n", mtu)
	script.WriteString("tmp=$(mktemp)\n")
	script.WriteString("trap 'rm -f \"$tmp\"' EXIT\n")
	script.WriteString("printf '%s' \"$priv\" > \"$tmp\"\n")
	script.WriteString("wg set \"$iface\" listen-port \"$port\" private-key \"$tmp\"\n")

	script.WriteString("desired_keys=\"")
	for i, p := range peers {
		if i > 0 {
			script.WriteString(" ")
		}
		script.WriteString(p.PublicKey.String())
	}
	script.WriteString("\"\n")
	script.WriteString("for k in $(wg show \"$iface\" peers 2>/dev/null || true); do\n")
	script.WriteString("  keep=0\n")
	script.WriteString("  for d in $desired_keys; do\n")
	script.WriteString("    if [ \"$k\" = \"$d\" ]; then keep=1; break; fi\n")
	script.WriteString("  done\n")
	script.WriteString("  if [ \"$keep\" -eq 0 ]; then wg set \"$iface\" peer \"$k\" remove; fi\n")
	script.WriteString("done\n")

	for _, p := range peers {
		allowed := make([]string, len(p.AllowedPrefixes))
		for i, pref := range p.AllowedPrefixes {
			allowed[i] = pref.String()
		}
		allowedStr := strings.Join(allowed, ",")
		if p.Endpoint != nil {
			fmt.Fprintf(
				&script,
				"wg set \"$iface\" peer %q endpoint %q persistent-keepalive 25 allowed-ips %q\n",
				p.PublicKey.String(),
				p.Endpoint.String(),
				allowedStr,
			)
		} else {
			fmt.Fprintf(
				&script,
				"wg set \"$iface\" peer %q persistent-keepalive 25 allowed-ips %q\n",
				p.PublicKey.String(),
				allowedStr,
			)
		}
		for _, pref := range p.AllowedPrefixes {
			routeCmd := "ip route replace"
			if pref.Addr().Is6() {
				routeCmd = "ip -6 route replace"
			}
			fmt.Fprintf(&script, "%s %q dev \"$iface\"\n", routeCmd, pref.String())
		}
	}

	script.WriteString("ip addr replace \"$machine_addr/$machine_bits\" dev \"$iface\"\n")
	script.WriteString("ip addr replace \"$mgmt_addr/$mgmt_bits\" dev \"$iface\"\n")
	script.WriteString("ip link set up dev \"$iface\"\n")

	if err := run(ctx, script.String()); err != nil {
		return fmt.Errorf("configure wireguard through linux helper: %w", err)
	}
	return nil
}

func CleanupWithHelper(ctx context.Context, run RunFunc, iface string) error {
	script := fmt.Sprintf(`set -eu
iface=%q
ip link del dev "$iface" >/dev/null 2>&1 || true`, iface)
	if err := run(ctx, script); err != nil {
		return fmt.Errorf("cleanup wireguard through linux helper: %w", err)
	}
	return nil
}

func addrBits(addr netip.Addr) int {
	if addr.Is6() {
		return 128
	}
	return 32
}
