package node

import (
	"testing"

	"ployz/pkg/sdk/types"
)

func TestLocalMachineFromIdentity(t *testing.T) {
	entry, err := localMachineFromIdentity(types.Identity{
		ID:                " node-a ",
		PublicKey:         " pub-a ",
		Subnet:            " 10.210.1.0/24 ",
		ManagementIP:      " fd00::1 ",
		AdvertiseEndpoint: " 1.2.3.4:51820 ",
	})
	if err != nil {
		t.Fatalf("localMachineFromIdentity() error: %v", err)
	}
	if entry.ID != "node-a" {
		t.Fatalf("entry.ID = %q, want node-a", entry.ID)
	}
	if entry.PublicKey != "pub-a" {
		t.Fatalf("entry.PublicKey = %q, want pub-a", entry.PublicKey)
	}
	if entry.Subnet != "10.210.1.0/24" {
		t.Fatalf("entry.Subnet = %q, want 10.210.1.0/24", entry.Subnet)
	}
	if entry.ManagementIP != "fd00::1" {
		t.Fatalf("entry.ManagementIP = %q, want fd00::1", entry.ManagementIP)
	}
	if entry.Endpoint != "1.2.3.4:51820" {
		t.Fatalf("entry.Endpoint = %q, want 1.2.3.4:51820", entry.Endpoint)
	}
}

func TestLocalMachineFromIdentityMissingID(t *testing.T) {
	_, err := localMachineFromIdentity(types.Identity{})
	if err == nil {
		t.Fatal("localMachineFromIdentity() error = nil, want missing machine identity")
	}
}
