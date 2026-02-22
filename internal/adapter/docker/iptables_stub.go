//go:build !linux

package docker

import "net/netip"

func EnsureIptablesRules(_ netip.Prefix, _, _ string) error { return nil }
func CleanupIptablesRules(_, _, _ string) error             { return nil }
