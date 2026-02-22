package mesh

import (
	"net/netip"
	"testing"
)

func TestNormalizeRegistryRows(t *testing.T) {
	mgmtPrefix := netip.MustParsePrefix(ManagementCIDR)

	tests := []struct {
		name    string
		rows    []MachineRow
		wantErr bool
		check   func(t *testing.T, out []MachineRow)
	}{
		{
			name: "empty rows returns empty slice",
			rows: []MachineRow{},
			check: func(t *testing.T, out []MachineRow) {
				if len(out) != 0 {
					t.Errorf("len(out) = %d, want 0", len(out))
				}
			},
		},
		{
			name: "nil rows returns empty slice",
			rows: nil,
			check: func(t *testing.T, out []MachineRow) {
				if len(out) != 0 {
					t.Errorf("len(out) = %d, want 0", len(out))
				}
			},
		},
		{
			name: "single valid row sets management IP",
			rows: func() []MachineRow {
				// Defer key generation to avoid top-level helper call.
				return nil
			}(),
			check: func(t *testing.T, out []MachineRow) {
				// Replaced at runtime below.
			},
		},
		{
			name: "multiple valid rows all get management IPs",
			rows: func() []MachineRow {
				return nil
			}(),
			check: func(t *testing.T, out []MachineRow) {},
		},
		{
			name: "preserves non-management fields",
			rows: func() []MachineRow {
				return nil
			}(),
			check: func(t *testing.T, out []MachineRow) {},
		},
		{
			name:    "invalid public key returns error",
			rows:    []MachineRow{{ID: "bad", PublicKey: "not-a-valid-key", Subnet: "10.0.0.0/24"}},
			wantErr: true,
		},
		{
			name:    "empty public key returns error",
			rows:    []MachineRow{{ID: "empty", PublicKey: "", Subnet: "10.0.0.0/24"}},
			wantErr: true,
		},
	}

	// Generate keys for the dynamic test cases.
	key1 := testPubKey(t)
	key2 := testPubKey(t)
	key3 := testPubKey(t)

	// Wire up the "single valid row" case.
	tests[2].rows = []MachineRow{
		{ID: "m1", PublicKey: key1, Subnet: "10.0.0.0/24", Endpoint: "5.9.85.203:51820"},
	}
	tests[2].check = func(t *testing.T, out []MachineRow) {
		if len(out) != 1 {
			t.Fatalf("len(out) = %d, want 1", len(out))
		}
		if out[0].ManagementIP == "" {
			t.Fatal("Management should be set")
		}
		addr, err := netip.ParseAddr(out[0].ManagementIP)
		if err != nil {
			t.Fatalf("parse Management IP: %v", err)
		}
		if !addr.Is6() {
			t.Errorf("Management IP %v is not IPv6", addr)
		}
		if !mgmtPrefix.Contains(addr) {
			t.Errorf("Management IP %v not in %s", addr, ManagementCIDR)
		}
	}

	// Wire up "multiple valid rows" case.
	tests[3].rows = []MachineRow{
		{ID: "m1", PublicKey: key1, Subnet: "10.0.0.0/24"},
		{ID: "m2", PublicKey: key2, Subnet: "10.0.1.0/24"},
		{ID: "m3", PublicKey: key3, Subnet: "10.0.2.0/24"},
	}
	tests[3].check = func(t *testing.T, out []MachineRow) {
		if len(out) != 3 {
			t.Fatalf("len(out) = %d, want 3", len(out))
		}
		for i, row := range out {
			if row.ManagementIP == "" {
				t.Errorf("row[%d].ManagementIP should be set", i)
				continue
			}
			addr, err := netip.ParseAddr(row.ManagementIP)
			if err != nil {
				t.Errorf("row[%d]: parse Management IP: %v", i, err)
				continue
			}
			if !mgmtPrefix.Contains(addr) {
				t.Errorf("row[%d]: Management IP %v not in %s", i, addr, ManagementCIDR)
			}
		}
		// All management IPs should be distinct.
		seen := make(map[string]bool, len(out))
		for _, row := range out {
			if seen[row.ManagementIP] {
				t.Errorf("duplicate Management IP %s", row.ManagementIP)
			}
			seen[row.ManagementIP] = true
		}
	}

	// Wire up "preserves non-management fields" case.
	tests[4].rows = []MachineRow{
		{
			ID:        "node-42",
			PublicKey: key1,
			Subnet:    "10.99.0.0/24",
			Endpoint:  "1.2.3.4:51820",
			UpdatedAt: "2026-01-01T00:00:00Z",
			Version:   7,
		},
	}
	tests[4].check = func(t *testing.T, out []MachineRow) {
		if len(out) != 1 {
			t.Fatalf("len(out) = %d, want 1", len(out))
		}
		row := out[0]
		if row.ID != "node-42" {
			t.Errorf("ID = %q, want %q", row.ID, "node-42")
		}
		if row.PublicKey != key1 {
			t.Errorf("PublicKey = %q, want %q", row.PublicKey, key1)
		}
		if row.Subnet != "10.99.0.0/24" {
			t.Errorf("Subnet = %q, want %q", row.Subnet, "10.99.0.0/24")
		}
		if row.Endpoint != "1.2.3.4:51820" {
			t.Errorf("Endpoint = %q, want %q", row.Endpoint, "1.2.3.4:51820")
		}
		if row.UpdatedAt != "2026-01-01T00:00:00Z" {
			t.Errorf("UpdatedAt = %q, want %q", row.UpdatedAt, "2026-01-01T00:00:00Z")
		}
		if row.Version != 7 {
			t.Errorf("Version = %d, want %d", row.Version, 7)
		}
		// Management should be overwritten regardless of input.
		if row.ManagementIP == "" {
			t.Fatal("Management should be set")
		}
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			out, err := normalizeRegistryRows(tt.rows)
			if tt.wantErr {
				if err == nil {
					t.Fatal("normalizeRegistryRows() expected error")
				}
				return
			}
			if err != nil {
				t.Fatalf("normalizeRegistryRows() error = %v", err)
			}
			if tt.check != nil {
				tt.check(t, out)
			}
		})
	}
}

func TestNormalizeRegistryRowsDeterministic(t *testing.T) {
	key := testPubKey(t)
	row := MachineRow{ID: "m1", PublicKey: key, Subnet: "10.0.0.0/24"}

	out1, err := normalizeRegistryRows([]MachineRow{row})
	if err != nil {
		t.Fatalf("first call error = %v", err)
	}
	out2, err := normalizeRegistryRows([]MachineRow{row})
	if err != nil {
		t.Fatalf("second call error = %v", err)
	}

	if out1[0].ManagementIP != out2[0].ManagementIP {
		t.Errorf("not deterministic: %q != %q", out1[0].ManagementIP, out2[0].ManagementIP)
	}
}

func TestNormalizeRegistryRowsDoesNotMutateInput(t *testing.T) {
	key := testPubKey(t)
	input := []MachineRow{
		{ID: "m1", PublicKey: key, Subnet: "10.0.0.0/24", ManagementIP: "original"},
	}

	out, err := normalizeRegistryRows(input)
	if err != nil {
		t.Fatalf("normalizeRegistryRows() error = %v", err)
	}

	// The input slice's Management field should be unchanged.
	if input[0].ManagementIP != "original" {
		t.Errorf("input was mutated: Management = %q, want %q", input[0].ManagementIP, "original")
	}
	// The output should have a derived management IP, not the original.
	if out[0].ManagementIP == "original" {
		t.Error("output Management should be derived, not the original value")
	}
}
