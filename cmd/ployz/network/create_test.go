package network

import "testing"

func TestCreateCmdShape(t *testing.T) {
	cmd := createCmd()
	if cmd.Use != "create [name] [user@host]" {
		t.Fatalf("unexpected use: %q", cmd.Use)
	}

	if err := cmd.Args(cmd, []string{"a", "b", "c"}); err == nil {
		t.Fatal("expected args validation error for too many args")
	}
}

func TestCreateCmdIncludesInitFlags(t *testing.T) {
	cmd := createCmd()
	flags := []string{
		"cidr",
		"advertise-endpoint",
		"wg-port",
		"data-root",
		"helper-image",
		"ssh-port",
		"ssh-key",
		"force",
	}

	for _, name := range flags {
		if cmd.Flags().Lookup(name) == nil {
			t.Fatalf("missing flag %q", name)
		}
	}
}
