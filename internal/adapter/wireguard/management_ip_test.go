package wireguard

import (
	"net/netip"
	"testing"

	"golang.zx2c4.com/wireguard/wgctrl/wgtypes"
)

func TestMigrateLegacyManagementAddr(t *testing.T) {
	legacy := netip.MustParseAddr("fdcc:3992:6d25:277e:9360:5cab:232b:3f19")
	migrated, ok := MigrateLegacyManagementAddr(legacy)
	if !ok {
		t.Fatal("MigrateLegacyManagementAddr() ok = false, want true")
	}
	if got, want := migrated.String(), "fd8c:88ad:7f06:3992:6d25:277e:9360:5cab"; got != want {
		t.Fatalf("MigrateLegacyManagementAddr() = %s, want %s", got, want)
	}
}

func TestMigrateLegacyManagementAddrNoop(t *testing.T) {
	for _, addr := range []netip.Addr{
		netip.MustParseAddr("fd8c:88ad:7f06:3992:6d25:277e:9360:5cab"),
		netip.MustParseAddr("10.210.0.1"),
	} {
		if _, ok := MigrateLegacyManagementAddr(addr); ok {
			t.Fatalf("MigrateLegacyManagementAddr(%s) ok = true, want false", addr)
		}
	}
}

func TestManagementIPFromWGKeyUsesManagementCIDR(t *testing.T) {
	priv, err := wgtypes.GeneratePrivateKey()
	if err != nil {
		t.Fatalf("GeneratePrivateKey() error = %v", err)
	}

	prefix := netip.MustParsePrefix(ManagementCIDR)
	got := ManagementIPFromWGKey(priv.PublicKey())
	if !prefix.Contains(got) {
		t.Fatalf("ManagementIPFromWGKey() = %s, not in %s", got, prefix)
	}
	if got2 := ManagementIPFromWGKey(priv.PublicKey()); got2 != got {
		t.Fatalf("ManagementIPFromWGKey() is not deterministic: first %s second %s", got, got2)
	}
}

func TestManagementIPFromPublicKey(t *testing.T) {
	priv, err := wgtypes.GeneratePrivateKey()
	if err != nil {
		t.Fatalf("GeneratePrivateKey() error = %v", err)
	}

	got, err := ManagementIPFromPublicKey(priv.PublicKey().String())
	if err != nil {
		t.Fatalf("ManagementIPFromPublicKey() error = %v", err)
	}
	want := ManagementIPFromWGKey(priv.PublicKey())
	if got != want {
		t.Fatalf("ManagementIPFromPublicKey() = %s, want %s", got, want)
	}
}

func TestManagementIPFromPublicKeyInvalid(t *testing.T) {
	if _, err := ManagementIPFromPublicKey("not-a-wireguard-public-key"); err == nil {
		t.Fatal("ManagementIPFromPublicKey() error = nil, want non-nil")
	}
}
