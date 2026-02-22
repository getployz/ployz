package mesh

import (
	"net/netip"
	"testing"
)

func TestResolve(t *testing.T) {
	// validState returns a State with all required fields populated.
	validState := func() *State {
		return &State{
			Network:           "prod",
			CIDR:              "10.99.0.0/16",
			Subnet:            "10.99.1.0/24",
			Management:        "fd8c:abcd::1",
			Advertise:         "1.2.3.4:51820",
			WGInterface:       "plz-prod",
			WGPort:            51999,
			DockerNetwork:     "ployz-prod",
			CorrosionName:     "ployz-corrosion-prod",
			CorrosionImage:    "custom-corrosion:v2",
			CorrosionMemberID: 42,
			CorrosionAPIToken: "state-token-abc",
			Bootstrap:         []string{"10.0.0.1:53000", "10.0.0.2:53000"},
		}
	}

	tests := []struct {
		name    string
		cfg     Config
		state   *State
		wantErr bool
		check   func(t *testing.T, got Config)
	}{
		{
			name:  "nil state returns normalized config",
			cfg:   Config{},
			state: nil,
			check: func(t *testing.T, got Config) {
				if got.Network != "default" {
					t.Errorf("Network = %q, want %q", got.Network, "default")
				}
				// NormalizeConfig sets WGInterface from network name.
				if got.WGInterface != "plz-default" {
					t.Errorf("WGInterface = %q, want %q", got.WGInterface, "plz-default")
				}
			},
		},
		{
			name: "state fills missing config fields",
			cfg:  Config{Network: "prod"},
			state: func() *State {
				s := validState()
				return s
			}(),
			check: func(t *testing.T, got Config) {
				if got.Network != "prod" {
					t.Errorf("Network = %q, want %q", got.Network, "prod")
				}
				cidr := netip.MustParsePrefix("10.99.0.0/16")
				if got.NetworkCIDR != cidr {
					t.Errorf("NetworkCIDR = %s, want %s", got.NetworkCIDR, cidr)
				}
				subnet := netip.MustParsePrefix("10.99.1.0/24")
				if got.Subnet != subnet {
					t.Errorf("Subnet = %s, want %s", got.Subnet, subnet)
				}
				mgmt := netip.MustParseAddr("fd8c:abcd::1")
				if got.Management != mgmt {
					t.Errorf("Management = %s, want %s", got.Management, mgmt)
				}
				if got.AdvertiseEndpoint != "1.2.3.4:51820" {
					t.Errorf("AdvertiseEndpoint = %q, want %q", got.AdvertiseEndpoint, "1.2.3.4:51820")
				}
				if got.CorrosionMemberID != 42 {
					t.Errorf("CorrosionMemberID = %d, want %d", got.CorrosionMemberID, 42)
				}
				if got.CorrosionAPIToken != "state-token-abc" {
					t.Errorf("CorrosionAPIToken = %q, want %q", got.CorrosionAPIToken, "state-token-abc")
				}
			},
		},
		{
			name: "config values take precedence over state",
			cfg: Config{
				Network:           "staging",
				DockerNetwork:     "my-docker",
				CorrosionName:     "my-corrosion",
				CorrosionImage:    "my-img:latest",
				CorrosionMemberID: 99,
				CorrosionAPIToken: "cfg-token",
				NetworkCIDR:       netip.MustParsePrefix("10.50.0.0/16"),
				Subnet:            netip.MustParsePrefix("10.50.1.0/24"),
			},
			state: validState(),
			check: func(t *testing.T, got Config) {
				if got.Network != "staging" {
					t.Errorf("Network = %q, want %q", got.Network, "staging")
				}
				if got.DockerNetwork != "my-docker" {
					t.Errorf("DockerNetwork = %q, want %q", got.DockerNetwork, "my-docker")
				}
				if got.CorrosionName != "my-corrosion" {
					t.Errorf("CorrosionName = %q, want %q", got.CorrosionName, "my-corrosion")
				}
				if got.CorrosionImage != "my-img:latest" {
					t.Errorf("CorrosionImage = %q, want %q", got.CorrosionImage, "my-img:latest")
				}
				if got.CorrosionMemberID != 99 {
					t.Errorf("CorrosionMemberID = %d, want %d", got.CorrosionMemberID, 99)
				}
				if got.CorrosionAPIToken != "cfg-token" {
					t.Errorf("CorrosionAPIToken = %q, want %q", got.CorrosionAPIToken, "cfg-token")
				}
				wantCIDR := netip.MustParsePrefix("10.50.0.0/16")
				if got.NetworkCIDR != wantCIDR {
					t.Errorf("NetworkCIDR = %s, want %s", got.NetworkCIDR, wantCIDR)
				}
				wantSubnet := netip.MustParsePrefix("10.50.1.0/24")
				if got.Subnet != wantSubnet {
					t.Errorf("Subnet = %s, want %s", got.Subnet, wantSubnet)
				}
			},
		},
		{
			name: "invalid CIDR in state returns error",
			cfg:  Config{Network: "test"},
			state: func() *State {
				s := validState()
				s.CIDR = "not-a-cidr"
				return s
			}(),
			wantErr: true,
		},
		{
			name: "missing subnet in state returns error",
			cfg:  Config{Network: "test"},
			state: func() *State {
				s := validState()
				s.Subnet = ""
				return s
			}(),
			wantErr: true,
		},
		{
			name: "missing management in state returns error",
			cfg:  Config{Network: "test"},
			state: func() *State {
				s := validState()
				s.Management = ""
				return s
			}(),
			wantErr: true,
		},
		{
			name: "bootstrap merging from state",
			cfg:  Config{Network: "prod"},
			state: func() *State {
				s := validState()
				s.Bootstrap = []string{"10.0.0.5:53000", "10.0.0.6:53000"}
				return s
			}(),
			check: func(t *testing.T, got Config) {
				if len(got.CorrosionBootstrap) != 2 {
					t.Fatalf("CorrosionBootstrap len = %d, want 2", len(got.CorrosionBootstrap))
				}
				if got.CorrosionBootstrap[0] != "10.0.0.5:53000" {
					t.Errorf("CorrosionBootstrap[0] = %q, want %q", got.CorrosionBootstrap[0], "10.0.0.5:53000")
				}
				if got.CorrosionBootstrap[1] != "10.0.0.6:53000" {
					t.Errorf("CorrosionBootstrap[1] = %q, want %q", got.CorrosionBootstrap[1], "10.0.0.6:53000")
				}
			},
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			got, err := Resolve(tt.cfg, tt.state)
			if tt.wantErr {
				if err == nil {
					t.Fatal("Resolve() expected error, got nil")
				}
				return
			}
			if err != nil {
				t.Fatalf("Resolve() unexpected error: %v", err)
			}
			if tt.check != nil {
				tt.check(t, got)
			}
		})
	}
}
