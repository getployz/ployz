package reconcile

import (
	"fmt"
	"testing"

	"ployz/internal/mesh"
	"ployz/pkg/sdk/defaults"
)

func TestApplyMachineChange(t *testing.T) {
	m1 := mesh.MachineRow{ID: "m1", PublicKey: "pk1", Subnet: "10.0.0.0/24"}
	m2 := mesh.MachineRow{ID: "m2", PublicKey: "pk2", Subnet: "10.0.1.0/24"}
	m3 := mesh.MachineRow{ID: "m3", PublicKey: "pk3", Subnet: "10.0.2.0/24"}

	tests := []struct {
		name     string
		initial  []mesh.MachineRow
		change   mesh.MachineChange
		expected []mesh.MachineRow
	}{
		{
			name:    "ChangeAdded to empty list",
			initial: nil,
			change: mesh.MachineChange{
				Kind:    mesh.ChangeAdded,
				Machine: m1,
			},
			expected: []mesh.MachineRow{m1},
		},
		{
			name:    "ChangeUpdated replaces existing machine by ID",
			initial: []mesh.MachineRow{m1, m2},
			change: mesh.MachineChange{
				Kind:    mesh.ChangeUpdated,
				Machine: mesh.MachineRow{ID: "m1", PublicKey: "pk1-new", Subnet: "10.0.10.0/24"},
			},
			expected: []mesh.MachineRow{
				{ID: "m1", PublicKey: "pk1-new", Subnet: "10.0.10.0/24"},
				m2,
			},
		},
		{
			name:    "ChangeAdded appends to non-empty list when no match",
			initial: []mesh.MachineRow{m1},
			change: mesh.MachineChange{
				Kind:    mesh.ChangeAdded,
				Machine: m2,
			},
			expected: []mesh.MachineRow{m1, m2},
		},
		{
			name:    "ChangeDeleted removes by ID",
			initial: []mesh.MachineRow{m1, m2, m3},
			change: mesh.MachineChange{
				Kind:    mesh.ChangeDeleted,
				Machine: mesh.MachineRow{ID: "m2"},
			},
			expected: []mesh.MachineRow{m1, m3},
		},
		{
			name:    "ChangeDeleted removes by PublicKey when ID is empty",
			initial: []mesh.MachineRow{m1, m2, m3},
			change: mesh.MachineChange{
				Kind:    mesh.ChangeDeleted,
				Machine: mesh.MachineRow{PublicKey: "pk2"},
			},
			expected: []mesh.MachineRow{m1, m3},
		},
		{
			name:    "ChangeDeleted non-existent leaves list unchanged",
			initial: []mesh.MachineRow{m1, m2},
			change: mesh.MachineChange{
				Kind:    mesh.ChangeDeleted,
				Machine: mesh.MachineRow{ID: "nonexistent"},
			},
			expected: []mesh.MachineRow{m1, m2},
		},
		{
			name:    "ChangeDeleted on empty list does not panic",
			initial: nil,
			change: mesh.MachineChange{
				Kind:    mesh.ChangeDeleted,
				Machine: mesh.MachineRow{ID: "m1"},
			},
			expected: []mesh.MachineRow{},
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			// Copy initial slice to avoid mutation across subtests.
			var input []mesh.MachineRow
			if tt.initial != nil {
				input = make([]mesh.MachineRow, len(tt.initial))
				copy(input, tt.initial)
			}

			got := applyMachineChange(input, tt.change)

			if len(got) != len(tt.expected) {
				t.Fatalf("length mismatch: got %d, want %d\ngot:  %+v\nwant: %+v", len(got), len(tt.expected), got, tt.expected)
			}
			for i := range tt.expected {
				if got[i] != tt.expected[i] {
					t.Errorf("index %d: got %+v, want %+v", i, got[i], tt.expected[i])
				}
			}
		})
	}
}

func TestResolvePingAddrs(t *testing.T) {
	tests := []struct {
		name     string
		machines []mesh.MachineRow
		network  string
		expected map[string]string
	}{
		{
			name:     "empty machines list returns empty map",
			machines: nil,
			network:  "default",
			expected: map[string]string{},
		},
		{
			name: "valid subnet produces host:port",
			machines: []mesh.MachineRow{
				{ID: "m1", Subnet: "10.0.0.0/24"},
			},
			network: "default",
			expected: map[string]string{
				"m1": fmt.Sprintf("10.0.0.1:%d", defaults.DaemonAPIPort("default")),
			},
		},
		{
			name: "invalid subnet is skipped",
			machines: []mesh.MachineRow{
				{ID: "m1", Subnet: "not-a-cidr"},
				{ID: "m2", Subnet: "10.0.1.0/24"},
			},
			network: "default",
			expected: map[string]string{
				"m2": fmt.Sprintf("10.0.1.1:%d", defaults.DaemonAPIPort("default")),
			},
		},
		{
			name: "empty network uses default port",
			machines: []mesh.MachineRow{
				{ID: "m1", Subnet: "10.0.0.0/24"},
			},
			network: "",
			expected: map[string]string{
				"m1": fmt.Sprintf("10.0.0.1:%d", defaults.DaemonAPIPort("")),
			},
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			got := resolvePingAddrs(tt.machines, tt.network)

			if len(got) != len(tt.expected) {
				t.Fatalf("length mismatch: got %d, want %d\ngot:  %v\nwant: %v", len(got), len(tt.expected), got, tt.expected)
			}
			for k, wantV := range tt.expected {
				gotV, ok := got[k]
				if !ok {
					t.Errorf("missing key %q in result", k)
					continue
				}
				if gotV != wantV {
					t.Errorf("key %q: got %q, want %q", k, gotV, wantV)
				}
			}
		})
	}
}
