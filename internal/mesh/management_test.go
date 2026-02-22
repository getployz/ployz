package mesh

import (
	"net/netip"
	"testing"

	"golang.zx2c4.com/wireguard/wgctrl/wgtypes"
)

func TestManagementIPFromPublicKey(t *testing.T) {
	mgmtPrefix := netip.MustParsePrefix(ManagementCIDR)

	t.Run("valid key produces valid IPv6 in fd8c::/48", func(t *testing.T) {
		key, err := wgtypes.GeneratePrivateKey()
		if err != nil {
			t.Fatalf("GeneratePrivateKey() error = %v", err)
		}
		pubKey := key.PublicKey()

		addr, err := ManagementIPFromPublicKey(pubKey.String())
		if err != nil {
			t.Fatalf("ManagementIPFromPublicKey() error = %v", err)
		}
		if !addr.Is6() {
			t.Errorf("result %v is not IPv6", addr)
		}
		if !mgmtPrefix.Contains(addr) {
			t.Errorf("result %v not in %s", addr, ManagementCIDR)
		}
	})

	t.Run("invalid key returns error", func(t *testing.T) {
		_, err := ManagementIPFromPublicKey("not-a-valid-key")
		if err == nil {
			t.Fatal("ManagementIPFromPublicKey() expected error for invalid key")
		}
	})

	t.Run("empty key returns error", func(t *testing.T) {
		_, err := ManagementIPFromPublicKey("")
		if err == nil {
			t.Fatal("ManagementIPFromPublicKey() expected error for empty key")
		}
	})
}

func TestManagementIPFromWGKey(t *testing.T) {
	mgmtPrefix := netip.MustParsePrefix(ManagementCIDR)

	t.Run("known key produces expected IPv6", func(t *testing.T) {
		key, err := wgtypes.GeneratePrivateKey()
		if err != nil {
			t.Fatalf("GeneratePrivateKey() error = %v", err)
		}
		pubKey := key.PublicKey()

		addr := ManagementIPFromWGKey(pubKey)
		if !addr.IsValid() {
			t.Fatal("ManagementIPFromWGKey() returned invalid addr")
		}
		if !addr.Is6() {
			t.Errorf("result %v is not IPv6", addr)
		}

		// Same key always produces the same address.
		addr2 := ManagementIPFromWGKey(pubKey)
		if addr != addr2 {
			t.Errorf("ManagementIPFromWGKey() not deterministic: %v != %v", addr, addr2)
		}
	})

	t.Run("result is always in fd8c::/48", func(t *testing.T) {
		for i := range 10 {
			key, err := wgtypes.GeneratePrivateKey()
			if err != nil {
				t.Fatalf("GeneratePrivateKey() error = %v", err)
			}
			addr := ManagementIPFromWGKey(key.PublicKey())
			if !mgmtPrefix.Contains(addr) {
				t.Errorf("iteration %d: result %v not in %s", i, addr, ManagementCIDR)
			}
		}
	})
}
