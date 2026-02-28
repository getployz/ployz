package overlay

import (
	"context"
	"errors"
	"net/netip"
	"testing"
)

func TestExpectedCorrosionMembersRemoteRows(t *testing.T) {
	t.Parallel()

	controller := &Controller{
		newRegistry: func(netip.AddrPort, string) Registry {
			return fakeStatusRegistry{
				rows: []MachineRow{
					{ID: "local-id", PublicKey: "local-id"},
					{ID: "peer-a", PublicKey: "peer-a"},
				},
			}
		},
	}

	state := &State{
		WGPublic: "local-id",
	}

	got := controller.expectedCorrosionMembers(context.Background(), Config{}, state)
	if got != 1 {
		t.Fatalf("expectedCorrosionMembers() = %d, want 1", got)
	}
}

func TestExpectedCorrosionMembersFallsBackToBootstrapOnRegistryError(t *testing.T) {
	t.Parallel()

	controller := &Controller{
		newRegistry: func(netip.AddrPort, string) Registry {
			return fakeStatusRegistry{listErr: errors.New("boom")}
		},
	}

	state := &State{
		WGPublic:  "local-id",
		Bootstrap: []string{"peer-a", "peer-b"},
	}

	got := controller.expectedCorrosionMembers(context.Background(), Config{}, state)
	if got != 2 {
		t.Fatalf("expectedCorrosionMembers() = %d, want 2", got)
	}
}

type fakeStatusRegistry struct {
	rows    []MachineRow
	listErr error
}

func (f fakeStatusRegistry) EnsureMachineTable(context.Context) error { return nil }

func (f fakeStatusRegistry) UpsertMachine(context.Context, MachineRow) error { return nil }

func (f fakeStatusRegistry) DeleteByEndpointExceptID(context.Context, string, string) error {
	return nil
}

func (f fakeStatusRegistry) DeleteMachine(context.Context, string) error { return nil }

func (f fakeStatusRegistry) ListMachineRows(context.Context) ([]MachineRow, error) {
	if f.listErr != nil {
		return nil, f.listErr
	}
	return f.rows, nil
}

func (f fakeStatusRegistry) EnsureNetworkConfigTable(context.Context) error { return nil }

func (f fakeStatusRegistry) EnsureNetworkCIDR(_ context.Context, requested netip.Prefix, _ string, defaultCIDR netip.Prefix) (netip.Prefix, error) {
	if requested.IsValid() {
		return requested, nil
	}
	return defaultCIDR, nil
}
