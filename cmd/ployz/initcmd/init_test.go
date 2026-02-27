package initcmd

import (
	"testing"

	"ployz/pkg/sdk/cluster"
)

func TestMergeContextEntryPreservesRemoteConnections(t *testing.T) {
	existing := cluster.Cluster{
		Network: "default",
		Connections: []cluster.Connection{
			{Unix: "/tmp/old.sock", DataRoot: "/old"},
			{SSH: "root@10.0.0.10", SSHKeyFile: "~/.ssh/id_ed25519"},
		},
	}
	preferred := cluster.Cluster{
		Network: "default",
		Connections: []cluster.Connection{
			{Unix: "/tmp/ployzd.sock", DataRoot: "/var/db/ployz/networks"},
		},
	}

	merged := mergeContextEntry(existing, preferred)
	if len(merged.Connections) != 2 {
		t.Fatalf("expected 2 connections, got %d", len(merged.Connections))
	}
	if merged.Connections[0].Unix != "/tmp/ployzd.sock" {
		t.Fatalf("expected preferred unix connection first, got %q", merged.Connections[0].Unix)
	}
	if merged.Connections[1].SSH != "root@10.0.0.10" {
		t.Fatalf("expected ssh connection preserved, got %q", merged.Connections[1].SSH)
	}
}

func TestMergeContextEntryKeepsPreferredNetwork(t *testing.T) {
	existing := cluster.Cluster{Network: "old"}
	preferred := cluster.Cluster{Network: "new"}
	merged := mergeContextEntry(existing, preferred)
	if merged.Network != "new" {
		t.Fatalf("expected preferred network, got %q", merged.Network)
	}
}
