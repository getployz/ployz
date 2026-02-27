package service

import "testing"

func TestListCmdShape(t *testing.T) {
	cmd := listCmd()
	if cmd.Use != "list [service]" {
		t.Fatalf("unexpected use: %q", cmd.Use)
	}
	if err := cmd.Args(cmd, []string{"a", "b"}); err == nil {
		t.Fatal("expected args validation error for too many args")
	}
}

func TestListCmdBindsContextFlag(t *testing.T) {
	cmd := listCmd()
	if cmd.Flags().Lookup("context") == nil {
		t.Fatal("expected --context flag")
	}
}
