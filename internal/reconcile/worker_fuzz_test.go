package reconcile

import (
	"testing"

	"ployz/internal/mesh"
)

func FuzzApplyMachineChange(f *testing.F) {
	f.Add("m1", "pk1", "10.0.0.0/24")

	f.Fuzz(func(t *testing.T, id, pubkey, subnet string) {
		row := mesh.MachineRow{ID: id, PublicKey: pubkey, Subnet: subnet}

		// Add then delete by same ID returns original length.
		initial := []mesh.MachineRow{{ID: "existing", PublicKey: "pk0", Subnet: "10.0.1.0/24"}}

		added := applyMachineChange(copyRows(initial), mesh.MachineChange{
			Kind:    mesh.ChangeAdded,
			Machine: row,
		})

		deleted := applyMachineChange(copyRows(added), mesh.MachineChange{
			Kind:    mesh.ChangeDeleted,
			Machine: mesh.MachineRow{ID: id},
		})

		if id != "existing" && id != "" {
			if len(deleted) != len(initial) {
				t.Errorf("add then delete: got %d, want %d", len(deleted), len(initial))
			}
		}

		// Add same ID twice results in one entry with that ID.
		twice := applyMachineChange(nil, mesh.MachineChange{Kind: mesh.ChangeAdded, Machine: row})
		twice = applyMachineChange(twice, mesh.MachineChange{Kind: mesh.ChangeAdded, Machine: row})
		count := 0
		for _, m := range twice {
			if m.ID == id {
				count++
			}
		}
		if count > 1 {
			t.Errorf("duplicate IDs after double add: got %d entries with ID %q", count, id)
		}
	})
}

func copyRows(rows []mesh.MachineRow) []mesh.MachineRow {
	out := make([]mesh.MachineRow, len(rows))
	copy(out, rows)
	return out
}
