//go:build darwin

package wireguard

import (
	"context"
	"errors"
	"fmt"
	"log/slog"
	"net/netip"
	"os"
	"strings"
	"sync"

	"ployz/internal/mesh"

	"golang.org/x/sys/unix"
	"golang.zx2c4.com/wireguard/conn"
	"golang.zx2c4.com/wireguard/device"
	"golang.zx2c4.com/wireguard/tun"
	"golang.zx2c4.com/wireguard/wgctrl/wgtypes"
)

var (
	activeMu         sync.Mutex
	activeSession    *darwinSession
	provisionedState *provisionedTUN

	errNoProvisionedTUN = errors.New("no provisioned macOS tun")
)

// Configure creates (or reuses) a userspace WireGuard TUN device on macOS,
// configures the private key, listen port, peers, interface addresses and routes.
func Configure(ctx context.Context, iface string, mtu int, privateKey string,
	port int, machineIP, mgmtIP netip.Addr, peers []PeerConfig) error {

	log := slog.With("component", "wireguard-darwin")

	priv, err := wgtypes.ParseKey(privateKey)
	if err != nil {
		return fmt.Errorf("parse private key: %w", err)
	}

	activeMu.Lock()
	defer activeMu.Unlock()

	// Build IPC config.
	ipcConf := buildIPC(priv, port, peers)

	if activeSession != nil {
		// Reuse existing TUN — just update WireGuard config.
		log.Debug("updating existing device", "iface", activeSession.ifaceName)
		if err := activeSession.dev.IpcSet(ipcConf); err != nil {
			return fmt.Errorf("update wireguard config: %w", err)
		}
		if err := syncRoutes(ctx, activeSession.ifaceName, machineIP, mgmtIP, peers); err != nil {
			return err
		}
		return nil
	}

	// Create userspace WireGuard device from provisioned TUN fd.
	tunDev, tunName, err := cloneProvisionedTUN()
	if err != nil {
		if errors.Is(err, errNoProvisionedTUN) {
			return fmt.Errorf("macOS TUN not configured; run `sudo ployz configure` first")
		}
		return fmt.Errorf("prepare provisioned TUN: %w", err)
	}
	log.Debug("using provisioned TUN", "iface", tunName, "requested_iface", iface, "mtu", mtu)

	// Create WireGuard device.
	dev := device.NewDevice(tunDev, conn.NewDefaultBind(), device.NewLogger(device.LogLevelSilent, ""))
	if err := dev.IpcSet(ipcConf); err != nil {
		dev.Close()
		return fmt.Errorf("configure wireguard device: %w", err)
	}
	if err := dev.Up(); err != nil {
		dev.Close()
		return fmt.Errorf("bring up wireguard device: %w", err)
	}

	// Configure interface addresses.
	if err := configureInterface(ctx, tunName, machineIP, mgmtIP); err != nil {
		dev.Close()
		return err
	}

	// Determine network CIDR for route (use the subnet containing machineIP).
	// We route the whole /24 (or whatever the peer subnets imply) via this interface.
	// For now, add individual peer routes.
	if err := syncRoutes(ctx, tunName, machineIP, mgmtIP, peers); err != nil {
		dev.Close()
		return err
	}

	activeSession = &darwinSession{
		dev:       dev,
		tun:       tunDev,
		ifaceName: tunName,
		mgmtCIDR:  mesh.ManagementCIDR,
	}

	log.Debug("wireguard active", "iface", tunName)
	return nil
}

// Cleanup tears down the active WireGuard TUN device and removes routes.
func Cleanup(ctx context.Context) error {
	activeMu.Lock()
	defer activeMu.Unlock()

	s := activeSession
	if s == nil {
		return nil
	}
	activeSession = nil

	log := slog.With("component", "wireguard-darwin")
	log.Debug("tearing down", "iface", s.ifaceName)

	// Remove routes (best-effort).
	if s.routeCIDR != "" {
		_, _ = runPrivilegedCommand(ctx, "route", "-n", "delete", "-net", s.routeCIDR, "-interface", s.ifaceName)
	}
	_, _ = runPrivilegedCommand(ctx, "route", "-n", "delete", "-inet6", s.mgmtCIDR, "-interface", s.ifaceName)

	if s.dev != nil {
		s.dev.Close()
	}
	log.Debug("stopped")
	return nil
}

// IsActive returns true if a userspace WireGuard device is currently running.
func IsActive() bool {
	activeMu.Lock()
	defer activeMu.Unlock()
	return activeSession != nil
}

type darwinSession struct {
	dev       *device.Device
	tun       tun.Device
	ifaceName string
	routeCIDR string
	mgmtCIDR  string
}

type provisionedTUN struct {
	file      *os.File
	ifaceName string
	mtu       int
}

func installProvisionedTUN(file *os.File, ifaceName string, mtu int) error {
	if file == nil {
		return fmt.Errorf("tun file descriptor is required")
	}
	ifaceName = strings.TrimSpace(ifaceName)
	if ifaceName == "" {
		return fmt.Errorf("tun interface name is required")
	}

	activeMu.Lock()
	prev := provisionedState
	provisionedState = &provisionedTUN{
		file:      file,
		ifaceName: ifaceName,
		mtu:       mtu,
	}
	activeMu.Unlock()

	if prev != nil && prev.file != nil {
		_ = prev.file.Close()
	}

	return nil
}

// cloneProvisionedTUN duplicates the provisioned utun descriptor.
// Caller must hold activeMu.
func cloneProvisionedTUN() (tun.Device, string, error) {
	state := provisionedState
	if state == nil || state.file == nil {
		return nil, "", errNoProvisionedTUN
	}
	dupFD, err := unix.Dup(int(state.file.Fd()))
	ifaceName := state.ifaceName
	mtu := state.mtu
	if err != nil {
		return nil, "", fmt.Errorf("duplicate tun descriptor: %w", err)
	}

	file := os.NewFile(uintptr(dupFD), ifaceName)
	if file == nil {
		_ = unix.Close(dupFD)
		return nil, "", fmt.Errorf("wrap tun descriptor")
	}

	tunDev := newFDTUN(file, ifaceName, mtu)
	tunName, err := tunDev.Name()
	if err != nil {
		_ = tunDev.Close()
		return nil, "", fmt.Errorf("get tun name: %w", err)
	}

	return tunDev, tunName, nil
}

func buildIPC(priv wgtypes.Key, port int, peers []PeerConfig) string {
	var b strings.Builder
	fmt.Fprintf(&b, "private_key=%x\nlisten_port=%d\nreplace_peers=true\n", priv[:], port)
	for _, p := range peers {
		fmt.Fprintf(&b, "public_key=%x\n", p.PublicKey[:])
		if p.Endpoint != nil {
			fmt.Fprintf(&b, "endpoint=%s\n", p.Endpoint.String())
		}
		for _, prefix := range p.AllowedPrefixes {
			fmt.Fprintf(&b, "allowed_ip=%s\n", prefix.String())
		}
		fmt.Fprintf(&b, "persistent_keepalive_interval=25\n")
	}
	return b.String()
}

func configureInterface(ctx context.Context, iface string, machineIP, mgmtIP netip.Addr) error {
	// Assign IPv4 address (point-to-point style on macOS).
	ipStr := machineIP.String()
	if out, err := runPrivilegedCommand(ctx, "ifconfig", iface, "inet", ipStr, ipStr, "up"); err != nil {
		return fmt.Errorf("configure %s address %s: %w: %s", iface, ipStr, err, strings.TrimSpace(string(out)))
	}

	// Assign IPv6 management address.
	if mgmtIP.IsValid() && mgmtIP.Is6() {
		if out, err := runPrivilegedCommand(ctx, "ifconfig", iface, "inet6", mgmtIP.String(), "prefixlen", "128"); err != nil {
			slog.Debug("assign IPv6 management address failed (non-fatal)", "addr", mgmtIP, "err", err, "output", strings.TrimSpace(string(out)))
		}
	}

	return nil
}

func syncRoutes(ctx context.Context, iface string, machineIP, mgmtIP netip.Addr, peers []PeerConfig) error {
	// Add routes for each peer's allowed prefixes.
	for _, p := range peers {
		for _, prefix := range p.AllowedPrefixes {
			cidr := prefix.String()
			if prefix.Addr().Is4() {
				_, _ = runPrivilegedCommand(ctx, "route", "-n", "delete", "-net", cidr, "-interface", iface)
				if out, err := runPrivilegedCommand(ctx, "route", "-n", "add", "-net", cidr, "-interface", iface); err != nil {
					return fmt.Errorf("add route %s via %s: %w: %s", cidr, iface, err, strings.TrimSpace(string(out)))
				}
			} else {
				_, _ = runPrivilegedCommand(ctx, "route", "-n", "delete", "-inet6", cidr, "-interface", iface)
				if out, err := runPrivilegedCommand(ctx, "route", "-n", "add", "-inet6", cidr, "-interface", iface); err != nil {
					slog.Debug("IPv6 route failed (non-fatal)", "cidr", cidr, "err", err, "output", strings.TrimSpace(string(out)))
				}
			}
		}
	}

	// Management CIDR route.
	_, _ = runPrivilegedCommand(ctx, "route", "-n", "delete", "-inet6", mesh.ManagementCIDR, "-interface", iface)
	if out, err := runPrivilegedCommand(ctx, "route", "-n", "add", "-inet6", mesh.ManagementCIDR, "-interface", iface); err != nil {
		slog.Debug("IPv6 management route failed (non-fatal)", "err", err, "output", strings.TrimSpace(string(out)))
	}

	return nil
}
