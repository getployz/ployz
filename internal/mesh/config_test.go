package mesh

import (
	"testing"
)

func TestNormalizeConfig(t *testing.T) {
	t.Run("empty network defaults to default", func(t *testing.T) {
		cfg, err := NormalizeConfig(Config{})
		if err != nil {
			t.Fatalf("NormalizeConfig() error = %v", err)
		}
		if cfg.Network != "default" {
			t.Errorf("Network = %q, want %q", cfg.Network, "default")
		}
	})

	t.Run("explicit values preserved", func(t *testing.T) {
		cfg, err := NormalizeConfig(Config{
			Network:       "staging",
			DockerNetwork: "my-docker",
			CorrosionName: "my-corrosion",
			CorrosionImage: "custom-img:latest",
		})
		if err != nil {
			t.Fatalf("NormalizeConfig() error = %v", err)
		}
		if cfg.Network != "staging" {
			t.Errorf("Network = %q, want %q", cfg.Network, "staging")
		}
		if cfg.DockerNetwork != "my-docker" {
			t.Errorf("DockerNetwork = %q, want %q", cfg.DockerNetwork, "my-docker")
		}
		if cfg.CorrosionName != "my-corrosion" {
			t.Errorf("CorrosionName = %q, want %q", cfg.CorrosionName, "my-corrosion")
		}
		if cfg.CorrosionImage != "custom-img:latest" {
			t.Errorf("CorrosionImage = %q, want %q", cfg.CorrosionImage, "custom-img:latest")
		}
	})

	t.Run("invalid advertise endpoint returns error", func(t *testing.T) {
		_, err := NormalizeConfig(Config{
			AdvertiseEndpoint: "not-an-addr-port",
		})
		if err == nil {
			t.Fatal("NormalizeConfig() expected error for invalid advertise endpoint")
		}
	})
}

func TestInterfaceName(t *testing.T) {
	tests := []struct {
		name    string
		network string
		want    string
	}{
		{
			name:    "short name",
			network: "default",
			want:    "plz-default",
		},
		{
			name:    "exactly 15 chars",
			network: "12345678901", // "plz-" (4) + 11 = 15
			want:    "plz-12345678901",
		},
		{
			name:    "16+ chars truncated",
			network: "abcdefghijklmnop", // "plz-" (4) + 16 = 20, truncated to 15
			want:    "plz-abcdefghijk",
		},
		{
			name:    "empty name",
			network: "",
			want:    "plz-",
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			got := InterfaceName(tt.network)
			if got != tt.want {
				t.Errorf("InterfaceName(%q) = %q, want %q", tt.network, got, tt.want)
			}
			if len(got) > maxInterfaceNameLength {
				t.Errorf("InterfaceName(%q) len = %d, exceeds max %d", tt.network, len(got), maxInterfaceNameLength)
			}
		})
	}
}
