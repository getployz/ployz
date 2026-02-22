package mesh

import (
	"net/netip"
	"testing"

	"golang.zx2c4.com/wireguard/wgctrl/wgtypes"
)

// testPubKey generates a fresh WireGuard public key string for use in tests.
func testPubKey(t *testing.T) string {
	t.Helper()
	key, err := wgtypes.GeneratePrivateKey()
	if err != nil {
		t.Fatalf("GeneratePrivateKey() error = %v", err)
	}
	return key.PublicKey().String()
}

func TestParsePeerSpec(t *testing.T) {
	validPubKey := testPubKey(t)

	tests := []struct {
		name    string
		peer    Peer
		wantErr bool
	}{
		{
			name:    "empty pubkey",
			peer:    Peer{PublicKey: "", Subnet: "10.0.0.0/24"},
			wantErr: true,
		},
		{
			name:    "invalid pubkey",
			peer:    Peer{PublicKey: "not-a-key", Subnet: "10.0.0.0/24"},
			wantErr: true,
		},
		{
			name:    "empty subnet and management",
			peer:    Peer{PublicKey: validPubKey},
			wantErr: true,
		},
		{
			name:    "invalid management",
			peer:    Peer{PublicKey: validPubKey, ManagementIP: "not-an-ip"},
			wantErr: true,
		},
		{
			name:    "invalid subnet",
			peer:    Peer{PublicKey: validPubKey, Subnet: "not-a-prefix"},
			wantErr: true,
		},
		{
			name:    "invalid endpoint",
			peer:    Peer{PublicKey: validPubKey, Subnet: "10.0.0.0/24", Endpoint: "not-an-addrport"},
			wantErr: true,
		},
		{
			name: "valid spec with all fields",
			peer: Peer{
				PublicKey:   validPubKey,
				Subnet:      "10.0.0.0/24",
				ManagementIP: "fd8c:88ad:7f06::1",
				Endpoint:    "5.9.85.203:51820",
			},
			wantErr: false,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			spec, err := parsePeerSpec(tt.peer)
			if tt.wantErr {
				if err == nil {
					t.Fatal("parsePeerSpec() expected error")
				}
				return
			}
			if err != nil {
				t.Fatalf("parsePeerSpec() error = %v", err)
			}

			// Verify all fields are populated for the valid case.
			if spec.PublicKey.String() != validPubKey {
				t.Errorf("PublicKey = %q, want %q", spec.PublicKey.String(), validPubKey)
			}
			if spec.Endpoint == nil {
				t.Fatal("Endpoint should not be nil")
			}
			if spec.Endpoint.String() != "5.9.85.203:51820" {
				t.Errorf("Endpoint = %q, want %q", spec.Endpoint.String(), "5.9.85.203:51820")
			}
			// Management (/128) + subnet = 2 allowed prefixes.
			if len(spec.AllowedPrefixes) != 2 {
				t.Fatalf("AllowedPrefixes len = %d, want 2", len(spec.AllowedPrefixes))
			}
		})
	}
}

func TestBuildPeerSpecs(t *testing.T) {
	validPubKey := testPubKey(t)

	t.Run("empty list", func(t *testing.T) {
		specs, err := BuildPeerSpecs(nil)
		if err != nil {
			t.Fatalf("BuildPeerSpecs(nil) error = %v", err)
		}
		if len(specs) != 0 {
			t.Errorf("BuildPeerSpecs(nil) len = %d, want 0", len(specs))
		}
	})

	t.Run("one valid", func(t *testing.T) {
		specs, err := BuildPeerSpecs([]Peer{
			{PublicKey: validPubKey, Subnet: "10.0.0.0/24"},
		})
		if err != nil {
			t.Fatalf("BuildPeerSpecs() error = %v", err)
		}
		if len(specs) != 1 {
			t.Fatalf("BuildPeerSpecs() len = %d, want 1", len(specs))
		}
	})

	t.Run("one invalid returns error", func(t *testing.T) {
		_, err := BuildPeerSpecs([]Peer{
			{PublicKey: validPubKey, Subnet: "10.0.0.0/24"},
			{PublicKey: "", Subnet: "10.0.1.0/24"}, // empty pubkey
		})
		if err == nil {
			t.Fatal("BuildPeerSpecs() expected error for invalid peer")
		}
	})
}

func TestSingleIPPrefix(t *testing.T) {
	tests := []struct {
		name string
		addr netip.Addr
		bits int
	}{
		{
			name: "IPv4 produces /32",
			addr: netip.MustParseAddr("10.0.0.1"),
			bits: 32,
		},
		{
			name: "IPv6 produces /128",
			addr: netip.MustParseAddr("fd8c:88ad:7f06::1"),
			bits: 128,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			prefix := SingleIPPrefix(tt.addr)
			if prefix.Bits() != tt.bits {
				t.Errorf("SingleIPPrefix(%v).Bits() = %d, want %d", tt.addr, prefix.Bits(), tt.bits)
			}
			if !prefix.Contains(tt.addr) {
				t.Errorf("SingleIPPrefix(%v) does not contain the address", tt.addr)
			}
		})
	}
}

func TestMachineIP(t *testing.T) {
	tests := []struct {
		name   string
		prefix netip.Prefix
		want   netip.Addr
	}{
		{
			name:   "IPv4 /24 returns .1",
			prefix: netip.MustParsePrefix("10.0.0.0/24"),
			want:   netip.MustParseAddr("10.0.0.1"),
		},
		{
			name:   "IPv4 /16 returns .0.1",
			prefix: netip.MustParsePrefix("10.210.0.0/16"),
			want:   netip.MustParseAddr("10.210.0.1"),
		},
		{
			name:   "IPv6 prefix returns next addr",
			prefix: netip.MustParsePrefix("fd00::/64"),
			want:   netip.MustParseAddr("fd00::1"),
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			got := MachineIP(tt.prefix)
			if got != tt.want {
				t.Errorf("MachineIP(%v) = %v, want %v", tt.prefix, got, tt.want)
			}
		})
	}
}
