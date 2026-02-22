package sqlite

import (
	"path/filepath"
	"testing"

	"ployz/pkg/sdk/types"
)

func openTestStore(t *testing.T) *Store {
	t.Helper()
	store, err := Open(filepath.Join(t.TempDir(), "test.db"))
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { store.Close() })
	return store
}

func TestSpecStore_SaveAndGet(t *testing.T) {
	store := openTestStore(t)

	spec := types.NetworkSpec{
		Network:           "prod",
		DataRoot:          "/var/lib/ployz",
		NetworkCIDR:       "10.42.0.0/16",
		Subnet:            "10.42.1.0/24",
		ManagementIP:      "10.42.0.1",
		AdvertiseEndpoint: "1.2.3.4:51820",
		WGPort:            51820,
		CorrosionMemberID: 42,
		CorrosionAPIToken: "secret-token",
		Bootstrap:         []string{"10.0.0.1:4001", "10.0.0.2:4001"},
		HelperImage:       "ghcr.io/ployz/helper:latest",
	}

	if err := store.SaveSpec(spec, true); err != nil {
		t.Fatalf("SaveSpec: %v", err)
	}

	got, found, err := store.GetSpec("prod")
	if err != nil {
		t.Fatalf("GetSpec: %v", err)
	}
	if !found {
		t.Fatal("GetSpec returned found=false for saved spec")
	}
	if !got.Enabled {
		t.Error("expected Enabled=true")
	}

	g := got.Spec
	if g.Network != spec.Network {
		t.Errorf("Network: got %q, want %q", g.Network, spec.Network)
	}
	if g.DataRoot != spec.DataRoot {
		t.Errorf("DataRoot: got %q, want %q", g.DataRoot, spec.DataRoot)
	}
	if g.NetworkCIDR != spec.NetworkCIDR {
		t.Errorf("NetworkCIDR: got %q, want %q", g.NetworkCIDR, spec.NetworkCIDR)
	}
	if g.Subnet != spec.Subnet {
		t.Errorf("Subnet: got %q, want %q", g.Subnet, spec.Subnet)
	}
	if g.ManagementIP != spec.ManagementIP {
		t.Errorf("ManagementIP: got %q, want %q", g.ManagementIP, spec.ManagementIP)
	}
	if g.AdvertiseEndpoint != spec.AdvertiseEndpoint {
		t.Errorf("AdvertiseEndpoint: got %q, want %q", g.AdvertiseEndpoint, spec.AdvertiseEndpoint)
	}
	if g.WGPort != spec.WGPort {
		t.Errorf("WGPort: got %d, want %d", g.WGPort, spec.WGPort)
	}
	if g.CorrosionMemberID != spec.CorrosionMemberID {
		t.Errorf("CorrosionMemberID: got %d, want %d", g.CorrosionMemberID, spec.CorrosionMemberID)
	}
	if g.CorrosionAPIToken != spec.CorrosionAPIToken {
		t.Errorf("CorrosionAPIToken: got %q, want %q", g.CorrosionAPIToken, spec.CorrosionAPIToken)
	}
	if len(g.Bootstrap) != len(spec.Bootstrap) {
		t.Fatalf("Bootstrap length: got %d, want %d", len(g.Bootstrap), len(spec.Bootstrap))
	}
	for i := range spec.Bootstrap {
		if g.Bootstrap[i] != spec.Bootstrap[i] {
			t.Errorf("Bootstrap[%d]: got %q, want %q", i, g.Bootstrap[i], spec.Bootstrap[i])
		}
	}
	if g.HelperImage != spec.HelperImage {
		t.Errorf("HelperImage: got %q, want %q", g.HelperImage, spec.HelperImage)
	}
}

func TestSpecStore_List(t *testing.T) {
	store := openTestStore(t)

	specs := []struct {
		spec    types.NetworkSpec
		enabled bool
	}{
		{types.NetworkSpec{Network: "charlie", NetworkCIDR: "10.3.0.0/16"}, true},
		{types.NetworkSpec{Network: "alpha", NetworkCIDR: "10.1.0.0/16"}, true},
		{types.NetworkSpec{Network: "bravo", NetworkCIDR: "10.2.0.0/16"}, false},
	}

	for _, s := range specs {
		if err := store.SaveSpec(s.spec, s.enabled); err != nil {
			t.Fatalf("SaveSpec(%q): %v", s.spec.Network, err)
		}
	}

	got, err := store.ListSpecs()
	if err != nil {
		t.Fatalf("ListSpecs: %v", err)
	}

	if len(got) != 3 {
		t.Fatalf("ListSpecs returned %d specs, want 3", len(got))
	}

	// ListSpecs orders by network name.
	wantOrder := []string{"alpha", "bravo", "charlie"}
	for i, name := range wantOrder {
		if got[i].Spec.Network != name {
			t.Errorf("ListSpecs[%d].Network = %q, want %q", i, got[i].Spec.Network, name)
		}
	}

	// Verify enabled flags are preserved.
	if got[0].Enabled != true {
		t.Error("alpha should be enabled")
	}
	if got[1].Enabled != false {
		t.Error("bravo should be disabled")
	}
	if got[2].Enabled != true {
		t.Error("charlie should be enabled")
	}
}

func TestSpecStore_Delete(t *testing.T) {
	store := openTestStore(t)

	spec := types.NetworkSpec{Network: "deleteme", NetworkCIDR: "10.99.0.0/16"}
	if err := store.SaveSpec(spec, true); err != nil {
		t.Fatalf("SaveSpec: %v", err)
	}

	// Confirm it exists.
	_, found, err := store.GetSpec("deleteme")
	if err != nil {
		t.Fatalf("GetSpec before delete: %v", err)
	}
	if !found {
		t.Fatal("spec should exist before delete")
	}

	if err := store.DeleteSpec("deleteme"); err != nil {
		t.Fatalf("DeleteSpec: %v", err)
	}

	_, found, err = store.GetSpec("deleteme")
	if err != nil {
		t.Fatalf("GetSpec after delete: %v", err)
	}
	if found {
		t.Error("spec should not exist after delete")
	}
}

func TestSpecStore_Update(t *testing.T) {
	store := openTestStore(t)

	original := types.NetworkSpec{
		Network:     "mynet",
		NetworkCIDR: "10.0.0.0/16",
		WGPort:      51820,
		HelperImage: "helper:v1",
	}
	if err := store.SaveSpec(original, true); err != nil {
		t.Fatalf("SaveSpec (original): %v", err)
	}

	updated := types.NetworkSpec{
		Network:     "mynet",
		NetworkCIDR: "10.0.0.0/16",
		WGPort:      51821,
		HelperImage: "helper:v2",
		Subnet:      "10.0.5.0/24",
	}
	if err := store.SaveSpec(updated, false); err != nil {
		t.Fatalf("SaveSpec (updated): %v", err)
	}

	got, found, err := store.GetSpec("mynet")
	if err != nil {
		t.Fatalf("GetSpec: %v", err)
	}
	if !found {
		t.Fatal("spec should exist after update")
	}
	if got.Enabled {
		t.Error("expected Enabled=false after update")
	}
	if got.Spec.WGPort != 51821 {
		t.Errorf("WGPort: got %d, want 51821", got.Spec.WGPort)
	}
	if got.Spec.HelperImage != "helper:v2" {
		t.Errorf("HelperImage: got %q, want %q", got.Spec.HelperImage, "helper:v2")
	}
	if got.Spec.Subnet != "10.0.5.0/24" {
		t.Errorf("Subnet: got %q, want %q", got.Spec.Subnet, "10.0.5.0/24")
	}

	// Verify only one row exists (upsert, not insert).
	all, err := store.ListSpecs()
	if err != nil {
		t.Fatalf("ListSpecs: %v", err)
	}
	if len(all) != 1 {
		t.Errorf("expected 1 spec after update, got %d", len(all))
	}
}

func TestSpecStore_GetMissing(t *testing.T) {
	store := openTestStore(t)

	_, found, err := store.GetSpec("nonexistent")
	if err != nil {
		t.Fatalf("GetSpec: %v", err)
	}
	if found {
		t.Error("expected found=false for non-existent network")
	}
}
