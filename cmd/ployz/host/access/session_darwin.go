//go:build darwin

package access

import (
	"context"
	"errors"
	"fmt"
	"net/netip"
	"strings"

	"ployz/cmd/ployz/cmdutil"

	"golang.zx2c4.com/wireguard/conn"
	"golang.zx2c4.com/wireguard/device"
	"golang.zx2c4.com/wireguard/tun"
	"golang.zx2c4.com/wireguard/wgctrl/wgtypes"
)

type darwinSession struct {
	dev       *device.Device
	tun       tun.Device
	ifaceName string
	routeCIDR string
}

func startSession(
	ctx context.Context,
	network string,
	privateKey string,
	hostIP netip.Addr,
	helperPublicKey string,
	helperEndpoint netip.AddrPort,
	allowedCIDR string,
) (session, error) {
	localKey, err := wgtypes.ParseKey(privateKey)
	if err != nil {
		return nil, fmt.Errorf("parse host private key: %w", err)
	}
	peerKey, err := wgtypes.ParseKey(helperPublicKey)
	if err != nil {
		return nil, fmt.Errorf("parse helper public key: %w", err)
	}

	tunDev, err := tun.CreateTUN("utun", device.DefaultMTU)
	if err != nil {
		return nil, fmt.Errorf("create host tunnel interface: %w", err)
	}

	iface, err := tunDev.Name()
	if err != nil {
		tunDev.Close()
		return nil, fmt.Errorf("read host tunnel interface name: %w", err)
	}

	dev := device.NewDevice(tunDev, conn.NewDefaultBind(), device.NewLogger(device.LogLevelSilent, ""))
	conf := fmt.Sprintf(
		"private_key=%s\n"+
			"public_key=%s\n"+
			"endpoint=%s\n"+
			"allowed_ip=%s\n"+
			"persistent_keepalive_interval=25\n",
		fmt.Sprintf("%x", localKey[:]),
		fmt.Sprintf("%x", peerKey[:]),
		helperEndpoint.String(),
		allowedCIDR,
	)
	if err := dev.IpcSet(conf); err != nil {
		dev.Close()
		tunDev.Close()
		return nil, fmt.Errorf("configure host wireguard device: %w", err)
	}
	if err := dev.Up(); err != nil {
		dev.Close()
		tunDev.Close()
		return nil, fmt.Errorf("enable host wireguard device: %w", err)
	}

	if err := setInterfaceAddress(ctx, iface, hostIP); err != nil {
		dev.Close()
		tunDev.Close()
		return nil, err
	}

	_ = cmdutil.RunSudo(ctx, "route", "-n", "delete", "-net", allowedCIDR, "-interface", iface)
	if err := cmdutil.RunSudo(ctx, "route", "-n", "add", "-net", allowedCIDR, "-interface", iface); err != nil {
		dev.Close()
		tunDev.Close()
		return nil, fmt.Errorf("add route %s via %s: %w", allowedCIDR, iface, err)
	}

	return &darwinSession{
		dev:       dev,
		tun:       tunDev,
		ifaceName: iface,
		routeCIDR: allowedCIDR,
	}, nil
}

func setInterfaceAddress(ctx context.Context, iface string, ip netip.Addr) error {
	ipStr := ip.String()
	attempts := [][]string{
		{"ifconfig", iface, "inet", ipStr, ipStr, "up"},
		{"ifconfig", iface, "inet", ipStr, ipStr, "alias", "up"},
	}

	var lastErr error
	for _, args := range attempts {
		if err := cmdutil.RunSudo(ctx, args[0], args[1:]...); err == nil {
			return nil
		} else {
			lastErr = err
		}
	}

	if lastErr == nil {
		lastErr = fmt.Errorf("unknown error")
	}
	return fmt.Errorf("configure host interface %s address %s: %w", iface, ipStr, lastErr)
}

func (s *darwinSession) InterfaceName() string {
	return s.ifaceName
}

func (s *darwinSession) Close(ctx context.Context) error {
	var errs []error
	if strings.TrimSpace(s.routeCIDR) != "" {
		if err := cmdutil.RunSudo(ctx, "route", "-n", "delete", "-net", s.routeCIDR, "-interface", s.ifaceName); err != nil {
			errs = append(errs, err)
		}
	}
	if s.dev != nil {
		s.dev.Close()
		s.dev = nil
	}
	if s.tun != nil {
		_ = s.tun.Close()
		s.tun = nil
	}
	if len(errs) > 0 {
		return errors.Join(errs...)
	}
	return nil
}
