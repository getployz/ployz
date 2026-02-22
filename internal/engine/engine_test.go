package engine

import (
	"context"
	"net/netip"
	"strings"
	"testing"
	"time"

	netctrl "ployz/internal/mesh"
	"ployz/pkg/sdk/types"
)

func TestConfigFromSpec(t *testing.T) {
	t.Run("valid spec", func(t *testing.T) {
		spec := types.NetworkSpec{
			Network:           "test-net",
			DataRoot:          "/tmp/ployz-test",
			NetworkCIDR:       "10.210.0.0/16",
			Subnet:            "10.210.1.0/24",
			ManagementIP:      "10.210.1.1",
			AdvertiseEndpoint: "5.9.85.203:51820",
			WGPort:            51820,
			CorrosionMemberID: 42,
			CorrosionAPIToken: "test-token",
			Bootstrap:         []string{"5.9.85.203:53094", "", "10.0.0.1:53094"},
			HelperImage:       "ghcr.io/test/helper:latest",
		}

		cfg, err := netctrl.ConfigFromSpec(spec)
		if err != nil {
			t.Fatalf("configFromSpec() error: %v", err)
		}

		if cfg.Network != "test-net" {
			t.Errorf("Network: got %q, want %q", cfg.Network, "test-net")
		}
		if cfg.DataRoot != "/tmp/ployz-test" {
			t.Errorf("DataRoot: got %q, want %q", cfg.DataRoot, "/tmp/ployz-test")
		}
		wantCIDR := netip.MustParsePrefix("10.210.0.0/16")
		if cfg.NetworkCIDR != wantCIDR {
			t.Errorf("NetworkCIDR: got %v, want %v", cfg.NetworkCIDR, wantCIDR)
		}
		wantSubnet := netip.MustParsePrefix("10.210.1.0/24")
		if cfg.Subnet != wantSubnet {
			t.Errorf("Subnet: got %v, want %v", cfg.Subnet, wantSubnet)
		}
		wantMgmt := netip.MustParseAddr("10.210.1.1")
		if cfg.Management != wantMgmt {
			t.Errorf("Management: got %v, want %v", cfg.Management, wantMgmt)
		}
		if cfg.AdvertiseEndpoint != "5.9.85.203:51820" {
			t.Errorf("AdvertiseEndpoint: got %q, want %q", cfg.AdvertiseEndpoint, "5.9.85.203:51820")
		}
		if cfg.WGPort != 51820 {
			t.Errorf("WGPort: got %d, want %d", cfg.WGPort, 51820)
		}
		if cfg.CorrosionMemberID != 42 {
			t.Errorf("CorrosionMemberID: got %d, want %d", cfg.CorrosionMemberID, 42)
		}
		if cfg.CorrosionAPIToken != "test-token" {
			t.Errorf("CorrosionAPIToken: got %q, want %q", cfg.CorrosionAPIToken, "test-token")
		}
		if cfg.HelperImage != "ghcr.io/test/helper:latest" {
			t.Errorf("HelperImage: got %q, want %q", cfg.HelperImage, "ghcr.io/test/helper:latest")
		}
		// Empty bootstrap entry should be skipped, leaving 2.
		if len(cfg.CorrosionBootstrap) != 2 {
			t.Fatalf("CorrosionBootstrap len: got %d, want 2 (%v)", len(cfg.CorrosionBootstrap), cfg.CorrosionBootstrap)
		}
		if cfg.CorrosionBootstrap[0] != "5.9.85.203:53094" {
			t.Errorf("CorrosionBootstrap[0]: got %q, want %q", cfg.CorrosionBootstrap[0], "5.9.85.203:53094")
		}
		if cfg.CorrosionBootstrap[1] != "10.0.0.1:53094" {
			t.Errorf("CorrosionBootstrap[1]: got %q, want %q", cfg.CorrosionBootstrap[1], "10.0.0.1:53094")
		}
	})

	t.Run("invalid NetworkCIDR", func(t *testing.T) {
		spec := types.NetworkSpec{
			Network:     "test-net",
			NetworkCIDR: "not-a-cidr",
		}
		_, err := netctrl.ConfigFromSpec(spec)
		if err == nil {
			t.Fatal("expected error for invalid NetworkCIDR")
		}
		if !strings.Contains(err.Error(), "parse network cidr") {
			t.Errorf("error %q should contain %q", err.Error(), "parse network cidr")
		}
	})

	t.Run("invalid Subnet", func(t *testing.T) {
		spec := types.NetworkSpec{
			Network:     "test-net",
			NetworkCIDR: "10.210.0.0/16",
			Subnet:      "bad-subnet",
		}
		_, err := netctrl.ConfigFromSpec(spec)
		if err == nil {
			t.Fatal("expected error for invalid Subnet")
		}
		if !strings.Contains(err.Error(), "parse subnet") {
			t.Errorf("error %q should contain %q", err.Error(), "parse subnet")
		}
	})

	t.Run("invalid ManagementIP", func(t *testing.T) {
		spec := types.NetworkSpec{
			Network:      "test-net",
			NetworkCIDR:  "10.210.0.0/16",
			Subnet:       "10.210.1.0/24",
			ManagementIP: "not-an-ip",
		}
		_, err := netctrl.ConfigFromSpec(spec)
		if err == nil {
			t.Fatal("expected error for invalid ManagementIP")
		}
		if !strings.Contains(err.Error(), "parse management ip") {
			t.Errorf("error %q should contain %q", err.Error(), "parse management ip")
		}
	})

	t.Run("empty bootstrap entries skipped", func(t *testing.T) {
		spec := types.NetworkSpec{
			Network:   "test-net",
			Bootstrap: []string{"", "  ", "10.0.0.1:53094", ""},
		}
		cfg, err := netctrl.ConfigFromSpec(spec)
		if err != nil {
			t.Fatalf("configFromSpec() error: %v", err)
		}
		if len(cfg.CorrosionBootstrap) != 1 {
			t.Fatalf("CorrosionBootstrap len: got %d, want 1 (%v)", len(cfg.CorrosionBootstrap), cfg.CorrosionBootstrap)
		}
		if cfg.CorrosionBootstrap[0] != "10.0.0.1:53094" {
			t.Errorf("CorrosionBootstrap[0]: got %q, want %q", cfg.CorrosionBootstrap[0], "10.0.0.1:53094")
		}
	})
}

func TestSleepWithContext(t *testing.T) {
	t.Run("completes normally", func(t *testing.T) {
		ctx := context.Background()
		ok := sleepWithContext(ctx, 10*time.Millisecond)
		if !ok {
			t.Error("sleepWithContext returned false, want true for uncancelled context")
		}
	})

	t.Run("returns false when context cancelled", func(t *testing.T) {
		ctx, cancel := context.WithCancel(context.Background())
		cancel()
		ok := sleepWithContext(ctx, 10*time.Second)
		if ok {
			t.Error("sleepWithContext returned true, want false for cancelled context")
		}
	})
}
