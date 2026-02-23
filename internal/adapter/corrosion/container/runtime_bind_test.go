package container

import (
	"net/netip"
	"strings"
	"testing"
)

func TestValidateGossipBindAddrWithLocalAddrs(t *testing.T) {
	t.Run("address present", func(t *testing.T) {
		gossipAddr := netip.MustParseAddrPort("[fd8c:88ad:7f06::10]:53000")
		localAddrs := []netip.Addr{
			netip.MustParseAddr("10.210.0.1"),
			netip.MustParseAddr("fd8c:88ad:7f06::10"),
		}

		if err := validateGossipBindAddrWithLocalAddrs(gossipAddr, localAddrs); err != nil {
			t.Fatalf("validateGossipBindAddrWithLocalAddrs() error = %v", err)
		}
	})

	t.Run("address missing", func(t *testing.T) {
		gossipAddr := netip.MustParseAddrPort("[fd8c:88ad:7f06::99]:53000")
		localAddrs := []netip.Addr{
			netip.MustParseAddr("10.210.0.1"),
			netip.MustParseAddr("fd8c:88ad:7f06::10"),
		}

		err := validateGossipBindAddrWithLocalAddrs(gossipAddr, localAddrs)
		if err == nil {
			t.Fatal("validateGossipBindAddrWithLocalAddrs() expected error, got nil")
		}
		if !strings.Contains(err.Error(), "not assigned on this host") {
			t.Fatalf("validateGossipBindAddrWithLocalAddrs() error = %q, want missing-address message", err)
		}
	})
}
