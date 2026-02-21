//go:build linux

package docker

import (
	"fmt"
	"net/netip"

	"github.com/docker/docker/libnetwork/iptables"
)

func EnsureIptablesRules(subnet netip.Prefix, wgIface, bridge string) error {
	ipt := iptables.GetIptable(iptables.IPv4)
	wgRule := []string{"--in-interface", wgIface, "--out-interface", bridge, "-j", "ACCEPT"}
	if err := ipt.ProgramRule(iptables.Filter, "DOCKER-USER", iptables.Insert, wgRule); err != nil {
		return fmt.Errorf("insert DOCKER-USER rule: %w", err)
	}
	skipMasq := []string{"--src", subnet.String(), "--out-interface", wgIface, "-j", "RETURN"}
	_ = ipt.ProgramRule(iptables.Nat, "POSTROUTING", iptables.Delete, skipMasq)
	if err := ipt.ProgramRule(iptables.Nat, "POSTROUTING", iptables.Insert, skipMasq); err != nil {
		return fmt.Errorf("insert POSTROUTING rule: %w", err)
	}
	return nil
}

func CleanupIptablesRules(subnet, wgIface, bridge string) error {
	ipt := iptables.GetIptable(iptables.IPv4)
	wgRule := []string{"--in-interface", wgIface, "--out-interface", bridge, "-j", "ACCEPT"}
	if err := ipt.ProgramRule(iptables.Filter, "DOCKER-USER", iptables.Delete, wgRule); err != nil {
		return fmt.Errorf("delete DOCKER-USER rule: %w", err)
	}
	skipMasq := []string{"--src", subnet, "--out-interface", wgIface, "-j", "RETURN"}
	if err := ipt.ProgramRule(iptables.Nat, "POSTROUTING", iptables.Delete, skipMasq); err != nil {
		return fmt.Errorf("delete POSTROUTING rule: %w", err)
	}
	return nil
}
